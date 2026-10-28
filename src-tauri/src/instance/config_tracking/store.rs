use crate::instance::config_tracking::model::*;
use crate::instance::config_tracking::scan::{scan_config, MAX_CONFIG_FILE_BYTES};
use crate::instance::config_tracking::semantic::{diff_trees, file_diff, semantics};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

static ROOT_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

pub struct ConfigHistory {
  game_root: PathBuf,
}

impl ConfigHistory {
  pub fn new(game_root: impl Into<PathBuf>) -> Self {
    Self {
      game_root: game_root.into(),
    }
  }

  pub fn config_dir(&self) -> PathBuf {
    self.game_root.join("config")
  }

  pub fn history_dir(&self) -> PathBuf {
    self.game_root.join(".ustbl-update/config-history")
  }

  pub fn append(&self, request: CheckpointRequest) -> io::Result<(u64, LogEntry)> {
    validate_request(&request)?;
    let lock = ROOT_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let root_lock = {
      let mut locks = lock
        .lock()
        .map_err(|_| io::Error::other("config history lock poisoned"))?;
      locks
        .entry(self.game_root.clone())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
    };
    let _guard = root_lock
      .lock()
      .map_err(|_| io::Error::other("config history lock poisoned"))?;
    let scan = scan_config(&self.config_dir(), MAX_CONFIG_FILE_BYTES, |bytes| {
      self.write_object(bytes)
    })?;
    let tree_bytes = serde_json::to_vec(&scan.tree).map_err(io::Error::other)?;
    let tree = self.write_object(&tree_bytes)?;
    let entry = LogEntry {
      v: 1,
      at: chrono::Utc::now().to_rfc3339(),
      checkpoint: request.checkpoint,
      session_id: request.session_id,
      instance_id: request.instance_id,
      exit: request.exit,
      config_present: scan.config_present,
      tree,
      errors: scan.errors,
    };
    fs::create_dir_all(&self.history_dir())?;
    let log_path = self.history_dir().join("log.jsonl");
    let prior = match fs::read(&log_path) {
      Ok(bytes) => bytes,
      Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
      Err(error) => return Err(error),
    };
    let mut log = OpenOptions::new()
      .create(true)
      .append(true)
      .open(&log_path)?;
    if !prior.is_empty() && prior.last() != Some(&b'\n') {
      log.write_all(b"\n")?;
    }
    let line_breaks = prior.iter().filter(|byte| **byte == b'\n').count() as u64;
    let id = line_breaks
      + if prior.is_empty() || prior.last() == Some(&b'\n') {
        1
      } else {
        2
      };
    let mut line = serde_json::to_vec(&entry).map_err(io::Error::other)?;
    line.push(b'\n');
    log.write_all(&line)?;
    Ok((id, entry))
  }

