use super::diff::FileKey;
use super::ignores;
use super::plan::{local_changes, LocalChange, NoDiffReason};
use super::release::{Release, ReleaseFile};
use crate::error::USTBLResult;
use crate::instance::helpers::modpack::baseline::{hash_file, ModpackBaseline};
use crate::instance::models::misc::ModpackFileList;
use crate::instance::modpack_update::diff::DISABLED_SUFFIX;
use crate::instance::modpack_update::remote::{RemoteRule, RuleKind, RuleState};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeClass {
  Modified,
  Deleted,
  Toggled,
  OnceChanged,
  Added,
  AddedExcluded,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeDetailEntry {
  pub path: String,
  pub class: ChangeClass,
  pub counted: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub size: Option<u64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub baseline: Option<FileKey>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub local: Option<FileKey>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub hunks: Option<Vec<super::text_diff::DiffHunk>>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub no_diff: Option<NoDiffReason>,
  #[serde(default)]
  pub toggled: bool,
}

fn folded(path: &str) -> String {
  path.to_lowercase()
}

fn rule_for<'a>(rules: &'a [RemoteRule], path: &str) -> Option<&'a RemoteRule> {
  let path = folded(path);
  if let Some(rule) = rules
    .iter()
    .find(|rule| rule.kind == RuleKind::File && folded(&rule.path) == path)
  {
    return Some(rule);
  }
  rules
    .iter()
    .filter(|rule| rule.kind == RuleKind::Dir)
    .filter(|rule| path.starts_with(&format!("{}/", folded(&rule.path))))
    .max_by_key(|rule| folded(&rule.path).len())
}

fn default_included(path: &str, release: &Release) -> bool {
  !matches!(rule_for(&release.rules, path).map(|rule| rule.state), Some(RuleState::Ignore | RuleState::Delete))
}

fn suggested_included(path: &str, listed: &ModpackFileList) -> bool {
  !listed.suggested.iter().any(|suggested| {
    let key = folded(&suggested.path);
    match suggested.kind.as_str() {
      "file" => folded(path) == key,
      "dir" => folded(path).starts_with(&format!("{key}/")),
      _ => false,
    }
  })
}

pub fn added_is_counted(path: &str, release: &Release) -> bool {
  default_included(path, release)
    && !crate::instance::helpers::modpack::export::is_suggested_blacklisted(path)
    && content_path(path)
}

fn content_path(path: &str) -> bool {
  ["mods/", "resourcepacks/", "shaderpacks/"]
    .iter()
    .any(|prefix| folded(path).starts_with(prefix))
}

pub(crate) fn local_file_key(root: &Path, path: &str) -> Option<(FileKey, bool)> {
  let enabled = root.join(path);
  let disabled = root.join(format!("{path}{DISABLED_SUFFIX}"));
  let enabled_key = hash_file(&enabled).ok().map(|(sha1, size)| FileKey { sha1, size });
  let disabled_key = hash_file(&disabled).ok().map(|(sha1, size)| FileKey { sha1, size });
  match (enabled_key, disabled_key) {
    (Some(key), None) => Some((key, false)),
    (None, Some(key)) => Some((key, true)),
    (Some(key), Some(_)) => Some((key, false)),
    _ => None,
  }
}

/// The manifest name represented by a disk path. Only mods use `.disabled` as
/// a file attribute; a file with that suffix elsewhere is an ordinary path.
pub(crate) fn logical_disk_path(path: &str) -> &str {
  if folded(path).starts_with("mods/") && folded(path).ends_with(DISABLED_SUFFIX) {
    &path[..path.len() - DISABLED_SUFFIX.len()]
  } else {
    path
  }
}

