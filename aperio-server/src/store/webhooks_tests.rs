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
  let hook = store
    .create(
      "notify".to_string(),
      "http://127.0.0.1:1/hook".to_string(),
      vec!["client_connected".to_string()],
      None,
      WebhookFormat::Generic,
      None,
    )
    .expect("the test store can be written to");
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
  assert!(store3.delete(&hook.id).is_ok());
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
  store
    .create(
      "chat".to_string(),
      "http://127.0.0.1:1/hook".to_string(),
      vec![],
      None,
      WebhookFormat::Discord,
      None,
    )
    .expect("the test store can be written to");
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
  store
    .create(
      "signed".to_string(),
      "http://127.0.0.1:1/hook".to_string(),
      vec![],
      Some("super-secret-key!".to_string()),
      WebhookFormat::Slack,
      None,
    )
    .expect("the test store can be written to");
  let reloaded = WebhookStore::load(&dir_str);
  assert_eq!(
    reloaded.list()[0].secret.as_deref(),
    Some("super-secret-key!")
  );

  let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Delivery: send_once, the retry policy, and the log it feeds.
// ---------------------------------------------------------------------------

/// What one receiver saw, kept per receiver rather than in one place.
type Received = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// A local receiver answering one canned status per connection, and the
/// record of what reached *it*.
///
/// The record used to be a single `static`, shared by every receiver every
/// test in this file spawns. Three of them use this helper, so a test
/// asserting its own receiver saw nothing was really asserting that no
/// webhook test anywhere in the binary had delivered while it ran. Under the
/// full suite that is false often enough to fail, and it fails by accusing
/// the outbound fence of following a redirect it never followed: a security
/// claim disproved by a scheduling accident. It went the other way too, a
/// `clear()` between another test's delivery and its assertion would have
/// lost the headers it was about to check.
async fn canned_receiver(status: u16) -> (std::net::SocketAddr, Received) {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let received: Received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
  let sink = received.clone();
  tokio::spawn(async move {
    loop {
      let Ok((mut socket, _)) = listener.accept().await else {
        return;
      };
      let sink = sink.clone();
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await;
        let head = String::from_utf8_lossy(&buf).to_string();
        let response =
          format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        let _ = socket.write_all(response.as_bytes()).await;
        // Keep what arrived observable for the signature assertion.
        sink.lock().unwrap().push(head);
      });
    }
  });
  (addr, received)
}

fn hook_to(addr: std::net::SocketAddr, secret: Option<&str>) -> Webhook {
  Webhook {
    id: uuid::Uuid::new_v4().to_string(),
    name: "test-hook".to_string(),
    url: format!("http://{addr}/hook"),
    events: vec!["*".to_string()],
    enabled: true,
    created_at: 0,
    format: WebhookFormat::default(),
    secret: secret.map(str::to_string),
    org_id: None,
  }
}

#[tokio::test]
async fn a_delivery_succeeds_and_carries_the_signature_headers() {
  // Pin the retry schedule before any delivery initializes the OnceLock:
  // whatever it holds, this test's cases never retry (200 and 4xx).
  let (addr, received) = canned_receiver(200).await;
  let log = std::sync::Arc::new(tokio::sync::Mutex::new(DeliveryLog::load(
    &crate::test_support::test_temp_root()
      .join(format!("wh-{}", uuid::Uuid::new_v4()))
      .to_string_lossy(),
  )));
  deliver_with_retries(
    hook_to(addr, Some("signing-secret")),
    "client_connected".to_string(),
    "{\"event\":\"client_connected\"}".to_string(),
    log.clone(),
    crate::outbound::OutboundPolicy::default(),
  )
  .await;

  let deliveries = log.lock().await;
  let list = deliveries.list(None, 10);
  assert_eq!(list.len(), 1);
  assert!(list[0].success);
  assert_eq!(list[0].status, Some(200));
  assert_eq!(list[0].attempts, 1);
  let received = received.lock().unwrap().join("\n");
  assert!(
    received.contains("x-aperio-signature: sha256="),
    "the signed delivery announces its MAC: {received}"
  );
  assert!(received.contains("x-aperio-timestamp:"), "{received}");
}

