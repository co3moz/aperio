use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, error, info, warn};

/// A webhook definition: which events to deliver to which URL.
#[derive(Serialize, Deserialize, Clone)]
pub struct Webhook {
  pub id: String,
  pub name: String,
  pub url: String,
  /// Subscribed event names; `["*"]` (or empty) = all events.
  pub events: Vec<String>,
  pub enabled: bool,
  pub created_at: u64,
}

impl Webhook {
  fn subscribes_to(&self, event: &str) -> bool {
    self.events.is_empty() || self.events.iter().any(|e| e == "*" || e == event)
  }
}

#[derive(Serialize, Deserialize, Default)]
struct WebhookFile {
  webhooks: Vec<Webhook>,
}

/// Persistent store of webhook definitions (`<data_dir>/webhooks.json`).
pub struct WebhookStore {
  path: PathBuf,
  webhooks: Vec<Webhook>,
}

impl WebhookStore {
  pub fn load(data_dir: &str) -> Self {
    let path = PathBuf::from(data_dir).join("webhooks.json");
    let webhooks = match std::fs::read_to_string(&path) {
      Ok(raw) => match serde_json::from_str::<WebhookFile>(&raw) {
        Ok(file) => file.webhooks,
        Err(e) => {
          // Preserve the unparseable file instead of silently discarding every
          // webhook (the next write would overwrite it with an empty store).
          let backup = crate::store::backup_corrupt(&path);
          error!(
            "Failed to parse {:?}: {} — backed up to {:?}, starting with no webhooks",
            path, e, backup
          );
          Vec::new()
        }
      },
      Err(_) => Vec::new(),
    };
    if !webhooks.is_empty() {
      info!("Loaded {} webhook(s) from {:?}", webhooks.len(), path);
    }
    WebhookStore { path, webhooks }
  }

  fn persist(&self) {
    let file = WebhookFile {
      webhooks: self.webhooks.clone(),
    };
    match serde_json::to_string_pretty(&file) {
      Ok(json) => {
        if let Err(e) = crate::store::atomic_write(&self.path, json.as_bytes()) {
          error!("Failed to persist webhooks to {:?}: {}", self.path, e);
        }
      }
      Err(e) => error!("Failed to serialize webhooks: {}", e),
    }
  }

  pub fn create(&mut self, name: String, url: String, events: Vec<String>) -> Webhook {
    let hook = Webhook {
      id: uuid::Uuid::new_v4().to_string(),
      name,
      url,
      events,
      enabled: true,
      created_at: crate::store::tokens::now_secs(),
    };
    self.webhooks.push(hook.clone());
    self.persist();
    hook
  }

  pub fn delete(&mut self, id: &str) -> bool {
    let before = self.webhooks.len();
    self.webhooks.retain(|w| w.id != id);
    let removed = self.webhooks.len() != before;
    if removed {
      self.persist();
    }
    removed
  }

  pub fn list(&self) -> &[Webhook] {
    &self.webhooks
  }

  /// Enabled webhooks subscribed to `event`.
  pub fn subscribers(&self, event: &str) -> Vec<Webhook> {
    self
      .webhooks
      .iter()
      .filter(|w| w.enabled && w.subscribes_to(event))
      .cloned()
      .collect()
  }
}

/// Fire-and-forget delivery of an event to all subscribed webhooks.
/// Payload shape: `{"event": "...", "timestamp": "...", "data": {...}}`.
pub fn dispatch(subscribers: Vec<Webhook>, event: &str, data: serde_json::Value) {
  if subscribers.is_empty() {
    return;
  }
  let payload = serde_json::json!({
    "event": event,
    "timestamp": chrono::Local::now().to_rfc3339(),
    "data": data,
  });
  for hook in subscribers {
    let payload = payload.clone();
    let event = event.to_string();
    tokio::spawn(async move {
      let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();
      match client.post(&hook.url).json(&payload).send().await {
        Ok(res) if res.status().is_success() => {
          debug!("Webhook '{}' delivered event {}", hook.name, event);
        }
        Ok(res) => {
          warn!(
            "Webhook '{}' returned {} for event {}",
            hook.name,
            res.status(),
            event
          );
        }
        Err(e) => {
          warn!(
            "Webhook '{}' delivery failed for event {}: {}",
            hook.name, event, e
          );
        }
      }
    });
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_store_and_subscription() {
    let dir = std::env::temp_dir().join(format!("aperio-webhooks-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_string_lossy().to_string();

    let mut store = WebhookStore::load(&dir_str);
    let hook = store.create(
      "notify".to_string(),
      "http://127.0.0.1:1/hook".to_string(),
      vec!["client_connected".to_string()],
    );
    assert_eq!(store.subscribers("client_connected").len(), 1);
    assert_eq!(store.subscribers("token_created").len(), 0);

    // Wildcard subscription
    store.create(
      "all".to_string(),
      "http://127.0.0.1:1/all".to_string(),
      vec!["*".to_string()],
    );
    assert_eq!(store.subscribers("token_created").len(), 1);

    // Persistence
    let store2 = WebhookStore::load(&dir_str);
    assert_eq!(store2.list().len(), 2);

    // Delete
    let mut store3 = WebhookStore::load(&dir_str);
    assert!(store3.delete(&hook.id));
    assert_eq!(store3.list().len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
  }
}
