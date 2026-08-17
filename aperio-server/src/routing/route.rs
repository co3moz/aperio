//! What a route says about itself, asked before anything is dispatched: does
//! it exist, is it public, what gate is in front of it, what path does it bind,
//! and is it worth waiting for one to appear.

use std::net::IpAddr;

use super::*;
use crate::state::AppState;

/// True when `creds` is a well-formed visitor login (`user:password` with both
/// parts non-empty). The password may itself contain `:` (split on the first).
pub(crate) fn valid_visitor_creds(creds: &str) -> bool {
  match creds.split_once(':') {
    Some((user, pass)) => !user.is_empty() && !pass.is_empty(),
    None => false,
  }
}

/// Resolves the client-declared visitor credentials for a route, if any.
///
/// Returns `Some("user:password")` only when the serving pool is non-empty and
/// *every* client in it declares the *same* override, mirroring the "all
/// members must agree" rule of [`route_is_public`], so a request can never be
/// gated by (or leak past) an override that only some pool members set. Returns
/// `None` (use the server's own gate) when the pool is empty, mixed, declares
/// differing credentials, or the server set `APERIO_IGNORE_CLIENT_AUTH`.
pub(crate) async fn route_visitor_auth(
  state: &AppState,
  uri_path: &str,
  request_host: Option<&str>,
) -> Option<String> {
  if state.config().ignore_client_auth {
    return None;
  }
  // Mirror `route_is_public`: a traversal path must not select (or unlock) a
  // client's per-service credentials for a scope it could escape from.
  if request_path_has_traversal(uri_path) {
    return None;
  }
  let clients = state.clients.read().await;
  let (pool, _) = select_client_pool(
    &clients,
    uri_path,
    request_host,
    state.config().require_hostname_bind,
    state.config().client_down_threshold,
  )?;
  if pool.is_empty() {
    return None;
  }
  let mut creds: Option<&str> = None;
  for r in &pool {
    match r.get(&clients).and_then(|s| s.visitor_auth.as_deref()) {
      Some(c) => match creds {
        None => creds = Some(c),
        Some(existing) if existing == c => {}
        // Differing overrides in the same pool: ambiguous, fall back.
        Some(_) => return None,
      },
      // A pool member without an override: not unanimous, fall back.
      None => return None,
    }
  }
  creds.map(str::to_string)
}

/// The full visitor-auth policy the clients of this route declare, when they
/// all declare the same one (`planned_features.md` #111).
///
/// Same unanimity rule as [`route_visitor_auth`], and for the same reason: a
/// request must never be gated by, or slip past, a policy that only some
/// members of the pool set. A mixed pool falls back to the server's own gate,
/// which is the strictest thing that can be said about a route nobody agrees
/// on.
pub(crate) async fn route_visitor_policy(
  state: &AppState,
  uri_path: &str,
  request_host: Option<&str>,
) -> Option<crate::visitor_auth::Policy> {
  if state.config().ignore_client_auth {
    return None;
  }
  if request_path_has_traversal(uri_path) {
    return None;
  }
  let clients = state.clients.read().await;
  let (pool, _) = select_client_pool(
    &clients,
    uri_path,
    request_host,
    state.config().require_hostname_bind,
    state.config().client_down_threshold,
  )?;
  if pool.is_empty() {
    return None;
  }
  let mut policy: Option<&crate::visitor_auth::Policy> = None;
  for r in &pool {
    match r.get(&clients).and_then(|s| s.visitor_auth_policy.as_ref()) {
      Some(p) => match policy {
        None => policy = Some(p),
        Some(existing) if existing == p => {}
        Some(_) => return None,
      },
      None => return None,
    }
  }
  policy.cloned()
}

