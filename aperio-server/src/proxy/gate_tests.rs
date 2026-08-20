//! Who a visitor is and whether this route lets them through: every method
//! the gate accepts, the refusals it gives when it cannot answer, and the
//! identity headers a admitted visitor arrives at the backend with.

use super::*;
use crate::test_support::*;
use std::sync::Arc;

// --- check_visitor_gate ------------------------------------------------------

/// The address a test's visitor arrives from, where the test is about
/// something else.
const VISITOR_IP: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7));

/// [`crate::proxy::check_visitor_gate`] with an address supplied.
///
/// The gate takes the caller's address because a `forward` method tells the
/// endpoint who is asking, and it takes it as an argument rather than reading
/// `X-Forwarded-For` itself, since that header is worth something only after
/// the trusted-proxy rules have been applied to it. Nearly every test here is
/// about something other than where the request came from, so they go through
/// this; a test that *is* about the address calls the real one and passes its
/// own.
async fn check_visitor_gate(
  state: &Arc<AppState>,
  method: &axum::http::Method,
  headers: &HeaderMap,
  uri: &axum::http::Uri,
  host: Option<&str>,
) -> VisitorGate {
  crate::proxy::check_visitor_gate(state, method, headers, uri, host, VISITOR_IP).await
}

#[tokio::test]
async fn visitor_gate_allows_without_auth() {
  let state = Arc::new(test_state_with(test_config()));
  let uri: axum::http::Uri = "/anything".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

#[tokio::test]
async fn visitor_gate_denies_when_auth_configured() {
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/private".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  match gate {
    VisitorGate::Deny(resp) => {
      assert_eq!(resp.status(), StatusCode::FOUND);
      let loc = resp.headers().get("Location").unwrap().to_str().unwrap();
      assert!(loc.starts_with("/aperio/auth?redirect="));
    }
    VisitorGate::Allow(_) => panic!("expected deny"),
    VisitorGate::Undeclared(_) => panic!("expected a deny, not an undeclared route"),
  }
}

#[tokio::test]
async fn visitor_gate_traversal_requires_session() {
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/a/../b".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Deny(_)));
}

#[tokio::test]
async fn visitor_gate_per_route_visitor_auth() {
  // A client declaring a per-service visitor password supersedes the server
  // gate: without a host session (and no share), the visitor is denied.
  let state = Arc::new(test_state_with(test_config()));
  let mut c = mock_client(None, None, None, None);
  c.sole_mut().visitor_auth = Some("pw".to_string());
  state.clients.write().await.insert("c1".to_string(), c);
  let uri: axum::http::Uri = "/svc".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Deny(_)));

  // A valid session for the host unlocks it.
  let token =
    crate::test_support::seed_session(&state, crate::store::users::Role::Admin, None, None).await;
  let mut headers = HeaderMap::new();
  headers.insert(
    "cookie",
    HeaderValue::from_str(&format!("aperio_session={token}")).unwrap(),
  );
  let gate = check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

#[tokio::test]
async fn visitor_gate_admits_a_bearer_secret_from_a_header() {
  // The case that had no answer at all: a caller with no browser reaching a
  // gated route. The session cookie was the whole of what the gate looked at.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str("{method: bearer, secret: \"0123456789abcdef-secret\"}").unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/api/items".parse().unwrap();

  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    HeaderValue::from_static("Bearer 0123456789abcdef-secret"),
  );
  let gate = check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await;
  assert!(matches!(gate, VisitorGate::Allow(_)));

  let mut wrong = HeaderMap::new();
  wrong.insert("authorization", HeaderValue::from_static("Bearer nope"));
  let gate = check_visitor_gate(&state, &axum::http::Method::GET, &wrong, &uri, None).await;
  assert!(matches!(gate, VisitorGate::Deny(_)));
}

