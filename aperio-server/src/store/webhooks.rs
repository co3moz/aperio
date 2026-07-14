use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Delivery payload format of a webhook: raw JSON, or a ready-made message
/// for a chat service's incoming-webhook endpoint.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WebhookFormat {
  /// Raw `{event, timestamp, data}` JSON (default).
  #[default]
  Generic,
  /// Slack incoming-webhook message (`text`, mrkdwn).
  Slack,
  /// Discord webhook message (`content`, markdown).
  Discord,
  /// Microsoft Teams incoming-webhook MessageCard.
  Teams,
}

impl WebhookFormat {
  pub fn parse(raw: &str) -> Option<Self> {
    match raw.trim().to_ascii_lowercase().as_str() {
      "" | "generic" => Some(WebhookFormat::Generic),
      "slack" => Some(WebhookFormat::Slack),
      "discord" => Some(WebhookFormat::Discord),
      "teams" => Some(WebhookFormat::Teams),
      _ => None,
    }
  }

  pub fn as_str(&self) -> &'static str {
    match self {
      WebhookFormat::Generic => "generic",
      WebhookFormat::Slack => "slack",
      WebhookFormat::Discord => "discord",
      WebhookFormat::Teams => "teams",
    }
  }
}

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
  /// Delivery payload format (rows predating the field are `generic`).
  #[serde(default)]
  pub format: WebhookFormat,
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

  /// Replaces every webhook record with the given list (dump import) and
  /// persists. Returns how many records are now stored.
  pub fn import(&mut self, webhooks: Vec<Webhook>) -> usize {
    self.webhooks = webhooks;
    self.persist();
    self.webhooks.len()
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
    format: WebhookFormat,
  ) -> Webhook {
    let hook = Webhook {
      id: uuid::Uuid::new_v4().to_string(),
      name,
      url,
      events,
      enabled: true,
      created_at: crate::store::tokens::now_secs(),
      format,
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

/// Outcome of delivering one event to one webhook (all attempts included).
/// Persisted in the `webhook_deliveries` table so operators can see which
/// deliveries succeeded or failed and redeliver any of them.
#[derive(Serialize, Deserialize, Clone, utoipa::ToSchema)]
pub struct Delivery {
  pub id: String,
  pub webhook_id: String,
  pub webhook_name: String,
  pub event: String,
  /// RFC3339 time of the first attempt.
  pub timestamp: String,
  pub success: bool,
  /// HTTP status of the last attempt (None = the request never completed).
  pub status: Option<u16>,
  /// Error text of the last attempt when it failed without a status.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub error: Option<String>,
  /// How many attempts were made (1 = delivered first try).
  pub attempts: u32,
  /// Milliseconds from the first attempt to the final outcome.
  pub duration_ms: u64,
  /// The exact payload that was sent, kept for redelivery (truncated to
  /// [`DELIVERY_BODY_CAP`] bytes for storage).
  pub body: String,
  /// Sort key: unix seconds of the first attempt.
  pub created_at: u64,
}

/// Largest payload persisted with a delivery record.
const DELIVERY_BODY_CAP: usize = 8 * 1024;
/// Delivery records kept (oldest pruned past this).
const DELIVERY_LOG_CAP: usize = 500;

/// Persistent log of webhook delivery outcomes (`webhook_deliveries` table).
pub struct DeliveryLog {
  conn: rusqlite::Connection,
  deliveries: Vec<Delivery>,
}

impl DeliveryLog {
  pub fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let mut deliveries: Vec<Delivery> = crate::store::load_all(&conn, "webhook_deliveries");
    deliveries.sort_by_key(|d| d.created_at);
    DeliveryLog { conn, deliveries }
  }

  fn persist(&mut self) {
    let rows: Vec<(String, String)> = self
      .deliveries
      .iter()
      .filter_map(|d| {
        serde_json::to_string(d)
          .ok()
          .map(|json| (d.id.clone(), json))
      })
      .collect();
    crate::store::replace_all(&mut self.conn, "webhook_deliveries", &rows);
  }

  pub fn record(&mut self, mut delivery: Delivery) {
    if delivery.body.len() > DELIVERY_BODY_CAP {
      delivery.body.truncate(DELIVERY_BODY_CAP);
    }
    self.deliveries.push(delivery);
    if self.deliveries.len() > DELIVERY_LOG_CAP {
      let excess = self.deliveries.len() - DELIVERY_LOG_CAP;
      self.deliveries.drain(0..excess);
    }
    self.persist();
  }

  /// Most recent deliveries first, optionally only one webhook's.
  pub fn list(&self, webhook_id: Option<&str>, limit: usize) -> Vec<Delivery> {
    self
      .deliveries
      .iter()
      .rev()
      .filter(|d| webhook_id.is_none_or(|id| d.webhook_id == id))
      .take(limit)
      .cloned()
      .collect()
  }

  pub fn get(&self, id: &str) -> Option<&Delivery> {
    self.deliveries.iter().find(|d| d.id == id)
  }
}

/// Delays between delivery attempts. Overridable for tests and impatient
/// operators via APERIO_WEBHOOK_RETRY_SCHEDULE (comma-separated seconds;
/// empty string = no retries). Attempt count = schedule length + 1.
pub(crate) fn retry_schedule() -> &'static [std::time::Duration] {
  use std::sync::OnceLock;
  static SCHEDULE: OnceLock<Vec<std::time::Duration>> = OnceLock::new();
  SCHEDULE.get_or_init(|| {
    match std::env::var("APERIO_WEBHOOK_RETRY_SCHEDULE") {
      Ok(raw) => raw
        .split(',')
        .filter_map(|part| {
          let part = part.trim();
          if part.is_empty() {
            None
          } else {
            part.parse::<u64>().ok().map(std::time::Duration::from_secs)
          }
        })
        .collect(),
      // Default: 4 retries over ~1.5 minutes (1s, 5s, 25s, 60s).
      Err(_) => vec![1, 5, 25, 60]
        .into_iter()
        .map(std::time::Duration::from_secs)
        .collect(),
    }
  })
}