fn add_entry(
  entries: &mut Vec<ChangeDetailEntry>,
  path: String,
  class: ChangeClass,
  counted: bool,
  size: Option<u64>,
  local: Option<(FileKey, bool)>,
  release: Option<&ReleaseFile>,
  change: Option<&LocalChange>,
) {
  let toggled = local
    .as_ref()
    .zip(release)
    .is_some_and(|((_, disabled), release)| *disabled != release.disabled);
  entries.push(ChangeDetailEntry {
    path,
    class,
    counted,
    size,
    baseline: change.map(|change| change.baseline.clone()),
    local: local.map(|(key, _)| key).or_else(|| change.and_then(|change| change.local.clone())),
    hunks: change.and_then(|change| change.hunks.clone()),
    no_diff: change.and_then(|change| change.no_diff),
    toggled: toggled || change.is_some_and(|change| change.toggled),
  });
}

pub async fn classify(
  root: &Path,
  baseline: &ModpackBaseline,
  release: &Release,
  listed: &ModpackFileList,
) -> USTBLResult<Vec<ChangeDetailEntry>> {
  let local = local_changes(root, baseline, true).await?;
  let local_by_path: HashMap<String, LocalChange> = local
    .into_iter()
    .map(|change| (folded(&change.path), change))
    .collect();
  let ignored = ignores::read_valid_for_release(root, baseline, release);
  let mut entries = Vec::new();
  for release_file in &release.files {
    if ignored.files.get(&release_file.path).is_some_and(|entry| entry.whole) {
      continue;
    }
    let path = &release_file.path;
    let local = local_file_key(root, path);
    let change = local_by_path.get(&folded(path));
    if release_file.once {
      if local.as_ref().is_none_or(|(key, _)| *key != release_file.key()) {
        add_entry(
          &mut entries,
          path.clone(),
          ChangeClass::OnceChanged,
          false,
          local.as_ref().map(|(key, _)| key.size),
          local,
          Some(release_file),
          None,
        );
      }
      continue;
    }
    let Some((local_key, local_disabled)) = local.as_ref() else {
      add_entry(
        &mut entries,
        path.clone(),
        ChangeClass::Deleted,
        true,
        None,
        None,
        Some(release_file),
        change,
      );
      continue;
    };
    if *local_key == release_file.key() && *local_disabled == release_file.disabled {
      continue;
    }
    let class = if *local_key == release_file.key() {
      ChangeClass::Toggled
    } else {
      ChangeClass::Modified
    };
    add_entry(
      &mut entries,
      path.clone(),
      class,
      true,
      Some(local_key.size),
      local,
      Some(release_file),
      change,
    );
  }
  let disk = crate::instance::modpack_publish::defaults::normalize_disk_files(&listed.all);
  let known: HashSet<String> = release.files.iter().map(|file| folded(&file.path)).collect();
  for file in disk {
    let path_key = folded(&file.path);
    if known.contains(&path_key) {
      continue;
    }
    if ignored.added.contains(&path_key) {
      continue;
    }
    let class = if default_included(&file.path, release) && suggested_included(&file.path, listed) {
      ChangeClass::Added
    } else {
      ChangeClass::AddedExcluded
    };
    let counted = class == ChangeClass::Added && content_path(&file.path);
    let size = root.join(if file.disabled { format!("{}{}", file.path, DISABLED_SUFFIX) } else { file.path.clone() });
    add_entry(
      &mut entries,
      file.path,
      class,
      counted,
      std::fs::metadata(size).ok().map(|meta| meta.len()),
      None,
      None,
      None,
    );
  }
  Ok(entries)
}

