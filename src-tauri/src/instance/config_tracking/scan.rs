use super::model::{ConfigFile, ConfigFileType, ConfigTree, ReadState};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

const READ_RETRIES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
  volume: u64,
  index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EntrySignature {
  file_type: u8,
  identity: Option<FileIdentity>,
  size: u64,
  modified: Option<SystemTime>,
}

pub(crate) struct ScannedTree {
  pub tree: ConfigTree,
  pub objects: BTreeMap<String, Vec<u8>>,
}

#[allow(dead_code)]
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

pub(crate) fn validate_paths(
  game_root: &Path,
  config_dir: &Path,
) -> io::Result<(PathBuf, PathBuf)> {
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
  ensure_no_symlink_components(&root, &config)?;
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
  let directory_before = match fs::symlink_metadata(directory) {
    Ok(metadata) => metadata,
    Err(error) => {
      errors.push(format!("{}: {}", display_path(directory), error));
      return Ok(());
    }
  };
  if directory_before.file_type().is_symlink()
    || !directory_before.is_dir()
    || !canonical_inside(root, directory)
  {
    errors.push(format!(
      "{}: directory changed during scan",
      display_path(directory)
    ));
    return Ok(());
  }
  let directory_identity = match identity_for_path(directory) {
    Ok(identity) => identity,
    Err(error) => {
      errors.push(format!("{}: {}", display_path(directory), error));
      return Ok(());
    }
  };
  let entries = match fs::read_dir(directory) {
    Ok(entries) => entries,
    Err(error) => {
      errors.push(format!("{}: {}", display_path(directory), error));
      return Ok(());
    }
  };
  let mut before_entries = BTreeMap::new();
  for entry in entries {
    let entry = match entry {
      Ok(entry) => entry,
      Err(error) => {
        errors.push(error.to_string());
        continue;
      }
    };
    let path = entry.path();
    if !path.starts_with(directory) || !path.starts_with(root) {
      errors.push(format!("{}: path escaped scan root", display_path(&path)));
      continue;
    }
    let file_type = match entry.file_type() {
      Ok(file_type) => file_type,
      Err(error) => {
        errors.push(format!("{}: {}", display_path(&path), error));
        continue;
      }
    };
    let entry_metadata = match fs::symlink_metadata(&path) {
      Ok(metadata) => metadata,
      Err(error) => {
        errors.push(format!("{}: {}", display_path(&path), error));
        continue;
      }
    };
    let signature = match entry_signature(&path, &entry_metadata) {
      Ok(signature) => signature,
      Err(error) => {
        errors.push(format!("{}: {}", display_path(&path), error));
        continue;
      }
    };
    before_entries.insert(entry.file_name(), signature);
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
    let (file, bytes) = read_file_stably(&path, relative_path.clone(), root);
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
  let directory_changed = match fs::symlink_metadata(directory) {
    Ok(directory_after) => {
      let same_identity = identity_for_path(directory)
        .map(|identity| identity == directory_identity)
        .unwrap_or(false);
      directory_after.file_type().is_symlink()
        || !directory_after.is_dir()
        || !canonical_inside(root, directory)
        || !same_identity
        || directory_before.modified().ok() != directory_after.modified().ok()
    }
    Err(error) => {
      errors.push(format!("{}: {}", display_path(directory), error));
      true
    }
  };
  let listing_changed = match directory_signature(directory) {
    Ok(after_entries) => before_entries != after_entries,
    Err(error) => {
      errors.push(format!("{}: {}", display_path(directory), error));
      true
    }
  };
  if directory_changed || listing_changed {
    errors.push(format!(
      "{}: directory changed during scan",
      display_path(directory)
    ));
  }
  Ok(())
}

fn read_file_stably(
  path: &Path,
  relative_path: String,
  scan_root: &Path,
) -> (Option<ConfigFile>, Option<Vec<u8>>) {
  read_file_stably_with_hook(path, relative_path, scan_root, None)
}

#[cfg(test)]
pub(crate) fn read_file_stably_for_test(
  path: &Path,
  relative_path: String,
  hook: &mut dyn FnMut(),
) -> (Option<ConfigFile>, Option<Vec<u8>>) {
  read_file_stably_with_hook(
    path,
    relative_path,
    path.parent().unwrap_or(path),
    Some(hook),
  )
}

fn read_file_stably_with_hook(
  path: &Path,
  relative_path: String,
  scan_root: &Path,
  mut hook: Option<&mut dyn FnMut()>,
) -> (Option<ConfigFile>, Option<Vec<u8>>) {
  let mut saw_change = false;
  for _ in 0..READ_RETRIES {
    if !path.starts_with(scan_root) || !canonical_inside(scan_root, path) {
      return (Some(unstable_file(relative_path)), None);
    }
    let before = match fs::symlink_metadata(path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
        return (Some(unstable_file(relative_path)), None)
      }
      Err(_) => return (Some(unreadable_file(relative_path)), None),
    };
    if before.file_type().is_symlink() || !before.is_file() {
      return (Some(unstable_file(relative_path)), None);
    }
    let before_identity = match identity_for_path(path) {
      Ok(identity) => identity,
      Err(_) => return (Some(unstable_file(relative_path)), None),
    };
    let (first, first_handle_metadata, first_identity) = match read_file_no_follow(path) {
      Ok(value) => value,
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
        return (Some(unstable_file(relative_path)), None)
      }
      Err(_) => return (Some(unreadable_file(relative_path)), None),
    };
    if !same_file_state(&before, &first_handle_metadata) || before_identity != first_identity {
      saw_change = true;
      continue;
    }
    if let Some(hook) = hook.as_deref_mut() {
      hook();
    }
    let after = match fs::symlink_metadata(path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
        saw_change = true;
        continue;
      }
      Err(_) => return (Some(unreadable_file(relative_path)), None),
    };
    if after.file_type().is_symlink()
      || !after.is_file()
      || !canonical_inside(scan_root, path)
      || !same_file_state(&before, &after)
    {
      saw_change = true;
      continue;
    }
    let (second, second_handle_metadata, second_identity) = match read_file_no_follow(path) {
      Ok(value) => value,
      Err(_) => {
        saw_change = true;
        continue;
      }
    };
    if first_identity != second_identity
      || !same_file_state(&before, &second_handle_metadata)
      || first != second
    {
      saw_change = true;
      continue;
    }
    let after_second = match fs::symlink_metadata(path) {
      Ok(metadata) => metadata,
      Err(_) => {
        saw_change = true;
        continue;
      }
    };
    if after_second.file_type().is_symlink()
      || !after_second.is_file()
      || !canonical_inside(scan_root, path)
      || !same_file_state(&before, &after_second)
    {
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
  let size = fs::symlink_metadata(path)
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

fn ensure_no_symlink_components(root: &Path, path: &Path) -> io::Result<()> {
  let relative = path
    .strip_prefix(root)
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path escapes game root"))?;
  let mut current = root.to_path_buf();
  for component in relative.components() {
    let Component::Normal(name) = component else {
      continue;
    };
    current.push(name);
    match fs::symlink_metadata(&current) {
      Ok(metadata) if metadata.file_type().is_symlink() => {
        return Err(io::Error::new(
          io::ErrorKind::InvalidInput,
          "config path contains a symbolic link",
        ));
      }
      Ok(_) => {}
      Err(error) if error.kind() == io::ErrorKind::NotFound => break,
      Err(error) => return Err(error),
    }
  }
  Ok(())
}

fn directory_signature(directory: &Path) -> io::Result<BTreeMap<OsString, EntrySignature>> {
  let mut signature = BTreeMap::new();
  for entry in fs::read_dir(directory)? {
    let entry = entry?;
    let path = entry.path();
    let metadata = fs::symlink_metadata(&path)?;
    signature.insert(entry.file_name(), entry_signature(&path, &metadata)?);
  }
  Ok(signature)
}

fn read_file_no_follow(path: &Path) -> io::Result<(Vec<u8>, fs::Metadata, FileIdentity)> {
  let mut file = open_file_no_follow(path)?;
  let metadata = file.metadata()?;
  if metadata.file_type().is_symlink() || !metadata.is_file() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "config entry is not a regular file",
    ));
  }
  let mut bytes = Vec::new();
  file.read_to_end(&mut bytes)?;
  let identity = handle_identity(&file, &metadata)?;
  Ok((bytes, metadata, identity))
}

