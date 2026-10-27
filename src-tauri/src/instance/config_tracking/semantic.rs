use crate::instance::config_tracking::model::{
  ChangeSet, FileDiffBody, LogEntry, ReadState, Semantic, Tree,
};
use crate::instance::helpers::modpack::text::normalize_text;
use crate::instance::modpack_update::text_diff::{lines_of, unified_diff_hiding, CONTEXT_LINES};
use std::collections::{BTreeMap, HashMap};

/// Derive time-boundary semantics without persisting session or overlap state.
///
/// Compared with the superseded design, incomplete scans do not force an
/// ambiguous semantic, and overlap is evaluated only for each adjacent log
/// interval. Once one session no longer covers an interval, another session's
/// own evidence can be used normally.
pub fn semantics(entries: &[LogEntry]) -> Vec<Semantic> {
  #[derive(Default)]
  struct Session {
    first: usize,
    last: usize,
    ready: Option<usize>,
    has_after: bool,
  }
  let mut sessions: HashMap<String, Session> = HashMap::new();
  for (index, entry) in entries.iter().enumerate() {
    if let Some(session_id) = lifecycle_session(entry) {
      let session = sessions.entry(session_id.to_string()).or_insert(Session {
        first: index,
        last: index,
        ..Session::default()
      });
      session.last = index;
      if matches!(
        entry.checkpoint,
        crate::instance::config_tracking::Checkpoint::LauncherReady
      ) && session.ready.is_none()
      {
        session.ready = Some(index);
      }
      if matches!(
        entry.checkpoint,
        crate::instance::config_tracking::Checkpoint::AfterExit
      ) {
        session.has_after = true;
      }
    }
  }

  entries
    .iter()
    .enumerate()
    .map(|(index, entry)| {
      if index == 0 {
        return Semantic::InitialObservation;
      }
      let covered: Vec<&Session> = sessions
        .values()
        .filter(|session| session.first <= index - 1 && index <= session.last)
        .collect();
      if covered.len() >= 2 {
        return Semantic::Ambiguous;
      }
      if let Some(session) = covered.first() {
        return match session.ready {
          Some(ready) if index <= ready => Semantic::DuringLoading,
          Some(ready) if index - 1 >= ready => Semantic::DuringRuntime,
          Some(_) => Semantic::InterruptedOrUnknown,
          None => Semantic::InterruptedOrUnknown,
        };
      }
      if let Some(session_id) = lifecycle_session(entry) {
        if sessions
          .get(session_id)
          .is_some_and(|session| session.first == index)
          && !matches!(
            entry.checkpoint,
            crate::instance::config_tracking::Checkpoint::BeforeLaunch
          )
        {
          return Semantic::InterruptedOrUnknown;
        }
      }
      if let Some(session_id) = lifecycle_session(&entries[index - 1]) {
        if sessions
          .get(session_id)
          .is_some_and(|session| session.last == index - 1 && !session.has_after)
        {
          return Semantic::InterruptedOrUnknown;
        }
      }
      Semantic::BetweenSessions
    })
    .collect()
}

fn lifecycle_session(entry: &LogEntry) -> Option<&str> {
  match entry.checkpoint {
    crate::instance::config_tracking::Checkpoint::BeforeLaunch
    | crate::instance::config_tracking::Checkpoint::LauncherReady
    | crate::instance::config_tracking::Checkpoint::AfterExit => entry.session_id.as_deref(),
    crate::instance::config_tracking::Checkpoint::ManualScan => None,
  }
}

pub fn diff_trees(old: Option<&Tree>, new: &Tree, new_has_errors: bool) -> ChangeSet {
  let old_map = old.map(file_map).unwrap_or_default();
  let new_map = file_map(new);
  let mut result = ChangeSet::default();
  let mut paths = old_map
    .keys()
    .chain(new_map.keys())
    .cloned()
    .collect::<Vec<_>>();
  paths.sort();
  paths.dedup();
  for path in paths {
    match (old_map.get(&path), new_map.get(&path)) {
      (None, Some(new_file)) => {
        if new_file.state == ReadState::Stable {
          result.added.push(path);
        } else {
          result.uncertain.push(path);
        }
      }
      (Some(_), None) => {
        if new_has_errors {
          result.uncertain.push(path);
        } else {
          result.deleted.push(path);
        }
      }
      (Some(old_file), Some(new_file)) => {
        if old_file.state != ReadState::Stable || new_file.state != ReadState::Stable {
          result.uncertain.push(path);
        } else if old_file.sha1 != new_file.sha1 {
          result.modified.push(path);
        }
      }
      (None, None) => unreachable!(),
    }
  }
  result
}

fn file_map(tree: &Tree) -> BTreeMap<String, &crate::instance::config_tracking::FileEntry> {
  tree
    .files
    .iter()
    .map(|file| (file.path.clone(), file))
    .collect()
}