/// The path bind of the pool that serves `uri_path`, when it has one.
///
/// **The scope a route's own gate speaks for.** A credential resolved by
/// [`route_visitor_policy`] belongs to the pool that matched, and that pool
/// covers its bind and everything under it, not the hostname: `/metrics` and
/// `/` on one host are two routes with two gates. Anything minted from such a
/// credential has to carry this, or it outranks the policy that produced it.
///
/// `None` means the pool binds no path, so it does serve the whole host and a
/// host-wide scope is the honest answer.
pub(crate) async fn route_path_bind(
  state: &AppState,
  uri_path: &str,
  request_host: Option<&str>,
) -> Option<String> {
  if request_path_has_traversal(uri_path) {
    return None;
  }
  let clients = state.clients.read().await;
  let (pool, _) = select_client_pool(
    &clients,
    uri_path,
    request_host,
    state.config().require_hostname_bind,
    state.config().client_down_threshold,
  )?;
  // Every client in a pool shares the bind, that is what pools them.
  pool
    .first()
    .and_then(|r| r.get(&clients))
    .and_then(|s| s.effective_path_bind().cloned())
}

/// True when any connected client that could serve this host declares a
/// visitor gate of any shape. Used for traversal paths, where the matched
/// path scope cannot be trusted: the gate must assume the strictest override
/// present on the host instead of resolving one per path bind.
///
/// **Both shapes, and that is the whole point of the first condition.** A
/// client's gate lives in one of two fields: the scalar `visitor_auth` for a
/// single `user:password`, and `visitor_auth_policy` for everything the
/// scalar cannot carry, which is `bearer`, `jwt`, and a `basic` naming more
/// than one user. This asked only about the scalar, so a route gated by any
/// of those read as ungated here, and since this is the *entire* gate for a
/// traversal path, the answer was to serve it: `/./admin` reached the backend
/// with no credential while `/admin` answered 401.
pub(crate) async fn host_has_visitor_auth(state: &AppState, request_host: Option<&str>) -> bool {
  if state.config().ignore_client_auth {
    return false;
  }
  let clients = state.clients.read().await;
  // Asked of each service, not of the connection. Reading the gate off one
  // service and the hostname off another is the same bug in a second
  // spelling: a connection carrying a gated `a.example.com` beside an
  // ungated `b.example.com` would answer "gated" for the wrong host and, far
  // worse, "ungated" for the right one. Since this is the *entire* gate for a
  // traversal path, the second reading serves `/./admin` with no credential
  // on a route whose `/admin` answers 401.
  clients.values().any(|c| {
    c.services.iter().any(|s| {
      let declares_gate = s.visitor_auth.is_some()
        || s
          .visitor_auth_policy
          .as_ref()
          .is_some_and(|policy| policy.gates());
      declares_gate
        && match request_host {
          Some(h) => s.matches_host(h) || !s.has_hostname_bind(),
          None => !s.has_hostname_bind(),
        }
    })
  })
}

/// Polls the routing pool until a candidate appears or the deadline passes.
pub(crate) async fn wait_for_candidate(
  state: &AppState,
  uri_path: &str,
  request_host: Option<&str>,
  require_instance: Option<&str>,
  deadline: tokio::time::Instant,
  visitor_ip: Option<IpAddr>,
) -> Option<SelectedClient> {
  loop {
    match pick_proxy_client(
      state,
      uri_path,
      request_host,
      require_instance,
      None,
      visitor_ip,
      // Waiting for *any* client to come back: narrowing to one side of a
      // split here would keep waiting for a version that may not be coming,
      // while the other one is already serving.
      None,
    )
    .await
    {
      PickOutcome::Selected(client) => return Some(*client),
      // Denied: the connected candidates reject this visitor, waiting out
      // the failover window would not change that.
      PickOutcome::Denied(_) => return None,
      PickOutcome::NoRoute => {}
    }
    if tokio::time::Instant::now() >= deadline {
      return None;
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
  }
}

/// True when in-flight failover may re-dispatch this method: idempotent
/// methods (RFC 9110) are safe to send twice, while POST/PATCH may execute
/// twice on the backend and require the APERIO_FAILOVER_ALL_METHODS opt-in.
pub(crate) fn method_retryable(method: &str, all_methods: bool) -> bool {
  all_methods
    || matches!(
      method,
      "GET" | "HEAD" | "OPTIONS" | "PUT" | "DELETE" | "TRACE"
    )
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod tests;