  pub fn read_log(&self) -> io::Result<LogRead> {
    let path = self.history_dir().join("log.jsonl");
    let bytes = match fs::read(path) {
      Ok(bytes) => bytes,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(LogRead::default()),
      Err(error) => return Err(error),
    };
    let terminated = bytes.last() == Some(&b'\n');
    let line_count = bytes.split(|byte| *byte == b'\n').count();
    let mut result = LogRead::default();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
      let id = index as u64 + 1;
      if line.is_empty() {
        continue;
      }
      let entry = match serde_json::from_slice::<LogEntry>(line) {
        Ok(entry) => entry,
        Err(error) if index + 1 == line_count && !terminated => {
          result
            .diagnostics
            .push(format!("ignored incomplete final line {id}: {error}"));
          continue;
        }
        Err(error) => {
          result.diagnostics.push(format!("line {id}: {error}"));
          continue;
        }
      };
      if entry.v != 1 {
        result
          .diagnostics
          .push(format!("line {id}: unsupported version {}", entry.v));
        continue;
      }
      result.entries.push((id, entry));
    }
    Ok(result)
  }

  pub fn write_object(&self, bytes: &[u8]) -> io::Result<String> {
    let id = sha1_hex(bytes);
    let objects = self.history_dir().join("objects");
    fs::create_dir_all(&objects)?;
    let target = objects.join(&id);
    if target.exists() {
      return Ok(id);
    }
    let temporary = objects.join(format!("{id}.tmp-{}", Uuid::new_v4().simple()));
    fs::write(&temporary, bytes)?;
    match fs::rename(&temporary, &target) {
      Ok(()) => Ok(id),
      Err(_) if target.exists() => {
        let _ = fs::remove_file(&temporary);
        Ok(id)
      }
      Err(error) => {
        let _ = fs::remove_file(&temporary);
        Err(error)
      }
    }
  }

  pub fn load_object(&self, id: &str) -> io::Result<Vec<u8>> {
    validate_object_id(id)?;
    let bytes = fs::read(self.history_dir().join("objects").join(id))?;
    if sha1_hex(&bytes) != id {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "object SHA-1 mismatch",
      ));
    }
    Ok(bytes)
  }

  pub fn timeline(&self) -> io::Result<Timeline> {
    let log = self.read_log()?;
    let semantic_values = semantics(
      &log
        .entries
        .iter()
        .map(|(_, entry)| entry.clone())
        .collect::<Vec<_>>(),
    );
    let mut diagnostics = log.diagnostics;
    let mut cache: HashMap<String, Result<Tree, String>> = HashMap::new();
    let mut entries = Vec::new();
    for (index, (id, entry)) in log.entries.iter().enumerate() {
      let current = cached_tree(self, &entry.tree, &mut cache, &mut diagnostics);
      let previous = index.checked_sub(1).and_then(|previous_index| {
        cached_tree(
          self,
          &log.entries[previous_index].1.tree,
          &mut cache,
          &mut diagnostics,
        )
      });
      let changes = match (&previous, &current) {
        (Some(old), Some(new)) => Some(counts(&diff_trees(
          Some(old),
          new,
          !entry.errors.is_empty(),
        ))),
        (None, Some(new)) if index == 0 => {
          Some(counts(&diff_trees(None, new, !entry.errors.is_empty())))
        }
        _ => None,
      };
      let file_count = current.as_ref().map_or(0, |tree| tree.files.len());
      let incomplete = !entry.errors.is_empty()
        || current.as_ref().is_some_and(|tree| {
          tree
            .files
            .iter()
            .any(|file| file.state != ReadState::Stable)
        });
      entries.push(TimelineEntry {
        id: *id,
        previous_id: index
          .checked_sub(1)
          .map(|previous_index| log.entries[previous_index].0),
        at: entry.at.clone(),
        checkpoint: entry.checkpoint.clone(),
        semantic: semantic_values[index].clone(),
        session_id: entry.session_id.clone(),
        instance_id: entry.instance_id.clone(),
        exit: entry.exit.clone(),
        config_present: entry.config_present,
        file_count,
        changes,
        incomplete,
      });
    }
    Ok(Timeline {
      schema: 1,
      entries,
      diagnostics,
      stats: self.stats_with_entry_count(log.entries.len())?,
    })
  }

  pub fn entry_changes(&self, id: u64) -> io::Result<EntryChanges> {
    let log = self.read_log()?;
    let position = log
      .entries
      .iter()
      .position(|(entry_id, _)| *entry_id == id)
      .ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::NotFound,
          format!("history entry {id} not found"),
        )
      })?;
    let (_, entry) = &log.entries[position];
    let new_tree = self.load_tree(&entry.tree)?;
    let (previous_id, old_tree) = match position.checked_sub(1) {
      Some(previous) => (
        Some(log.entries[previous].0),
        Some(self.load_tree(&log.entries[previous].1.tree)?),
      ),
      None => (None, None),
    };
    let changes = diff_trees(old_tree.as_ref(), &new_tree, !entry.errors.is_empty());
    let all = [
      (&changes.added, ChangeKind::Added),
      (&changes.modified, ChangeKind::Modified),
      (&changes.deleted, ChangeKind::Deleted),
      (&changes.uncertain, ChangeKind::Uncertain),
    ];
    let old_map = old_tree
      .as_ref()
      .map(|tree| {
        tree
          .files
          .iter()
          .map(|file| (file.path.as_str(), file))
          .collect::<HashMap<_, _>>()
      })
      .unwrap_or_default();
    let new_map = new_tree
      .files
      .iter()
      .map(|file| (file.path.as_str(), file))
      .collect::<HashMap<_, _>>();
    let mut paths = Vec::new();
    for (path_list, kind) in all {
      for path in path_list {
        paths.push((path.clone(), kind.clone()));
      }
    }
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    let files = paths
      .into_iter()
      .map(|(path, kind)| {
        let old = old_map.get(path.as_str()).map(|file| (*file).clone());
        let new = new_map.get(path.as_str()).map(|file| (*file).clone());
        let body = self.file_body(&path, old.as_ref(), new.as_ref(), &kind);
        FileChange {
          path,
          kind,
          old,
          new,
          body,
        }
      })
      .collect();
    let semantic_values = semantics(
      &log
        .entries
        .iter()
        .map(|(_, item)| item.clone())
        .collect::<Vec<_>>(),
    );
    Ok(EntryChanges {
      schema: 1,
      id,
      previous_id,
      semantic: semantic_values[position].clone(),
      files,
    })
  }

  pub fn stats(&self) -> io::Result<HistoryStats> {
    self.stats_with_entry_count(self.read_log()?.entries.len())
  }

  fn stats_with_entry_count(&self, entries: usize) -> io::Result<HistoryStats> {
    let log_path = self.history_dir().join("log.jsonl");
    let log_bytes = fs::metadata(log_path)
      .map(|metadata| metadata.len())
      .or_else(|error| {
        if error.kind() == io::ErrorKind::NotFound {
          Ok(0)
        } else {
          Err(error)
        }
      })?;
    let mut object_bytes = 0;
    let objects = self.history_dir().join("objects");
    match fs::read_dir(objects) {
      Ok(entries) => {
        for entry in entries {
          let entry = entry?;
          if entry.file_type()?.is_file() && entry.file_name().to_str().is_some_and(valid_object_id)
          {
            object_bytes += entry.metadata()?.len();
          }
        }
      }
      Err(error) if error.kind() == io::ErrorKind::NotFound => {}
      Err(error) => return Err(error),
    }
    Ok(HistoryStats {
      entries: entries as u64,
      log_bytes,
      object_bytes,
    })
  }

  fn load_tree(&self, id: &str) -> io::Result<Tree> {
    let bytes = self.load_object(id)?;
    serde_json::from_slice(&bytes)
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
  }

  fn file_body(
    &self,
    path: &str,
    old: Option<&FileEntry>,
    new: Option<&FileEntry>,
    kind: &ChangeKind,
  ) -> FileDiffBody {
    if *kind == ChangeKind::Uncertain {
      return FileDiffBody::Unavailable {
        reason: "file was unstable or unreadable during scan".into(),
      };
    }
    let old_bytes = match old {
      Some(file) => match file
        .sha1
        .as_deref()
        .and_then(|id| self.load_object(id).ok())
      {
        Some(bytes) => Some(bytes),
        None => {
          return FileDiffBody::Unavailable {
            reason: "old file object is unavailable".into(),
          }
        }
      },
      None => None,
    };
    let new_bytes = match new {
      Some(file) => match file
        .sha1
        .as_deref()
        .and_then(|id| self.load_object(id).ok())
      {
        Some(bytes) => Some(bytes),
        None => {
          return FileDiffBody::Unavailable {
            reason: "new file object is unavailable".into(),
          }
        }
      },
      None => None,
    };
    file_diff(path, old_bytes.as_deref(), new_bytes.as_deref())
  }
}

