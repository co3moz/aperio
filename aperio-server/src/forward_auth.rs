//! The `forward` visitor-auth method: the gate asks an endpoint the operator
//! runs (`planned_features.md` #104).
//!
//! What nginx spells `auth_request` and Traefik spells ForwardAuth. It is the
//! escape hatch that lets the method set stay closed (#103 withdrew the open
//! version): anything deliberately left out of the set, LDAP, SAML, a rule
//! nobody anticipated, is thirty lines behind this URL, in a process that is
//! not ours, with a contract that is two HTTP messages rather than an ABI.
//!
//! The five things this method *is* are the five questions the backlog entry
//! said had to be settled first, and each is answered here:
//!
//! 1. **Which headers cross.** To the endpoint: the original method, protocol,
//!    host, path and the visitor's address, as the `X-Forwarded-*` names that
//!    pattern already uses, plus the request headers the operator names,
//!    defaulting to `cookie` and `authorization`. Back from it: only the
//!    response headers the operator names, empty by default, because that
//!    list is how an identity is delivered and an open one is how it becomes
//!    a header injection.
//! 2. **A timeout refuses.** An auth gate that opens when its check is
//!    unreachable is not a gate, so the endpoint's availability becomes the
//!    route's, and that is the trade being made rather than an accident.
//! 3. **The verdict is cacheable**, keyed on the credential that produced it,
//!    because a round trip per request is unusable on anything busy.
//! 4. **The destination is an outbound callback** and goes through the same
//!    policy as webhooks, scaling hooks and JWKS fetches.
//! 5. **A refusal is the endpoint's own answer**, forwarded verbatim, so it
//!    can redirect to a login of its own instead of being flattened into a
//!    401 that means nothing to whoever wrote it.

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use sha2::{Digest, Sha256};

use crate::state::AppState;

/// Request headers copied to the subrequest when the file names none.
///
/// The two that carry an identity. Not "everything the visitor sent", which
/// is what some implementations do: the endpoint is being asked one question,
/// and handing it the whole request makes every header it happens to read
/// part of a contract nobody wrote down.
const DEFAULT_REQUEST_HEADERS: [&str; 2] = ["cookie", "authorization"];

/// Largest endpoint answer read back, so a broken one cannot be answered by
/// reading it into memory.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Most verdicts remembered at once.
const MAX_CACHED: usize = 4096;

/// A `forward` method, compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForwardConfig {
  pub(crate) url: String,
  pub(crate) request_headers: Vec<String>,
  pub(crate) response_headers: Vec<String>,
  pub(crate) timeout: Duration,
  pub(crate) cache: Duration,
}

/// A remembered verdict.
pub(crate) struct CachedVerdict {
  headers: Vec<(String, String)>,
  decided: Instant,
}

/// What the endpoint said.
pub(crate) enum Verdict {
  /// Admitted, with the headers the operator asked to carry onward.
  Allow(Vec<(String, String)>),
  /// Refused, with the endpoint's own answer for the visitor.
  Deny(Response),
}

