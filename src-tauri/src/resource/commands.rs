use crate::error::USTBLResult;
use crate::instance::helpers::client_json::McClientInfo;
use crate::instance::helpers::misc::get_instance_subdir_path_by_id;
use crate::instance::models::misc::{InstanceSubdirType, ModLoaderType};
use crate::launcher_config::models::LauncherConfig;
use crate::resource::helpers::curseforge::{
  fetch_remote_resource_by_id_curseforge, fetch_remote_resource_by_local_curseforge,
  fetch_resource_list_by_name_curseforge, fetch_resource_version_packs_curseforge,
};
use crate::resource::helpers::loader_meta::fabric::get_fabric_meta_by_game_version;
use crate::resource::helpers::loader_meta::forge::get_forge_meta_by_game_version;
use crate::resource::helpers::loader_meta::neoforge::get_neoforge_meta_by_game_version;
use crate::resource::helpers::loader_meta::optifine::get_optifine_meta_by_game_version;
use crate::resource::helpers::misc::get_source_priority_list;
use crate::resource::helpers::modrinth::{
  fetch_remote_resource_by_id_modrinth, fetch_remote_resource_by_local_modrinth,
  fetch_resource_list_by_name_modrinth, fetch_resource_version_packs_modrinth,
};
use crate::resource::helpers::translation::translate_resource_descriptions as translate_descriptions_batch;
use crate::resource::helpers::version_manifest::get_game_version_manifest;
use crate::resource::models::{
  GameClientResourceInfo, ModLoaderResourceInfo, ModUpdateQuery, OptiFineResourceInfo,
  OtherResourceFileInfo, OtherResourceInfo, OtherResourceSearchQuery, OtherResourceSearchRes,
  OtherResourceSource, OtherResourceVersionPack, OtherResourceVersionPackQuery, ResourceError,
};
use crate::tasks::commands::schedule_progressive_task_group;
use crate::tasks::download::DownloadParam;
use crate::tasks::PTaskParam;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_http::reqwest;

#[tauri::command]
pub async fn fetch_game_version_list(app: AppHandle) -> USTBLResult<Vec<GameClientResourceInfo>> {
  let priority_list = {
    let launcher_config_state = app.state::<Mutex<LauncherConfig>>();
    let launcher_config = launcher_config_state.lock()?;
    get_source_priority_list(&launcher_config)
  };
  get_game_version_manifest(&app, &priority_list).await
}

#[tauri::command]
pub async fn fetch_game_version_specific(
  app: AppHandle,
  game_version: String,
) -> USTBLResult<GameClientResourceInfo> {
  let all_versions = fetch_game_version_list(app.clone()).await?;

  all_versions
    .into_iter()
    .find(|item| item.id == game_version)
    .ok_or_else(|| ResourceError::ClientVersionNotFound.into())
}

#[tauri::command]
pub async fn fetch_mod_loader_version_list(
  app: AppHandle,
  game_version: String,
  mod_loader_type: ModLoaderType,
) -> USTBLResult<Vec<ModLoaderResourceInfo>> {
  let priority_list = {
    let launcher_config_state = app.state::<Mutex<LauncherConfig>>();
    let launcher_config = launcher_config_state.lock()?;
    get_source_priority_list(&launcher_config)
  };
  match mod_loader_type {
    ModLoaderType::Forge | ModLoaderType::LegacyForge => {
      Ok(get_forge_meta_by_game_version(&app, &priority_list, &game_version).await?)
    }
    ModLoaderType::Fabric => {
      Ok(get_fabric_meta_by_game_version(&app, &priority_list, &game_version).await?)
    }
    ModLoaderType::NeoForge => {
      Ok(get_neoforge_meta_by_game_version(&app, &priority_list, &game_version).await?)
    }
    // TODO here
    _ => Err(ResourceError::NoDownloadApi.into()),
  }
}

#[tauri::command]
pub async fn fetch_optifine_version_list(
  app: AppHandle,
  game_version: String,
) -> USTBLResult<Vec<OptiFineResourceInfo>> {
  let priority_list = {
    let launcher_config_state = app.state::<Mutex<LauncherConfig>>();
    let launcher_config = launcher_config_state.lock()?;
    get_source_priority_list(&launcher_config)
  };
  get_optifine_meta_by_game_version(&app, &priority_list, &game_version).await
}

