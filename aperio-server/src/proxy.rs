use axum::{
  body::Body,
  extract::{ConnectInfo, State, ws::Message},
  http::{HeaderName, HeaderValue, StatusCode},
  response::{IntoResponse, Response},
};
use chrono::Local;
use futures_util::stream::StreamExt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::oneshot;
use tracing::{Instrument, error, warn};

use crate::access_log::{log_request_failure, log_request_success};
use crate::auth::{safe_redirect_path, validate_session_for_host, validate_session_for_visitor};
use crate::limits::{Limit, refuse};
use crate::protocol::{FRAME_REQUEST_CHUNK, TunnelMessage, encode_binary_frame};
use crate::routing::{
  PickOutcome, extract_client_ip, extract_request_host, method_retryable, pick_proxy_client,
  wait_for_candidate,
};
use crate::settings::{FailoverMode, LbStrategy};
use crate::share::{check_share_access, cookie_value};
use crate::state::{
  AppState, CAPTURE_BODY_LIMIT, CAPTURE_MAX_ENTRIES, CapturedRequest, PendingRequest,
  REQUEST_STREAM_THRESHOLD, TunnelResponse,
};
use crate::telemetry;

// Split by which stage of a request each part is. `forward` is the request
// path itself and is one function, moved whole rather than cut: it holds a
// dispatch slot, a pending registration, a body pump and failure counters that
// every one of its many exits has to release.
pub(crate) mod cache;
pub(crate) mod forward;
pub(crate) mod gate;
pub(crate) mod ws;

pub(crate) use cache::*;
pub(crate) use forward::*;
pub(crate) use gate::*;

/// Whether the buffered-5xx retry policy covers a given response status.
/// `retry_statuses` empty = every 5xx (500-599); otherwise only the listed
/// codes. `retry_on_5xx` off = never.
fn retry_covers(retry_on_5xx: bool, retry_statuses: &[u16], status: u16) -> bool {
  if !retry_on_5xx {
    return false;
  }
  if retry_statuses.is_empty() {
    (500..600).contains(&status)
  } else {
    retry_statuses.contains(&status)
  }
}

/// Records one dispatch failure (buffered 5xx, response timeout, or connection
/// loss) against the serving client for passive outlier ejection. A no-op
/// unless `APERIO_OUTLIER_EJECTION` is enabled.
/// `service` is the chosen service's *name*, not its index.
///
/// The index was captured under a read lock the dispatch has long since
/// released, and a Ping carrying a list rebuilds `services` wholesale, which
/// `match_declarations` is expressly designed to survive across a reorder or
/// a withdrawal. So by the time a 30-second timeout fires, index 1 may be a
/// different service or may not exist: the failure is charged to a healthy
/// neighbour and eventually ejects it, while the one that actually failed
/// keeps serving. The name is the identity reconcile preserves, which is why
/// it is the thing to carry.
///
/// `None` is a connection carrying one service, where no list arrived, no
/// rebuild happened, and index 0 is exactly as stable as the connection.
async fn record_outlier_failure(state: &AppState, client_id: &str, service: Option<&str>) {
  let cfg = state.config();
  if !cfg.outlier_ejection {
    return;
  }
  let now = Instant::now();
  let mut clients = state.clients.write().await;
  // Charged to the service that failed, not to the connection. Ejection
  // removes a candidate from routing, and routing chooses services, so
  // ejecting the connection would take every service on it out over one
  // backend's bad minute.
  if let Some(service) = clients.get_mut(client_id).and_then(|c| match service {
    Some(name) => c
      .services
      .iter_mut()
      .find(|s| s.service_name.as_deref() == Some(name)),
    None => c.services.first_mut(),
  }) && service.record_failure(
    now,
    cfg.outlier_window,
    cfg.outlier_max_failures,
    cfg.outlier_eject,
  ) {
    warn!(
      "Passive ejection: client {} removed from routing for {}s after {} failures within {}s",
      client_id,
      cfg.outlier_eject.as_secs(),
      cfg.outlier_max_failures,
      cfg.outlier_window.as_secs()
    );
  }
}

/// Whether it is worth waiting for a route that has no candidate right now.
///
/// Two different questions used to be one flag. *Does this route have a live
/// candidate* is per route, and asking the server-wide "is any client
/// connected" instead meant a dead route skipped the wait entirely whenever
/// an unrelated service was online. *Could it come back* is what decides
/// whether waiting is worth anything at all, and answering it with the same
/// flag meant a route that has been dead for hours still burned the full
/// gateway timeout before saying so.
///
/// Waiting pays only when something might arrive: a client dropped recently
/// enough that it is plausibly reconnecting, or scale-to-zero is configured
/// and a cold start can wake one. Otherwise the route is simply not served,
/// and the caller should be told that now rather than in thirty seconds.
///
/// The disconnect clock is server-wide because that is the only one there is;
/// it is used only to decide *whether* to wait, never how long, and the wait
/// itself stops the moment this route has a candidate.
fn worth_waiting_for_route(
  last_disconnect: Option<Instant>,
  now: Instant,
  recent: std::time::Duration,
  scaling_enabled: bool,
) -> bool {
  if scaling_enabled {
    return true;
  }
  last_disconnect.is_some_and(|t| now.saturating_duration_since(t) <= recent)
}

