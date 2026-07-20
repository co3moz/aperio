//! Unit tests for `oidc.rs`. The discovery-fetch code paths in
//! [`build_runtime`] / [`load_from_env`] are exercised against a tiny local
//! mock IdP (an axum server bound to `127.0.0.1:0`) that serves the OpenID
//! Connect discovery document with configurable status/body so we can drive
//! the success, non-200, malformed-JSON, and missing-`userinfo_endpoint`
//! branches without touching the network.

use super::*;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use std::sync::Mutex;
use tokio::net::TcpListener;

/// Spawns a mock IdP that answers the discovery endpoint with a fixed
/// `status`/`body`, returning the base issuer URL (`http://127.0.0.1:<port>`).
/// The background task is torn down when the test's tokio runtime shuts down.
async fn spawn_idp(status: StatusCode, body: String) -> String {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let app = Router::new().route(
    "/.well-known/openid-configuration",
    get(move || {
      let body = body.clone();
      async move { (status, body) }
    }),
  );
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });
  format!("http://{addr}")
}

/// Spawns a mock IdP that serves a discovery document built from its own
/// address by `make_doc(self_base)` — used when endpoints must point back at
/// the running server.
async fn spawn_idp_self(make_doc: impl Fn(&str) -> String) -> String {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let self_base = format!("http://{addr}");
  let doc = make_doc(&self_base);
  let app = Router::new().route(
    "/.well-known/openid-configuration",
    get(move || {
      let doc = doc.clone();
      async move { (StatusCode::OK, doc) }
    }),
  );
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });
  self_base
}

/// A well-formed discovery document whose endpoints point back at `base`.
fn good_doc(base: &str) -> String {
  format!(
    r#"{{
      "issuer": "{base}",
      "authorization_endpoint": "{base}/authorize",
      "token_endpoint": "{base}/token",
      "userinfo_endpoint": "{base}/userinfo"
    }}"#
  )
}

// --- email_allowed (pure) ---------------------------------------------------

#[test]
fn email_allowed_exact_and_domain() {
  let patterns = vec!["ceo@corp.com".to_string(), "*@team.example.com".to_string()];
  assert!(email_allowed("ceo@corp.com", &patterns));
  // Case-insensitive on the input.
  assert!(email_allowed("CEO@Corp.com", &patterns));
  // Leading/trailing whitespace is trimmed.
  assert!(email_allowed("  ceo@corp.com  ", &patterns));
  assert!(email_allowed("dev@team.example.com", &patterns));
  assert!(!email_allowed("dev@corp.com", &patterns));
  // A domain-suffix appended after the allowed domain must not match.
  assert!(!email_allowed(
    "dev@evil-team.example.com.attacker.io",
    &patterns
  ));
  // Empty / whitespace-only email is rejected.
  assert!(!email_allowed("", &patterns));
  assert!(!email_allowed("   ", &patterns));
}

#[test]
fn email_allowed_wildcard_all() {
  assert!(email_allowed("anyone@anywhere.io", &["*".to_string()]));
  // `*` still requires a non-empty email.
  assert!(!email_allowed("", &["*".to_string()]));
}

#[test]
fn email_allowed_domain_prefix_trickery() {
  // A local-part-less lookalike domain must not satisfy `*@team.example.com`.
  assert!(!email_allowed(
    "x@nteam.example.com",
    &["*@team.example.com".to_string()]
  ));
  // An address with no `@` cannot match a domain pattern.
  assert!(!email_allowed(
    "no-at-sign",
    &["*@team.example.com".to_string()]
  ));
}

// --- build_runtime: input validation ---------------------------------------

#[tokio::test]
async fn build_runtime_rejects_empty_issuer() {
  let err = build_runtime(
    "   /",
    "id",
    "secret",
    vec!["*".into()],
    "openid".into(),
    None,
  )
  .await
  .err()
  .unwrap();
  assert!(err.contains("issuer is empty"), "{err}");
}

#[tokio::test]
async fn build_runtime_rejects_missing_client_credentials() {
  let err = build_runtime(
    "https://issuer.example",
    "   ",
    "secret",
    vec!["*".into()],
    "openid".into(),
    None,
  )
  .await
  .err()
  .unwrap();
  assert!(err.contains("client id / client secret"), "{err}");

  let err2 = build_runtime(
    "https://issuer.example",
    "id",
    "",
    vec!["*".into()],
    "openid".into(),
    None,
  )
  .await
  .err()
  .unwrap();
  assert!(err2.contains("client id / client secret"), "{err2}");
}

#[tokio::test]
async fn build_runtime_rejects_empty_allowed_emails() {
  let err = build_runtime(
    "https://issuer.example",
    "id",
    "secret",
    Vec::new(),
    "openid".into(),
    None,
  )
  .await
  .err()
  .unwrap();
  assert!(err.contains("allowed emails must be set"), "{err}");
}

// --- build_runtime: discovery fetch/parse -----------------------------------

