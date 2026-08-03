//! Forwarding a client's OpenTelemetry exports to the collector
//! (planned_features: the OTel bridge).
//!
//! The client's half of this runs an OTLP receiver on loopback at the edge, so
//! anything there exports with one environment variable and no new firewall
//! rule. This is where those exports land, and all it does is hand them to the
//! collector the server already exports its own spans to.
//!
//! Two rules make it safe to expose at all.
//!
//! **Identity is stamped here, never taken from the payload.** The client says
//! which spans; the server says whose they are. Without that, one tenant's
//! telemetry could be written under another's name, which is worse than no
//! telemetry, because it is believed.
//!
//! **Two switches, both off by default.** The server's `otel_bridge` says the
//! server will forward at all; a token's `allow_otel` says this client may ask
//! it to. Either alone is a refusal. The server switch is the operator's
//! decision that an outbound path a client can drive should exist; the token
//! flag is the decision about which clients get it, which is not the same
//! question and should not be answered by the same setting.
//!
//! `allow_otel` defaults to false for the reason `topics` does: a capability
//! that switches itself on for every token that predates it is how a
//! permission model quietly stops meaning anything. The master token has it,
//! as it has everything.

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use crate::state::{AppState, ClientPerms};

/// Largest export accepted, matching the client's own fence.
const MAX_EXPORT_BYTES: usize = 8 * 1024 * 1024;

/// The signal paths OTLP defines. Anything else is a 404 rather than a
/// forward: the collector would answer the same way, later and less clearly.
fn signal_path(signal: &str) -> Option<&'static str> {
  match signal {
    "traces" => Some("v1/traces"),
    "metrics" => Some("v1/metrics"),
    "logs" => Some("v1/logs"),
    _ => None,
  }
}

/// Handler for `POST /aperio/otlp/v1/{signal}`.
#[utoipa::path(post, path = "/aperio/otlp/v1/{signal}", tag = "public",
  description = "Forwards an OTLP protobuf export from a tunnel client to the server's configured collector. Authenticated with the tunnel token; the server's otel_bridge must be on and the token must carry allow_otel.",
  params(("signal" = String, Path, description = "traces, metrics or logs")),
  request_body(content = String, description = "An OTLP protobuf export", content_type = "application/x-protobuf"),
  responses((status = 200, description = "Accepted for delivery"),
            (status = 401, description = "Unknown tunnel token"),
            (status = 403, description = "The token does not carry allow_otel"),
            (status = 404, description = "The bridge is off, or the signal is not one OTLP defines"),
            (status = 413, description = "Export too large"),
            (status = 502, description = "The collector refused it")))]
pub(crate) async fn otlp_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
  Path(signal): Path<String>,
  headers: HeaderMap,
  body: Bytes,
) -> Response {
  if !state.config().otel_bridge {
    return (StatusCode::NOT_FOUND, "the OTel bridge is not enabled").into_response();
  }
  let Some(path) = signal_path(signal.trim()) else {
    return (StatusCode::NOT_FOUND, "not an OTLP signal").into_response();
  };
  if body.len() > MAX_EXPORT_BYTES {
    return (StatusCode::PAYLOAD_TOO_LARGE, "export too large").into_response();
  }
  // The real peer, resolved exactly as every other endpoint resolves it. A
  // placeholder here would be read as the caller's address by the token's
  // `allowed_ips` fence, so a token allow-listing loopback would have been
  // accepted from anywhere, and one fenced to a private range refused from
  // the host it was issued for.
  let caller_ip = crate::routing::extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  let Some(perms) = crate::auth::authorize_tunnel_token(&state, &headers, caller_ip).await else {
    return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
  };
  if !may_bridge(&perms) {
    return (
      StatusCode::FORBIDDEN,
      "this token may not use the OTel bridge (set allow_otel on it)",
    )
      .into_response();
  }
  match forward(&state, path, &identity(&perms), body).await {
    Ok(()) => StatusCode::OK.into_response(),
    Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
  }
}

/// Whether a token may use the bridge.
///
/// The master token may, as it may everything else. A dynamic token needs
/// `allow_otel` written on it.
pub(crate) fn may_bridge(perms: &ClientPerms) -> bool {
  perms.master || perms.allow_otel
}

/// Who this export belongs to, as resource attributes the server can vouch
/// for.
pub(crate) fn identity(perms: &ClientPerms) -> Vec<(String, String)> {
  let mut out = vec![(
    "aperio.token".to_string(),
    perms
      .token_name
      .clone()
      .unwrap_or_else(|| "master".to_string()),
  )];
  if let Some(org) = &perms.org_id {
    out.push(("aperio.org".to_string(), org.clone()));
  }
  out
}

/// Sends one export on to the collector.
///
/// The payload is forwarded verbatim apart from the injected attributes:
/// nothing here understands OTLP's contents, which is what keeps it correct
/// against versions of the format this build has never seen.
pub(crate) async fn forward(
  _state: &Arc<AppState>,
  signal_path: &str,
  identity: &[(String, String)],
  payload: Bytes,
) -> Result<(), String> {
  let Some(endpoint) = collector_endpoint() else {
    return Err("no collector endpoint is configured on this server".to_string());
  };
  let payload = crate::otlp_identity::stamp(payload, identity);
  let url = format!("{}/{}", endpoint.trim_end_matches('/'), signal_path);
  let client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(30))
    .build()
    .map_err(|e| format!("cannot build the http client: {e}"))?;
  let mut req = client
    .post(&url)
    .header("content-type", "application/x-protobuf")
    .body(payload);
  // The collector's own credential, never the client's: the edge is not
  // supposed to hold one, which is the whole reason this path exists.
  if let Ok(headers) = std::env::var("APERIO_OTEL_HEADERS") {
    for pair in headers.split(',') {
      if let Some((k, v)) = pair.split_once('=') {
        req = req.header(k.trim(), v.trim());
      }
    }
  }
  let response = req.send().await.map_err(|e| format!("{e}"))?;
  if response.status().is_success() {
    Ok(())
  } else {
    Err(format!("the collector answered {}", response.status()))
  }
}

/// The collector base URL, from the same setting the server's own exporter
/// uses. Forwarding to a second, separate endpoint would be a way to have
/// telemetry arrive in two places and be reconciled in neither.
fn collector_endpoint() -> Option<String> {
  std::env::var("APERIO_OTEL_ENDPOINT")
    .ok()
    .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok())
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty())
}

#[cfg(test)]
#[path = "otlp_tests.rs"]
mod tests;
