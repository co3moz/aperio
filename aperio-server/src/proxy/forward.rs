//! Forwarding one request over the tunnel and streaming the answer back.
//!
//! Two phases. Here are the five gates a request passes before anything is
//! sent: the per-IP rate limit, the visitor-auth gate, the wait for a client
//! (including the cold start that may wake one), the admission slot, and
//! picking who serves it. Then it reads the body, and hands everything to
//! [`attempt`], which dispatches and maps the answer back.
//!
//! This file used to say it was one function "deliberately whole", because
//! cutting it would mean a piece returning early without releasing the
//! dispatch slot or the pending-response registration. That was the wrong
//! reason: both are RAII guards, and both are released on every exit by the
//! same `Drop` this file already relied on. The admission permit stays here as
//! `_permit` for exactly that reason, and holding it across the call below is
//! all the "one scope" ever actually bought.

use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use super::*;
use crate::state::AppState;

pub(crate) mod attempt;
mod server_side;

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

  let selected = match pick_proxy_client(
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
  // A service served from this server buffers its request body instead of
  // streaming it: streaming exists to avoid holding a large upload while it
  // crosses the tunnel, and there is no tunnel here. Deciding it at this one
  // place rather than in the dispatch keeps the two from disagreeing, which
  // would have shown up as a body pumped into a socket nobody is reading.
  let stream_request = selected.server_side_target.is_none()
    && selected.protocol.unwrap_or(1) >= 2
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

  // 6. Dispatch and await the response, re-dispatching to another client while
  // failover allows it. Everything the five gates above produced travels as
  // one value; `_permit` deliberately does not, it stays here so the admission
  // slot is held for as long as this call runs.
  attempt::Attempt {
    state,
    method_str,
    uri_str,
    uri_path_owned,
    caller_ip,
    request_host,
    selected,
    canary,
    visitor,
    body_bytes,
    body_limit,
    stream_request,
    streamed_body,
    streamed_bytes,
    serialized_headers,
    cache_eligible,
    cache_key,
    marks: attempt::Marks {
      start: start_time,
      client_ready_at,
      admitted_at,
      selected_at,
    },
    capture: attempt::Capture {
      req_headers: capture_req_headers,
      req_body: capture_req_body,
      req_truncated: capture_req_truncated,
    },
    request_id: attempt::RequestIdPolicy {
      header: request_id_header,
      manage: manage_request_id,
      adopted: adopted_request_id,
    },
  }
  .run()
  .await
}