#[cfg(windows)]
fn open_file_no_follow(path: &Path) -> io::Result<fs::File> {
  use std::os::windows::ffi::OsStrExt;
  use std::os::windows::io::FromRawHandle;
  use winapi::shared::minwindef::DWORD;
  use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING};
  use winapi::um::handleapi::INVALID_HANDLE_VALUE;
  use winapi::um::winnt::{FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, GENERIC_READ};

  let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
  let handle = unsafe {
    CreateFileW(
      path.as_ptr(),
      GENERIC_READ,
      FILE_SHARE_READ | FILE_SHARE_WRITE,
      std::ptr::null_mut(),
      OPEN_EXISTING,
      FILE_ATTRIBUTE_NORMAL | 0x00200000 as DWORD | 0x02000000 as DWORD,
      std::ptr::null_mut(),
    )
  };
  if handle == INVALID_HANDLE_VALUE {
    return Err(io::Error::last_os_error());
  }
  Ok(unsafe { fs::File::from_raw_handle(handle as _) })
}

#[cfg(unix)]
fn open_file_no_follow(path: &Path) -> io::Result<fs::File> {
  use std::os::unix::fs::OpenOptionsExt;
  let mut options = fs::OpenOptions::new();
  options.read(true);
  #[cfg(target_os = "linux")]
  options.custom_flags(0x20000);
  #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd"))]
  options.custom_flags(0x100);
  options.open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_file_no_follow(path: &Path) -> io::Result<fs::File> {
  fs::File::open(path)
}

fn entry_signature(path: &Path, metadata: &fs::Metadata) -> io::Result<EntrySignature> {
  let identity = if metadata.file_type().is_symlink() {
    None
  } else {
    Some(identity_for_path(path)?)
  };
  Ok(EntrySignature {
    file_type: file_type_code(&metadata.file_type()),
    identity,
    size: metadata.len(),
    modified: metadata.modified().ok(),
  })
}

fn identity_for_path(path: &Path) -> io::Result<FileIdentity> {
  let file = open_file_no_follow(path)?;
  let metadata = file.metadata()?;
  handle_identity(&file, &metadata)
}

fn file_type_code(file_type: &fs::FileType) -> u8 {
  if file_type.is_symlink() {
    1
  } else if file_type.is_dir() {
    2
  } else if file_type.is_file() {
    3
  } else {
    4
  }
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

fn canonical_inside(root: &Path, path: &Path) -> bool {
  let Ok(canonical_root) = root.canonicalize() else {
    return false;
  };
  path
    .canonicalize()
    .map(|canonical_path| canonical_path.starts_with(canonical_root))
    .unwrap_or(false)
}

fn unstable_file(path: String) -> ConfigFile {
  ConfigFile {
    path,
    sha1: String::new(),
    size: 0,
    object: None,
    file_type: ConfigFileType::Regular,
    read_state: ReadState::Unstable,
  }
}

fn same_file_state(left: &fs::Metadata, right: &fs::Metadata) -> bool {
  same_file_identity(left, right)
    && left.len() == right.len()
    && left.modified().ok() == right.modified().ok()
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> Option<FileIdentity> {
  Some(FileIdentity {
    volume: metadata.dev(),
    index: metadata.ino(),
  })
}

#[cfg(windows)]
fn file_identity(metadata: &fs::Metadata) -> Option<FileIdentity> {
  let _ = metadata;
  None
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_metadata: &fs::Metadata) -> Option<FileIdentity> {
  None
}

fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
  file_identity(left)
    .zip(file_identity(right))
    .is_none_or(|(left, right)| left == right)
}

#[cfg(unix)]
fn handle_identity(_file: &fs::File, metadata: &fs::Metadata) -> io::Result<FileIdentity> {
  file_identity(metadata)
    .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "file identity unavailable"))
}

