//! Webhook inbox API: browse, delete, and re-fire the inbound third-party
//! webhooks persisted for services that opted in with `webhook_inbox: true`.

use axum::{
  Json,
  extract::{ConnectInfo, State, ws::Message},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::oneshot;
use tracing::info;

use crate::protocol::TunnelMessage;
use crate::routing::{apply_lb_strategy, extract_client_ip, select_client_pool};
use crate::state::{AppState, PendingRequest, TunnelResponse};

/// Inbox list row: everything except the payload.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct InboxSummary {
  pub(crate) id: String,
  pub(crate) timestamp: String,
  pub(crate) method: String,
  pub(crate) uri: String,
  pub(crate) host: Option<String>,
  pub(crate) status: u16,
  pub(crate) service: Option<String>,
  /// Decoded body size in bytes (0 = no body captured).
  pub(crate) body_bytes: usize,
  pub(crate) body_truncated: bool,
}

/// Lists the webhook inbox (newest first, payloads omitted).
#[utoipa::path(get, path = "/aperio/api/inbox", tag = "dashboard",
  description = "Inbound webhooks persisted for services with webhook_inbox: true (newest first, payloads omitted).",
  responses((status = 200, description = "Inbox entries", body = Vec<InboxSummary>)))]
pub(crate) async fn inbox_list_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Json<Vec<InboxSummary>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let store = state.inbox_store.lock().await;
  let rows = store
    .list(&org)
    .into_iter()
    .map(|e| InboxSummary {
      id: e.id.clone(),
      timestamp: e.timestamp.clone(),
      method: e.method.clone(),
      uri: e.uri.clone(),
      host: e.host.clone(),
      status: e.status,
      service: e.service.clone(),
      body_bytes: e.body.as_deref().map(|b| b.len() * 3 / 4).unwrap_or(0),
      body_truncated: e.body_truncated,
    })
    .collect();
  Json(rows)
}

/// One inbox entry with its full (redacted) headers and body.
#[utoipa::path(get, path = "/aperio/api/inbox/{id}", tag = "dashboard",
  description = "One webhook inbox entry with redacted headers and payload.",
  params(("id" = String, Path, description = "Inbox entry id")),
  responses((status = 200, description = "Entry detail", body = serde_json::Value), (status = 404, description = "Unknown entry")))]
pub(crate) async fn inbox_detail_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  headers: HeaderMap,
) -> Response {
  let org = crate::auth::effective_org(&state, &headers).await;
  let store = state.inbox_store.lock().await;
  let Some(entry) = store.get(&id, &org) else {
    return (StatusCode::NOT_FOUND, "Inbox entry not found").into_response();
  };
  // Redacted like the request inspector: credential headers and secret body
  // fields are masked in the view while the raw capture backs the re-fire.
  let (view_headers, view_body) = if crate::redact::redaction_enabled() {
    (
      crate::redact::redact_headers(&entry.headers),
      entry.body.as_deref().map(crate::redact::redact_body_b64),
    )
  } else {
    (entry.headers.clone(), entry.body.clone())
  };
  Json(serde_json::json!({
    "id": entry.id,
    "timestamp": entry.timestamp,
    "method": entry.method,
    "uri": entry.uri,
    "host": entry.host,
    "headers": view_headers,
    "body": view_body,
    "body_truncated": entry.body_truncated,
    "status": entry.status,
    "service": entry.service,
  }))
  .into_response()
}

/// Empties the caller's organization's inbox.
#[utoipa::path(delete, path = "/aperio/api/inbox", tag = "dashboard",
  description = "Deletes every webhook inbox entry of the caller's organization.",
  responses((status = 200, description = "Removed count", body = serde_json::Value)))]
pub(crate) async fn inbox_clear_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  let org = crate::auth::effective_org(&state, &headers).await;
  let removed = state.inbox_store.lock().await.clear(&org);
  Json(serde_json::json!({"status": "ok", "removed": removed})).into_response()
}

/// Deletes one inbox entry.
#[utoipa::path(delete, path = "/aperio/api/inbox/{id}", tag = "dashboard",
  description = "Deletes one webhook inbox entry.",
  params(("id" = String, Path, description = "Inbox entry id")),
  responses((status = 200, description = "Deleted"), (status = 404, description = "Unknown entry")))]
pub(crate) async fn inbox_delete_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  headers: HeaderMap,
) -> Response {
  let org = crate::auth::effective_org(&state, &headers).await;
  if state.inbox_store.lock().await.delete(&id, &org) {
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Inbox entry not found").into_response()
  }
}

