use super::commands::{PreviousFile, PreviousRelease};
use crate::instance::modpack_update::remote::{RemoteRule, RuleKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};

const DISABLED_SUFFIX: &str = ".disabled";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskFile {
  pub path: String,
  pub disabled: bool,
  pub conflict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhostEntry {
  pub path: String,
  pub kind: RuleKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishDefaults {
  pub files: Vec<DiskFile>,
  pub ghosts: Vec<GhostEntry>,
  pub rules: Vec<RemoteRule>,
  pub included: Vec<String>,
  pub once: Vec<String>,
}

fn key(path: &str) -> String {
  path.to_lowercase()
}

pub fn normalize_disk_files(all: &[String]) -> Vec<DiskFile> {
  let mut files: HashMap<String, DiskFile> = HashMap::new();
  let mut order = Vec::new();
  for path in all {
    let folded = key(path);
    let disabled = folded.ends_with(DISABLED_SUFFIX);
    if disabled {
      let base = &path[..path.len() - DISABLED_SUFFIX.len()];
      if base.is_empty() || !key(base).starts_with("mods/") {
        continue;
      }
      let folded = key(base);
      if let Some(existing) = files.get_mut(&folded) {
        existing.conflict = true;
      } else {
        files.insert(
          folded.clone(),
          DiskFile {
            path: base.to_string(),
            disabled: true,
            conflict: false,
          },
        );
        order.push(folded);
      }
      continue;
    }
    if let Some(existing) = files.get_mut(&folded) {
      existing.conflict = true;
      existing.path = path.clone();
      existing.disabled = false;
    } else {
      files.insert(
        folded.clone(),
        DiskFile {
          path: path.clone(),
          disabled: false,
          conflict: false,
        },
      );
      order.push(folded);
    }
  }
  order.into_iter().filter_map(|folded| files.remove(&folded)).collect()
}

fn dir_keys(keys: &HashSet<String>) -> HashSet<String> {
  let mut dirs = HashSet::new();
  for path in keys {
    let mut cut = path.rfind('/');
    while let Some(index) = cut {
      if index == 0 || !dirs.insert(path[..index].to_string()) {
        break;
      }
      cut = path[..index].rfind('/');
    }
  }
  dirs
}

fn covering_rule<'a>(rules: &'a [RemoteRule], path: &str) -> Option<&'a RemoteRule> {
  let folded = key(path);
  rules
    .iter()
    .filter(|rule| rule.kind == RuleKind::Dir)
    .filter(|rule| folded.starts_with(&format!("{}/", key(&rule.path))))
    .max_by_key(|rule| key(&rule.path).len())
}

fn strip_ghosts(ghosts: Vec<GhostEntry>, disk_keys: &HashSet<String>, disk_dirs: &HashSet<String>) -> Vec<GhostEntry> {
  let mut taken = HashSet::new();
  let file_ghosts: HashSet<String> = ghosts
    .iter()
    .filter(|ghost| ghost.kind == RuleKind::File)
    .map(|ghost| key(&ghost.path))
    .collect();
  ghosts
    .into_iter()
    .filter(|ghost| {
      let folded = key(&ghost.path);
      if !taken.insert(folded.clone()) || disk_keys.contains(&folded) {
        return false;
      }
      if ghost.kind == RuleKind::File && disk_dirs.contains(&folded) {
        return false;
      }
      let mut current = folded.as_str();
      while let Some(index) = current.rfind('/') {
        let parent = &current[..index];
        if disk_keys.contains(parent) || file_ghosts.contains(parent) {
          return false;
        }
        current = parent;
      }
      true
    })
    .collect()
}

fn mentioned(entry: &SuggestedEntry, previous: &[PreviousFile], rules: &[RemoteRule]) -> bool {
  let wanted = key(&entry.path);
  let inside = |other: &str| other == wanted || other.starts_with(&format!("{wanted}/"));
  previous.iter().any(|file| inside(&key(&file.path)))
    || rules.iter().any(|rule| {
      let rule_key = key(&rule.path);
      inside(&rule_key)
        || (rule.kind == RuleKind::Dir && wanted.starts_with(&format!("{rule_key}/")))
    })
}

#[derive(Debug, Clone)]
pub struct SuggestedEntry {
  pub path: String,
  pub kind: RuleKind,
}