#[cfg(windows)]
fn handle_identity(file: &fs::File, _metadata: &fs::Metadata) -> io::Result<FileIdentity> {
  use std::os::windows::io::AsRawHandle;
  use winapi::um::fileapi::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
  let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
  if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut information) } == 0 {
    return Err(io::Error::last_os_error());
  }
  Ok(FileIdentity {
    volume: u64::from(information.dwVolumeSerialNumber),
    index: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
  })
}

#[cfg(not(any(unix, windows)))]
fn handle_identity(_file: &fs::File, _metadata: &fs::Metadata) -> io::Result<FileIdentity> {
  Err(io::Error::new(
    io::ErrorKind::Unsupported,
    "file identity unavailable",
  ))
}

pub(crate) fn make_tree(
  mut files: Vec<ConfigFile>,
  incomplete: bool,
  scan_errors: Vec<String>,
) -> ConfigTree {
  files.sort_by(|left, right| left.path.cmp(&right.path));
  let id = tree_id_for_parts(&files, incomplete, &scan_errors);
  ConfigTree {
    id,
    files,
    incomplete,
    scan_errors,
  }
}

pub(crate) fn tree_id_for(tree: &ConfigTree) -> String {
  tree_id_for_parts(&tree.files, tree.incomplete, &tree.scan_errors)
}

fn tree_id_for_parts(files: &[ConfigFile], incomplete: bool, scan_errors: &[String]) -> String {
  let payload = serde_json::to_vec(&(files, incomplete, scan_errors))
    .expect("config tree metadata must serialize");
  hex::encode(Sha1::digest(payload))
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