/// Sends one webhook payload once. `Ok(status)` for a completed request of
/// any status; `Err(text)` when it never completed.
async fn send_once(hook: &Webhook, body: &str) -> Result<u16, String> {
  let client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(10))
    .build()
    .unwrap_or_default();
  let mut req = client
    .post(&hook.url)
    .header("content-type", "application/json");
  if let Some(ref secret) = hook.secret {
    let ts = crate::store::tokens::now_secs();
    let sig = sign_payload(secret, ts, body);
    req = req
      .header("x-aperio-timestamp", ts.to_string())
      .header("x-aperio-signature", format!("sha256={sig}"));
  }
  match req.body(body.to_string()).send().await {
    Ok(res) => Ok(res.status().as_u16()),
    Err(e) => Err(e.to_string()),
  }
}

/// True when an attempt outcome is worth retrying: transport errors, 5xx,
/// and 429. Other 4xx are permanent (a misconfigured receiver won't heal).
fn retryable(outcome: &Result<u16, String>) -> bool {
  match outcome {
    Ok(status) => *status >= 500 || *status == 429,
    Err(_) => true,
  }
}

/// Delivers one payload to one webhook with retries, then records the
/// outcome. Used by both the event dispatcher and manual redelivery.
pub(crate) async fn deliver_with_retries(
  hook: Webhook,
  event: String,
  body: String,
  log: std::sync::Arc<tokio::sync::Mutex<DeliveryLog>>,
) {
  let started = std::time::Instant::now();
  let timestamp = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
  let created_at = crate::store::tokens::now_secs();
  let mut attempts: u32 = 0;
  let mut outcome = Err("not attempted".to_string());
  for (i, delay) in std::iter::once(&std::time::Duration::ZERO)
    .chain(retry_schedule().iter())
    .enumerate()
  {
    if i > 0 {
      tokio::time::sleep(*delay).await;
    }
    attempts += 1;
    outcome = send_once(&hook, &body).await;
    match &outcome {
      Ok(status) if (200..300).contains(&(*status as u32)) => {
        debug!(
          "Webhook '{}' delivered event {} (attempt {})",
          hook.name, event, attempts
        );
        break;
      }
      _ if !retryable(&outcome) => break,
      Ok(status) => warn!(
        "Webhook '{}' returned {} for event {} (attempt {}); will retry",
        hook.name, status, event, attempts
      ),
      Err(e) => warn!(
        "Webhook '{}' delivery failed for event {} (attempt {}): {}; will retry",
        hook.name, event, attempts, e
      ),
    }
  }
  let success = matches!(&outcome, Ok(status) if (200..300).contains(&(*status as u32)));
  if !success {
    warn!(
      "Webhook '{}' delivery of event {} gave up after {} attempt(s)",
      hook.name, event, attempts
    );
  }
  log.lock().await.record(Delivery {
    id: uuid::Uuid::new_v4().to_string(),
    webhook_id: hook.id.clone(),
    webhook_name: hook.name.clone(),
    event,
    timestamp,
    success,
    status: outcome.as_ref().ok().copied(),
    error: outcome.err(),
    attempts,
    duration_ms: started.elapsed().as_millis() as u64,
    body,
    created_at,
  });
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

/// Renders `data`'s top-level fields as `key: value` bullet lines for the
/// chat-service formats. Non-object payloads become a single JSON line.
fn data_lines(data: &serde_json::Value, bullet: &str) -> String {
  match data.as_object() {
    Some(map) if !map.is_empty() => map
      .iter()
      .map(|(k, v)| {
        let val = match v {
          serde_json::Value::String(s) => s.clone(),
          other => other.to_string(),
        };
        format!("{bullet}{k}: {val}")
      })
      .collect::<Vec<_>>()
      .join("\n"),
    Some(_) => String::new(),
    None => format!("{bullet}{data}"),
  }
}

/// Builds the delivery body for one webhook: the raw event JSON for
/// `generic`, or a ready-made message for the chat service's
/// incoming-webhook endpoint.
pub(crate) fn render_payload(
  format: WebhookFormat,
  event: &str,
  timestamp: &str,
  data: &serde_json::Value,
) -> String {
  match format {
    WebhookFormat::Generic => serde_json::json!({
      "event": event,
      "timestamp": timestamp,
      "data": data,
    })
    .to_string(),
    WebhookFormat::Slack => {
      let lines = data_lines(data, "• ");
      let text = if lines.is_empty() {
        format!("*aperio* — `{event}`")
      } else {
        format!("*aperio* — `{event}`\n{lines}")
      };
      serde_json::json!({ "text": text }).to_string()
    }
    WebhookFormat::Discord => {
      let lines = data_lines(data, "- ");
      let content = if lines.is_empty() {
        format!("**aperio** — `{event}`")
      } else {
        format!("**aperio** — `{event}`\n{lines}")
      };
      serde_json::json!({ "content": content }).to_string()
    }
    WebhookFormat::Teams => {
      let facts: Vec<serde_json::Value> = data
        .as_object()
        .map(|map| {
          map
            .iter()
            .map(|(k, v)| {
              let val = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
              };
              serde_json::json!({ "name": k, "value": val })
            })
            .collect()
        })
        .unwrap_or_default();
      serde_json::json!({
        "@type": "MessageCard",
        "@context": "https://schema.org/extensions",
        "themeColor": "84cc16",
        "summary": format!("aperio: {event}"),
        "title": format!("aperio — {event}"),
        "sections": [{ "facts": facts, "text": timestamp }],
      })
      .to_string()
    }
  }
}

