//! `forward` asked over the tunnel (`planned_features.md` #157): the endpoint
//! that decides lives on the client's network, so the server, holding a
//! request it cannot yet admit, asks the connection that would serve it, the
//! client calls its own internal endpoint, and the verdict comes back the way
//! a response does.
//!
//! What crosses is the same question [`crate::forward_auth::ask`] would put
//! to an endpoint of the server's own: the request line as `X-Forwarded-*`
//! and the allow-listed request headers, composed here so the client sends
//! exactly what the server decided and nothing it chose itself. What comes
//! back is a status and the allow-listed response headers, never a body:
//! the endpoint is being asked a question about a request, not asked to
//! serve it.
//!
//! Two things this is careful about, both from the entry. **Which
//! connection**: the one the request would be dispatched to, through the
//! same pick the proxy makes, so one client's endpoint failing takes down
//! nothing another client is serving. **What it spends**: the visitor's own
//! IP bucket, at the price of a credential attempt, and a small ceiling of
//! open asks per connection, never the service's request permits, so a flood
//! of unauthenticated visitors cannot be a denial of service performed by the
//! site's own gate.
//!
//! **Fails closed, in every shape "unreachable" takes over a tunnel**:
//! disconnected, draining, saturated, too old to answer, or simply slow. A
//! gate that opens when its check cannot be reached is not a gate. During
//! the handover a reload makes, the site is not only unserved, it also
//! cannot be logged into, and that is the documented trade.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use tokio::sync::oneshot;

use crate::forward_auth::{ForwardConfig, Verdict};
use crate::protocol::TunnelMessage;
use crate::routing::PickOutcome;
use crate::state::AppState;

/// Open asks one connection may hold at once. Past it, a visitor is refused
/// rather than queued: the ceiling is what keeps pre-auth traffic from
/// reaching the client's network faster than its endpoint can answer.
pub(crate) const MAX_ASKS_PER_CONNECTION: usize = 32;

/// An ask the server is waiting on: who was asked, and where the answer goes.
pub(crate) struct PendingAsk {
  pub(crate) client_id: String,
  pub(crate) tx: oneshot::Sender<AskAnswer>,
}

/// What a client answers an ask with.
#[derive(Debug, Clone)]
pub(crate) struct AskAnswer {
  pub(crate) status: u16,
  pub(crate) headers: Vec<(String, String)>,
  /// Why the endpoint could not be asked, when it could not; the status is
  /// meaningless then and the request is refused.
  pub(crate) error: Option<String>,
}

/// The headers relayed from a refusal whatever the allowlist says: what a
/// browser needs to act on the endpoint's own answer.
const RELAYED_ON_REFUSAL: &[&str] = &["location", "content-type", "www-authenticate", "set-cookie"];

