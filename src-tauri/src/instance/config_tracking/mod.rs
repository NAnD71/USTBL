mod diff;
mod model;
mod scan;
mod store;

#[allow(unused_imports)]
pub use diff::{derive_semantic, diff_trees, normalized_text_diff, normalized_text_equal};
#[allow(unused_imports)]
pub use model::{
  ActiveSession, ChangeSummary, Checkpoint, CheckpointRequest, ConfigCommit, ConfigFile,
  ConfigFileType, ConfigTree, HistoryState, ProcessExit, ReadState, Semantic, SemanticContext,
};
pub use scan::scan_config;
#[allow(unused_imports)]
pub use store::{HistoryStats, HistoryStore, HISTORY_DIR_NAME};

#[cfg(test)]
mod tests {
  use super::*;
  use sha1::Digest;
  use std::fs;
  use std::sync::Arc;
  use std::thread;

  fn fixture() -> (tempfile_like::TempDir, HistoryStore) {
    let temp = tempfile_like::TempDir::new();
    let root = temp.path().join("game");
    let config = root.join("config");
    let history = root.join(".ustbl-update/config-history");
    fs::create_dir_all(&config).unwrap();
    (temp, HistoryStore::new(root, config, history))
  }

  fn checkpoint(store: &HistoryStore, checkpoint: Checkpoint) -> ConfigCommit {
    store
      .checkpoint(CheckpointRequest::new(checkpoint))
      .unwrap()
  }

  #[test]
  fn first_observation_and_empty_checkpoints_are_preserved() {
    let (_temp, store) = fixture();
    let first = checkpoint(&store, Checkpoint::FirstObserved);
    let second = checkpoint(&store, Checkpoint::ManualScan);
    assert_eq!(first.semantic, Semantic::InitialObservation);
    assert!(second.changed.is_empty());
    assert_eq!(second.parent_id, Some(first.id));
    assert_eq!(store.stats().unwrap().commit_count, 2);
  }

  #[test]
  fn added_modified_deleted_and_mtime_only_changes_are_classified() {
    let (_temp, store) = fixture();
    let first = checkpoint(&store, Checkpoint::FirstObserved);
    fs::write(store.config_dir.join("a.txt"), b"one\n").unwrap();
    fs::write(store.config_dir.join("same.txt"), b"same\n").unwrap();
    let added = checkpoint(&store, Checkpoint::ManualScan);
    assert_eq!(added.changed.added, vec!["config/a.txt", "config/same.txt"]);
    fs::write(store.config_dir.join("a.txt"), b"two\n").unwrap();
    let same_mtime = fs::metadata(store.config_dir.join("same.txt"))
      .unwrap()
      .modified()
      .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    fs::write(store.config_dir.join("same.txt"), b"same\n").unwrap();
    let modified = checkpoint(&store, Checkpoint::ManualScan);
    assert_eq!(modified.changed.modified, vec!["config/a.txt"]);
    assert!(
      fs::metadata(store.config_dir.join("same.txt"))
        .unwrap()
        .modified()
        .unwrap()
        >= same_mtime
    );
    fs::remove_file(store.config_dir.join("a.txt")).unwrap();
    let deleted = checkpoint(&store, Checkpoint::ManualScan);
    assert_eq!(deleted.changed.deleted, vec!["config/a.txt"]);
    assert_eq!(deleted.parent_id, Some(modified.id));
    assert_eq!(first.changed.added.len(), 0);
  }

  #[test]
  fn duplicate_contents_share_one_object() {
    let (_temp, store) = fixture();
    fs::write(store.config_dir.join("a.txt"), b"same").unwrap();
    fs::write(store.config_dir.join("b.txt"), b"same").unwrap();
    checkpoint(&store, Checkpoint::FirstObserved);
    let sha1 = hex::encode(sha1::Sha1::digest(b"same"));
    assert_eq!(store.load_object(&sha1).unwrap(), Some(b"same".to_vec()));
    assert_eq!(
      fs::read_dir(store.history_dir.join("objects"))
        .unwrap()
        .count(),
      1
    );
  }

