//! Tests for the generated OpenAPI document: that it stays valid and that
//! it keeps describing the routes the server actually serves.

use super::*;

#[test]
fn test_openapi_document_is_complete_and_serializable() {
  let doc = ApiDoc::openapi();
  // Every annotated route is present (paths with multiple methods share
  // one entry, e.g. /aperio/api/tokens carries GET and POST).
  let paths: Vec<&String> = doc.paths.paths.keys().collect();
  for expected in [
    "/aperio/health",
    "/aperio/metrics",
    "/aperio/api/edge/ask",
    "/aperio/api/edge/traefik",
    "/aperio/api/stats",
    "/aperio/api/stats/history",
    "/aperio/api/uptime",
    "/aperio/api/me/totp",
    "/aperio/api/me/totp/setup",
    "/aperio/api/me/totp/enable",
    "/aperio/api/users/{id}/totp",
    "/aperio/auth/passkey",
    "/aperio/auth/passkey/start",
    "/aperio/auth/passkey/finish",
    "/aperio/api/me/passkeys",
    "/aperio/api/me/passkeys/{id}",
    "/aperio/api/me/passkeys/register/start",
    "/aperio/api/me/passkeys/register/finish",
    "/aperio/api/logs",
    "/aperio/api/stream",
    "/aperio/api/session",
    "/aperio/api/clients/{id}/config",
    "/aperio/api/clients/{id}/override",
    "/aperio/api/clients/{id}/enabled",
    "/aperio/api/tokens",
    "/aperio/api/tokens/{id}",
    "/aperio/api/tokens/refresh",
    "/aperio/api/tokens/{id}/rotate",
    "/aperio/api/purge",
    "/aperio/api/scaling",
    "/aperio/api/scaling/{id}",
    "/aperio/api/cache/purge",
    "/aperio/api/publish",
    "/aperio/api/subscribers",
    "/aperio/api/slow-endpoints",
    "/aperio/api/bandwidth",
    "/aperio/api/route-trends",
    "/aperio/api/inbox",
    "/aperio/api/inbox/{id}",
    "/aperio/api/inbox/{id}/refire",
    "/aperio/api/tunnels",
    "/aperio/api/tunnels/{id}",
    "/aperio/api/requests/{id}",
    "/aperio/api/requests/{id}/replay",
    "/aperio/api/audit",
    "/aperio/api/maintenance",
    "/aperio/api/orgs/{id}/hostnames",
    "/aperio/api/share",
    "/aperio/api/settings",
    "/aperio/api/webhooks",
    "/aperio/api/webhooks/{id}",
    "/aperio/auth",
    "/aperio/auth/logout",
    "/aperio/api/users",
    "/aperio/api/users/{id}",
  ] {
    assert!(
      paths.iter().any(|p| p.as_str() == expected),
      "missing path {expected}; got: {paths:?}"
    );
  }
  // The document serializes to valid JSON with schemas included.
  let json = serde_json::to_string(&doc).expect("openapi serializes");
  assert!(json.contains("EnhancedServerStats"));
  assert!(json.contains("TokenCreateRequest"));
}

/// Routes that are deliberately outside the OpenAPI document, and why.
///
/// The document describes an API. These are the endpoints that are not one:
/// the dashboard's own pages and assets, the document itself, two browser
/// redirects, and the four upgrades that stop speaking HTTP the moment they
/// succeed. Each is a decision, so each carries its reason; a new entry here
/// should cost a sentence rather than a line.
const NOT_AN_API: &[(&str, &str)] = &[
  ("/", "the dashboard SPA at the site root"),
  (
    "/aperio/",
    "the dashboard SPA, served as HTML to a browser rather than called",
  ),
  (
    "/aperio/{*rest}",
    "the SPA's client-side routes, served the same index",
  ),
  (
    "/aperio/assets/{*path}",
    "static assets from the embedded dashboard build",
  ),
  (
    "/aperio/api/openapi.json",
    "the document itself; describing it in itself is a mirror, not documentation",
  ),
  (
    "/aperio/oidc/login",
    "a browser redirect into the provider, not a call anything makes",
  ),
  (
    "/aperio/oidc/callback",
    "the provider's redirect back, consumed by the browser that started it",
  ),
  (
    "/aperio/ws",
    "the tunnel's WebSocket upgrade; the protocol past the handshake is the tunnel protocol, not HTTP",
  ),
  (
    "/aperio/tcp",
    "a raw TCP relay upgrade, same reason as /aperio/ws",
  ),
  (
    "/aperio/udp",
    "a UDP relay upgrade, same reason as /aperio/ws",
  ),
  (
    "/aperio/tunnels/{client_id}",
    "binding a declared tunnel: an upgrade a peer client dials, not an API call",
  ),
];

