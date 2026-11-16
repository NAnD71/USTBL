use crate::error::USTBLError;
use crate::error::USTBLResult;
use crate::instance::helpers::misc::refresh_instances;
use crate::launcher_config::helpers::java::{
  build_mojang_java_download_params, get_java_info_from_command, get_java_info_from_release_file,
  refresh_and_update_javas,
};
use crate::launcher_config::helpers::updater::{
  self, download_target_version, fetch_latest_version,
};
use crate::launcher_config::models::{
  GameDirectory, JavaInfo, LauncherConfig, LauncherConfigError, VersionMetaInfo,
};
use crate::storage::Storage;
use crate::tasks::{
  commands::schedule_progressive_task_group, monitor::TaskMonitor, streams::desc::PDesc, PTaskParam,
};
use crate::utils::fs::{generate_unique_filename, get_subdirectories, is_plain_file_name};
use crate::utils::string::{camel_to_snake_case, snake_to_camel_case};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;
use tauri_plugin_opener::reveal_item_in_dir;

const SYNC_LOCAL_FIELDS: &[&str] = &[
  "run_count",
  "local_game_directories",
  "extra_java_paths",
  "global_game_config.game_java.exec_path",
  "global_game_config.advanced.custom_commands.precall_command",
  "global_game_config.advanced.custom_commands.post_exit_command",
  "global_game_config.advanced.custom_commands.wrapper_launcher",
  "global_game_config.advanced.jvm.args",
  "download.proxy",
  "download.cache.directory",
];

const BACKGROUND_EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "png", "gif", "webp"];

fn remove_sync_field(value: &mut Value, path: &str) {
  let mut components = path.split('.').peekable();
  let mut current = value;
  while let Some(component) = components.next() {
    let key = snake_to_camel_case(component);
    if components.peek().is_none() {
      if let Some(object) = current.as_object_mut() {
        object.remove(&key);
      }
      return;
    }
    let Some(next) = current
      .as_object_mut()
      .and_then(|object| object.get_mut(&key))
    else {
      return;
    };
    current = next;
  }
}

fn config_for_sync_export(config: LauncherConfig) -> Result<Value, serde_json::Error> {
  let mut value = serde_json::to_value(config)?;
  for key in SYNC_LOCAL_FIELDS {
    remove_sync_field(&mut value, key);
  }
  Ok(value)
}

#[tauri::command]
pub fn retrieve_launcher_config(app: AppHandle) -> USTBLResult<LauncherConfig> {
  let binding = app.state::<Mutex<LauncherConfig>>();
  let state = binding.lock()?;
  Ok(state.clone())
}

#[tauri::command]
pub fn update_launcher_config(app: AppHandle, key_path: String, value: String) -> USTBLResult<()> {
  let config_binding = app.state::<Mutex<LauncherConfig>>();
  let mut config_state = config_binding.lock()?;
  let key_path = camel_to_snake_case(key_path.as_str());
  config_state.partial_update(&app, &key_path, &value)?;
  config_state.save()?;
  Ok(())
}

#[tauri::command]
pub fn restore_launcher_config(app: AppHandle) -> USTBLResult<LauncherConfig> {
  let mut default_config = LauncherConfig::default();
  default_config.setup_with_app(&app)?;

  let binding = app.state::<Mutex<LauncherConfig>>();
  let mut state = binding.lock()?;

  let preserved_fields = &["run_count"];
  state.replace_with_preserved(default_config, preserved_fields);
  state.save()?;
  Ok(state.clone())
}

#[tauri::command]
pub async fn export_launcher_config(
  app: AppHandle,
  client: tauri::State<'_, reqwest::Client>,
) -> USTBLResult<String> {
  let binding = app.state::<Mutex<LauncherConfig>>();
  let state = config_for_sync_export(binding.lock()?.clone())?;
  match client
    .post("https://mc.sjtu.cn/api-sjmcl/settings")
    .json(&json!({
      "version": app.package_info().version.to_string(),
      "json_data": state,
    }))
    .send()
    .await
  {
    Ok(response) => {
      let status = response.status();
      let json: Value = response
        .json()
        .await
        .map_err(|_| LauncherConfigError::FetchError)?;
      if status.is_success() {
        let code = json["code"]
          .as_str()
          .ok_or(LauncherConfigError::FetchError)?
          .to_string();

        Ok(code)
      } else {
        Err(LauncherConfigError::FetchError.into())
      }
    }
    Err(_) => Err(LauncherConfigError::FetchError.into()),
  }
}