#[tokio::test]
async fn a_caller_without_a_browser_is_refused_with_a_challenge_rather_than_a_redirect() {
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str("{method: bearer, secret: \"0123456789abcdef-secret\"}").unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/api/items".parse().unwrap();

  // No `Accept: text/html`: a script, which cannot act on an HTML login page.
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  match gate {
    VisitorGate::Deny(resp) => {
      assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
      assert_eq!(
        resp.headers().get("WWW-Authenticate").unwrap(),
        "Bearer",
        "the refusal has to say what to present"
      );
    }
    VisitorGate::Allow(_) => panic!("expected deny"),
    VisitorGate::Undeclared(_) => panic!("expected a deny, not an undeclared route"),
  }

  // The same gate, a browser navigation: still the login page, because that
  // is the shape a browser can act on.
  let mut browser = HeaderMap::new();
  browser.insert("accept", HeaderValue::from_static("text/html"));
  let gate = check_visitor_gate(&state, &axum::http::Method::GET, &browser, &uri, None).await;
  match gate {
    VisitorGate::Deny(resp) => assert_eq!(resp.status(), StatusCode::FOUND),
    VisitorGate::Allow(_) => panic!("expected deny"),
    VisitorGate::Undeclared(_) => panic!("expected a deny, not an undeclared route"),
  }
}

#[tokio::test]
async fn a_secret_in_the_url_opens_nothing_unless_the_gate_asked_for_that_form() {
  let header_only = "{method: bearer, secret: \"0123456789abcdef-secret\"}";
  let mut cfg = test_config();
  cfg.visitor_auth =
    crate::visitor_auth::Policy::compile(&serde_yaml::from_str(header_only).unwrap());
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/api/items?aperio_token=0123456789abcdef-secret"
    .parse()
    .unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(
    matches!(gate, VisitorGate::Deny(_)),
    "the query form is opt-in, and this gate did not opt in"
  );
}

#[tokio::test]
async fn a_page_opened_with_a_secret_in_its_url_is_sent_to_a_clean_address() {
  // Otherwise the secret is in the browser's history, in the `Referer` of
  // every outbound link, and on each of the page's own assets.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str("{method: bearer, secret: \"0123456789abcdef-secret\", query: true}")
      .unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/report?aperio_token=0123456789abcdef-secret&page=2"
    .parse()
    .unwrap();
  let mut browser = HeaderMap::new();
  browser.insert("accept", HeaderValue::from_static("text/html"));

  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &browser,
    &uri,
    Some("app.example.com"),
  )
  .await;
  match gate {
    VisitorGate::Deny(resp) => {
      assert_eq!(resp.status(), StatusCode::FOUND);
      let location = resp.headers().get("Location").unwrap().to_str().unwrap();
      assert_eq!(location, "/report?page=2", "the other parameters survive");
      assert!(!location.contains("aperio_token"));
      let cookie = resp.headers().get("Set-Cookie").unwrap().to_str().unwrap();
      assert!(cookie.starts_with("aperio_share="), "{cookie}");
    }
    VisitorGate::Allow(_) => panic!("expected the clean-address redirect"),
    VisitorGate::Undeclared(_) => panic!("expected the clean-address redirect"),
  }

  // A non-navigation with the same secret is simply admitted: there is no
  // page whose assets need a cookie, and a redirect would break the call.
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    Some("app.example.com"),
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

// --- identity headers (planned_features #47) --------------------------------

#[test]
fn the_aperio_header_namespace_is_recognised_case_insensitively() {
  // The strip is a prefix test on the raw name, so this pins the shape of it:
  // anything in the namespace goes, anything else stays, whatever the case.
  let is_ours = |k: &str| k.len() > 9 && k[..9].eq_ignore_ascii_case("x-aperio-");
  assert!(is_ours("x-aperio-org"));
  assert!(is_ours("X-Aperio-Client-Id"));
  assert!(is_ours("X-APERIO-TOKEN"));
  // Not in the namespace.
  assert!(!is_ours("x-aperio"), "the bare prefix names no header");
  assert!(!is_ours("x-aperio-"), "nothing after the prefix");
  assert!(!is_ours("x-request-id"));
  assert!(!is_ours("authorization"));
  assert!(
    !is_ours("x-aperiox-thing"),
    "a different namespace that starts alike"
  );
}

#[tokio::test]
async fn closed_by_default_refuses_a_route_nothing_declares_open() {
  // The posture, and the whole of what it changes: with no `auth:` anywhere,
  // a route used to be served because nothing said otherwise.
  let mut cfg = test_config();
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/anything".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  match gate {
    // `Undeclared` rather than `Deny`, and the distinction is the point: the
    // thing that would declare this route open is a client, and under
    // scale-to-zero it may be asleep, so the handler asks again after the
    // cold start rather than refusing here. The answer carried is the one to
    // give if nobody arrives, and it is the answer an unclaimed route already
    // gives, so the existence of something here does not leak to a caller who
    // was never going to be let in.
    VisitorGate::Undeclared(resp) => assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT),
    VisitorGate::Deny(_) => panic!("expected the closed-by-default answer, not a refusal"),
    VisitorGate::Allow(_) => panic!("expected the closed-by-default answer"),
  }

  // The same request under the default posture, which is unchanged.
  let state = Arc::new(test_state_with(test_config()));
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

