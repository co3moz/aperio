//! What stands between a visitor and the tunnel: who they are, whether this
//! route lets them through, and what they are told when it does not.
//!
//! The refusals matter more than the admissions here. A gate that fails open is
//! a route serving traffic its config says is closed, so every branch that
//! cannot answer the question refuses rather than guessing.

use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use std::sync::Arc;

use super::*;
use crate::state::AppState;

/// Checks if an HTTP request is a WebSocket upgrade request.
pub(crate) fn is_websocket_upgrade(method: &Method, headers: &HeaderMap) -> bool {
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

/// Who the gate let in, when it knows.
///
/// The gate has always been a wall: it decided whether a request continued
/// and told the backend nothing, so an application behind a tunnel could not
/// say "welcome back" without building a second login next to Aperio's. This
/// is what it knows at the moment it admits someone (`planned_features.md`
/// #109), and it travels only where the operator asked for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VisitorIdentity {
  /// How they were admitted: `session`, `bearer` or `share`.
  pub(crate) how: &'static str,
  /// Who they are, where that is a question with an answer: the email or
  /// username behind a session, or a share link's own id. A `bearer` secret
  /// identifies a caller and not a person, so it has none.
  pub(crate) who: Option<String>,
  /// Headers a `forward` endpoint asked to be carried onto the request that
  /// reaches the backend. This is how that pattern delivers an identity.
  pub(crate) extra_headers: Vec<(String, String)>,
  /// True when the `Authorization` header was the credential that opened the
  /// gate, and is therefore **Aperio's own** rather than the visitor's
  /// message to the backend.
  ///
  /// It is then stripped before the request is forwarded, on the same rule
  /// that already strips the `aperio_session` and `aperio_share` cookies
  /// while leaving every other cookie alone: a credential addressed to the
  /// gate is not addressed to what is behind it, and handing a backend a
  /// secret that opens every route the gate protects is worse than useless to
  /// it. An `Authorization` that did *not* open the gate is the visitor's own
  /// and travels untouched.
  pub(crate) consumed_authorization: bool,
}

