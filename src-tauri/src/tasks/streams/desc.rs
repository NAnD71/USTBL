use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::storage::{write_atomic, Durability};
use crate::tasks::events::GEventStatus;

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
    let bytes = serde_json::to_vec(self)?;
    // Rewritten about once a second and regenerable; the rename already prevents truncation,
    // so skip the sync.
    write_atomic(path, &bytes, Durability::NoSync)
  }

  pub fn load(path: &PathBuf) -> std::io::Result<Self> {
    let file = std::fs::File::open(path)?;
    let desc: Self = serde_json::from_reader(file)?;
    Ok(desc)
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
  use super::{PDesc, PStatus};
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