pub fn baseline_release(baseline: &ModpackBaseline) -> Release {
  Release::from_baseline(baseline, baseline.origin.version_id.clone())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::modpack::baseline::{BaselineFile, BaselineLoader};
  use crate::instance::models::misc::{InstanceOrigin, ModLoaderType, OriginPlatform};
  use crate::instance::modpack_update::remote::{RemoteRule, RuleKind, RuleState};
  use sha1::{Digest, Sha1};
  use std::fs;

  fn key(content: &str) -> FileKey {
    FileKey { sha1: hex::encode(Sha1::digest(content.as_bytes())), size: content.len() as u64 }
  }

  fn origin() -> InstanceOrigin {
    InstanceOrigin {
      platform: OriginPlatform::Campus,
      project_id: "p".into(),
      version_id: "7".into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
    }
  }

  fn release_file(path: &str, content: &str, disabled: bool, once: bool) -> ReleaseFile {
    let key = key(content);
    ReleaseFile { path: path.into(), sha1: key.sha1, size: key.size, disabled, once }
  }

  #[tokio::test]
  async fn classifies_every_release_and_added_file_kind() {
    let root = std::env::temp_dir().join(format!("ustbl-change-detail-{}", uuid::Uuid::new_v4()));
    for (path, content) in [
      ("config/edited.toml", "local"),
      ("config/once.toml", "changed"),
      ("mods/toggle.jar.disabled", "toggle"),
      ("mods/new.jar", "new"),
      ("config/new.toml", "new-config"),
      ("saves/world.dat", "save"),
      ("config/cache/new.toml", "cache"),
    ] {
      let file = root.join(path);
      fs::create_dir_all(file.parent().unwrap()).unwrap();
      fs::write(file, content).unwrap();
    }
    let managed = [
      ("config/edited.toml", "base"),
      ("config/once.toml", "original"),
      ("mods/toggle.jar", "toggle"),
      ("config/deleted.toml", "deleted"),
    ];
    let baseline = ModpackBaseline::build(
      &origin(),
      BaselineLoader { loader_type: ModLoaderType::Fabric, version: "0.16".into() },
      managed.iter().map(|(path, content)| BaselineFile::managed(path, key(content), false)).collect(),
    );
    let release = Release {
      schema: 1,
      version_id: "7".into(),
      label: "v7".into(),
      files: vec![
        release_file("config/edited.toml", "base", false, false),
        release_file("config/once.toml", "original", false, true),
        release_file("mods/toggle.jar", "toggle", false, false),
        release_file("config/deleted.toml", "deleted", false, false),
      ],
      rules: vec![RemoteRule { path: "config/cache".into(), kind: RuleKind::Dir, state: RuleState::Ignore }],
      rules_manifest: true,
    };
    let listed = ModpackFileList {
      all: vec![
        "config/edited.toml".into(),
        "config/once.toml".into(),
        "mods/toggle.jar.disabled".into(),
        "mods/new.jar".into(),
        "config/new.toml".into(),
        "saves/world.dat".into(),
        "config/cache/new.toml".into(),
      ],
      unchecked: vec![],
      suggested: vec![crate::instance::models::misc::SuggestedEntry { path: "saves".into(), kind: "dir".into() }],
    };

    let entries = classify(&root, &baseline, &release, &listed).await.unwrap();
    let by_path: HashMap<_, _> = entries.iter().map(|entry| (entry.path.as_str(), entry)).collect();
    assert_eq!(by_path["config/edited.toml"].class, ChangeClass::Modified);
    assert_eq!(by_path["config/once.toml"].class, ChangeClass::OnceChanged);
    assert_eq!(by_path["config/once.toml"].counted, false);
    assert_eq!(by_path["config/deleted.toml"].class, ChangeClass::Deleted);
    assert_eq!(by_path["mods/toggle.jar"].class, ChangeClass::Toggled);
    assert!(by_path["mods/toggle.jar"].counted);
    assert_eq!(by_path["mods/new.jar"].class, ChangeClass::Added);
    assert!(by_path["mods/new.jar"].counted);
    assert_eq!(by_path["config/new.toml"].class, ChangeClass::Added);
    assert!(!by_path["config/new.toml"].counted);
    assert_eq!(by_path["saves/world.dat"].class, ChangeClass::AddedExcluded);
    assert_eq!(by_path["config/cache/new.toml"].class, ChangeClass::AddedExcluded);
    assert!(!by_path.contains_key("config/edited.toml.disabled"));

    let edited_sha1 = baseline.files.iter().find(|file| file.path == "config/edited.toml").unwrap().sha1.clone();
    ignores::add(&root, &baseline, "config/edited.toml", &edited_sha1, None).unwrap();
    let entries = classify(&root, &baseline, &release, &listed).await.unwrap();
    assert!(!entries.iter().any(|entry| entry.path == "config/edited.toml"));
    let _ = fs::remove_dir_all(root);
  }
}
