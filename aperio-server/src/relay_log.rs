//! An access log for the relays (planned_features #66).
//!
//! Every proxied HTTP request produces a structured line. A TCP or UDP relay
//! produced nothing at all, so a database reached through a tunnel left no
//! record of who connected, for how long, or how much moved. That is the one
//! surface where the question is asked after the fact, by somebody who wants
//! to know what a compromised credential could have reached.
//!
//! Connection-level, never per packet. A relay that logged every datagram
//! would produce a line per packet of a video stream, which is not a log
//! anybody reads; a line per connection, with the totals, is.
//!
//! The line goes to the same two places an HTTP access line goes: a structured
//! `aperio_access` tracing event and, when configured, the access-log file. It
//! is deliberately the same shape, so one pipeline ingests both and a query
//! for "everything that touched this token" answers across transports.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::state::AppState;

/// What one relay connection turned out to be.
///
/// Built at the start so the fields that identify the connection are captured
/// while they are still in hand, and finished at the end with what it did.
pub(crate) struct RelayRecord {
  /// `tcp` or `udp`.
  transport: &'static str,
  /// How the connection arrived: an `expose:` port, or a peer client dialling
  /// a tunnel. They are different security stories and the log should not make
  /// the reader infer which one it is looking at.
  kind: &'static str,
  /// Address the connection came from.
  peer: String,
  /// Connection id of the client that served it.
  client_id: String,
  /// Tunnel name, where the connection named one.
  tunnel: Option<String>,
  /// Label of the token that authorized it (`master` for the master token,
  /// absent for a public `expose:` port, which authorizes nobody).
  token: Option<String>,
  /// Public port, for an `expose:` connection.
  port: Option<u16>,
  opened_at: Instant,
  /// Bytes from the peer towards the backend, and the other way. Shared with
  /// the relay tasks, which are the only things that can count them.
  to_backend: Arc<AtomicU64>,
  to_peer: Arc<AtomicU64>,
}

impl RelayRecord {
  pub(crate) fn new(
    transport: &'static str,
    kind: &'static str,
    peer: String,
    client_id: String,
  ) -> Self {
    RelayRecord {
      transport,
      kind,
      peer,
      client_id,
      tunnel: None,
      token: None,
      port: None,
      opened_at: Instant::now(),
      to_backend: Arc::new(AtomicU64::new(0)),
      to_peer: Arc::new(AtomicU64::new(0)),
    }
  }

  pub(crate) fn tunnel(mut self, name: Option<String>) -> Self {
    self.tunnel = name;
    self
  }

  pub(crate) fn token(mut self, label: Option<String>) -> Self {
    self.token = label;
    self
  }

  pub(crate) fn port(mut self, port: u16) -> Self {
    self.port = Some(port);
    self
  }

  /// The counter for bytes travelling towards the backend.
  pub(crate) fn up_counter(&self) -> Arc<AtomicU64> {
    self.to_backend.clone()
  }

  /// The counter for bytes travelling back to the peer.
  pub(crate) fn down_counter(&self) -> Arc<AtomicU64> {
    self.to_peer.clone()
  }

  /// Writes the line. Called once, when the relay is over.
  ///
  /// Sampling applies exactly as it does to an HTTP line, so an operator who
  /// turned the volume down gets it turned down here too rather than
  /// discovering one surface still at full volume.
  pub(crate) fn finish(self, state: &AppState) {
    let duration_ms = self.opened_at.elapsed().as_millis() as u64;
    let to_backend = self.to_backend.load(Ordering::Relaxed);
    let to_peer = self.to_peer.load(Ordering::Relaxed);
    if !crate::access_log::relay_sampled_in(state.config().access_log_sample_rate) {
      return;
    }
    if state.config().access_events {
      tracing::info!(
        target: "aperio_access",
        event = "relay_closed",
        transport = self.transport,
        kind = self.kind,
        peer = %self.peer,
        client_id = %self.client_id,
        tunnel = self.tunnel.as_deref().unwrap_or(""),
        token = self.token.as_deref().unwrap_or(""),
        port = self.port.unwrap_or(0),
        duration_ms,
        bytes_to_backend = to_backend,
        bytes_to_peer = to_peer,
        "relay closed"
      );
    }
    if state.access_log.is_none() {
      return;
    }
    crate::access_log::append_relay_line(
      state,
      &serde_json::json!({
        "ts": chrono::Local::now().to_rfc3339(),
        "event": "relay_closed",
        "transport": self.transport,
        "kind": self.kind,
        "peer": self.peer,
        "client_id": self.client_id,
        "tunnel": self.tunnel,
        "token": self.token,
        "port": self.port,
        "duration_ms": duration_ms,
        "bytes_to_backend": to_backend,
        "bytes_to_peer": to_peer,
      }),
    );
  }
}