fn cached_tree(
  history: &ConfigHistory,
  id: &str,
  cache: &mut HashMap<String, Result<Tree, String>>,
  diagnostics: &mut Vec<String>,
) -> Option<Tree> {
  let value = cache.entry(id.to_string()).or_insert_with(|| {
    history
      .load_tree(id)
      .map_err(|error| format!("tree {id}: {error}"))
  });
  if let Err(error) = value {
    if !diagnostics.iter().any(|diagnostic| diagnostic == error) {
      diagnostics.push(error.clone());
    }
  }
  value.as_ref().ok().cloned()
}

fn counts(changes: &ChangeSet) -> ChangeCounts {
  ChangeCounts {
    added: changes.added.len(),
    modified: changes.modified.len(),
    deleted: changes.deleted.len(),
    uncertain: changes.uncertain.len(),
  }
}

fn validate_request(request: &CheckpointRequest) -> io::Result<()> {
  let requires_session = matches!(
    request.checkpoint,
    Checkpoint::BeforeLaunch | Checkpoint::LauncherReady | Checkpoint::AfterExit
  );
  if requires_session != request.session_id.is_some() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "session_id does not match checkpoint",
    ));
  }
  if let Some(session_id) = &request.session_id {
    if session_id.is_empty()
      || session_id.len() > 64
      || !session_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid session_id",
      ));
    }
  }
  if request.exit.is_some() && request.checkpoint != Checkpoint::AfterExit {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "exit is only valid for after_exit",
    ));
  }
  Ok(())
}