  #[test]
  fn normalized_text_difference_is_separate_from_raw_change() {
    let (_temp, store) = fixture();
    fs::write(
      store.config_dir.join("a.properties"),
      b"#Wed Oct 14 20:14:57 CST 2026\r\na=b",
    )
    .unwrap();
    checkpoint(&store, Checkpoint::FirstObserved);
    fs::write(store.config_dir.join("a.properties"), b"a=b\n").unwrap();
    let commit = checkpoint(&store, Checkpoint::ManualScan);
    assert_eq!(commit.changed.modified, vec!["config/a.properties"]);
    assert_eq!(
      normalized_text_equal("config/a.properties", b"a=b", b"a=b\r\n"),
      Some(true)
    );
    assert!(
      normalized_text_diff("config/a.properties", b"a=1", b"a=2")
        .unwrap()
        .len()
        > 0
    );
  }

  #[test]
  fn semantic_state_machine_is_table_driven() {
    let make = |_current, previous| ConfigCommit {
      id: "id".into(),
      parent_id: None,
      tree_id: "tree".into(),
      created_at: String::new(),
      game_root_key: String::new(),
      instance_id: None,
      session_id: None,
      checkpoint: previous,
      semantic: Semantic::InitialObservation,
      process_exit: None,
      changed: ChangeSummary::default(),
      incomplete: false,
    };
    let cases = [
      (
        None,
        Checkpoint::FirstObserved,
        Semantic::InitialObservation,
      ),
      (
        Some(Checkpoint::BeforeLaunch),
        Checkpoint::LauncherReady,
        Semantic::DuringLoading,
      ),
      (
        Some(Checkpoint::LauncherReady),
        Checkpoint::AfterExit,
        Semantic::DuringRuntime,
      ),
      (
        Some(Checkpoint::AfterExit),
        Checkpoint::BeforeLaunch,
        Semantic::BetweenSessions,
      ),
      (
        Some(Checkpoint::BeforeLaunch),
        Checkpoint::AfterExit,
        Semantic::InterruptedOrUnknown,
      ),
      (
        Some(Checkpoint::BeforeLaunch),
        Checkpoint::LauncherReady,
        Semantic::Ambiguous,
      ),
    ];
    for (previous, current, expected) in cases {
      let previous = previous.map(|value| make(current, value));
      let context = if expected == Semantic::Ambiguous {
        SemanticContext {
          overlapping_sessions: true,
          scan_incomplete: false,
        }
      } else {
        SemanticContext::default()
      };
      let actual = derive_semantic(previous.as_ref(), current, context);
      assert_eq!(actual, expected);
    }
    assert_eq!(
      derive_semantic(
        None,
        Checkpoint::ManualScan,
        SemanticContext {
          overlapping_sessions: false,
          scan_incomplete: true
        }
      ),
      Semantic::Ambiguous
    );
  }

  #[test]
  fn ready_exit_and_recovery_semantics_and_session_state() {
    let (_temp, store) = fixture();
    let mut request = CheckpointRequest::new(Checkpoint::BeforeLaunch);
    request.session_id = Some("s1".into());
    let before = store.checkpoint(request).unwrap();
    assert_eq!(before.semantic, Semantic::InitialObservation);
    let mut request = CheckpointRequest::new(Checkpoint::LauncherReady);
    request.session_id = Some("s1".into());
    assert_eq!(
      store.checkpoint(request).unwrap().semantic,
      Semantic::DuringLoading
    );
    let mut request = CheckpointRequest::new(Checkpoint::AfterExit);
    request.session_id = Some("s1".into());
    request.process_exit = Some(ProcessExit {
      success: false,
      code: Some(1),
    });
    assert_eq!(
      store.checkpoint(request).unwrap().semantic,
      Semantic::DuringRuntime
    );
    assert!(store.load_state().unwrap().active_sessions.is_empty());
    let mut request = CheckpointRequest::new(Checkpoint::BeforeLaunch);
    request.session_id = Some("old".into());
    store.checkpoint(request).unwrap();
    let recovered = checkpoint(&store, Checkpoint::Recovered);
    assert_eq!(recovered.semantic, Semantic::InterruptedOrUnknown);
    assert!(store.load_state().unwrap().active_sessions.is_empty());
  }

  #[test]
  fn overlapping_sessions_are_ambiguous() {
    let (_temp, store) = fixture();
    store.start_session("one", None).unwrap();
    store.start_session("two", None).unwrap();
    let mut request = CheckpointRequest::new(Checkpoint::ManualScan);
    request.session_id = Some("one".into());
    let commit = store.checkpoint(request).unwrap();
    assert_eq!(commit.checkpoint, Checkpoint::Ambiguous);
    assert_eq!(commit.semantic, Semantic::Ambiguous);
  }

