//! Where a request goes and what it carries: header rules both ways, path-bind
//! trimming at segment boundaries, the absolute-form URI, and which redirects
//! are followed rather than handed back to the visitor.

use super::super::http_tests::*;
use super::*;

#[test]
fn test_header_transform_apply() {
  // No rules = identity (fast path).
  let noop = HeaderTransform::default();
  let headers = vec![("x-a".to_string(), "1".to_string())];
  assert_eq!(noop.apply(headers.clone()), headers);

  let directives = aperio_config::HeaderDirectives {
    add: [("X-Env".to_string(), "staging".to_string())]
      .into_iter()
      .collect(),
    remove: vec!["Server".to_string()],
  };
  let t = HeaderTransform::compile(Some(&directives));
  let out = t.apply(vec![
    ("server".to_string(), "nginx".to_string()), // removed (case-insensitive)
    ("x-env".to_string(), "prod".to_string()),   // replaced by the add
    ("x-keep".to_string(), "yes".to_string()),
  ]);
  assert_eq!(
    out,
    vec![
      ("x-keep".to_string(), "yes".to_string()),
      ("X-Env".to_string(), "staging".to_string()),
    ]
  );
}

#[test]
fn test_same_site() {
  // Exact and case/dot-insensitive matches.
  assert!(same_site("example.com", "example.com"));
  assert!(same_site("Example.COM.", "example.com"));
  // Same root domain: parent↔child and siblings.
  assert!(same_site("example.com", "test.example.com"));
  assert!(same_site("a.example.com", "b.example.com"));
  assert!(same_site("x.y.example.com", "example.com"));
  // Different domains never match.
  assert!(!same_site("example.com", "evil.com"));
  assert!(!same_site("example.com", "example.org"));
  // IPs and single-label hosts only match exactly.
  assert!(same_site("127.0.0.1", "127.0.0.1"));
  assert!(!same_site("127.0.0.1", "127.0.0.2"));
  assert!(same_site("localhost", "localhost"));
  assert!(!same_site("localhost", "example.com"));
}

#[tokio::test]
async fn test_redirects_followed_same_host() {
  // Target redirects to a second local server on the same host (127.0.0.1);
  // the client must follow it transparently and return the final 200.
  let final_port = mock_server(
    "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfinal".to_string(),
  )
  .await;
  let first_port = mock_server(format!(
    "HTTP/1.1 301 Moved Permanently\r\nLocation: http://127.0.0.1:{}/moved\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    final_port
  ))
  .await;

  let ctx = ForwardContext {
    client: reqwest::Client::builder()
      .redirect(redirect_policy(5))
      .build()
      .unwrap(),
    ..test_ctx(
      &format!("http://127.0.0.1:{}", first_port),
      test_tunnel_tx(),
    )
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-redir-1".to_string(),
      method: "GET".to_string(),
      uri: "/".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered response");

  if let TunnelMessage::Response { status, body, .. } = result {
    assert_eq!(status, 200);
    let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
    assert_eq!(String::from_utf8(decoded).unwrap(), "final");
  } else {
    panic!("Expected response variant");
  }
}

#[tokio::test]
async fn test_redirects_passed_through_cross_site() {
  // A redirect to an unrelated domain must NOT be followed: the 301 goes
  // back through the tunnel untouched.
  let first_port = mock_server(
    "HTTP/1.1 301 Moved Permanently\r\nLocation: http://unrelated.invalid/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
      .to_string(),
  )
  .await;

  let ctx = ForwardContext {
    client: reqwest::Client::builder()
      .redirect(redirect_policy(5))
      .build()
      .unwrap(),
    ..test_ctx(
      &format!("http://127.0.0.1:{}", first_port),
      test_tunnel_tx(),
    )
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-redir-2".to_string(),
      method: "GET".to_string(),
      uri: "/".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered response");

  if let TunnelMessage::Response {
    status, headers, ..
  } = result
  {
    assert_eq!(status, 301);
    let loc = headers
      .iter()
      .find(|(k, _)| k == "location")
      .map(|(_, v)| v.as_str());
    assert_eq!(loc, Some("http://unrelated.invalid/"));
  } else {
    panic!("Expected response variant");
  }
}

#[tokio::test]
async fn test_pass_hostname_sends_exactly_one_host_header() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);

  tokio::spawn(async move {
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0; 2048];
      let n = socket.read(&mut buf).await.unwrap();
      let req_str = String::from_utf8_lossy(&buf[..n]).to_lowercase();
      // The visitor's Host must be forwarded exactly once (a duplicate is a
      // protocol violation that strict backends reject with 400).
      let host_lines = req_str.matches("\r\nhost:").count();
      assert_eq!(
        host_lines, 1,
        "expected exactly one host header, got: {req_str}"
      );
      assert!(
        req_str.contains("host: app.example.com"),
        "visitor host must be passed through, got: {req_str}"
      );

      let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
      socket.write_all(response.as_bytes()).await.unwrap();
    }
  });

  let mut ctx = test_ctx(&target_url, test_tunnel_tx());
  ctx.pass_hostname = true;

  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-host".to_string(),
      method: "GET".to_string(),
      uri: "/".to_string(),
      headers: vec![("host".to_string(), "app.example.com".to_string())],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered response");

  let TunnelMessage::Response { status, .. } = result else {
    panic!("Expected response variant");
  };
  // The mock asserts inside its task; a 200 here means the read succeeded
  // (an assert failure in the task would leave the connection unanswered).
  assert_eq!(status, 200);
}

