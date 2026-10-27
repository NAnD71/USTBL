use super::model::{ConfigFile, ConfigFileType, ConfigTree, ReadState};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

const READ_RETRIES: usize = 3;

pub(crate) struct ScannedTree {
  pub tree: ConfigTree,
  pub objects: BTreeMap<String, Vec<u8>>,
}

pub fn scan_config(game_root: &Path, config_dir: &Path) -> io::Result<ConfigTree> {
  Ok(scan_config_with_objects(game_root, config_dir)?.tree)
}

pub(crate) fn scan_config_with_objects(
  game_root: &Path,
  config_dir: &Path,
) -> io::Result<ScannedTree> {
  let (root, config) = validate_paths(game_root, config_dir)?;
  if !config.exists() {
    return Ok(ScannedTree {
      tree: make_tree(Vec::new(), false, Vec::new()),
      objects: BTreeMap::new(),
    });
  }
  let metadata = fs::symlink_metadata(&config)?;
  if metadata.file_type().is_symlink() || !metadata.is_dir() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "config path must be a directory and not a symbolic link",
    ));
  }

  let config_prefix = config
    .strip_prefix(&root)
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "config path escapes game root"))?;
  let prefix = path_to_slash(config_prefix)?;
  let mut files = Vec::new();
  let mut objects = BTreeMap::new();
  let mut errors = Vec::new();
  scan_directory(
    &root,
    &config,
    &prefix,
    &mut files,
    &mut objects,
    &mut errors,
  )?;
  files.sort_by(|left: &ConfigFile, right| left.path.cmp(&right.path));
  let has_unstable_file = files
    .iter()
    .any(|file| file.read_state != ReadState::Stable);
  Ok(ScannedTree {
    tree: make_tree(files, !errors.is_empty() || has_unstable_file, errors),
    objects,
  })
}

fn validate_paths(game_root: &Path, config_dir: &Path) -> io::Result<(PathBuf, PathBuf)> {
  let root = game_root.canonicalize()?;
  if !root.is_dir() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "game root is not a directory",
    ));
  }
  let config_input = if config_dir.is_absolute() {
    config_dir.to_path_buf()
  } else {
    root.join(config_dir)
  };
  let config_metadata = fs::symlink_metadata(&config_input);
  if let Ok(metadata) = &config_metadata {
    if metadata.file_type().is_symlink() {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "config path must not be a symbolic link",
      ));
    }
  }
  let config =
    if config_metadata.is_ok() {
      config_input.canonicalize()?
    } else if config_metadata
      .as_ref()
      .err()
      .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
    {
      let parent = config_input
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config path has no parent"))?;
      let canonical_parent = parent.canonicalize()?;
      canonical_parent.join(config_input.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "config path has no file name")
      })?)
    } else {
      return Err(config_metadata.unwrap_err());
    };
  if !config.starts_with(&root) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "config path escapes game root",
    ));
  }
  Ok((root, config))
}

fn scan_directory(
  root: &Path,
  directory: &Path,
  prefix: &str,
  files: &mut Vec<ConfigFile>,
  objects: &mut BTreeMap<String, Vec<u8>>,
  errors: &mut Vec<String>,
) -> io::Result<()> {
  let entries = match fs::read_dir(directory) {
    Ok(entries) => entries,
    Err(error) => {
      errors.push(format!("{}: {}", display_path(directory), error));
      return Ok(());
    }
  };
  for entry in entries {
    let entry = match entry {
      Ok(entry) => entry,
      Err(error) => {
        errors.push(error.to_string());
        continue;
      }
    };
    let path = entry.path();
    let file_type = match entry.file_type() {
      Ok(file_type) => file_type,
      Err(error) => {
        errors.push(format!("{}: {}", display_path(&path), error));
        continue;
      }
    };
    if file_type.is_symlink() {
      continue;
    }
    if file_type.is_dir() {
      let entry_name = entry.file_name();
      let name = match entry_name.to_str() {
        Some(name) => name,
        None => {
          errors.push(format!("{}: non-UTF-8 directory name", display_path(&path)));
          continue;
        }
      };
      let child_prefix = if prefix.is_empty() {
        name.to_string()
      } else {
        format!("{prefix}/{name}")
      };
      scan_directory(root, &path, &child_prefix, files, objects, errors)?;
      continue;
    }
    if !file_type.is_file() {
      continue;
    }
    let entry_name = entry.file_name();
    let name = match entry_name.to_str() {
      Some(name) => name,
      None => {
        errors.push(format!("{}: non-UTF-8 file name", display_path(&path)));
        continue;
      }
    };
    let relative_path = if prefix.is_empty() {
      name.to_string()
    } else {
      format!("{prefix}/{name}")
    };
    let (file, bytes) = read_file_stably(&path, relative_path.clone());
    if let (Some(file), Some(bytes)) = (&file, &bytes) {
      objects.insert(file.sha1.clone(), bytes.clone());
    }
    files.push(file.unwrap_or_else(|| {
      ConfigFile {
        path: path_to_slash(
          path
            .strip_prefix(root)
            .expect("scanned path must remain below game root"),
        )
        .unwrap_or_else(|_| relative_path.clone()),
        sha1: String::new(),
        size: 0,
        object: None,
        file_type: ConfigFileType::Regular,
        read_state: ReadState::Unreadable,
      }
    }));
  }
  Ok(())
}