#[tokio::test]
async fn closed_by_default_still_serves_what_declares_itself_open() {
  // `public: true` is the sentence that opens a route, which is what makes
  // the posture expressible rather than being a second, parallel switch.
  let mut cfg = test_config();
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  let state = Arc::new(test_state_with(cfg));
  let mut c = mock_client(None, None, None, None);
  c.sole_mut().public = true;
  state.clients.write().await.insert("c1".to_string(), c);

  let uri: axum::http::Uri = "/anything".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

#[tokio::test]
async fn closed_by_default_leaves_a_configured_gate_exactly_as_it_was() {
  // The posture decides what an *unstated* route means. A route with a gate
  // has stated something, so nothing about it changes.
  let mut cfg = test_config();
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/private".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  match gate {
    VisitorGate::Deny(resp) => assert_eq!(
      resp.status(),
      StatusCode::FOUND,
      "a gated route still sends the visitor somewhere they can act"
    ),
    VisitorGate::Allow(_) => panic!("expected deny"),
    VisitorGate::Undeclared(_) => panic!("expected a deny, not an undeclared route"),
  }
}

#[tokio::test]
async fn a_session_from_one_organization_does_not_open_another_ones_gated_site() {
  // The visitor gate and the dashboard share one session store, and the gate
  // asked only "is this a global session". A session bound to `acme`, even a
  // read-only one, therefore walked past the gate on every hostname on the
  // server, including hostnames served for other tenants.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["acme.example.com".to_string()], None)
    .expect("the organization");

  // The shape a per-organization OIDC login produces: a global session that
  // is fixed to one organization.
  let token = uuid::Uuid::new_v4().to_string();
  {
    let now = crate::store::sessions::now_secs();
    state.sessions.lock().await.insert(
      &token,
      crate::store::sessions::SessionInfo {
        plane: crate::store::sessions::Plane::Admin,
        expires_at: now + 86400,
        created_at: now,
        ip: Some("127.0.0.1".to_string()),
        user_agent: None,
        scope_host: None,
        username: Some("viewer@acme.example.com".to_string()),
        role: crate::store::users::Role::Viewer,
        selected_org: None,
        bound_org: Some(org.id.clone()),
      },
    );
  }
  let headers = crate::test_support::cookie_headers(&token);
  let uri: axum::http::Uri = "/private".parse().unwrap();

  // Its own organization's hostname: admitted, as it always was.
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &headers,
    &uri,
    Some("acme.example.com"),
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));

  // Another tenant's: refused.
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &headers,
    &uri,
    Some("globex.example.com"),
  )
  .await;
  assert!(
    matches!(gate, VisitorGate::Deny(_)),
    "an organization's session reached past another organization's gate"
  );
}

#[tokio::test]
async fn a_master_session_still_reaches_every_gated_site() {
  // The fence is on the organization, and master has none. An operator's own
  // dashboard login behaves exactly as it did, which is what keeps this a fix
  // for the cross-tenant case rather than a change for everyone.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let token =
    crate::test_support::seed_session(&state, crate::store::users::Role::Admin, None, None).await;
  let headers = crate::test_support::cookie_headers(&token);
  let uri: axum::http::Uri = "/private".parse().unwrap();

  for host in ["acme.example.com", "globex.example.com", "anything.at.all"] {
    let gate =
      check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, Some(host)).await;
    assert!(matches!(gate, VisitorGate::Allow(_)), "{host}");
  }
}

#[tokio::test]
async fn a_fenced_session_without_a_host_header_is_refused() {
  // A fenced organization has no claim on a request that names no hostname,
  // and admitting it would be the same hole wearing a missing header.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["acme.example.com".to_string()], None)
    .expect("the organization");
  let token = uuid::Uuid::new_v4().to_string();
  {
    let now = crate::store::sessions::now_secs();
    state.sessions.lock().await.insert(
      &token,
      crate::store::sessions::SessionInfo {
        plane: crate::store::sessions::Plane::Admin,
        expires_at: now + 86400,
        created_at: now,
        ip: Some("127.0.0.1".to_string()),
        user_agent: None,
        scope_host: None,
        username: Some("viewer@acme.example.com".to_string()),
        role: crate::store::users::Role::Viewer,
        selected_org: None,
        bound_org: Some(org.id.clone()),
      },
    );
  }
  let headers = crate::test_support::cookie_headers(&token);
  let uri: axum::http::Uri = "/private".parse().unwrap();
  let gate = check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await;
  assert!(matches!(gate, VisitorGate::Deny(_)));
}

