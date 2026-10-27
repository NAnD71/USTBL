use crate::instance::modpack_update::text_diff::DiffHunk;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Checkpoint {
  BeforeLaunch,
  LauncherReady,
  AfterExit,
  ManualScan,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Semantic {
  InitialObservation,
  DuringLoading,
  DuringRuntime,
  BetweenSessions,
  InterruptedOrUnknown,
  Ambiguous,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadState {
  Stable,
  Unstable,
  Unreadable,
  TooLarge,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
  pub path: String,
  pub sha1: Option<String>,
  pub size: u64,
  pub state: ReadState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Tree {
  pub files: Vec<FileEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessExit {
  pub code: Option<i32>,
  pub success: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
  pub v: u32,
  pub at: String,
  pub checkpoint: Checkpoint,
  pub session_id: Option<String>,
  pub instance_id: Option<String>,
  pub exit: Option<ProcessExit>,
  pub config_present: bool,
  pub tree: String,
  pub errors: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointRequest {
  pub checkpoint: Checkpoint,
  pub session_id: Option<String>,
  pub instance_id: Option<String>,
  pub exit: Option<ProcessExit>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRead {
  pub entries: Vec<(u64, LogEntry)>,
  pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSet {
  pub added: Vec<String>,
  pub modified: Vec<String>,
  pub deleted: Vec<String>,
  pub uncertain: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileDiffBody {
  Text {
    hunks: Vec<DiffHunk>,
    #[serde(rename = "normalizedEqual")]
    normalized_equal: bool,
  },
  Binary,
  Unavailable {
    reason: String,
  },
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
  Added,
  Modified,
  Deleted,
  Uncertain,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
  pub path: String,
  pub kind: ChangeKind,
  pub old: Option<FileEntry>,
  pub new: Option<FileEntry>,
  pub body: FileDiffBody,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeCounts {
  pub added: usize,
  pub modified: usize,
  pub deleted: usize,
  pub uncertain: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEntry {
  pub id: u64,
  pub previous_id: Option<u64>,
  pub at: String,
  pub checkpoint: Checkpoint,
  pub semantic: Semantic,
  pub session_id: Option<String>,
  pub instance_id: Option<String>,
  pub exit: Option<ProcessExit>,
  pub config_present: bool,
  pub file_count: usize,
  pub changes: Option<ChangeCounts>,
  pub incomplete: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Timeline {
  pub schema: u32,
  pub entries: Vec<TimelineEntry>,
  pub diagnostics: Vec<String>,
  pub stats: HistoryStats,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryChanges {
  pub schema: u32,
  pub id: u64,
  pub previous_id: Option<u64>,
  pub semantic: Semantic,
  pub files: Vec<FileChange>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStats {
  pub entries: u64,
  pub log_bytes: u64,
  pub object_bytes: u64,
}
