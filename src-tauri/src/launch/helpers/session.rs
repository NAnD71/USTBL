use crate::error::USTBLResult;
use crate::launch::models::{LaunchError, LaunchingState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LAST_LAUNCHING_ID: AtomicU64 = AtomicU64::new(0);

/// Returns the next launching id: the current Unix time in milliseconds, bumped past the
/// previously issued id when the clock has not advanced (or went backwards).
///
/// The id names the game log file, so it must not repeat the small counters of earlier runs;
/// a millisecond clock keeps it clear of the second-based ids used by older versions as well.
pub fn next_launching_id() -> u64 {
  let now_ms = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|elapsed| elapsed.as_millis() as u64)
    .unwrap_or(0);
  next_launching_id_from(&LAST_LAUNCHING_ID, now_ms)
}

fn next_launching_id_from(last: &AtomicU64, now_ms: u64) -> u64 {
  let mut current = last.load(Ordering::SeqCst);
  loop {
    let next = now_ms.max(current.saturating_add(1));
    match last.compare_exchange_weak(current, next, Ordering::SeqCst, Ordering::SeqCst) {
      Ok(_) => return next,
      Err(actual) => current = actual,
    }
  }
}

/// Finds the session a step command belongs to. A missing id is an error, never a fallback to
/// another session: several games (or several launches of one instance) may run at once.
pub fn session_mut(
  queue: &mut [LaunchingState],
  launching_id: u64,
) -> Result<&mut LaunchingState, LaunchError> {
  queue
    .iter_mut()
    .find(|launching| launching.id == launching_id)
    .ok_or(LaunchError::LaunchingStateNotFound)
}

/// Drops a session that will not reach a running game. Never fails: a poisoned queue is
/// left alone because there is nothing more useful to do with it here.
pub fn remove_session(queue: &Mutex<Vec<LaunchingState>>, launching_id: u64) {
  if let Ok(mut queue) = queue.lock() {
    queue.retain(|launching| launching.id != launching_id);
  }
}

/// Records the spawned process on its session. Fails when the session is already gone
/// (cancelled while the process was being spawned), so the caller can stop the process.
pub fn register_game_process(
  queue: &mut [LaunchingState],
  launching_id: u64,
  pid: u32,
  full_command: String,
) -> Result<(), LaunchError> {
  let launching = session_mut(queue, launching_id)?;
  launching.pid = pid;
  launching.full_command = full_command;
  Ok(())
}

/// Cancels exactly one session. Unknown ids do nothing, so a cancel for a launch that never
/// got an id (or already finished) cannot touch another running game.
///
/// A session with a running process is marked cancelled (step 0) and its process killed; the
/// process monitor removes it once the process exits. A session without a process is removed.
pub fn cancel_session(
  queue: &mut Vec<LaunchingState>,
  launching_id: u64,
  kill: impl FnOnce(u32) -> USTBLResult<()>,
) -> USTBLResult<()> {
  let Some(index) = queue
    .iter()
    .position(|launching| launching.id == launching_id)
  else {
    return Ok(());
  };
  if queue[index].pid == 0 {
    queue.remove(index);
    return Ok(());
  }
  // Mark as manually cancelled to avoid the game error window popping up.
  queue[index].current_step = 0;
  kill(queue[index].pid)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::HashSet;
  use std::sync::Arc;

  fn session(id: u64, pid: u32) -> LaunchingState {
    LaunchingState {
      id,
      pid,
      ..LaunchingState::default()
    }
  }

  #[test]
  fn launching_ids_increase_while_the_clock_stands_still_or_goes_back() {
    let last = AtomicU64::new(0);
    assert_eq!(next_launching_id_from(&last, 1_000), 1_000);
    assert_eq!(next_launching_id_from(&last, 1_000), 1_001);
    assert_eq!(next_launching_id_from(&last, 900), 1_002);
    assert_eq!(next_launching_id_from(&last, 5_000), 5_000);
  }

  #[test]
  fn launching_ids_are_unique_across_threads() {
    let last = Arc::new(AtomicU64::new(0));
    let handles: Vec<_> = (0..8)
      .map(|_| {
        let last = last.clone();
        std::thread::spawn(move || {
          (0..500)
            .map(|_| next_launching_id_from(&last, 42))
            .collect::<Vec<_>>()
        })
      })
      .collect();
    let ids: Vec<u64> = handles
      .into_iter()
      .flat_map(|handle| handle.join().unwrap())
      .collect();
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len());
  }

  #[test]
  fn next_launching_id_is_a_millisecond_timestamp() {
    // Older versions used seconds; a log file left by any earlier run must not collide.
    assert!(next_launching_id() > 1_000_000_000_000);
  }

  #[test]
  fn step_commands_never_fall_back_to_another_session() {
    let mut queue = vec![session(1, 100), session(2, 0)];
    assert_eq!(session_mut(&mut queue, 2).unwrap().id, 2);
    assert!(matches!(
      session_mut(&mut queue, 3),
      Err(LaunchError::LaunchingStateNotFound)
    ));
  }

  #[test]
  fn cancelling_an_unknown_id_kills_nothing() {
    // Step 1 failed before a session existed, the user clicked cancel, and game A runs.
    let mut queue = vec![session(1, 4242)];
    let mut killed = Vec::new();
    cancel_session(&mut queue, 99, |pid| {
      killed.push(pid);
      Ok(())
    })
    .unwrap();
    assert!(killed.is_empty());
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].current_step, 1);
  }

  #[test]
  fn cancelling_a_running_session_kills_only_its_process() {
    let mut queue = vec![session(1, 4242), session(2, 777)];
    let mut killed = Vec::new();
    cancel_session(&mut queue, 2, |pid| {
      killed.push(pid);
      Ok(())
    })
    .unwrap();
    assert_eq!(killed, vec![777]);
    // The monitor removes it after the exit; the mark suppresses the error window.
    assert_eq!(queue[1].current_step, 0);
    assert_eq!(queue[0].current_step, 1);
  }

  #[test]
  fn cancelling_a_session_without_process_removes_it() {
    let mut queue = vec![session(1, 4242), session(2, 0)];
    cancel_session(&mut queue, 2, |_| panic!("no process to kill")).unwrap();
    assert_eq!(queue.iter().map(|s| s.id).collect::<Vec<_>>(), vec![1]);
  }

  #[test]
  fn a_process_spawned_after_cancel_cannot_be_registered() {
    let mut queue = vec![session(1, 4242)];
    assert!(register_game_process(&mut queue, 2, 9, "cmd".into()).is_err());
    assert_eq!(queue[0].pid, 4242);
    register_game_process(&mut queue, 1, 9, "cmd".into()).unwrap();
    assert_eq!((queue[0].pid, queue[0].full_command.as_str()), (9, "cmd"));
  }

  #[test]
  fn failed_sessions_leave_the_queue_and_others_stay() {
    let queue = Mutex::new(vec![session(1, 4242), session(2, 0)]);
    remove_session(&queue, 2);
    remove_session(&queue, 2);
    assert_eq!(
      queue
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.id)
        .collect::<Vec<_>>(),
      vec![1]
    );
  }
}
