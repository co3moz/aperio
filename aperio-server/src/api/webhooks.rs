use axum::{
  Json,
  extract::{ConnectInfo, State},
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
use crate::store::webhooks::{self};

/// Returns recent audit events (dashboard).
pub(crate) async fn audit_handler(
  State(state): State<Arc<AppState>>,
) -> Json<Vec<audit::AuditEvent>> {
  Json(state.audit.lock().await.recent())
}

/// Payload for creating a webhook definition.
#[derive(Deserialize)]
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
}

/// Lists webhook definitions. The signing secret itself is never returned —
/// only whether one is set.
pub(crate) async fn webhooks_list_handler(
  State(state): State<Arc<AppState>>,
) -> Json<Vec<serde_json::Value>> {
  let hooks = state.webhook_store.lock().await.list().to_vec();
  Json(
    hooks
      .into_iter()
      .map(|w| {
        serde_json::json!({
          "id": w.id,
          "name": w.name,
          "url": w.url,
          "events": w.events,
          "enabled": w.enabled,
          "created_at": w.created_at,
          "signed": w.secret.is_some(),
        })
      })
      .collect(),
  )
}

/// Creates a webhook definition. Only http/https URLs are accepted.
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

  let hook = state
    .webhook_store
    .lock()
    .await
    .create(name, url, events, secret);
  info!("Webhook created: {} -> {}", hook.name, hook.url);
  state
    .audit(
      "webhook_created",
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
  if state.webhook_store.lock().await.delete(&id) {
    state
      .audit("webhook_deleted", &actor_ip, &format!("id={}", id))
      .await;
    Json(serde_json::json!({"status": "ok"})).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Webhook not found").into_response()
  }
}
