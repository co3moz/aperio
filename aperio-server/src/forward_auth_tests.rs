//! The five decisions this method is made of, each pinned by a test: what
//! crosses in each direction, that a timeout refuses, that a verdict is
//! remembered, that the destination is fenced, and that a refusal is the
//! endpoint's own answer rather than a generic one.

use super::*;
use crate::test_support::{test_config, test_state, test_state_with};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One request the stand-in endpoint saw: its request line, and its headers
/// lowercased.
type SeenRequest = (String, Vec<(String, String)>);

/// A stand-in endpoint. Every request it sees is recorded so a test can
/// assert on what crossed.
struct Endpoint {
  url: String,
  seen: Arc<std::sync::Mutex<Vec<SeenRequest>>>,
  calls: Arc<AtomicUsize>,
}

/// Starts an endpoint that answers `status` with a `content-length` it never
/// satisfies, which is what a broken or hostile one looks like.
async fn endpoint_declaring(status: u16, declared: usize) -> Endpoint {
  endpoint_promising(status, declared).await
}

/// Starts an endpoint that answers `status` with `headers` and `body_bytes`
/// bytes of body.
async fn endpoint_with_body(
  status: u16,
  headers: Vec<(&'static str, &'static str)>,
  body_bytes: usize,
) -> Endpoint {
  endpoint_inner(status, headers, body_bytes).await
}

/// Starts an endpoint that answers `status` with `headers`.
async fn endpoint(status: u16, headers: Vec<(&'static str, &'static str)>) -> Endpoint {
  endpoint_inner(status, headers, 0).await
}

async fn endpoint_promising(status: u16, declared: usize) -> Endpoint {
  endpoint_build(status, Vec::new(), 0, Some(declared)).await
}

async fn endpoint_inner(
  status: u16,
  headers: Vec<(&'static str, &'static str)>,
  body_bytes: usize,
) -> Endpoint {
  endpoint_build(status, headers, body_bytes, None).await
}

async fn endpoint_build(
  status: u16,
  headers: Vec<(&'static str, &'static str)>,
  body_bytes: usize,
  promise_only: Option<usize>,
) -> Endpoint {
  let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
  let calls = Arc::new(AtomicUsize::new(0));
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let (seen_task, calls_task) = (seen.clone(), calls.clone());
  tokio::spawn(async move {
    loop {
      let Ok((mut sock, _)) = listener.accept().await else {
        return;
      };
      let seen = seen_task.clone();
      let calls = calls_task.clone();
      let headers = headers.clone();
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let raw = String::from_utf8_lossy(&buf[..n]).to_string();
        let mut lines = raw.lines();
        let request_line = lines.next().unwrap_or_default().to_string();
        let got: Vec<(String, String)> = lines
          .take_while(|l| !l.is_empty())
          .filter_map(|l| l.split_once(':'))
          .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
          .collect();
        seen.lock().unwrap().push((request_line, got));
        calls.fetch_add(1, Ordering::SeqCst);
        let mut out = format!("HTTP/1.1 {status} X\r\n");
        for (k, v) in &headers {
          out.push_str(&format!("{k}: {v}\r\n"));
        }
        let declared = promise_only.unwrap_or(body_bytes);
        out.push_str(&format!("content-length: {declared}\r\n\r\n"));
        if promise_only.is_none() {
          out.push_str(&"x".repeat(body_bytes));
        }
        let _ = sock.write_all(out.as_bytes()).await;
        if promise_only.is_some() {
          // Never sends what it promised, and holds the socket so the reader
          // is waiting on it rather than on a closed connection.
          tokio::time::sleep(Duration::from_secs(30)).await;
        }
      });
    }
  });
  Endpoint {
    url: format!("http://127.0.0.1:{port}/authcheck"),
    seen,
    calls,
  }
}

fn config(url: &str) -> ForwardConfig {
  ForwardConfig {
    url: url.to_string(),
    request_headers: Vec::new(),
    response_headers: Vec::new(),
    timeout: Duration::from_secs(5),
    cache: Duration::ZERO,
  }
}

fn request_headers(pairs: &[(&str, &str)]) -> HeaderMap {
  let mut h = HeaderMap::new();
  for (k, v) in pairs {
    h.insert(
      axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
      axum::http::HeaderValue::from_str(v).unwrap(),
    );
  }
  h
}

