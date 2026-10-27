use crate::instance::config_tracking::model::{FileEntry, ReadState, Tree};
use std::fs;
use std::io;
use std::path::Path;
use walkdir::WalkDir;

pub(crate) const MAX_CONFIG_FILE_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) struct ScanResult {
  pub tree: Tree,
  pub config_present: bool,
  pub errors: Vec<String>,
}

pub(crate) fn is_stable(before: &fs::Metadata, bytes_len: usize, after: &fs::Metadata) -> bool {
  let (Ok(before_modified), Ok(after_modified)) = (before.modified(), after.modified()) else {
    return false;
  };
  before.len() == after.len()
    && before.len() == bytes_len as u64
    && before_modified == after_modified
}

pub(crate) fn scan_config(
  config: &Path,
  max_bytes: u64,
  mut write_object: impl FnMut(&[u8]) -> io::Result<String>,
) -> io::Result<ScanResult> {
  let root_metadata = match fs::symlink_metadata(config) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
      return Ok(ScanResult {
        tree: Tree { files: Vec::new() },
        config_present: false,
        errors: Vec::new(),
      });
    }
    Err(error) => {
      return Ok(ScanResult {
        tree: Tree { files: Vec::new() },
        config_present: true,
        errors: vec![format!("cannot inspect config directory: {error}")],
      });
    }
  };
  if !root_metadata.is_dir() {
    return Ok(ScanResult {
      tree: Tree { files: Vec::new() },
      config_present: true,
      errors: vec!["config path exists but is not a directory".to_string()],
    });
  }

  let mut files = Vec::new();
  let mut errors = Vec::new();
  for result in WalkDir::new(config)
    .follow_links(false)
    .sort_by_file_name()
    .into_iter()
  {
    let entry = match result {
      Ok(entry) => entry,
      Err(error) => {
        errors.push(format!("walk error: {error}"));
        continue;
      }
    };
    if entry.path() == config || !entry.file_type().is_file() {
      continue;
    }
    let relative = match relative_path(config, entry.path()) {
      Some(path) => path,
      None => {
        errors.push("non-UTF-8 path under config".to_string());
        continue;
      }
    };
    let file = scan_file(entry.path(), &relative, max_bytes, &mut write_object)?;
    files.push(file);
  }
  files.sort_by(|a, b| a.path.cmp(&b.path));
  Ok(ScanResult {
    tree: Tree { files },
    config_present: true,
    errors,
  })
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
  let relative = path.strip_prefix(root).ok()?;
  let mut components = Vec::new();
  for component in relative.components() {
    components.push(component.as_os_str().to_str()?.to_string());
  }
  Some(components.join("/"))
}

fn scan_file(
  path: &Path,
  relative: &str,
  max_bytes: u64,
  write_object: &mut impl FnMut(&[u8]) -> io::Result<String>,
) -> io::Result<FileEntry> {
  let mut last_size = 0;
  for _ in 0..3 {
    let before = match fs::symlink_metadata(path) {
      Ok(metadata) => metadata,
      Err(_) => return Ok(unreadable(relative)),
    };
    if before.len() > max_bytes {
      return Ok(FileEntry {
        path: relative.to_string(),
        sha1: None,
        size: before.len(),
        state: ReadState::TooLarge,
      });
    }
    let bytes = match fs::read(path) {
      Ok(bytes) => bytes,
      Err(_) => return Ok(unreadable(relative)),
    };
    last_size = bytes.len() as u64;
    let after = match fs::symlink_metadata(path) {
      Ok(metadata) => metadata,
      Err(_) => return Ok(unreadable(relative)),
    };
    if is_stable(&before, bytes.len(), &after) {
      let sha1 = write_object(&bytes)?;
      return Ok(FileEntry {
        path: relative.to_string(),
        sha1: Some(sha1),
        size: bytes.len() as u64,
        state: ReadState::Stable,
      });
    }
  }
  Ok(FileEntry {
    path: relative.to_string(),
    sha1: None,
    size: last_size,
    state: ReadState::Unstable,
  })
}

fn unreadable(path: &str) -> FileEntry {
  FileEntry {
    path: path.to_string(),
    sha1: None,
    size: 0,
    state: ReadState::Unreadable,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;
  use std::time::Duration;

  fn temp_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("ustbl-config-scan-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    root
  }

  #[test]
  fn scans_nested_regular_files_in_path_order() {
    let root = temp_root();
    fs::create_dir_all(root.join("config/z/nested")).unwrap();
    fs::write(root.join("config/z/nested/b.txt"), b"b").unwrap();
    fs::write(root.join("config/a.toml"), b"a").unwrap();
    let result = scan_config(&root.join("config"), MAX_CONFIG_FILE_BYTES, |_| {
      Ok("x".into())
    })
    .unwrap();
    assert_eq!(
      result
        .tree
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect::<Vec<_>>(),
      ["a.toml", "z/nested/b.txt"]
    );
    assert!(result.config_present);
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn missing_config_is_empty_without_error() {
    let root = temp_root();
    let result = scan_config(&root.join("config"), 10, |_| Ok("x".into())).unwrap();
    assert!(!result.config_present);
    assert!(result.tree.files.is_empty());
    assert!(result.errors.is_empty());
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn config_file_is_reported_without_being_scanned() {
    let root = temp_root();
    fs::write(root.join("config"), b"not a directory").unwrap();
    let result = scan_config(&root.join("config"), 10, |_| Ok("x".into())).unwrap();
    assert!(result.config_present);
    assert!(result.tree.files.is_empty());
    assert_eq!(result.errors.len(), 1);
    fs::remove_dir_all(root).unwrap();
  }

  #[cfg(unix)]
  #[test]
  fn skips_symbolic_links() {
    let root = temp_root();
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(root.join("outside"), b"outside").unwrap();
    std::os::unix::fs::symlink(root.join("outside"), root.join("config/link")).unwrap();
    let result = scan_config(&root.join("config"), 10, |_| Ok("x".into())).unwrap();
    assert!(result.tree.files.is_empty());
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn stable_requires_matching_length_mtime_and_bytes_length() {
    let root = temp_root();
    let path = root.join("file");
    fs::write(&path, b"one").unwrap();
    let before = fs::metadata(&path).unwrap();
    assert!(!is_stable(&before, 2, &before));
    assert!(is_stable(&before, 3, &before));
    std::thread::sleep(Duration::from_millis(20));
    fs::write(&path, b"two").unwrap();
    let after = fs::metadata(&path).unwrap();
    assert!(!is_stable(&before, 3, &after));
    assert!(!is_stable(&before, 4, &after));
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn too_large_does_not_write_an_object() {
    let root = temp_root();
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(root.join("config/a"), b"12345").unwrap();
    let mut writes = 0;
    let result = scan_config(&root.join("config"), 2, |_| {
      writes += 1;
      Ok("never".into())
    })
    .unwrap();
    assert_eq!(writes, 0);
    assert_eq!(result.tree.files[0].state, ReadState::TooLarge);
    assert_eq!(result.tree.files[0].sha1, None);
    fs::remove_dir_all(root).unwrap();
  }
}