fn read_file_stably(path: &Path, relative_path: String) -> (Option<ConfigFile>, Option<Vec<u8>>) {
  read_file_stably_with_hook(path, relative_path, None)
}

#[cfg(test)]
pub(crate) fn read_file_stably_for_test(
  path: &Path,
  relative_path: String,
  hook: &mut dyn FnMut(),
) -> (Option<ConfigFile>, Option<Vec<u8>>) {
  read_file_stably_with_hook(path, relative_path, Some(hook))
}

fn read_file_stably_with_hook(
  path: &Path,
  relative_path: String,
  mut hook: Option<&mut dyn FnMut()>,
) -> (Option<ConfigFile>, Option<Vec<u8>>) {
  let mut saw_change = false;
  for _ in 0..READ_RETRIES {
    let before = match fs::metadata(path) {
      Ok(metadata) => metadata,
      Err(_) => return (Some(unreadable_file(relative_path)), None),
    };
    let first = match fs::read(path) {
      Ok(bytes) => bytes,
      Err(_) => return (Some(unreadable_file(relative_path)), None),
    };
    if let Some(hook) = hook.as_deref_mut() {
      hook();
    }
    let after = match fs::metadata(path) {
      Ok(metadata) => metadata,
      Err(_) => {
        saw_change = true;
        continue;
      }
    };
    if !same_file_state(&before, &after) {
      saw_change = true;
      continue;
    }
    let second = match fs::read(path) {
      Ok(bytes) => bytes,
      Err(_) => {
        saw_change = true;
        continue;
      }
    };
    if first != second {
      saw_change = true;
      continue;
    }
    let sha1 = hex::encode(Sha1::digest(&first));
    return (
      Some(ConfigFile {
        path: relative_path,
        sha1: sha1.clone(),
        size: first.len() as u64,
        object: Some(sha1),
        file_type: ConfigFileType::Regular,
        read_state: ReadState::Stable,
      }),
      Some(first),
    );
  }
  let size = fs::metadata(path)
    .map(|metadata| metadata.len())
    .unwrap_or(0);
  (
    Some(ConfigFile {
      path: relative_path,
      sha1: String::new(),
      size,
      object: None,
      file_type: ConfigFileType::Regular,
      read_state: if saw_change {
        ReadState::Unstable
      } else {
        ReadState::Unreadable
      },
    }),
    None,
  )
}

fn unreadable_file(path: String) -> ConfigFile {
  ConfigFile {
    path,
    sha1: String::new(),
    size: 0,
    object: None,
    file_type: ConfigFileType::Regular,
    read_state: ReadState::Unreadable,
  }
}

fn same_file_state(left: &fs::Metadata, right: &fs::Metadata) -> bool {
  left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

pub(crate) fn make_tree(
  mut files: Vec<ConfigFile>,
  incomplete: bool,
  scan_errors: Vec<String>,
) -> ConfigTree {
  files.sort_by(|left, right| left.path.cmp(&right.path));
  let payload = serde_json::to_vec(&(&files, incomplete, &scan_errors))
    .expect("config tree metadata must serialize");
  let id = hex::encode(Sha1::digest(payload));
  ConfigTree {
    id,
    files,
    incomplete,
    scan_errors,
  }
}

fn path_to_slash(path: &Path) -> io::Result<String> {
  let mut result = String::new();
  for component in path.components() {
    let component = match component {
      Component::Normal(value) => value.to_str().ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::InvalidData,
          "path contains non-UTF-8 component",
        )
      })?,
      Component::CurDir => continue,
      _ => {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "scanned path contains unsafe component",
        ))
      }
    };
    if !result.is_empty() {
      result.push('/');
    }
    result.push_str(component);
  }
  Ok(result)
}

fn display_path(path: &Path) -> String {
  path.to_string_lossy().replace('\\', "/")
}