#[tokio::test]
async fn the_gate_says_who_it_let_in() {
  // The identity a backend may be told (#109). It is what the gate already
  // knew at the moment it admitted someone and never said, which is why an
  // application behind a tunnel had to build a second login to greet anyone.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str("{method: bearer, secret: \"0123456789abcdef-secret\"}").unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/api/items".parse().unwrap();

  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    HeaderValue::from_static("Bearer 0123456789abcdef-secret"),
  );
  match check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await {
    VisitorGate::Allow(Some(id)) => {
      assert_eq!(id.how, "bearer");
      assert_eq!(id.who, None, "a secret identifies a caller, not a person");
    }
    _ => panic!("expected an admitted bearer caller"),
  }

  // A session carries the name behind it, which is the answer worth having.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:secret");
  let state = Arc::new(test_state_with(cfg));
  let token = crate::test_support::seed_session(
    &state,
    crate::store::users::Role::Admin,
    Some("alice@example.com"),
    None,
  )
  .await;
  let headers = crate::test_support::cookie_headers(&token);
  match check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await {
    VisitorGate::Allow(Some(id)) => {
      assert_eq!(id.how, "session");
      assert_eq!(id.who.as_deref(), Some("alice@example.com"));
    }
    _ => panic!("expected an admitted session"),
  }
}

#[tokio::test]
async fn an_open_route_names_nobody() {
  // Nothing was asked of this visitor, so there is nothing to announce, and
  // a header saying "anonymous" would be noise a backend learns to ignore.
  let state = Arc::new(test_state_with(test_config()));
  let uri: axum::http::Uri = "/anything".parse().unwrap();
  match check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await
  {
    VisitorGate::Allow(identity) => assert_eq!(identity, None),
    VisitorGate::Deny(_) => panic!("expected allow"),
    VisitorGate::Undeclared(_) => panic!("expected allow"),
  }
}

#[tokio::test]
async fn a_route_nothing_is_connected_to_is_undeclared_rather_than_refused() {
  // The distinction the cold start depends on. Closed by default and nothing
  // connected means the client that would declare this route open may simply
  // be asleep, so the gate hands back the answer to give *if* nobody arrives
  // and lets the handler ask again after the wake. Refusing outright here
  // would have switched scale-to-zero off, since the request that wakes a
  // service is exactly the one nothing has declared anything for.
  let mut cfg = test_config();
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/anything".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    Some("asleep.e2e.local"),
  )
  .await;
  match gate {
    VisitorGate::Undeclared(resp) => assert_eq!(
      resp.status(),
      StatusCode::GATEWAY_TIMEOUT,
      "the held answer is the one an unclaimed route gives"
    ),
    VisitorGate::Deny(_) => panic!("a sleeping service must not be refused outright"),
    VisitorGate::Allow(_) => panic!("nothing declared this route open"),
  }

  // And a client that *is* connected and declares itself open is served, so
  // the posture is not simply deferring everything.
  let mut c = mock_client(Some("awake.e2e.local"), None, None, None);
  c.sole_mut().public = true;
  state.clients.write().await.insert("c1".to_string(), c);
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    Some("awake.e2e.local"),
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

#[tokio::test]
async fn the_credential_that_opened_the_gate_does_not_travel_to_the_backend() {
  // The header that opened Aperio's gate is Aperio's, on the same rule that
  // already strips the internal cookies: handing a backend a secret that
  // opens every route the gate protects is worse than useless to it.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str("{method: bearer, secret: \"0123456789abcdef-secret\", query: true}")
      .unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/api/items".parse().unwrap();

  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    HeaderValue::from_static("Bearer 0123456789abcdef-secret"),
  );
  match check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await {
    VisitorGate::Allow(Some(id)) => assert!(id.consumed_authorization),
    _ => panic!("expected an admitted bearer caller"),
  }

  // The query form consumes no header, so an `Authorization` the visitor
  // happened to be sending is theirs and reaches the backend untouched.
  let query: axum::http::Uri = "/api/items?aperio_token=0123456789abcdef-secret"
    .parse()
    .unwrap();
  match check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &query,
    None,
  )
  .await
  {
    VisitorGate::Allow(Some(id)) => assert!(!id.consumed_authorization),
    _ => panic!("expected an admitted query caller"),
  }
}