#[tauri::command]
pub async fn import_launcher_config(
  app: AppHandle,
  client: tauri::State<'_, reqwest::Client>,
  code: String,
) -> USTBLResult<LauncherConfig> {
  match client
    .post("https://mc.sjtu.cn/api-sjmcl/validate")
    .json(&json!({
      "version": app.package_info().version.to_string(),
      "code": code,
    }))
    .send()
    .await
  {
    Ok(response) => {
      let status = response.status();
      let json: serde_json::Value = response
        .json()
        .await
        .map_err(|_| LauncherConfigError::FetchError)?;
      if status.is_success() {
        let new_config: LauncherConfig =
          serde_json::from_value(json).map_err(|_| LauncherConfigError::FetchError)?;
        let binding = app.state::<Mutex<LauncherConfig>>();
        let mut state = binding.lock()?;

        state.replace_with_preserved(new_config, SYNC_LOCAL_FIELDS);
        state.save()?;

        Ok(state.clone())
      } else {
        let message = json["message"]
          .as_str()
          .ok_or(LauncherConfigError::FetchError)?;
        match message {
          "Invalid code" => Err(LauncherConfigError::InvalidCode.into()),
          "Code expired" => Err(LauncherConfigError::CodeExpired.into()),
          "Version mismatch" => Err(LauncherConfigError::VersionMismatch.into()),
          _ => Err(LauncherConfigError::FetchError.into()),
        }
      }
    }
    Err(_err) => Err(LauncherConfigError::FetchError.into()),
  }
}

#[tauri::command]
pub fn reveal_launcher_config() -> USTBLResult<()> {
  let file_path = LauncherConfig::file_path();
  reveal_item_in_dir(file_path).map_err(USTBLError::from)
}

fn is_background_file_name(file_name: &str) -> bool {
  let path = Path::new(file_name);
  is_plain_file_name(path)
    && path
      .extension()
      .and_then(|extension| extension.to_str())
      .map(|extension| extension.to_ascii_lowercase())
      .is_some_and(|extension| BACKGROUND_EXTENSIONS.contains(&extension.as_str()))
}

#[tauri::command]
pub fn retrieve_custom_background_list(app: AppHandle) -> USTBLResult<Vec<String>> {
  let custom_bg_dir = app
    .path()
    .resolve::<PathBuf>("UserContent/Backgrounds".into(), BaseDirectory::AppData)?;

  if !custom_bg_dir.exists() {
    return Ok(Vec::new());
  }

  let file_names: Vec<String> = fs::read_dir(custom_bg_dir)?
    .filter_map(|entry| entry.ok())
    .filter_map(|entry| {
      let file_name = entry.file_name().into_string().ok()?;

      if is_background_file_name(&file_name) {
        Some(file_name)
      } else {
        None
      }
    })
    .collect();

  Ok(file_names)
}

#[tauri::command]
pub fn add_custom_background(app: AppHandle, source_src: String) -> USTBLResult<String> {
  let source_path = Path::new(&source_src);
  if !source_path.exists() || !source_path.is_file() {
    return Ok(String::new());
  }

  // Copy to custom background dir under tauri's pre-defined app_data dir
  let custom_bg_dir = app
    .path()
    .resolve::<PathBuf>("UserContent/Backgrounds".into(), BaseDirectory::AppData)?;

  if !custom_bg_dir.exists() {
    fs::create_dir_all(&custom_bg_dir)?;
  }

  let file_name = source_path.file_name().unwrap();
  let dest_path = generate_unique_filename(&custom_bg_dir, file_name);
  fs::copy(source_path, &dest_path)?;

  Ok(dest_path.file_name().unwrap().to_string_lossy().to_string())
}

#[tauri::command]
pub fn delete_custom_background(app: AppHandle, file_name: String) -> USTBLResult<()> {
  if !is_background_file_name(&file_name) {
    return Err(LauncherConfigError::FileDeletionFailed.into());
  }
  let custom_bg_dir = app
    .path()
    .resolve::<PathBuf>("UserContent/Backgrounds".into(), BaseDirectory::AppData)?;
  let file_path = custom_bg_dir.join(file_name);

  if file_path.exists() && file_path.is_file() {
    fs::remove_file(&file_path)?;
  }
  Ok(())
}