/// Builds a 504 response: the hostname's own `error_pages:` page when one is
/// configured, then the global APERIO_504_PAGE HTML, then the given
/// plain-text message.
pub(crate) fn gateway_timeout_response(
  state: &AppState,
  request_host: Option<&str>,
  fallback: &str,
) -> Response {
  let config = state.config();
  let html = config
    .error_pages
    .page_504(request_host)
    .or(config.custom_504_page.as_deref());
  match html {
    Some(html) => (
      StatusCode::GATEWAY_TIMEOUT,
      [("content-type", "text/html; charset=utf-8")],
      html.to_string(),
    )
      .into_response(),
    None => (StatusCode::GATEWAY_TIMEOUT, fallback.to_string()).into_response(),
  }
}

/// Effective request body cap for a dispatch: a service's client-declared
/// `max_request_body` can only tighten the global APERIO_MAX_BODY_SIZE limit,
/// never widen it.
pub(crate) fn effective_body_limit(global: usize, declared: Option<u64>) -> usize {
  match declared {
    Some(limit) => (limit as usize).min(global),
    None => global,
  }
}

/// Whether a buffered body travels as bytes in the dispatch frame (v6) rather
/// than base64 inside the JSON.
///
/// A function rather than a line at the call site because the call site is
/// inside the dispatch loop, which re-runs with a *different* client after a
/// failover, and the answer is a property of that client rather than of the
/// request.
fn body_frame_negotiated(protocol: Option<u32>, body: &[u8]) -> bool {
  protocol.is_some_and(|v| v >= 6) && !body.is_empty()
}

/// Whether a visitor-supplied request id may be adopted.
///
/// The value ends up in the access log, in the audit trail of what served
/// what, and in a header sent to the backend, so it is bounded and restricted
/// to characters that cannot break any of those: no control characters to
/// forge a log line, no spaces or commas to look like two values, and a
/// length a header and a log field can carry without truncation deciding
/// where the id ends. Anything else is not rejected loudly, it simply is not
/// adopted, and the server's own id is used instead.
fn is_safe_request_id(v: &str) -> bool {
  !v.is_empty()
    && v.len() <= 128
    && v
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'/' | b'+'))
}

/// Builds the 503 maintenance response: the hostname's own `error_pages:`
/// page, then the global APERIO_503_PAGE, then plain text.
///
/// The flag's reason reaches the visitor: a maintenance page that says why,
/// and until when, is the difference between "this is broken" and "this is
/// planned". A custom page opts in by writing `{reason}` and `{until}` where
/// it wants them, so an existing page is unchanged.
fn maintenance_response(
  state: &AppState,
  request_host: Option<&str>,
  flag: &crate::state::MaintenanceFlag,
) -> Response {
  let config = state.config();
  let now = crate::store::tokens::now_secs();
  let reason = flag.reason.clone().unwrap_or_default();
  let until = flag
    .until
    .map(|end| {
      chrono::DateTime::from_timestamp(end as i64, 0)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
    })
    .unwrap_or_default();
  let html = config
    .error_pages
    .page_503(request_host)
    .or(config.custom_503_page.as_deref());
  let mut resp = match html {
    Some(html) => (
      StatusCode::SERVICE_UNAVAILABLE,
      [("content-type", "text/html; charset=utf-8")],
      html.replace("{reason}", &reason).replace("{until}", &until),
    )
      .into_response(),
    None => {
      let mut text =
        "503 Service Unavailable - This site is temporarily down for maintenance".to_string();
      if !reason.is_empty() {
        text.push_str("\n\n");
        text.push_str(&reason);
      }
      if !until.is_empty() {
        text.push_str("\n\nExpected back at ");
        text.push_str(&until);
      }
      text.push('\n');
      (StatusCode::SERVICE_UNAVAILABLE, text).into_response()
    }
  };
  // A window that is known is a truthful Retry-After; without one, the fixed
  // fallback, which promises nothing in particular.
  let seconds = flag.retry_after(now).unwrap_or(300);
  if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
    resp.headers_mut().insert("retry-after", value);
  }
  resp
}

