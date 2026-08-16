use axum::{
  Json,
  extract::{ConnectInfo, State},
  http::HeaderMap,
  response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::routing::extract_client_ip;
use crate::state::AppState;
use crate::tunnel::pubsub;

#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct PublishRequest {
  /// Topic to publish on. Wildcards are filter syntax and are refused here.
  pub(crate) topic: String,
  /// The message, as text. Mutually exclusive with `payload_base64`.
  #[serde(default)]
  pub(crate) payload: Option<String>,
  /// The message, Base64-encoded, for anything that is not text.
  #[serde(default)]
  pub(crate) payload_base64: Option<String>,
  /// `0` (the default) sends once. `1` keeps the message until each
  /// subscriber acknowledges it, resending meanwhile, for as long as the
  /// server's acknowledgement window lasts.
  #[serde(default)]
  pub(crate) qos: u8,
}

/// Publishes a message to the subscribers of the caller's organization.
///
/// The push half of client-to-client messaging, deliberately over the admin
/// API rather than a tunnel: publishing is a one-shot, and a script or a CI
/// job that wants to signal a fleet should not have to hold a tunnel
/// connection to do it. `aperio-client api publish ...` is the same call from
/// a command line.
#[utoipa::path(post, path = "/aperio/api/publish", tag = "dashboard",
  description = "Publishes a message to the subscribers of this organization (operator).",
  request_body = PublishRequest,
  responses((status = 200, description = "Delivery counts", body = serde_json::Value)))]
pub(crate) async fn publish_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<PublishRequest>,
) -> Response {
  let bytes = match (
    payload.payload.as_deref(),
    payload.payload_base64.as_deref(),
  ) {
    (Some(_), Some(_)) => {
      return (
        axum::http::StatusCode::BAD_REQUEST,
        "give either `payload` or `payload_base64`, not both",
      )
        .into_response();
    }
    (Some(text), None) => text.as_bytes().to_vec(),
    (None, Some(b64)) => {
      use base64::prelude::*;
      match BASE64_STANDARD.decode(b64) {
        Ok(b) => b,
        Err(e) => {
          return (
            axum::http::StatusCode::BAD_REQUEST,
            format!("`payload_base64` is not valid Base64: {e}"),
          )
            .into_response();
        }
      }
    }
    // An empty message is a legitimate signal: the topic is the message.
    (None, None) => Vec::new(),
  };

  let org = crate::auth::effective_org(&state, &headers).await;
  let actor = state.session_actor(&headers).await;
  let delivered = match pubsub::publish(
    &state,
    org.as_deref(),
    &payload.topic,
    &bytes,
    pubsub::Publisher::Api(&actor),
    payload.qos,
  )
  .await
  {
    Ok(d) => d,
    Err(why) => return (axum::http::StatusCode::BAD_REQUEST, why).into_response(),
  };

  let ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  // Audited because it reaches other machines: a message that made something
  // happen elsewhere should be attributable afterwards.
  state
    .audit_in(
      "message_published",
      &actor,
      &ip,
      org.clone(),
      &format!(
        "topic={} bytes={} clients={}",
        payload.topic,
        bytes.len(),
        delivered.processes
      ),
    )
    .await;

  Json(serde_json::json!({
    "topic": payload.topic,
    "qos": payload.qos.min(1),
    "clients": delivered.processes,
    "connections": delivered.connections,
  }))
  .into_response()
}

/// What one client process is listening to.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct SubscriberView {
  /// The client process, as the dashboard groups connections elsewhere.
  pub(crate) instance_group: Option<String>,
  /// Its display name, when its connections announced one.
  pub(crate) service: Option<String>,
  /// The token it connected with, for reading a scope against a subscription.
  pub(crate) token_name: Option<String>,
  /// Connections of that process holding a subscription.
  pub(crate) connections: usize,
  /// The filters it asked for, deduplicated across those connections.
  pub(crate) topics: Vec<String>,
}

/// Lists who in this organization is listening, and to what.
///
/// The one thing about messaging that cannot be worked out from the outside.
/// A publish that reaches nobody looks exactly like a publish that reached
/// everybody, and the difference is usually a filter that does not match or a
/// token without the topic.
#[utoipa::path(get, path = "/aperio/api/subscribers", tag = "dashboard",
  description = "Lists the client processes subscribed to messages in this organization, and their topic filters.",
  responses((status = 200, description = "Subscribers and their filters", body = serde_json::Value)))]
pub(crate) async fn subscribers_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Json<Vec<SubscriberView>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let clients = state.clients.read().await;
  // Grouped by process, like every other per-client view: one client running
  // three services is one subscriber, not three.
  let mut by_process: std::collections::BTreeMap<String, SubscriberView> =
    std::collections::BTreeMap::new();
  for (connection_id, handle) in clients.iter() {
    if handle.perms.org_id.as_deref() != org.as_deref() || handle.subscriptions.is_empty() {
      continue;
    }
    let key = handle
      .instance_group
      .clone()
      .unwrap_or_else(|| connection_id.clone());
    let entry = by_process
      .entry(key.clone())
      .or_insert_with(|| SubscriberView {
        instance_group: handle.instance_group.clone(),
        service: handle.sole().service_name.clone(),
        token_name: handle.perms.token_name.clone(),
        connections: 0,
        topics: Vec::new(),
      });
    entry.connections += 1;
    for topic in &handle.subscriptions {
      if !entry.topics.iter().any(|t| t == topic) {
        entry.topics.push(topic.clone());
      }
    }
  }
  for view in by_process.values_mut() {
    view.topics.sort();
  }
  Json(by_process.into_values().collect())
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
