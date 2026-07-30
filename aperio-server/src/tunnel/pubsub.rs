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
//! size correctly. Keyed on `instance_group`, the process identity the server
//! already tracks for the dashboard and for random-hostname sharing, the
//! duplicate never exists.
//!
//! **Delivery is best-effort and bounded.** A message goes to the connections
//! that are up when it is published; nothing is stored for a client that is
//! away. That is a deliberate limit, not an unfinished one: the case this
//! serves is a client reacting to something happening now, and replaying an
//! hour-old event to a machine that just came back is a bug, not a service.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use aperio_config::{RESERVED_TOPIC_PREFIX, topic_matches};

use crate::protocol::TunnelMessage;
use crate::state::{AppState, ClientPerms};

/// Most filters one client process may hold at once. A subscription costs a
/// string and a linear match per publish; the cap is here so a loop in someone
/// else's code cannot turn into unbounded server memory.
pub(crate) const MAX_FILTERS_PER_CLIENT: usize = 64;

/// How long the server waits for an acknowledgement before sending a QoS 1
/// message again.
pub(crate) const ACK_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a QoS 1 message may go unacknowledged before it is given up on.
///
/// This is the whole of "at least once" here, and the number says so: the
/// window covers a connection that died between the write and the
/// acknowledgement, not a subscriber that is away. Storing for an absent
/// client is a different feature, and replaying a half-hour-old event at a
/// machine that just came back is a bug rather than a service.
pub(crate) const MAX_ACK_WAIT: Duration = Duration::from_secs(30);

/// Un-acknowledged messages held for one client process. Bounded so a
/// subscriber that stopped acknowledging costs memory that stops growing.
pub(crate) const MAX_PENDING_PER_PROCESS: usize = 256;

/// Largest payload accepted, before Base64. Big enough for an event with
/// context, small enough that a publish is never a way to move bulk data:
/// that is what tunnels are for. Shared with the client so it can refuse the
/// same thing rather than reporting success for a message about to be dropped.
pub(crate) const MAX_PAYLOAD_BYTES: usize = aperio_config::MAX_MESSAGE_BYTES;

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

/// What the messaging path has done, for the metrics endpoint.
///
/// Atomics rather than a counter behind the stats mutex: a publish already
/// takes the client-map lock to find its subscribers, and adding a second
/// contended lock to the same path to count what it did would be paying for
/// the observation in the thing observed.
#[derive(Default)]
pub(crate) struct MessageMetrics {
  /// Messages accepted for publication, whatever they reached.
  pub(crate) published: std::sync::atomic::AtomicU64,
  /// Deliveries handed to a client process. One publish to three subscribers
  /// counts three.
  pub(crate) delivered: std::sync::atomic::AtomicU64,
  /// Deliveries dropped because a subscriber's connection was not keeping up.
  /// The number to alert on: it means a client is missing messages.
  pub(crate) dropped: std::sync::atomic::AtomicU64,
  /// QoS 1 deliveries sent again because no acknowledgement came back.
  pub(crate) resent: std::sync::atomic::AtomicU64,
  /// QoS 1 messages given up on after the acknowledgement window.
  pub(crate) abandoned: std::sync::atomic::AtomicU64,
}

/// One QoS 1 delivery waiting to be acknowledged.
pub(crate) struct Pending {
  pub(crate) id: String,
  pub(crate) topic: String,
  /// The encoded frame, kept whole so a resend is a write rather than a
  /// re-serialization.
  pub(crate) frame: String,
  pub(crate) first_sent: Instant,
  pub(crate) last_sent: Instant,
  pub(crate) attempts: u32,
}

/// What a publish did, for the caller to report.
pub(crate) struct Delivery {
  /// Client processes the message was handed to.
  pub(crate) processes: usize,
  /// Connections written to. Equal to `processes` unless a process lost a
  /// connection between the lookup and the send.
  pub(crate) connections: usize,
}