/// Proxy handler for forwarding all incoming HTTP requests to active client.
/// Also detects WebSocket upgrade requests and proxies them as persistent streams.
pub(crate) async fn proxy_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  req: axum::extract::Request<Body>,
) -> Response {
  let method = req.method().clone();
  let uri = req.uri().clone();
  let mut headers = req.headers().clone();
  // HTTP/2 requests carry the host in the :authority pseudo-header (surfaced
  // as the URI authority), not a Host header. Mirror it so hostname routing,
  // maintenance scoping, and the visitor gate see h2 traffic (gRPC) too.
  if !headers.contains_key("host")
    && let Some(authority) = uri.authority()
    && let Ok(val) = axum::http::HeaderValue::from_str(authority.as_str())
  {
    headers.insert("host", val);
  }
  let caller_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );

  // Maintenance mode wins over everything else (including WS upgrades):
  // visitors get the 503 page even while tunnel clients stay connected.
  let host_for_maintenance = extract_request_host(&headers);
  if let Some(flag) = state.maintenance_for(host_for_maintenance.as_deref()).await {
    return maintenance_response(&state, host_for_maintenance.as_deref(), &flag);
  }

  // Client-less routes (aperio-server.yaml `routes:`): redirects and fixed
  // responses answered straight from the server, before client routing.
  if !state.config().static_routes.is_empty()
    && let Some(answer) = state.config().static_routes.answer(
      extract_request_host(&headers).as_deref(),
      uri.path(),
      uri.query(),
    )
  {
    return answer;
  }

  // Preview noindex: on random-subdomain hosts, answer robots.txt with a
  // disallow-all straight from the server (after static routes, so an
  // explicit `routes:` robots.txt still wins).
  if state.config().preview_noindex
    && uri.path() == "/robots.txt"
    && let Some(ref pattern) = state.config().random_subdomain_suffix
    && extract_request_host(&headers)
      .as_deref()
      .is_some_and(|h| crate::routing::host_matches_random_pattern(h, pattern))
  {
    return Response::builder()
      .status(StatusCode::OK)
      .header("content-type", "text/plain")
      .header("x-robots-tag", "noindex, nofollow")
      .body(Body::from("User-agent: *\nDisallow: /\n"))
      .unwrap_or_default();
  }

  // First-run convenience: on a fresh install (no client has ever connected,
  // no request ever proxied) a visit to the bare root is almost certainly the
  // operator checking their new server, send them to the dashboard with a
  // temporary redirect. The moment a client connects or any traffic flows,
  // this never triggers again.
  if state.dashboard_enabled
    && method == axum::http::Method::GET
    && uri.path() == "/"
    && state.clients.read().await.is_empty()
    && state.persistent_stats.lock().await.lifetime_requests() == 0
  {
    return Response::builder()
      .status(StatusCode::TEMPORARY_REDIRECT)
      .header("location", "/aperio")
      .body(Body::empty())
      .unwrap();
  }

  // Detect WebSocket upgrade requests and handle separately
  if is_websocket_upgrade(&method, &headers) {
    return ws::handle_ws_proxy(state, req, method, uri, headers, addr, caller_ip).await;
  }

  // --- Normal HTTP proxy below ---

  // Per-request OpenTelemetry span (no-op export when APERIO_OTEL is off). The
  // span adopts any incoming W3C trace context as its parent; its own context
  // is forwarded through the tunnel so the backend continues the trace.
  let host_for_span = extract_request_host(&headers);
  let span = telemetry::request_span(
    &headers,
    method.as_str(),
    uri.path(),
    host_for_span.as_deref(),
  );
  let trace_headers = telemetry::trace_headers(&span);
  let body = req.into_body();
  let response = proxy_http_request(state, method, uri, headers, body, caller_ip, trace_headers)
    .instrument(span.clone())
    .await;
  telemetry::record_status(&span, response.status().as_u16());
  response
}

/// Maps a relayed body frame to an HTTP body frame (data or trailers).
fn frame_from_body_item(
  item: Result<crate::state::BodyFrame, std::io::Error>,
) -> Result<http_body::Frame<axum::body::Bytes>, axum::BoxError> {
  match item {
    Ok(crate::state::BodyFrame::Data(bytes)) => Ok(http_body::Frame::data(bytes)),
    Ok(crate::state::BodyFrame::Trailers(trailers)) => {
      Ok(http_body::Frame::trailers(trailer_header_map(&trailers)))
    }
    Err(e) => Err(e.into()),
  }
}

/// Builds a HeaderMap from relayed trailer pairs, skipping invalid names.
fn trailer_header_map(trailers: &[(String, String)]) -> axum::http::HeaderMap {
  let mut map = axum::http::HeaderMap::new();
  for (k, v) in trailers {
    if let (Ok(name), Ok(val)) = (
      axum::http::HeaderName::from_bytes(k.as_bytes()),
      axum::http::HeaderValue::from_str(v),
    ) {
      map.append(name, val);
    }
  }
  map
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod proxy_tests;