#[tauri::command]
pub async fn retrieve_java_list(app: AppHandle) -> USTBLResult<Vec<JavaInfo>> {
  refresh_and_update_javas(&app).await; // firstly refresh and update
  let binding = app.state::<Mutex<Vec<JavaInfo>>>();
  let state = binding.lock()?;
  Ok(state.clone())
}

#[tauri::command]
pub async fn validate_java(java_path: String) -> USTBLResult<()> {
  if get_java_info_from_release_file(&java_path)
    .1
    .or_else(|| get_java_info_from_command(&java_path).1)
    .is_some()
  {
    Ok(())
  } else {
    Err(LauncherConfigError::JavaExecInvalid.into())
  }
}

#[tauri::command]
pub async fn download_mojang_java(app: AppHandle, version: String) -> USTBLResult<()> {
  let download_params = build_mojang_java_download_params(&app, &version).await?;

  schedule_progressive_task_group(
    app,
    format!("mojang-java?{}", version),
    download_params,
    true,
  )
  .await?;

  Ok(())
}

#[tauri::command]
pub fn game_directory_exists(app: AppHandle, dir: String) -> USTBLResult<bool> {
  let directory = PathBuf::from(dir);
  let binding = app.state::<Mutex<LauncherConfig>>();
  let state = binding.lock()?;
  if !state
    .local_game_directories
    .iter()
    .any(|known| known.dir == directory)
  {
    return Err(LauncherConfigError::GameDirNotExist.into());
  }
  directory.try_exists().map_err(Into::into)
}

#[tauri::command]
pub async fn check_game_directory(app: AppHandle, dir: String) -> USTBLResult<String> {
  let local_game_directories: Vec<_>;
  {
    let binding = app.state::<Mutex<LauncherConfig>>();
    let state = binding.lock()?;
    local_game_directories = state.local_game_directories.clone();
  }
  let directory = PathBuf::from(&dir);

  if local_game_directories.iter().any(|d| d.dir == directory) {
    return Err(LauncherConfigError::GameDirAlreadyAdded.into());
  }
  if !directory.exists() {
    return Err(LauncherConfigError::GameDirNotExist.into());
  }

  if !refresh_instances(
    &app,
    &GameDirectory {
      dir: directory.clone(),
      name: "".to_string(),
    },
    false,
  )
  .await
  .unwrap_or_default()
  .is_empty()
  {
    return Ok("".to_string());
  }

  let sub_dirs = get_subdirectories(&directory).unwrap_or_default();
  for sub_dir in sub_dirs.into_iter().filter(|d| {
    matches!(
      d.file_name().and_then(|n| n.to_str()),
      Some(".minecraft") | Some("minecraft")
    )
  }) {
    if !refresh_instances(
      &app,
      &GameDirectory {
        dir: sub_dir.clone(),
        name: "".to_string(),
      },
      false,
    )
    .await
    .unwrap_or_default()
    .is_empty()
    {
      return Ok(sub_dir.to_str().unwrap().to_string());
    }
  }

  Ok("".to_string())
}

fn has_reparse_point(path: &Path) -> bool {
  fs::symlink_metadata(path)
    .map(|metadata| metadata.file_type().is_symlink())
    .unwrap_or(false)
}

fn safe_download_path(root: &Path, dest: &Path) -> bool {
  if has_reparse_point(root) {
    return false;
  }
  let Ok(relative) = dest.strip_prefix(root) else {
    return false;
  };
  let mut current = root.to_path_buf();
  for component in relative.components() {
    current.push(component);
    if has_reparse_point(&current) {
      return false;
    }
  }
  true
}

fn delete_downloaded_file(root: &Path, dest: &Path) -> std::io::Result<()> {
  if !safe_download_path(root, dest) {
    return Ok(());
  }
  match fs::remove_file(dest) {
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
    other => other,
  }
}

fn clear_cache_records(cache_path: &Path) -> std::io::Result<()> {
  if has_reparse_point(cache_path) {
    return Ok(());
  }
  for entry in fs::read_dir(cache_path)? {
    let path = entry?.path();
    let Some(name) = path.file_name().map(|name| name.to_string_lossy()) else {
      continue;
    };
    if !name.starts_with("task-") || !name.ends_with(".json") || has_reparse_point(&path) {
      continue;
    }
    if let Ok(desc) = PDesc::<PTaskParam>::load(&path) {
      if let PTaskParam::Download(param) = desc.payload {
        delete_downloaded_file(cache_path, &param.dest)?;
      }
    }
    fs::remove_file(path)?;
  }
  Ok(())
}

