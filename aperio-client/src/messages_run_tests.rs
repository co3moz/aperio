//! Tests for the command a subscription can run.
//!
//! The properties under test are the ones that make a remote-execution
//! primitive safe to offer: the payload cannot reach the command line, the
//! cap holds, and a hung command is killed.

use super::*;
use crate::pubsub::MessageBus;
use std::time::Instant;

fn tempdir(name: &str) -> std::path::PathBuf {
  let dir = std::env::temp_dir().join(format!("aperio-run-{name}-{}", std::process::id()));
  let _ = std::fs::create_dir_all(&dir);
  dir
}

fn delivery(topic: &str, payload: &[u8]) -> Delivery {
  Delivery {
    topic: topic.to_string(),
    payload: payload.to_vec(),
    id: Some("m-1".to_string()),
  }
}

/// Waits for `path` to exist, up to two seconds.
async fn wait_for(path: &std::path::Path) -> bool {
  for _ in 0..40 {
    if path.exists() {
      return true;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  false
}

#[tokio::test]
async fn a_message_runs_the_command_with_its_payload_on_stdin() {
  let dir = tempdir("stdin");
  let out = dir.join("stdin.txt");
  let _ = std::fs::remove_file(&out);
  let bus = MessageBus::new(vec!["deploy/#".to_string()]);
  spawn(
    bus.clone(),
    vec![Runner::new(
      "deploy/#".to_string(),
      format!("cat > {}", out.display()),
      Some(5),
      Some(1),
    )],
  );
  tokio::time::sleep(Duration::from_millis(100)).await;

  bus.deliver(delivery("deploy/web", b"v1.9.2"));
  assert!(wait_for(&out).await, "the command did not run");
  tokio::time::sleep(Duration::from_millis(200)).await;
  assert_eq!(std::fs::read_to_string(&out).unwrap(), "v1.9.2");
}

#[tokio::test]
async fn the_topic_and_id_reach_the_command_through_the_environment() {
  let dir = tempdir("env");
  let out = dir.join("env.txt");
  let _ = std::fs::remove_file(&out);
  let bus = MessageBus::new(vec!["deploy/#".to_string()]);
  spawn(
    bus.clone(),
    vec![Runner::new(
      "deploy/#".to_string(),
      format!(
        "printf '%s %s' \"$APERIO_MESSAGE_TOPIC\" \"$APERIO_MESSAGE_ID\" > {}",
        out.display()
      ),
      Some(5),
      Some(1),
    )],
  );
  tokio::time::sleep(Duration::from_millis(100)).await;

  bus.deliver(delivery("deploy/web", b""));
  assert!(wait_for(&out).await, "the command did not run");
  tokio::time::sleep(Duration::from_millis(200)).await;
  assert_eq!(std::fs::read_to_string(&out).unwrap(), "deploy/web m-1");
}

#[tokio::test]
async fn a_payload_can_never_become_part_of_the_command() {
  // The property the whole design rests on. A payload built to break out of a
  // shell command reaches the process as data and nothing else, because it
  // never goes near the command line however it is written.
  let dir = tempdir("injection");
  let marker = dir.join("PWNED");
  let out = dir.join("injection.txt");
  let _ = std::fs::remove_file(&marker);
  let _ = std::fs::remove_file(&out);

  let bus = MessageBus::new(vec!["deploy/#".to_string()]);
  spawn(
    bus.clone(),
    vec![Runner::new(
      "deploy/#".to_string(),
      format!("cat > {}", out.display()),
      Some(5),
      Some(1),
    )],
  );
  tokio::time::sleep(Duration::from_millis(100)).await;

  let hostile = format!("'; touch {} ; echo '", marker.display());
  bus.deliver(delivery("deploy/web", hostile.as_bytes()));
  assert!(wait_for(&out).await, "the command did not run");
  tokio::time::sleep(Duration::from_millis(300)).await;

  assert!(
    !marker.exists(),
    "the payload was interpreted by the shell: {}",
    marker.display()
  );
  assert_eq!(
    std::fs::read_to_string(&out).unwrap(),
    hostile,
    "it should arrive as data, byte for byte"
  );
}

#[tokio::test]
async fn a_flood_cannot_start_more_runs_than_the_cap() {
  // A publisher in a loop must not fork a process per message. Over the cap
  // the message is dropped rather than queued: a queue for a command that
  // cannot keep up is the same problem with the memory growing instead.
  let dir = tempdir("cap");
  let counter = dir.join("cap-runs");
  let _ = std::fs::remove_dir_all(&counter);
  std::fs::create_dir_all(&counter).unwrap();

  let bus = MessageBus::new(vec!["flood/#".to_string()]);
  spawn(
    bus.clone(),
    vec![Runner::new(
      "flood/#".to_string(),
      // Each run leaves a uniquely named file and lingers, so the cap is
      // what decides how many exist.
      format!("touch {}/$$ && sleep 1", counter.display()),
      Some(5),
      Some(2),
    )],
  );
  tokio::time::sleep(Duration::from_millis(100)).await;

  for _ in 0..20 {
    bus.deliver(delivery("flood/now", b""));
  }
  tokio::time::sleep(Duration::from_millis(500)).await;
  let started = std::fs::read_dir(&counter).unwrap().count();
  assert!(
    started <= 2,
    "the cap is 2 and {started} run(s) were started"
  );
  assert!(started >= 1, "at least one should have run");
}

#[tokio::test]
async fn a_command_that_hangs_is_killed_rather_than_holding_the_slot() {
  let bus = MessageBus::new(vec!["slow/#".to_string()]);
  let started = Instant::now();
  let outcome = run_once("sleep 30", "slow/one", "m-1", b"", Duration::from_secs(1)).await;
  assert!(
    matches!(outcome, Ok(None)),
    "should report being killed, got {outcome:?}"
  );
  assert!(
    started.elapsed() < Duration::from_secs(5),
    "it should have been killed at the timeout, not waited out"
  );
  drop(bus);
}

#[tokio::test]
async fn a_message_on_another_topic_runs_nothing() {
  let dir = tempdir("filter");
  let out = dir.join("filter.txt");
  let _ = std::fs::remove_file(&out);
  let bus = MessageBus::new(vec!["deploy/#".to_string()]);
  spawn(
    bus.clone(),
    vec![Runner::new(
      "deploy/#".to_string(),
      format!("touch {}", out.display()),
      Some(5),
      Some(1),
    )],
  );
  tokio::time::sleep(Duration::from_millis(100)).await;

  bus.deliver(delivery("metrics/cpu", b""));
  tokio::time::sleep(Duration::from_millis(400)).await;
  assert!(!out.exists(), "a non-matching topic started a run");
}

#[tokio::test]
async fn a_failing_command_does_not_stop_the_next_message() {
  let dir = tempdir("failure");
  let out = dir.join("second.txt");
  let _ = std::fs::remove_file(&out);
  let bus = MessageBus::new(vec!["deploy/#".to_string()]);
  spawn(
    bus.clone(),
    vec![Runner::new(
      "deploy/#".to_string(),
      format!(
        "test -f {} && exit 0 || (touch {} && exit 3)",
        out.display(),
        out.display()
      ),
      Some(5),
      Some(1),
    )],
  );
  tokio::time::sleep(Duration::from_millis(100)).await;

  bus.deliver(delivery("deploy/web", b""));
  assert!(wait_for(&out).await, "the first run did not happen");
  tokio::time::sleep(Duration::from_millis(300)).await;
  // The runner is still listening after a non-zero exit.
  bus.deliver(delivery("deploy/web", b""));
  tokio::time::sleep(Duration::from_millis(400)).await;
}
