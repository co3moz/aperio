//! Fetching one request from a target this server reaches itself.
//!
//! The alternative to dispatching over the tunnel, for a service that declared
//! `server_side: true` and was permitted it by both its token and the
//! operator's `server_side_targets:`. Two hops go away: the one to the client
//! and the one from the client to the target.
//!
//! **This produces a [`TunnelResponse`] and nothing else.** That is the whole
//! design: the answer is handed back through the same oneshot channel a
//! tunnel response arrives on, so every stage after it, the response timeout,
//! the header rewrite rules, the stats and per-organization attribution, the
//! response cache, the inspector capture and the access log, runs the code it
//! already ran and cannot tell the two apart. A second delivery path would
//! have been a second set of behaviours to keep in step, and the first thing
//! to drift would have been the quiet ones: a `server_side` service silently
//! not appearing in captures.
//!
//! What is deliberately absent is the client's own timing report. A
//! [`TunnelResponse::timings`] describes stages measured on a client's clock,
//! and there is no client here, so it is `None` rather than invented.

use std::sync::Arc;

use crate::state::AppState;
use crate::state::stream::TunnelResponse;

/// Sends `method`/`path` to `target` and shapes the answer like a tunnel one.
///
/// `None` means the request never produced a response, which is the same thing
/// a lost client means to the caller: it falls into the failure arm and is
/// logged and counted there.
pub(super) async fn fetch(
  state: Arc<AppState>,
  target: &str,
  method: &str,
  path_and_query: &str,
  headers: Vec<(String, String)>,
  body: Vec<u8>,
) -> Option<TunnelResponse> {
  let url = join_target(target, path_and_query)?;
  let method = reqwest::Method::from_bytes(method.as_bytes()).ok()?;

  let client = crate::outbound::client_builder()
    // Redirects are the visitor's to follow, not this server's. Following one
    // here would hide it from the visitor and, worse, would let a target the
    // allowlist admitted bounce the connection to one it never saw.
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .ok()?;

  let mut req = client.request(method, url);
  for (name, value) in &headers {
    if is_hop_by_hop(name) {
      continue;
    }
    if let Ok(header) = reqwest::header::HeaderName::from_bytes(name.as_bytes())
      && let Ok(v) = reqwest::header::HeaderValue::from_str(value)
    {
      req = req.header(header, v);
    }
  }
  if !body.is_empty() {
    req = req.body(body);
  }

  let res = match req.send().await {
    Ok(res) => res,
    Err(e) => {
      tracing::warn!("Server-side request to {} failed: {}", target, e);
      return None;
    }
  };

  let status = res.status().as_u16();
  let mut out_headers = Vec::new();
  for (name, value) in res.headers() {
    if let Ok(v) = value.to_str() {
      out_headers.push((name.as_str().to_string(), v.to_string()));
    }
  }
  let body_raw = match res.bytes().await {
    Ok(b) => b,
    Err(e) => {
      tracing::warn!("Server-side response body from {} failed: {}", target, e);
      return None;
    }
  };
  let _ = &state;

  Some(TunnelResponse {
    status,
    headers: out_headers,
    body: None,
    body_raw: Some(body_raw),
    trailers: None,
    // Buffered rather than streamed, which is the one behavioural difference
    // from the tunnel path and is honest about what it costs: a large
    // response is held in memory here instead of flowing through. The body
    // cap that bounds it is the same `max_request_body`-shaped question the
    // relayed path answers, and a streaming version is a separate change.
    stream_rx: None,
    // No client, so no client-side stages. Not invented.
    timings: None,
  })
}

/// Headers a visitor may not hand to the target through this path.
///
/// The relayed path strips exactly these in `aperio-client`'s
/// `proxy/http.rs`, and the reason is written there in full: forwarding a
/// visitor-supplied `transfer-encoding: chunked` collides with reqwest's own
/// body framing and opens an HTTP desync and request-smuggling surface.
/// Dropping it leaves `content-length` as the single framing signal.
///
/// The same list has to exist here because the two paths reach a backend the
/// same way, through reqwest, and only one of them used to be reachable by a
/// visitor's headers. Serving from the server was not meant to be a way past
/// a strip the relayed path performs.
///
/// `host` is on the list for a different and sharper reason. What was checked
/// against `server_side_targets:` is the *target*, and reqwest lets an
/// explicit `Host` header override the authority the target's URL carries, so
/// forwarding the visitor's would let them pick a virtual host on an address
/// the operator allowed for something else entirely. The connection goes
/// where the allowlist said; the name it asks for goes with it.
fn is_hop_by_hop(name: &str) -> bool {
  let n = name.to_ascii_lowercase();
  aperio_config::hop_by_hop::HOP_BY_HOP_CORE.contains(&n.as_str())
    || n == "trailer"
    || n == "host"
    || n.starts_with(aperio_config::hop_by_hop::WEBSOCKET_PREFIX)
}

/// Joins the declared target with the visitor's path and query.
///
/// The target is what the operator's `server_side_targets:` was checked
/// against, so the path may not be allowed to change which host is reached:
/// this only ever appends.
fn join_target(target: &str, path_and_query: &str) -> Option<reqwest::Url> {
  let base = if target.contains("://") {
    target.to_string()
  } else {
    format!("http://{target}")
  };
  let mut url = reqwest::Url::parse(&base).ok()?;
  let (path, query) = match path_and_query.split_once('?') {
    Some((p, q)) => (p, Some(q)),
    None => (path_and_query, None),
  };
  let base_path = url.path().trim_end_matches('/').to_string();
  url.set_path(&format!("{base_path}{path}"));
  url.set_query(query);
  Some(url)
}

#[cfg(test)]
#[path = "server_side_tests.rs"]
mod tests;