#[tokio::test]
async fn a_forward_endpoint_is_told_the_address_the_server_decided_on() {
  // `X-Forwarded-For` is a header any visitor can write, and the gate is the
  // last place that should take one at face value: the address it sends is
  // what an endpoint allowlisting source addresses decides on. So the gate is
  // handed the address the trusted-proxy rules already produced, and a visitor
  // writing their own header changes nothing. A visitor behind no proxy at all
  // still has one, the socket's own peer, rather than reaching the endpoint as
  // an unnamed caller its rules can never match.
  let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let seen_task = seen.clone();
  tokio::spawn(async move {
    while let Ok((mut sock, _)) = listener.accept().await {
      let seen = seen_task.clone();
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 4096];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        seen
          .lock()
          .unwrap()
          .push(String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase());
        let _ = sock
          .write_all(b"HTTP/1.1 403 X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
          .await;
      });
    }
  });

  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str(&format!(
      "{{method: forward, url: \"http://127.0.0.1:{port}/authcheck\"}}"
    ))
    .unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));

  let mut spoofed = HeaderMap::new();
  spoofed.insert("x-forwarded-for", HeaderValue::from_static("10.0.0.5"));
  let gate = crate::proxy::check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &spoofed,
    &"/private".parse().unwrap(),
    None,
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 9)),
  )
  .await;
  assert!(matches!(gate, VisitorGate::Deny(_)), "the endpoint refused");

  let asked = seen.lock().unwrap();
  let raw = asked.first().expect("the endpoint was asked");
  assert!(
    raw.contains("x-forwarded-for: 198.51.100.9"),
    "the endpoint is told the address the server decided on: {raw}"
  );
  assert!(
    !raw.contains("10.0.0.5"),
    "and never the one the visitor wrote for themselves: {raw}"
  );
}

#[tokio::test]
async fn a_jwt_gate_admits_the_token_a_visitor_already_holds() {
  // No round trip per request, which is what separates this from `forward`,
  // and the identity comes out of the token rather than out of a login.
  let secret = "0123456789abcdef-jwt-gate-secret";
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str(&format!(
      "{{method: jwt, hmac_secret: \"{secret}\", issuer: \"https://accounts.example.com\"}}"
    ))
    .unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/private".parse().unwrap();

  let exp = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_secs()
    + 600;
  let good = jsonwebtoken::encode(
    &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
    &serde_json::json!({
      "sub": "u-1", "email": "alice@example.com",
      "iss": "https://accounts.example.com", "exp": exp
    }),
    &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
  )
  .unwrap();

  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    HeaderValue::from_str(&format!("Bearer {good}")).unwrap(),
  );
  match check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await {
    VisitorGate::Allow(Some(id)) => {
      assert_eq!(id.how, "jwt");
      assert_eq!(id.who.as_deref(), Some("alice@example.com"));
      assert!(
        id.consumed_authorization,
        "the header carried Aperio's credential, so it does not travel on"
      );
    }
    _ => panic!("expected the token to be admitted"),
  }

  // A token for another issuer is refused, with the challenge a caller that
  // speaks in headers can act on.
  let wrong = jsonwebtoken::encode(
    &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
    &serde_json::json!({"sub": "u-1", "iss": "https://somewhere.else", "exp": exp}),
    &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
  )
  .unwrap();
  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    HeaderValue::from_str(&format!("Bearer {wrong}")).unwrap(),
  );
  match check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await {
    VisitorGate::Deny(resp) => {
      assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
      assert_eq!(resp.headers().get("WWW-Authenticate").unwrap(), "Bearer");
    }
    VisitorGate::Allow(_) => panic!("expected deny"),
    VisitorGate::Undeclared(_) => panic!("expected a deny, not an undeclared route"),
  }
}

