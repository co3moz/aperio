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
