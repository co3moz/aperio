use axum::{
  Json,
  extract::{ConnectInfo, State},
  http::HeaderMap,
  response::{IntoResponse, Response},
};
use serde::Deserialize;
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
    "clients": delivered.processes,
    "connections": delivered.connections,
  }))
  .into_response()
}