#[tokio::test]
async fn what_crosses_to_the_endpoint_is_the_request_described_not_replayed() {
  let ep = endpoint(200, vec![]).await;
  let state = test_state();
  let headers = request_headers(&[
    ("cookie", "session=abc"),
    ("authorization", "Bearer t"),
    ("x-secret-of-the-app", "should-not-cross"),
  ]);
  let uri: axum::http::Uri = "/reports?page=2".parse().unwrap();
  let verdict = ask(
    &state,
    &config(&ep.url),
    &axum::http::Method::POST,
    &headers,
    &uri,
    Some("app.example.com"),
    Some("203.0.113.7"),
  )
  .await;
  assert!(matches!(verdict, Verdict::Allow(_)));

  let seen = ep.seen.lock().unwrap();
  let (line, got) = seen.first().expect("the endpoint was asked");
  // A question about a request, not the request itself.
  assert!(line.starts_with("GET /authcheck"), "{line}");
  let find = |name: &str| got.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
  assert_eq!(find("x-forwarded-method"), Some("POST"));
  assert_eq!(find("x-forwarded-host"), Some("app.example.com"));
  assert_eq!(find("x-forwarded-uri"), Some("/reports?page=2"));
  assert_eq!(find("x-forwarded-for"), Some("203.0.113.7"));
  // The default list is the two headers that carry an identity, and nothing
  // else: handing the endpoint the whole request would make every header it
  // happens to read part of a contract nobody wrote down.
  assert_eq!(find("cookie"), Some("session=abc"));
  assert_eq!(find("authorization"), Some("Bearer t"));
  assert_eq!(find("x-secret-of-the-app"), None);
}

#[tokio::test]
async fn only_the_named_response_headers_come_back() {
  // This list is how the pattern delivers an identity, and an open one is how
  // it becomes a header injection, so nothing crosses unless it is named.
  let ep = endpoint(
    200,
    vec![("x-auth-user", "alice"), ("x-not-asked-for", "surprise")],
  )
  .await;
  let state = test_state();
  let mut cfg = config(&ep.url);
  cfg.response_headers = vec!["x-auth-user".to_string()];
  match ask(
    &state,
    &cfg,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &"/".parse().unwrap(),
    None,
    None,
  )
  .await
  {
    Verdict::Allow(carried) => {
      assert_eq!(
        carried,
        vec![("x-auth-user".to_string(), "alice".to_string())]
      );
    }
    Verdict::Deny(_) => panic!("expected allow"),
  }
}

#[tokio::test]
async fn a_refusal_is_the_endpoints_own_answer() {
  // So it can send the visitor to a login of its own. Flattening this into a
  // 401 would discard the only part of the method its author wrote.
  let ep = endpoint(302, vec![("location", "https://sso.example.com/start")]).await;
  let state = test_state();
  match ask(
    &state,
    &config(&ep.url),
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &"/".parse().unwrap(),
    None,
    None,
  )
  .await
  {
    Verdict::Deny(resp) => {
      assert_eq!(resp.status(), StatusCode::FOUND);
      assert_eq!(
        resp.headers().get("location").unwrap(),
        "https://sso.example.com/start"
      );
    }
    Verdict::Allow(_) => panic!("expected deny"),
  }
}

#[tokio::test]
async fn an_endpoint_that_cannot_be_reached_refuses_the_request() {
  // An auth gate that opens when its check is unreachable is not a gate. The
  // endpoint's availability becomes the route's, and that is the trade.
  let state = test_state();
  let mut cfg = config("http://127.0.0.1:1/authcheck");
  cfg.timeout = Duration::from_millis(300);
  match ask(
    &state,
    &cfg,
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &"/".parse().unwrap(),
    None,
    None,
  )
  .await
  {
    Verdict::Deny(resp) => assert_eq!(resp.status(), StatusCode::FORBIDDEN),
    Verdict::Allow(_) => panic!("an unreachable endpoint must not admit anyone"),
  }
}