#[tokio::test]
async fn a_jwt_in_a_cookie_is_the_visitors_own_and_keeps_travelling() {
  // Where an identity-aware proxy in front puts it. Stripping the cookie
  // header would take the application's own session with it, so unlike the
  // bearer case nothing is consumed.
  let secret = "0123456789abcdef-jwt-cookie-key";
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str(&format!(
      "{{method: jwt, hmac_secret: \"{secret}\", cookie: CF_Authorization}}"
    ))
    .unwrap(),
  );
  let state = Arc::new(test_state_with(cfg));
  let exp = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_secs()
    + 600;
  let t = jsonwebtoken::encode(
    &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
    &serde_json::json!({"sub": "u-9", "exp": exp}),
    &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
  )
  .unwrap();

  let mut headers = HeaderMap::new();
  headers.insert(
    "cookie",
    HeaderValue::from_str(&format!("other=1; CF_Authorization={t}")).unwrap(),
  );
  let uri: axum::http::Uri = "/private".parse().unwrap();
  match check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await {
    VisitorGate::Allow(Some(id)) => {
      assert_eq!(id.how, "jwt");
      assert_eq!(id.who.as_deref(), Some("u-9"));
      assert!(!id.consumed_authorization);
    }
    _ => panic!("expected the cookie token to be admitted"),
  }
}

#[tokio::test]
async fn one_written_policy_behaves_the_same_whichever_side_wrote_it() {
  // The two branches of the gate, a client-declared policy and the server's
  // own, ran the same helpers in two hand-written sequences, and they had
  // drifted: a `bearer` with `query: true` got the clean-address redirect on
  // the server side and a bare admission on the client side, so a page loaded
  // through a client-declared gate rendered and then failed to fetch a single
  // one of its own assets.
  let yaml = "{method: bearer, secret: \"0123456789abcdef-secret\", query: true}";
  let uri: axum::http::Uri = "/report?aperio_token=0123456789abcdef-secret&page=2"
    .parse()
    .unwrap();
  let mut browser = HeaderMap::new();
  browser.insert("accept", HeaderValue::from_static("text/html"));

  // Written on the server.
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::compile(&serde_yaml::from_str(yaml).unwrap());
  let server_side = Arc::new(test_state_with(cfg));

  // Written on a client that serves the route.
  let client_side = Arc::new(test_state_with(test_config()));
  let mut c = mock_client(None, None, None, None);
  c.sole_mut().visitor_auth_policy = Some(crate::visitor_auth::Policy::compile(
    &serde_yaml::from_str(yaml).unwrap(),
  ));
  client_side
    .clients
    .write()
    .await
    .insert("c1".to_string(), c);

  for (label, state) in [("server", &server_side), ("client", &client_side)] {
    let gate = check_visitor_gate(
      state,
      &axum::http::Method::GET,
      &browser,
      &uri,
      Some("app.example.com"),
    )
    .await;
    match gate {
      VisitorGate::Deny(resp) => {
        assert_eq!(resp.status(), StatusCode::FOUND, "{label}");
        assert_eq!(
          resp.headers().get("Location").unwrap(),
          "/report?page=2",
          "{label}"
        );
        assert!(
          resp
            .headers()
            .get("Set-Cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("aperio_share="),
          "{label}"
        );
      }
      VisitorGate::Allow(_) | VisitorGate::Undeclared(_) => {
        panic!("{label}: a page load carrying the secret should be sent to a clean address")
      }
    }
  }
}

#[test]
pub(crate) fn login_redirect_preserves_path() {
  let resp = login_redirect("/aperio/auth", "/secret?x=1");
  assert_eq!(resp.status(), StatusCode::FOUND);
  let loc = resp.headers().get("Location").unwrap().to_str().unwrap();
  assert!(loc.starts_with("/aperio/auth?redirect="));
}

#[tokio::test]
pub(crate) async fn visitor_gate_traversal_allowed_without_gate() {
  // No server auth configured and no per-route gate → a traversal path is
  // allowed straight through.
  let state = Arc::new(test_state_with(test_config()));
  let uri: axum::http::Uri = "/a/../b".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Allow(_)));
}

