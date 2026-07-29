//! The client's half of messaging between the clients of an organization.
//!
//! One bus per process, shared by every tunnel connection. It holds two
//! things: the topic filters this process wants (from `subscribe:` in the
//! config and from whatever is attached to the local face), and the live
//! connections a publish can go out on.
//!
//! Every connection subscribes with the full filter set when it comes up.
//! That looks redundant for a client running several services, and is not:
//! the server keys delivery on the client *process*, so the copies collapse
//! to one, while a connection that never subscribed would leave the process
//! deaf the moment its sibling dropped.

use std::sync::Arc;

use aperio_config::topic_matches;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, warn};

use crate::protocol::TunnelMessage;

/// How many deliveries a slow local subscriber may fall behind before it
/// starts missing them. Bounded on purpose: an SSE client that stopped
/// reading must cost memory that stops growing, not memory that grows until
/// the process dies.
const DELIVERY_BACKLOG: usize = 256;

/// One message that arrived for this process.
#[derive(Clone, Debug)]
pub(crate) struct Delivery {
  pub(crate) topic: String,
  pub(crate) payload: Vec<u8>,
  /// Server-assigned, the same across every delivery of one publish.
  pub(crate) id: Option<String>,
}

/// Process-wide message bus.
pub(crate) struct MessageBus {
  /// Filters this process wants, from the config and from local subscribers.
  filters: Mutex<Vec<String>>,
  /// Fan-out to whoever is listening inside this process.
  incoming: broadcast::Sender<Delivery>,
  /// Writers of the live tunnel connections, in the order they came up. A
  /// publish goes out on the first one that accepts it: they all reach the
  /// same server, so any will do, and trying the next covers the window where
  /// one has dropped but has not been removed yet.
  writers: Mutex<Vec<(String, mpsc::Sender<Message>)>>,
}

impl MessageBus {
  pub(crate) fn new(filters: Vec<String>) -> Arc<Self> {
    let (incoming, _) = broadcast::channel(DELIVERY_BACKLOG);
    Arc::new(MessageBus {
      filters: Mutex::new(filters),
      incoming,
      writers: Mutex::new(Vec::new()),
    })
  }

  pub(crate) async fn filters(&self) -> Vec<String> {
    self.filters.lock().await.clone()
  }

  /// Adds a filter and returns whether it was new, so the caller knows
  /// whether the connections have to be told.
  pub(crate) async fn add_filter(&self, filter: &str) -> bool {
    let mut filters = self.filters.lock().await;
    if filters.iter().any(|f| f == filter) {
      return false;
    }
    filters.push(filter.to_string());
    true
  }

  /// Listens for deliveries. Each listener gets its own cursor, so a slow one
  /// misses messages instead of holding everybody up.
  pub(crate) fn listen(&self) -> broadcast::Receiver<Delivery> {
    self.incoming.subscribe()
  }

  /// Registers a live connection's writer under `label`.
  pub(crate) async fn attach(&self, label: &str, tx: mpsc::Sender<Message>) {
    let mut writers = self.writers.lock().await;
    writers.retain(|(l, _)| l != label);
    writers.push((label.to_string(), tx));
  }

  pub(crate) async fn detach(&self, label: &str) {
    self.writers.lock().await.retain(|(l, _)| l != label);
  }

  /// Sends the current filter set on `tx`. Called once per connection, as it
  /// comes up and after every reconnect.
  pub(crate) async fn subscribe_on(&self, tx: &mpsc::Sender<Message>) {
    let topics = self.filters().await;
    if topics.is_empty() {
      return;
    }
    if let Ok(json) = serde_json::to_string(&TunnelMessage::Subscribe {
      topics: topics.clone(),
    }) && tx.send(Message::Text(json)).await.is_ok()
    {
      debug!(
        "Subscribed to {} topic filter(s): {}",
        topics.len(),
        topics.join(", ")
      );
    }
  }

  /// Tells every live connection about the current filter set. Used when a
  /// local subscriber asks for a filter the process did not already hold.
  pub(crate) async fn resubscribe_all(&self) {
    let writers: Vec<_> = self.writers.lock().await.clone();
    for (_, tx) in writers {
      self.subscribe_on(&tx).await;
    }
  }

  /// Hands a delivery to the local listeners. Dropped silently when nothing
  /// in this process is listening, which is the normal case for a client that
  /// only publishes.
  pub(crate) fn deliver(&self, delivery: Delivery) {
    let _ = self.incoming.send(delivery);
  }

  /// Publishes over any live tunnel connection.
  pub(crate) async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), String> {
    // Everything the client can know, it decides here. Handing the frame to
    // the tunnel and letting the server drop it would answer "accepted" to a
    // local application for a message that never went anywhere, with the
    // reason in a log the application cannot see.
    aperio_config::validate_publish_topic(topic)?;
    if payload.len() > aperio_config::MAX_MESSAGE_BYTES {
      return Err(format!(
        "payload is {} bytes, over the {}-byte limit",
        payload.len(),
        aperio_config::MAX_MESSAGE_BYTES
      ));
    }
    use base64::prelude::*;
    let frame = TunnelMessage::Publish {
      topic: topic.to_string(),
      payload: BASE64_STANDARD.encode(payload),
      id: None,
    };
    let json = serde_json::to_string(&frame).map_err(|e| e.to_string())?;
    let writers: Vec<_> = self.writers.lock().await.clone();
    if writers.is_empty() {
      return Err("no tunnel connection is up to publish on".to_string());
    }
    for (label, tx) in writers {
      if tx.send(Message::Text(json.clone())).await.is_ok() {
        return Ok(());
      }
      warn!("Connection '{label}' would not take a publish; trying the next");
    }
    Err("every tunnel connection refused the message".to_string())
  }

  /// Does this process want `topic` at all? Used to drop a delivery that
  /// arrived for a filter that has since been removed.
  pub(crate) async fn wants(&self, topic: &str) -> bool {
    self
      .filters
      .lock()
      .await
      .iter()
      .any(|f| topic_matches(f, topic))
  }
}
