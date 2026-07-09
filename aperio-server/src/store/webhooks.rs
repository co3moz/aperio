use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// A webhook definition: which events to deliver to which URL.
#[derive(Serialize, Deserialize, Clone, utoipa::ToSchema)]
pub struct Webhook {
  pub id: String,
  pub name: String,
  pub url: String,
  /// Subscribed event names; `["*"]` (or empty) = all events.
  pub events: Vec<String>,
  pub enabled: bool,
  pub created_at: u64,
  /// Optional HMAC signing secret. When set, deliveries carry
  /// `X-Aperio-Timestamp` and `X-Aperio-Signature: sha256=<hex>` computed over
  /// `"<timestamp>.<body>"`, so the receiver can verify origin and freshness.
  /// Never exposed through the list API (only persisted here).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub secret: Option<String>,
}

impl Webhook {
  fn subscribes_to(&self, event: &str) -> bool {
    self.events.is_empty() || self.events.iter().any(|e| e == "*" || e == event)
  }
}

/// Persistent store of webhook definitions, backed by the `webhooks` table
/// of the shared SQLite store (`<data_dir>/aperio.db`).
pub struct WebhookStore {
  conn: rusqlite::Connection,
  webhooks: Vec<Webhook>,
}

impl WebhookStore {
  pub fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let webhooks: Vec<Webhook> = crate::store::load_all(&conn, "webhooks");
    if !webhooks.is_empty() {
      info!("Loaded {} webhook(s) from the store", webhooks.len());
    }
    WebhookStore { conn, webhooks }
  }

  fn persist(&mut self) {
    let rows: Vec<(String, String)> = self
      .webhooks
      .iter()
      .filter_map(|w| {
        serde_json::to_string(w)
          .ok()
          .map(|json| (w.id.clone(), json))
      })
      .collect();
    crate::store::replace_all(&mut self.conn, "webhooks", &rows);
  }

  pub fn create(
    &mut self,
    name: String,
    url: String,
    events: Vec<String>,
    secret: Option<String>,
  ) -> Webhook {
    let hook = Webhook {
      id: uuid::Uuid::new_v4().to_string(),
      name,
      url,
      events,
      enabled: true,
      created_at: crate::store::tokens::now_secs(),
      secret,
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

/// Computes the webhook delivery signature: hex HMAC-SHA256 of
/// `"<timestamp>.<body>"` with the webhook's secret. The timestamp is bound
/// into the MAC so a captured delivery cannot be replayed later without the
/// receiver noticing the stale `X-Aperio-Timestamp`.
pub(crate) fn sign_payload(secret: &str, timestamp: u64, body: &str) -> String {
  use hmac::{Hmac, Mac};
  use sha2::Sha256;
  let mut mac =
    Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
  mac.update(timestamp.to_string().as_bytes());
  mac.update(b".");
  mac.update(body.as_bytes());
  let out = mac.finalize().into_bytes();
  out.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Fire-and-forget delivery of an event to all subscribed webhooks.
/// Payload shape: `{"event": "...", "timestamp": "...", "data": {...}}`.
/// Webhooks with a signing secret get `X-Aperio-Timestamp` and
/// `X-Aperio-Signature: sha256=<hex>` headers (see [`sign_payload`]).
pub fn dispatch(subscribers: Vec<Webhook>, event: &str, data: serde_json::Value) {
  if subscribers.is_empty() {
    return;
  }
  let payload = serde_json::json!({
    "event": event,
    "timestamp": chrono::Local::now().to_rfc3339(),
    "data": data,
  });
  // Serialize once: the signature must cover the exact bytes sent.
  let body = payload.to_string();
  for hook in subscribers {
    let body = body.clone();
    let event = event.to_string();
    tokio::spawn(async move {
      let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();
      let mut req = client
        .post(&hook.url)
        .header("content-type", "application/json");
      if let Some(ref secret) = hook.secret {
        let ts = crate::store::tokens::now_secs();
        let sig = sign_payload(secret, ts, &body);
        req = req
          .header("x-aperio-timestamp", ts.to_string())
          .header("x-aperio-signature", format!("sha256={sig}"));
      }
      match req.body(body).send().await {
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
      None,
    );
    assert_eq!(store.subscribers("client_connected").len(), 1);
    assert_eq!(store.subscribers("token_created").len(), 0);

    // Wildcard subscription
    store.create(
      "all".to_string(),
      "http://127.0.0.1:1/all".to_string(),
      vec!["*".to_string()],
      None,
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

  #[test]
  fn test_signature_is_stable_and_key_dependent() {
    let body = r#"{"event":"token_created","data":{}}"#;
    let sig = sign_payload("super-secret-key!", 1_700_000_000, body);
    // Deterministic for identical inputs.
    assert_eq!(sig, sign_payload("super-secret-key!", 1_700_000_000, body));
    assert_eq!(sig.len(), 64); // hex SHA-256
    // Any change to key, timestamp or body changes the MAC.
    assert_ne!(sig, sign_payload("other-secret-key!", 1_700_000_000, body));
    assert_ne!(sig, sign_payload("super-secret-key!", 1_700_000_001, body));
    assert_ne!(sig, sign_payload("super-secret-key!", 1_700_000_000, "{}"));
  }

  #[test]
  fn test_secret_persists_across_reload() {
    let dir = std::env::temp_dir().join(format!("aperio-webhooks-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_string_lossy().to_string();

    let mut store = WebhookStore::load(&dir_str);
    store.create(
      "signed".to_string(),
      "http://127.0.0.1:1/hook".to_string(),
      vec![],
      Some("super-secret-key!".to_string()),
    );
    let reloaded = WebhookStore::load(&dir_str);
    assert_eq!(
      reloaded.list()[0].secret.as_deref(),
      Some("super-secret-key!")
    );

    let _ = std::fs::remove_dir_all(&dir);
  }
}
