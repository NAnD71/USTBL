use crate::error::USTBLResult;
use crate::instance::models::misc::InstanceError;
use crate::tasks::download::{DownloadParam, DownloadTask};
use crate::tasks::events::GEventStatus;
use crate::tasks::monitor::TaskMonitor;
use crate::tasks::streams::desc::PStatus;
use crate::tasks::{PTaskDesc, PTaskGroupDesc, PTaskParam, THandle, USTBLFutureDesc};
use crate::utils::fs::extract_filename;
use std::pin::Pin;
use std::time::Duration;
use tauri::{AppHandle, Manager};

fn same_download(left: &DownloadParam, right: &DownloadParam) -> bool {
  left.src == right.src
    && left.dest == right.dest
    && left.sha1 == right.sha1
    && left.custom_headers == right.custom_headers
    && left.transfer_options == right.transfer_options
}

fn valid_new_resource_download(param: &DownloadParam) -> bool {
  let extension = param
    .dest
    .extension()
    .and_then(|extension| extension.to_str())
    .map(|extension| extension.to_ascii_lowercase());
  param.src.scheme() == "https"
    && param.dest.is_absolute()
    && param.dest.file_name().is_some()
    && matches!(
      extension.as_deref(),
      Some("jar" | "zip" | "disabled" | "schematic" | "schem" | "litematic")
    )
    && !param.dest.try_exists().unwrap_or(true)
    && param
      .custom_headers
      .as_ref()
      .is_none_or(|headers| headers.is_empty())
    && param
      .transfer_options
      .fallback_sources
      .iter()
      .all(|source| source.scheme() == "https")
}

fn valid_ipc_download(param: &DownloadParam, known: &[PTaskDesc]) -> bool {
  let retry = known.iter().any(|desc| {
    matches!(
      desc.status,
      PStatus::Failed | PStatus::Cancelled | PStatus::Stopped
    ) && matches!(&desc.payload, PTaskParam::Download(original) if same_download(param, original))
  });
  retry || valid_new_resource_download(param)
}

pub async fn schedule_progressive_task_group(
  app: AppHandle,
  task_group: String,
  params: Vec<PTaskParam>,
  with_timestamp: bool,
) -> USTBLResult<PTaskGroupDesc> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  let mut task_descs = Vec::new();
  let mut future_descs = Vec::new();
  let task_group = if with_timestamp {
    // If with_timestamp is true, append a timestamp to the task group name
    // to ensure uniqueness and avoid conflicts.
    let timestamp = chrono::Utc::now().timestamp_millis();
    format!("{task_group}@{timestamp}")
  } else {
    task_group.clone()
  };

  for param in params {
    let task_id = monitor.get_new_id();
    match param {
      // Uploads are created by the code that runs them, never by the frontend.
      PTaskParam::Upload(_) => {
        return Err(crate::error::USTBLError(
          "Upload tasks cannot be scheduled".into(),
        ))
      }
      PTaskParam::Download(mut param) => {
        if param.filename.is_none() {
          param.filename = Some(extract_filename(
            param.dest.to_str().unwrap_or_default(),
            true,
          ));
        }
        let task = DownloadTask::new(
          app.clone(),
          task_id,
          Some(task_group.clone()),
          param,
          Duration::from_secs(1),
        );
        let (f, h) = task
          .future(app.clone(), monitor.download_rate_limiter.clone())
          .await?;
        let task_desc = h.read().unwrap().desc.clone();
        let future_desc = USTBLFutureDesc {
          task_id,
          f: Box::pin(f),
          h: h.clone(),
        };
        task_descs.push(task_desc);
        future_descs.push(future_desc);
      }
    }
  }
  monitor
    .enqueue_task_group(task_group.clone(), future_descs)
    .await;
  Ok(PTaskGroupDesc {
    task_group,
    task_descs,
    status: GEventStatus::Started,
  })
}

#[tauri::command]
pub async fn schedule_resource_download(
  app: AppHandle,
  task_group: String,
  params: Vec<PTaskParam>,
  with_timestamp: bool,
) -> USTBLResult<PTaskGroupDesc> {
  let known = app.state::<Pin<Box<TaskMonitor>>>().task_descriptors();
  for param in &params {
    let PTaskParam::Download(download) = param else {
      return Err(InstanceError::InvalidSourcePath.into());
    };
    if !valid_ipc_download(download, &known) {
      return Err(InstanceError::InvalidSourcePath.into());
    }
  }
  schedule_progressive_task_group(app, task_group, params, with_timestamp).await
}

