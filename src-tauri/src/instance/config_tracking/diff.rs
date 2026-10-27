use super::model::{
  ChangeSummary, Checkpoint, ConfigCommit, ConfigTree, Semantic, SemanticContext,
};
use crate::instance::helpers::modpack::text::normalize_text;
use crate::instance::modpack_update::text_diff::{lines_of, unified_diff, DiffHunk};
use std::collections::BTreeSet;

pub fn diff_trees(old: Option<&ConfigTree>, new: &ConfigTree) -> ChangeSummary {
  let old_files = old.map(ConfigTree::files_by_path).unwrap_or_default();
  let new_files = new.files_by_path();
  let mut paths = BTreeSet::new();
  paths.extend(old_files.keys().copied());
  paths.extend(new_files.keys().copied());
  let mut result = ChangeSummary::default();
  for path in paths {
    match (old_files.get(path), new_files.get(path)) {
      (None, Some(file)) => {
        if file.read_state != super::model::ReadState::Stable {
          result.unstable.push(path.to_string());
        } else {
          result.added.push(path.to_string());
        }
      }
      (Some(_), None) => {
        if new.incomplete {
          result.unstable.push(path.to_string());
        } else {
          result.deleted.push(path.to_string());
        }
      }
      (Some(left), Some(right)) => {
        if right.read_state != super::model::ReadState::Stable
          || left.read_state != super::model::ReadState::Stable
        {
          result.unstable.push(path.to_string());
        } else if left.sha1 != right.sha1 {
          result.modified.push(path.to_string());
        }
      }
      (None, None) => unreachable!(),
    }
  }
  result
}

pub fn derive_semantic(
  previous: Option<&ConfigCommit>,
  checkpoint: Checkpoint,
  context: SemanticContext,
) -> Semantic {
  if context.overlapping_sessions || context.scan_incomplete || checkpoint == Checkpoint::Ambiguous
  {
    return Semantic::Ambiguous;
  }
  let Some(previous) = previous else {
    return Semantic::InitialObservation;
  };
  match (previous.checkpoint, checkpoint) {
    (Checkpoint::BeforeLaunch, Checkpoint::LauncherReady) => Semantic::DuringLoading,
    (Checkpoint::LauncherReady, Checkpoint::AfterExit) => Semantic::DuringRuntime,
    (Checkpoint::AfterExit, Checkpoint::BeforeLaunch | Checkpoint::ManualScan) => {
      Semantic::BetweenSessions
    }
    (_, Checkpoint::Recovered) => Semantic::InterruptedOrUnknown,
    (Checkpoint::BeforeLaunch, Checkpoint::AfterExit)
    | (Checkpoint::BeforeLaunch, Checkpoint::ManualScan)
    | (Checkpoint::LauncherReady, Checkpoint::ManualScan)
    | (Checkpoint::LauncherReady, Checkpoint::BeforeLaunch)
    | (_, Checkpoint::AfterExit) => Semantic::InterruptedOrUnknown,
    (_, Checkpoint::FirstObserved) => Semantic::InitialObservation,
    _ => Semantic::InterruptedOrUnknown,
  }
}

pub fn normalized_text_equal(path: &str, old: &[u8], new: &[u8]) -> Option<bool> {
  Some(normalize_text(path, old)? == normalize_text(path, new)?)
}

pub fn normalized_text_diff(path: &str, old: &[u8], new: &[u8]) -> Option<Vec<DiffHunk>> {
  let old = normalize_text(path, old)?;
  let new = normalize_text(path, new)?;
  Some(unified_diff(&lines_of(&old), &lines_of(&new), 3))
}