pub fn file_diff(path: &str, old: Option<&[u8]>, new: Option<&[u8]>) -> FileDiffBody {
  let old_bytes = old.unwrap_or_default();
  let new_bytes = new.unwrap_or_default();
  let (Some(old_text), Some(new_text)) = (
    normalize_text(path, old_bytes),
    normalize_text(path, new_bytes),
  ) else {
    return FileDiffBody::Binary;
  };
  let normalized_equal = old_text == new_text;
  let hunks = if normalized_equal {
    Vec::new()
  } else {
    unified_diff_hiding(
      &lines_of(&old_text),
      &lines_of(&new_text),
      CONTEXT_LINES,
      &|_| false,
    )
  };
  FileDiffBody::Text {
    hunks,
    normalized_equal,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::config_tracking::{Checkpoint, FileEntry};

  fn entry(checkpoint: Checkpoint, session_id: Option<&str>) -> LogEntry {
    LogEntry {
      v: 1,
      at: String::new(),
      checkpoint,
      session_id: session_id.map(str::to_string),
      instance_id: None,
      exit: None,
      config_present: true,
      tree: String::new(),
      errors: Vec::new(),
    }
  }

  #[test]
  fn semantics_table_covers_boundaries_and_overlap() {
    use Checkpoint::*;
    use Semantic::*;
    let cases = vec![
      (vec![entry(ManualScan, None)], vec![InitialObservation]),
      (
        vec![
          entry(BeforeLaunch, Some("S")),
          entry(LauncherReady, Some("S")),
          entry(AfterExit, Some("S")),
        ],
        vec![InitialObservation, DuringLoading, DuringRuntime],
      ),
      (
        vec![
          entry(BeforeLaunch, Some("S")),
          entry(AfterExit, Some("S")),
          entry(BeforeLaunch, Some("T")),
        ],
        vec![InitialObservation, InterruptedOrUnknown, BetweenSessions],
      ),
      (
        vec![
          entry(BeforeLaunch, Some("S")),
          entry(BeforeLaunch, Some("T")),
        ],
        vec![InitialObservation, InterruptedOrUnknown],
      ),
      (
        vec![
          entry(BeforeLaunch, Some("S")),
          entry(BeforeLaunch, Some("T")),
          entry(LauncherReady, Some("S")),
          entry(LauncherReady, Some("T")),
          entry(AfterExit, Some("S")),
          entry(AfterExit, Some("T")),
        ],
        vec![
          InitialObservation,
          DuringLoading,
          Ambiguous,
          Ambiguous,
          Ambiguous,
          DuringRuntime,
        ],
      ),
      (
        vec![
          entry(ManualScan, None),
          entry(LauncherReady, Some("S")),
          entry(AfterExit, Some("S")),
        ],
        vec![InitialObservation, InterruptedOrUnknown, DuringRuntime],
      ),
      (
        vec![
          entry(BeforeLaunch, Some("S")),
          entry(ManualScan, Some("S")),
          entry(LauncherReady, Some("S")),
          entry(ManualScan, None),
          entry(AfterExit, Some("S")),
        ],
        vec![
          InitialObservation,
          DuringLoading,
          DuringLoading,
          DuringRuntime,
          DuringRuntime,
        ],
      ),
      (
        vec![
          entry(BeforeLaunch, Some("S")),
          entry(LauncherReady, Some("S")),
          entry(AfterExit, Some("S")),
          entry(ManualScan, None),
          entry(BeforeLaunch, Some("T")),
        ],
        vec![
          InitialObservation,
          DuringLoading,
          DuringRuntime,
          BetweenSessions,
          BetweenSessions,
        ],
      ),
    ];
    for (input, expected) in cases {
      assert_eq!(semantics(&input), expected);
    }
    let with_manual_id = vec![
      entry(ManualScan, Some("S")),
      entry(LauncherReady, Some("S")),
      entry(AfterExit, Some("S")),
    ];
    assert_eq!(
      semantics(&with_manual_id),
      vec![InitialObservation, InterruptedOrUnknown, DuringRuntime]
    );
  }

  fn file(path: &str, sha1: Option<&str>, state: ReadState) -> FileEntry {
    FileEntry {
      path: path.into(),
      sha1: sha1.map(str::to_string),
      size: 1,
      state,
    }
  }

  #[test]
  fn tree_diff_reports_all_kinds_and_mtime_is_not_present() {
    let old = Tree {
      files: vec![
        file("deleted", Some("a"), ReadState::Stable),
        file("same", Some("a"), ReadState::Stable),
        file("changed", Some("a"), ReadState::Stable),
      ],
    };
    let new = Tree {
      files: vec![
        file("added", Some("b"), ReadState::Stable),
        file("same", Some("a"), ReadState::Stable),
        file("changed", Some("b"), ReadState::Stable),
        file("uncertain", None, ReadState::Unstable),
      ],
    };
    let changes = diff_trees(Some(&old), &new, false);
    assert_eq!(changes.added, ["added"]);
    assert_eq!(changes.modified, ["changed"]);
    assert_eq!(changes.deleted, ["deleted"]);
    assert_eq!(changes.uncertain, ["uncertain"]);
    let same = Tree {
      files: vec![file("same", Some("a"), ReadState::Stable)],
    };
    let unchanged = diff_trees(Some(&same), &same, false);
    assert!(unchanged.added.is_empty());
    assert!(unchanged.modified.is_empty());
    assert!(unchanged.deleted.is_empty());
    assert!(unchanged.uncertain.is_empty());
  }

  #[test]
  fn file_diff_normalizes_line_endings_and_detects_binary() {
    match file_diff("a.txt", Some(b"a\r\nb\r\n"), Some(b"a\nb\n")) {
      FileDiffBody::Text {
        hunks,
        normalized_equal,
      } => assert!(hunks.is_empty() && normalized_equal),
      _ => panic!("expected text"),
    }
    match file_diff("a.txt", Some(b"a\n"), Some(b"b\n")) {
      FileDiffBody::Text {
        hunks,
        normalized_equal,
      } => assert!(!hunks.is_empty() && !normalized_equal),
      _ => panic!("expected text"),
    }
    assert!(matches!(
      file_diff("a.bin", Some(&[0]), Some(&[1])),
      FileDiffBody::Binary
    ));
  }
}
