use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::storage::{write_atomic, Durability};
use crate::tasks::events::GEventStatus;

pub const JOURNAL_SCHEMA: u32 = 1;
static SESSION_ID: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().to_string());

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JournalRecord<'a, T>
where
  T: Clone + Serialize,
{
  schema_version: u32,
  session_id: &'static str,
  desc: &'a PDesc<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnedJournalRecord<T>
where
  T: Clone + Serialize,
{
  schema_version: u32,
  session_id: String,
  desc: PDesc<T>,
}

pub fn journal_dir(cache: &Path) -> PathBuf {
  cache.join("task-journal")
}

pub fn journal_path(cache: &Path, task_id: u32) -> PathBuf {
  journal_dir(cache).join(format!("task-{task_id}.json"))
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PDesc<T>
where
  T: Clone + Serialize,
{
  pub task_id: u32,
  pub task_group: Option<String>,
  pub total: i64,
  pub current: i64,
  pub payload: T,
  pub status: PStatus,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PStatus {
  Waiting,
  Stopped,
  Cancelled,
  Completed,
  InProgress,
  Failed,
}

impl PStatus {
  pub fn is_terminated(&self) -> bool {
    matches!(self, PStatus::Cancelled | PStatus::Completed)
  }

  pub fn is_stopped(&self) -> bool {
    *self == PStatus::Stopped
  }

  #[allow(dead_code)]
  pub fn is_completed(&self) -> bool {
    *self == PStatus::Completed
  }

  pub fn is_in_progress(&self) -> bool {
    *self == PStatus::InProgress
  }

  pub fn is_cancelled(&self) -> bool {
    *self == PStatus::Cancelled
  }

  pub fn is_waiting(&self) -> bool {
    *self == PStatus::Waiting
  }
}

impl<T> PDesc<T>
where
  T: Clone + Serialize + for<'de> Deserialize<'de>,
{
  pub fn new(
    task_id: u32,
    task_group: Option<String>,
    total: i64,
    payload: T,
    status: PStatus,
  ) -> Self {
    Self {
      task_id,
      task_group,
      total,
      current: 0,
      payload,
      status,
    }
  }
  pub fn save(&self, path: &PathBuf) -> std::io::Result<()> {
    let record = JournalRecord {
      schema_version: JOURNAL_SCHEMA,
      session_id: SESSION_ID.as_str(),
      desc: self,
    };
    let bytes = serde_json::to_vec(&record)?;
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    // Rewritten about once a second and regenerable; the rename already prevents truncation,
    // so skip the sync.
    write_atomic(path, &bytes, Durability::NoSync)
  }

  pub fn load(path: &PathBuf) -> std::io::Result<Self> {
    let bytes = std::fs::read(path)?;
    if let Ok(record) = serde_json::from_slice::<OwnedJournalRecord<T>>(&bytes) {
      if record.schema_version != JOURNAL_SCHEMA || record.session_id.is_empty() {
        return Err(std::io::Error::new(
          std::io::ErrorKind::InvalidData,
          "unsupported task journal record",
        ));
      }
      return Ok(record.desc);
    }
    // Legacy descriptors were bare PDesc values. They are never scanned from
    // the new journal directory, but remain readable for compatibility tests.
    Ok(serde_json::from_slice(&bytes)?)
  }

  pub fn increment_progress(&mut self, size: i64) {
    if self.status != PStatus::InProgress {
      return;
    }
    self.current += size;
  }

  pub fn start(&mut self) {
    self.status = PStatus::InProgress
  }

  pub fn stop(&mut self) {
    if self.status == PStatus::InProgress || self.status == PStatus::Waiting {
      self.status = PStatus::Stopped
    }
  }

  pub fn cancel(&mut self) {
    self.status = PStatus::Cancelled
  }

  pub fn resume(&mut self) {
    if self.status == PStatus::Stopped {
      self.status = PStatus::InProgress
    }
  }

  pub fn complete(&mut self) {
    self.status = PStatus::Completed;
  }

  pub fn fail(&mut self) {
    self.status = PStatus::Failed;
  }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GDesc<T>
where
  T: Clone + Serialize,
{
  pub task_group: String,
  pub task_descs: Vec<PDesc<T>>,
  pub status: GEventStatus,
}

#[cfg(test)]
mod tests {
  use super::{journal_path, PDesc, PStatus, JOURNAL_SCHEMA};
  use crate::storage::test_support::TempDir;
  use serde::ser::SerializeStruct;
  use serde::{Deserialize, Serialize, Serializer};
  use std::fs;
  use std::io::ErrorKind;

  /// Writes `first` and then fails, like a value that breaks off partway through.
  #[derive(Clone, Deserialize)]
  struct BreaksMidSerialize;

  impl Serialize for BreaksMidSerialize {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
      let mut state = serializer.serialize_struct("BreaksMidSerialize", 2)?;
      state.serialize_field("first", &1)?;
      Err(<S::Error as serde::ser::Error>::custom("injected failure"))
    }
  }

  #[test]
  fn journal_records_round_trip_and_legacy_bare_descriptors_remain_readable() {
    let dir = TempDir::new("desc-journal");
    let path = journal_path(&dir.0, 7);
    let desc = PDesc::new(
      7,
      Some("mods".into()),
      12,
      "payload".to_string(),
      PStatus::Stopped,
    );
    desc.save(&path).unwrap();

    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["schemaVersion"], JOURNAL_SCHEMA);
    assert!(value["sessionId"].as_str().is_some_and(|id| !id.is_empty()));
    let loaded = PDesc::<String>::load(&path).unwrap();
    assert_eq!((loaded.task_id, loaded.current), (7, 0));
    assert_eq!(loaded.task_group.as_deref(), Some("mods"));
    assert_eq!(loaded.payload, "payload");
    assert!(loaded.status == PStatus::Stopped);

    let legacy = dir.0.join("legacy.json");
    fs::write(&legacy, serde_json::to_vec(&desc).unwrap()).unwrap();
    assert_eq!(PDesc::<String>::load(&legacy).unwrap().payload, "payload");
  }

  #[test]
  fn missing_descriptor_is_not_found() {
    let dir = TempDir::new("desc-missing");
    let Err(error) = PDesc::<String>::load(&dir.0.join("task-1.json")) else {
      panic!("a missing descriptor must not load");
    };
    assert_eq!(error.kind(), ErrorKind::NotFound);
  }

  #[test]
  fn corrupt_descriptor_is_invalid_data() {
    let dir = TempDir::new("desc-corrupt");
    let path = dir.0.join("task-1.json");
    fs::write(&path, b"desc not json").unwrap();
    let Err(error) = PDesc::<String>::load(&path) else {
      panic!("corrupt descriptor must not load");
    };
    assert_eq!(error.kind(), ErrorKind::InvalidData);
  }

  #[cfg(windows)]
  #[test]
  fn failed_open_keeps_previous_descriptor() {
    let dir = TempDir::new("desc-open");
    let path = dir.0.join("task-1.json");
    PDesc::new(1, None, 10, "old".to_string(), PStatus::InProgress)
      .save(&path)
      .unwrap();
    let held = crate::storage::test_support::lock_exclusive(&path);
    let saved = PDesc::new(1, None, 10, "new".to_string(), PStatus::Completed).save(&path);
    drop(held);
    assert!(saved.is_err());
    assert_eq!(PDesc::<String>::load(&path).unwrap().payload, "old");
  }

  // Expected safe: a save that fails while serializing must leave the previous descriptor
  // loadable. The current save truncates the file first and streams JSON into it.
  #[test]
  fn failed_serialization_keeps_previous_descriptor() {
    let dir = TempDir::new("desc-serialize");
    let path = dir.0.join("task-1.json");
    PDesc::new(1, None, 10, "old".to_string(), PStatus::InProgress)
      .save(&path)
      .unwrap();
    let saved = PDesc::new(2, None, 10, BreaksMidSerialize, PStatus::InProgress).save(&path);
    assert!(saved.is_err());
    let loaded = PDesc::<String>::load(&path);
    assert!(
      loaded.is_ok(),
      "previous descriptor was destroyed: {:?}",
      loaded.as_ref().err()
    );
    assert_eq!(loaded.unwrap().task_id, 1);
  }
}
