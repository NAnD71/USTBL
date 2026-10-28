use crate::error::USTBLResult;
use crate::instance::config_tracking::{
  Checkpoint, CheckpointRequest, ConfigHistory, EntryChanges, Timeline, TimelineEntry,
};
use crate::instance::helpers::misc::get_instance_subdir_paths;
use crate::instance::models::misc::{Instance, InstanceError, InstanceSubdirType};
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigTrackingTimeline {
  pub game_root: String,
  pub shared_root: bool,
  pub timeline: Timeline,
}

fn instance_root(app: &AppHandle, instance_id: &str) -> USTBLResult<(Instance, PathBuf)> {
  let instances = app.state::<Mutex<HashMap<String, Instance>>>();
  let instance = instances
    .lock()?
    .get(instance_id)
    .cloned()
    .ok_or(InstanceError::InstanceNotFoundByID)?;
  let root = get_instance_subdir_paths(app, &instance, &[&InstanceSubdirType::Root])
    .ok_or(InstanceError::InstanceNotFoundByID)?
    .into_iter()
    .next()
    .ok_or(InstanceError::InstanceNotFoundByID)?;
  Ok((instance, root))
}

fn slash_path(path: &std::path::Path) -> String {
  path.to_string_lossy().replace('\\', "/")
}

fn missing_entry(id: u64) -> io::Error {
  io::Error::new(
    io::ErrorKind::NotFound,
    format!("history entry {id} not found"),
  )
}

#[tauri::command]
pub async fn get_config_tracking_timeline(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<ConfigTrackingTimeline> {
  tokio::task::spawn_blocking(move || {
    let (instance, root) = instance_root(&app, &instance_id)?;
    let timeline = ConfigHistory::new(&root).timeline()?;
    Ok(ConfigTrackingTimeline {
      game_root: slash_path(&root),
      shared_root: root != instance.version_path,
      timeline,
    })
  })
  .await?
}

#[tauri::command]
pub async fn get_config_tracking_entry_changes(
  app: AppHandle,
  instance_id: String,
  entry_id: u64,
) -> USTBLResult<EntryChanges> {
  tokio::task::spawn_blocking(move || {
    let (_, root) = instance_root(&app, &instance_id)?;
    Ok(ConfigHistory::new(root).entry_changes(entry_id)?)
  })
  .await?
}

#[tauri::command]
pub async fn scan_config_tracking(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<TimelineEntry> {
  tokio::task::spawn_blocking(move || {
    let (_, root) = instance_root(&app, &instance_id)?;
    let history = ConfigHistory::new(root);
    let (id, _) = history.append(CheckpointRequest {
      checkpoint: Checkpoint::ManualScan,
      session_id: None,
      instance_id: Some(instance_id),
      exit: None,
    })?;
    history
      .timeline()?
      .entries
      .into_iter()
      .find(|entry| entry.id == id)
      .ok_or_else(|| missing_entry(id).into())
  })
  .await?
}