pub fn create_initial_model(
  all: &[String],
  previous: Option<&PreviousRelease>,
  suggested: &[SuggestedEntry],
) -> PublishDefaults {
  let files = normalize_disk_files(all);
  let disk_keys: HashSet<String> = files.iter().map(|file| key(&file.path)).collect();
  let disk_dirs = dir_keys(&disk_keys);
  let previous_files = previous.map(|release| release.files.as_slice()).unwrap_or(&[]);
  let mut rules = previous.map(|release| release.rules.clone()).unwrap_or_default();
  let mut ghosts = Vec::new();
  for file in previous_files {
    if !disk_keys.contains(&key(&file.path)) {
      ghosts.push(GhostEntry {
        path: file.path.clone(),
        kind: RuleKind::File,
      });
    }
  }
  for rule in &rules {
    let present = match rule.kind {
      RuleKind::File => disk_keys.contains(&key(&rule.path)),
      RuleKind::Dir => disk_dirs.contains(&key(&rule.path)),
    };
    if !present {
      ghosts.push(GhostEntry {
        path: rule.path.clone(),
        kind: rule.kind,
      });
    }
  }
  ghosts = strip_ghosts(ghosts, &disk_keys, &disk_dirs);
  for entry in suggested {
    if !mentioned(entry, previous_files, &rules) {
      rules.push(RemoteRule {
        path: entry.path.clone(),
        kind: entry.kind,
        state: crate::instance::modpack_update::remote::RuleState::Ignore,
      });
    }
  }
  let previous_by_key: HashMap<String, &PreviousFile> = previous_files
    .iter()
    .map(|file| (key(&file.path), file))
    .collect();
  let mut included = BTreeSet::new();
  let mut once = BTreeSet::new();
  for file in &files {
    let folded = key(&file.path);
    let Some(previous) = previous_by_key.get(&folded) else { continue };
    if previous.once {
      once.insert(folded.clone());
    }
    if !rules.iter().any(|rule| rule.kind == RuleKind::File && key(&rule.path) == folded)
      && covering_rule(&rules, &file.path).is_some()
    {
      included.insert(folded);
    }
  }
  PublishDefaults {
    files,
    ghosts,
    rules,
    included: included.into_iter().collect(),
    once: once.into_iter().collect(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::modpack_update::remote::RuleState;

  fn file(path: &str, once: bool) -> PreviousFile {
    PreviousFile {
      path: path.into(),
      sha1: String::new(),
      size: 0,
      disabled: false,
      once,
    }
  }

  fn rule(path: &str, kind: RuleKind, state: RuleState) -> RemoteRule {
    RemoteRule { path: path.into(), kind, state }
  }

  fn previous(files: Vec<PreviousFile>, rules: Vec<RemoteRule>) -> PreviousRelease {
    PreviousRelease { files, rules, rules_manifest: true }
  }

  fn model(all: &[&str], previous: Option<PreviousRelease>, suggested: &[(&str, RuleKind)]) -> PublishDefaults {
    create_initial_model(
      &all.iter().map(|path| (*path).to_string()).collect::<Vec<_>>(),
      previous.as_ref(),
      &suggested
        .iter()
        .map(|(path, kind)| SuggestedEntry { path: (*path).into(), kind: *kind })
        .collect::<Vec<_>>(),
    )
  }

  #[test]
  fn first_release_every_file_is_included() {
    let result = model(&["config/a.toml", "mods/x.jar", "options.txt"], None, &[]);
    assert_eq!(result.files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), [
      "config/a.toml", "mods/x.jar", "options.txt"
    ]);
    assert!(result.rules.is_empty());
    assert!(result.included.is_empty());
    assert!(result.once.is_empty());
    assert!(result.ghosts.is_empty());
  }

  #[test]
  fn suggested_entries_become_one_rule_at_their_own_level() {
    let result = model(
      &["saves/w1/level.dat", "saves/w1/region/r.0.mca", "saves/w2/level.dat", "mods/x.jar"],
      None,
      &[("saves", RuleKind::Dir)],
    );
    assert_eq!(result.rules, vec![rule("saves", RuleKind::Dir, RuleState::Ignore)]);

    let result = model(
      &["options.txt", "mods/VoxelMods/a.json", "mods/VoxelMods/b.json", "mods/x.jar"],
      None,
      &[("options.txt", RuleKind::File), ("mods/VoxelMods", RuleKind::Dir)],
    );
    assert_eq!(result.rules.len(), 2);
    assert!(result.rules.contains(&rule("options.txt", RuleKind::File, RuleState::Ignore)));
    assert!(result.rules.contains(&rule("mods/VoxelMods", RuleKind::Dir, RuleState::Ignore)));
  }

  #[test]
  fn previous_files_keep_once_and_new_files_are_included() {
    let result = model(
      &["config/a.toml", "config/b.toml", "config/new.toml"],
      Some(previous(vec![file("config/a.toml", true), file("config/b.toml", false)], vec![])),
      &[],
    );
    assert_eq!(result.once, vec!["config/a.toml"]);
    assert!(result.included.is_empty());
    assert!(result.files.iter().all(|file| !file.conflict));
  }

  #[test]
  fn previous_rules_are_kept_and_new_files_follow_them() {
    let rules = vec![
      rule("gone/dir", RuleKind::Dir, RuleState::Delete),
      rule("resourcepacks/old.zip", RuleKind::File, RuleState::Delete),
      rule("config/cache", RuleKind::Dir, RuleState::Ignore),
    ];
    let result = model(
      &["resourcepacks/old.zip", "config/cache/x.bin", "config/a.toml"],
      Some(previous(vec![file("config/a.toml", false)], rules.clone())),
      &[],
    );
    assert_eq!(result.rules, rules);
    assert_eq!(result.ghosts, vec![GhostEntry { path: "gone/dir".into(), kind: RuleKind::Dir }]);

    let result = model(
      &["config/cache/new.bin"],
      Some(previous(vec![], vec![rule("config/cache", RuleKind::Dir, RuleState::Delete)])),
      &[],
    );
    assert!(result.included.is_empty());
  }

  #[test]
  fn listed_files_override_rules_and_new_siblings_inherit_rules() {
    let result = model(
      &["saves/keep.dat", "saves/other.dat"],
      Some(previous(
        vec![file("saves/keep.dat", false)],
        vec![rule("saves", RuleKind::Dir, RuleState::Ignore)],
      )),
      &[],
    );
    assert_eq!(result.included, vec!["saves/keep.dat"]);
  }

  #[test]
  fn suggestions_are_skipped_when_previous_release_mentions_them() {
    let result = model(
      &["config/a.toml", "saves/w1/level.dat"],
      Some(previous(vec![file("config/a.toml", false)], vec![rule("config/cache", RuleKind::Dir, RuleState::Ignore)])),
      &[("saves", RuleKind::Dir)],
    );
    assert!(result.rules.contains(&rule("config/cache", RuleKind::Dir, RuleState::Ignore)));
    assert!(result.rules.contains(&rule("saves", RuleKind::Dir, RuleState::Ignore)));
    assert_eq!(result.rules.len(), 2);

    let result = model(
      &["saves/w1/level.dat", "saves/w2/level.dat"],
      Some(previous(vec![file("saves/w1/level.dat", false)], vec![])),
      &[("saves", RuleKind::Dir)],
    );
    assert!(result.rules.is_empty());

    let result = model(
      &["saves/w1/level.dat"],
      Some(previous(vec![], vec![rule("saves/w1", RuleKind::Dir, RuleState::Delete)])),
      &[("saves", RuleKind::Dir)],
    );
    assert_eq!(result.rules.len(), 1);
  }

  #[test]
  fn suggestion_matching_is_case_insensitive_and_does_not_use_name_prefixes() {
    let result = model(
      &["saves/w1/level.dat", "saves2/a.dat"],
      Some(previous(vec![file("saves2/a.dat", false)], vec![])),
      &[("saves", RuleKind::Dir)],
    );
    assert_eq!(result.rules, vec![rule("saves", RuleKind::Dir, RuleState::Ignore)]);

    let result = model(
      &["saves/w1/level.dat"],
      Some(previous(vec![file("Saves/W1/Level.dat", false)], vec![])),
      &[("saves", RuleKind::Dir)],
    );
    assert!(result.rules.is_empty());
  }

  #[test]
  fn virtual_files_and_missing_rules_are_ghosts() {
    let result = model(
      &["a/keep.txt"],
      Some(previous(vec![file("a/keep.txt", false), file("a/old.txt", true)], vec![])),
      &[],
    );
    assert_eq!(result.ghosts, vec![GhostEntry { path: "a/old.txt".into(), kind: RuleKind::File }]);

    let result = model(
      &["config/a.toml"],
      Some(previous(
        vec![],
        vec![rule("gone.txt", RuleKind::File, RuleState::Ignore), rule("gonedir", RuleKind::Dir, RuleState::Delete)],
      )),
      &[],
    );
    assert_eq!(result.ghosts.len(), 2);
  }

  #[test]
  fn normalize_disk_files_handles_disabled_mods_and_conflicts() {
    let result = normalize_disk_files(&[
      "mods/a.jar.disabled".into(),
      "mods/b.jar".into(),
      "mods/a.jar".into(),
      "config/x.disabled".into(),
      "Mods/C.JAR.DISABLED".into(),
    ]);
    assert_eq!(result.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), [
      "mods/a.jar", "mods/b.jar", "Mods/C.JAR"
    ]);
    assert!(result[0].conflict);
    assert!(result[0].disabled == false);
    assert!(result[2].disabled);
  }

  #[test]
  fn previous_release_matches_disabled_and_once_case_insensitively() {
    let result = model(
      &["mods/a.jar.disabled", "config/a.toml"],
      Some(previous(vec![file("MODS/A.JAR", true), file("Config/A.toml", true)], vec![])),
      &[],
    );
    assert!(result.ghosts.is_empty());
    assert_eq!(result.once, vec!["config/a.toml", "mods/a.jar"]);
  }
}