#[tokio::test]
async fn a_verdict_is_remembered_for_the_credential_that_produced_it() {
  // Without this the method is a round trip per request, which is unusable on
  // anything busy.
  let ep = endpoint(200, vec![("x-auth-user", "alice")]).await;
  let state = test_state();
  let mut cfg = config(&ep.url);
  cfg.cache = Duration::from_secs(60);
  cfg.response_headers = vec!["x-auth-user".to_string()];
  let headers = request_headers(&[("cookie", "session=abc")]);

  for _ in 0..3 {
    match ask(
      &state,
      &cfg,
      &axum::http::Method::GET,
      &headers,
      &"/".parse().unwrap(),
      Some("app.example.com"),
      None,
    )
    .await
    {
      Verdict::Allow(carried) => assert_eq!(carried.len(), 1, "the answer is remembered whole"),
      Verdict::Deny(_) => panic!("expected allow"),
    }
  }
  assert_eq!(ep.calls.load(Ordering::SeqCst), 1, "asked once");

  // A different credential is a different question.
  let other = request_headers(&[("cookie", "session=zzz")]);
  let _ = ask(
    &state,
    &cfg,
    &axum::http::Method::GET,
    &other,
    &"/".parse().unwrap(),
    Some("app.example.com"),
    None,
  )
  .await;
  assert_eq!(ep.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_refusal_is_never_remembered() {
  // Somebody who has just been given access must not keep being turned away
  // for the rest of the window.
  let ep = endpoint(403, vec![]).await;
  let state = test_state();
  let mut cfg = config(&ep.url);
  cfg.cache = Duration::from_secs(60);
  for _ in 0..2 {
    let _ = ask(
      &state,
      &cfg,
      &axum::http::Method::GET,
      &HeaderMap::new(),
      &"/".parse().unwrap(),
      None,
      None,
    )
    .await;
  }
  assert_eq!(ep.calls.load(Ordering::SeqCst), 2, "asked every time");
  assert!(state.forward_auth_cache.lock().await.is_empty());
}

#[tokio::test]
async fn the_endpoint_goes_through_the_outbound_policy() {
  // A URL from a configuration file that makes the server issue a request is
  // exactly the shape the policy exists to fence.
  let ep = endpoint(200, vec![]).await;
  let mut cfg = test_config();
  cfg.outbound_policy = crate::outbound::OutboundPolicy {
    allowlist: Vec::new(),
    block_private: true,
  };
  let state = test_state_with(cfg);
  match ask(
    &state,
    &config(&ep.url),
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &"/".parse().unwrap(),
    None,
    None,
  )
  .await
  {
    Verdict::Deny(resp) => assert_eq!(resp.status(), StatusCode::FORBIDDEN),
    Verdict::Allow(_) => panic!("expected the policy to refuse the destination"),
  }
  assert_eq!(ep.calls.load(Ordering::SeqCst), 0, "never asked");
}

#[tokio::test]
async fn an_answer_inside_the_limit_still_arrives_whole() {
  // The guard has to be a limit, not a wall: the ordinary small body an
  // endpoint sends to explain itself must still reach the visitor.
  let ep = endpoint_with_body(403, vec![("content-type", "text/plain")], 64).await;
  let state = test_state();
  match ask(
    &state,
    &config(&ep.url),
    &axum::http::Method::GET,
    &HeaderMap::new(),
    &"/".parse().unwrap(),
    None,
    None,
  )
  .await
  {
    Verdict::Deny(resp) => {
      let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
      assert_eq!(body.len(), 64);
    }
    Verdict::Allow(_) => panic!("expected deny"),
  }
}

#[tokio::test]
async fn every_subrequest_goes_through_one_shared_client() {
  // A `reqwest::Client` owns a connection pool, so one per request is a fresh
  // TCP and TLS handshake per visitor request on a gated route.
  let first = client().expect("a client");
  let second = client().expect("the same client");
  assert!(std::ptr::eq(first, second));
}

// --- read_bounded ------------------------------------------------------------
//
// Tested here rather than through `ask`, because the property it exists for,
// that nothing past the limit is ever held, is not visible from outside: a
// caller sees the same refusal whether the limit was applied while reading or
// after. Measured, in fact, and the two are within a millisecond of each other.
// So the contract is pinned where it lives, and the reason lives on the
// constant.

/// Fetches from a stand-in endpoint and hands the response to `read_bounded`.
async fn bounded(ep: &Endpoint, limit: usize) -> Option<String> {
  let res = client().unwrap().get(&ep.url).send().await.unwrap();
  crate::outbound::read_bounded(res, limit).await
}

#[tokio::test]
async fn a_body_within_the_limit_comes_back_whole() {
  let ep = endpoint_with_body(200, vec![], 128).await;
  assert_eq!(bounded(&ep, 256).await.map(|b| b.len()), Some(128));
}

#[tokio::test]
async fn a_body_over_the_limit_is_refused_rather_than_truncated() {
  // Not a prefix: half an answer is not an answer, and returning one would
  // hand the caller something that parses and means something else.
  let ep = endpoint_with_body(200, vec![], 512).await;
  assert_eq!(bounded(&ep, 256).await, None);
}

#[tokio::test]
async fn a_length_declared_over_the_limit_is_refused_without_reading() {
  // The cheap refusal: a body that says how big it is and is too big never
  // costs a chunk.
  let ep = endpoint_declaring(200, 1024).await;
  assert_eq!(bounded(&ep, 256).await, None);
}

/// An endpoint that admits only requests carrying `admits` in the lowercased
/// text of the subrequest, so a cached verdict reused for a different question
/// is visible as an admission the endpoint never gave.
async fn endpoint_admitting(admits: &'static [&'static str]) -> Endpoint {
  let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
  let calls = Arc::new(AtomicUsize::new(0));
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let (seen_task, calls_task) = (seen.clone(), calls.clone());
  tokio::spawn(async move {
    loop {
      let Ok((mut sock, _)) = listener.accept().await else {
        return;
      };
      let (seen, calls) = (seen_task.clone(), calls_task.clone());
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let raw = String::from_utf8_lossy(&buf[..n]).to_string();
        let lower = raw.to_ascii_lowercase();
        seen.lock().unwrap().push((raw.clone(), Vec::new()));
        calls.fetch_add(1, Ordering::SeqCst);
        let ok = admits.iter().all(|needle| lower.contains(needle));
        let status = if ok { 200 } else { 403 };
        let out = format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        let _ = sock.write_all(out.as_bytes()).await;
      });
    }
  });
  Endpoint {
    url: format!("http://127.0.0.1:{port}/authcheck"),
    seen,
    calls,
  }
}

