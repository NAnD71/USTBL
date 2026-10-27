use super::diff::{derive_semantic, diff_trees};
use super::model::{
  ActiveSession, ChangeSummary, Checkpoint, CheckpointRequest, CommitListEntry, CommitListStatus,
  ConfigCommit, ConfigTree, HistoryState, SemanticContext,
};
use super::scan::{scan_config_with_objects, tree_id_for, validate_paths};
use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha1::{Digest, Sha1};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

pub const HISTORY_DIR_NAME: &str = "config-history";

static HISTORY_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

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
    Ok(self.load_state_recovering()?.0)
  }

  /// Read state while retaining a usable view of valid commit files after a
  /// missing or malformed state file. The warning is intentionally returned to
  /// the caller so lifecycle integration can log it without blocking launch.
  pub fn load_state_recovering(&self) -> io::Result<(HistoryState, Option<String>)> {
    let stored = match read_json::<HistoryState>(&self.state_path()) {
      Ok(state) => Some(state),
      Err(error)
        if matches!(
          error.kind(),
          io::ErrorKind::NotFound | io::ErrorKind::InvalidData
        ) =>
      {
        None
      }
      Err(error) => return Err(error),
    };
    let (reconciled, diagnostics) = self.reconcile_state(stored.as_ref())?;
    let changed = stored.as_ref() != Some(&reconciled);
    let warning = changed || stored.is_none() || !diagnostics.is_empty();
    let warning = warning.then(|| {
      if diagnostics.is_empty() {
        "configuration history state was recovered or reconciled".to_string()
      } else {
        format!(
          "configuration history state was recovered or reconciled: {}",
          diagnostics.join("; ")
        )
      }
    });
    Ok((reconciled, warning))
  }

  pub fn load_commit(&self, id: &str) -> io::Result<ConfigCommit> {
    validate_commit_id(id)?;
    let commit: ConfigCommit = read_json(&self.commit_path(id))?;
    validate_commit(&commit)?;
    if commit.id != id {
      return Err(invalid_data("commit id does not match its path"));
    }
    Ok(commit)
  }

  pub fn list_commits(&self) -> io::Result<Vec<CommitListEntry>> {
    let mut entries = Vec::new();
    if !self.commits_dir().exists() {
      return Ok(entries);
    }
    for entry in fs::read_dir(self.commits_dir())? {
      let entry = entry?;
      if entry.file_type()?.is_file()
        && entry
          .path()
          .extension()
          .is_some_and(|extension| extension == "json")
      {
        let entry_path = entry.path();
        let id = entry_path
          .file_stem()
          .and_then(|value| value.to_str())
          .unwrap_or("<invalid-name>")
          .to_string();
        let parsed = read_json::<ConfigCommit>(&entry_path);
        let (commit, status, diagnostic) = match parsed {
          Err(error) => (None, CommitListStatus::Corrupt, Some(error.to_string())),
          Ok(commit) => {
            let result = (|| {
              validate_commit(&commit)?;
              if commit.id != id {
                return Err(invalid_data("commit id does not match its path"));
              }
              self.load_tree(&commit.tree_id)?;
              Ok::<_, io::Error>(())
            })();
            match result {
              Ok(()) => (Some(commit), CommitListStatus::Expandable, None),
              Err(error) => (
                Some(commit),
                CommitListStatus::Corrupt,
                Some(error.to_string()),
              ),
            }
          }
        };
        entries.push(CommitListEntry {
          id,
          commit,
          status,
          diagnostic,
        });
      }
    }
    let mut expandable_ids: BTreeSet<String> = entries
      .iter()
      .filter(|entry| entry.status == CommitListStatus::Expandable)
      .map(|entry| entry.id.clone())
      .collect();
    loop {
      let invalid_parents: Vec<_> = entries
        .iter()
        .filter(|entry| entry.status == CommitListStatus::Expandable)
        .filter_map(|entry| {
          entry
            .commit
            .as_ref()
            .and_then(|commit| commit.parent_id.as_ref())
            .filter(|parent| !expandable_ids.contains(*parent))
            .map(|parent| (entry.id.clone(), parent.clone()))
        })
        .collect();
      if invalid_parents.is_empty() {
        break;
      }
      for (id, parent) in invalid_parents {
        expandable_ids.remove(&id);
        if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
          entry.status = CommitListStatus::Corrupt;
          entry.diagnostic = Some(format!("missing or corrupt parent commit: {parent}"));
        }
      }
    }
    entries.sort_by(|left, right| {
      left
        .commit
        .as_ref()
        .map(|commit| commit.created_at.as_str())
        .unwrap_or("")
        .cmp(
          &right
            .commit
            .as_ref()
            .map(|commit| commit.created_at.as_str())
            .unwrap_or(""),
        )
        .then_with(|| left.id.cmp(&right.id))
    });
    Ok(entries)
  }

  pub fn load_tree(&self, id: &str) -> io::Result<ConfigTree> {
    validate_tree_id(id)?;
    let tree: ConfigTree = read_json(&self.tree_path(id))?;
    validate_tree(&tree)?;
    for file in &tree.files {
      if file.read_state == super::model::ReadState::Stable
        && self.load_object(&file.sha1)?.is_none()
      {
        return Err(invalid_data("tree references a missing object"));
      }
    }
    if tree.id != id {
      return Err(invalid_data("tree id does not match its path"));
    }
    Ok(tree)
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
      Ok(bytes) => {
        if hex::encode(Sha1::digest(&bytes)) != sha1 {
          return Err(invalid_data("object SHA-1 does not match its path"));
        }
        Ok(Some(bytes))
      }
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
    let overlap = if request.checkpoint == Checkpoint::Recovered {
      state
        .active_sessions
        .values()
        .any(|session| session.ambiguous)
        || state.active_sessions.len() > 1
    } else {
      state.active_sessions.iter().any(|(id, session)| {
        session.ambiguous || request.session_id.as_deref() != Some(id.as_str())
      })
    };
    let session_active = request
      .session_id
      .as_deref()
      .is_some_and(|session_id| state.active_sessions.contains_key(session_id));
    let inactive_lifecycle_checkpoint = matches!(
      request.checkpoint,
      Checkpoint::LauncherReady | Checkpoint::AfterExit
    ) && !session_active;
    if overlap {
      for session in state.active_sessions.values_mut() {
        session.ambiguous = true;
      }
    }
    let checkpoint = if overlap || inactive_lifecycle_checkpoint {
      Checkpoint::Ambiguous
    } else {
      request.checkpoint
    };
    let semantic = derive_semantic(
      parent.as_ref(),
      checkpoint,
      request.session_id.as_deref(),
      SemanticContext {
        overlapping_sessions: overlap,
        scan_incomplete: scanned.tree.incomplete || !changed.unstable.is_empty(),
        session_active,
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
      requested_checkpoint: Some(request.checkpoint),
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
    state.revision = state.revision.saturating_add(1);
    match (request.checkpoint, request.session_id) {
      (Checkpoint::BeforeLaunch, Some(session_id)) => {
        state.active_sessions.insert(
          session_id.clone(),
          ActiveSession {
            session_id,
            instance_id: commit.instance_id.clone(),
            started_at: commit.created_at.clone(),
            ambiguous: overlap,
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
    let overlapping = !state.active_sessions.is_empty();
    if overlapping {
      for session in state.active_sessions.values_mut() {
        session.ambiguous = true;
      }
    }
    let now = Utc::now().to_rfc3339();
    state.active_sessions.insert(
      session_id.to_string(),
      ActiveSession {
        session_id: session_id.to_string(),
        instance_id,
        started_at: now,
        ambiguous: overlapping,
      },
    );
    state.revision = state.revision.saturating_add(1);
    self.ensure_layout()?;
    self.write_json_atomic(&self.state_path(), &state)
  }

  pub fn finish_session(&self, session_id: &str) -> io::Result<()> {
    let lock = lock_for(&self.history_dir);
    let _guard = lock.lock().expect("config history lock poisoned");
    self.validate_history_location()?;
    let mut state = self.load_state()?;
    state.active_sessions.remove(session_id);
    state.revision = state.revision.saturating_add(1);
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
    let (_root, config) = validate_paths(&self.game_root, &self.config_dir)?;
    let history = stable_path_key(&self.history_dir)?;
    if same_or_inside_path(&history, &config) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "config history must not be stored inside config",
      ));
    }
    Ok(())
  }

  fn recover_state(&self) -> io::Result<HistoryState> {
    Ok(self.reconcile_state(None)?.0)
  }

  fn reconcile_state(
    &self,
    stored: Option<&HistoryState>,
  ) -> io::Result<(HistoryState, Vec<String>)> {
    let (commits, diagnostics) = self.valid_commits()?;
    let by_id: HashMap<_, _> = commits
      .iter()
      .map(|commit| (commit.id.as_str(), commit))
      .collect();
    let mut tips = Vec::new();
    for commit in &commits {
      if !commits
        .iter()
        .any(|child| child.parent_id.as_deref() == Some(commit.id.as_str()))
        && chain_depth(commit, &by_id).is_some()
      {
        tips.push(commit);
      }
    }
    tips.sort_by(|left, right| {
      chain_depth(left, &by_id)
        .unwrap_or(0)
        .cmp(&chain_depth(right, &by_id).unwrap_or(0))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.id.cmp(&right.id))
    });
    let chain = tips
      .last()
      .map(|tip| collect_chain(tip, &by_id))
      .unwrap_or_default();
    let stored_head_index = stored
      .and_then(|state| state.head.as_deref())
      .and_then(|head| chain.iter().position(|commit| commit.id == head));
    let mut active_sessions = if let Some(state) = stored {
      if stored_head_index.is_some() || (state.head.is_none() && chain.is_empty()) {
        state.active_sessions.clone()
      } else {
        Default::default()
      }
    } else {
      Default::default()
    };
    let replay_from = stored_head_index.map_or(0, |index| index.saturating_add(1));
    for commit in chain.iter().skip(replay_from) {
      replay_session_state(&mut active_sessions, commit);
    }
    Ok((
      HistoryState {
        schema: 1,
        head: chain.last().map(|commit| commit.id.clone()),
        next_sequence: chain.len() as u64,
        commit_count: chain.len() as u64,
        revision: stored.map_or(chain.len() as u64, |state| state.revision),
        active_sessions,
      },
      diagnostics,
    ))
  }

  fn valid_commits(&self) -> io::Result<(Vec<ConfigCommit>, Vec<String>)> {
    let mut commits = Vec::new();
    let mut diagnostics = Vec::new();
    for entry in self.list_commits()? {
      match (entry.status, entry.commit, entry.diagnostic) {
        (CommitListStatus::Expandable, Some(commit), _) => commits.push(commit),
        (_, _, diagnostic) => diagnostics.push(format!(
          "commit {} is not trusted: {}",
          entry.id,
          diagnostic.unwrap_or_else(|| "invalid commit reference".to_string())
        )),
      }
    }
    Ok((commits, diagnostics))
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

fn invalid_data(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

fn validate_commit_id(value: &str) -> io::Result<()> {
  validate_hex_id(value, 32, "commit")
}

fn validate_tree_id(value: &str) -> io::Result<()> {
  validate_hex_id(value, 40, "tree")
}

fn validate_hex_id(value: &str, length: usize, kind: &str) -> io::Result<()> {
  if value.len() != length
    || !value
      .bytes()
      .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
  {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      format!("invalid {kind} id"),
    ));
  }
  Ok(())
}

fn validate_commit(commit: &ConfigCommit) -> io::Result<()> {
  validate_commit_id(&commit.id)?;
  if let Some(parent_id) = &commit.parent_id {
    validate_commit_id(parent_id)?;
  }
  validate_tree_id(&commit.tree_id)
}

fn validate_tree(tree: &ConfigTree) -> io::Result<()> {
  validate_tree_id(&tree.id)?;
  if tree_id_for(tree) != tree.id {
    return Err(invalid_data("tree id does not match tree contents"));
  }
  for file in &tree.files {
    validate_relative_path(&file.path)?;
    match file.read_state {
      super::model::ReadState::Stable => {
        validate_sha1(&file.sha1)?;
        if file.object.as_deref() != Some(file.sha1.as_str()) {
          return Err(invalid_data("stable file object does not match SHA-1"));
        }
      }
      super::model::ReadState::Unstable | super::model::ReadState::Unreadable => {
        if !file.sha1.is_empty() || file.object.is_some() {
          return Err(invalid_data("unstable file has a content object"));
        }
      }
    }
    if file.file_type != super::model::ConfigFileType::Regular {
      return Err(invalid_data("unsupported config file type"));
    }
  }
  Ok(())
}

fn validate_relative_path(value: &str) -> io::Result<()> {
  if value.is_empty() || value.contains('\\') || value.starts_with('/') || value.ends_with('/') {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "invalid config file path",
    ));
  }
  let path = Path::new(value);
  if path.is_absolute()
    || path.components().any(|component| {
      matches!(
        component,
        std::path::Component::ParentDir
          | std::path::Component::RootDir
          | std::path::Component::Prefix(_)
          | std::path::Component::CurDir
      )
    })
  {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "unsafe config file path",
    ));
  }
  Ok(())
}

