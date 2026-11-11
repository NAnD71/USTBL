use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Url};
use tauri_plugin_http::reqwest;

use crate::error::USTBLResult;
use crate::instance::models::misc::InstanceError;
use crate::resource::helpers::misc::get_download_api;
use crate::resource::models::{ResourceType, SourceType};
use crate::storage::{load_json_async, save_json_async};
use crate::utils::web::fetch_json_with_fallbacks;

const VANILLA_METADATA_DOWNLOAD_ATTEMPTS: usize = 10;

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(default)]
pub struct AssetIndex {
  pub objects: HashMap<String, AssetIndexItem>,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(default)]
pub struct AssetIndexItem {
  pub hash: String,
  pub size: i64,
}

impl AssetIndexItem {
  /// `<first two hex digits>/<hash>` below `assets/objects`. The hash comes from
  /// an index a pack or a mirror can influence, so anything but a SHA-1 in hex
  /// is refused: it would otherwise reach `[..2]` (a panic on short or
  /// multi-byte text) and the path (`..` segments, separators).
  pub fn object_path(&self) -> USTBLResult<String> {
    if self.hash.len() != 40 || !self.hash.bytes().all(|b| b.is_ascii_hexdigit()) {
      return Err(InstanceError::AssetIndexParseError.into());
    }
    Ok(format!("{}/{}", &self.hash[..2], self.hash))
  }
}

/// The path of `indexes/<id>.json` below the assets directory. The id comes from
/// the version JSON or a MultiMC patch, so it must be a plain file name stem.
pub fn asset_index_file(assets_dir: &Path, asset_index_id: &str) -> USTBLResult<PathBuf> {
  if asset_index_id.is_empty()
    || asset_index_id == "."
    || asset_index_id == ".."
    || !asset_index_id
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
  {
    return Err(InstanceError::AssetIndexParseError.into());
  }
  Ok(assets_dir.join(format!("indexes/{asset_index_id}.json")))
}

pub async fn load_asset_index(
  app: &AppHandle,
  asset_index_path: &Path,
  asset_index_id: &str,
  asset_index_sha1: &str,
  asset_index_url: &str,
  priority: &[SourceType],
) -> USTBLResult<AssetIndex> {
  // Callers build `asset_index_path` themselves; the id also lands in URLs here.
  asset_index_file(Path::new(""), asset_index_id)?;
  if asset_index_path.exists() {
    let asset_index = load_json_async::<AssetIndex>(asset_index_path)
      .await
      .map_err(|_| InstanceError::AssetIndexParseError)?;

    Ok(asset_index)
  } else {
    let original_url = Url::parse(asset_index_url).map_err(|_| InstanceError::NetworkError)?;
    let official_url = Url::parse(&format!(
      "https://piston-meta.mojang.com/v1/packages/{asset_index_sha1}/{asset_index_id}.json"
    ))
    .map_err(|_| InstanceError::NetworkError)?;
    let mut sources = Vec::new();
    for source in priority {
      let candidate = match source {
        SourceType::Official => official_url.clone(),
        SourceType::BMCLAPIMirror => get_download_api(*source, ResourceType::Launcher)?
          .join(&format!("assets/indexes/{asset_index_id}.json"))?,
      };
      if !sources.contains(&candidate) {
        sources.push(candidate);
      }
    }
    if sources.is_empty() {
      sources.push(original_url.clone());
    }
    if !sources.contains(&original_url) {
      sources.push(original_url);
    }

    let client = app.state::<reqwest::Client>();
    let asset_index =
      fetch_json_with_fallbacks(client.inner(), &sources, VANILLA_METADATA_DOWNLOAD_ATTEMPTS)
        .await?;

    save_json_async(&asset_index, asset_index_path).await?;

    Ok(asset_index)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn item(hash: &str) -> AssetIndexItem {
    AssetIndexItem {
      hash: hash.into(),
      size: 1,
    }
  }

  #[test]
  fn real_index_ids_stay_inside_the_indexes_directory() {
    let assets = Path::new("assets");
    // Ids Mojang and MultiMC metadata use, GTNH's 1.7.10 included.
    for id in [
      "1.7.10", "legacy", "pre-1.6", "12", "1.21.4", "26w14a", "1.20+pre",
    ] {
      assert_eq!(
        asset_index_file(assets, id).unwrap(),
        assets.join(format!("indexes/{id}.json"))
      );
    }
    for id in [
      "",
      ".",
      "..",
      "../../../options",
      "..\\x",
      "a/b",
      "/abs",
      "C:x",
      "x%2e%2e",
      "a b",
      "a\0b",
      "版本",
    ] {
      assert!(asset_index_file(assets, id).is_err(), "{id:?} was accepted");
    }
  }

  #[test]
  fn object_paths_take_only_sha1_hex() {
    let hash = "bdf48ef6b5d0d23bbb02e17d04865216179f510a";
    assert_eq!(item(hash).object_path().unwrap(), format!("bd/{hash}"));
    assert!(item(&hash.to_uppercase()).object_path().is_ok());
    // `[..2]` used to panic on the first two and escape the objects directory on the last.
    for bad in [
      "",
      "a",
      "é0000000000000000000000000000000000000",
      "../../../../etc/passwd",
      "..\\..\\x",
      "bd/../../escape00000000000000000000000000",
      &hash[..39],
      format!("{hash}0").as_str(),
    ] {
      assert!(item(bad).object_path().is_err(), "{bad:?} was accepted");
    }
  }
}