#[tokio::test]
async fn test_handle_incoming_request_header_rules() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);

  tokio::spawn(async move {
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0; 2048];
      let n = socket.read(&mut buf).await.unwrap();
      let req_str = String::from_utf8_lossy(&buf[..n]).to_lowercase();
      // The request rules injected a header and stripped another.
      assert!(req_str.contains("x-env: staging"), "got: {req_str}");
      assert!(!req_str.contains("x-secret"), "got: {req_str}");

      let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nServer: mock\r\nX-Old: 1\r\n\r\nok";
      socket.write_all(response.as_bytes()).await.unwrap();
    }
  });

  let mut ctx = test_ctx(&target_url, test_tunnel_tx());
  ctx.request_headers = HeaderTransform::compile(Some(&aperio_config::HeaderDirectives {
    add: [("X-Env".to_string(), "staging".to_string())]
      .into_iter()
      .collect(),
    remove: vec!["X-Secret".to_string()],
  }));
  ctx.response_headers = HeaderTransform::compile(Some(&aperio_config::HeaderDirectives {
    add: [("X-Served-By".to_string(), "aperio".to_string())]
      .into_iter()
      .collect(),
    remove: vec!["Server".to_string()],
  }));

  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-headers".to_string(),
      method: "GET".to_string(),
      uri: "/".to_string(),
      headers: vec![("x-secret".to_string(), "leak-me-not".to_string())],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered response");

  let TunnelMessage::Response { headers, .. } = result else {
    panic!("Expected response variant");
  };
  assert!(
    headers
      .iter()
      .any(|(k, v)| k == "X-Served-By" && v == "aperio"),
    "got: {headers:?}"
  );
  assert!(
    !headers
      .iter()
      .any(|(k, _)| k.eq_ignore_ascii_case("server")),
    "got: {headers:?}"
  );
  // Untouched backend headers pass through.
  assert!(
    headers.iter().any(|(k, _)| k == "x-old"),
    "got: {headers:?}"
  );
}

#[tokio::test]
async fn test_handle_incoming_request_trim_bind() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;
  use tokio::sync::oneshot;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);

  // Channel to receive the observed request line from the mock server.
  let (tx, rx) = oneshot::channel::<String>();

  tokio::spawn(async move {
    let _tx = tx;
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0; 1024];
      let n = socket.read(&mut buf).await.unwrap();
      let req_str = String::from_utf8_lossy(&buf[..n]).to_string();
      let request_line = req_str.lines().next().unwrap_or("").to_string();
      // Send the observed request line back, then write a minimal response.
      let _ = _tx.send(request_line);
      let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
      let _ = socket.write_all(response.as_bytes()).await;
    }
  });

  // path_bind = "/api", trim_bind = true → /api/hello should become /hello
  let ctx = ForwardContext {
    path_bind: Some("/api".to_string()),
    trim_bind: true,
    ..test_ctx(&target_url, test_tunnel_tx())
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-trim-1".to_string(),
      method: "GET".to_string(),
      uri: "/api/hello".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered response");

  let observed = rx.await.unwrap();
  // The mock server should have received the trimmed path "/hello".
  assert!(
    observed.contains("GET /hello"),
    "expected trimmed path '/hello' in request line, got: {}",
    observed
  );

  if let TunnelMessage::Response { status, .. } = result {
    assert_eq!(status, 200);
  } else {
    panic!("Expected response variant");
  }
}

#[tokio::test]
async fn test_build_dest_url_trims_only_at_segment_boundary() {
  let ctx = ForwardContext {
    path_bind: Some("/api".to_string()),
    trim_bind: true,
    ..test_ctx("http://127.0.0.1:1", test_tunnel_tx())
  };
  let path = |uri: &str| build_dest_url(&ctx, "id", uri).unwrap().path().to_string();
  // Exact bind and a sub-path are trimmed.
  assert_eq!(path("/api/hello"), "/hello");
  assert_eq!(path("/api"), "/");
  // A different route that merely shares the `api` prefix must NOT be trimmed.
  assert_eq!(path("/apiv2/hello"), "/apiv2/hello");
  assert_eq!(path("/apitrash"), "/apitrash");
}