#[tauri::command]
pub async fn clear_download_cache(app: AppHandle) -> USTBLResult<()> {
  let launcher_config = app.state::<Mutex<LauncherConfig>>();
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();

  if monitor.has_active_download_tasks() {
    return Err(LauncherConfigError::HasActiveDownloadTasks.into());
  }

  let cache_path = {
    let config = launcher_config.lock()?;
    config.download.cache.directory.clone()
  };

  clear_cache_records(&cache_path).map_err(|_| LauncherConfigError::FileDeletionFailed)?;

  Ok(())
}

#[tauri::command]
pub async fn check_launcher_update(app: AppHandle) -> USTBLResult<VersionMetaInfo> {
  let config_binding = app.state::<Mutex<LauncherConfig>>();
  let current_version = {
    let config_state = config_binding.lock()?;
    config_state.basic_info.launcher_version.clone()
  };

  // skip non-semver versions
  if semver::Version::parse(&current_version).is_err() {
    return Ok(VersionMetaInfo::default());
  }

  if let Ok(Some(new_version)) = fetch_latest_version(&app).await {
    if let (Ok(current), Ok(latest)) = (
      semver::Version::parse(&current_version),
      semver::Version::parse(&new_version.version),
    ) {
      return Ok(match latest.cmp(&current) {
        std::cmp::Ordering::Greater => new_version,
        std::cmp::Ordering::Equal => VersionMetaInfo {
          version: "up2date".to_string(),
          ..Default::default()
        },
        std::cmp::Ordering::Less => VersionMetaInfo::default(),
      });
    }
  }

  Ok(VersionMetaInfo::default())
}

#[tauri::command]
pub async fn download_launcher_update(app: AppHandle, version: VersionMetaInfo) -> USTBLResult<()> {
  if version.version.is_empty() || version.version == "up2date" {
    Ok(())
  } else {
    // TODO: handle already downloaded case
    return download_target_version(&app, version).await;
  }
}

#[tauri::command]
pub async fn verify_launcher_update(downloaded_filename: String) -> USTBLResult<String> {
  Ok(
    updater::verify_downloaded_update(&downloaded_filename)
      .await?
      .version,
  )
}

#[tauri::command]
pub async fn install_launcher_update(
  app: AppHandle,
  downloaded_filename: String,
  restart: bool,
) -> USTBLResult<()> {
  // Re-hash at the execution boundary; the confirmation check is not a capability.
  let update = updater::verify_downloaded_update(&downloaded_filename).await?;
  #[cfg(target_os = "windows")]
  {
    updater::install_update_windows(&app, update.path, restart).await
  }
  #[cfg(target_os = "macos")]
  {
    updater::install_update_macos(&app, update.path, update.version, restart).await
  }
  #[cfg(target_os = "linux")]
  {
    let _ = update;
    Ok(()) // No supported
  }
}

#[cfg(test)]
mod background_path_tests {
  use super::*;

  #[test]
  fn background_deletion_accepts_only_plain_image_file_names() {
    for name in ["image.png", "photo.JPG", "animated.webp"] {
      assert!(is_background_file_name(name), "{name}");
    }
    for name in ["../secret.txt", "folder/image.png", "image.exe", "image"] {
      assert!(!is_background_file_name(name), "{name}");
    }
  }
}

#[cfg(test)]
mod sync_config_tests {
  use super::*;
  use crate::partial::PartialAccess;

  fn local_config() -> LauncherConfig {
    let mut config = LauncherConfig::default();
    config.run_count = 42;
    config.local_game_directories = vec![GameDirectory {
      name: "Local games".into(),
      dir: PathBuf::from(r"D:\Games"),
    }];
    config.extra_java_paths = vec![r"D:\Java\bin\javaw.exe".into()];
    config.global_game_config.game_java.exec_path = r"D:\Java\bin\javaw.exe".into();
    config
      .global_game_config
      .advanced
      .custom_commands
      .precall_command = "pre-call".into();
    config
      .global_game_config
      .advanced
      .custom_commands
      .post_exit_command = "post-exit".into();
    config
      .global_game_config
      .advanced
      .custom_commands
      .wrapper_launcher = "wrapper".into();
    config.global_game_config.advanced.jvm.args = "-Xmx4G".into();
    config.download.proxy.enabled = true;
    config.download.proxy.host = "127.0.0.1".into();
    config.download.proxy.port = 7890;
    config.download.cache.directory = PathBuf::from(r"D:\Cache");
    config
  }