/// Re-fires an inbox entry to the local client, using the same routing rules
/// as live traffic (so it lands on whichever client currently serves the
/// entry's host/path — the point of the inbox: replay an event the backend
/// missed or mishandled the first time).
#[utoipa::path(post, path = "/aperio/api/inbox/{id}/refire", tag = "dashboard",
  description = "Re-dispatches a stored webhook to the currently connected client for its route.",
  params(("id" = String, Path, description = "Inbox entry id")),
  responses((status = 200, description = "Backend status of the re-fire", body = serde_json::Value), (status = 404, description = "Unknown entry"), (status = 504, description = "No client for the route")))]
pub(crate) async fn inbox_refire_handler(
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
  let org = crate::auth::effective_org(&state, &headers).await;
  let entry = {
    let store = state.inbox_store.lock().await;
    store.get(&id, &org).cloned()
  };
  let Some(entry) = entry else {
    return (StatusCode::NOT_FOUND, "Inbox entry not found").into_response();
  };
  if entry.body_truncated {
    return (
      StatusCode::BAD_REQUEST,
      "The body was truncated at capture time; a re-fire would be incomplete",
    )
      .into_response();
  }

  // Route exactly like live traffic for the entry's host/path.
  let uri_path = entry.uri.split('?').next().unwrap_or(&entry.uri);
  let client_info = {
    let clients = state.clients.lock().await;
    match select_client_pool(
      &clients,
      uri_path,
      entry.host.as_deref(),
      state.config().require_hostname_bind,
      state.config().client_down_threshold,
    ) {
      None => None,
      Some((pool, group_key)) => {
        let pool = apply_lb_strategy(pool, &clients, state.config().lb_strategy);
        let mut rr_map = state.path_rr.lock().await;
        let idx = rr_map.entry(group_key).or_insert(0);
        let chosen_id = &pool[*idx % pool.len()];
        *idx = (*idx + 1) % pool.len();
        clients
          .get(chosen_id)
          .map(|c| (chosen_id.clone(), c.tx.clone(), c.request_count.clone()))
      }
    }
  };
  let Some((chosen_client_id, client_tx, client_req_counter)) = client_info else {
    return (
      StatusCode::GATEWAY_TIMEOUT,
      "No tunnel client available for the entry's route",
    )
      .into_response();
  };

  let refire_id = uuid::Uuid::new_v4().to_string();
  let (tx_response, rx_response) = oneshot::channel::<TunnelResponse>();
  state.pending_requests.lock().await.insert(
    refire_id.clone(),
    PendingRequest {
      tx: tx_response,
      client_id: chosen_client_id,
    },
  );
  let tunnel_req = TunnelMessage::Request {
    id: refire_id.clone(),
    method: entry.method.clone(),
    uri: entry.uri.clone(),
    headers: entry.headers.clone(),
    body: entry.body.clone(),
  };
  let req_json = match serde_json::to_string(&tunnel_req) {
    Ok(json) => json,
    Err(_) => {
      state.pending_requests.lock().await.remove(&refire_id);
      return (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response();
    }
  };
  if client_tx.send(Message::Text(req_json)).await.is_err() {
    state.pending_requests.lock().await.remove(&refire_id);
    return (StatusCode::BAD_GATEWAY, "Tunnel client socket error").into_response();
  }
  client_req_counter.fetch_add(1, Ordering::SeqCst);

  let start = Instant::now();
  let result = tokio::time::timeout(state.config().gateway_response_timeout, rx_response).await;
  state.pending_requests.lock().await.remove(&refire_id);

  match result {
    Ok(Ok(tunnel_res)) => {
      info!(
        "Re-fired webhook {} → {} ({} ms)",
        id,
        tunnel_res.status,
        start.elapsed().as_millis()
      );
      state
        .audit_session(
          "webhook_refired",
          &headers,
          &actor_ip,
          &format!(
            "id={} {} {} -> {}",
            id, entry.method, entry.uri, tunnel_res.status
          ),
        )
        .await;
      Json(serde_json::json!({
        "refired_id": id,
        "status": tunnel_res.status,
        "duration_ms": start.elapsed().as_millis() as u64,
      }))
      .into_response()
    }
    Ok(Err(_)) => (
      StatusCode::BAD_GATEWAY,
      "Client connection lost during the re-fire",
    )
      .into_response(),
    Err(_) => (StatusCode::GATEWAY_TIMEOUT, "Re-fire response timeout").into_response(),
  }
}