/// May a token holding `perms` use `filter` at all?
///
/// One rule for both directions. Publishing to a topic and subscribing to it
/// are the same access to the same conversation: a token that may read
/// `deploy/#` may signal on it, and a token that may do neither cannot do
/// either by asking the other way round.
///
/// A subscription filter is permitted when the token's own filters *cover*
/// it. `deploy/#` covers `deploy/web`, and does not cover `#`: otherwise
/// subscribing to everything would be a way around a scope that named one
/// subtree.
pub(crate) fn may_use_topic(perms: &ClientPerms, filter: &str) -> bool {
  if perms.master {
    return true;
  }
  perms.topics.iter().any(|granted| covers(granted, filter))
}

/// Does the filter `granted` permit everything `asked` could match?
///
/// Level by level: a granted `#` swallows the rest, a granted `+` accepts one
/// concrete level or another `+` but not a `#`, and a literal matches only
/// itself. Anything the asked filter could reach beyond the granted one makes
/// it a widening, which is refused.
fn covers(granted: &str, asked: &str) -> bool {
  let mut g = granted.split('/');
  let mut a = asked.split('/');
  loop {
    match (g.next(), a.next()) {
      (Some("#"), _) => return true,
      (Some("+"), Some(level)) if level != "#" => continue,
      (Some(gl), Some(al)) if gl == al => continue,
      (None, None) => return true,
      _ => return false,
    }
  }
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
    if !may_use_topic(&handle.perms, &topic) {
      refused.push((
        topic,
        "the token does not carry this topic; add it to the token's topics".to_string(),
      ));
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
  qos: u8,
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
  // Anything above 1 is delivered as 1. There is no store-and-forward here to
  // build exactly-once on, and granting a level the machinery does not have
  // is worse than saying what it does.
  let qos = qos.min(1);
  let frame = TunnelMessage::Publish {
    topic: topic.to_string(),
    payload: {
      use base64::prelude::*;
      BASE64_STANDARD.encode(payload)
    },
    id: Some(id.clone()),
    qos,
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
      if seen.insert(process.clone()) {
        out.push((process, connection_id.clone(), handle.tx.clone()));
      }
    }
    out
  };

  use std::sync::atomic::Ordering as AtomicOrdering;
  state
    .message_metrics
    .published
    .fetch_add(1, AtomicOrdering::Relaxed);
  let processes = targets.len();
  let mut connections = 0usize;
  let mut delivered_to: Vec<String> = Vec::new();
  for (process, connection_id, tx) in targets {
    // `try_send`, not `send`: a subscriber whose channel is full is a
    // subscriber that is not keeping up, and blocking the publisher on it
    // would let one slow client stall the fan-out for everyone.
    match tx.try_send(axum::extract::ws::Message::Text(text.clone().into())) {
      Ok(()) => {
        connections += 1;
        state
          .message_metrics
          .delivered
          .fetch_add(1, AtomicOrdering::Relaxed);
        delivered_to.push(process);
      }
      Err(e) => {
        state
          .message_metrics
          .dropped
          .fetch_add(1, AtomicOrdering::Relaxed);
        tracing::warn!("Dropping message {id} for client {connection_id}: {e}");
      }
    }
  }

  // At-least-once: remember it until the subscriber says it arrived. Only for
  // the processes actually written to, one whose channel was full never got
  // it, and holding a retry for it would be a queue for a client that is not
  // reading, which is what the bound above exists to refuse.
  if qos >= 1 {
    let now = Instant::now();
    let mut pending = state.pending_messages.lock().await;
    for process in delivered_to {
      let queue = pending.entry(process).or_default();
      if queue.len() >= MAX_PENDING_PER_PROCESS {
        let dropped = queue.remove(0);
        tracing::warn!(
          "Message {} to a client that is not acknowledging was dropped at the {MAX_PENDING_PER_PROCESS}-message limit",
          dropped.id
        );
      }
      queue.push(Pending {
        id: id.clone(),
        topic: topic.to_string(),
        frame: text.clone(),
        first_sent: now,
        last_sent: now,
        attempts: 1,
      });
    }
  }

  tracing::debug!(
    "Published {id} on '{topic}' (qos {qos}) to {processes} process(es) ({} bytes, {})",
    payload.len(),
    publisher.label()
  );
  Ok(Delivery {
    processes,
    connections,
  })
}

