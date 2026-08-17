//! Forwarding one request over the tunnel and streaming the answer back.
//!
//! One function, deliberately whole. It is the request path: it holds a
//! dispatch slot, a pending-response registration, a body pump and a set of
//! failure counters that all have to be released on every exit, and the exits
//! are many (timeout, abort, client loss, body limit, retry). Cutting it up
//! means handing that state between the pieces, and a piece that returns early
//! without releasing its share is exactly the leak the single scope prevents.

use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use super::*;
use crate::state::AppState;

/// Forwards a buffered/streamed HTTP request over the tunnel and maps the
/// response back. Split out of [`proxy_handler`] so the whole flow runs inside
/// one instrumented request span.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn proxy_http_request(
  state: Arc<AppState>,
  method: Method,
  uri: axum::http::Uri,
  headers: HeaderMap,
  body: Body,
  caller_ip: std::net::IpAddr,
  trace_headers: Vec<(String, String)>,
) -> Response {
  let method_str = method.to_string();
  // The gate's own query parameter is stripped before anything downstream
  // sees it: it is a credential for Aperio, and a backend has no more business
  // reading it than it has reading the session cookie, which is stripped for
  // the same reason. The access log already keeps only the path, and the
  // inspector masks the value, so this closes the last place it travelled.
  let uri_str = uri_without_token(&uri);
  let start_time = Instant::now();

  // 1. Per-IP Rate Limiting (Token Bucket)
  if !state.check_rate_limit(caller_ip).await {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      429,
      start_time.elapsed(),
      Some(&format!("{} (IP {})", Limit::Ip.log_detail(), caller_ip)),
      None,
    )
    .await;
    return refuse(&state, Limit::Ip);
  }

  // 2. Visitor-auth gate: a client-declared per-service password (if any)
  // supersedes the server's own visitor password / OIDC; public routes skip it.
  // Held rather than returned when the verdict is `Undeclared`: the thing
  // that would declare this route open is a client, and under scale-to-zero
  // it is asleep. The question is asked again below, once the cold start has
  // had its chance to wake one.
  let mut undeclared: Option<Response> = None;
  let mut visitor = match check_visitor_gate(
    &state,
    &method,
    &headers,
    &uri,
    extract_request_host(&headers).as_deref(),
    caller_ip,
  )
  .await
  {
    VisitorGate::Deny(resp) => return resp,
    VisitorGate::Allow(identity) => identity,
    VisitorGate::Undeclared(resp) => {
      undeclared = Some(resp);
      None
    }
  };

  // Client-declared visitor IP allowlists are enforced per candidate during
  // client selection below: the request dispatches to any candidate whose
  // own list admits the visitor, and a fully rejected visitor gets the
  // winning `denied:` redirect, or a stealth answer identical to an
  // unclaimed route, so blocked traffic still never enters the tunnel.

  // 3. Wait for connection if client is disconnected.
  // Take a consistent snapshot of connection state under a single lock to avoid TOCTOU.
  //
  // Asked of *this route*, not of the server, and only then asked whether
  // waiting could help. See `worth_waiting_for_route` for why those are two
  // questions: one flag answering both is what let a dead route skip the wait
  // because a neighbour was up, and what made a long-dead one wait anyway.
  let (_, last_disconnect) = {
    let conn = state.connection_state.lock().await;
    (conn.connected, conn.last_disconnect)
  };
  let host_for_route = extract_request_host(&headers);
  let route_up = || {
    crate::routing::route_exists(
      &state,
      uri.path(),
      host_for_route.as_deref(),
      Some(caller_ip),
    )
  };
  let scaling_enabled = state.config().scaling_enabled;
  if !route_up().await
    && worth_waiting_for_route(
      last_disconnect,
      Instant::now(),
      state.config().gateway_timeout,
      scaling_enabled,
    )
  {
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
              if res.is_ok() && *rx.borrow() && route_up().await {
                  reconnected = true;
                  break;
              }
          }
      }
    }

    // Scale-to-zero means *no* client is connected, so this global wait is
    // exactly where a cold start has to happen: without it the request would
    // 504 here and never reach the per-route check further down.
    let mut recovered = reconnected;
    if !recovered && state.config().scaling_enabled {
      crate::scaling::cold_start_wait(
        &state,
        extract_request_host(&headers).as_deref(),
        uri.path(),
        caller_ip,
      )
      .await;
      recovered = route_up().await;
    }

    if !recovered {
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

  // The second asking. A client may have arrived while the cold start ran, and
  // if it has, its own declaration decides exactly as it would have done had
  // it never slept. If none arrived, the answer held from the first asking is
  // the one to give, which is the same 504 an unclaimed hostname gets.
  if let Some(resp) = undeclared {
    match check_visitor_gate(
      &state,
      &method,
      &headers,
      &uri,
      extract_request_host(&headers).as_deref(),
      caller_ip,
    )
    .await
    {
      VisitorGate::Allow(identity) => visitor = identity,
      VisitorGate::Deny(denied) => return denied,
      VisitorGate::Undeclared(_) => {
        // Nothing declares this route open, which under the closed posture is
        // a stealth refusal. Except that a *resilient* cached answer is
        // itself the declaration, and this is the one condition it exists
        // for: an entry is only consulted here once its client is gone, so
        // refusing makes `resilience: true` work exactly while nobody needs
        // it.
        //
        // This is #119. The intermittent 504 was this refusal, reached
        // whenever the request arrived with the route unserved and the
        // posture closed; the runs that passed had taken the reconnect path
        // and served the entry from further down.
        //
        // It cannot disclose a route nothing declared: `get_for_outage`
        // returns nothing for a key whose client never asked for serve-stale,
        // which the second test beside this one pins.
        if let Some(stale) =
          stale_cache_response(&state, &method_str, &uri_str, &headers, start_time).await
        {
          return stale;
        }
        return resp;
      }
    }
  }

  // A connected client is available (or was never waited for). Trace boundary
  // for the pre-dispatch sub-phases (no-op unless OTLP is on).
  let client_ready_at = Instant::now();

  // 4. Limit concurrency to prevent resource starvation / DoS.
  // Kept in an Option so the cold-start hold below can *release* it: waiting
  // up to a minute for a service to start while holding a global concurrency
  // slot would starve every healthy service on the server.
  let mut permit = match state.try_acquire_request_slot() {
    Some(p) => Some(p),
    None => {
      log_request_failure(
        &state,
        &method_str,
        &uri_str,
        429,
        start_time.elapsed(),
        Some(&Limit::ServerConcurrency.log_detail()),
        None,
      )
      .await;
      return refuse(&state, Limit::ServerConcurrency);
    }
  };

  // Admitted past the server-wide concurrency limit.
  let admitted_at = Instant::now();

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

  // Per-route rate limit (a route's own `rate_limit:`, else the `rate_limits:`
  // section): a shared token bucket caps aggregate rps to a matched
  // host+path+method, protecting expensive endpoints.
  if !state
    .check_route_rate_limit(request_host.as_deref(), &uri_path_owned, &method_str)
    .await
  {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      429,
      start_time.elapsed(),
      Some(&format!(
        "{} (path {})",
        Limit::Route.log_detail(),
        uri_path_owned
      )),
      None,
    )
    .await;
    return refuse(&state, Limit::Route);
  }

  // Sticky strategy: a returning visitor carries an affinity cookie naming
  // the client that served them before.
  let affinity = if state.config().lb_strategy == LbStrategy::Sticky {
    cookie_value(&headers, "aperio_affinity")
  } else {
    None
  };
  // Cold start (scale-to-zero): when nothing serves this route and an
  // autoscaling record is armed for it, ask for capacity and hold the request
  // for the record's budget instead of answering 504. The request was never
  // dispatched, so holding it is safe for any method, unlike a failover
  // re-dispatch.
  // This only asks whether anything serves the route, so it must not go
  // through `pick_proxy_client`, which rotates the group's round-robin cursor.
  if state.config().scaling_enabled
    && !crate::routing::route_exists(
      &state,
      &uri_path_owned,
      request_host.as_deref(),
      Some(caller_ip),
    )
    .await
  {
    // Release the global slot first: the hold can last tens of seconds.
    drop(permit.take());
    crate::scaling::cold_start_wait(&state, request_host.as_deref(), &uri_path_owned, caller_ip)
      .await;
    permit = match state.try_acquire_request_slot() {
      Some(p) => Some(p),
      None => {
        log_request_failure(
          &state,
          &method_str,
          &uri_str,
          429,
          start_time.elapsed(),
          Some(&format!(
            "{} (after a cold start)",
            Limit::ServerConcurrency.log_detail()
          )),
          None,
        )
        .await;
        return refuse(&state, Limit::ServerConcurrency);
      }
    };
  }
  // From here the slot is held for the rest of the request; the binding keeps
  // the guard alive without the cold-start branch being able to touch it again.
  let _permit = permit;

  // Canary split for this route, decided once and reused for every re-dispatch
  // below: a failover that landed a visitor on the other version would make
  // the split mean nothing precisely when something is going wrong.
  let canary_side = state
    .config()
    .static_routes
    .policy_for(request_host.as_deref(), &uri_path_owned)
    .and_then(|rule| rule.canary.as_ref())
    .map(|rule| {
      let sent = rule
        .header
        .as_deref()
        .and_then(|name| headers.get(name))
        .and_then(|v| v.to_str().ok());
      (rule.service.clone(), rule.side_for(sent, Some(caller_ip)))
    });
  let canary = canary_side
    .as_ref()
    .map(|(service, side)| (service.as_str(), *side));

  let mut selected = match pick_proxy_client(
    &state,
    &uri_path_owned,
    request_host.as_deref(),
    None,
    affinity.as_deref(),
    Some(caller_ip),
    canary,
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
      // A resilient cached answer (possibly stale) beats the 504, but never
      // for a denied visitor: serving cache would leak the route's existence.
      if !denied
        && let Some(resp) =
          stale_cache_response(&state, &method_str, &uri_str, &headers, start_time).await
      {
        return resp;
      }
      // Per-hostname fallback URL (`fallbacks:` section): redirect an
      // unclaimed hostname to a configured origin/status page instead of 504.
      // Never for a denied visitor (stealth), the redirect would leak the
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
  // A serving client is chosen (routing done).
  let selected_at = Instant::now();
  tracing::Span::current().record("aperio.client.id", selected.id.as_str());

  // Per-token rate limit / daily quota of the serving token (dynamic tokens
  // only). Enforced once at admission; failover re-dispatches of an already
  // admitted request are not double-counted.
  if let Err(limit) = state.check_token_limits(selected.token_id.as_deref()).await {
    log_request_failure(
      &state,
      &method_str,
      &uri_str,
      429,
      start_time.elapsed(),
      Some(&limit.log_detail()),
      selected.org_id.clone(),
    )
    .await;
    return refuse(&state, limit);
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
      Some(&Limit::OrgQuota.log_detail()),
      selected.org_id.clone(),
    )
    .await;
    return refuse(&state, Limit::OrgQuota);
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
  // The guard is held until this handler returns, by then the leader's
  // response is cached (or proved uncacheable), and its Drop wakes waiters
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
              selected.service_name.clone(),
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
      // be uncacheable there is nothing to coalesce onto, dispatch normally.
      if waited {
        break;
      }
      let follow_rx = {
        // Recover from a poisoned lock instead of panicking: this runs on the
        // cacheable-request hot path, so one panic under the lock must not turn
        // every subsequent request into a panic. The in-flight map is valid
        // regardless of who poisoned it (mirrors the Drop impl above).
        let mut inflight = state
          .cache_inflight
          .lock()
          .unwrap_or_else(|e| e.into_inner());
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
  if content_length.is_some_and(|l| l > body_limit as u64) {
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

  // Map headers (preserve duplicates by collecting into a Vec).
  // Filter out internal aperio session cookies to prevent leaking dashboard
  // session tokens to tunnel clients.
  // When OTLP export is on we replace any inbound W3C trace headers with this
  // span's context; when off, `trace_headers` is empty and inbound headers
  // pass through unchanged.
  let inject_trace = !trace_headers.is_empty();
  // The request-id header the server manages: any inbound copy is dropped
  // here and exactly one value is added below, so a visitor can neither
  // smuggle a second one to the backend nor have an untrusted value
  // forwarded when `trust_inbound` is off.
  let request_id_header = state.config().request_id_header.clone();
  let manage_request_id = state.config().request_id_enabled;
  let carried_names = carried_identity_names(&state);
  let consumed_authorization = visitor.as_ref().is_some_and(|v| v.consumed_authorization);
  let mut serialized_headers: Vec<(String, String)> = Vec::new();
  for (k, v) in headers.iter() {
    if let Ok(val_str) = v.to_str() {
      // Aperio's own: the namespace it speaks in, the credential that opened
      // its gate, the name an endpoint delivers an identity under.
      if header_is_aperios(k.as_str(), &carried_names, consumed_authorization) {
        continue;
      }
      if inject_trace {
        let k_lower = k.as_str().to_ascii_lowercase();
        if k_lower == "traceparent" || k_lower == "tracestate" {
          continue;
        }
      }
      if manage_request_id && k.as_str().eq_ignore_ascii_case(&request_id_header) {
        continue;
      }
      if k.as_str() == "cookie" {
        let filtered = cookies_without_aperios(val_str);
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

  // A visitor-supplied request id is only adopted where the operator says the
  // header is trustworthy, and only in a shape that is safe to log and to
  // forward. Resolved once per visitor request, so every failover attempt
  // carries the same value.
  let adopted_request_id = (manage_request_id && state.config().request_id_trust_inbound)
    .then(|| {
      headers
        .get(&request_id_header)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| is_safe_request_id(v))
        .map(str::to_string)
    })
    .flatten();

  // Server-side `headers.request` rewrite rules (aperio-server.yaml), applied
  // before the inspector capture so replay and capture match what was sent.
  // A matching `routes:` policy entry applies after the server-wide rules, so
  // the narrower rule is the one that gets the last word.
  let serialized_headers = state
    .config()
    .header_rules
    .request
    .apply(serialized_headers);
  let serialized_headers = match state
    .config()
    .static_routes
    .policy_for(request_host.as_deref(), &uri_path_owned)
  {
    Some(rule) => rule.header_transforms.request.apply(serialized_headers),
    None => serialized_headers,
  };

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
              Some(&Limit::ClientConcurrency.log_detail()),
              selected.org_id.clone(),
            )
            .await;
            break refuse(&state, Limit::ClientConcurrency);
          }
        }
      }
      None => None,
    };

    // Increment request stats for the chosen client.
    selected.request_count.fetch_add(1, Ordering::SeqCst);

    // The internal id is always server-minted and is never the visitor's,
    // because it keys `pending_requests`: a visitor able to choose it could
    // collide with another request in flight and be handed its answer.
    let request_id = uuid::Uuid::new_v4().to_string();
    // What the backend and the visitor see. An adopted inbound id stays the
    // same across failover attempts, which is what makes it the visitor's
    // trace; without one this is the attempt's own internal id, so the header
    // matches the id in our access log and the inspector exactly.
    let correlation_id = manage_request_id.then(|| {
      adopted_request_id
        .clone()
        .unwrap_or_else(|| request_id.clone())
    });
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
    // Takes the entry back out if this handler stops existing: a visitor that
    // hangs up mid-request drops this future, and every explicit `remove`
    // below is on a path that is no longer running. Held for the rest of the
    // attempt; the response path removes the entry first in the ordinary
    // case, and then this finds nothing.
    let _pending_guard = crate::state::PendingGuard::new(
      state.clone(),
      crate::state::PendingMap::Requests,
      request_id.clone(),
    );

    // Dispatch: buffered requests go out as a single Request message;
    // streamed requests send RequestStart here and a pump task feeds the
    // body as raw binary chunk frames.
    // Which way this client takes a buffered body. Decided *here*, per
    // iteration, because a failover or a 5xx retry re-enters this loop with a
    // different `selected`: deciding once outside would send a v6 frame to
    // whichever client the first one failed over to, and one that does not
    // speak v6 cannot read it, so the request would hang until the gateway
    // timeout with no sign of why.
    let full_body_frame = !stream_request && body_frame_negotiated(selected.protocol, &body_bytes);
    let base64_body = if stream_request || full_body_frame || body_bytes.is_empty() {
      None
    } else {
      use base64::prelude::*;
      Some(BASE64_STANDARD.encode(&body_bytes))
    };
    // The dispatched headers are this attempt's: the same list plus the
    // request id, appended here rather than baked in above because the id can
    // differ per attempt when none was adopted from the visitor.
    let mut dispatch_headers = serialized_headers.clone();
    if let Some(ref id) = correlation_id {
      dispatch_headers.push((request_id_header.clone(), id.clone()));
    }
    // Who is serving this attempt, for a backend that is shared between
    // tenants. Added here rather than above because a failover attempt can
    // land on a different client, and a header naming the previous one would
    // be worse than none.
    if state.config().identity_headers {
      dispatch_headers.push(("x-aperio-client-id".to_string(), selected.id.clone()));
      if let Some(ref org) = selected.org_id {
        dispatch_headers.push(("x-aperio-org".to_string(), org.clone()));
      }
      dispatch_headers.push((
        "x-aperio-token".to_string(),
        selected
          .token_name
          .clone()
          .unwrap_or_else(|| "master".to_string()),
      ));
    }
    // Who the gate let in. Separate from the switch above because the two
    // answer different questions, which client is serving this and who is
    // asking, and a backend may well want one without the other. Nothing is
    // sent where nobody was identified: an open route has no visitor to name,
    // and a header saying "anonymous" would be noise a backend has to learn
    // to ignore.
    if state.config().visitor_identity_headers
      && let Some(ref visitor) = visitor
    {
      dispatch_headers.push(("x-aperio-visitor-how".to_string(), visitor.how.to_string()));
      if let Some(ref who) = visitor.who {
        dispatch_headers.push(("x-aperio-visitor-id".to_string(), who.clone()));
      }
    }
    // Headers a `forward` endpoint asked to be carried onward. Not behind the
    // switch above: the operator named these one at a time in the file, which
    // is a more explicit request than the announcement setting is, and they
    // are the whole point of running the endpoint.
    if let Some(ref visitor) = visitor {
      for (name, value) in &visitor.extra_headers {
        dispatch_headers.push((name.clone(), value.clone()));
      }
    }
    let dispatch_msg = if stream_request {
      TunnelMessage::RequestStart {
        id: request_id.clone(),
        // The service routing chose, so a client carrying several knows
        // which of its targets this is for.
        service: selected.service_name.clone(),
        method: method_str.clone(),
        uri: uri_str.clone(),
        headers: dispatch_headers,
      }
    } else {
      TunnelMessage::Request {
        id: request_id.clone(),
        // The service routing chose, so a client carrying several knows
        // which of its targets this is for.
        service: selected.service_name.clone(),
        method: method_str.clone(),
        uri: uri_str.clone(),
        headers: dispatch_headers,
        body: base64_body,
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
    // v6: the envelope and the body in one binary frame. The writer deflates
    // it when this connection negotiated compression, the same way the client
    // does with a full response.
    let dispatch_frame = if full_body_frame {
      crate::protocol::encode_full_request_frame(
        crate::protocol::FRAME_REQUEST_FULL,
        &request_id,
        &req_json,
        &body_bytes,
      )
      .map(|frame| Message::Binary(frame.into()))
    } else {
      None
    };
    let dispatched = selected
      .tx
      .send(dispatch_frame.unwrap_or_else(|| Message::Text(req_json.into())))
      .await
      .is_ok();
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
        // Whether the whole upload was relayed. On an over-limit truncation or a
        // mid-stream read error we must NOT finalize the body: RequestEnd tells
        // the backend "this body is complete", so sending it after a truncation
        // would have the backend silently process a partial request as whole.
        let mut complete = true;
        while let Some(chunk) = stream.next().await {
          match chunk {
            Ok(bytes) => {
              total += bytes.len();
              if total > max_body {
                warn!(
                  "Streamed request {} exceeded the max body size; aborting the upload",
                  pump_id
                );
                complete = false;
                break;
              }
              counter.fetch_add(bytes.len() as u64, Ordering::Relaxed);
              {
                let mut stats = pump_state.stats.lock().await;
                stats.total_bytes_transferred += bytes.len() as u64;
              }
              let framed = encode_binary_frame(FRAME_REQUEST_CHUNK, &pump_id, &bytes);
              if framed.is_none() {
                warn!("Refusing to frame a request chunk: request id is too long to encode");
              }
              if match framed {
                Some(frame) => pump_tx.send(Message::Binary(frame.into())).await.is_err(),
                None => true,
              } {
                complete = false;
                break;
              }
            }
            Err(e) => {
              warn!("Request body stream error for {}: {}", pump_id, e);
              complete = false;
              break;
            }
          }
        }
        if complete {
          if let Ok(json) = serde_json::to_string(&TunnelMessage::RequestEnd { id: pump_id }) {
            let _ = pump_tx.send(Message::Text(json.into())).await;
          }
        } else {
          // Abort: drop the pending request so the awaiting handler resolves
          // immediately (the visitor gets a prompt gateway error) rather than
          // the backend receiving a truncated body framed as complete.
          pump_state.pending_requests.lock().await.remove(&pump_id);
        }
      });
    }

    // Await the response with the per-attempt response timeout, most specific
    // first: a matching `routes:` policy entry's `timeout`, then the serving
    // client's per-service `response_timeout` override when it declared one,
    // then the global gateway response timeout. The route wins over the
    // service because it is the operator's own server-side configuration,
    // while the service value is announced by the client.
    // A declared 0 means "use the global value" (not a zero-second timeout that
    // would fail every request instantly), matching the global timeout's own
    // `.max(1)` clamp.
    let route_timeout = state
      .config()
      .static_routes
      .policy_for(request_host.as_deref(), &uri_path_owned)
      .and_then(|rule| rule.timeout)
      .filter(|s| *s > 0);
    let response_timeout = route_timeout
      .or(selected.response_timeout.filter(|s| *s > 0))
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
              record_outlier_failure(&state, &selected.id, selected.service_name.as_deref()).await;
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
        // applied before every consumer, the visitor response, the response
        // cache and the inspector capture, so all views agree.
        tunnel_res.headers = state
          .config()
          .header_rules
          .response
          .apply(std::mem::take(&mut tunnel_res.headers));
        // Then the matching `routes:` policy entry, which wins over the
        // server-wide rules for the route it names. A `cache-control` added
        // here is what the visitor, the response cache and the inspector all
        // see, since this runs before any of them.
        if let Some(rule) = state
          .config()
          .static_routes
          .policy_for(request_host.as_deref(), &uri_path_owned)
        {
          tunnel_res.headers = rule
            .header_transforms
            .response
            .apply(std::mem::take(&mut tunnel_res.headers));
        }
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
          record_outlier_failure(&state, &selected.id, selected.service_name.as_deref()).await;
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
            canary,
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

        // A v5 client sends the body as bytes in the same frame as the
        // envelope, so there is nothing to decode. Anything older sends it
        // base64 inside the JSON.
        let res_bytes: axum::body::Bytes = if let Some(raw) = tunnel_res.body_raw.take() {
          raw
        } else if let Some(ref encoded_body) = tunnel_res.body {
          use base64::prelude::*;
          BASE64_STANDARD
            .decode(encoded_body)
            .unwrap_or_default()
            .into()
        } else {
          axum::body::Bytes::new()
        };

        let body_len = res_bytes.len() as u64;

        let mut response_builder = Response::builder().status(status_code);

        // Echo the correlation id, so a visitor reporting a problem can quote
        // the id that is in our log and in the backend's. Set before the
        // backend's own headers are copied, so a backend that echoes it too
        // does not end up with the header twice.
        if let Some(ref id) = correlation_id
          && let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(request_id_header.as_bytes()),
            HeaderValue::from_str(id),
          )
        {
          response_builder = response_builder.header(name, value);
        }

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
          // The request id is ours to set: a backend that echoes the header
          // back would otherwise leave the visitor with two copies of it, and
          // a builder appends rather than replaces.
          if correlation_id.is_some() && k_lower == request_id_header {
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
              // The cache owns its copy, but a `Bytes` clone is a refcount
              // bump, not a byte copy, so the lock is held only that long.
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

        // Capture the transaction for the dashboard inspector, unless the
        // server has it off (`inspector: false`) or this service asked not to
        // be recorded. What it costs is a mutex, two header clones and an
        // entry per request; what it buys is the screen an operator opens
        // first when something is wrong, so it is on unless someone says
        // otherwise.
        if state.config().inspector && selected.capture {
          use base64::prelude::*;
          let resp_streamed = tunnel_res.stream_rx.is_some();
          // A pre-v5 body arrived base64-encoded and the capture wants it
          // base64-encoded, so the string that came in is reused rather than
          // computed twice. A v5 body arrived as bytes and has no string to
          // reuse: it is encoded here, once, and only when the capture is on
          // at all.
          let (resp_body_cap, resp_truncated) = if resp_streamed || res_bytes.is_empty() {
            (None, false)
          } else if res_bytes.len() > CAPTURE_BODY_LIMIT {
            (
              Some(BASE64_STANDARD.encode(&res_bytes[..CAPTURE_BODY_LIMIT])),
              true,
            )
          } else {
            match tunnel_res.body.as_ref() {
              Some(encoded) => (Some(encoded.clone()), false),
              None => (Some(BASE64_STANDARD.encode(&res_bytes)), false),
            }
          };
          let mut captured = state.captured_requests.lock().await;
          if captured.len() >= CAPTURE_MAX_ENTRIES {
            crate::state::evict_for_fairness(&mut captured);
          }
          let us = |at: Instant| at.duration_since(start_time).as_micros() as u64;
          let mut timeline = crate::state::RequestTimeline::assemble(
            us(dispatched_at),
            us(response_received_at),
            start_time.elapsed().as_micros() as u64,
            tunnel_res.timings,
          );
          // Real server-side sub-boundaries of the pre-dispatch phase, for the
          // trace waterfall (queue & routing → await client / admission /
          // routing / dispatch prep).
          timeline.client_ready_us = Some(us(client_ready_at));
          timeline.admitted_us = Some(us(admitted_at));
          timeline.selected_us = Some(us(selected_at));
          state.stage_stats.lock().await.record(
            request_host.as_deref(),
            selected.org_id.as_deref(),
            &timeline,
          );
          // Mirror the request waterfall into the trace as child spans of
          // proxy.request (no-op unless OTLP export is on).
          crate::telemetry::emit_phase_spans(start_time, &timeline);
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
            client_id: selected.id.clone(),
            client_name: selected
              .service_custom_name
              .clone()
              .or_else(|| selected.service_name.clone()),
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
          // Per-visitor ceiling on open streams (planned_features #20). The
          // slot is taken here and moved into the stream's own state, so it
          // lives exactly as long as the response body does and is released
          // whether the stream ends or the visitor walks away.
          //
          // Taken after the response has been produced rather than before the
          // request is dispatched: only now is it known that this response
          // *is* a stream, and refusing a request that would have answered in
          // one buffered frame would be a limit firing on traffic it was never
          // about.
          let stream_slot = if state.config().max_streams_per_ip == 0 {
            None
          } else {
            match state.try_acquire_stream_slot(caller_ip) {
              Some(slot) => Some(slot),
              None => {
                log_request_failure(
                  &state,
                  &method_str,
                  &uri_str,
                  429,
                  start_time.elapsed(),
                  Some(&Limit::StreamsPerIp.log_detail()),
                  selected.org_id.clone(),
                )
                .await;
                return refuse(&state, Limit::StreamsPerIp);
              }
            }
          };
          let stream =
            futures_util::stream::unfold((chunk_rx, stream_slot), |(mut rx, slot)| async move {
              rx.recv()
                .await
                .map(|item| (frame_from_body_item(item), (rx, slot)))
            });
          Body::new(http_body_util::StreamBody::new(stream))
        } else if let Some(trailers) = tunnel_res.trailers.take() {
          let frames: Vec<Result<http_body::Frame<axum::body::Bytes>, axum::BoxError>> = vec![
            Ok(http_body::Frame::data(res_bytes.clone())),
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
        record_outlier_failure(&state, &selected.id, selected.service_name.as_deref()).await;
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
                canary,
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
