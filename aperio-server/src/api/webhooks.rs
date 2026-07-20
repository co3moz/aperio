use axum::{
  Json,
  extract::{ConnectInfo, Path, Query, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::routing::extract_client_ip;
use crate::state::AppState;
use crate::store::audit::{self};

/// Returns recent audit events (dashboard), scoped to the caller's effective
/// organization: a named user sees only their org's events, and the master
/// super-admin sees the events of whichever org is selected on their session
/// (`None` = the implicit master org, which also owns server-global events).
#[utoipa::path(get, path = "/aperio/api/audit", tag = "dashboard",
  description = "The most recent audit events for the caller's organization (bounded ring buffer; the durable log is audit.jsonl).",
  responses((status = 200, description = "Recent audit events", body = Vec<audit::AuditEvent>)))]
pub(crate) async fn audit_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Json<Vec<audit::AuditEvent>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let events = state
    .audit
    .lock()
    .await
    .recent()
    .into_iter()
    .filter(|e| e.org_id == org)
    .collect();
  Json(events)
}

/// Verifies the tamper-evident hash chain of the audit log (active file plus
/// rotated generations). Returns `{ok, broken: [{file, line}]}`. Not org-scoped
/// — the audit files are server-global, so this is an admin-only integrity
/// check surfaced from the dashboard.
#[utoipa::path(get, path = "/aperio/api/audit/verify", tag = "dashboard",
  description = "Verifies the audit log hash chain across all files; reports any broken line.",
  responses((status = 200, description = "Chain verification result", body = serde_json::Value)))]
pub(crate) async fn audit_verify_handler(
  State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
  let broken = state.audit.lock().await.verify();
  let broken_json: Vec<serde_json::Value> = broken
    .into_iter()
    .map(|(file, line)| serde_json::json!({"file": file, "line": line}))
    .collect();
  Json(serde_json::json!({
    "ok": broken_json.is_empty(),
    "broken": broken_json,
  }))
}

/// Payload for creating a webhook definition.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct WebhookCreateRequest {
  pub(crate) name: String,
  pub(crate) url: String,
  /// Subscribed events; `["*"]` (or empty) = all events.
  #[serde(default)]
  pub(crate) events: Vec<String>,
  /// Optional HMAC signing secret; deliveries then carry
  /// `X-Aperio-Signature` / `X-Aperio-Timestamp` headers.
  #[serde(default)]
  pub(crate) secret: Option<String>,
  /// Delivery payload format: `generic` (default), `slack`, `discord`, or `teams`.
  #[serde(default)]
  pub(crate) format: Option<String>,
}

/// Lists webhook definitions. The signing secret itself is never returned —
/// only whether one is set.
#[utoipa::path(get, path = "/aperio/api/webhooks", tag = "webhooks",
  description = "Lists webhook definitions (signing secrets are never exposed, only a signed flag).",
  responses((status = 200, description = "Webhook definitions", body = serde_json::Value)))]
pub(crate) async fn webhooks_list_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Json<Vec<serde_json::Value>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let hooks = state.webhook_store.lock().await.list().to_vec();
  Json(
    hooks
      .into_iter()
      .filter(|w| w.org_id == org)
      .map(|w| {
        serde_json::json!({
          "id": w.id,
          "name": w.name,
          "url": w.url,
          "events": w.events,
          "enabled": w.enabled,
          "created_at": w.created_at,
          "format": w.format.as_str(),
          "signed": w.secret.is_some(),
        })
      })
      .collect(),
  )
}

/// Creates a webhook definition. Only http/https URLs are accepted.
#[utoipa::path(post, path = "/aperio/api/webhooks", tag = "webhooks",
  description = "Creates a webhook; an optional HMAC signing secret (16-128 chars) enables signed deliveries.",
  request_body = WebhookCreateRequest,
  responses((status = 200, description = "Created webhook", body = serde_json::Value), (status = 400, description = "Invalid URL/secret")))]
pub(crate) async fn webhooks_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<WebhookCreateRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  let name = payload.name.trim().to_string();
  if name.is_empty() || name.len() > 64 {
    return (
      StatusCode::BAD_REQUEST,
      "Webhook name must be 1-64 characters",
    )
      .into_response();
  }
  let url = payload.url.trim().to_string();
  if !(url.starts_with("http://") || url.starts_with("https://")) {
    return (StatusCode::BAD_REQUEST, "Webhook URL must be http(s)").into_response();
  }
  let events: Vec<String> = payload
    .events
    .iter()
    .map(|e| e.trim().to_string())
    .filter(|e| !e.is_empty())
    .collect();
  let secret = payload
    .secret
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_string);
  if secret
    .as_deref()
    .is_some_and(|s| s.len() < 16 || s.len() > 128)
  {
    return (
      StatusCode::BAD_REQUEST,
      "Webhook signing secret must be 16-128 characters",
    )
      .into_response();
  }
  let Some(format) =
    crate::store::webhooks::WebhookFormat::parse(payload.format.as_deref().unwrap_or(""))
  else {
    return (
      StatusCode::BAD_REQUEST,
      "Webhook format must be generic, slack, discord, or teams",
    )
      .into_response();
  };

  // New webhooks belong to the caller's effective organization and fire only
  // for that org's events.
  let org = crate::auth::effective_org(&state, &headers).await;
  let hook = state
    .webhook_store
    .lock()
    .await
    .create(name, url, events, secret, format, org);
  info!("Webhook created: {} -> {}", hook.name, hook.url);
  state
    .audit_session(
      "webhook_created",
      &headers,
      &actor_ip,
      &format!(
        "name={} url={} events={:?}",
        hook.name, hook.url, hook.events
      ),
    )
    .await;
  Json(serde_json::json!({"status": "ok", "id": hook.id})).into_response()
}