#[tauri::command]
pub async fn fetch_resource_list_by_name(
  app: AppHandle,
  download_source: OtherResourceSource,
  query: OtherResourceSearchQuery,
) -> USTBLResult<OtherResourceSearchRes> {
  match download_source {
    OtherResourceSource::Modrinth => Ok(fetch_resource_list_by_name_modrinth(&app, &query).await?),
    OtherResourceSource::CurseForge => {
      Ok(fetch_resource_list_by_name_curseforge(&app, &query).await?)
    }
    _ => Err(ResourceError::NoDownloadApi.into()),
  }
}

/// Translated descriptions for a page of search results, as `id -> text`.
/// Ids without a translation (or whose request failed) are absent.
#[tauri::command]
pub async fn translate_resource_descriptions(
  app: AppHandle,
  download_source: OtherResourceSource,
  resource_ids: Vec<String>,
) -> USTBLResult<HashMap<String, String>> {
  Ok(translate_descriptions_batch(&app, &download_source, &resource_ids).await)
}

#[tauri::command]
pub async fn fetch_resource_version_packs(
  app: AppHandle,
  download_source: OtherResourceSource,
  query: OtherResourceVersionPackQuery,
) -> USTBLResult<Vec<OtherResourceVersionPack>> {
  match download_source {
    OtherResourceSource::Modrinth => Ok(fetch_resource_version_packs_modrinth(&app, &query).await?),
    OtherResourceSource::CurseForge => {
      Ok(fetch_resource_version_packs_curseforge(&app, &query).await?)
    }
    _ => Err(ResourceError::NoDownloadApi.into()),
  }
}

#[tauri::command]
pub async fn download_game_server(
  app: AppHandle,
  client: State<'_, reqwest::Client>,
  resource_info: GameClientResourceInfo,
  dest: String,
) -> USTBLResult<()> {
  let version_details = client
    .get(&resource_info.url)
    .send()
    .await
    .map_err(|_| ResourceError::NetworkError)?
    .json::<McClientInfo>()
    .await
    .map_err(|_| ResourceError::ParseError)?;

  let download_info = version_details
    .downloads
    .get("server")
    .ok_or(ResourceError::ParseError)?;

  schedule_progressive_task_group(
    app,
    format!("game-server?{}", resource_info.id),
    vec![PTaskParam::Download(DownloadParam {
      src: url::Url::parse(&download_info.url.clone()).map_err(|_| ResourceError::ParseError)?,
      dest: dest.clone().into(),
      filename: None,
      sha1: Some(download_info.sha1.clone()),
      custom_headers: None,
      transfer_options: Default::default(),
    })],
    true,
  )
  .await?;

  Ok(())
}

#[tauri::command]
pub async fn fetch_remote_resource_by_local(
  app: AppHandle,
  download_source: OtherResourceSource,
  file_path: String,
) -> USTBLResult<OtherResourceFileInfo> {
  match download_source {
    OtherResourceSource::Modrinth => {
      Ok(fetch_remote_resource_by_local_modrinth(&app, &file_path).await?)
    }
    OtherResourceSource::CurseForge => {
      Ok(fetch_remote_resource_by_local_curseforge(&app, &file_path).await?)
    }
    _ => Err(ResourceError::NoDownloadApi.into()),
  }
}

fn new_mod_download_path(path: &Path) -> PathBuf {
  let name = path
    .file_name()
    .map(|name| name.to_string_lossy())
    .unwrap_or_default();
  path.with_file_name(format!(".{name}.new"))
}

fn finalize_mod_file(query: &ModUpdateQuery, mods_dir: &Path) -> std::io::Result<()> {
  let new_file_path = mods_dir.join(&query.file_name);
  let temporary = new_mod_download_path(&new_file_path);
  if query.old_file_path != new_file_path.to_string_lossy().to_string() {
    let old_backup_path = format!("{}.old", query.old_file_path);
    std::fs::rename(&query.old_file_path, &old_backup_path)?;
    if let Err(error) = std::fs::rename(&temporary, &new_file_path) {
      // A failed replacement must not leave the only old copy at a new name.
      let _ = std::fs::rename(old_backup_path, &query.old_file_path);
      return Err(error);
    }
    return Ok(());
  }
  std::fs::rename(temporary, new_file_path)
}

fn discard_mod_download(query: &ModUpdateQuery, mods_dir: &Path) {
  let temporary = new_mod_download_path(&mods_dir.join(&query.file_name));
  let _ = std::fs::remove_file(temporary);
}