#[tokio::test]
async fn a_remembered_verdict_is_not_reused_for_a_different_address() {
  // The address is sent to the endpoint as `X-Forwarded-For`, and allowlisting
  // source addresses is the commonest thing this method fronts. A key that
  // leaves it out remembers one address's admission and hands it to every
  // other address asking the same question, with the endpoint never asked.
  let ep = endpoint_admitting(&["x-forwarded-for: 203.0.113.7"]).await;
  let state = test_state();
  let mut cfg = config(&ep.url);
  cfg.cache = Duration::from_secs(60);
  let ask_from = async |ip: Option<&str>| {
    ask(
      &state,
      &cfg,
      &axum::http::Method::GET,
      &HeaderMap::new(),
      &"/reports".parse().unwrap(),
      Some("app.example.com"),
      ip,
    )
    .await
  };

  assert!(
    matches!(ask_from(Some("203.0.113.7")).await, Verdict::Allow(_)),
    "the endpoint admits this address"
  );
  assert!(
    matches!(ask_from(Some("198.51.100.9")).await, Verdict::Deny(_)),
    "a yes for one address must not admit another"
  );
  assert!(
    matches!(ask_from(None).await, Verdict::Deny(_)),
    "nor must it admit a request carrying no address at all"
  );
  assert_eq!(
    ep.calls.load(Ordering::SeqCst),
    3,
    "each distinct address is its own question"
  );
}

/// An endpoint that admits only what it is asked about, so a cached verdict
/// reused for a different request is visible.
async fn per_request_endpoint() -> Endpoint {
  let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
  let calls = Arc::new(AtomicUsize::new(0));
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let (seen_task, calls_task) = (seen.clone(), calls.clone());
  tokio::spawn(async move {
    loop {
      let Ok((mut sock, _)) = listener.accept().await else {
        return;
      };
      let (seen, calls) = (seen_task.clone(), calls_task.clone());
      tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let raw = String::from_utf8_lossy(&buf[..n]).to_string();
        let lower = raw.to_ascii_lowercase();
        seen.lock().unwrap().push((raw.clone(), Vec::new()));
        calls.fetch_add(1, Ordering::SeqCst);
        // Admits `/allowed`, refuses everything else, and only for GET.
        let ok =
          lower.contains("x-forwarded-uri: /allowed") && lower.contains("x-forwarded-method: get");
        let status = if ok { 200 } else { 403 };
        let out = format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        let _ = sock.write_all(out.as_bytes()).await;
      });
    }
  });
  Endpoint {
    url: format!("http://127.0.0.1:{port}/authcheck"),
    seen,
    calls,
  }
}

#[tokio::test]
async fn a_remembered_verdict_is_not_reused_for_a_different_request() {
  // The endpoint is told the method and the path and may answer on them,
  // which is the ordinary way this pattern is used. A cache keyed on the
  // credential alone therefore carries one path's "yes" to every other path
  // for the length of its window.
  let ep = per_request_endpoint().await;
  let state = test_state();
  let mut cfg = config(&ep.url);
  cfg.cache = Duration::from_secs(60);
  let headers = request_headers(&[("cookie", "session=abc")]);
  let ask_for = async |uri: &str, method: axum::http::Method| {
    ask(
      &state,
      &cfg,
      &method,
      &headers,
      &uri.parse().unwrap(),
      Some("app.example.com"),
      None,
    )
    .await
  };

  assert!(
    matches!(
      ask_for("/allowed", axum::http::Method::GET).await,
      Verdict::Allow(_)
    ),
    "the endpoint admits this one"
  );
  assert!(
    matches!(
      ask_for("/admin", axum::http::Method::GET).await,
      Verdict::Deny(_)
    ),
    "a yes for /allowed must not admit /admin"
  );
  assert!(
    matches!(
      ask_for("/allowed", axum::http::Method::POST).await,
      Verdict::Deny(_)
    ),
    "a yes for GET must not admit POST"
  );
}
