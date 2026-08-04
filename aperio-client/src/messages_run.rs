//! Running a command when a message arrives.
//!
//! This is a remote-execution primitive by design: a message published by
//! another client of the organization causes a command to run on this
//! machine. Every constraint here follows from that sentence, and none of
//! them is a detail to relax later.
//!
//! - **The payload never reaches the command line.** It goes to stdin, and
//!   the topic and message id go to the environment. A message can therefore
//!   never become part of the command, however it is quoted, whatever the
//!   shell would have done with it.
//! - **Concurrency is capped, and the excess is dropped rather than queued.**
//!   A publisher in a loop must not fork a thousand processes; a queue for a
//!   command that cannot keep up is the same problem one step later, with the
//!   memory growing instead.
//! - **Every run is timed.** A command that hangs must not hold the
//!   subscription's slot forever.
//! - **It is opt-in per topic**, written by the operator in a file, and
//!   bounded by the token's `topics` on the server side.
//! - **Every run is logged**, started and finished, with the topic and the
//!   exit status.

use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use aperio_config::topic_matches;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{info, warn};

use crate::pubsub::{Delivery, MessageBus};

/// Seconds a run may take before it is killed.
const DEFAULT_TIMEOUT: u64 = 60;

/// Runs allowed at once for one subscription.
const DEFAULT_MAX_CONCURRENT: u32 = 1;

/// A subscription that runs something.
pub(crate) struct Runner {
  pub(crate) topic: String,
  pub(crate) command: String,
  pub(crate) timeout: Duration,
  pub(crate) max_concurrent: u32,
  /// Operator-declared environment for this subscription's command.
  pub(crate) env: Vec<(String, String)>,
  /// Runs in flight, so the cap is enforced without a lock on the hot path.
  running: Arc<AtomicU32>,
}

impl Runner {
  pub(crate) fn new(
    topic: String,
    command: String,
    timeout: Option<u64>,
    max: Option<u32>,
    env: Vec<(String, String)>,
  ) -> Self {
    Runner {
      topic,
      command,
      timeout: Duration::from_secs(timeout.unwrap_or(DEFAULT_TIMEOUT).max(1)),
      max_concurrent: max.unwrap_or(DEFAULT_MAX_CONCURRENT).max(1),
      env,
      running: Arc::new(AtomicU32::new(0)),
    }
  }
}

/// Watches the bus and runs what the runners ask for, until the process ends.
pub(crate) fn spawn(
  bus: Arc<MessageBus>,
  runners: Vec<Runner>,
) -> Option<tokio::task::JoinHandle<()>> {
  if runners.is_empty() {
    return None;
  }
  for runner in &runners {
    info!(
      "Running `{}` for messages on '{}' (timeout {}s, {} at a time)",
      runner.command,
      runner.topic,
      runner.timeout.as_secs(),
      runner.max_concurrent
    );
  }
  let runners = Arc::new(runners);
  // Subscribed here, not inside the task. A broadcast receiver only sees what
  // is sent after it exists, and the task does not exist until the runtime
  // gets round to it: on a reload, where the previous dispatcher has already
  // been stopped, everything delivered in that gap reached nobody. Taking the
  // receiver on this line closes the gap to the caller's own ordering, which
  // it can control.
  let mut deliveries = bus.listen();
  Some(tokio::spawn(async move {
    loop {
      match deliveries.recv().await {
        Ok(delivery) => {
          for runner in runners.iter() {
            if topic_matches(&runner.topic, &delivery.topic) {
              dispatch(runner, &delivery);
            }
          }
        }
        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
          warn!("Message runners fell behind; {n} message(s) did not trigger a run");
        }
        Err(_) => return,
      }
    }
  }))
}