  #[test]
  fn symlinks_are_not_followed_and_paths_are_slash_separated() {
    let (_temp, store) = fixture();
    fs::create_dir_all(store.config_dir.join("nested")).unwrap();
    fs::write(store.config_dir.join("nested/a.txt"), b"a").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
      store.config_dir.join("nested/a.txt"),
      store.config_dir.join("link.txt"),
    )
    .unwrap();
    #[cfg(windows)]
    let _ = std::os::windows::fs::symlink_file(
      store.config_dir.join("nested/a.txt"),
      store.config_dir.join("link.txt"),
    );
    let tree = scan_config(&store.game_root, &store.config_dir).unwrap();
    assert_eq!(
      tree
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>(),
      vec!["config/nested/a.txt"]
    );
    assert!(tree.files.iter().all(|file| !file.path.contains('\\')));
  }

  #[test]
  fn concurrent_same_root_checkpoints_keep_both_nodes() {
    let (_temp, store) = fixture();
    let store = Arc::new(store);
    let handles: Vec<_> = (0..2)
      .map(|_| {
        let store = Arc::clone(&store);
        thread::spawn(move || checkpoint(&store, Checkpoint::ManualScan).id)
      })
      .collect();
    let ids: Vec<_> = handles
      .into_iter()
      .map(|handle| handle.join().unwrap())
      .collect();
    let state = store.load_state().unwrap();
    assert_eq!(state.commit_count, 2);
    assert!(ids.iter().all(|id| store.load_commit(id).is_ok()));
  }

  #[test]
  fn atomic_storage_can_be_read_back() {
    let (_temp, store) = fixture();
    let commit = checkpoint(&store, Checkpoint::FirstObserved);
    assert!(store.history_dir.join("state.json").is_file());
    assert_eq!(store.load_commit(&commit.id).unwrap().id, commit.id);
    assert_eq!(store.load_tree(&commit.tree_id).unwrap().id, commit.tree_id);
    let temporary_files = fs::read_dir(&store.history_dir)
      .unwrap()
      .flat_map(|entry| entry.ok())
      .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
      .count();
    assert_eq!(temporary_files, 0);
  }

  #[test]
  fn missing_or_corrupt_state_recovers_valid_commit_history() {
    let (_temp, store) = fixture();
    let commit = checkpoint(&store, Checkpoint::FirstObserved);
    fs::remove_file(store.history_dir.join("state.json")).unwrap();
    let recovered = store.load_state().unwrap();
    assert_eq!(recovered.head, Some(commit.id.clone()));
    assert_eq!(recovered.commit_count, 1);
    fs::write(store.history_dir.join("state.json"), b"not json").unwrap();
    let (recovered, warning) = store.load_state_recovering().unwrap();
    assert_eq!(recovered.head, Some(commit.id));
    assert!(warning.is_some());
  }

  #[test]
  fn an_outside_config_path_is_rejected() {
    let (_temp, store) = fixture();
    let outside = store.game_root.parent().unwrap().join("outside-config");
    fs::create_dir_all(&outside).unwrap();
    assert!(scan_config(&store.game_root, &outside).is_err());
  }

  #[test]
  fn history_cannot_be_written_inside_config() {
    let (_temp, mut store) = fixture();
    store.history_dir = store.config_dir.join("history");
    assert!(store
      .checkpoint(CheckpointRequest::new(Checkpoint::FirstObserved))
      .is_err());
  }

  #[test]
  fn a_file_that_changes_during_every_read_is_unstable() {
    let (_temp, store) = fixture();
    let path = store.config_dir.join("changing.txt");
    fs::write(&path, b"initial").unwrap();
    let mut value = 0u8;
    let (file, object) =
      super::scan::read_file_stably_for_test(&path, "config/changing.txt".into(), &mut || {
        value = value.saturating_add(1);
        fs::write(&path, vec![value; value as usize + 1]).unwrap();
      });
    assert_eq!(object, None);
    assert_eq!(file.unwrap().read_state, ReadState::Unstable);
  }

  mod tempfile_like {
    use std::path::{Path, PathBuf};
    use uuid::Uuid;
    pub struct TempDir(PathBuf);
    impl TempDir {
      pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ustbl-config-tracking-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
      }
      pub fn path(&self) -> &Path {
        &self.0
      }
    }
    impl Drop for TempDir {
      fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
      }
    }
  }
}
