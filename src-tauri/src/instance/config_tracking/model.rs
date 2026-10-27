use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Checkpoint {
  FirstObserved,
  BeforeLaunch,
  LauncherReady,
  AfterExit,
  ManualScan,
  Recovered,
  Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Semantic {
  InitialObservation,
  DuringLoading,
  DuringRuntime,
  BetweenSessions,
  InterruptedOrUnknown,
  Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadState {
  Stable,
  Unstable,
  Unreadable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigFileType {
  Regular,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigFile {
  pub path: String,
  pub sha1: String,
  pub size: u64,
  pub object: Option<String>,
  pub file_type: ConfigFileType,
  pub read_state: ReadState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigTree {
  pub id: String,
  pub files: Vec<ConfigFile>,
  pub incomplete: bool,
  #[serde(default)]
  pub scan_errors: Vec<String>,
}

impl ConfigTree {
  pub fn files_by_path(&self) -> BTreeMap<&str, &ConfigFile> {
    self
      .files
      .iter()
      .map(|file| (file.path.as_str(), file))
      .collect()
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSummary {
  pub added: Vec<String>,
  pub modified: Vec<String>,
  pub deleted: Vec<String>,
  pub unstable: Vec<String>,
}

impl ChangeSummary {
  pub fn is_empty(&self) -> bool {
    self.added.is_empty()
      && self.modified.is_empty()
      && self.deleted.is_empty()
      && self.unstable.is_empty()
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessExit {
  pub success: bool,
  pub code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigCommit {
  pub id: String,
  pub parent_id: Option<String>,
  pub tree_id: String,
  pub created_at: String,
  pub game_root_key: String,
  pub instance_id: Option<String>,
  pub session_id: Option<String>,
  pub checkpoint: Checkpoint,
  pub semantic: Semantic,
  pub process_exit: Option<ProcessExit>,
  pub changed: ChangeSummary,
  pub incomplete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveSession {
  pub session_id: String,
  pub instance_id: Option<String>,
  pub started_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryState {
  pub schema: u32,
  pub head: Option<String>,
  pub next_sequence: u64,
  pub commit_count: u64,
  #[serde(default)]
  pub active_sessions: BTreeMap<String, ActiveSession>,
}

impl Default for HistoryState {
  fn default() -> Self {
    Self {
      schema: 1,
      head: None,
      next_sequence: 0,
      commit_count: 0,
      active_sessions: BTreeMap::new(),
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRequest {
  pub checkpoint: Checkpoint,
  pub instance_id: Option<String>,
  pub session_id: Option<String>,
  pub process_exit: Option<ProcessExit>,
}

impl CheckpointRequest {
  pub fn new(checkpoint: Checkpoint) -> Self {
    Self {
      checkpoint,
      instance_id: None,
      session_id: None,
      process_exit: None,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticContext {
  pub overlapping_sessions: bool,
  pub scan_incomplete: bool,
}

impl Default for SemanticContext {
  fn default() -> Self {
    Self {
      overlapping_sessions: false,
      scan_incomplete: false,
    }
  }
}