fn validate_sha1(value: &str) -> io::Result<()> {
  if value.len() != 40
    || !value
      .bytes()
      .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
  {
    return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid SHA-1"));
  }
  Ok(())
}

fn replay_session_state(
  sessions: &mut std::collections::BTreeMap<String, ActiveSession>,
  commit: &ConfigCommit,
) {
  let requested = commit.requested_checkpoint.unwrap_or(commit.checkpoint);
  let overlap = if requested == Checkpoint::Recovered {
    sessions.values().any(|session| session.ambiguous) || sessions.len() > 1
  } else {
    sessions
      .iter()
      .any(|(id, session)| session.ambiguous || commit.session_id.as_deref() != Some(id.as_str()))
  };
  if overlap {
    for session in sessions.values_mut() {
      session.ambiguous = true;
    }
  }
  match (requested, commit.session_id.as_deref()) {
    (Checkpoint::BeforeLaunch, Some(session_id)) => {
      sessions.insert(
        session_id.to_string(),
        ActiveSession {
          session_id: session_id.to_string(),
          instance_id: commit.instance_id.clone(),
          started_at: commit.created_at.clone(),
          ambiguous: overlap,
        },
      );
    }
    (Checkpoint::AfterExit, Some(session_id)) => {
      sessions.remove(session_id);
    }
    (Checkpoint::Recovered, _) => sessions.clear(),
    _ => {}
  }
}

