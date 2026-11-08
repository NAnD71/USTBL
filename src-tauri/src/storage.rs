use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub trait Storage {
  fn file_path() -> std::path::PathBuf;

  fn load() -> Result<Self, std::io::Error>
  where
    Self: Sized + DeserializeOwned,
  {
    let json_string = fs::read_to_string(Self::file_path())?;
    let value = serde_json::from_str(&json_string)?;
    Ok(value)
  }

  fn save(&self) -> Result<(), std::io::Error>
  where
    Self: Serialize,
  {
    write_json_atomic(&Self::file_path(), self)
  }
}

pub async fn load_json_async<T>(file_path: &Path) -> Result<T, std::io::Error>
where
  T: Sized + DeserializeOwned + Send,
{
  let json_string = tokio::fs::read_to_string(file_path).await?;
  let value = serde_json::from_str(&json_string)?;
  Ok(value)
}

pub async fn save_json_async<T>(value: &T, file_path: &Path) -> Result<(), std::io::Error>
where
  T: Serialize + Send,
{
  let bytes = json_bytes(value)?;
  let path = file_path.to_path_buf();
  tokio::task::spawn_blocking(move || write_json_bytes(&path, &bytes))
    .await
    .map_err(io::Error::other)?
}

/// Whether `write_atomic` flushes the temporary file to disk before the rename.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Durability {
  Sync,
  NoSync,
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Replaces `path` with `bytes`: writes a uniquely named temporary file in the
/// same directory, flushes it, then renames it over the target. On failure the
/// previous target is untouched and the temporary file is removed. The parent
/// directory must exist.
///
/// The temporary name starts with `._` and ends with `.tmp`. The export and
/// publish listings skip `._*` paths, so a leftover from a crash is not
/// offered as a user file.
pub fn write_atomic(path: &Path, bytes: &[u8], durability: Durability) -> io::Result<()> {
  let (file, temporary) = create_temporary(path)?;
  let result = write_then_rename(file, &temporary, path, bytes, durability);
  if result.is_err() {
    let _ = fs::remove_file(&temporary);
  }
  result
}

/// Pretty-printed JSON, creating the parent directory like the writers before it.
pub fn write_json_atomic<T>(path: &Path, value: &T) -> io::Result<()>
where
  T: Serialize + ?Sized,
{
  write_json_bytes(path, &json_bytes(value)?)
}

fn json_bytes<T>(value: &T) -> io::Result<Vec<u8>>
where
  T: Serialize + ?Sized,
{
  Ok(serde_json::to_vec_pretty(value)?)
}

fn write_json_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent)?;
  }
  write_atomic(path, bytes, Durability::Sync)
}

fn create_temporary(path: &Path) -> io::Result<(fs::File, PathBuf)> {
  let name = path.file_name().ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::InvalidInput,
      "atomic write target has no file name",
    )
  })?;
  let directory = path.parent().unwrap_or(Path::new(""));
  let mut attempts = 0;
  loop {
    let mut temporary_name = std::ffi::OsString::from("._");
    temporary_name.push(name);
    temporary_name.push(format!(
      ".{}-{}.tmp",
      std::process::id(),
      TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = directory.join(temporary_name);
    match fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&temporary)
    {
      Ok(file) => return Ok((file, temporary)),
      // A leftover from an earlier process with the same pid and counter.
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempts < 8 => attempts += 1,
      Err(error) => return Err(error),
    }
  }
}

fn write_then_rename(
  mut file: fs::File,
  temporary: &Path,
  target: &Path,
  bytes: &[u8],
  durability: Durability,
) -> io::Result<()> {
  file.write_all(bytes)?;
  file.flush()?;
  if durability == Durability::Sync {
    file.sync_all()?;
  }
  drop(file);
  fs::rename(temporary, target)
}

/// Shared fixtures for persistence tests. Test-only, so production signatures stay unchanged.
#[cfg(test)]
pub(crate) mod test_support {
  use std::fs;
  use std::path::{Path, PathBuf};
  use std::sync::{LazyLock, Mutex, MutexGuard};

  static APP_DATA: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = std::env::temp_dir().join(format!("ustbl-persist-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
  });
  static SERIAL: Mutex<()> = Mutex::new(());

  /// Points `APP_DATA_DIR` at a temp directory. It can be set once per test binary, so every
  /// test shares this directory and must serialize on [`serial`] when it uses fixed file names.
  pub(crate) fn app_data_dir() -> PathBuf {
    let _ = crate::APP_DATA_DIR.set(APP_DATA.clone());
    crate::APP_DATA_DIR.get().unwrap().clone()
  }

  pub(crate) fn serial() -> MutexGuard<'static, ()> {
    let guard = SERIAL
      .lock()
      .unwrap_or_else(|poisoned| poisoned.into_inner());
    app_data_dir();
    guard
  }

  pub(crate) struct TempDir(pub PathBuf);

  impl TempDir {
    pub(crate) fn new(name: &str) -> Self {
      let path = std::env::temp_dir().join(format!("ustbl-{name}-{}", uuid::Uuid::new_v4()));
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }
  }

  impl Drop for TempDir {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  /// Holds `path` with no sharing, so any other open for read or write fails on Windows.
  #[cfg(windows)]
  pub(crate) fn lock_exclusive(path: &Path) -> fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    fs::OpenOptions::new()
      .read(true)
      .share_mode(0)
      .open(path)
      .unwrap()
  }
}