#[tokio::test]
async fn a_permanent_refusal_is_not_retried() {
  let (addr, _received) = canned_receiver(404).await;
  let log = std::sync::Arc::new(tokio::sync::Mutex::new(DeliveryLog::load(
    &crate::test_support::test_temp_root()
      .join(format!("wh-{}", uuid::Uuid::new_v4()))
      .to_string_lossy(),
  )));
  deliver_with_retries(
    hook_to(addr, None),
    "client_connected".to_string(),
    "{}".to_string(),
    log.clone(),
    crate::outbound::OutboundPolicy::default(),
  )
  .await;
  let deliveries = log.lock().await;
  let list = deliveries.list(None, 10);
  assert_eq!(list.len(), 1);
  assert!(!list[0].success);
  assert_eq!(list[0].status, Some(404));
  assert_eq!(list[0].attempts, 1, "a 404 receiver will not heal; one try");
}

#[tokio::test]
async fn an_outbound_policy_refusal_never_contacts_the_receiver() {
  // The policy check happens at delivery time, so it covers webhooks stored
  // before the policy existed. The URL is internal and blocked; nothing
  // listens there, and nothing must try.
  let log = std::sync::Arc::new(tokio::sync::Mutex::new(DeliveryLog::load(
    &crate::test_support::test_temp_root()
      .join(format!("wh-{}", uuid::Uuid::new_v4()))
      .to_string_lossy(),
  )));
  let hook = Webhook {
    id: "h1".to_string(),
    name: "internal".to_string(),
    url: "http://127.0.0.1:9/hook".to_string(),
    events: vec!["*".to_string()],
    enabled: true,
    created_at: 0,
    format: WebhookFormat::default(),
    secret: None,
    org_id: None,
  };
  deliver_with_retries(
    hook,
    "client_connected".to_string(),
    "{}".to_string(),
    log.clone(),
    crate::outbound::OutboundPolicy {
      allowlist: Vec::new(),
      block_private: true,
      egress: Default::default(),
    },
  )
  .await;
  let deliveries = log.lock().await;
  let list = deliveries.list(None, 10);
  assert_eq!(list.len(), 1);
  assert!(!list[0].success);
  assert_eq!(list[0].attempts, 0, "refused before any attempt");
  assert!(
    list[0]
      .error
      .as_deref()
      .unwrap_or_default()
      .contains("internal address"),
    "{:?}",
    list[0].error
  );
}

#[test]
fn the_default_retry_schedule_is_the_documented_one() {
  // First toucher wins the OnceLock; whatever test got there first, the
  // schedule is a fixed list this asserts the shape of.
  let schedule = retry_schedule();
  assert!(!schedule.is_empty());
  assert!(schedule.iter().all(|d| d.as_secs() >= 1));
}

/// A receiver that redirects everything it is asked to `target`.
async fn redirecting_receiver(target: &str) -> std::net::SocketAddr {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let target = target.to_string();
  tokio::spawn(async move {
    loop {
      let Ok((mut socket, _)) = listener.accept().await else {
        return;
      };
      let target = target.clone();
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await;
        let response = format!(
          "HTTP/1.1 302 Found\r\nlocation: {target}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        let _ = socket.write_all(response.as_bytes()).await;
      });
    }
  });
  addr
}

#[tokio::test]
async fn a_redirect_does_not_carry_a_delivery_past_the_outbound_policy() {
  // The policy is checked against the URL that was stored, at the moment of
  // delivery. If the transport then follows a `Location`, the destination the
  // policy actually vetted is not the destination that gets the request, and
  // an allowed receiver can point the server at anything: the metadata
  // service, something on the loopback, whatever the fence exists to refuse.
  let (internal, internal_saw) = canned_receiver(200).await;
  let redirector = redirecting_receiver(&format!("http://{internal}/internal")).await;

  let status = send_once(&hook_to(redirector, None), "{}").await;

  // The redirect itself is the answer, and the place it pointed at was never
  // asked. Following it would report 200 here and leave a request in the log
  // of a host nobody vetted.
  assert_eq!(status, Ok(302), "the redirect must not be followed");
  assert!(
    internal_saw.lock().unwrap().is_empty(),
    "the redirect target received a request"
  );
}