#[tauri::command]
pub async fn update_mods(
  app: AppHandle,
  instance_id: String,
  queries: Vec<ModUpdateQuery>,
) -> USTBLResult<()> {
  if queries.is_empty() {
    return Ok(());
  }

  let mods_dir = match get_instance_subdir_path_by_id(&app, &instance_id, &InstanceSubdirType::Mods)
  {
    Some(path) => path,
    None => return Ok(()),
  };

  let mut download_tasks = Vec::new();
  for query in &queries {
    let file_path = new_mod_download_path(&mods_dir.join(&query.file_name));
    let download_param = DownloadParam {
      src: url::Url::parse(&query.url).map_err(|_| ResourceError::ParseError)?,
      dest: file_path,
      filename: None,
      sha1: Some(query.sha1.clone()),
      custom_headers: None,
      transfer_options: Default::default(),
    };
    download_tasks.push(PTaskParam::Download(download_param));
  }

  let group =
    schedule_progressive_task_group(app.clone(), "mod-update".to_string(), download_tasks, true)
      .await?;
  let app_for_finish = app.clone();
  tauri::async_runtime::spawn(async move {
    let monitor = app_for_finish.state::<Pin<Box<crate::tasks::monitor::TaskMonitor>>>();
    let status = monitor.wait_for_group(&group.task_group).await;
    for query in &queries {
      if status == crate::tasks::events::GEventStatus::Completed {
        if let Err(error) = finalize_mod_file(query, &mods_dir) {
          log::error!("Failed to replace the old mod file: {error}");
          monitor.fail_group(&group.task_group);
        }
      } else {
        discard_mod_download(query, &mods_dir);
      }
    }
  });

  Ok(())
}

#[tauri::command]
pub async fn fetch_remote_resource_by_id(
  app: AppHandle,
  download_source: OtherResourceSource,
  resource_id: String,
) -> USTBLResult<OtherResourceInfo> {
  match download_source {
    OtherResourceSource::Modrinth => {
      Ok(fetch_remote_resource_by_id_modrinth(&app, &resource_id).await?)
    }
    OtherResourceSource::CurseForge => {
      Ok(fetch_remote_resource_by_id_curseforge(&app, &resource_id).await?)
    }
    _ => Err(ResourceError::NoDownloadApi.into()),
  }
}

#[cfg(test)]
mod mod_update_tests {
  use super::{discard_mod_download, finalize_mod_file};
  use crate::resource::models::ModUpdateQuery;
  use crate::storage::test_support::TempDir;
  use std::fs;

  fn query(root: &std::path::Path, old: &str) -> ModUpdateQuery {
    ModUpdateQuery {
      file_name: "new.jar".into(),
      old_file_path: root.join(old).to_string_lossy().to_string(),
      sha1: String::new(),
      url: "https://example.com/new.jar".into(),
    }
  }

  #[test]
  fn a_same_path_mod_is_replaced_only_at_finalize() {
    let dir = TempDir::new("mod-finalize-same");
    let target = dir.0.join("new.jar");
    let temp = dir.0.join(".new.jar.new");
    fs::write(&target, b"old").unwrap();
    fs::write(&temp, b"new").unwrap();
    finalize_mod_file(&query(&dir.0, "new.jar"), &dir.0).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert!(!dir.0.join("new.jar.old").exists());
  }

  #[test]
  fn a_renamed_mod_keeps_the_old_file_as_backup() {
    let dir = TempDir::new("mod-finalize-rename");
    let old = dir.0.join("old.jar");
    let temp = dir.0.join(".new.jar.new");
    fs::write(&old, b"old").unwrap();
    fs::write(&temp, b"new").unwrap();
    finalize_mod_file(&query(&dir.0, "old.jar"), &dir.0).unwrap();
    assert_eq!(fs::read(dir.0.join("old.jar.old")).unwrap(), b"old");
    assert_eq!(fs::read(dir.0.join("new.jar")).unwrap(), b"new");
  }

  #[test]
  fn a_failed_final_rename_restores_the_old_file() {
    let dir = TempDir::new("mod-finalize-rollback");
    let old = dir.0.join("old.jar");
    let target = dir.0.join("new.jar");
    let temp = dir.0.join(".new.jar.new");
    fs::write(&old, b"old").unwrap();
    fs::write(&temp, b"new").unwrap();
    fs::create_dir(&target).unwrap();
    assert!(finalize_mod_file(&query(&dir.0, "old.jar"), &dir.0).is_err());
    assert_eq!(fs::read(&old).unwrap(), b"old");
  }
}