/// Asks the endpoint about this request.
pub(crate) async fn ask(
  state: &AppState,
  cfg: &ForwardConfig,
  method: &axum::http::Method,
  headers: &HeaderMap,
  uri: &axum::http::Uri,
  host: Option<&str>,
  visitor_ip: Option<&str>,
) -> Verdict {
  let names: Vec<&str> = if cfg.request_headers.is_empty() {
    DEFAULT_REQUEST_HEADERS.to_vec()
  } else {
    cfg.request_headers.iter().map(String::as_str).collect()
  };

  if !cfg.cache.is_zero()
    && let Some(headers) = cached(state, cfg, &names, headers, host).await
  {
    return Verdict::Allow(headers);
  }

  if let Err(why) = state.config().outbound_policy.check(&cfg.url).await {
    tracing::warn!("Refusing to ask {} about a request: {why}", cfg.url);
    return Verdict::Deny(refusal());
  }
  let http = match reqwest::Client::builder()
    .timeout(cfg.timeout)
    // Redirects are **not** followed, and this is the whole method rather
    // than a detail: a `302` is how an endpoint sends a browser to its own
    // login, which is the commonest refusal it can give. Following it would
    // turn the answer meant for the visitor into a request Aperio makes,
    // and the visitor would get whatever was at the other end, or a failure
    // to reach it, instead of being sent there.
    .redirect(reqwest::redirect::Policy::none())
    .build()
  {
    Ok(c) => c,
    Err(e) => {
      tracing::error!("Could not build the forward-auth client: {e}");
      return Verdict::Deny(refusal());
    }
  };

  // The subrequest describes the original rather than replaying it: no body,
  // no method of its own beyond GET, because the endpoint is being asked a
  // question about a request, not asked to serve it.
  let mut req = http
    .get(&cfg.url)
    .header("X-Forwarded-Method", method.as_str())
    .header("X-Forwarded-Proto", "https")
    .header(
      "X-Forwarded-Uri",
      uri.path_and_query().map(|p| p.as_str()).unwrap_or("/"),
    );
  if let Some(host) = host {
    req = req.header("X-Forwarded-Host", host);
  }
  if let Some(ip) = visitor_ip {
    req = req.header("X-Forwarded-For", ip);
  }
  for name in &names {
    if let Some(value) = headers.get(*name).and_then(|v| v.to_str().ok()) {
      req = req.header(*name, value);
    }
  }

  let res = match req.send().await {
    Ok(res) => res,
    Err(e) => {
      // Fail closed, and say so at warn rather than debug: this is the shape
      // of an outage, and an operator reading their logs should find the
      // reason their site went dark in them.
      tracing::warn!("{} did not answer, refusing the request: {e}", cfg.url);
      return Verdict::Deny(refusal());
    }
  };

  let status = res.status();
  if status.is_success() {
    let carried: Vec<(String, String)> = cfg
      .response_headers
      .iter()
      .filter_map(|name| {
        res
          .headers()
          .get(name)
          .and_then(|v| v.to_str().ok())
          .map(|v| (name.clone(), v.to_string()))
      })
      .collect();
    if !cfg.cache.is_zero() {
      remember(state, cfg, &names, headers, host, carried.clone()).await;
    }
    return Verdict::Allow(carried);
  }

  // The endpoint's own answer, forwarded: a redirect to its login, a page
  // explaining itself, whatever it chose. Flattening it into a generic 401
  // would throw away the only part of this that its author wrote.
  Verdict::Deny(relay(res).await)
}

/// The refusal used where the endpoint could not be asked at all.
fn refusal() -> Response {
  Response::builder()
    .status(StatusCode::FORBIDDEN)
    .body(Body::from("403 Forbidden"))
    .unwrap()
}

/// Turns the endpoint's answer into the visitor's, keeping the status and the
/// headers a browser needs to act on it.
async fn relay(res: reqwest::Response) -> Response {
  let status = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::FORBIDDEN);
  let mut builder = Response::builder().status(status);
  for name in ["location", "content-type", "www-authenticate", "set-cookie"] {
    if let Some(value) = res.headers().get(name) {
      builder = builder.header(name, value);
    }
  }
  let body = res.text().await.unwrap_or_default();
  let body = if body.len() > MAX_RESPONSE_BYTES {
    String::new()
  } else {
    body
  };
  builder.body(Body::from(body)).unwrap_or_else(|_| refusal())
}

/// The cache key: the endpoint, the host, and the credential headers that
/// were sent, hashed so no secret is held in the key itself.
fn key(cfg: &ForwardConfig, names: &[&str], headers: &HeaderMap, host: Option<&str>) -> String {
  let mut hasher = Sha256::default();
  hasher.update(cfg.url.as_bytes());
  hasher.update(b"\0");
  hasher.update(host.unwrap_or("-").as_bytes());
  for name in names {
    hasher.update(b"\0");
    hasher.update(name.as_bytes());
    hasher.update(b"=");
    hasher.update(headers.get(*name).map(|v| v.as_bytes()).unwrap_or_default());
  }
  format!("{:x}", hasher.finalize())
}

/// A verdict remembered for this exact credential, if it is still fresh.
async fn cached(
  state: &AppState,
  cfg: &ForwardConfig,
  names: &[&str],
  headers: &HeaderMap,
  host: Option<&str>,
) -> Option<Vec<(String, String)>> {
  let key = key(cfg, names, headers, host);
  let cache = state.forward_auth_cache.lock().await;
  let entry = cache.get(&key)?;
  (entry.decided.elapsed() < cfg.cache).then(|| entry.headers.clone())
}

/// Remembers an admission. **Only an admission**: a refusal is not cached, so
/// somebody who has just been given access does not keep being turned away
/// for the rest of the window.
async fn remember(
  state: &AppState,
  cfg: &ForwardConfig,
  names: &[&str],
  headers: &HeaderMap,
  host: Option<&str>,
  carried: Vec<(String, String)>,
) {
  let key = key(cfg, names, headers, host);
  let mut cache = state.forward_auth_cache.lock().await;
  if cache.len() >= MAX_CACHED {
    cache.retain(|_, v| v.decided.elapsed() < cfg.cache);
    if cache.len() >= MAX_CACHED {
      cache.clear();
    }
  }
  cache.insert(
    key,
    CachedVerdict {
      headers: carried,
      decided: Instant::now(),
    },
  );
}

#[cfg(test)]
#[path = "forward_auth_tests.rs"]
mod tests;
