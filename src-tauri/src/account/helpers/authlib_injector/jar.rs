use crate::account::helpers::authlib_injector::constants::AUTHLIB_INJECTOR_JAR_NAME;
use crate::account::models::AccountError;
use crate::error::USTBLResult;
use crate::launcher_config::models::LauncherConfig;
use crate::resource::helpers::misc::{get_download_api, get_source_priority_list};
use crate::resource::models::{ResourceType, SourceType};
use crate::storage::{write_atomic, Durability};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;
use url::Url;

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
pub struct AuthlibInjectorMeta {
  pub version: String,
  pub download_url: String,
  #[serde(default)]
  pub checksums: Option<AuthlibInjectorChecksums>,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
pub struct AuthlibInjectorChecksums {
  pub sha256: String,
}

pub fn get_jar_path(app: &AppHandle) -> USTBLResult<PathBuf> {
  Ok(
    app
      .path()
      .resolve::<PathBuf>(AUTHLIB_INJECTOR_JAR_NAME.into(), BaseDirectory::AppData)?,
  )
}

async fn get_latest_meta(
  app: &AppHandle,
  priority_list: &[SourceType],
) -> USTBLResult<AuthlibInjectorMeta> {
  let client = app.state::<reqwest::Client>();

  for source in priority_list.iter() {
    let url = get_download_api(*source, ResourceType::AuthlibInjector)?;
    let response = client
      .get(url.join("artifact/latest.json")?)
      .send()
      .await
      .map_err(|_| AccountError::NetworkError)?;

    if response.status().is_success() {
      return Ok(
        response
          .json::<AuthlibInjectorMeta>()
          .await
          .map_err(|_| AccountError::ParseError)?,
      );
    }
  }

  Err(AccountError::NoDownloadApi.into())
}

fn get_local_version(app: &AppHandle) -> USTBLResult<String> {
  let jar_path = get_jar_path(app)?;
  if !jar_path.exists() {
    return Err(AccountError::NotFound.into());
  }

  let file = std::fs::File::open(jar_path)?;

  let mut archive = zip::ZipArchive::new(file)?;
  let mut file = archive.by_name("META-INF/MANIFEST.MF")?;
  let mut content = String::new();

  file.read_to_string(&mut content)?;

  let version_line = content
    .lines()
    .find(|line| line.starts_with("Implementation-Version:"))
    .ok_or(AccountError::ParseError)?;

  let version = version_line
    .split(':')
    .nth(1)
    .ok_or(AccountError::ParseError)?
    .trim()
    .to_string();

  Ok(version)
}

fn replace_verified(jar_path: &Path, bytes: &[u8], expected_sha256: &str) -> USTBLResult<()> {
  let actual = hex::encode(Sha256::digest(bytes));
  if !actual.eq_ignore_ascii_case(expected_sha256) {
    log::warn!("Authlib-Injector checksum mismatch: expected {expected_sha256}, got {actual}");
    return Err(AccountError::ParseError.into());
  }
  write_atomic(jar_path, bytes, Durability::Sync).map_err(|_| AccountError::SaveError)?;
  Ok(())
}

async fn download(app: &AppHandle, url: Url, expected_sha256: &str) -> USTBLResult<()> {
  let client = app.state::<reqwest::Client>();
  let jar_path = get_jar_path(app)?;

  let response = client
    .get(url)
    .send()
    .await
    .map_err(|_| AccountError::NetworkError)?;

  if response.status().is_success() {
    let bytes = response
      .bytes()
      .await
      .map_err(|_| AccountError::NetworkError)?;
    replace_verified(&jar_path, &bytes, expected_sha256)
  } else {
    Err(AccountError::NetworkError.into())
  }
}

pub async fn check_authlib_jar(app: &AppHandle) -> USTBLResult<()> {
  let latest_meta = {
    let config_state = app.state::<Mutex<LauncherConfig>>();
    let launcher_config = config_state.lock()?.clone();
    get_latest_meta(app, &get_source_priority_list(&launcher_config)).await?
  };

  if let Ok(local_version) = get_local_version(app) {
    if local_version == latest_meta.version {
      println!("Authlib-Injector up to date: {}", local_version);
      return Ok(());
    }
  }

  println!(
    "Authlib-Injector new version downloading: {}",
    latest_meta.version
  );
  let Some(expected_sha256) = latest_meta
    .checksums
    .as_ref()
    .map(|checksums| checksums.sha256.as_str())
  else {
    if get_jar_path(app)?.is_file() {
      log::warn!("Authlib-Injector latest.json has no sha256; keeping the existing jar");
      return Ok(());
    }
    return Err(AccountError::NoDownloadApi.into());
  };
  download(app, Url::parse(&latest_meta.download_url)?, expected_sha256).await?;

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::storage::test_support::TempDir;

  #[test]
  fn a_checksum_mismatch_preserves_the_existing_jar() {
    let dir = TempDir::new("authlib-checksum-mismatch");
    let jar = dir.0.join("authlib-injector.jar");
    std::fs::write(&jar, b"old jar").unwrap();
    let wrong = hex::encode(Sha256::digest(b"different jar"));

    assert!(replace_verified(&jar, b"new jar", &wrong).is_err());
    assert_eq!(std::fs::read(&jar).unwrap(), b"old jar");
  }

  #[test]
  fn a_verified_jar_atomically_replaces_the_old_one() {
    let dir = TempDir::new("authlib-checksum-match");
    let jar = dir.0.join("authlib-injector.jar");
    std::fs::write(&jar, b"old jar").unwrap();
    let expected = hex::encode(Sha256::digest(b"new jar")).to_uppercase();

    replace_verified(&jar, b"new jar", &expected).unwrap();
    assert_eq!(std::fs::read(&jar).unwrap(), b"new jar");
  }

  #[test]
  fn metadata_without_checksums_still_deserializes_for_the_existing_jar_fallback() {
    let meta: AuthlibInjectorMeta = serde_json::from_str(
      r#"{"version":"1.2.8","download_url":"https://example.com/authlib.jar"}"#,
    )
    .unwrap();
    assert!(meta.checksums.is_none());
  }
}