/// Asks the client that would serve `(host, path)` about this request and
/// turns its answer into a verdict.
#[allow(clippy::too_many_arguments)] // the request, described; a struct would be the same list
pub(crate) async fn ask_over_tunnel(
  state: &AppState,
  cfg: &ForwardConfig,
  method: &axum::http::Method,
  headers: &HeaderMap,
  uri: &axum::http::Uri,
  host: Option<&str>,
  visitor_ip: IpAddr,
  path: &str,
) -> Verdict {
  let names: Vec<&str> = if cfg.request_headers.is_empty() {
    crate::forward_auth::DEFAULT_REQUEST_HEADERS.to_vec()
  } else {
    cfg.request_headers.iter().map(String::as_str).collect()
  };
  let visitor = visitor_ip.to_string();
  let cache_key = (!cfg.cache.is_zero())
    .then(|| crate::forward_auth::key(cfg, &names, headers, host, method, uri, Some(&visitor)));
  if let Some(ref k) = cache_key
    && let Some(carried) = crate::forward_auth::cached(state, cfg, k).await
  {
    return Verdict::Allow(carried);
  }

  // Priced as a credential attempt, from the visitor's own bucket: this is
  // the first method that lets a visitor nobody has admitted cause work on
  // the client's network, and the bucket is what bounds how much.
  if !state
    .check_rate_limit_cost(visitor_ip, crate::state::RateCost::Guessable)
    .await
  {
    return Verdict::Deny(
      Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .body(Body::from("429 Too Many Requests"))
        .unwrap(),
    );
  }

  // The connection the request itself would go to, by the proxy's own pick.
  let picked =
    crate::routing::pick_proxy_client(state, path, host, None, None, Some(visitor_ip), None).await;
  let client = match picked {
    PickOutcome::Selected(client) => client,
    _ => {
      tracing::warn!(
        "No connection serves {} on {} to ask about a visitor; refusing the request",
        path,
        host.unwrap_or("-")
      );
      return Verdict::Deny(crate::forward_auth::refusal());
    }
  };

  let id = uuid::Uuid::new_v4().to_string();
  let (tx, rx) = oneshot::channel();
  {
    let mut pending = state.pending_auth_asks.lock().await;
    let open = pending
      .values()
      .filter(|p| p.client_id == client.id)
      .count();
    if open >= MAX_ASKS_PER_CONNECTION {
      tracing::warn!(
        "Client {} already has {} open auth checks; refusing a visitor rather than queuing another",
        client.id,
        open
      );
      return Verdict::Deny(crate::forward_auth::refusal());
    }
    pending.insert(
      id.clone(),
      PendingAsk {
        client_id: client.id.clone(),
        tx,
      },
    );
  }

  // The question, composed here so the client sends exactly what the server
  // decided: the request line as `X-Forwarded-*`, then the allow-listed
  // headers, which is what an endpoint of the server's own would receive.
  let mut request_headers: Vec<(String, String)> = vec![
    (
      "x-forwarded-method".to_string(),
      method.as_str().to_string(),
    ),
    ("x-forwarded-proto".to_string(), "https".to_string()),
    (
      "x-forwarded-uri".to_string(),
      uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_string(),
    ),
    ("x-forwarded-for".to_string(), visitor.clone()),
  ];
  if let Some(host) = host {
    request_headers.push(("x-forwarded-host".to_string(), host.to_string()));
  }
  for name in &names {
    if let Some(value) = headers.get(*name).and_then(|v| v.to_str().ok()) {
      request_headers.push((name.to_string(), value.to_string()));
    }
  }
  let frame = TunnelMessage::AuthAsk {
    id: id.clone(),
    url: cfg.url.clone(),
    method: method.as_str().to_string(),
    uri: uri.to_string(),
    host: host.map(str::to_string),
    visitor_ip: visitor,
    request_headers,
    response_headers: cfg.response_headers.clone(),
    timeout_ms: cfg.timeout.as_millis() as u64,
  };
  let sent = match serde_json::to_string(&frame) {
    Ok(json) => client
      .tx
      .send(axum::extract::ws::Message::Text(json.into()))
      .await
      .is_ok(),
    Err(_) => false,
  };
  if !sent {
    state.pending_auth_asks.lock().await.remove(&id);
    tracing::warn!(
      "Client {} could not be asked about a visitor (its connection is gone); refusing the request",
      client.id
    );
    return Verdict::Deny(crate::forward_auth::refusal());
  }

  // A little past the endpoint's own budget, which the client applies; this
  // is for the frame never coming back at all.
  let wait = cfg.timeout + Duration::from_millis(500);
  let answer = match tokio::time::timeout(wait, rx).await {
    Ok(Ok(answer)) => answer,
    Ok(Err(_)) => {
      tracing::warn!(
        "Client {} disconnected before answering an auth check; refusing the request",
        client.id
      );
      return Verdict::Deny(crate::forward_auth::refusal());
    }
    Err(_) => {
      state.pending_auth_asks.lock().await.remove(&id);
      tracing::warn!(
        "Client {} did not answer an auth check within {}s; refusing the request",
        client.id,
        wait.as_secs()
      );
      return Verdict::Deny(crate::forward_auth::refusal());
    }
  };
  if let Some(why) = answer.error {
    tracing::warn!(
      "Client {} could not ask {} about a visitor ({why}); refusing the request",
      client.id,
      cfg.url
    );
    return Verdict::Deny(crate::forward_auth::refusal());
  }
  let status = StatusCode::from_u16(answer.status).unwrap_or(StatusCode::FORBIDDEN);
  if status.is_success() {
    // Only the names the operator listed, whatever the client sent back:
    // the allowlist is what keeps identity delivery from being header
    // injection, and it is enforced on the side that wrote it.
    let carried: Vec<(String, String)> = answer
      .headers
      .iter()
      .filter(|(name, _)| {
        cfg
          .response_headers
          .iter()
          .any(|allowed| allowed.eq_ignore_ascii_case(name))
      })
      .cloned()
      .collect();
    if let Some(k) = cache_key {
      crate::forward_auth::remember(state, cfg, k, carried.clone()).await;
    }
    return Verdict::Allow(carried);
  }
  // The endpoint's own refusal, relayed with the headers a browser needs to
  // act on it and nothing else.
  let mut builder = Response::builder().status(status);
  for (name, value) in &answer.headers {
    if RELAYED_ON_REFUSAL
      .iter()
      .any(|allowed| allowed.eq_ignore_ascii_case(name))
    {
      builder = builder.header(name, value);
    }
  }
  Verdict::Deny(
    builder
      .body(Body::empty())
      .unwrap_or_else(|_| crate::forward_auth::refusal()),
  )
}

/// Delivers a client's answer to the ask waiting for it. An answer nobody is
/// waiting for (a late one, or one for an ask that timed out) is dropped.
pub(crate) async fn resolve(state: &AppState, id: &str, answer: AskAnswer) {
  let pending = state.pending_auth_asks.lock().await.remove(id);
  if let Some(ask) = pending {
    let _ = ask.tx.send(answer);
  }
}

/// Drops every ask waiting on `client_id`: its connection is gone, and each
/// waiting visitor is refused at once rather than after the timeout.
pub(crate) async fn forget_client(state: &AppState, client_id: &str) -> usize {
  let mut pending = state.pending_auth_asks.lock().await;
  let before = pending.len();
  pending.retain(|_, ask| ask.client_id != client_id);
  before - pending.len()
}

/// The pending map's type, for the state.
pub(crate) type PendingAsks = HashMap<String, PendingAsk>;

#[cfg(test)]
#[path = "forward_auth_tunnel_tests.rs"]
mod tests;