/// Records that `id` arrived at the process behind `connection_id`.
///
/// Keyed on the process, not the connection: the acknowledgement may come
/// back on a different one of that process's connections than the delivery
/// went out on, and it is the same subscriber either way.
pub(crate) async fn acknowledge(state: &AppState, connection_id: &str, id: &str) {
  let process = {
    let clients = state.clients.lock().await;
    let Some(handle) = clients.get(connection_id) else {
      return;
    };
    handle
      .instance_group
      .clone()
      .unwrap_or_else(|| connection_id.to_string())
  };
  let mut pending = state.pending_messages.lock().await;
  if let Some(queue) = pending.get_mut(&process) {
    queue.retain(|p| p.id != id);
    if queue.is_empty() {
      pending.remove(&process);
    }
  }
}

/// Resends what has gone unacknowledged, and gives up on what has waited too
/// long. Called on a timer by [`run_ack_sweeper`].
///
/// Returns how many were resent and how many were abandoned, for the test to
/// assert on rather than for the caller to use.
pub(crate) async fn sweep_pending(state: &AppState) -> (usize, usize) {
  let now = Instant::now();
  // Which processes have work due, decided under the pending lock alone. The
  // client map is locked afterwards, never both at once: two locks taken in
  // different orders in different places is how a deadlock gets written.
  let due: Vec<(String, Vec<(String, String)>)> = {
    let mut pending = state.pending_messages.lock().await;
    let mut out = Vec::new();
    for (process, queue) in pending.iter_mut() {
      let mut resend = Vec::new();
      for message in queue.iter_mut() {
        if now.duration_since(message.last_sent) < ACK_TIMEOUT {
          continue;
        }
        message.last_sent = now;
        message.attempts += 1;
        resend.push((message.id.clone(), message.frame.clone()));
      }
      if !resend.is_empty() {
        out.push((process.clone(), resend));
      }
    }
    out
  };

  let mut resent = 0usize;
  for (process, messages) in due {
    // Any live connection of that process will do; they all reach it.
    let target = {
      let clients = state.clients.lock().await;
      clients
        .values()
        .find(|h| {
          h.instance_group
            .as_deref()
            .is_some_and(|group| group == process)
        })
        .map(|h| h.tx.clone())
        .or_else(|| clients.get(&process).map(|h| h.tx.clone()))
    };
    let Some(tx) = target else {
      continue;
    };
    for (id, frame) in messages {
      if tx
        .try_send(axum::extract::ws::Message::Text(frame.into()))
        .is_ok()
      {
        resent += 1;
        state
          .message_metrics
          .resent
          .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::debug!("Resent message {id} to a client that has not acknowledged it");
      }
    }
  }

  // Give up on anything past the window, and on a process with no live
  // connection at all: nothing is stored for a subscriber that is away.
  let mut abandoned = 0usize;
  {
    let mut pending = state.pending_messages.lock().await;
    pending.retain(|_, queue| {
      queue.retain(|message| {
        let expired = now.duration_since(message.first_sent) >= MAX_ACK_WAIT;
        if expired {
          abandoned += 1;
          state
            .message_metrics
            .abandoned
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
          tracing::warn!(
            "Giving up on message {} to '{}' after {} attempt(s) with no acknowledgement",
            message.id,
            message.topic,
            message.attempts
          );
        }
        !expired
      });
      !queue.is_empty()
    });
  }
  (resent, abandoned)
}

/// Runs [`sweep_pending`] until the process ends.
pub(crate) fn run_ack_sweeper(state: std::sync::Arc<AppState>) {
  tokio::spawn(async move {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
      tick.tick().await;
      sweep_pending(&state).await;
    }
  });
}

#[cfg(test)]
#[path = "pubsub_tests.rs"]
mod tests;
