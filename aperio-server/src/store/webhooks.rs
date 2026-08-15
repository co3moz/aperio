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
  /// Slack incoming-webhook message: a coloured attachment (card with fields).
  Slack,
  /// Discord webhook message: a rich embed (coloured card with title + fields).
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
  /// Organization that owns this webhook (`None` = the implicit master org).
  /// Deliveries fire only for events in the same organization, and the webhook
  /// is visible/manageable only within it.
  #[serde(default)]
  pub org_id: Option<String>,
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
  /// Bookkeeping: the dump-restore path, whose caller reports on the whole
  /// import rather than on one row. See `store::replace_all`.
  pub fn import(&mut self, webhooks: Vec<Webhook>) -> usize {
    self.webhooks = webhooks;
    self.persist();
    self.webhooks.len()
  }

  /// Rewrites the webhooks table. Returns whether the write succeeded.
  fn persist(&mut self) -> bool {
    let rows: Vec<(String, String)> = self
      .webhooks
      .iter()
      .filter_map(|w| {
        serde_json::to_string(w)
          .ok()
          .map(|json| (w.id.clone(), json))
      })
      .collect();
    crate::store::replace_all(&mut self.conn, "webhooks", &rows)
  }

  #[allow(clippy::too_many_arguments)]
  pub fn create(
    &mut self,
    name: String,
    url: String,
    events: Vec<String>,
    secret: Option<String>,
    format: WebhookFormat,
    org_id: Option<String>,
  ) -> Option<Webhook> {
    let hook = Webhook {
      id: uuid::Uuid::new_v4().to_string(),
      name,
      url,
      events,
      enabled: true,
      created_at: crate::store::tokens::now_secs(),
      format,
      secret,
      org_id,
    };
    self.webhooks.push(hook.clone());
    if !self.persist() {
      // The same reasoning as `delete` below: a webhook that is not written
      // down stops existing at the next restart, and the operator was told it
      // was created and will be waiting for deliveries from it.
      self.webhooks.pop();
      return None;
    }
    Some(hook)
  }

  /// Deletes a webhook by id. `Ok` only when it was removed *and* durably
  /// persisted; on a write failure the removal is reverted, so a deleted
  /// webhook cannot silently reappear on restart.
  ///
  /// The failure is named rather than folded into "no such webhook": one is a
  /// 404 and the other a 500, and a hook that keeps delivering after the
  /// dashboard reported it gone is worth telling the operator about.
  pub fn delete(&mut self, id: &str) -> Result<(), crate::store::NotWritten> {
    let Some(pos) = self.webhooks.iter().position(|w| w.id == id) else {
      return Err(crate::store::NotWritten::NoSuchRecord);
    };
    let removed = self.webhooks.remove(pos);
    if self.persist() {
      Ok(())
    } else {
      self.webhooks.insert(pos, removed);
      Err(crate::store::NotWritten::NotPersisted)
    }
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
  /// Organization of the webhook this delivery was sent to (`None` = master).
  #[serde(default)]
  pub org_id: Option<String>,
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

  /// Bookkeeping: a failed write is logged, not rolled back. See
  /// `store::replace_all`.
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

  /// Disk guard: drops the oldest deliveries so at most `keep` remain.
  /// Returns removed count.
  /// Bookkeeping, and it runs *because* space is short. See
  /// `store::replace_all`.
  pub fn truncate_oldest(&mut self, keep: usize) -> usize {
    if self.deliveries.len() <= keep {
      return 0;
    }
    let excess = self.deliveries.len() - keep;
    self.deliveries.drain(0..excess);
    self.persist();
    excess
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
  // Fail rather than fall back to a default client: the default has no
  // timeout at all, so a receiver that accepts the connection and never
  // answers would hang this delivery task forever.
  //
  // **Redirects are not followed.** The outbound policy is checked against
  // the URL that was stored, and following a `Location` would mean the
  // destination it vetted is not the destination that gets the request: an
  // allowed receiver could answer `302` and point the server at the metadata
  // service, at something on the loopback, at whatever the fence exists to
  // refuse. A redirect is reported as the status it is, which is also more
  // useful to whoever configured a webhook against a URL that moved.
  let client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(10))
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .map_err(|e| format!("cannot build the http client: {e}"))?;
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
  policy: crate::outbound::OutboundPolicy,
) {
  let started = std::time::Instant::now();
  let timestamp = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
  let created_at = crate::store::tokens::now_secs();
  let mut attempts: u32 = 0;
  let mut outcome = Err("not attempted".to_string());
  // The outbound policy is enforced when the call is made, not only when
  // the URL was stored, so a policy added later also covers webhooks
  // created before it. A refusal is recorded like any failed delivery
  // (without ever contacting the destination) and is not retried.
  if let Err(reason) = policy.check(&hook.url).await {
    warn!(
      "Webhook '{}' delivery of event {} refused by the outbound policy: {}",
      hook.name, event, reason
    );
    outcome = Err(reason);
  } else {
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
    org_id: hook.org_id.clone(),
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

/// The event payload's top-level fields as `(key, value)` string pairs, for the
/// chat-service cards. A non-object payload becomes a single `data` entry.
/// Empty values become a dash so a card field is never blank (Discord rejects
/// an empty field value outright).
fn data_entries(data: &serde_json::Value) -> Vec<(String, String)> {
  let stringify = |v: &serde_json::Value| -> String {
    let s = match v {
      serde_json::Value::String(s) => s.clone(),
      other => other.to_string(),
    };
    if s.is_empty() { ", ".to_string() } else { s }
  };
  match data.as_object() {
    Some(map) => map.iter().map(|(k, v)| (k.clone(), stringify(v))).collect(),
    None => vec![("data".to_string(), stringify(data))],
  }
}

/// Turns an event slug into a human title: `client_connected` → `Client
/// connected`.
fn pretty_event(event: &str) -> String {
  let spaced = event.replace('_', " ");
  let mut chars = spaced.chars();
  match chars.next() {
    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    None => spaced,
  }
}

/// Card colour (hex RGB, no `#`) encoding an event's nature, shared by every
/// chat format: green for a good/recovered state, red for a failure, amber for
/// something needing attention, and a neutral blue for everything else. New
/// events fall through to neutral.
fn event_hex(event: &str) -> &'static str {
  match event {
    "client_connected" | "alert_resolved" | "maintenance_off" | "tunnel_created"
    | "share_created" | "token_created" | "db_backup" | "import_applied" => "2ecc71",
    "client_disconnected"
    | "alert_triggered"
    | "canary_tripped"
    | "token_revoked"
    | "token_pin_mismatch" => "e74c3c",
    "client_draining" | "maintenance_on" | "token_expiring" | "token_new_ip" | "org_usage" => {
      "f1c40f"
    }
    _ => "5865f2",
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
      // A coloured attachment (the Slack analogue of a Discord embed): a card
      // whose left bar encodes the event's nature, titled with the event, with
      // each event field shown as a short attachment field.
      let fields: Vec<serde_json::Value> = data_entries(data)
        .into_iter()
        .map(|(k, v)| serde_json::json!({ "title": k, "value": v, "short": true }))
        .collect();
      serde_json::json!({
        "attachments": [{
          "color": format!("#{}", event_hex(event)),
          "title": pretty_event(event),
          "fields": fields,
          "footer": "aperio",
        }],
      })
      .to_string()
    }
    WebhookFormat::Discord => {
      // A rich embed rather than a plain-text line: a coloured card titled with
      // the event, one embed field per event data entry.
      let fields: Vec<serde_json::Value> = data_entries(data)
        .into_iter()
        .map(|(k, v)| serde_json::json!({ "name": k, "value": v, "inline": true }))
        .collect();
      let color = u32::from_str_radix(event_hex(event), 16).unwrap_or(0x5865f2);
      serde_json::json!({
        "username": "aperio",
        "embeds": [{
          "title": pretty_event(event),
          "color": color,
          "fields": fields,
          "timestamp": timestamp,
        }],
      })
      .to_string()
    }
    WebhookFormat::Teams => {
      // A MessageCard whose theme colour now tracks the event's nature (it was
      // a fixed green for every event), titled with the event.
      let facts: Vec<serde_json::Value> = data_entries(data)
        .into_iter()
        .map(|(k, v)| serde_json::json!({ "name": k, "value": v }))
        .collect();
      serde_json::json!({
        "@type": "MessageCard",
        "@context": "https://schema.org/extensions",
        "themeColor": event_hex(event),
        "summary": format!("aperio: {event}"),
        "title": pretty_event(event),
        "sections": [{ "facts": facts, "text": timestamp }],
      })
      .to_string()
    }
  }
}