#[tauri::command]
pub fn create_transient_task(app: AppHandle, desc: THandle) -> USTBLResult<()> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.create_transient_task(app.clone(), desc);
  Ok(())
}

#[tauri::command]
pub fn set_transient_task_state(app: AppHandle, task_id: u32, state: String) -> USTBLResult<()> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.set_transient_task(app.clone(), task_id, state);
  Ok(())
}

#[tauri::command]
pub fn cancel_transient_task(app: AppHandle, task_id: u32) -> USTBLResult<()> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.cancel_transient_task(task_id);
  Ok(())
}

#[tauri::command]
pub fn get_transient_task(app: AppHandle, task_id: u32) -> USTBLResult<Option<THandle>> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  Ok(monitor.get_transient_task(task_id))
}

#[tauri::command]
pub fn cancel_progressive_task(app: AppHandle, task_id: u32) -> USTBLResult<()> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.cancel_progress(task_id);
  Ok(())
}

#[tauri::command]
pub fn resume_progressive_task(app: AppHandle, task_id: u32) -> USTBLResult<()> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.resume_progress(task_id);
  Ok(())
}

#[tauri::command]
#[allow(dead_code)]
pub async fn restart_progressive_task(app: AppHandle, task_id: u32) -> USTBLResult<()> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.restart_progress(task_id).await;
  Ok(())
}

#[tauri::command]
pub fn stop_progressive_task(app: AppHandle, task_id: u32) -> USTBLResult<()> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.stop_progress(task_id);
  Ok(())
}

#[tauri::command]
pub fn cancel_progressive_task_group(app: AppHandle, task_group: String) -> USTBLResult<()> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.cancel_progressive_task_group(task_group);
  Ok(())
}

#[tauri::command]
pub fn stop_progressive_task_group(app: AppHandle, task_group: String) -> USTBLResult<()> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.stop_progressive_task_group(task_group);
  Ok(())
}

#[tauri::command]
pub async fn resume_progressive_task_group(app: AppHandle, task_group: String) -> USTBLResult<()> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.resume_progressive_task_group(task_group).await;
  Ok(())
}

#[tauri::command]
pub fn delete_progressive_task_group(app: AppHandle, task_group: String) -> USTBLResult<()> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.delete_progressive_task_group(task_group);
  Ok(())
}

#[tauri::command]
pub fn retrieve_progressive_task_list(app: AppHandle) -> Vec<PTaskGroupDesc> {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.state_list()
}

#[cfg(test)]
mod ipc_download_tests {
  use super::*;
  use crate::storage::test_support::TempDir;

  fn param(path: &std::path::Path) -> DownloadParam {
    DownloadParam {
      src: url::Url::parse("https://example.com/resource.jar").unwrap(),
      dest: path.to_path_buf(),
      filename: None,
      sha1: Some("0123456789abcdef".into()),
      custom_headers: None,
      transfer_options: Default::default(),
    }
  }

  #[test]
  fn a_new_ipc_download_requires_a_new_resource_file_on_an_absolute_path() {
    let dir = TempDir::new("ipc-download-new");
    assert!(valid_new_resource_download(&param(&dir.0.join("mod.jar"))));
    assert!(valid_new_resource_download(&param(&dir.0.join("pack.zip"))));
    assert!(!valid_new_resource_download(&param(
      &dir.0.join("program.exe")
    )));
    assert!(!valid_new_resource_download(&param(std::path::Path::new(
      "mod.jar"
    ))));

    let existing = dir.0.join("existing.jar");
    std::fs::write(&existing, b"existing").unwrap();
    assert!(!valid_new_resource_download(&param(&existing)));
    let mut insecure = param(&dir.0.join("other.jar"));
    insecure.src = url::Url::parse("http://example.com/other.jar").unwrap();
    assert!(!valid_new_resource_download(&insecure));
  }

  #[test]
  fn an_ipc_retry_must_match_an_inactive_known_download_exactly() {
    let dir = TempDir::new("ipc-download-retry");
    let path = dir.0.join("mod.jar");
    std::fs::write(&path, b"partial").unwrap();
    let original = param(&path);
    let known = vec![PTaskDesc::new(
      1,
      Some("mod".into()),
      0,
      PTaskParam::Download(original.clone()),
      PStatus::Failed,
    )];
    assert!(valid_ipc_download(&original, &known));

    let mut changed = original.clone();
    changed.src = url::Url::parse("https://evil.example/mod.jar").unwrap();
    assert!(!valid_ipc_download(&changed, &known));
    let mut running = known.clone();
    running[0].status = PStatus::InProgress;
    assert!(!valid_ipc_download(&original, &running));
  }
}
