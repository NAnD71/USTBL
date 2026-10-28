use crate::instance::config_tracking::{Checkpoint, CheckpointRequest, ConfigHistory, ProcessExit};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

pub(crate) struct TrackingSession {
  pub(crate) history: ConfigHistory,
  pub(crate) session_id: String,
  pub(crate) instance_id: String,
  pub(crate) ready_started: AtomicBool,
  pub(crate) ready_handle: Mutex<Option<JoinHandle<()>>>,
}

impl TrackingSession {
  pub(crate) fn new(game_root: PathBuf, session_id: String, instance_id: String) -> Arc<Self> {
    Arc::new(Self {
      history: ConfigHistory::new(game_root),
      session_id,
      instance_id,
      ready_started: AtomicBool::new(false),
      ready_handle: Mutex::new(None),
    })
  }

  pub(crate) async fn before_launch(self: &Arc<Self>) {
    let started = Instant::now();
    let session = Arc::clone(self);
    let result =
      tokio::task::spawn_blocking(move || session.append(Checkpoint::BeforeLaunch, None)).await;
    log::info!(
      "Config tracking before_launch finished in {:?}",
      started.elapsed()
    );
    match result {
      Ok(Ok(())) => {}
      Ok(Err(error)) => log::warn!("Config tracking before_launch failed: {error}"),
      Err(error) => log::warn!("Config tracking before_launch task failed: {error}"),
    }
  }

  pub(crate) fn on_ready(self: &Arc<Self>) {
    if self.ready_started.swap(true, Ordering::SeqCst) {
      return;
    }
    let session = Arc::clone(self);
    let handle = std::thread::spawn(move || {
      if let Err(error) = session.append(Checkpoint::LauncherReady, None) {
        log::warn!("Config tracking launcher_ready failed: {error}");
      }
    });
    match self.ready_handle.lock() {
      Ok(mut ready_handle) => *ready_handle = Some(handle),
      Err(_) => log::warn!("Config tracking ready handle lock was poisoned"),
    }
  }

  pub(crate) async fn on_exit(self: &Arc<Self>, exit: ProcessExit) {
    let ready_handle = self
      .ready_handle
      .lock()
      .ok()
      .and_then(|mut handle| handle.take());
    if let Some(handle) = ready_handle {
      let _ = tokio::task::spawn_blocking(move || handle.join()).await;
    }

    let session = Arc::clone(self);
    let result =
      tokio::task::spawn_blocking(move || session.append(Checkpoint::AfterExit, Some(exit))).await;
    match result {
      Ok(Ok(())) => {}
      Ok(Err(error)) => log::warn!("Config tracking after_exit failed: {error}"),
      Err(error) => log::warn!("Config tracking after_exit task failed: {error}"),
    }
  }

  fn append(&self, checkpoint: Checkpoint, exit: Option<ProcessExit>) -> std::io::Result<()> {
    self.history.append(CheckpointRequest {
      checkpoint,
      session_id: Some(self.session_id.clone()),
      instance_id: Some(self.instance_id.clone()),
      exit,
    })?;
    Ok(())
  }
}

pub(crate) fn process_exit_from_wait(result: Result<ExitStatus, std::io::Error>) -> ProcessExit {
  match result {
    Ok(status) => process_exit_from_status(status.code(), status.success()),
    Err(_) => process_exit_from_status(None, false),
  }
}

pub(crate) fn process_exit_from_status(code: Option<i32>, success: bool) -> ProcessExit {
  ProcessExit { code, success }
}

#[cfg(test)]
mod tests {
  use super::{process_exit_from_status, TrackingSession};
  use crate::instance::config_tracking::{Checkpoint, ConfigHistory};
  use std::fs;
  use std::path::PathBuf;
  use uuid::Uuid;

  struct TempDir(PathBuf);

  impl TempDir {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("ustbl-tracking-lifecycle-{}", Uuid::new_v4()));
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }
  }

  impl Drop for TempDir {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
      .enable_all()
      .build()
      .unwrap()
  }

  #[test]
  fn ready_is_appended_only_once() {
    let root = TempDir::new();
    let session = TrackingSession::new(root.0.clone(), "session".into(), "instance".into());
    session.on_ready();
    session.on_ready();
    runtime().block_on(session.on_exit(process_exit_from_status(Some(0), true)));
    let entries = ConfigHistory::new(root.0.clone())
      .read_log()
      .unwrap()
      .entries;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].1.checkpoint, Checkpoint::LauncherReady);
    assert_eq!(entries[1].1.checkpoint, Checkpoint::AfterExit);
  }

  #[test]
  fn exit_waits_for_ready_before_appending_exit() {
    let root = TempDir::new();
    let session = TrackingSession::new(root.0.clone(), "session".into(), "instance".into());
    session
      .history
      .append(crate::instance::config_tracking::CheckpointRequest {
        checkpoint: Checkpoint::BeforeLaunch,
        session_id: Some("session".into()),
        instance_id: Some("instance".into()),
        exit: None,
      })
      .unwrap();
    session.on_ready();
    runtime().block_on(session.on_exit(process_exit_from_status(Some(0), true)));
    let entries = ConfigHistory::new(root.0.clone())
      .read_log()
      .unwrap()
      .entries;
    assert_eq!(
      entries
        .iter()
        .map(|(_, entry)| entry.checkpoint.clone())
        .collect::<Vec<_>>(),
      [
        Checkpoint::BeforeLaunch,
        Checkpoint::LauncherReady,
        Checkpoint::AfterExit
      ]
    );
  }

  #[test]
  fn append_failure_does_not_panic_or_disable_later_calls() {
    let root = TempDir::new();
    fs::write(root.0.join(".ustbl-update"), b"not a directory").unwrap();
    let session = TrackingSession::new(root.0.clone(), "session".into(), "instance".into());
    runtime().block_on(async {
      session.before_launch().await;
      session.on_ready();
      session.on_exit(process_exit_from_status(None, false)).await;
    });
  }

  #[test]
  fn exit_mapping_preserves_success_code_and_wait_errors_are_failure() {
    let successful = process_exit_from_status(Some(0), true);
    assert_eq!(successful.code, Some(0));
    assert!(successful.success);
    let failed = process_exit_from_status(Some(3), false);
    assert_eq!(failed.code, Some(3));
    assert!(!failed.success);
    let wait_error = process_exit_from_status(None, false);
    assert_eq!(wait_error.code, None);
    assert!(!wait_error.success);
  }
}