/// Background delivery of an event to all subscribed webhooks, with retries.
/// The default (`generic`) payload shape is
/// `{"event": "...", "timestamp": "...", "data": {...}}`; the chat formats
/// (`slack`/`discord`/`teams`) send a ready-made coloured card instead.
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
  policy: crate::outbound::OutboundPolicy,
) {
  if subscribers.is_empty() {
    return;
  }
  let timestamp = chrono::Local::now().to_rfc3339();
  for hook in subscribers {
    let body = render_payload(hook.format, event, &timestamp, &data);
    let event = event.to_string();
    let log = log.clone();
    let policy = policy.clone();
    tokio::spawn(async move {
      deliver_with_retries(hook, event, body, log, policy).await;
    });
  }
}

#[cfg(test)]
#[path = "webhooks_tests.rs"]
mod tests;

/// Sends one synthetic event to a webhook and reports what happened, for the
/// dashboard's "test fire" (planned_features #39).
///
/// The same path a real event takes, so what it proves is what an operator
/// wants proven: the outbound policy check, the signature, the same client and
/// timeout, and a row in the delivery log like any other. Two deliberate
/// differences from `deliver_with_retries`:
///
/// * **One attempt.** The operator is waiting for the answer. Retrying for a
///   minute and a half while they watch would report a success that took four
///   tries as if it were a success, and hide the first failure that is the
///   thing they are testing for.
/// * **The outcome is returned**, not only logged, because the point is to see
///   it now rather than to go and look for it.
pub(crate) async fn deliver_test(
  hook: Webhook,
  body: String,
  log: std::sync::Arc<tokio::sync::Mutex<DeliveryLog>>,
  policy: crate::outbound::OutboundPolicy,
) -> Delivery {
  let started = std::time::Instant::now();
  let outcome = match policy.check(&hook.url).await {
    Err(reason) => Err(reason),
    Ok(()) => send_once(&hook, &body).await,
  };
  let success = matches!(&outcome, Ok(status) if (200..300).contains(&(*status as u32)));
  let delivery = Delivery {
    id: uuid::Uuid::new_v4().to_string(),
    webhook_id: hook.id.clone(),
    webhook_name: hook.name.clone(),
    org_id: hook.org_id.clone(),
    event: TEST_EVENT.to_string(),
    timestamp: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
    success,
    status: outcome.as_ref().ok().copied(),
    error: outcome.err(),
    attempts: 1,
    duration_ms: started.elapsed().as_millis() as u64,
    body,
    created_at: crate::store::tokens::now_secs(),
  };
  log.lock().await.record(delivery.clone());
  delivery
}

/// Event name of a test delivery. Distinct from every real event so a
/// receiver can ignore it, and so the delivery log does not claim something
/// happened that did not.
pub(crate) const TEST_EVENT: &str = "webhook_test";
