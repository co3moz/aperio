//! One visitor request's trip over the tunnel, from dispatch to the response
//! the visitor gets, including every re-dispatch failover makes along the way.
//!
//! Split out of [`super::forward`] on a measurement rather than a feeling.
//! Twenty-seven values cross into this loop, and unlike the client's read loop
//! they are not one thing said many ways: they are eight. Five of the eight
//! are small enough and coherent enough to be named ([`Marks`], [`Capture`],
//! [`RequestIdPolicy`]), and the rest stay separate fields because grouping
//! them further would be inventing a relationship the code does not have.
//!
//! What does *not* cross is the admission permit. It stays in the caller as
//! `_permit`, which is what the old "one whole function so nothing leaks"
//! argument was really about: the guard outlives this call because the caller's
//! frame does, and Rust releases it on the way out of every exit here, the same
//! way it did when the exits were all in one scope.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use super::super::*;
use crate::routing::select::SelectedClient;
use crate::state::AppState;

/// Where the pre-dispatch phase's boundaries fell, for the trace waterfall.
/// Four `Instant`s taken three hundred lines apart and read in one place, so
/// they travel as one thing.
pub(super) struct Marks {
  pub(super) start: Instant,
  pub(super) client_ready_at: Instant,
  pub(super) admitted_at: Instant,
  pub(super) selected_at: Instant,
}

/// What the inspector keeps of the request. Taken before dispatch because the
/// tunnel message moves the originals, and read six hundred lines later, which
/// is the whole reason it needs a name.
pub(super) struct Capture {
  pub(super) req_headers: Vec<(String, String)>,
  pub(super) req_body: Option<String>,
  pub(super) req_truncated: bool,
}

/// How this request's id was decided. Resolved once per visitor request, so
/// every failover attempt carries the same value.
pub(super) struct RequestIdPolicy {
  pub(super) header: String,
  pub(super) manage: bool,
  pub(super) adopted: Option<String>,
}

/// Everything the dispatch loop needs, which is everything the five gates
/// before it produced.
pub(super) struct Attempt<'a> {
  pub(super) state: Arc<AppState>,
  /// What the visitor asked for, in the spellings this loop uses: the method
  /// and URI as strings for the log lines, the path alone for routing. The
  /// `Uri` and `HeaderMap` themselves stay behind, they are the gates' input
  /// and nothing past dispatch reads them again.
  pub(super) method_str: String,
  pub(super) uri_str: String,
  pub(super) uri_path_owned: String,
  pub(super) caller_ip: std::net::IpAddr,
  pub(super) request_host: Option<String>,
  /// Who is serving this attempt, and which side of a canary split they are
  /// on. `selected` is reassigned by failover, which is why it is the loop's
  /// only mutable input.
  pub(super) selected: SelectedClient,
  pub(super) canary: Option<(&'a str, crate::static_routes::Side)>,
  pub(super) visitor: Option<VisitorIdentity>,
  /// The request body, one way or the other: `body_bytes` when it was
  /// buffered, `streamed_body` when the client takes it as a stream.
  pub(super) body_bytes: axum::body::Bytes,
  pub(super) body_limit: usize,
  pub(super) stream_request: bool,
  pub(super) streamed_body: Option<Body>,
  pub(super) streamed_bytes: Arc<AtomicU64>,
  pub(super) serialized_headers: Vec<(String, String)>,
  pub(super) cache_eligible: bool,
  pub(super) cache_key: String,
  pub(super) marks: Marks,
  pub(super) capture: Capture,
  pub(super) request_id: RequestIdPolicy,
}

impl Attempt<'_> {
  /// Dispatches, awaits, and maps the answer back, re-dispatching to another
  /// client while failover allows it.
  pub(super) async fn run(self) -> Response {
    let Attempt {
      state,
      method_str,
      uri_str,
      uri_path_owned,
      caller_ip,
      request_host,
      mut selected,
      canary,
      visitor,
      body_bytes,
      body_limit,
      stream_request,
      mut streamed_body,
      streamed_bytes,
      serialized_headers,
      cache_eligible,
      cache_key,
      marks:
        Marks {
          start: start_time,
          client_ready_at,
          admitted_at,
          selected_at,
        },
      capture:
        Capture {
          req_headers: capture_req_headers,
          req_body: capture_req_body,
          req_truncated: capture_req_truncated,
        },
      request_id:
        RequestIdPolicy {
          header: request_id_header,
          manage: manage_request_id,
          adopted: adopted_request_id,
        },
    } = self;
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
          match tokio::time::timeout(state.config().gateway_timeout, limiter.acquire_owned()).await
          {
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

      // Served from this server rather than relayed. Nothing is registered in
      // `pending_requests` and nothing goes over the tunnel: the answer is
      // fetched here and handed back through the same channel, so the response
      // timeout, the header rules, the stats, the cache, the capture and the
      // access log below all run unchanged and cannot tell the two apart.
      let server_side_target = selected.server_side_target.clone();
      if let Some(target) = server_side_target.clone() {
        let st = state.clone();
        let method = method_str.clone();
        let pq = uri_str.clone();
        let hdrs = serialized_headers.clone();
        let body = body_bytes.to_vec();
        tokio::spawn(async move {
          let res = super::server_side::fetch(st, &target, &method, &pq, hdrs, body).await;
          if let Some(res) = res {
            let _ = tx_response.send(res);
          }
        });
      } else {
        // Insert oneshot receiver to await response mapping
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
      let full_body_frame =
        !stream_request && body_frame_negotiated(selected.protocol, &body_bytes);
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
      let dispatched = if server_side_target.is_some() {
        // Already under way in the task above; the tunnel is not involved.
        true
      } else {
        selected
          .tx
          .send(dispatch_frame.unwrap_or_else(|| Message::Text(req_json.into())))
          .await
          .is_ok()
      };
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
            if k_lower == "connection" || k_lower == "keep-alive" || k_lower == "transfer-encoding"
            {
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
}