/// Background delivery of an event to all subscribed webhooks, with retries.
/// The default (`generic`) payload shape is
/// `{"event": "...", "timestamp": "...", "data": {...}}`; the chat formats
/// (`slack`/`discord`/`teams`) send a ready-made message instead.
/// Webhooks with a signing secret get `X-Aperio-Timestamp` and
/// `X-Aperio-Signature: sha256=<hex>` headers (see [`sign_payload`]) over the
/// exact body sent. Failed attempts are retried per [`retry_schedule`] (5xx,
/// 429, and transport errors only), and every final outcome is recorded in
/// the delivery log.
pub fn dispatch(
  subscribers: Vec<Webhook>,
  event: &str,
  data: serde_json::Value,
  log: std::sync::Arc<tokio::sync::Mutex<DeliveryLog>>,
) {
  if subscribers.is_empty() {
    return;
  }
  let timestamp = chrono::Local::now().to_rfc3339();
  for hook in subscribers {
    let body = render_payload(hook.format, event, &timestamp, &data);
    let event = event.to_string();
    let log = log.clone();
    tokio::spawn(async move {
      deliver_with_retries(hook, event, body, log).await;
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
      WebhookFormat::Generic,
    );
    assert_eq!(store.subscribers("client_connected").len(), 1);
    assert_eq!(store.subscribers("token_created").len(), 0);

    // Wildcard subscription
    store.create(
      "all".to_string(),
      "http://127.0.0.1:1/all".to_string(),
      vec!["*".to_string()],
      None,
      WebhookFormat::Generic,
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
  fn test_format_parse_and_persist() {
    assert_eq!(WebhookFormat::parse("slack"), Some(WebhookFormat::Slack));
    assert_eq!(WebhookFormat::parse(" TEAMS "), Some(WebhookFormat::Teams));
    assert_eq!(WebhookFormat::parse(""), Some(WebhookFormat::Generic));
    assert_eq!(WebhookFormat::parse("telegram"), None);

    let dir = std::env::temp_dir().join(format!("aperio-webhooks-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_string_lossy().to_string();
    let mut store = WebhookStore::load(&dir_str);
    store.create(
      "chat".to_string(),
      "http://127.0.0.1:1/hook".to_string(),
      vec![],
      None,
      WebhookFormat::Discord,
    );
    let reloaded = WebhookStore::load(&dir_str);
    assert_eq!(reloaded.list()[0].format, WebhookFormat::Discord);
    let _ = std::fs::remove_dir_all(&dir);

    // Rows persisted before the field existed deserialize as generic.
    let legacy: Webhook = serde_json::from_str(
      r#"{"id":"1","name":"old","url":"http://x","events":[],"enabled":true,"created_at":0}"#,
    )
    .unwrap();
    assert_eq!(legacy.format, WebhookFormat::Generic);
  }

  #[test]
  fn test_render_payload_formats() {
    let data = serde_json::json!({"client_id": "abc", "ip": "10.0.0.1"});
    let ts = "2026-01-01T00:00:00+00:00";

    let generic: serde_json::Value = serde_json::from_str(&render_payload(
      WebhookFormat::Generic,
      "client_connected",
      ts,
      &data,
    ))
    .unwrap();
    assert_eq!(generic["event"], "client_connected");
    assert_eq!(generic["data"]["client_id"], "abc");

    let slack: serde_json::Value = serde_json::from_str(&render_payload(
      WebhookFormat::Slack,
      "client_connected",
      ts,
      &data,
    ))
    .unwrap();
    let text = slack["text"].as_str().unwrap();
    assert!(text.contains("client_connected"), "got: {text}");
    assert!(text.contains("client_id: abc"), "got: {text}");

    let discord: serde_json::Value = serde_json::from_str(&render_payload(
      WebhookFormat::Discord,
      "client_connected",
      ts,
      &data,
    ))
    .unwrap();
    assert!(
      discord["content"]
        .as_str()
        .unwrap()
        .contains("ip: 10.0.0.1")
    );

    let teams: serde_json::Value = serde_json::from_str(&render_payload(
      WebhookFormat::Teams,
      "client_connected",
      ts,
      &data,
    ))
    .unwrap();
    assert_eq!(teams["@type"], "MessageCard");
    let facts = teams["sections"][0]["facts"].as_array().unwrap();
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[0]["name"], "client_id");
    assert_eq!(facts[0]["value"], "abc");
  }

  #[test]
  fn test_delivery_log_records_and_caps() {
    let dir = std::env::temp_dir().join(format!("aperio-deliveries-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_string_lossy().to_string();

    let mut log = DeliveryLog::load(&dir_str);
    let delivery = |i: u64, hook: &str, ok: bool| Delivery {
      id: format!("d{i}"),
      webhook_id: hook.to_string(),
      webhook_name: hook.to_string(),
      event: "client_connected".to_string(),
      timestamp: "2026-01-01T00:00:00+00:00".to_string(),
      success: ok,
      status: ok.then_some(200),
      error: (!ok).then(|| "connection refused".to_string()),
      attempts: if ok { 1 } else { 3 },
      duration_ms: 12,
      body: format!("{{\"n\":{i}}}"),
      created_at: i,
    };
    log.record(delivery(1, "a", true));
    log.record(delivery(2, "b", false));
    log.record(delivery(3, "a", true));

    // Newest first; per-webhook filter; lookup by id.
    let all = log.list(None, 10);
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].id, "d3");
    assert_eq!(log.list(Some("a"), 10).len(), 2);
    assert!(log.get("d2").unwrap().error.is_some());

    // Survives a reload.
    let reloaded = DeliveryLog::load(&dir_str);
    assert_eq!(reloaded.list(None, 10).len(), 3);

    // Oversized bodies are truncated at record time.
    let mut big = delivery(4, "a", true);
    big.body = "x".repeat(DELIVERY_BODY_CAP + 100);
    log.record(big);
    assert_eq!(log.get("d4").unwrap().body.len(), DELIVERY_BODY_CAP);

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn test_retryable_outcomes() {
    assert!(retryable(&Err("connection refused".to_string())));
    assert!(retryable(&Ok(500)));
    assert!(retryable(&Ok(503)));
    assert!(retryable(&Ok(429)));
    assert!(!retryable(&Ok(200)));
    assert!(!retryable(&Ok(404)));
    assert!(!retryable(&Ok(401)));
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
      WebhookFormat::Slack,
    );
    let reloaded = WebhookStore::load(&dir_str);
    assert_eq!(
      reloaded.list()[0].secret.as_deref(),
      Some("super-secret-key!")
    );

    let _ = std::fs::remove_dir_all(&dir);
  }
}
