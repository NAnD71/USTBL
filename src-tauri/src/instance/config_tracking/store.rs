use super::diff::{derive_semantic, diff_trees};
use super::model::{
  ActiveSession, ChangeSummary, Checkpoint, CheckpointRequest, ConfigCommit, ConfigTree,
  HistoryState, SemanticContext,
};
use super::scan::scan_config_with_objects;
use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

pub const HISTORY_DIR_NAME: &str = "config-history";

static HISTORY_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryStats {
  pub commit_count: u64,
  pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct HistoryStore {
  pub game_root: PathBuf,
  pub config_dir: PathBuf,
  pub history_dir: PathBuf,
}

#[allow(dead_code)]
impl HistoryStore {
  pub fn new(
    game_root: impl Into<PathBuf>,
    config_dir: impl Into<PathBuf>,
    history_dir: impl Into<PathBuf>,
  ) -> Self {
    Self {
      game_root: game_root.into(),
      config_dir: config_dir.into(),
      history_dir: history_dir.into(),
    }
  }

  pub fn default_history_path(game_root: &Path) -> PathBuf {
    game_root.join(".ustbl-update").join(HISTORY_DIR_NAME)
  }

  pub fn load_state(&self) -> io::Result<HistoryState> {
    match read_json::<HistoryState>(&self.state_path()) {
      Ok(state) => Ok(state),
      Err(error) if error.kind() == io::ErrorKind::NotFound => self.recover_state(),
      Err(error) => Err(error),
    }
  }

  /// Read state while retaining a usable view of valid commit files after a
  /// missing or malformed state file. The warning is intentionally returned to
  /// the caller so lifecycle integration can log it without blocking launch.
  pub fn load_state_recovering(&self) -> io::Result<(HistoryState, Option<String>)> {
    match read_json::<HistoryState>(&self.state_path()) {
      Ok(state) => Ok((state, None)),
      Err(error)
        if matches!(
          error.kind(),
          io::ErrorKind::NotFound | io::ErrorKind::InvalidData
        ) =>
      {
        Ok((self.recover_state()?, Some(error.to_string())))
      }
      Err(error) => Err(error),
    }
  }

  pub fn load_commit(&self, id: &str) -> io::Result<ConfigCommit> {
    read_json(&self.commit_path(id))
  }

  pub fn list_commits(&self) -> io::Result<Vec<ConfigCommit>> {
    let mut commits = Vec::new();
    if !self.commits_dir().exists() {
      return Ok(commits);
    }
    for entry in fs::read_dir(self.commits_dir())? {
      let entry = entry?;
      if entry.file_type()?.is_file()
        && entry
          .path()
          .extension()
          .is_some_and(|extension| extension == "json")
      {
        commits.push(read_json(&entry.path())?);
      }
    }
    commits.sort_by(|left: &ConfigCommit, right| {
      left
        .created_at
        .cmp(&right.created_at)
        .then_with(|| left.id.cmp(&right.id))
    });
    Ok(commits)
  }

  pub fn load_tree(&self, id: &str) -> io::Result<ConfigTree> {
    read_json(&self.tree_path(id))
  }

  pub fn diff_commits(&self, old_id: Option<&str>, new_id: &str) -> io::Result<ChangeSummary> {
    let new = self.load_commit(new_id)?;
    let new_tree = self.load_tree(&new.tree_id)?;
    let old_tree = old_id
      .map(|id| self.load_commit(id))
      .transpose()?
      .map(|commit| self.load_tree(&commit.tree_id))
      .transpose()?;
    Ok(diff_trees(old_tree.as_ref(), &new_tree))
  }

  pub fn load_object(&self, sha1: &str) -> io::Result<Option<Vec<u8>>> {
    validate_sha1(sha1)?;
    match fs::read(self.object_path(sha1)) {
      Ok(bytes) => Ok(Some(bytes)),
      Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
      Err(error) => Err(error),
    }
  }

  pub fn checkpoint(&self, request: CheckpointRequest) -> io::Result<ConfigCommit> {
    let lock = lock_for(&self.history_dir);
    let _guard = lock.lock().expect("config history lock poisoned");
    self.validate_history_location()?;
    let mut state = self.load_state()?;
    let scanned = scan_config_with_objects(&self.game_root, &self.config_dir)?;
    self.ensure_layout()?;
    for (sha1, bytes) in &scanned.objects {
      self.store_object(sha1, bytes)?;
    }
    let parent = state
      .head
      .as_deref()
      .map(|id| self.load_commit(id))
      .transpose()?;
    let old_tree = parent
      .as_ref()
      .map(|commit| self.load_tree(&commit.tree_id))
      .transpose()?;
    let changed = diff_trees(old_tree.as_ref(), &scanned.tree);
    let overlap = request.checkpoint != Checkpoint::Recovered
      && state
        .active_sessions
        .keys()
        .any(|id| request.session_id.as_deref() != Some(id.as_str()));
    let checkpoint = if overlap {
      Checkpoint::Ambiguous
    } else {
      request.checkpoint
    };
    let semantic = derive_semantic(
      parent.as_ref(),
      checkpoint,
      SemanticContext {
        overlapping_sessions: overlap,
        scan_incomplete: scanned.tree.incomplete || !changed.unstable.is_empty(),
      },
    );
    let commit = ConfigCommit {
      id: Uuid::new_v4().simple().to_string(),
      parent_id: state.head.clone(),
      tree_id: scanned.tree.id.clone(),
      created_at: Utc::now().to_rfc3339(),
      game_root_key: self
        .game_root
        .canonicalize()
        .unwrap_or_else(|_| self.game_root.clone())
        .to_string_lossy()
        .replace('\\', "/"),
      instance_id: request.instance_id,
      session_id: request.session_id.clone(),
      checkpoint,
      semantic,
      process_exit: request.process_exit,
      changed,
      incomplete: scanned.tree.incomplete
        || !scanned
          .tree
          .files
          .iter()
          .all(|file| file.read_state == super::model::ReadState::Stable),
    };
    self.write_json_atomic(&self.tree_path(&scanned.tree.id), &scanned.tree)?;
    self.write_json_atomic(&self.commit_path(&commit.id), &commit)?;
    state.head = Some(commit.id.clone());
    state.next_sequence = state.next_sequence.saturating_add(1);
    state.commit_count = state.commit_count.saturating_add(1);
    match (request.checkpoint, request.session_id) {
      (Checkpoint::BeforeLaunch, Some(session_id)) => {
        state.active_sessions.insert(
          session_id.clone(),
          ActiveSession {
            session_id,
            instance_id: commit.instance_id.clone(),
            started_at: commit.created_at.clone(),
          },
        );
      }
      (Checkpoint::AfterExit, Some(session_id)) => {
        state.active_sessions.remove(&session_id);
      }
      (Checkpoint::Recovered, _) => state.active_sessions.clear(),
      _ => {}
    }
    self.write_json_atomic(&self.state_path(), &state)?;
    Ok(commit)
  }

  pub fn start_session(&self, session_id: &str, instance_id: Option<String>) -> io::Result<()> {
    let lock = lock_for(&self.history_dir);
    let _guard = lock.lock().expect("config history lock poisoned");
    self.validate_history_location()?;
    let mut state = self.load_state()?;
    let now = Utc::now().to_rfc3339();
    state.active_sessions.insert(
      session_id.to_string(),
      ActiveSession {
        session_id: session_id.to_string(),
        instance_id,
        started_at: now,
      },
    );
    self.ensure_layout()?;
    self.write_json_atomic(&self.state_path(), &state)
  }

  pub fn finish_session(&self, session_id: &str) -> io::Result<()> {
    let lock = lock_for(&self.history_dir);
    let _guard = lock.lock().expect("config history lock poisoned");
    let mut state = self.load_state()?;
    state.active_sessions.remove(session_id);
    self.ensure_layout()?;
    self.write_json_atomic(&self.state_path(), &state)
  }

  pub fn stats(&self) -> io::Result<HistoryStats> {
    let state = self.load_state()?;
    let mut bytes: u64 = 0;
    if self.history_dir.exists() {
      for entry in walk_files(&self.history_dir)? {
        bytes = bytes.saturating_add(entry.metadata()?.len());
      }
    }
    Ok(HistoryStats {
      commit_count: state.commit_count,
      bytes,
    })
  }

  fn ensure_layout(&self) -> io::Result<()> {
    fs::create_dir_all(self.commits_dir())?;
    fs::create_dir_all(self.trees_dir())?;
    fs::create_dir_all(self.objects_dir())?;
    Ok(())
  }

  fn validate_history_location(&self) -> io::Result<()> {
    let config = self
      .config_dir
      .canonicalize()
      .unwrap_or_else(|_| self.config_dir.clone());
    let history_input = if self.history_dir.is_absolute() {
      self.history_dir.clone()
    } else {
      std::env::current_dir()?.join(&self.history_dir)
    };
    let history = if history_input.exists() {
      history_input.canonicalize().unwrap_or(history_input)
    } else {
      let parent = history_input.parent().unwrap_or(Path::new("."));
      parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf())
        .join(history_input.file_name().unwrap_or_default())
    };
    if history == config || history.starts_with(&config) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "config history must not be stored inside config",
      ));
    }
    Ok(())
  }

  fn recover_state(&self) -> io::Result<HistoryState> {
    let mut commits = Vec::new();
    if self.commits_dir().exists() {
      for entry in fs::read_dir(self.commits_dir())? {
        let entry = entry?;
        if entry.file_type()?.is_file()
          && entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "json")
        {
          if let Ok(commit) = read_json::<ConfigCommit>(&entry.path()) {
            commits.push(commit);
          }
        }
      }
    }
    commits.sort_by(|left, right| {
      left
        .created_at
        .cmp(&right.created_at)
        .then_with(|| left.id.cmp(&right.id))
    });
    Ok(HistoryState {
      schema: 1,
      head: commits.last().map(|commit| commit.id.clone()),
      next_sequence: commits.len() as u64,
      commit_count: commits.len() as u64,
      active_sessions: Default::default(),
    })
  }

  fn store_object(&self, sha1: &str, bytes: &[u8]) -> io::Result<()> {
    validate_sha1(sha1)?;
    if hex::encode(Sha1::digest(bytes)) != sha1 {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "object SHA-1 mismatch",
      ));
    }
    let target = self.object_path(sha1);
    if target.exists() {
      if fs::read(&target)? != bytes {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "object content mismatch",
        ));
      }
      return Ok(());
    }
    self.write_bytes_atomic(&target, bytes)
  }

  fn write_json_atomic<T: Serialize>(&self, target: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    self.write_bytes_atomic(target, &bytes)
  }

  fn write_bytes_atomic(&self, target: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = target
      .parent()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
      ".{}.{}.tmp",
      target.file_name().unwrap().to_string_lossy(),
      Uuid::new_v4()
    ));
    let result = (|| {
      let mut file = fs::File::create(&temporary)?;
      use std::io::Write;
      file.write_all(bytes)?;
      file.sync_all()?;
      atomic_replace(&temporary, target)
    })();
    let _ = fs::remove_file(&temporary);
    result
  }

  fn state_path(&self) -> PathBuf {
    self.history_dir.join("state.json")
  }
  fn commits_dir(&self) -> PathBuf {
    self.history_dir.join("commits")
  }
  fn trees_dir(&self) -> PathBuf {
    self.history_dir.join("trees")
  }
  fn objects_dir(&self) -> PathBuf {
    self.history_dir.join("objects")
  }
  fn commit_path(&self, id: &str) -> PathBuf {
    self.commits_dir().join(format!("{id}.json"))
  }
  fn tree_path(&self, id: &str) -> PathBuf {
    self.trees_dir().join(format!("{id}.json"))
  }
  fn object_path(&self, sha1: &str) -> PathBuf {
    self.objects_dir().join(sha1)
  }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<T> {
  let bytes = fs::read(path)?;
  serde_json::from_slice(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn validate_sha1(value: &str) -> io::Result<()> {
  if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid SHA-1"));
  }
  Ok(())
}

fn lock_for(path: &Path) -> Arc<Mutex<()>> {
  let key = path
    .canonicalize()
    .unwrap_or_else(|_| path.to_path_buf())
    .to_string_lossy()
    .replace('\\', "/")
    .into();
  let locks = HISTORY_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
  let mut locks = locks.lock().expect("config history locks poisoned");
  locks
    .entry(key)
    .or_insert_with(|| Arc::new(Mutex::new(())))
    .clone()
}

fn walk_files(root: &Path) -> io::Result<Vec<fs::DirEntry>> {
  let mut result = Vec::new();
  for entry in fs::read_dir(root)? {
    let entry = entry?;
    if entry.file_type()?.is_dir() {
      result.extend(walk_files(&entry.path())?);
    } else if entry.file_type()?.is_file() {
      result.push(entry);
    }
  }
  Ok(result)
}

#[cfg(unix)]
fn atomic_replace(from: &Path, to: &Path) -> io::Result<()> {
  fs::rename(from, to)
}

#[cfg(windows)]
fn atomic_replace(from: &Path, to: &Path) -> io::Result<()> {
  use std::os::windows::ffi::OsStrExt;
  use winapi::um::winbase::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
  let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
  let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
  let result = unsafe {
    MoveFileExW(
      from.as_ptr(),
      to.as_ptr(),
      MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
    )
  };
  if result == 0 {
    Err(io::Error::last_os_error())
  } else {
    Ok(())
  }
}