/// Deletes a webhook definition.
#[utoipa::path(delete, path = "/aperio/api/webhooks/{id}", tag = "webhooks",
  description = "Deletes a webhook definition.",
  params(("id" = String, Path, description = "Webhook id")),
  responses((status = 200, description = "Deleted"), (status = 404, description = "Unknown id")))]
pub(crate) async fn webhooks_delete_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  // Isolation: only webhooks in the caller's effective org may be deleted.
  let org = crate::auth::effective_org(&state, &headers).await;
  let in_org = state
    .webhook_store
    .lock()
    .await
    .list()
    .iter()
    .any(|w| w.id == id && w.org_id == org);
  if !in_org {
    return (StatusCode::NOT_FOUND, "Webhook not found").into_response();
  }
  if state.webhook_store.lock().await.delete(&id) {
    state
      .audit_session(
        "webhook_deleted",
        &headers,
        &actor_ip,
        &format!("id={}", id),
      )
      .await;
    Json(serde_json::json!({"status": "ok"})).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Webhook not found").into_response()
  }
}

/// Query of the delivery-log listing.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct DeliveriesQuery {
  /// Only this webhook's deliveries.
  pub(crate) webhook_id: Option<String>,
  /// Most-recent rows to return (default 50, max 200).
  pub(crate) limit: Option<usize>,
}

/// Lists recent webhook delivery outcomes, newest first.
#[utoipa::path(get, path = "/aperio/api/webhooks/deliveries", tag = "webhooks",
  description = "Recent webhook delivery outcomes (attempts, status, payload), newest first.",
  responses((status = 200, description = "Delivery log", body = Vec<crate::store::webhooks::Delivery>)))]
pub(crate) async fn webhook_deliveries_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Query(q): Query<DeliveriesQuery>,
) -> Json<Vec<crate::store::webhooks::Delivery>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let limit = q.limit.unwrap_or(50).min(200);
  // Fetch a wider window, then keep only this org's deliveries up to the limit.
  let rows = state
    .webhook_deliveries
    .lock()
    .await
    .list(q.webhook_id.as_deref(), 500);
  Json(
    rows
      .into_iter()
      .filter(|d| d.org_id == org)
      .take(limit)
      .collect(),
  )
}

/// Re-sends a logged delivery's exact payload to its webhook.
#[utoipa::path(post, path = "/aperio/api/webhooks/deliveries/{id}/redeliver", tag = "webhooks",
  description = "Queues a redelivery of the logged payload to the webhook's current URL (fresh signature, normal retry policy); the outcome lands in the delivery log as a new row.",
  responses(
    (status = 202, description = "Redelivery queued"),
    (status = 404, description = "Unknown delivery or deleted webhook")))]
pub(crate) async fn webhook_redeliver_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
) -> Response {
  let Some(delivery) = state.webhook_deliveries.lock().await.get(&id).cloned() else {
    return (StatusCode::NOT_FOUND, "unknown delivery id").into_response();
  };
  // Isolation: only deliveries in the caller's effective org may be redelivered.
  let org = crate::auth::effective_org(&state, &headers).await;
  if delivery.org_id != org {
    return (StatusCode::NOT_FOUND, "unknown delivery id").into_response();
  }
  let Some(hook) = state
    .webhook_store
    .lock()
    .await
    .list()
    .iter()
    .find(|w| w.id == delivery.webhook_id)
    .cloned()
  else {
    return (
      StatusCode::NOT_FOUND,
      "the webhook this delivery belonged to no longer exists",
    )
      .into_response();
  };
  let ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  state
    .audit_session(
      "webhook_redelivered",
      &headers,
      &ip.to_string(),
      &format!("webhook={} event={}", hook.name, delivery.event),
    )
    .await;
  info!(
    "Redelivering event {} to webhook '{}' on operator request",
    delivery.event, hook.name
  );
  let log = state.webhook_deliveries.clone();
  tokio::spawn(async move {
    crate::store::webhooks::deliver_with_retries(hook, delivery.event, delivery.body, log).await;
  });
  (
    StatusCode::ACCEPTED,
    Json(serde_json::json!({"queued": true})),
  )
    .into_response()
}
