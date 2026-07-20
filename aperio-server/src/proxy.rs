use axum::{
  body::Body,
  extract::{ConnectInfo, State, ws::Message},
  http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
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
use crate::auth::{safe_redirect_path, validate_session, validate_session_for_host};
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

pub(crate) mod ws;

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
async fn record_outlier_failure(state: &AppState, client_id: &str) {
  let cfg = state.config();
  if !cfg.outlier_ejection {
    return;
  }
  let now = Instant::now();
  let mut clients = state.clients.lock().await;
  if let Some(handle) = clients.get_mut(client_id)
    && handle.record_failure(
      now,
      cfg.outlier_window,
      cfg.outlier_max_failures,
      cfg.outlier_eject,
    )
  {
    warn!(
      "Passive ejection: client {} removed from routing for {}s after {} failures within {}s",
      client_id,
      cfg.outlier_eject.as_secs(),
      cfg.outlier_max_failures,
      cfg.outlier_window.as_secs()
    );
  }
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

/// Single-flight leadership over one cache key: held by the first request
/// that misses the cache for a coalescable GET while followers wait. Drop
/// removes the key from the in-flight table and (by dropping the watch
/// sender) wakes every waiting follower — on all exit paths.
struct CacheSingleFlight {
  state: Arc<AppState>,
  key: String,
  _done: tokio::sync::watch::Sender<bool>,
}

impl Drop for CacheSingleFlight {
  fn drop(&mut self) {
    if let Ok(mut inflight) = self.state.cache_inflight.lock() {
      inflight.remove(&self.key);
    }
  }
}

/// Background stale-while-revalidate refresh: re-fetches one cacheable GET
/// through the already-selected tunnel client and replaces the cache entry
/// on a cacheable 200. Fire-and-forget — a failure leaves the stale entry
/// serving until its SWR window closes (the leader election retries).
#[allow(clippy::too_many_arguments)]
fn spawn_swr_revalidation(
  state: Arc<AppState>,
  cache_key: String,
  uri: String,
  headers: Vec<(String, String)>,
  client_id: String,
  client_tx: tokio::sync::mpsc::Sender<Message>,
  resilient: bool,
) {
  tokio::spawn(async move {
    let revalidate_id = uuid::Uuid::new_v4().to_string();
    let (tx_response, rx_response) = oneshot::channel::<TunnelResponse>();
    state.pending_requests.lock().await.insert(
      revalidate_id.clone(),
      PendingRequest {
        tx: tx_response,
        client_id,
      },
    );
    let msg = TunnelMessage::Request {
      id: revalidate_id.clone(),
      method: "GET".to_string(),
      uri,
      headers,
      body: None,
    };
    let Ok(json) = serde_json::to_string(&msg) else {
      state.pending_requests.lock().await.remove(&revalidate_id);
      return;
    };
    if client_tx.send(Message::Text(json)).await.is_err() {
      state.pending_requests.lock().await.remove(&revalidate_id);
      return;
    }
    let result = tokio::time::timeout(state.config().gateway_response_timeout, rx_response).await;
    state.pending_requests.lock().await.remove(&revalidate_id);
    if let Ok(Ok(tunnel_res)) = result {
      // Streamed bodies never refresh the cache (dropping stream_rx makes
      // the tunnel read loop clean the stream up).
      if tunnel_res.stream_rx.is_none()
        && tunnel_res.status == 200
        && let Some(ttl) = crate::cache::response_cache_ttl(&tunnel_res.headers)
      {
        use base64::prelude::*;
        let body = tunnel_res
          .body
          .as_deref()
          .and_then(|b| BASE64_STANDARD.decode(b).ok())
          .unwrap_or_default();
        let swr = crate::cache::response_swr_window(&tunnel_res.headers);
        let surrogate = crate::cache::response_surrogate_keys(&tunnel_res.headers);
        state.response_cache.lock().await.insert(
          cache_key,
          tunnel_res.status,
          tunnel_res.headers,
          body,
          ttl,
          state.config().cache_max_bytes,
          resilient,
          swr,
          surrogate,
        );
      }
    }
  });
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

/// True when the request's hostname is currently in maintenance mode
/// (either listed explicitly or covered by the `*` wildcard entry).
async fn in_maintenance(state: &AppState, request_host: Option<&str>) -> bool {
  let set = state.maintenance.lock().await;
  if set.is_empty() {
    return false;
  }
  set.contains_key("*") || request_host.is_some_and(|h| set.contains_key(h))
}

/// Builds the 503 maintenance response: the hostname's own `error_pages:`
/// page, then the global APERIO_503_PAGE, then plain text.
fn maintenance_response(state: &AppState, request_host: Option<&str>) -> Response {
  let config = state.config();
  let html = config
    .error_pages
    .page_503(request_host)
    .or(config.custom_503_page.as_deref());
  let mut resp = match html {
    Some(html) => (
      StatusCode::SERVICE_UNAVAILABLE,
      [("content-type", "text/html; charset=utf-8")],
      html.to_string(),
    )
      .into_response(),
    None => (
      StatusCode::SERVICE_UNAVAILABLE,
      "503 Service Unavailable - This site is temporarily down for maintenance",
    )
      .into_response(),
  };
  resp
    .headers_mut()
    .insert("retry-after", HeaderValue::from_static("300"));
  resp
}

/// Checks if an HTTP request is a WebSocket upgrade request.
fn is_websocket_upgrade(method: &Method, headers: &HeaderMap) -> bool {
  if method != Method::GET {
    return false;
  }
  let has_upgrade_header = headers
    .get("upgrade")
    .and_then(|v| v.to_str().ok())
    .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
  let has_connection_upgrade = headers
    .get("connection")
    .and_then(|v| v.to_str().ok())
    .is_some_and(|v| v.to_lowercase().contains("upgrade"));
  has_upgrade_header && has_connection_upgrade
}

/// Outcome of the visitor-auth gate for a proxied request.
pub(crate) enum VisitorGate {
  /// The visitor may proceed.
  Allow,
  /// The visitor is not authorized; reply with this response (a login/OIDC
  /// redirect, or a share-link redirect).
  Deny(Response),
}

/// Builds a 302 to the login flow, preserving the original path so the visitor
/// returns to it after authenticating.
fn login_redirect(login_path: &str, uri_str: &str) -> Response {
  let redirect_url = format!("{}?redirect={}", login_path, safe_redirect_path(uri_str));
  Response::builder()
    .status(StatusCode::FOUND)
    .header("Location", redirect_url)
    .body(Body::empty())
    .unwrap()
}

/// Applies the visitor-auth gate for a proxied request to (host, path), shared
/// by the HTTP and WebSocket proxy paths.
///
/// 0. A request path containing a traversal segment never weakens the gate:
///    it is treated as covered by no public/override/share scope and, when any
///    gate could apply on the host, requires a full session.
/// 1. When a client declared a per-service visitor password for this route
///    (`route_visitor_auth`), it supersedes the server's own gate: the visitor
///    must hold a session valid for this host (a host-scoped login, or any
///    global session) — or a share cookie/link that covers the route. The login
///    always uses the password form (never OIDC), since the credentials are the
///    client's.
/// 2. Otherwise the server's own gate applies: public routes skip it; a
///    configured server password / OIDC requires a global session or a share.
pub(crate) async fn check_visitor_gate(
  state: &Arc<AppState>,
  headers: &HeaderMap,
  uri: &axum::http::Uri,
  host: Option<&str>,
) -> VisitorGate {
  let path = uri.path();

  // 0. Traversal paths never weaken the gate. `/a/../b` matches an `/a` path
  // bind, but a backend that resolves `..` serves `/b` — so such a path is
  // never public, never unlocks a client's per-service credentials or a share
  // scope (share checks reject it too), and when any gate could apply on this
  // host it requires a full (global) session.
  if crate::routing::request_path_has_traversal(path) {
    let gated = state.config().auth_credentials.is_some()
      || state.oidc.is_some()
      || crate::routing::host_has_visitor_auth(state, host).await;
    if !gated || validate_session(state, headers).await {
      return VisitorGate::Allow;
    }
    let login_path = if state.oidc.is_some() {
      "/aperio/oidc/login"
    } else {
      "/aperio/auth"
    };
    return VisitorGate::Deny(login_redirect(login_path, &uri.to_string()));
  }

  // 1. Client-declared per-service visitor password override.
  if crate::routing::route_visitor_auth(state, path, host)
    .await
    .is_some()
  {
    if validate_session_for_host(state, headers, host).await {
      return VisitorGate::Allow;
    }
    return match check_share_access(state, headers, uri, host) {
      Some(Some(redirect)) => VisitorGate::Deny(redirect),
      Some(None) => VisitorGate::Allow,
      None => VisitorGate::Deny(login_redirect("/aperio/auth", &uri.to_string())),
    };
  }

  // 2. Server's own visitor gate.
  let auth_configured = state.config().auth_credentials.is_some() || state.oidc.is_some();
  if !auth_configured || crate::routing::route_is_public(state, path, host).await {
    return VisitorGate::Allow;
  }
  if validate_session(state, headers).await {
    return VisitorGate::Allow;
  }
  match check_share_access(state, headers, uri, host) {
    Some(Some(redirect)) => VisitorGate::Deny(redirect),
    Some(None) => VisitorGate::Allow,
    None => {
      let login_path = if state.oidc.is_some() {
        "/aperio/oidc/login"
      } else {
        "/aperio/auth"
      };
      VisitorGate::Deny(login_redirect(login_path, &uri.to_string()))
    }
  }
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
  if in_maintenance(&state, extract_request_host(&headers).as_deref()).await {
    return maintenance_response(&state, extract_request_host(&headers).as_deref());
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
  // operator checking their new server — send them to the dashboard with a
  // temporary redirect. The moment a client connects or any traffic flows,
  // this never triggers again.
  if state.dashboard_enabled
    && method == axum::http::Method::GET
    && uri.path() == "/"
    && state.clients.lock().await.is_empty()
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

/// Builds the visitor response for a cache hit: a full 200 with the stored
/// body, or a bodyless 304 when the request's `If-None-Match` matches the
/// entry's validator (synthesized at store time when the backend sent none).
/// Returns the status actually sent, for stats and the access log.
fn cache_hit_response(
  hit: crate::cache::CacheHit,
  request_headers: &HeaderMap,
) -> (u16, u64, Response) {
  let etag = hit
    .headers
    .iter()
    .find(|(n, _)| n.eq_ignore_ascii_case("etag"))
    .map(|(_, v)| v.clone());
  let not_modified = request_headers
    .get("if-none-match")
    .and_then(|v| v.to_str().ok())
    .zip(etag.as_deref())
    .is_some_and(|(inm, tag)| crate::cache::if_none_match_matches(inm, tag));

  // Range requests (video scrubbing, resumable downloads) are satisfied
  // straight from the cached full body — a 304 still wins, and an `If-Range`
  // validator that no longer matches degrades to the full 200 per RFC 9110.
  let total_len = hit.body.len();
  let range_outcome = if not_modified || hit.status != 200 {
    crate::cache::RangeOutcome::Full
  } else {
    let if_range_ok = match request_headers
      .get("if-range")
      .and_then(|v| v.to_str().ok())
    {
      // An If-Range with a date (or a stale validator) means "full body".
      Some(validator) => etag.as_deref() == Some(validator.trim()),
      None => true,
    };
    match request_headers.get("range").and_then(|v| v.to_str().ok()) {
      Some(range) if if_range_ok => crate::cache::evaluate_range(range, total_len),
      _ => crate::cache::RangeOutcome::Full,
    }
  };

  let status = match range_outcome {
    _ if not_modified => 304,
    crate::cache::RangeOutcome::Partial(_, _) => 206,
    crate::cache::RangeOutcome::Unsatisfiable => 416,
    crate::cache::RangeOutcome::Full => hit.status,
  };
  let mut builder = Response::builder()
    .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
    .header("x-aperio-cache", "hit")
    .header("age", hit.age_secs.to_string());
  if hit.stale {
    builder = builder.header("x-aperio-stale", "true");
  }
  if hit.status == 200 && !not_modified {
    builder = builder.header("accept-ranges", "bytes");
  }
  match range_outcome {
    crate::cache::RangeOutcome::Partial(start, end) => {
      builder = builder.header(
        "content-range",
        format!("bytes {}-{}/{}", start, end, total_len),
      );
    }
    crate::cache::RangeOutcome::Unsatisfiable => {
      builder = builder.header("content-range", format!("bytes */{}", total_len));
    }
    crate::cache::RangeOutcome::Full => {}
  }
  for (k, v) in hit.headers.iter() {
    let k_lower = k.to_ascii_lowercase();
    // A 304 carries only the metadata a client may need to update its own
    // cache entry — never entity headers describing a body that isn't there.
    if not_modified
      && !matches!(
        k_lower.as_str(),
        "etag" | "cache-control" | "expires" | "last-modified" | "vary"
      )
    {
      continue;
    }
    // Never copy the cached body's Content-Length: a range/304 response sends
    // a different number of bytes than the full entity, so a stale value would
    // desync the framing (curl sees a truncated reply). Hyper derives the
    // correct Content-Length from the actual body below.
    if k_lower == "content-length" {
      continue;
    }
    if let (Ok(name), Ok(value)) = (
      HeaderName::from_bytes(k.as_bytes()),
      HeaderValue::from_str(v),
    ) {
      builder = builder.header(name, value);
    }
  }
  let (bytes, body) = if not_modified {
    (0u64, Body::empty())
  } else {
    match range_outcome {
      crate::cache::RangeOutcome::Partial(start, end) => {
        let slice = hit.body[start..=end].to_vec();
        (slice.len() as u64, Body::from(slice))
      }
      crate::cache::RangeOutcome::Unsatisfiable => (0u64, Body::empty()),
      crate::cache::RangeOutcome::Full => (hit.body.len() as u64, Body::from(hit.body)),
    }
  };
  let response = builder
    .body(body)
    .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "cache error").into_response());
  (status, bytes, response)
}

/// Serve-stale fallback: when a route has no client to dispatch to, a
/// resilient cached response (fresh, or expired within the max-stale window)
/// beats a 504. Entries qualify only if the client that produced them
/// announced `resilience`, so a normal service never serves stale content;
/// the moment a healthy client reconnects, the regular proxy path takes over.
async fn stale_cache_response(
  state: &Arc<AppState>,
  method: &str,
  uri: &str,
  headers: &HeaderMap,
  start_time: Instant,
) -> Option<Response> {
  let cfg = state.config();
  if !cfg.cache_enabled || !crate::cache::request_cacheable(method, headers) {
    return None;
  }
  let host = extract_request_host(headers);
  let key = crate::cache::cache_key(host.as_deref(), uri);
  let max_stale = std::time::Duration::from_secs(cfg.cache_max_stale);
  let hit = state
    .response_cache
    .lock()
    .await
    .get_for_outage(&key, max_stale)?;

  let duration = start_time.elapsed();
  let (status, body_len, response) = cache_hit_response(hit, headers);
  {
    let mut stats = state.stats.lock().await;
    stats.total_requests += 1;
    stats.successful_requests += 1;
    stats.total_bytes_transferred += body_len;
  }
  state.persistent_stats.lock().await.record_request_labeled(
    true,
    0,
    body_len,
    duration.as_millis() as u64,
    None,
    host.as_deref(),
    None,
  );
  log_request_success(
    state,
    uuid::Uuid::new_v4().to_string(),
    method,
    uri,
    status,
    duration,
    host.as_deref(),
    None,
    None,
    None,
  )
  .await;
  telemetry::record_status(&tracing::Span::current(), status);
  Some(response)
}

/// Forwards a buffered/streamed HTTP request over the tunnel and maps the
/// response back. Split out of [`proxy_handler`] so the whole flow runs inside
/// one instrumented request span.
#[allow(clippy::too_many_arguments)]
async fn proxy_http_request(
  state: Arc<AppState>,
  method: Method,
  uri: axum::http::Uri,
  headers: HeaderMap,
  body: Body,
  caller_ip: std::net::IpAddr,
  trace_headers: Vec<(String, String)>,
) -> Response {
  let method_str = method.to_string();
  let uri_str = uri.to_string();
  let start_time = Instant::now();

  // 1. Per-IP Rate Limiting (Token Bucket)
  if !state.check_rate_limit(caller_ip).await {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      429,
      start_time.elapsed(),
      Some(&format!("Rate Limit Exceeded for IP {}", caller_ip)),
      None,
    )
    .await;
    return (
      StatusCode::TOO_MANY_REQUESTS,
      "429 Too Many Requests - IP rate limit exceeded",
    )
      .into_response();
  }

  // 2. Visitor-auth gate: a client-declared per-service password (if any)
  // supersedes the server's own visitor password / OIDC; public routes skip it.
  if let VisitorGate::Deny(resp) = check_visitor_gate(
    &state,
    &headers,
    &uri,
    extract_request_host(&headers).as_deref(),
  )
  .await
  {
    return resp;
  }

  // Client-declared visitor IP allowlists are enforced per candidate during
  // client selection below: the request dispatches to any candidate whose
  // own list admits the visitor, and a fully rejected visitor gets the
  // winning `denied:` redirect — or a stealth answer identical to an
  // unclaimed route — so blocked traffic still never enters the tunnel.

  // 3. Wait for connection if client is disconnected.
  // Take a consistent snapshot of connection state under a single lock to avoid TOCTOU.
  let (is_connected, _last_disc) = {
    let conn = state.connection_state.lock().await;
    (conn.connected, conn.last_disconnect)
  };
  if !is_connected {
    // Wait for a client to reconnect, bounded by the configured gateway timeout.
    let mut rx = state.client_connected.subscribe();
    let timeout_fut = tokio::time::sleep(state.config().gateway_timeout);
    tokio::pin!(timeout_fut);

    let mut reconnected = false;
    loop {
      tokio::select! {
          _ = &mut timeout_fut => {
              break;
          }
          res = rx.changed() => {
              if res.is_ok() && *rx.borrow() {
                  reconnected = true;
                  break;
              }
          }
      }
    }

    if !reconnected {
      // A resilient cached answer (possibly stale) beats the 504.
      if let Some(resp) =
        stale_cache_response(&state, &method_str, &uri_str, &headers, start_time).await
      {
        return resp;
      }
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        504,
        start_time.elapsed(),
        Some("Gateway Timeout - Reconnect wait expired"),
        None,
      )
      .await;
      return gateway_timeout_response(
        &state,
        extract_request_host(&headers).as_deref(),
        "504 Gateway Timeout - No client connected in time",
      );
    }
  }

  // 4. Limit concurrency to prevent resource starvation / DoS
  let _permit = match state.try_acquire_request_slot() {
    Some(p) => p,
    None => {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        429,
        start_time.elapsed(),
        Some("Concurrency limit exceeded"),
        None,
      )
      .await;
      return (
        StatusCode::TOO_MANY_REQUESTS,
        "429 Too Many Requests - Concurrency limit reached on tunnel server",
      )
        .into_response();
    }
  };

  // 4. Get an active client, preferring hostname- and path-bound matches
  // with per-group round-robin.
  let request_host = extract_request_host(&headers);
  let uri_path_owned = uri_str.split('?').next().unwrap_or(&uri_str).to_string();

  // WAF-lite deny rules (`waf:` section): reject path/method/header attacks
  // with 403 before the request is dispatched or its body read.
  {
    let cfg = state.config();
    if !cfg.waf.is_empty()
      && let Some(reason) = cfg.waf.deny_reason(&method_str, &uri_path_owned, &headers)
    {
      let reason = reason.to_string();
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        403,
        start_time.elapsed(),
        Some(&format!("WAF deny: {reason}")),
        None,
      )
      .await;
      return (StatusCode::FORBIDDEN, "403 Forbidden - Blocked by WAF").into_response();
    }
  }

  // Per-route rate limit (`rate_limits:` section): a shared token bucket caps
  // aggregate rps to a matched host+path, protecting expensive endpoints.
  if !state
    .check_route_rate_limit(request_host.as_deref(), &uri_path_owned)
    .await
  {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      429,
      start_time.elapsed(),
      Some(&format!("Route rate limit exceeded for {}", uri_path_owned)),
      None,
    )
    .await;
    return (
      StatusCode::TOO_MANY_REQUESTS,
      "429 Too Many Requests - Route rate limit exceeded",
    )
      .into_response();
  }

  // Sticky strategy: a returning visitor carries an affinity cookie naming
  // the client that served them before.
  let affinity = if state.config().lb_strategy == LbStrategy::Sticky {
    cookie_value(&headers, "aperio_affinity")
  } else {
    None
  };
  let mut selected = match pick_proxy_client(
    &state,
    &uri_path_owned,
    request_host.as_deref(),
    None,
    affinity.as_deref(),
    Some(caller_ip),
  )
  .await
  {
    PickOutcome::Selected(client) => *client,
    PickOutcome::Denied(Some(redirect)) => {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        302,
        start_time.elapsed(),
        Some(&format!(
          "Visitor IP {} rejected by every candidate; redirected to the denied page",
          caller_ip
        )),
        None,
      )
      .await;
      return Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", redirect)
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::FOUND.into_response());
    }
    outcome @ (PickOutcome::NoRoute | PickOutcome::Denied(None)) => {
      // Stealth: a fully rejected visitor gets exactly the unclaimed-route
      // answer, so the route's existence never leaks to blocked IPs.
      let denied = matches!(outcome, PickOutcome::Denied(_));
      let reason = if denied {
        "Visitor IP rejected by every candidate (stealth answer)"
      } else {
        "No active client connection available"
      };
      // A resilient cached answer (possibly stale) beats the 504 — but never
      // for a denied visitor: serving cache would leak the route's existence.
      if !denied
        && let Some(resp) =
          stale_cache_response(&state, &method_str, &uri_str, &headers, start_time).await
      {
        return resp;
      }
      // Per-hostname fallback URL (`fallbacks:` section): redirect an
      // unclaimed hostname to a configured origin/status page instead of 504.
      // Never for a denied visitor (stealth) — the redirect would leak the
      // route's existence.
      if !denied {
        let cfg = state.config();
        if !cfg.fallbacks.is_empty()
          && let Some(rule) = cfg.fallbacks.matched(request_host.as_deref())
        {
          let path = uri_str.split('?').next().unwrap_or(&uri_str);
          let query = uri_str.split_once('?').map(|(_, q)| q);
          let location = crate::fallbacks::redirect_location(rule, path, query);
          let status = if rule.permanent {
            StatusCode::MOVED_PERMANENTLY
          } else {
            StatusCode::FOUND
          };
          log_request_success(
            &state,
            uuid::Uuid::new_v4().to_string(),
            &method_str,
            &uri_str,
            status.as_u16(),
            start_time.elapsed(),
            request_host.as_deref(),
            None,
            None,
            None,
          )
          .await;
          return Response::builder()
            .status(status)
            .header("location", location)
            .body(Body::empty())
            .unwrap_or_else(|_| status.into_response());
        }
      }
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        504,
        start_time.elapsed(),
        Some(reason),
        None,
      )
      .await;
      return gateway_timeout_response(
        &state,
        request_host.as_deref(),
        "504 Gateway Timeout - Client disconnected before request dispatch",
      );
    }
  };

  // Attribute the request span to the selected client (initial pick; failover
  // may re-dispatch to another client below).
  tracing::Span::current().record("aperio.client.id", selected.id.as_str());

  // Per-token rate limit / daily quota of the serving token (dynamic tokens
  // only). Enforced once at admission; failover re-dispatches of an already
  // admitted request are not double-counted.
  if let Err(reason) = state.check_token_limits(selected.token_id.as_deref()).await {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      429,
      start_time.elapsed(),
      Some(reason),
      selected.org_id.clone(),
    )
    .await;
    return (
      StatusCode::TOO_MANY_REQUESTS,
      format!("429 Too Many Requests - {}", reason),
    )
      .into_response();
  }

  // Per-organization monthly byte quota (max_bytes_month): once the serving
  // org is over budget for the calendar month, its traffic is refused.
  if state.org_over_month_bytes(selected.org_id.as_deref()).await {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      429,
      start_time.elapsed(),
      Some("Organization monthly byte quota exceeded"),
      selected.org_id.clone(),
    )
    .await;
    return (
      StatusCode::TOO_MANY_REQUESTS,
      "429 Too Many Requests - Organization monthly byte quota exceeded",
    )
      .into_response();
  }

  // Server-side response cache (APERIO_CACHE + the client's `cache: true`):
  // a fresh cached GET answer skips the tunnel round-trip entirely. Only
  // credential-less plain GETs qualify, and only responses whose
  // Cache-Control explicitly allowed shared caching were stored.
  let cache_eligible = state.config().cache_enabled
    && selected.cache
    && crate::cache::request_cacheable(&method_str, &headers);
  let cache_key = crate::cache::cache_key(request_host.as_deref(), &uri_str);
  // Single-flight coalescing: the first cacheable miss for a key becomes the
  // leader (the guard below); concurrent identical misses wait for it and
  // re-check the cache instead of stampeding the backend on cache expiry.
  // The guard is held until this handler returns — by then the leader's
  // response is cached (or proved uncacheable) — and its Drop wakes waiters
  // on every exit path, including errors and failover.
  let mut _cache_single_flight: Option<CacheSingleFlight> = None;
  if cache_eligible {
    let mut waited = false;
    loop {
      let lookup = state.response_cache.lock().await.lookup(
        &cache_key,
        std::time::Duration::from_secs(state.config().cache_max_stale),
      );
      let served_hit = match lookup {
        crate::cache::SwrLookup::Fresh(hit) => Some(hit),
        crate::cache::SwrLookup::StaleRevalidate { hit, lead } => {
          if lead {
            // The revalidation request carries the visitor's headers minus
            // the conditionals, so the backend answers with a full 200.
            let reval_headers: Vec<(String, String)> = headers
              .iter()
              .filter_map(|(k, v)| {
                let name = k.as_str().to_ascii_lowercase();
                if matches!(
                  name.as_str(),
                  "if-none-match"
                    | "if-modified-since"
                    | "connection"
                    | "keep-alive"
                    | "upgrade"
                    | "accept-encoding"
                ) {
                  return None;
                }
                v.to_str().ok().map(|val| (k.to_string(), val.to_string()))
              })
              .collect();
            spawn_swr_revalidation(
              state.clone(),
              cache_key.clone(),
              uri_str.clone(),
              reval_headers,
              selected.id.clone(),
              selected.tx.clone(),
              selected.resilience,
            );
          }
          Some(hit)
        }
        crate::cache::SwrLookup::Miss => None,
      };
      if let Some(hit) = served_hit {
        let duration = start_time.elapsed();
        let (status, body_len, response) = cache_hit_response(hit, &headers);
        {
          let mut stats = state.stats.lock().await;
          stats.total_requests += 1;
          stats.successful_requests += 1;
          stats.total_bytes_transferred += body_len;
        }
        state.persistent_stats.lock().await.record_request_labeled(
          true,
          0,
          body_len,
          duration.as_millis() as u64,
          Some(selected.token_name.as_deref().unwrap_or("master")),
          request_host.as_deref(),
          selected.org_id.as_deref(),
        );
        let request_id = uuid::Uuid::new_v4().to_string();
        log_request_success(
          &state,
          request_id,
          &method_str,
          &uri_str,
          status,
          duration,
          request_host.as_deref(),
          Some(&selected.id),
          selected.token_name.as_deref(),
          selected.org_id.clone(),
        )
        .await;
        telemetry::record_status(&tracing::Span::current(), status);
        return response;
      }
      // Followers wait at most once: if the leader's response turned out to
      // be uncacheable there is nothing to coalesce onto — dispatch normally.
      if waited {
        break;
      }
      let follow_rx = {
        let mut inflight = state
          .cache_inflight
          .lock()
          .expect("cache_inflight lock poisoned");
        match inflight.get(&cache_key) {
          Some(rx) => Some(rx.clone()),
          None => {
            let (tx, rx) = tokio::sync::watch::channel(false);
            inflight.insert(cache_key.clone(), rx);
            _cache_single_flight = Some(CacheSingleFlight {
              state: state.clone(),
              key: cache_key.clone(),
              _done: tx,
            });
            None
          }
        }
      };
      match follow_rx {
        // Leader: fall through to the normal dispatch below.
        None => break,
        Some(mut rx) => {
          waited = true;
          // `changed()` returns immediately once the leader's guard drops
          // (the sender is dropped with it); the timeout only bounds a
          // leader that itself hangs on the gateway timeout.
          let _ = tokio::time::timeout(state.config().gateway_response_timeout, rx.changed()).await;
        }
      }
    }
  }

  // Protocol v2 upload streaming: large (or chunked) request bodies are
  // forwarded as RequestStart/Chunk/End frames instead of being buffered,
  // when the selected client speaks v2. Streamed requests cannot fail over
  // (the body is consumed as it is forwarded).
  let content_length = headers
    .get("content-length")
    .and_then(|v| v.to_str().ok())
    .and_then(|v| v.parse::<u64>().ok());
  let chunked_upload = headers
    .get("transfer-encoding")
    .and_then(|v| v.to_str().ok())
    .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
  // Effective request body cap: the service's own declared limit (Ping
  // `max_request_body`) can only tighten the global APERIO_MAX_BODY_SIZE.
  let body_limit = effective_body_limit(state.config().max_body_size, selected.max_request_body);
  // Declared over-limit bodies keep failing fast with 413 even when they
  // would otherwise be streamed.
  if content_length.is_some_and(|l| l as usize > body_limit) {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      413,
      start_time.elapsed(),
      Some("Declared content-length exceeds the body size limit"),
      selected.org_id.clone(),
    )
    .await;
    return (
      StatusCode::PAYLOAD_TOO_LARGE,
      "413 Payload Too Large - Request body size exceeds limit",
    )
      .into_response();
  }
  let stream_request = selected.protocol.unwrap_or(1) >= 2
    && (chunked_upload || content_length.is_some_and(|l| l > REQUEST_STREAM_THRESHOLD));
  // Bytes forwarded by the streamed-body pump (for stats attribution).
  let streamed_bytes = Arc::new(AtomicU64::new(0));

  // 5. Read body with limit to prevent OOM / DoS (buffered requests only)
  let mut streamed_body: Option<Body> = None;
  let body_bytes = if stream_request {
    streamed_body = Some(body);
    axum::body::Bytes::new()
  } else {
    match axum::body::to_bytes(body, body_limit).await {
      Ok(bytes) => bytes,
      Err(e) => {
        log_request_failure(
          &state,
          &method_str,
          &uri_str,
          413,
          start_time.elapsed(),
          Some(&format!("Payload too large or read failure: {}", e)),
          selected.org_id.clone(),
        )
        .await;
        return (
          StatusCode::PAYLOAD_TOO_LARGE,
          "413 Payload Too Large - Request body size exceeds limit",
        )
          .into_response();
      }
    }
  };

  // WAF-lite body-size rules (`waf:` with `max_body`): reject an oversized
  // body on a matched route with 413, now that the length is known. Streamed
  // request bodies (protocol v2) are governed only by the global body limit.
  if !stream_request {
    let cfg = state.config();
    if !cfg.waf.is_empty()
      && let Some(reason) =
        cfg
          .waf
          .body_reason(&method_str, &uri_path_owned, &headers, body_bytes.len())
    {
      let reason = reason.to_string();
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        413,
        start_time.elapsed(),
        Some(&format!("WAF body-size deny: {reason}")),
        selected.org_id.clone(),
      )
      .await;
      return (
        StatusCode::PAYLOAD_TOO_LARGE,
        "413 Payload Too Large - Blocked by WAF",
      )
        .into_response();
    }
  }

  let base64_body = if body_bytes.is_empty() {
    None
  } else {
    use base64::prelude::*;
    Some(BASE64_STANDARD.encode(&body_bytes))
  };

  // Map headers (preserve duplicates by collecting into a Vec).
  // Filter out internal aperio session cookies to prevent leaking dashboard
  // session tokens to tunnel clients.
  // When OTLP export is on we replace any inbound W3C trace headers with this
  // span's context; when off, `trace_headers` is empty and inbound headers
  // pass through unchanged.
  let inject_trace = !trace_headers.is_empty();
  let mut serialized_headers: Vec<(String, String)> = Vec::new();
  for (k, v) in headers.iter() {
    if let Ok(val_str) = v.to_str() {
      if inject_trace {
        let k_lower = k.as_str().to_ascii_lowercase();
        if k_lower == "traceparent" || k_lower == "tracestate" {
          continue;
        }
      }
      if k.as_str() == "cookie" {
        let filtered: String = val_str
          .split(';')
          .filter(|part| {
            let trimmed = part.trim();
            // Internal aperio cookies never reach backends.
            !trimmed.starts_with("aperio_session=")
              && !trimmed.starts_with("aperio_share=")
              && !trimmed.starts_with("aperio_affinity=")
          })
          .map(|part| part.trim())
          .collect::<Vec<&str>>()
          .join("; ");
        if !filtered.is_empty() {
          serialized_headers.push((k.to_string(), filtered));
        }
        continue;
      }
      serialized_headers.push((k.to_string(), val_str.to_string()));
    }
  }
  // Forward this span's trace context to the backend (empty when OTLP is off).
  serialized_headers.extend(trace_headers);

  // Server-side `headers.request` rewrite rules (aperio-server.yaml), applied
  // before the inspector capture so replay and capture match what was sent.
  let serialized_headers = state
    .config()
    .header_rules
    .request
    .apply(serialized_headers);

  // Capture (truncated) request data for the dashboard inspector before the
  // originals are moved into the tunnel message. Streamed bodies are not
  // captured (marked truncated, which also disables replay).
  let capture_req_headers = serialized_headers.clone();
  let (capture_req_body, capture_req_truncated) = if stream_request {
    (None, true)
  } else {
    use base64::prelude::*;
    if body_bytes.is_empty() {
      (None, false)
    } else if body_bytes.len() > CAPTURE_BODY_LIMIT {
      (
        Some(BASE64_STANDARD.encode(&body_bytes[..CAPTURE_BODY_LIMIT])),
        true,
      )
    } else {
      (Some(BASE64_STANDARD.encode(&body_bytes)), false)
    }
  };

  // Update traffic metrics once per visitor request, regardless of how many
  // failover attempts it takes.
  {
    let mut stats = state.stats.lock().await;
    stats.total_requests += 1;
    stats.total_bytes_transferred += body_bytes.len() as u64;
  }

  // 6. Dispatch and await the response. When the assigned client is lost
  // before answering (nothing has been sent to the visitor yet), the
  // configured failover mode may re-dispatch the request to another client
  // or wait for one to return, bounded by max-jumps and the time window.
  let mut jumps_used: u32 = 0;
  // The failover window starts ticking at the first in-flight failure.
  let mut failover_deadline: Option<tokio::time::Instant> = None;

  loop {
    // Honor the client's announced concurrency limit: wait (up to the gateway
    // timeout) for an in-flight slot instead of flooding the client's backend.
    let _inflight_permit = match selected.inflight_limiter.clone() {
      Some(limiter) => {
        match tokio::time::timeout(state.config().gateway_timeout, limiter.acquire_owned()).await {
          Ok(Ok(permit)) => Some(permit),
          _ => {
            log_request_failure(
              &state,
              &method_str,
              &uri_str,
              429,
              start_time.elapsed(),
              Some("Client concurrency limit: no slot freed within gateway timeout"),
              selected.org_id.clone(),
            )
            .await;
            break (
              StatusCode::TOO_MANY_REQUESTS,
              "429 Too Many Requests - Tunnel client concurrency limit reached",
            )
              .into_response();
          }
        }
      }
      None => None,
    };

    // Increment request stats for the chosen client.
    selected.request_count.fetch_add(1, Ordering::SeqCst);

    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx_response, rx_response) = oneshot::channel::<TunnelResponse>();

    // Insert oneshot receiver to await response mapping
    {
      let mut pending = state.pending_requests.lock().await;
      pending.insert(
        request_id.clone(),
        PendingRequest {
          tx: tx_response,
          client_id: selected.id.clone(),
        },
      );
    }

    // Dispatch: buffered requests go out as a single Request message;
    // streamed requests send RequestStart here and a pump task feeds the
    // body as raw binary chunk frames.
    let dispatch_msg = if stream_request {
      TunnelMessage::RequestStart {
        id: request_id.clone(),
        method: method_str.clone(),
        uri: uri_str.clone(),
        headers: serialized_headers.clone(),
      }
    } else {
      TunnelMessage::Request {
        id: request_id.clone(),
        method: method_str.clone(),
        uri: uri_str.clone(),
        headers: serialized_headers.clone(),
        body: base64_body.clone(),
      }
    };

    let req_json = match serde_json::to_string(&dispatch_msg) {
      Ok(json) => json,
      Err(e) => {
        state.pending_requests.lock().await.remove(&request_id);
        log_request_failure(
          &state,
          &method_str,
          &uri_str,
          500,
          start_time.elapsed(),
          Some(&format!("Request serialization failed: {}", e)),
          selected.org_id.clone(),
        )
        .await;
        break (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
      }
    };

    // A failed send means the client is already gone; it goes through the
    // same failover decision as an in-flight connection loss.
    let dispatched_at = Instant::now();
    let dispatched = selected.tx.send(Message::Text(req_json)).await.is_ok();
    if !dispatched {
      state.pending_requests.lock().await.remove(&request_id);
    } else if let Some(raw_body) = streamed_body.take() {
      // Pump the visitor's body through the tunnel without buffering it.
      let pump_tx = selected.tx.clone();
      let pump_id = request_id.clone();
      let pump_state = state.clone();
      let counter = streamed_bytes.clone();
      let max_body = body_limit;
      tokio::spawn(async move {
        let mut stream = raw_body.into_data_stream();
        let mut total: usize = 0;
        while let Some(chunk) = stream.next().await {
          match chunk {
            Ok(bytes) => {
              total += bytes.len();
              if total > max_body {
                warn!(
                  "Streamed request {} exceeded the max body size; truncating the upload",
                  pump_id
                );
                break;
              }
              counter.fetch_add(bytes.len() as u64, Ordering::Relaxed);
              {
                let mut stats = pump_state.stats.lock().await;
                stats.total_bytes_transferred += bytes.len() as u64;
              }
              if pump_tx
                .send(Message::Binary(encode_binary_frame(
                  FRAME_REQUEST_CHUNK,
                  &pump_id,
                  &bytes,
                )))
                .await
                .is_err()
              {
                break;
              }
            }
            Err(e) => {
              warn!("Request body stream error for {}: {}", pump_id, e);
              break;
            }
          }
        }
        if let Ok(json) = serde_json::to_string(&TunnelMessage::RequestEnd { id: pump_id }) {
          let _ = pump_tx.send(Message::Text(json)).await;
        }
      });
    }

    // Await the response with the per-attempt response timeout: the serving
    // client's per-service `response_timeout` override when it declared one,
    // otherwise the global gateway response timeout.
    // A declared 0 means "use the global value" (not a zero-second timeout that
    // would fail every request instantly), matching the global timeout's own
    // `.max(1)` clamp.
    let response_timeout = selected
      .response_timeout
      .filter(|s| *s > 0)
      .map(std::time::Duration::from_secs)
      .unwrap_or_else(|| state.config().gateway_response_timeout);
    let outcome: Option<TunnelResponse> = if dispatched {
      let timeout_fut = tokio::time::sleep(response_timeout);
      tokio::pin!(timeout_fut);
      tokio::select! {
          _ = &mut timeout_fut => {
              state.pending_requests.lock().await.remove(&request_id);
              log_request_failure(
                  &state,
                  &method_str,
                  &uri_str,
                  504,
                  start_time.elapsed(),
                  Some("Client response timeout expired"),
                selected.org_id.clone(),
              )
              .await;
              state.persistent_stats.lock().await.record_request(false, body_bytes.len() as u64, 0, start_time.elapsed().as_millis() as u64, selected.org_id.as_deref());
              // Passive outlier ejection: a response timeout is a failure.
              record_outlier_failure(&state, &selected.id).await;
              break gateway_timeout_response(&state, request_host.as_deref(), "504 Gateway Timeout - Gateway response timeout expired");
          }
          res_opt = rx_response => res_opt.ok(),
      }
    } else {
      None
    };

    let duration = start_time.elapsed();
    match outcome {
      Some(mut tunnel_res) => {
        let response_received_at = Instant::now();
        // Server-side `headers.response` rewrite rules (aperio-server.yaml),
        // applied before every consumer — the visitor response, the response
        // cache and the inspector capture — so all views agree.
        tunnel_res.headers = state
          .config()
          .header_rules
          .response
          .apply(std::mem::take(&mut tunnel_res.headers));
        // Preview noindex: responses served via a random subdomain carry
        // X-Robots-Tag so search engines never index preview environments
        // (applied here so the cache and the inspector agree too).
        if state.config().preview_noindex
          && let Some(ref pattern) = state.config().random_subdomain_suffix
          && request_host
            .as_deref()
            .is_some_and(|h| crate::routing::host_matches_random_pattern(h, pattern))
        {
          tunnel_res
            .headers
            .retain(|(k, _)| !k.eq_ignore_ascii_case("x-robots-tag"));
          tunnel_res
            .headers
            .push(("x-robots-tag".to_string(), "noindex, nofollow".to_string()));
        }
        let status_code =
          StatusCode::from_u16(tunnel_res.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        // Passive outlier ejection: a server error counts against the serving
        // client (whether or not it is retried below).
        if status_code.is_server_error() {
          record_outlier_failure(&state, &selected.id).await;
        }

        // Transparent retry on a buffered server error (APERIO_RETRY_ON_5XX):
        // a fully-buffered 5xx the retry policy covers is re-dispatched to
        // another client instead of being returned. No response bytes have
        // reached the visitor yet, so this is safe for retryable methods.
        // Streamed responses and streamed request bodies are never retried.
        let cfg = state.config();
        if tunnel_res.stream_rx.is_none()
          && !stream_request
          && retry_covers(cfg.retry_on_5xx, &cfg.retry_statuses, tunnel_res.status)
          && method_retryable(&method_str, cfg.failover_all_methods)
          && jumps_used < cfg.failover_max_jumps
        {
          let next = match pick_proxy_client(
            &state,
            &uri_path_owned,
            request_host.as_deref(),
            None,
            None,
            Some(caller_ip),
          )
          .await
          {
            crate::routing::PickOutcome::Selected(c) => Some(*c),
            _ => None,
          };
          if let Some(next_client) = next {
            jumps_used += 1;
            warn!(
              "5xx retry: {} {} returned {} from client {}, re-dispatching to {} (jump {}/{})",
              method_str,
              uri_path_owned,
              tunnel_res.status,
              selected.id,
              next_client.id,
              jumps_used,
              cfg.failover_max_jumps
            );
            selected = next_client;
            continue;
          }
        }

        let res_bytes = if let Some(ref encoded_body) = tunnel_res.body {
          use base64::prelude::*;
          BASE64_STANDARD.decode(encoded_body).unwrap_or_default()
        } else {
          Vec::new()
        };

        let body_len = res_bytes.len() as u64;

        let mut response_builder = Response::builder().status(status_code);

        // Sticky sessions: pin this visitor to the client that just served
        // them. The instance ID is preferred so affinity survives client
        // reconnects; the connection ID is the fallback.
        if state.config().lb_strategy == LbStrategy::Sticky {
          let affinity_value = selected.instance_id.as_deref().unwrap_or(&selected.id);
          let secure_flag = if state.config().secure_cookies {
            "; Secure"
          } else {
            ""
          };
          response_builder = response_builder.header(
            "set-cookie",
            format!(
              "aperio_affinity={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400{}",
              affinity_value, secure_flag
            ),
          );
        }

        for (k, v) in tunnel_res.headers.iter() {
          let k_lower = k.to_lowercase();
          // Strip connection management headers
          if k_lower == "connection" || k_lower == "keep-alive" || k_lower == "transfer-encoding" {
            continue;
          }
          if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
          ) {
            response_builder = response_builder.header(name, value);
          }
        }

        {
          let mut stats = state.stats.lock().await;
          // Only count server errors (5xx) as failed. 2xx/3xx/4xx are
          // legitimate responses successfully proxied through the tunnel.
          if status_code.is_server_error() {
            stats.failed_requests += 1;
          } else {
            stats.successful_requests += 1;
          }
          // Streamed bodies are counted chunk-by-chunk as they arrive.
          stats.total_bytes_transferred += body_len;
        }

        // Persistent (restart-surviving) counters, attributed to the token
        // and hostname for per-tenant traceability.
        {
          let mut ps = state.persistent_stats.lock().await;
          ps.record_request_labeled(
            !status_code.is_server_error(),
            body_bytes.len() as u64 + streamed_bytes.load(Ordering::Relaxed),
            body_len,
            duration.as_millis() as u64,
            Some(selected.token_name.as_deref().unwrap_or("master")),
            request_host.as_deref(),
            selected.org_id.as_deref(),
          );
        }
        // Store cacheable buffered GET responses (streamed responses are never
        // cached). A 200 honors the advertised Cache-Control lifetime; a
        // 404/410 may be negatively cached for a short TTL
        // (APERIO_CACHE_NEGATIVE_TTL) to shield a backend from repeated misses.
        if cache_eligible && tunnel_res.stream_rx.is_none() {
          let ttl = if status_code == StatusCode::OK {
            crate::cache::response_cache_ttl(&tunnel_res.headers)
          } else if matches!(tunnel_res.status, 404 | 410)
            && !crate::cache::response_uncacheable(&tunnel_res.headers)
          {
            let neg = crate::cache::negative_cache_ttl();
            (!neg.is_zero()).then_some(neg)
          } else {
            None
          };
          if let Some(ttl) = ttl {
            let surrogate = crate::cache::response_surrogate_keys(&tunnel_res.headers);
            state.response_cache.lock().await.insert(
              cache_key.clone(),
              tunnel_res.status,
              tunnel_res.headers.clone(),
              res_bytes.clone(),
              ttl,
              state.config().cache_max_bytes,
              selected.resilience,
              crate::cache::response_swr_window(&tunnel_res.headers),
              surrogate,
            );
          }
        }

        // Feed the serving token's daily byte quota (request + response).
        state
          .add_token_bytes(
            selected.token_id.as_deref(),
            body_bytes.len() as u64 + streamed_bytes.load(Ordering::Relaxed) + body_len,
          )
          .await;

        log_request_success(
          &state,
          request_id.clone(),
          &method_str,
          &uri_str,
          tunnel_res.status,
          duration,
          request_host.as_deref(),
          Some(&selected.id),
          selected.token_name.as_deref(),
          selected.org_id.clone(),
        )
        .await;

        // Capture the transaction for the dashboard inspector.
        {
          use base64::prelude::*;
          let resp_streamed = tunnel_res.stream_rx.is_some();
          let (resp_body_cap, resp_truncated) = if resp_streamed || res_bytes.is_empty() {
            (None, false)
          } else if res_bytes.len() > CAPTURE_BODY_LIMIT {
            (
              Some(BASE64_STANDARD.encode(&res_bytes[..CAPTURE_BODY_LIMIT])),
              true,
            )
          } else {
            (Some(BASE64_STANDARD.encode(&res_bytes)), false)
          };
          let mut captured = state.captured_requests.lock().await;
          if captured.len() >= CAPTURE_MAX_ENTRIES {
            captured.pop_front();
          }
          let us = |at: Instant| at.duration_since(start_time).as_micros() as u64;
          let timeline = crate::state::RequestTimeline::assemble(
            us(dispatched_at),
            us(response_received_at),
            start_time.elapsed().as_micros() as u64,
            tunnel_res.timings,
          );
          state.stage_stats.lock().await.record(
            request_host.as_deref(),
            selected.org_id.as_deref(),
            &timeline,
          );
          captured.push_back(CapturedRequest {
            id: request_id.clone(),
            timestamp: Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
            method: method_str.clone(),
            uri: uri_str.clone(),
            req_headers: capture_req_headers.clone(),
            req_body: capture_req_body.clone(),
            req_body_truncated: capture_req_truncated,
            status: tunnel_res.status,
            resp_headers: tunnel_res.headers.clone(),
            resp_body: resp_body_cap,
            resp_body_truncated: resp_truncated,
            resp_streamed,
            duration_ms: duration.as_millis(),
            timeline: Some(timeline),
            org_id: selected.org_id.clone(),
          });
        }

        // Webhook inbox: services that opted in (`webhook_inbox: true`) get
        // every inbound POST persisted for browsing and re-firing.
        if selected.webhook_inbox && method_str.eq_ignore_ascii_case("POST") {
          state
            .inbox_store
            .lock()
            .await
            .insert(crate::store::inbox::InboxEntry {
              id: request_id.clone(),
              timestamp: Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
              method: method_str.clone(),
              uri: uri_str.clone(),
              host: request_host.clone(),
              headers: capture_req_headers.clone(),
              body: capture_req_body.clone(),
              body_truncated: capture_req_truncated || stream_request,
              status: tunnel_res.status,
              service: selected.service_name.clone(),
              org_id: selected.org_id.clone(),
            });
        }

        // Streamed response: forward frames as they arrive without
        // buffering; a trailer block (e.g. gRPC's grpc-status) becomes the
        // final HTTP frame. Buffered responses with trailers get a
        // two-frame body; plain buffered responses stay a simple body.
        let body = if let Some(chunk_rx) = tunnel_res.stream_rx.take() {
          let stream = futures_util::stream::unfold(chunk_rx, |mut rx| async move {
            rx.recv().await.map(|item| (frame_from_body_item(item), rx))
          });
          Body::new(http_body_util::StreamBody::new(stream))
        } else if let Some(trailers) = tunnel_res.trailers.take() {
          let frames: Vec<Result<http_body::Frame<axum::body::Bytes>, axum::BoxError>> = vec![
            Ok(http_body::Frame::data(axum::body::Bytes::from(
              res_bytes.clone(),
            ))),
            Ok(http_body::Frame::trailers(trailer_header_map(&trailers))),
          ];
          Body::new(http_body_util::StreamBody::new(futures_util::stream::iter(
            frames,
          )))
        } else {
          Body::from(res_bytes)
        };

        break match response_builder.body(body) {
          Ok(r) => r,
          Err(e) => {
            error!("Error constructing response: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
          }
        };
      }
      None => {
        // The client vanished before answering. No response bytes
        // have reached the visitor yet, so a failover re-dispatch
        // is safe (for retryable methods).
        // Passive outlier ejection: a vanished client is a failure.
        record_outlier_failure(&state, &selected.id).await;
        let can_failover = !stream_request
          && state.config().failover_mode != FailoverMode::Fail
          && method_retryable(&method_str, state.config().failover_all_methods)
          && jumps_used < state.config().failover_max_jumps;
        if can_failover {
          jumps_used += 1;
          let deadline = *failover_deadline
            .get_or_insert_with(|| tokio::time::Instant::now() + state.config().failover_window);
          let next = match state.config().failover_mode {
            FailoverMode::Retry => {
              // The visitor's IP eligibility is re-checked per candidate on
              // the re-dispatch too (a denied outcome maps to no candidate).
              match pick_proxy_client(
                &state,
                &uri_path_owned,
                request_host.as_deref(),
                None,
                None,
                Some(caller_ip),
              )
              .await
              {
                crate::routing::PickOutcome::Selected(c) => Some(*c),
                _ => None,
              }
            }
            FailoverMode::Wait => {
              // Wait for the same client process to return; when it
              // never reported an instance ID, any candidate counts.
              wait_for_candidate(
                &state,
                &uri_path_owned,
                request_host.as_deref(),
                selected.instance_id.as_deref(),
                deadline,
                Some(caller_ip),
              )
              .await
            }
            FailoverMode::RetryWait => {
              wait_for_candidate(
                &state,
                &uri_path_owned,
                request_host.as_deref(),
                None,
                deadline,
                Some(caller_ip),
              )
              .await
            }
            FailoverMode::Fail => None,
          };
          if let Some(next_client) = next {
            warn!(
              "In-flight failover: {} {} re-dispatched from client {} to {} (jump {}/{})",
              method_str,
              uri_path_owned,
              selected.id,
              next_client.id,
              jumps_used,
              state.config().failover_max_jumps
            );
            selected = next_client;
            continue;
          }
        }
        log_request_failure(
          &state,
          &method_str,
          &uri_str,
          502,
          duration,
          Some("Communication channel with client closed abruptly"),
          selected.org_id.clone(),
        )
        .await;
        state.persistent_stats.lock().await.record_request(
          false,
          body_bytes.len() as u64,
          0,
          duration.as_millis() as u64,
          selected.org_id.as_deref(),
        );
        break (
          StatusCode::BAD_GATEWAY,
          "502 Bad Gateway - Client connection lost in flight",
        )
          .into_response();
      }
    }
  }
}

/// Maps a relayed body frame to an HTTP body frame (data or trailers).
fn frame_from_body_item(
  item: Result<crate::state::BodyFrame, std::io::Error>,
) -> Result<http_body::Frame<axum::body::Bytes>, axum::BoxError> {
  match item {
    Ok(crate::state::BodyFrame::Data(bytes)) => {
      Ok(http_body::Frame::data(axum::body::Bytes::from(bytes)))
    }
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

#[cfg(test)]
mod retry_tests {
  use super::retry_covers;

  #[test]
  fn retry_covers_respects_switch_and_status_set() {
    // Off: never retries.
    assert!(!retry_covers(false, &[], 500));
    // On, no explicit set: every 5xx, but not 4xx/2xx.
    assert!(retry_covers(true, &[], 500));
    assert!(retry_covers(true, &[], 503));
    assert!(retry_covers(true, &[], 599));
    assert!(!retry_covers(true, &[], 404));
    assert!(!retry_covers(true, &[], 200));
    // On, explicit set: only the listed codes.
    assert!(retry_covers(true, &[502, 503, 504], 503));
    assert!(!retry_covers(true, &[502, 503, 504], 500));
  }
}
