//! Messages between the clients of one organization.
//!
//! Clients can already be reached from outside and can reach a private service
//! through a peer, but they had no way to *signal each other*. This is that:
//! a client subscribes to topic filters over the tunnel it already holds, any
//! client (or the admin API) publishes, and the server fans the message out to
//! whoever asked for it inside the same organization.
//!
//! Two decisions shape everything here.
//!
//! **Subscriptions belong to a client process, not to a connection.** A client
//! with a `services:` list holds one tunnel connection per service; keyed on
//! the connection, one publish would arrive at that process N times and every
//! subscriber would need a deduplication cache with a time window nobody can
//! size correctly. Keyed on `instance_group` — the process identity the server
//! already tracks for the dashboard and for random-hostname sharing — the
//! duplicate never exists.
//!
//! **Delivery is best-effort and bounded.** A message goes to the connections
//! that are up when it is published; nothing is stored for a client that is
//! away. That is a deliberate limit, not an unfinished one: the case this
//! serves is a client reacting to something happening now, and replaying an
//! hour-old event to a machine that just came back is a bug, not a service.

use std::collections::HashSet;

use aperio_config::{RESERVED_TOPIC_PREFIX, topic_matches};

use crate::protocol::TunnelMessage;
use crate::state::AppState;

/// Most filters one client process may hold at once. A subscription costs a
/// string and a linear match per publish; the cap is here so a loop in someone
/// else's code cannot turn into unbounded server memory.
pub(crate) const MAX_FILTERS_PER_CLIENT: usize = 64;

/// Largest payload accepted, before Base64. Big enough for an event with
/// context, small enough that a publish is never a way to move bulk data:
/// that is what tunnels are for.
pub(crate) const MAX_PAYLOAD_BYTES: usize = 256 * 1024;

/// Where a published message came from, for the audit line and for the
/// reserved-namespace rule.
pub(crate) enum Publisher<'a> {
  /// A connected client, named by its connection id.
  Client(&'a str),
  /// The admin API, on behalf of a dashboard user or an admin key.
  Api(&'a str),
  /// The server itself, publishing into `$aperio/`.
  Server,
}

impl Publisher<'_> {
  fn label(&self) -> String {
    match self {
      Publisher::Client(id) => format!("client={id}"),
      Publisher::Api(actor) => format!("api={actor}"),
      Publisher::Server => "server".to_string(),
    }
  }

  /// Only the server may publish into its own namespace, so a `$aperio/`
  /// event always means what it says rather than what a client claimed.
  fn may_use_reserved(&self) -> bool {
    matches!(self, Publisher::Server)
  }
}

/// What a publish did, for the caller to report.
pub(crate) struct Delivery {
  /// Client processes the message was handed to.
  pub(crate) processes: usize,
  /// Connections written to. Equal to `processes` unless a process lost a
  /// connection between the lookup and the send.
  pub(crate) connections: usize,
}

/// Replaces this connection's filters, rejecting the ones that are not usable.
///
/// Returns the filters that were refused, so the client can be told rather
/// than left believing it is subscribed. A whole `Subscribe` is never rejected
/// for one bad filter: the others are still what the operator asked for.
pub(crate) async fn set_subscriptions(
  state: &AppState,
  connection_id: &str,
  topics: Vec<String>,
  add: bool,
) -> Vec<(String, String)> {
  let mut refused = Vec::new();
  let mut clients = state.clients.lock().await;
  let Some(handle) = clients.get_mut(connection_id) else {
    return refused;
  };
  for topic in topics {
    if !add {
      handle.subscriptions.retain(|f| f != &topic);
      continue;
    }
    if let Err(why) = aperio_config::validate_topic_filter(&topic) {
      refused.push((topic, why));
      continue;
    }
    if handle.subscriptions.contains(&topic) {
      continue;
    }
    if handle.subscriptions.len() >= MAX_FILTERS_PER_CLIENT {
      refused.push((
        topic,
        format!("at the limit of {MAX_FILTERS_PER_CLIENT} topic filters"),
      ));
      continue;
    }
    handle.subscriptions.push(topic);
  }
  refused
}

/// Publishes `payload` on `topic` to every subscriber in `org`.
///
/// The organization is the boundary: a message never crosses it, and the
/// master organization is not a superset of the others. Nothing is stored, so
/// a client that is not connected does not receive it later.
pub(crate) async fn publish(
  state: &AppState,
  org: Option<&str>,
  topic: &str,
  payload: &[u8],
  publisher: Publisher<'_>,
) -> Result<Delivery, String> {
  aperio_config::validate_topic(topic)?;
  if topic.starts_with(RESERVED_TOPIC_PREFIX) && !publisher.may_use_reserved() {
    return Err(format!(
      "`{RESERVED_TOPIC_PREFIX}` is the server's own namespace and cannot be published into"
    ));
  }
  if payload.len() > MAX_PAYLOAD_BYTES {
    return Err(format!(
      "payload is {} bytes, over the {MAX_PAYLOAD_BYTES}-byte limit",
      payload.len()
    ));
  }

  let id = uuid::Uuid::new_v4().to_string();
  let frame = TunnelMessage::Publish {
    topic: topic.to_string(),
    payload: {
      use base64::prelude::*;
      BASE64_STANDARD.encode(payload)
    },
    id: Some(id.clone()),
  };
  let text = match serde_json::to_string(&frame) {
    Ok(t) => t,
    Err(e) => return Err(format!("cannot encode the message: {e}")),
  };

  // One connection per subscribing *process*. Collected under the lock and
  // sent outside it: a send can block on a slow client's channel, and holding
  // the client map while that happens stalls every other connection.
  let targets: Vec<(
    String,
    tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
  )> = {
    let clients = state.clients.lock().await;
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (connection_id, handle) in clients.iter() {
      if handle.perms.org_id.as_deref() != org {
        continue;
      }
      if !handle.subscriptions.iter().any(|f| topic_matches(f, topic)) {
        continue;
      }
      // A client that predates the instance header has no process identity to
      // group by; its connection is the best identity available, which is the
      // pre-existing behaviour for every other per-process feature.
      let process = handle
        .instance_group
        .clone()
        .unwrap_or_else(|| connection_id.clone());
      if seen.insert(process) {
        out.push((connection_id.clone(), handle.tx.clone()));
      }
    }
    out
  };

  let processes = targets.len();
  let mut connections = 0usize;
  for (connection_id, tx) in targets {
    // `try_send`, not `send`: a subscriber whose channel is full is a
    // subscriber that is not keeping up, and blocking the publisher on it
    // would let one slow client stall the fan-out for everyone.
    match tx.try_send(axum::extract::ws::Message::Text(text.clone())) {
      Ok(()) => connections += 1,
      Err(e) => {
        tracing::warn!("Dropping message {id} for client {connection_id}: {e}");
      }
    }
  }

  tracing::debug!(
    "Published {id} on '{topic}' to {processes} process(es) ({} bytes, {})",
    payload.len(),
    publisher.label()
  );
  Ok(Delivery {
    processes,
    connections,
  })
}

#[cfg(test)]
#[path = "pubsub_tests.rs"]
mod tests;