#[cfg(test)]
mod tests {
  #[cfg(windows)]
  use super::test_support::lock_exclusive;
  use super::test_support::{serial, TempDir};
  use super::{save_json_async, write_atomic, write_json_atomic, Durability, Storage};
  use crate::account::models::AccountInfo;
  use serde::ser::SerializeStruct;
  use serde::{Deserialize, Serialize, Serializer};
  use std::collections::BTreeMap;
  use std::fs;
  use std::io::ErrorKind;
  use std::path::Path;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::sync::Arc;

  /// Writes `first` and then fails, like a value that breaks off partway through.
  #[derive(Deserialize)]
  struct BreaksMidSerialize;

  impl Serialize for BreaksMidSerialize {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
      let mut state = serializer.serialize_struct("BreaksMidSerialize", 2)?;
      state.serialize_field("first", &1)?;
      Err(<S::Error as serde::ser::Error>::custom("injected failure"))
    }
  }

  fn reset(path: &Path) {
    let _ = fs::remove_file(path);
  }

  fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
      .unwrap()
      .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
      .collect();
    names.sort();
    names
  }

  fn leftovers(dir: &Path) -> Vec<String> {
    names(dir)
      .into_iter()
      .filter(|name| name.ends_with(".tmp"))
      .collect()
  }

  #[cfg(windows)]
  #[test]
  fn account_failed_save_keeps_previous_file() {
    let _guard = serial();
    let path = <AccountInfo as Storage>::file_path();
    fs::write(&path, b"previous account").unwrap();
    let held = lock_exclusive(&path);
    let saved = AccountInfo::default().save();
    drop(held);
    assert!(saved.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"previous account");
    reset(&path);
  }

  #[tokio::test]
  async fn json_write_that_fails_while_serializing_keeps_previous_file() {
    let dir = TempDir::new("storage-serialize");
    let path = dir.0.join("value.json");
    fs::write(&path, b"previous value").unwrap();
    let saved = save_json_async(&BreaksMidSerialize, &path).await;
    assert!(saved.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"previous value");
  }

  #[cfg(windows)]
  #[tokio::test]
  async fn json_write_that_cannot_open_keeps_previous_file() {
    let dir = TempDir::new("storage-open");
    let path = dir.0.join("value.json");
    fs::write(&path, b"previous value").unwrap();
    let held = lock_exclusive(&path);
    let saved = save_json_async(&vec![1, 2, 3], &path).await;
    drop(held);
    assert!(saved.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"previous value");
  }

  #[test]
  fn atomic_write_replaces_an_existing_file() {
    let dir = TempDir::new("atomic-replace");
    let path = dir.0.join("value.json");
    fs::write(&path, b"old bytes that are longer").unwrap();
    write_atomic(&path, b"new", Durability::Sync).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"new");
    assert_eq!(names(&dir.0), vec!["value.json"]);
    write_atomic(&path, b"newer", Durability::NoSync).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"newer");
    assert_eq!(names(&dir.0), vec!["value.json"]);
  }

  #[test]
  fn atomic_write_creates_a_missing_target() {
    let dir = TempDir::new("atomic-create");
    let path = dir.0.join("value.json");
    write_atomic(&path, b"fresh", Durability::Sync).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"fresh");
    assert_eq!(names(&dir.0), vec!["value.json"]);
  }

  #[test]
  fn atomic_write_needs_an_existing_parent() {
    let dir = TempDir::new("atomic-parent");
    let path = dir.0.join("missing").join("value.json");
    let error = write_atomic(&path, b"x", Durability::Sync).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound);
    assert!(names(&dir.0).is_empty());
  }

  #[cfg(windows)]
  #[test]
  fn atomic_write_over_a_file_open_with_default_sharing() {
    let dir = TempDir::new("atomic-default-share");
    let path = dir.0.join("value.json");
    fs::write(&path, b"old").unwrap();
    let held = fs::File::open(&path).unwrap();
    let written = write_atomic(&path, b"new", Durability::Sync);
    println!("default share (read/write/delete): {written:?}");
    drop(held);
    assert!(written.is_ok());
    assert_eq!(fs::read(&path).unwrap(), b"new");
    assert!(leftovers(&dir.0).is_empty());
  }

  #[cfg(windows)]
  #[test]
  fn atomic_write_over_a_file_open_without_delete_sharing() {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    let dir = TempDir::new("atomic-no-delete-share");
    let path = dir.0.join("value.json");
    fs::write(&path, b"old").unwrap();
    let held = fs::OpenOptions::new()
      .read(true)
      .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
      .open(&path)
      .unwrap();
    let written = write_atomic(&path, b"new", Durability::Sync);
    println!("share read|write, no delete: {written:?}");
    drop(held);
    assert!(written.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"old");
    assert!(leftovers(&dir.0).is_empty());
  }

  #[cfg(windows)]
  #[test]
  fn atomic_write_over_an_exclusively_held_file() {
    let dir = TempDir::new("atomic-exclusive");
    let path = dir.0.join("value.json");
    fs::write(&path, b"old").unwrap();
    let held = lock_exclusive(&path);
    let written = write_atomic(&path, b"new", Durability::Sync);
    println!("exclusive (share_mode 0): {written:?}");
    drop(held);
    assert!(written.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"old");
    assert!(leftovers(&dir.0).is_empty());
  }

  #[test]
  fn atomic_write_onto_a_directory_fails_and_leaves_it_alone() {
    let dir = TempDir::new("atomic-directory");
    let path = dir.0.join("value.json");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("inside.txt"), b"inside").unwrap();
    let written = write_atomic(&path, b"new", Durability::Sync);
    println!("directory target: {written:?}");
    assert!(written.is_err());
    assert!(path.is_dir());
    assert_eq!(fs::read(path.join("inside.txt")).unwrap(), b"inside");
    assert_eq!(names(&dir.0), vec!["value.json"]);
  }

  #[test]
  fn concurrent_writes_to_one_target_all_land_whole() {
    const WRITERS: usize = 8;
    const ROUNDS: usize = 50;
    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Payload {
      writer: usize,
      round: usize,
      filler: String,
    }
    let dir = TempDir::new("atomic-concurrent");
    let path = Arc::new(dir.0.join("value.json"));
    write_json_atomic(
      path.as_path(),
      &Payload {
        writer: 99,
        round: 0,
        filler: "seed".into(),
      },
    )
    .unwrap();

    let done = Arc::new(AtomicBool::new(false));
    let reader = {
      let (path, done) = (path.clone(), done.clone());
      std::thread::spawn(move || {
        let (mut reads, mut read_errors, mut torn) =
          (0usize, BTreeMap::<String, usize>::new(), 0usize);
        while !done.load(Ordering::Relaxed) {
          match fs::read(path.as_path()) {
            Ok(bytes) => {
              reads += 1;
              if serde_json::from_slice::<Payload>(&bytes).is_err() {
                torn += 1;
              }
            }
            Err(error) => {
              *read_errors
                .entry(format!("{:?}/{:?}", error.kind(), error.raw_os_error()))
                .or_default() += 1;
            }
          }
        }
        (reads, read_errors, torn)
      })
    };

    let handles: Vec<_> = (0..WRITERS)
      .map(|writer| {
        let path = path.clone();
        std::thread::spawn(move || {
          let mut errors = Vec::new();
          for round in 0..ROUNDS {
            let payload = Payload {
              writer,
              round,
              filler: "x".repeat(200 + writer * 37 + round),
            };
            if let Err(error) = write_json_atomic(path.as_path(), &payload) {
              errors.push(format!("{:?}/{:?}", error.kind(), error.raw_os_error()));
            }
          }
          errors
        })
      })
      .collect();
    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    for handle in handles {
      for error in handle.join().unwrap() {
        *failures.entry(error).or_default() += 1;
      }
    }
    done.store(true, Ordering::Relaxed);
    let (reads, read_errors, torn) = reader.join().unwrap();
    println!(
      "concurrent writes: {} attempts, write errors {failures:?}; reader: {reads} reads, read errors {read_errors:?}, unparsable {torn}",
      WRITERS * ROUNDS
    );

    let last: Payload = serde_json::from_slice(&fs::read(path.as_path()).unwrap()).unwrap();
    assert!(last.writer < WRITERS && last.round < ROUNDS);
    assert_eq!(last.filler, "x".repeat(200 + last.writer * 37 + last.round));
    assert!(leftovers(&dir.0).is_empty());
    assert_eq!(names(&dir.0), vec!["value.json"]);
    assert!(failures.is_empty(), "write errors: {failures:?}");
    assert_eq!(torn, 0);
  }

  #[tokio::test]
  async fn failed_serialization_creates_no_file_and_keeps_the_old_one() {
    let dir = TempDir::new("atomic-serialize");
    let path = dir.0.join("value.json");
    fs::write(&path, b"previous value").unwrap();
    assert!(save_json_async(&BreaksMidSerialize, &path).await.is_err());
    assert!(write_json_atomic(&path, &BreaksMidSerialize).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"previous value");
    assert_eq!(names(&dir.0), vec!["value.json"]);

    let nested = dir.0.join("new-dir").join("value.json");
    assert!(save_json_async(&BreaksMidSerialize, &nested).await.is_err());
    assert!(write_json_atomic(&nested, &BreaksMidSerialize).is_err());
    assert_eq!(names(&dir.0), vec!["value.json"]);
  }

  #[tokio::test]
  async fn json_output_is_pretty_and_creates_the_parent() {
    let dir = TempDir::new("atomic-pretty");
    let path = dir.0.join("a").join("b.json");
    save_json_async(&vec![1, 2], &path).await.unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"[\n  1,\n  2\n]");
    write_json_atomic(&path, &vec![3]).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"[\n  3\n]");
  }
}