#[tokio::test]
pub(crate) async fn visitor_gate_traversal_sees_a_policy_the_scalar_cannot_hold() {
  // The traversal branch is the *entire* gate for such a path, and it asks one
  // question: does anything on this host declare a gate. It used to ask only
  // about the scalar `visitor_auth`, so a `bearer`, a `jwt`, or a `basic`
  // naming two users read as ungated and `/./admin` was served with no
  // credential while `/admin` answered 401.
  for spelling in [
    "{method: bearer, secret: a-long-bearer-secret}",
    "{method: basic, users: [\"alice:one\", \"bob:two\"]}",
  ] {
    let setting =
      serde_yaml::from_str::<aperio_config::AuthSetting>(spelling).expect("a valid auth: value");
    let policy = crate::visitor_auth::Policy::compile(&setting);
    assert!(policy.gates(), "{spelling} is a gate");

    let state = Arc::new(test_state_with(test_config()));
    let mut c = mock_client(None, None, None, None);
    c.sole_mut().visitor_auth = None; // exactly the shape the bug turned on
    c.sole_mut().visitor_auth_policy = Some(policy);
    state.clients.write().await.insert("c1".to_string(), c);

    for path in ["/./admin", "/x/../admin", "/."] {
      let uri: axum::http::Uri = path.parse().unwrap();
      let gate = check_visitor_gate(
        &state,
        &axum::http::Method::GET,
        &HeaderMap::new(),
        &uri,
        None,
      )
      .await;
      assert!(
        matches!(gate, VisitorGate::Deny(_)),
        "{path} under `{spelling}` must not be served without a credential"
      );
    }
  }
}

#[tokio::test]
pub(crate) async fn a_query_token_cookie_is_scoped_to_the_route_that_admitted_it() {
  // The cookie a `?aperio_token=` page load mints is read by *every* branch of
  // the gate, including the server's own. Minted host-wide from a per-route
  // secret it outranked the policy that produced it: the holder of a secret
  // for `/metrics` got the whole hostname for an hour, including routes gated
  // by the operator's own password.
  let mut cfg = test_config();
  // The server's own gate covers everything this client does not.
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("admin:server-password");
  let state = Arc::new(test_state_with(cfg));

  let setting = serde_yaml::from_str::<aperio_config::AuthSetting>(
    "{method: bearer, secret: a-long-route-secret, query: true}",
  )
  .expect("a valid auth: value");
  let mut c = mock_client(Some("app.e2e.local"), Some("/metrics"), None, None);
  c.sole_mut().visitor_auth_policy = Some(crate::visitor_auth::Policy::compile(&setting));
  state.clients.write().await.insert("c1".to_string(), c);

  let mut headers = HeaderMap::new();
  headers.insert("host", "app.e2e.local".parse().unwrap());
  headers.insert("accept", "text/html".parse().unwrap());
  let uri: axum::http::Uri = "/metrics?aperio_token=a-long-route-secret".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &headers,
    &uri,
    Some("app.e2e.local"),
  )
  .await;

  // A navigation carrying the secret is redirected, with the cookie set.
  let VisitorGate::Deny(resp) = gate else {
    panic!("a navigation with the secret should be redirected to a clean address");
  };
  let cookie = resp
    .headers()
    .get("set-cookie")
    .and_then(|v| v.to_str().ok())
    .expect("a share cookie")
    .to_string();
  let token = cookie
    .split(';')
    .next()
    .and_then(|kv| kv.split_once('='))
    .map(|(_, v)| v.to_string())
    .expect("a cookie value");

  // The scope it carries is the route's bind, not the whole host.
  let claims = crate::share::verify_share_token(
    &token,
    &crate::share::share_signing_key(&state.config().token),
  )
  .expect("a valid share token");
  assert_eq!(
    claims.path.as_deref(),
    Some("/metrics"),
    "the cookie must not outrank the secret that minted it"
  );

  // And it does not open the route the server's own password gates.
  let mut with_cookie = HeaderMap::new();
  with_cookie.insert("host", "app.e2e.local".parse().unwrap());
  with_cookie.insert("cookie", format!("aperio_share={token}").parse().unwrap());
  let elsewhere: axum::http::Uri = "/".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &with_cookie,
    &elsewhere,
    Some("app.e2e.local"),
  )
  .await;
  assert!(
    matches!(gate, VisitorGate::Deny(_)),
    "a cookie minted for /metrics must not open /"
  );
}

#[tokio::test]
pub(crate) async fn visitor_gate_traversal_honors_the_closed_posture() {
  // `deny` is checked in section 2, and a traversal path returns before it,
  // so a `.` in the path was the one way to switch the posture off.
  let mut cfg = test_config();
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/a/../b".parse().unwrap();
  let gate = check_visitor_gate(
    &state,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &uri,
    None,
  )
  .await;
  assert!(matches!(gate, VisitorGate::Deny(_)));
}