#[tokio::test]
async fn build_runtime_success_trims_issuer_and_maps_endpoints() {
  let base = spawn_idp_self(good_doc).await;

  // Pass the issuer with a trailing slash to exercise the trim.
  let rt = build_runtime(
    &format!("{base}/"),
    "my-id",
    "my-secret",
    vec!["a@x.com".into()],
    "openid email".into(),
    Some("https://app.example/callback".into()),
  )
  .await
  .expect("runtime should build");

  assert_eq!(rt.authorization_endpoint, format!("{base}/authorize"));
  assert_eq!(rt.token_endpoint, format!("{base}/token"));
  assert_eq!(rt.userinfo_endpoint, format!("{base}/userinfo"));
  assert_eq!(rt.client_id, "my-id");
  assert_eq!(rt.client_secret, "my-secret");
  assert_eq!(rt.scopes, "openid email");
  assert_eq!(rt.allowed_emails, vec!["a@x.com".to_string()]);
  assert_eq!(
    rt.redirect_url_override.as_deref(),
    Some("https://app.example/callback")
  );

  // A clone shares the same resolved endpoints (exercises `#[derive(Clone)]`).
  let cloned = rt.clone();
  assert_eq!(cloned.token_endpoint, rt.token_endpoint);
}

#[tokio::test]
async fn build_runtime_errors_on_non_200_discovery() {
  let base = spawn_idp(StatusCode::INTERNAL_SERVER_ERROR, "boom".into()).await;
  let err = build_runtime(
    &base,
    "id",
    "secret",
    vec!["*".into()],
    "openid".into(),
    None,
  )
  .await
  .err()
  .unwrap();
  assert!(err.contains("failed to fetch OIDC discovery"), "{err}");
}

#[tokio::test]
async fn build_runtime_errors_on_connection_refused() {
  // Bind then immediately drop the listener so nothing answers on the port.
  let addr = {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap()
  };
  let base = format!("http://{addr}");
  let err = build_runtime(
    &base,
    "id",
    "secret",
    vec!["*".into()],
    "openid".into(),
    None,
  )
  .await
  .err()
  .unwrap();
  assert!(err.contains("failed to fetch OIDC discovery"), "{err}");
}

#[tokio::test]
async fn build_runtime_errors_on_malformed_json() {
  let base = spawn_idp(StatusCode::OK, "not json at all".into()).await;
  let err = build_runtime(
    &base,
    "id",
    "secret",
    vec!["*".into()],
    "openid".into(),
    None,
  )
  .await
  .err()
  .unwrap();
  assert!(err.contains("failed to parse OIDC discovery"), "{err}");
}

#[tokio::test]
async fn build_runtime_errors_when_userinfo_missing() {
  let base = spawn_idp_self(|b| {
    format!(r#"{{"authorization_endpoint":"{b}/authorize","token_endpoint":"{b}/token"}}"#)
  })
  .await;
  let err = build_runtime(
    &base,
    "id",
    "secret",
    vec!["*".into()],
    "openid".into(),
    None,
  )
  .await
  .err()
  .unwrap();
  assert!(
    err.contains("does not advertise a userinfo_endpoint"),
    "{err}"
  );
}

// --- load_from_env ----------------------------------------------------------

/// Serializes the env-mutating test below against any other env access.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const ENV_KEYS: [&str; 6] = [
  "APERIO_OIDC_ISSUER",
  "APERIO_OIDC_ALLOWED_EMAILS",
  "APERIO_OIDC_SCOPES",
  "APERIO_OIDC_REDIRECT_URL",
  "APERIO_OIDC_CLIENT_ID",
  "APERIO_OIDC_CLIENT_SECRET",
];

fn clear_env() {
  for k in ENV_KEYS {
    // SAFETY: env-mutating tests run serialized under ENV_LOCK.
    unsafe { std::env::remove_var(k) };
  }
}

// The std mutex guard is intentionally held across `.await` to serialize the
// process-global env mutations; the awaits inside never re-enter ENV_LOCK.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn load_from_env_covers_none_and_success() {
  let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

  // 1) Issuer unset -> None.
  clear_env();
  assert!(load_from_env().await.is_none());

  // 2) Issuer present but whitespace-only -> None.
  unsafe { std::env::set_var("APERIO_OIDC_ISSUER", "   ") };
  assert!(load_from_env().await.is_none());

  // 3) Fully configured -> Some, using a live mock IdP. Leave SCOPES unset to
  //    exercise the default-scopes branch, and set a redirect override.
  let base = spawn_idp_self(good_doc).await;

  clear_env();
  unsafe {
    std::env::set_var("APERIO_OIDC_ISSUER", &base);
    std::env::set_var("APERIO_OIDC_CLIENT_ID", "cid");
    std::env::set_var("APERIO_OIDC_CLIENT_SECRET", "csecret");
    // Includes blanks and mixed case to exercise trim/lowercase/filter.
    std::env::set_var("APERIO_OIDC_ALLOWED_EMAILS", " Boss@Corp.com , ,*@team.io ");
    std::env::set_var("APERIO_OIDC_REDIRECT_URL", "https://app.example/cb");
  }

  let rt = load_from_env().await.expect("should load runtime");
  assert_eq!(rt.token_endpoint, format!("{base}/token"));
  assert_eq!(rt.userinfo_endpoint, format!("{base}/userinfo"));
  // Default scopes applied when APERIO_OIDC_SCOPES is unset.
  assert_eq!(rt.scopes, "openid email profile");
  // Emails trimmed, lowercased, blanks filtered out.
  assert_eq!(
    rt.allowed_emails,
    vec!["boss@corp.com".to_string(), "*@team.io".to_string()]
  );
  assert_eq!(
    rt.redirect_url_override.as_deref(),
    Some("https://app.example/cb")
  );

  clear_env();
}
