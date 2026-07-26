//! Producer-side flow control (tunnel protocol v3).
//!
//! When a visitor reads slower than this client produces, the server's
//! per-stream backlog grows and it sends `StreamPause { id }`; producing for
//! that one stream then waits until the matching `StreamResume`, so ordinary
//! TCP backpressure reaches the backend instead of the server having to
//! buffer or drop the stream. Each producer (a streamed HTTP response body,
//! a proxied WebSocket, a raw TCP relay) registers its id here; the tunnel
//! read loop flips the switches as the messages arrive.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tracing::debug;

/// Upper bound on one uninterrupted pause. A `StreamResume` can be lost (the
/// stream was torn down server-side while paused and the best-effort resume
/// did not go out), and a producer must not hang forever on it: after this
/// long it carries on, which at worst re-sends bytes the server discards.
const PAUSE_SAFETY_CAP: Duration = Duration::from_secs(30);

/// One stream's pause switch.
pub(crate) struct PauseSignal {
  paused: AtomicBool,
  resumed: Notify,
}

impl PauseSignal {
  fn new() -> Self {
    PauseSignal {
      paused: AtomicBool::new(false),
      resumed: Notify::new(),
    }
  }

  pub(crate) fn pause(&self) {
    self.paused.store(true, Ordering::SeqCst);
  }

  pub(crate) fn resume(&self) {
    self.paused.store(false, Ordering::SeqCst);
    self.resumed.notify_waiters();
  }

  /// Waits while the stream is paused. Returns promptly when it is not, so
  /// producers simply call this before every chunk they read or send.
  pub(crate) async fn wait_while_paused(&self) {
    while self.paused.load(Ordering::SeqCst) {
      let notified = self.resumed.notified();
      tokio::pin!(notified);
      // Register as a waiter before re-reading the flag, so a resume landing
      // between the check and the await is not lost.
      notified.as_mut().enable();
      if !self.paused.load(Ordering::SeqCst) {
        return;
      }
      if tokio::time::timeout(PAUSE_SAFETY_CAP, notified)
        .await
        .is_err()
      {
        debug!("stream pause outlived its safety cap; resuming on our own");
        self.paused.store(false, Ordering::SeqCst);
        return;
      }
    }
  }
}

/// The pause switches of every stream this connection is currently
/// producing, keyed by the id used on the wire (request id or stream id).
/// A `StreamPause`/`StreamResume` for an id no longer (or not yet)
/// registered is a no-op, mirroring how the server treats frames for
/// unknown streams.
#[derive(Clone, Default)]
pub(crate) struct PauseRegistry(Arc<Mutex<HashMap<String, Arc<PauseSignal>>>>);

impl PauseRegistry {
  /// Registers a producer; the switch lives until the returned guard drops.
  pub(crate) fn register(&self, id: &str) -> PauseGuard {
    let signal = Arc::new(PauseSignal::new());
    self
      .0
      .lock()
      .expect("pause registry lock poisoned")
      .insert(id.to_string(), signal.clone());
    PauseGuard {
      registry: self.clone(),
      id: id.to_string(),
      signal,
    }
  }

  pub(crate) fn pause(&self, id: &str) {
    if let Some(signal) = self.0.lock().expect("pause registry lock poisoned").get(id) {
      signal.pause();
    }
  }

  pub(crate) fn resume(&self, id: &str) {
    if let Some(signal) = self.0.lock().expect("pause registry lock poisoned").get(id) {
      signal.resume();
    }
  }
}

/// Unregisters the stream on drop, so a finished producer cannot be paused.
pub(crate) struct PauseGuard {
  registry: PauseRegistry,
  id: String,
  signal: Arc<PauseSignal>,
}

impl PauseGuard {
  pub(crate) fn signal(&self) -> &PauseSignal {
    &self.signal
  }
}

impl Drop for PauseGuard {
  fn drop(&mut self) {
    self
      .registry
      .0
      .lock()
      .expect("pause registry lock poisoned")
      .remove(&self.id);
  }
}

#[cfg(test)]
#[path = "flow_tests.rs"]
mod tests;