/// Aperio's own headers are stripped, and the Authorization strip is
/// conditional on Aperio having consumed it.
///
/// `consumed_authorization && name == "authorization"` becoming `||` survived.
/// That mutant strips *every* header once Aperio has consumed the
/// Authorization one, so a backend behind a gated route stops receiving any
/// header at all, and it strips `authorization` even when the visitor's own
/// credential was meant to be forwarded. Both directions are wrong and
/// neither had a test.
#[test]
fn what_counts_as_aperios_own_header() {
  let carried = vec!["x-forwarded-user".to_string()];

  // The namespace, always, whatever the switches say.
  assert!(header_is_aperios("x-aperio-service", &carried, false));
  assert!(header_is_aperios("X-Aperio-Visitor-How", &carried, false));

  // A name an endpoint delivers an identity under.
  assert!(header_is_aperios("x-forwarded-user", &carried, false));

  // Authorization only when Aperio consumed it. This is the pair the `&&`
  // holds together: neither half alone is the rule.
  assert!(header_is_aperios("authorization", &carried, true));
  assert!(
    !header_is_aperios("authorization", &carried, false),
    "a credential Aperio did not consume belongs to the backend"
  );

  // Everything else travels, and it must keep travelling once Aperio has
  // consumed an Authorization header.
  assert!(!header_is_aperios("accept", &carried, false));
  assert!(
    !header_is_aperios("accept", &carried, true),
    "consuming Authorization must not strip every other header too"
  );
  assert!(!header_is_aperios("content-type", &carried, true));
}

/// The namespace is stripped including its degenerate member, a header named
/// exactly `x-aperio-`.
///
/// `name.len() > 9` mutated to `>=` survived, and the mutant was the better
/// spelling: `"x-aperio-"` is nine characters, so under `>` a header with that
/// exact name is not recognized as Aperio's and travels to the backend, while
/// every name in the namespace with anything after the dash is stripped. It is
/// a narrow hole, nothing the server itself sends is named that, but the rule
/// this function exists to enforce is that the namespace is not forgeable, and
/// a rule with one name exempted is worth more as a rule without one. The
/// comparison is `>=` now and this is what says so.
#[test]
fn the_bare_namespace_prefix_is_aperios_too() {
  let carried: Vec<String> = vec![];
  assert!(
    header_is_aperios("x-aperio-", &carried, false),
    "the prefix on its own is in the namespace it names"
  );
  assert!(
    header_is_aperios("X-APERIO-", &carried, false),
    "and the comparison stays case-insensitive at the boundary"
  );
  // One character short is a different header and keeps travelling.
  assert!(
    !header_is_aperios("x-aperio", &carried, false),
    "the prefix is `x-aperio-` with the dash; without it this is somebody \
     else's header and stripping it would be the opposite mistake"
  );
}

/// Under the closed posture an Aperio session reaches an undeclared route,
/// and a stranger still does not.
///
/// `auth:` on a service is the door for third parties, people holding no
/// Aperio credential. Its absence says nothing about whether this server's own
/// users may look, and the check that admits them already existed: it sits
/// two lines below, and every request with a server password or OIDC
/// configured has always gone through it. Closed-by-default returned before
/// reaching it, so one identity meant two different things depending on
/// whether a server password happened to be set, and the operator holding the
/// master token got the same opaque 504 as a stranger on their own route.
#[tokio::test]
async fn an_aperio_session_reaches_an_undeclared_route_and_a_stranger_does_not() {
  let mut cfg = test_config();
  cfg.default_access = crate::settings::DefaultAccess::Deny;
  let state = Arc::new(test_state_with(cfg));
  let uri: axum::http::Uri = "/anything".parse().unwrap();
  let call = async |headers: HeaderMap| {
    check_visitor_gate(&state, &axum::http::Method::GET, &headers, &uri, None).await
  };

  match call(admin_headers(&state).await).await {
    VisitorGate::Allow(_) => {}
    VisitorGate::Undeclared(_) => panic!("a signed-in Aperio user is not a stranger"),
    VisitorGate::Deny(_) => panic!("a signed-in Aperio user is not a stranger"),
  }

  // The posture is unchanged for everyone else, which is the half that must
  // not move: no session, and a cookie that is not a session.
  for (what, headers) in [
    ("no session at all", HeaderMap::new()),
    (
      "a cookie that is not a session",
      cookie_headers("not-a-real-session"),
    ),
  ] {
    match call(headers).await {
      VisitorGate::Undeclared(resp) => assert_eq!(
        resp.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "{what} still gets the unclaimed-hostname answer"
      ),
      _ => panic!("{what} must not reach the route"),
    }
  }
}
