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

use futures_util::StreamExt;
use std::sync::Arc;

use crate::state::AppState;
use crate::state::stream::TunnelResponse;

/// The visitor's request body, as it reaches this path.
///
/// Two shapes rather than one because the choice is the same one `forward.rs`
/// makes for the relayed path: a small upload is already in memory and a large
/// one should not be. Carrying the limit with the stream keeps the cap where
/// the bytes are counted.
pub(super) enum RequestBody {
  Buffered(Vec<u8>),
  Stream(axum::body::Body, u64),
}

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
  body: RequestBody,
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
  match body {
    RequestBody::Buffered(bytes) if !bytes.is_empty() => {
      req = req.body(bytes);
    }
    RequestBody::Buffered(_) => {}
    RequestBody::Stream(raw, limit) => {
      // The visitor's upload goes to the target as it arrives rather than
      // being held here first, which is the half of #140 that matches what
      // the relayed path already does. The cap still applies: a stream that
      // runs past it ends in an error rather than being truncated, because a
      // truncated upload is a request the backend would answer as if it were
      // whole.
      let mut seen: u64 = 0;
      let counted = raw.into_data_stream().map(move |chunk| match chunk {
        Ok(bytes) => {
          seen += bytes.len() as u64;
          if seen > limit {
            Err(std::io::Error::other(
              "request body exceeded the configured limit",
            ))
          } else {
            Ok(bytes)
          }
        }
        Err(e) => Err(std::io::Error::other(e.to_string())),
      });
      req = req.body(reqwest::Body::wrap_stream(counted));
    }
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

  // Streamed rather than buffered, which is what the relayed path does and
  // what this one owes it: a service is served from here because it carries
  // real traffic, so holding whole responses in memory would have made the
  // fast path the expensive one. The head goes back as soon as it arrives and
  // the body follows through the channel.
  let (chunk_tx, chunk_rx) =
    tokio::sync::mpsc::channel::<Result<crate::state::BodyFrame, std::io::Error>>(32);
  let target_for_log = target.to_string();
  tokio::spawn(async move {
    let mut stream = res.bytes_stream();
    while let Some(next) = stream.next().await {
      let frame = match next {
        Ok(bytes) => Ok(crate::state::BodyFrame::Data(bytes)),
        Err(e) => {
          tracing::warn!(
            "Server-side response body from {} failed mid-stream: {}",
            target_for_log,
            e
          );
          Err(std::io::Error::other(e.to_string()))
        }
      };
      let failed = frame.is_err();
      // A closed receiver is the visitor having hung up. Stop reading the
      // target rather than filling a channel nobody drains, which is the one
      // way a pump like this turns a dropped request into work that continues.
      if chunk_tx.send(frame).await.is_err() || failed {
        break;
      }
    }
  });
  let _ = &state;

  Some(TunnelResponse {
    status,
    headers: out_headers,
    body: None,
    body_raw: None,
    trailers: None,
    stream_rx: Some(chunk_rx),
    // No client, so no client-side stages. Not invented.
    timings: None,
  })
}

/// Headers a visitor may not hand to the target through this path.
///
/// The shared core is `aperio_config::hop_by_hop::HOP_BY_HOP_CORE`, and a test
/// holds this to it. The reason is written there in full: a visitor-supplied
/// `transfer-encoding: chunked` collides with the HTTP client's own body
/// framing and opens a desync and request-smuggling surface.
///
/// `host` is this path's own addition, for a sharper reason. What was checked
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