fn chain_depth(tip: &ConfigCommit, by_id: &HashMap<&str, &ConfigCommit>) -> Option<usize> {
  let mut current = tip;
  let mut depth = 1;
  let mut visited = std::collections::BTreeSet::new();
  loop {
    if !visited.insert(current.id.as_str()) {
      return None;
    }
    let Some(parent_id) = current.parent_id.as_deref() else {
      return Some(depth);
    };
    current = by_id.get(parent_id).copied()?;
    depth += 1;
  }
}

fn collect_chain<'a>(
  tip: &'a ConfigCommit,
  by_id: &HashMap<&str, &'a ConfigCommit>,
) -> Vec<&'a ConfigCommit> {
  let mut result = Vec::new();
  let mut current = tip;
  result.push(current);
  while let Some(parent_id) = current.parent_id.as_deref() {
    current = by_id[parent_id];
    result.push(current);
  }
  result.reverse();
  result
}

fn stable_path_key(path: &Path) -> io::Result<PathBuf> {
  let input = if path.is_absolute() {
    path.to_path_buf()
  } else {
    std::env::current_dir()?.join(path)
  };
  let mut missing = Vec::new();
  let mut existing = input.as_path();
  while !existing.exists() {
    let name = existing
      .file_name()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    missing.push(name.to_owned());
    existing = existing
      .parent()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no existing parent"))?;
  }
  let mut result = existing.canonicalize()?;
  for name in missing.iter().rev() {
    result.push(name);
  }
  Ok(result)
}

fn same_or_inside_path(path: &Path, base: &Path) -> bool {
  #[cfg(windows)]
  {
    let path = path
      .to_string_lossy()
      .replace('\\', "/")
      .to_ascii_lowercase();
    let base = base
      .to_string_lossy()
      .replace('\\', "/")
      .to_ascii_lowercase();
    path == base || path.starts_with(&(base + "/"))
  }
  #[cfg(not(windows))]
  {
    path == base || path.starts_with(base)
  }
}

fn lock_key(path: &Path) -> String {
  let path = stable_path_key(path).unwrap_or_else(|_| {
    let input = if path.is_absolute() {
      path.to_path_buf()
    } else {
      std::env::current_dir()
        .map(|current| current.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
    };
    input
  });
  let key = path.to_string_lossy().replace('\\', "/");
  #[cfg(windows)]
  {
    key.to_ascii_lowercase()
  }
  #[cfg(not(windows))]
  {
    key
  }
}

fn lock_for(path: &Path) -> Arc<Mutex<()>> {
  let key = lock_key(path);
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