/// Every route the server serves is either in the document or declared not to
/// be.
///
/// The test above asserts a hardcoded list of paths is *present*, which catches
/// a path that disappears and can never catch one that is added: a new admin
/// endpoint with no `utoipa::path` annotation is invisible to the document, to
/// anything generated from it, and to that test. This one scans the router
/// declarations instead, so the list it checks against cannot drift.
///
/// The same shape as the config-surface checks in `aperio-config`: enumerate
/// what exists, and make every absence a declared one with a reason.
#[test]
fn every_route_is_documented_or_declared_not_an_api() {
  fn walk(dir: &std::path::Path, routes: &mut Vec<String>, annotated: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
      return;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        walk(&path, routes, annotated);
        continue;
      }
      if path.extension().is_none_or(|e| e != "rs") || path.to_string_lossy().contains("_tests") {
        continue;
      }
      let Ok(text) = std::fs::read_to_string(&path) else {
        continue;
      };
      let literal_after = |i: usize, skip: usize| -> Option<String> {
        let rest = &text[i + skip..];
        let start = rest.find('"')? + 1;
        let end = rest[start..].find('"')?;
        Some(rest[start..start + end].to_string())
      };
      for (i, _) in text.match_indices(".route(") {
        if let Some(p) = literal_after(i, ".route(".len()) {
          routes.push(p);
        }
      }
      for (i, _) in text.match_indices("path = ") {
        if let Some(p) = literal_after(i, "path = ".len()) {
          annotated.push(p);
        }
      }
    }
  }
  let (mut routes, mut annotated) = (Vec::new(), Vec::new());
  walk(std::path::Path::new("src"), &mut routes, &mut annotated);
  assert!(
    routes.len() > 50 && annotated.len() > 50,
    "{} routes and {} annotations found; the scan is looking in the wrong place",
    routes.len(),
    annotated.len()
  );

  // Everything is nested under `/aperio` except the site root and the handful
  // already written absolute.
  let full: std::collections::BTreeSet<String> = routes
    .into_iter()
    .map(|r| {
      if r == "/" || r.starts_with("/aperio") {
        r
      } else {
        format!("/aperio{r}")
      }
    })
    .collect();
  let missing: Vec<&String> = full
    .iter()
    .filter(|r| {
      !annotated.contains(r) && !NOT_AN_API.iter().any(|(exempt, _)| *exempt == r.as_str())
    })
    .collect();
  assert!(
    missing.is_empty(),
    "routes with no OpenAPI annotation: {missing:?}.\n\n\
     Add a `#[utoipa::path(...)]` to the handler, or add the route to \
     NOT_AN_API with the reason it is not an API. An endpoint in neither is \
     one nobody generating a client will know exists."
  );
}

/// An exemption names a route that still exists, and gives a reason.
#[test]
fn every_openapi_exemption_is_for_a_route_that_exists() {
  let text = {
    let mut all = String::new();
    fn walk(dir: &std::path::Path, out: &mut String) {
      let Ok(entries) = std::fs::read_dir(dir) else {
        return;
      };
      for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
          walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs")
          && !path.to_string_lossy().contains("_tests")
          && let Ok(t) = std::fs::read_to_string(&path)
        {
          out.push_str(&t);
        }
      }
    }
    walk(std::path::Path::new("src"), &mut all);
    all
  };
  for (route, why) in NOT_AN_API {
    let bare = route.strip_prefix("/aperio").unwrap_or(route);
    assert!(
      text.contains(&format!("\"{route}\"")) || text.contains(&format!("\"{bare}\"")),
      "`{route}` is exempted from the OpenAPI document but is not a route; it \
       was probably renamed, and the real one now goes unchecked"
    );
    assert!(
      why.len() > 20,
      "`{route}` is exempted with no real reason given"
    );
  }
}