/// Outcome of the visitor-auth gate for a proxied request.
pub(crate) enum VisitorGate {
  /// The visitor may proceed, with what the gate learned about them. `None`
  /// where nothing was asked of them: an ungated or deliberately open route
  /// identifies nobody, and saying "anonymous" would be noise rather than
  /// information.
  Allow(Option<VisitorIdentity>),
  /// The visitor is not authorized; reply with this response (a login/OIDC
  /// redirect, or a share-link redirect).
  Deny(Response),
  /// Closed by default, and nothing connected to this route declares it open.
  ///
  /// **Not a refusal yet.** The thing that would declare a route open is a
  /// client, and under scale-to-zero the client is asleep: refusing here
  /// would mean the posture had switched cold start off, since the request
  /// that wakes a service is exactly the one nobody has declared anything
  /// for. So this carries the answer to give *if* no client arrives, and the
  /// caller asks again once one has. A woken client's own declaration then
  /// decides, which is what it would have decided had it never slept.
  Undeclared(Response),
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

/// The bearer secret a request presents, and where it presented it.
///
/// Two forms, and the difference matters beyond parsing: the header form is
/// invisible to logs, the query form is not, which is why a method has to opt
/// into the second and why a browser navigation carrying one is redirected to
/// a clean URL before anything records it.
struct PresentedSecret {
  value: String,
  from_query: bool,
}

/// Reads a bearer secret off a request: `Authorization: Bearer <secret>`
/// first, then `?aperio_token=` when some method in `policy` accepts it.
///
/// The header is preferred whatever the query says, so a caller that can set
/// one is never talked into the form that ends up in logs.
fn presented_secret(
  headers: &HeaderMap,
  uri: &axum::http::Uri,
  policy: &crate::visitor_auth::Policy,
) -> Option<PresentedSecret> {
  if let Some(secret) = headers
    .get("authorization")
    .and_then(|v| v.to_str().ok())
    .and_then(|v| v.strip_prefix("Bearer "))
    .map(str::trim)
    .filter(|s| !s.is_empty())
  {
    return Some(PresentedSecret {
      value: secret.to_string(),
      from_query: false,
    });
  }
  if !policy.accepts_query_token() {
    return None;
  }
  query_token(uri).map(|value| PresentedSecret {
    value,
    from_query: true,
  })
}

/// The `aperio_token=` value in a URI's query string, if there is one.
///
/// Named like `aperio_share`, which it sits beside in the same query string
/// and is treated by the same rule: an `aperio_`-prefixed parameter belongs to
/// the gate and never reaches the backend.
fn query_token(uri: &axum::http::Uri) -> Option<String> {
  uri.query()?.split('&').find_map(|pair| {
    pair
      .strip_prefix(concat!("aperio_token", "="))
      .filter(|v| !v.is_empty())
      .map(|v| v.to_string())
  })
}

/// The same URI without its `aperio_token=` parameter.
pub(crate) fn uri_without_token(uri: &axum::http::Uri) -> String {
  let path = uri.path();
  let Some(query) = uri.query() else {
    return path.to_string();
  };
  let rest: Vec<&str> = query
    .split('&')
    .filter(|pair| !pair.starts_with(concat!("aperio_token", "=")) && !pair.is_empty())
    .collect();
  if rest.is_empty() {
    path.to_string()
  } else {
    format!("{}?{}", path, rest.join("&"))
  }
}

/// The names a `forward` endpoint may answer with, lowercased.
///
/// A visitor's own copy of one of these is dropped on the way to a backend:
/// the operator named them so the backend could trust what is in them, and two
/// headers of one name is not a contradiction a backend is obliged to notice.
pub(super) fn carried_identity_names(state: &AppState) -> Vec<String> {
  state
    .config()
    .visitor_auth
    .forward_methods()
    .flat_map(|cfg| cfg.response_headers.iter())
    .map(|n| n.to_ascii_lowercase())
    .collect()
}

/// Does this inbound header belong to Aperio rather than to the backend?
///
/// The three that are never the visitor's to send onward, in one place because
/// there are two proxy paths and a rule written twice is a rule that will hold
/// on one of them: the `x-aperio-` namespace the server speaks in, a credential
/// that opened Aperio's own gate, and a name a `forward` endpoint delivers an
/// identity under. The `x-aperio-` strip is unconditional on purpose, a header
/// that is only removed while a feature is switched on is a header that can be
/// forged by switching it off.
pub(super) fn header_is_aperios(
  name: &str,
  carried_names: &[String],
  consumed_authorization: bool,
) -> bool {
  if name.len() > 9 && name[..9].eq_ignore_ascii_case("x-aperio-") {
    return true;
  }
  if consumed_authorization && name.eq_ignore_ascii_case("authorization") {
    return true;
  }
  carried_names.iter().any(|n| name.eq_ignore_ascii_case(n))
}

/// A `Cookie` header value with Aperio's own cookies removed, and everything
/// the visitor set left alone. Empty when nothing survives, which is the
/// caller's signal to send no cookie header at all.
pub(super) fn cookies_without_aperios(value: &str) -> String {
  value
    .split(';')
    .filter(|part| {
      let trimmed = part.trim();
      !trimmed.starts_with("aperio_session=")
        && !trimmed.starts_with("__Host-aperio_session=")
        && !trimmed.starts_with("aperio_share=")
        && !trimmed.starts_with("aperio_affinity=")
    })
    .map(str::trim)
    .collect::<Vec<&str>>()
    .join("; ")
}

/// Is this request a browser navigation, rather than a call from something
/// that speaks in headers?
///
/// The signal is the one `serve_spa` already uses for the same question. It
/// decides two things here: whether a refusal is a redirect to a login page or
/// a `401` the caller can answer, and whether a secret in the URL is turned
/// into a cookie so the page's own assets load.
fn is_navigation(method: &axum::http::Method, headers: &HeaderMap) -> bool {
  method == axum::http::Method::GET
    && headers
      .get("accept")
      .and_then(|v| v.to_str().ok())
      .is_some_and(|v| v.contains("text/html"))
}

/// Refuses a gated request in the shape its caller can act on.
///
/// A browser is sent to a login page, as it always was. Anything else, when
/// the gate has a method that lives on the request itself, gets `401` with a
/// `WWW-Authenticate` challenge: redirecting a script to an HTML login form
/// answers a question it did not ask, and it is why a gated route could not
/// be reached with `curl` at all.
fn refuse_visitor(
  policy: &crate::visitor_auth::Policy,
  navigation: bool,
  login_path: &str,
  uri_str: &str,
) -> Response {
  match policy.challenge() {
    Some(scheme) if !navigation && policy.has_direct_method() => Response::builder()
      .status(StatusCode::UNAUTHORIZED)
      .header("WWW-Authenticate", scheme)
      .body(Body::empty())
      .unwrap(),
    _ => login_redirect(login_path, uri_str),
  }
}

/// The token a `jwt` method should check, from wherever that method says it
/// arrives: the `Authorization: Bearer` header by default, or the cookie an
/// identity-aware proxy in front writes.
fn jwt_token_from_request(headers: &HeaderMap, cfg: &crate::jwt::JwtConfig) -> Option<String> {
  match cfg.cookie.as_deref() {
    Some(name) => headers
      .get("cookie")
      .and_then(|v| v.to_str().ok())?
      .split(';')
      .filter_map(|part| part.trim().split_once('='))
      .find(|(k, _)| *k == name)
      .map(|(_, v)| v.to_string()),
    None => headers
      .get("authorization")
      .and_then(|v| v.to_str().ok())
      .and_then(|v| v.strip_prefix("Bearer "))
      .map(str::trim)
      .filter(|s| !s.is_empty())
      .map(str::to_string),
  }
}

/// The identity behind a session cookie, for a request the gate has already
/// admitted on the strength of it.
async fn session_identity(state: &AppState, headers: &HeaderMap) -> Option<VisitorIdentity> {
  Some(VisitorIdentity {
    how: "session",
    who: crate::auth::session_username_any_scope(state, headers).await,
    extra_headers: Vec::new(),
    consumed_authorization: false,
  })
}

/// Applies the methods that live on the request itself, `bearer` and `jwt`,
/// and answers when one of them decides.
///
/// Shared by the client-declared gate and the server's own, because the same
/// written policy must behave the same whichever side wrote it. It did not:
/// the two branches ran the same helpers in two hand-written sequences, and a
/// `bearer` with `query: true` got the clean-address redirect on one side and
/// a bare admission on the other, so a page loaded through a client-declared
/// gate rendered and then failed to fetch a single one of its own assets.
async fn apply_request_methods(
  state: &Arc<AppState>,
  policy: &crate::visitor_auth::Policy,
  headers: &HeaderMap,
  uri: &axum::http::Uri,
  host: Option<&str>,
  navigation: bool,
) -> Option<VisitorGate> {
  if let Some(presented) = presented_secret(headers, uri, policy)
    && policy.admits_bearer(&presented.value, presented.from_query)
  {
    if presented.from_query
      && navigation
      && let Some(host) = host
    {
      // A secret in the URL of a page load becomes a cookie and the visitor
      // is sent to the clean address, so the page's own assets are not each a
      // second request carrying it, and so the address reaching the access
      // log, the `Referer` of every outbound link and the browser's history
      // has no secret in it. What a share link does on its first click, and
      // it reuses that cookie rather than inventing a second one.
      //
      // Scoped to the route whose policy just admitted the secret. The cookie
      // is read by every branch of the gate, so a host-wide one minted from a
      // per-route secret would open routes that secret was never a key to.
      let scope = crate::routing::route_path_bind(state, uri.path(), Some(host)).await;
      return Some(VisitorGate::Deny(crate::share::grant_cookie_and_redirect(
        state,
        host,
        scope,
        &uri_without_token(uri),
      )));
    }
    return Some(VisitorGate::Allow(Some(VisitorIdentity {
      how: "bearer",
      who: None,
      extra_headers: Vec::new(),
      consumed_authorization: !presented.from_query,
    })));
  }
  for cfg in policy.jwt_methods() {
    let Some(token) = jwt_token_from_request(headers, cfg) else {
      continue;
    };
    if let Some(verified) = crate::jwt::verify(state, cfg, &token).await {
      return Some(VisitorGate::Allow(Some(VisitorIdentity {
        how: "jwt",
        who: verified.who,
        extra_headers: Vec::new(),
        // Only where the bearer header carried it: a token in a cookie is the
        // visitor's own, and stripping the cookie header would take the
        // application's session with it.
        consumed_authorization: cfg.cookie.is_none(),
      })));
    }
  }
  None
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
///    global session), or a share cookie/link that covers the route. The login
///    always uses the password form (never OIDC), since the credentials are the
///    client's.
/// 2. Otherwise the server's own gate applies: public routes skip it; a
///    configured server password / OIDC requires a global session or a share,
///    and a `bearer` method may also be satisfied by the request itself.
///
/// `method` is here only to tell a browser navigation from a call by
/// something that speaks in headers, which decides the shape of a refusal and
/// what happens to a secret presented in the URL.
///
/// `caller_ip` is the address the rest of the server already decided this
/// request came from, and it is passed in rather than read here **because a
/// gate may not do its own version of that decision**. `X-Forwarded-For` is a
/// header any visitor can write, so it is worth something only after
/// `trust_proxy` and `trusted_proxies` have been applied to it, which is what
/// `extract_client_ip` does for rate limiting, the access log and every other
/// consumer. A gate that read the raw header would be the one place in the
/// server where a visitor picks their own address, and it would hand that
/// choice to an endpoint deciding whether to let them in.
pub(crate) async fn check_visitor_gate(
  state: &Arc<AppState>,
  method: &axum::http::Method,
  headers: &HeaderMap,
  uri: &axum::http::Uri,
  host: Option<&str>,
  caller_ip: std::net::IpAddr,
) -> VisitorGate {
  let path = uri.path();
  // Used only where a `forward` method will send it on: the header is what
  // tells the endpoint who is asking. Always present, because the socket's own
  // peer is the fallback, so an endpoint that decides on the address is never
  // asked about a visitor who appears to have none.
  let visitor_ip = caller_ip.to_string();
  let navigation = is_navigation(method, headers);

  // 0. Traversal paths never weaken the gate. `/a/../b` matches an `/a` path
  // bind, but a backend that resolves `..` serves `/b`, so such a path is
  // never public, never unlocks a client's per-service credentials or a share
  // scope (share checks reject it too), and when any gate could apply on this
  // host it requires a full (global) session.
  if crate::routing::request_path_has_traversal(path) {
    let gated = state.config().visitor_auth.gates()
      || state.oidc.is_some()
      || crate::routing::host_has_visitor_auth(state, host).await;
    if !gated {
      // The closed posture is decided here too, not only in section 2 below.
      // Section 2 is where it used to be checked and a traversal path returns
      // before ever reaching it, so `deny` was the one posture a `.` in the
      // path could switch off. The answer is the one an unclaimed hostname
      // gives, as it is there: a route nothing declared reachable does not
      // announce its existence to a caller who was never going to be let in.
      if state.config().default_access == crate::settings::DefaultAccess::Deny {
        tracing::debug!(
          "Nothing declares {} on {} open, and the posture is closed",
          path,
          host.unwrap_or("-")
        );
        return VisitorGate::Deny(gateway_timeout_response(
          state,
          host,
          "504 Gateway Timeout - No client connected in time",
        ));
      }
      return VisitorGate::Allow(None);
    }
    if validate_session_for_visitor(state, headers, host).await {
      return VisitorGate::Allow(session_identity(state, headers).await);
    }
    let login_path = if state.oidc.is_some() {
      "/aperio/oidc/login"
    } else {
      "/aperio/auth"
    };
    return VisitorGate::Deny(login_redirect(login_path, &uri.to_string()));
  }

  // 1a. A client-declared policy richer than a password: the same methods
  // the server's own gate has, evaluated by the same code, because a gate
  // written on the client and a gate written on the server are one grammar
  // (#105) and would otherwise be two implementations of it (#111).
  if let Some(declared) = crate::routing::route_visitor_policy(state, path, host).await {
    if declared.admits_everyone() {
      return VisitorGate::Allow(None);
    }
    if validate_session_for_host(state, headers, host).await {
      return VisitorGate::Allow(session_identity(state, headers).await);
    }
    if let Some(decided) =
      apply_request_methods(state, &declared, headers, uri, host, navigation).await
    {
      return decided;
    }
    return match check_share_access(state, headers, uri, host) {
      Some(Some(redirect)) => VisitorGate::Deny(redirect),
      Some(None) => VisitorGate::Allow(Some(VisitorIdentity {
        how: "share",
        who: None,
        extra_headers: Vec::new(),
        consumed_authorization: false,
      })),
      None => VisitorGate::Deny(refuse_visitor(
        &declared,
        navigation,
        "/aperio/auth",
        &uri.to_string(),
      )),
    };
  }

  // 1b. Client-declared per-service visitor password override.
  if crate::routing::route_visitor_auth(state, path, host)
    .await
    .is_some()
  {
    if validate_session_for_host(state, headers, host).await {
      return VisitorGate::Allow(session_identity(state, headers).await);
    }
    return match check_share_access(state, headers, uri, host) {
      Some(Some(redirect)) => VisitorGate::Deny(redirect),
      Some(None) => VisitorGate::Allow(Some(VisitorIdentity {
        how: "share",
        who: None,
        extra_headers: Vec::new(),
        consumed_authorization: false,
      })),
      None => VisitorGate::Deny(login_redirect("/aperio/auth", &uri.to_string())),
    };
  }

  // 2. Server's own visitor gate.
  let config = state.config();
  let policy = &config.visitor_auth;
  let auth_configured = policy.gates() || state.oidc.is_some();
  // Declared open, by every client that could serve this route. This is the
  // one sentence that opens a route under either posture, which is what makes
  // `deny` expressible at all rather than being a second, parallel switch.
  if crate::routing::route_is_public(state, path, host).await {
    return VisitorGate::Allow(None);
  }
  if !auth_configured {
    // Nothing gates this route. Under the default posture that has always
    // meant "serve it"; under `deny` it means the route was never declared
    // reachable, and the answer is the one an unclaimed hostname already
    // gives, so the existence of something here does not leak to a caller who
    // was never going to be let in.
    if config.default_access == crate::settings::DefaultAccess::Deny {
      tracing::debug!(
        "Nothing declares {} on {} open, and the posture is closed",
        path,
        host.unwrap_or("-")
      );
      return VisitorGate::Undeclared(gateway_timeout_response(
        state,
        host,
        "504 Gateway Timeout - No client connected in time",
      ));
    }
    return VisitorGate::Allow(None);
  }
  if validate_session_for_visitor(state, headers, host).await {
    return VisitorGate::Allow(session_identity(state, headers).await);
  }
  // The methods that live on the request itself, which is the only way a
  // caller without a browser gets in: the session cookie is the whole of what
  // the gate used to look at. The same call the client-declared branch above
  // makes, so one written policy cannot behave two ways.
  if let Some(decided) = apply_request_methods(state, policy, headers, uri, host, navigation).await
  {
    return decided;
  }
  // Delegated: an endpoint the operator runs is asked about this request.
  // Last of the methods, because it is the only one that costs a round trip,
  // and its refusal is held rather than returned so a share link still gets
  // its chance below.
  let mut delegated_refusal = None;
  for cfg in policy.forward_methods() {
    match crate::forward_auth::ask(state, cfg, method, headers, uri, host, Some(&visitor_ip)).await
    {
      crate::forward_auth::Verdict::Allow(carried) => {
        return VisitorGate::Allow(Some(VisitorIdentity {
          how: "forward",
          who: carried
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("x-auth-user"))
            .map(|(_, v)| v.clone()),
          extra_headers: carried,
          consumed_authorization: false,
        }));
      }
      crate::forward_auth::Verdict::Deny(resp) => delegated_refusal = Some(resp),
    }
  }
  match check_share_access(state, headers, uri, host) {
    Some(Some(redirect)) => VisitorGate::Deny(redirect),
    Some(None) => VisitorGate::Allow(Some(VisitorIdentity {
      how: "share",
      who: None,
      extra_headers: Vec::new(),
      consumed_authorization: false,
    })),
    None => {
      let login_path = if state.oidc.is_some() {
        "/aperio/oidc/login"
      } else {
        "/aperio/auth"
      };
      // The endpoint's own answer wins over the generic refusal: a redirect
      // to its login, a page explaining itself, whatever its author chose.
      // Flattening that into a 401 would discard the only part of this
      // method they wrote.
      match delegated_refusal {
        Some(resp) => VisitorGate::Deny(resp),
        None => VisitorGate::Deny(refuse_visitor(
          policy,
          navigation,
          login_path,
          &uri.to_string(),
        )),
      }
    }
  }
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;
