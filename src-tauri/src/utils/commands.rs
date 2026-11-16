use crate::error::USTBLResult;
use crate::instance::helpers::misc::{
  get_instance_subdir_paths, is_any_instance_subdir_direct_child, is_instance_subdir_direct_child,
};
use crate::instance::models::misc::{Instance, InstanceError, InstanceSubdirType};
use crate::launcher_config::models::{LauncherConfig, LauncherConfigError, MemoryInfo};
use crate::utils::fs::{extract_filename as extract_filename_helper, path_within};
use crate::utils::sys_info::get_memory_info;
use font_loader::system_fonts;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;
use tauri_plugin_opener::OpenerExt;
use tokio::time::Instant;
use url::Url;

#[tauri::command]
pub fn retrieve_memory_info() -> USTBLResult<MemoryInfo> {
  Ok(get_memory_info())
}

#[tauri::command]
pub fn extract_filename(path_str: String, with_ext: bool) -> USTBLResult<String> {
  Ok(extract_filename_helper(&path_str, with_ext))
}

#[tauri::command]
pub fn delete_file(app: AppHandle, instance_id: String, path: String) -> USTBLResult<()> {
  let path = PathBuf::from(path);
  if !is_instance_subdir_direct_child(&app, &instance_id, &InstanceSubdirType::Mods, &path) {
    return Err(InstanceError::InvalidSourcePath.into());
  }
  fs::remove_file(&path).map_err(Into::into)
}

fn known_directory_roots(app: &AppHandle) -> USTBLResult<Vec<PathBuf>> {
  let mut roots = {
    let binding = app.state::<Mutex<LauncherConfig>>();
    let config = binding.lock()?;
    let mut roots = config
      .local_game_directories
      .iter()
      .map(|directory| directory.dir.clone())
      .collect::<Vec<_>>();
    roots.push(config.download.cache.directory.clone());
    roots
  };
  roots.push(
    app
      .path()
      .resolve::<PathBuf>("launcher".into(), BaseDirectory::AppLog)?,
  );
  let instances = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let state = binding.lock()?;
    state.values().cloned().collect::<Vec<_>>()
  };
  for instance in instances {
    roots.push(instance.version_path.clone());
    if let Some(mut paths) = get_instance_subdir_paths(
      app,
      &instance,
      &[
        &InstanceSubdirType::Assets,
        &InstanceSubdirType::Libraries,
        &InstanceSubdirType::Mods,
        &InstanceSubdirType::NativeLibraries,
        &InstanceSubdirType::ResourcePacks,
        &InstanceSubdirType::Root,
        &InstanceSubdirType::Saves,
        &InstanceSubdirType::Schematics,
        &InstanceSubdirType::Screenshots,
        &InstanceSubdirType::ServerResourcePacks,
        &InstanceSubdirType::ShaderPacks,
      ],
    ) {
      roots.append(&mut paths);
    }
  }
  Ok(roots)
}

#[tauri::command]
pub fn open_directory(app: AppHandle, path: String) -> USTBLResult<()> {
  let path = PathBuf::from(path);
  if !path.is_dir()
    || !known_directory_roots(&app)?
      .iter()
      .any(|root| path_within(root, &path))
  {
    return Err(InstanceError::InvalidSourcePath.into());
  }
  app
    .opener()
    .open_path(path.to_string_lossy(), None::<&str>)?;
  Ok(())
}

#[tauri::command]
pub async fn read_screenshot(app: AppHandle, path: String) -> USTBLResult<Vec<u8>> {
  let path = PathBuf::from(path);
  let valid_image = path
    .extension()
    .and_then(|extension| extension.to_str())
    .map(|extension| extension.to_ascii_lowercase())
    .is_some_and(|extension| matches!(extension.as_str(), "jpg" | "jpeg" | "png"));
  if !valid_image
    || !is_any_instance_subdir_direct_child(&app, &InstanceSubdirType::Screenshots, &path)
  {
    return Err(InstanceError::InvalidSourcePath.into());
  }
  tokio::fs::read(path).await.map_err(Into::into)
}

#[tauri::command]
pub fn retrieve_truetype_font_list() -> USTBLResult<Vec<String>> {
  let sysfonts = system_fonts::query_all();
  Ok(sysfonts)
}

#[tauri::command]
pub async fn check_service_availability(
  client: tauri::State<'_, reqwest::Client>,
  url: String,
) -> USTBLResult<u128> {
  let parsed_url = Url::parse(&url)
    .or_else(|_| Url::parse(&format!("https://{}", url)))
    .map_err(|_| LauncherConfigError::FetchError)?;

  let start = Instant::now();
  let res = client.get(parsed_url).send().await;

  match res {
    Ok(response) => {
      if response.status().is_success() || response.status().is_client_error() {
        Ok(start.elapsed().as_millis())
      } else {
        Err(LauncherConfigError::FetchError.into())
      }
    }
    Err(_) => Err(LauncherConfigError::FetchError.into()),
  }
}