/// Starts one run, unless this subscription is already at its cap.
fn dispatch(runner: &Runner, delivery: &Delivery) {
  let running = runner.running.clone();
  // Claim a slot before spawning: two messages arriving together must not
  // both see the old count and both start.
  let claimed = running
    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
      (current < runner.max_concurrent).then_some(current + 1)
    })
    .is_ok();
  if !claimed {
    warn!(
      "Dropping a message on '{}': {} run(s) of `{}` are already going",
      delivery.topic, runner.max_concurrent, runner.command
    );
    return;
  }

  let command = runner.command.clone();
  let timeout = runner.timeout;
  let run_env = runner.env.clone();
  let topic = delivery.topic.clone();
  let id = delivery.id.clone().unwrap_or_default();
  let payload = delivery.payload.clone();

  tokio::spawn(async move {
    let started = std::time::Instant::now();
    info!("Message on '{topic}' is running `{command}`");
    let outcome = run_once(&command, &topic, &id, &payload, timeout, &run_env).await;
    running.fetch_sub(1, Ordering::SeqCst);
    match outcome {
      Ok(Some(status)) if status.success() => {
        info!(
          "`{command}` finished for '{topic}' in {:.1}s",
          started.elapsed().as_secs_f64()
        );
      }
      Ok(Some(status)) => {
        warn!("`{command}` exited {status} for a message on '{topic}'");
      }
      // Killed by the timeout.
      Ok(None) => {
        warn!(
          "`{command}` was killed after {}s for a message on '{topic}'",
          timeout.as_secs()
        );
      }
      Err(e) => warn!("`{command}` could not be run for '{topic}': {e}"),
    }
  });
}

/// Runs the command once. `Ok(None)` means it was killed by the timeout.
async fn run_once(
  command: &str,
  topic: &str,
  id: &str,
  payload: &[u8],
  timeout: Duration,
  env: &[(String, String)],
) -> Result<Option<std::process::ExitStatus>, String> {
  let shell = if cfg!(windows) { "cmd" } else { "sh" };
  let flag = if cfg!(windows) { "/C" } else { "-c" };
  let mut builder = Command::new(shell);
  // Its own process group, so the timeout can end the whole pipeline the
  // shell started rather than just the shell. Without it, `sh -c "curl … |
  // tee …"` leaves both halves running past the deadline the operator set,
  // still holding the pipe the payload was written to.
  #[cfg(unix)]
  builder.process_group(0);
  let mut child = builder
    .arg(flag)
    .arg(command)
    // The operator's own variables first, so the two below always win: a
    // command that reads APERIO_MESSAGE_TOPIC has to be able to trust that it
    // describes the message it is handling.
    .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
    // The message is data, so it travels where data goes. Nothing about it
    // is ever part of the command, which is the whole reason this is safe to
    // offer at all.
    .env("APERIO_MESSAGE_TOPIC", topic)
    .env("APERIO_MESSAGE_ID", id)
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .spawn()
    .map_err(|e| e.to_string())?;

  // The write is inside the budget, not before it. A command that never reads
  // its stdin does not necessarily close it: it may simply be busy, or
  // sleeping. Once the pipe buffer fills, and 64 KB is a perfectly ordinary
  // message, `write_all` blocks, and it used to block before the timeout had
  // started, so the runner task hung for the life of the process holding one
  // of the operator's concurrency slots forever.
  let mut stdin = child.stdin.take();
  let feed = async {
    if let Some(mut pipe) = stdin.take() {
      // A command that does close stdin gives a broken pipe here, and that is
      // not a failure of the run.
      let _ = pipe.write_all(payload).await;
      let _ = pipe.shutdown().await;
    }
    child.wait().await
  };
  let outcome = tokio::time::timeout(timeout, feed).await;
  match outcome {
    Ok(status) => status.map(Some).map_err(|e| e.to_string()),
    Err(_) => {
      // The shell, and on Unix everything it started. `kill` on its own ends
      // the `sh -c` and leaves the pipeline it spawned running past the
      // timeout the operator set, still holding the payload's pipe.
      #[cfg(unix)]
      if let Some(pid) = child.id() {
        // SAFETY: a kill of a process group this process created. A negative
        // pid is the group; the child was put in its own by `process_group(0)`
        // above, so nothing outside this command can be in it.
        unsafe {
          libc::kill(-(pid as i32), libc::SIGKILL);
        }
      }
      let _ = child.kill().await;
      Ok(None)
    }
  }
}

#[cfg(test)]
#[path = "messages_run_tests.rs"]
mod tests;
