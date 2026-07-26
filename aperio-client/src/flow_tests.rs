use super::*;

#[tokio::test]
async fn wait_returns_immediately_when_not_paused() {
  let reg = PauseRegistry::default();
  let guard = reg.register("s1");
  tokio::time::timeout(
    Duration::from_millis(50),
    guard.signal().wait_while_paused(),
  )
  .await
  .expect("an unpaused stream must not wait");
}

#[tokio::test]
async fn pause_blocks_until_resume() {
  let reg = PauseRegistry::default();
  let guard = reg.register("s1");
  reg.pause("s1");

  // Paused: the wait must not return yet.
  assert!(
    tokio::time::timeout(
      Duration::from_millis(50),
      guard.signal().wait_while_paused()
    )
    .await
    .is_err(),
    "a paused stream must wait"
  );

  // Resume from another task while a waiter is parked.
  let signal = guard.signal();
  let waiter = signal.wait_while_paused();
  tokio::pin!(waiter);
  reg.resume("s1");
  tokio::time::timeout(Duration::from_millis(200), waiter)
    .await
    .expect("resume must release the waiter");
}

#[tokio::test]
async fn resume_landing_before_the_wait_is_not_lost() {
  let reg = PauseRegistry::default();
  let guard = reg.register("s1");
  reg.pause("s1");
  reg.resume("s1");
  // The flag flipped back before anyone waited: no notification is pending,
  // and the wait must still return promptly.
  tokio::time::timeout(
    Duration::from_millis(50),
    guard.signal().wait_while_paused(),
  )
  .await
  .expect("a resumed stream must not wait");
}

#[tokio::test]
async fn unknown_ids_are_noops_and_guards_unregister() {
  let reg = PauseRegistry::default();
  // Nothing registered: both directions are no-ops.
  reg.pause("ghost");
  reg.resume("ghost");

  let guard = reg.register("s1");
  drop(guard);
  // The guard removed the entry, so a late pause cannot flip anything (and
  // must not panic).
  reg.pause("s1");
}
