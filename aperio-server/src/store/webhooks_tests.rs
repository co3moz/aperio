//! Tests for the webhook store: subscriptions, delivery bookkeeping, and
//! the retry state that decides whether an endpoint is still worth calling.

use super::*;

#[test]
fn test_store_and_subscription() {
  let dir =
    crate::test_support::test_temp_root().join(format!("webhooks-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let dir_str = dir.to_string_lossy().to_string();

  let mut store = WebhookStore::load(&dir_str);
  let hook = store.create(
    "notify".to_string(),
    "http://127.0.0.1:1/hook".to_string(),
    vec!["client_connected".to_string()],
    None,
    WebhookFormat::Generic,
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
    WebhookFormat::Generic,
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
fn test_format_parse_and_persist() {
  assert_eq!(WebhookFormat::parse("slack"), Some(WebhookFormat::Slack));
  assert_eq!(WebhookFormat::parse(" TEAMS "), Some(WebhookFormat::Teams));
  assert_eq!(WebhookFormat::parse(""), Some(WebhookFormat::Generic));
  assert_eq!(WebhookFormat::parse("telegram"), None);

  let dir =
    crate::test_support::test_temp_root().join(format!("webhooks-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let dir_str = dir.to_string_lossy().to_string();
  let mut store = WebhookStore::load(&dir_str);
  store.create(
    "chat".to_string(),
    "http://127.0.0.1:1/hook".to_string(),
    vec![],
    None,
    WebhookFormat::Discord,
    None,
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
  let att = &slack["attachments"][0];
  assert_eq!(att["title"], "Client connected");
  assert_eq!(att["color"], "#2ecc71"); // green for a "connected" event
  let sfields = att["fields"].as_array().unwrap();
  assert_eq!(sfields.len(), 2);
  assert!(
    sfields
      .iter()
      .any(|f| f["title"] == "client_id" && f["value"] == "abc"),
    "got: {sfields:?}"
  );

  let discord: serde_json::Value = serde_json::from_str(&render_payload(
    WebhookFormat::Discord,
    "client_connected",
    ts,
    &data,
  ))
  .unwrap();
  // A rich embed: title from the event, colour by nature, one field per datum.
  let embed = &discord["embeds"][0];
  assert_eq!(embed["title"], "Client connected");
  assert_eq!(embed["color"], 0x2ecc71); // green for a "connected" event
  assert_eq!(embed["timestamp"], ts);
  let fields = embed["fields"].as_array().unwrap();
  assert_eq!(fields.len(), 2);
  let ip = fields
    .iter()
    .find(|f| f["name"] == "ip")
    .expect("ip field present");
  assert_eq!(ip["value"], "10.0.0.1");
  // A failure event gets the red colour instead.
  let down: serde_json::Value = serde_json::from_str(&render_payload(
    WebhookFormat::Discord,
    "client_disconnected",
    ts,
    &data,
  ))
  .unwrap();
  assert_eq!(down["embeds"][0]["color"], 0xe74c3c);

  let teams: serde_json::Value = serde_json::from_str(&render_payload(
    WebhookFormat::Teams,
    "client_connected",
    ts,
    &data,
  ))
  .unwrap();
  assert_eq!(teams["@type"], "MessageCard");
  assert_eq!(teams["title"], "Client connected");
  assert_eq!(teams["themeColor"], "2ecc71"); // green, no longer a fixed colour
  let facts = teams["sections"][0]["facts"].as_array().unwrap();
  assert_eq!(facts.len(), 2);
  assert_eq!(facts[0]["name"], "client_id");
  assert_eq!(facts[0]["value"], "abc");
}

#[test]
fn test_delivery_log_records_and_caps() {
  let dir =
    crate::test_support::test_temp_root().join(format!("deliveries-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let dir_str = dir.to_string_lossy().to_string();

  let mut log = DeliveryLog::load(&dir_str);
  let delivery = |i: u64, hook: &str, ok: bool| Delivery {
    id: format!("d{i}"),
    webhook_id: hook.to_string(),
    webhook_name: hook.to_string(),
    org_id: None,
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
  let dir =
    crate::test_support::test_temp_root().join(format!("webhooks-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let dir_str = dir.to_string_lossy().to_string();

  let mut store = WebhookStore::load(&dir_str);
  store.create(
    "signed".to_string(),
    "http://127.0.0.1:1/hook".to_string(),
    vec![],
    Some("super-secret-key!".to_string()),
    WebhookFormat::Slack,
    None,
  );
  let reloaded = WebhookStore::load(&dir_str);
  assert_eq!(
    reloaded.list()[0].secret.as_deref(),
    Some("super-secret-key!")
  );

  let _ = std::fs::remove_dir_all(&dir);
}