#[tokio::test]
async fn test_handle_incoming_request_trim_bind_disabled() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;
  use tokio::sync::oneshot;

  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);

  let (tx, rx) = oneshot::channel::<String>();

  tokio::spawn(async move {
    let _tx = tx;
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0; 1024];
      let n = socket.read(&mut buf).await.unwrap();
      let req_str = String::from_utf8_lossy(&buf[..n]).to_string();
      let request_line = req_str.lines().next().unwrap_or("").to_string();
      let _ = _tx.send(request_line);
      let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
      let _ = socket.write_all(response.as_bytes()).await;
    }
  });

  // path_bind = "/api", trim_bind = false → path should NOT be stripped
  let ctx = ForwardContext {
    path_bind: Some("/api".to_string()),
    trim_bind: false,
    ..test_ctx(&target_url, test_tunnel_tx())
  };
  let _result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-trim-2".to_string(),
      method: "GET".to_string(),
      uri: "/api/hello".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await;

  let observed = rx.await.unwrap();
  assert!(
    observed.contains("GET /api/hello"),
    "expected untrimmed path '/api/hello' in request line, got: {}",
    observed
  );
}

#[tokio::test]
async fn test_redirect_stops_after_max_hops() {
  // A→B→C chain with max_hops=1: the second hop exceeds the limit, so the
  // redirect is passed through instead of followed (attempt.stop()).
  let c_port =
    mock_server("HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nend".to_string())
      .await;
  let b_port = mock_server(format!(
    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/c\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    c_port
  ))
  .await;
  let a_port = mock_server(format!(
    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/b\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    b_port
  ))
  .await;

  let ctx = ForwardContext {
    client: reqwest::Client::builder()
      .redirect(redirect_policy(1))
      .build()
      .unwrap(),
    ..test_ctx(&format!("http://127.0.0.1:{}", a_port), test_tunnel_tx())
  };
  let result = handle_incoming_request(
    &ctx,
    ForwardRequest {
      id: "req-hops".to_string(),
      method: "GET".to_string(),
      uri: "/".to_string(),
      headers: vec![],
      body: None,
      raw_body: None,
    },
    None,
    false,
    false,
  )
  .await
  .expect("expected buffered response");
  // The second redirect (B→C) is not followed → the 302 is returned as-is.
  let TunnelMessage::Response { status, .. } = result else {
    panic!("Expected Response variant");
  };
  assert_eq!(status, 302);
}

#[test]
fn test_redirect_policy_none_when_zero() {
  // max_hops == 0 yields a no-follow policy (smoke test of the early return).
  let _ = redirect_policy(0);
}

#[test]
fn splitting_the_uri_agrees_with_parsing_it_as_a_url() {
  // `build_dest_url` used to parse `http://localhost{uri}` just to reach the
  // path and the query. Parsing also normalizes (`..` collapses, stray
  // characters get encoded), and an SSRF check sits underneath, so the cheap
  // split may only replace it where the two agree exactly. This is that
  // proof, and it stays as the thing that fails if `set_path` ever stops
  // normalizing on its own.
  let base = url::Url::parse("http://127.0.0.1:3000").unwrap();
  for uri in [
    "/",
    "/a/b",
    "/a/../b",
    "/a/./b",
    "/a//b",
    "/..%2f..%2fetc/passwd",
    "/%2e%2e/x",
    "/a?x=1&y=2",
    "/?q",
    "/a?x=/../b",
    "/a%20b",
    "/ünicode/yol",
    "/a?",
    "/trailing/",
  ] {
    let parsed = url::Url::parse(&format!("http://localhost{uri}")).unwrap();

    let (raw_path, raw_query) = match uri.split_once('?') {
      Some((p, q)) => (p, Some(q)),
      None => (uri, None),
    };

    let mut from_parse = base.clone();
    from_parse.set_path(parsed.path());
    from_parse.set_query(parsed.query());

    let mut from_split = base.clone();
    from_split.set_path(raw_path);
    from_split.set_query(raw_query);

    assert_eq!(
      from_parse.as_str(),
      from_split.as_str(),
      "the two ways of reading {uri:?} disagree"
    );
  }
}

#[test]
fn an_absolute_form_uri_reaches_the_backend_as_its_path() {
  // HTTP/2 visitors send `:scheme` and `:authority`, so the URI arrives
  // rebuilt as `http://host/path`. The old expression prefixed
  // `http://localhost` to it, which made the whole thing the path: the
  // backend saw `/127.0.0.1:18110/echo` where it should see `/echo`.
  let (tx, _rx) = mpsc::channel(4);
  let ctx = test_ctx("http://127.0.0.1:3000", tx);

  let dest = build_dest_url(&ctx, "req-1", "http://127.0.0.1:18110/echo").unwrap();
  assert_eq!(dest.as_str(), "http://127.0.0.1:3000/echo");

  let dest = build_dest_url(&ctx, "req-2", "http://host/a?b=1").unwrap();
  assert_eq!(dest.as_str(), "http://127.0.0.1:3000/a?b=1");

  // Origin-form is unchanged, and neither shape reaches the backend when it
  // is not a URI at all.
  let dest = build_dest_url(&ctx, "req-3", "/plain?x=1").unwrap();
  assert_eq!(dest.as_str(), "http://127.0.0.1:3000/plain?x=1");
  assert_eq!(build_dest_url(&ctx, "req-4", ":notaport").unwrap_err(), 400);
}