fn sha1_hex(bytes: &[u8]) -> String {
  let mut digest = Sha1::new();
  digest.update(bytes);
  hex::encode(digest.finalize())
}

fn validate_object_id(id: &str) -> io::Result<()> {
  if id.len() != 40 || !id.bytes().all(valid_hex_byte) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "invalid object SHA-1",
    ));
  }
  Ok(())
}

fn valid_object_id(id: &str) -> bool {
  id.len() == 40 && id.bytes().all(valid_hex_byte)
}

fn valid_hex_byte(byte: u8) -> bool {
  byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::Path;
  use std::time::Duration;
  use walkdir::WalkDir;

  struct TempDir(PathBuf);
  impl TempDir {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("ustbl-config-history-{}", Uuid::new_v4()));
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }
  }
  impl Drop for TempDir {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  fn request(checkpoint: Checkpoint, session_id: Option<&str>) -> CheckpointRequest {
    CheckpointRequest {
      checkpoint,
      session_id: session_id.map(str::to_string),
      instance_id: None,
      exit: None,
    }
  }

  #[test]
  fn append_read_round_trip_uses_physical_line_ids() {
    let root = TempDir::new();
    let history = ConfigHistory::new(&root.0);
    let (first, _) = history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    let (second, _) = history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    assert_eq!((first, second), (1, 2));
    assert_eq!(
      history
        .read_log()
        .unwrap()
        .entries
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>(),
      [1, 2]
    );
  }

  #[test]
  fn duplicate_file_objects_and_trees_are_reused() {
    let root = TempDir::new();
    fs::create_dir_all(root.0.join("config")).unwrap();
    fs::write(root.0.join("config/a.txt"), b"same").unwrap();
    fs::write(root.0.join("config/b.txt"), b"same").unwrap();
    let history = ConfigHistory::new(&root.0);
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    let first = fs::read_dir(history.history_dir().join("objects"))
      .unwrap()
      .count();
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    assert_eq!(
      fs::read_dir(history.history_dir().join("objects"))
        .unwrap()
        .count(),
      first
    );
    assert_eq!(first, 2);
  }

  #[test]
  fn trailing_partial_line_is_diagnosed_and_next_append_gets_new_id() {
    let root = TempDir::new();
    let history = ConfigHistory::new(&root.0);
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    let log = history.history_dir().join("log.jsonl");
    let mut file = OpenOptions::new().append(true).open(&log).unwrap();
    file.write_all(b"{\"v\":").unwrap();
    let read = history.read_log().unwrap();
    assert_eq!(read.entries.len(), 1);
    assert_eq!(read.diagnostics.len(), 1);
    let (id, _) = history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    assert_eq!(id, 3);
    assert_eq!(
      history
        .read_log()
        .unwrap()
        .entries
        .iter()
        .map(|(entry_id, _)| *entry_id)
        .collect::<Vec<_>>(),
      [1, 3]
    );
  }

  #[test]
  fn invalid_middle_line_and_version_are_skipped() {
    let root = TempDir::new();
    let history = ConfigHistory::new(&root.0);
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    let log = history.history_dir().join("log.jsonl");
    let valid = fs::read_to_string(&log).unwrap();
    let valid_line = valid.trim_end();
    let mut version =
      serde_json::from_str::<serde_json::Value>(valid.lines().next().unwrap()).unwrap();
    version["v"] = serde_json::json!(2);
    let replacement = format!("{valid_line}\n\nbad\n{valid_line}\n{version}\n");
    fs::write(&log, replacement).unwrap();
    let read = history.read_log().unwrap();
    assert_eq!(
      read
        .entries
        .iter()
        .map(|(entry_id, _)| *entry_id)
        .collect::<Vec<_>>(),
      [1, 4]
    );
    assert_eq!(read.diagnostics.len(), 2);
  }

  #[test]
  fn request_validation_happens_before_disk_creation() {
    let root = TempDir::new();
    let history = ConfigHistory::new(&root.0);
    for invalid in [
      CheckpointRequest {
        checkpoint: Checkpoint::ManualScan,
        session_id: Some("x".into()),
        instance_id: None,
        exit: None,
      },
      CheckpointRequest {
        checkpoint: Checkpoint::BeforeLaunch,
        session_id: None,
        instance_id: None,
        exit: None,
      },
      CheckpointRequest {
        checkpoint: Checkpoint::ManualScan,
        session_id: None,
        instance_id: None,
        exit: Some(ProcessExit {
          code: None,
          success: false,
        }),
      },
    ] {
      assert_eq!(
        history.append(invalid).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
      );
    }
    assert!(!history.history_dir().join("log.jsonl").exists());
  }

  #[test]
  fn load_object_rejects_bad_ids_and_modified_content() {
    let root = TempDir::new();
    let history = ConfigHistory::new(&root.0);
    let id = history.write_object(b"data").unwrap();
    for bad in ["../x", "A00000000000000000000000000000000000000", "short"] {
      assert_eq!(
        history.load_object(bad).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
      );
    }
    fs::write(history.history_dir().join("objects").join(&id), b"changed").unwrap();
    assert_eq!(
      history.load_object(&id).unwrap_err().kind(),
      io::ErrorKind::InvalidData
    );
  }

  #[test]
  fn entry_changes_reports_missing_file_object_as_unavailable() {
    let root = TempDir::new();
    fs::create_dir_all(root.0.join("config")).unwrap();
    fs::write(root.0.join("config/a.txt"), b"old").unwrap();
    let history = ConfigHistory::new(&root.0);
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    fs::write(root.0.join("config/a.txt"), b"new").unwrap();
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    let entries = history.read_log().unwrap().entries;
    let first_tree = history.load_object(&entries[0].1.tree).unwrap();
    let tree: Tree = serde_json::from_slice(&first_tree).unwrap();
    fs::remove_file(
      history
        .history_dir()
        .join("objects")
        .join(tree.files[0].sha1.as_ref().unwrap()),
    )
    .unwrap();
    let changes = history.entry_changes(entries[1].0).unwrap();
    assert!(matches!(
      changes.files[0].body,
      FileDiffBody::Unavailable { .. }
    ));
  }

  #[test]
  fn rewriting_same_content_between_appends_is_not_modified() {
    let root = TempDir::new();
    fs::create_dir_all(root.0.join("config")).unwrap();
    let path = root.0.join("config/a.txt");
    fs::write(&path, b"same").unwrap();
    let history = ConfigHistory::new(&root.0);
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    fs::write(&path, b"same").unwrap();
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    let timeline = history.timeline().unwrap();
    let changes = timeline.entries[1].changes.as_ref().unwrap();
    assert_eq!(
      (changes.added, changes.modified, changes.deleted),
      (0, 0, 0)
    );
    assert_eq!(changes.uncertain, 0);
  }

  #[test]
  fn delete_then_reappear_is_deleted_then_added() {
    let root = TempDir::new();
    fs::create_dir_all(root.0.join("config")).unwrap();
    let path = root.0.join("config/a.txt");
    fs::write(&path, b"same").unwrap();
    let history = ConfigHistory::new(&root.0);
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    fs::remove_file(&path).unwrap();
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    fs::write(&path, b"same").unwrap();
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    let entries = history.read_log().unwrap().entries;
    let deleted = history.entry_changes(entries[1].0).unwrap();
    let added = history.entry_changes(entries[2].0).unwrap();
    assert_eq!(deleted.files[0].kind, ChangeKind::Deleted);
    assert_eq!(added.files[0].kind, ChangeKind::Added);
  }

  #[test]
  fn timeline_reports_missing_tree_with_diagnostic() {
    let root = TempDir::new();
    fs::create_dir_all(root.0.join("config")).unwrap();
    fs::write(root.0.join("config/a.txt"), b"one").unwrap();
    let history = ConfigHistory::new(&root.0);
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    fs::write(root.0.join("config/a.txt"), b"two").unwrap();
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    let entry = history.read_log().unwrap().entries[0].1.clone();
    fs::remove_file(history.history_dir().join("objects").join(entry.tree)).unwrap();
    let timeline = history.timeline().unwrap();
    assert!(timeline.entries[1].changes.is_none());
    assert!(!timeline.diagnostics.is_empty());
  }

  #[test]
  fn timeline_marks_too_large_file_incomplete() {
    let root = TempDir::new();
    fs::create_dir_all(root.0.join("config")).unwrap();
    fs::write(
      root.0.join("config/large.bin"),
      vec![b'x'; MAX_CONFIG_FILE_BYTES as usize + 1],
    )
    .unwrap();
    let history = ConfigHistory::new(&root.0);
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    let timeline = history.timeline().unwrap();
    assert!(timeline.entries[0].incomplete);
    let changes = timeline.entries[0].changes.as_ref().unwrap();
    assert_eq!(changes.uncertain, 1);
  }

  #[test]
  fn concurrent_appends_are_all_parseable() {
    let root = TempDir::new();
    let root_path = root.0.clone();
    let handles = (0..2)
      .map(|_| {
        let root = root_path.clone();
        std::thread::spawn(move || {
          let history = ConfigHistory::new(root);
          for _ in 0..10 {
            history
              .append(request(Checkpoint::ManualScan, None))
              .unwrap();
          }
        })
      })
      .collect::<Vec<_>>();
    for handle in handles {
      handle.join().unwrap();
    }
    assert_eq!(
      ConfigHistory::new(&root.0)
        .read_log()
        .unwrap()
        .entries
        .len(),
      20
    );
  }

  #[test]
  fn config_is_unchanged_by_repeated_scans_and_timeline_reports_stats() {
    let root = TempDir::new();
    fs::create_dir_all(root.0.join("config/nested/deeper")).unwrap();
    fs::write(root.0.join("config/nested/a.txt"), b"a").unwrap();
    fs::write(root.0.join("config/nested/deeper/b.txt"), b"b").unwrap();
    let before = snapshot_config(&root.0.join("config"));
    let history = ConfigHistory::new(&root.0);
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    history
      .append(request(Checkpoint::ManualScan, None))
      .unwrap();
    assert_eq!(snapshot_config(&root.0.join("config")), before);
    let timeline = history.timeline().unwrap();
    assert_eq!(timeline.entries[1].previous_id, Some(1));
    assert_eq!(timeline.entries[0].changes.as_ref().unwrap().added, 2);
    assert_eq!(timeline.entries[1].changes.as_ref().unwrap().added, 0);
    assert!(!timeline.entries[0].incomplete);
    assert_eq!(timeline.stats.entries, 2);
    assert_eq!(
      timeline.stats.log_bytes,
      fs::metadata(history.history_dir().join("log.jsonl"))
        .unwrap()
        .len()
    );
    assert!(timeline.stats.object_bytes > 0);
    std::thread::sleep(Duration::from_millis(1));
  }

  fn snapshot_config(config: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = WalkDir::new(config)
      .follow_links(false)
      .into_iter()
      .filter_map(Result::ok)
      .filter(|entry| entry.file_type().is_file())
      .map(|entry| {
        let relative = entry
          .path()
          .strip_prefix(config)
          .unwrap()
          .components()
          .map(|component| component.as_os_str().to_str().unwrap())
          .collect::<Vec<_>>()
          .join("/");
        (relative, fs::read(entry.path()).unwrap())
      })
      .collect::<Vec<_>>();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
  }
}