  #[test]
  fn sync_import_preserves_local_execution_and_network_fields() {
    let local = local_config();
    let mut incoming = LauncherConfig::default();
    incoming.general.general.language = "en".into();
    incoming.states.instance_groups.order = vec!["custom".into()];
    incoming.discover_source_endpoints = vec![("https://example.com/rss".into(), true)];
    let mut merged = local.clone();

    merged.replace_with_preserved(incoming, SYNC_LOCAL_FIELDS);

    for key in SYNC_LOCAL_FIELDS {
      assert_eq!(
        merged.access(key).unwrap(),
        local.access(key).unwrap(),
        "{key}"
      );
    }
    assert_eq!(merged.general.general.language, "en");
    assert_eq!(merged.states.instance_groups.order, vec!["custom"]);
    assert_eq!(
      merged.discover_source_endpoints,
      vec![("https://example.com/rss".into(), true)]
    );
  }

  #[test]
  fn sync_export_does_not_upload_local_execution_or_network_values() {
    let exported = config_for_sync_export(local_config()).unwrap();

    for key in SYNC_LOCAL_FIELDS {
      let value = key.split('.').try_fold(&exported, |current, component| {
        current.get(snake_to_camel_case(component))
      });
      assert_eq!(value, None, "{key}");
    }
    // v0.5.4 can still import the object with these defaulted fields omitted.
    serde_json::from_value::<LauncherConfig>(exported).unwrap();
  }
}

#[cfg(test)]
mod download_cache_tests {
  use super::*;
  use crate::storage::test_support::TempDir;
  use crate::tasks::download::{DownloadParam, DownloadTransferOptions};
  use crate::tasks::streams::desc::PStatus;

  fn write_record(path: &Path, dest: PathBuf) {
    let param = DownloadParam {
      src: tauri::Url::parse("https://example.com/file.jar").unwrap(),
      dest,
      filename: None,
      sha1: None,
      custom_headers: None,
      transfer_options: DownloadTransferOptions::default(),
    };
    PDesc::new(1, None, 0, PTaskParam::Download(param), PStatus::Completed)
      .save(&path.to_path_buf())
      .unwrap();
  }

  #[test]
  fn only_recorded_downloads_and_records_are_removed() {
    let dir = TempDir::new("cache-clean");
    let dest = dir.0.join("file.jar");
    let record = dir.0.join("task-1.json");
    let other = dir.0.join("keep.txt");
    fs::write(&dest, b"jar").unwrap();
    fs::write(&other, b"keep").unwrap();
    write_record(&record, dest.clone());
    clear_cache_records(&dir.0).unwrap();
    assert!(!dest.exists());
    assert!(!record.exists());
    assert!(other.exists());
  }

  #[test]
  fn a_record_cannot_delete_a_file_outside_the_cache() {
    let dir = TempDir::new("cache-outside");
    let outside = TempDir::new("cache-outside-file");
    let dest = outside.0.join("outside.jar");
    let record = dir.0.join("task-2.json");
    fs::write(&dest, b"outside").unwrap();
    write_record(&record, dest.clone());
    clear_cache_records(&dir.0).unwrap();
    assert!(dest.exists());
    assert!(!record.exists());
  }

  #[cfg(windows)]
  #[test]
  fn a_cache_root_reparse_point_is_not_followed() {
    let actual = TempDir::new("cache-actual");
    let link = std::env::temp_dir().join(format!("cache-link-{}", uuid::Uuid::new_v4()));
    let status = std::process::Command::new("cmd")
      .args(["/c", "mklink", "/J"])
      .arg(&link)
      .arg(&actual.0)
      .status()
      .unwrap();
    if !status.success() {
      println!("cannot create a junction for this test");
      return;
    }
    let record = link.join("task-3.json");
    write_record(&record, actual.0.join("file.jar"));
    clear_cache_records(&link).unwrap();
    assert!(record.exists());
    let _ = std::fs::remove_dir(link);
  }
}
