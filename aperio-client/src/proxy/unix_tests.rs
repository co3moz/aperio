use super::*;

use crate::proxy::http::HeaderTransform;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::Message;

#[test]
fn test_is_unix_target() {
  assert!(is_unix_target("unix:///var/run/app.sock"));
  assert!(is_unix_target("unix://./app.sock"));
  assert!(!is_unix_target("http://localhost:3000"));
  assert!(!is_unix_target("h2c://127.0.0.1:50051"));
}

#[test]
fn test_unix_socket_path() {
  assert_eq!(
    unix_socket_path("unix:///var/run/app.sock").as_deref(),
    Some("/var/run/app.sock")
  );
  assert_eq!(
    unix_socket_path("unix://./app.sock").as_deref(),
    Some("./app.sock")
  );
  assert_eq!(unix_socket_path("unix://"), None);
  assert_eq!(unix_socket_path("http://x"), None);
}

#[tokio::test]
async fn test_build_origin_uri() {
  // No bind: path passes through with a leading slash, query preserved.
  let ctx = base_ctx("/tmp/x.sock", drained_tx());
  assert_eq!(build_origin_uri(&ctx, "/hello"), "/hello");
  assert_eq!(build_origin_uri(&ctx, "/hello?a=1&b=2"), "/hello?a=1&b=2");
  assert_eq!(build_origin_uri(&ctx, "/"), "/");

  // trim_bind strips the configured bind prefix.
  let mut ctx = base_ctx("/tmp/x.sock", drained_tx());
  ctx.path_bind = Some("/api".to_string());
  ctx.trim_bind = true;
  assert_eq!(build_origin_uri(&ctx, "/api/users?q=1"), "/users?q=1");
  assert_eq!(build_origin_uri(&ctx, "/api"), "/");
  // Non-matching prefix is left intact.
  assert_eq!(build_origin_uri(&ctx, "/other/x"), "/other/x");

  // trim_bind disabled: prefix retained.
  let mut ctx = base_ctx("/tmp/x.sock", drained_tx());
  ctx.path_bind = Some("/api".to_string());
  ctx.trim_bind = false;
  assert_eq!(build_origin_uri(&ctx, "/api/users"), "/api/users");
}

// ---- Integration tests over a real Unix-socket HTTP/1 backend ----

type SrvBody = http_body_util::combinators::BoxBody<Bytes, std::io::Error>;

fn full(b: impl Into<Bytes>) -> SrvBody {
  http_body_util::combinators::BoxBody::new(Full::new(b.into()).map_err(|never| match never {}))
}

/// Drains the tunnel channel in the background.
fn drained_tx() -> mpsc::Sender<Message> {
  let (tx, mut rx) = mpsc::channel::<Message>(256);
  tokio::spawn(async move { while rx.recv().await.is_some() {} });
  tx
}

/// Base forwarding context for a unix-socket target.
fn base_ctx(socket_path: &str, tunnel_tx: mpsc::Sender<Message>) -> ForwardContext {
  ForwardContext {
    client: reqwest::Client::new(),
    h2_client: None,
    unix_socket: Some(socket_path.to_string()),
    timeout_secs: 30,
    target: format!("unix://{socket_path}"),
    pass_hostname: false,
    path_bind: None,
    trim_bind: false,
    max_response_body_size: 10 * 1024 * 1024,
    tunnel_tx,
    request_headers: HeaderTransform::default(),
    response_headers: HeaderTransform::default(),
  }
}

fn req(id: &str, method: &str, uri: &str) -> ForwardRequest {
  ForwardRequest {
    id: id.to_string(),
    method: method.to_string(),
    uri: uri.to_string(),
    headers: vec![],
    body: None,
  }
}

async fn unix_handler(
  req: Request<hyper::body::Incoming>,
) -> Result<Response<SrvBody>, std::convert::Infallible> {
  let path = req.uri().path().to_string();
  let resp = match path.as_str() {
    "/big" => {
      let payload = vec![0x7Eu8; 600 * 1024];
      Response::builder()
        .status(200)
        .header("content-type", "application/octet-stream")
        .body(full(payload))
        .unwrap()
    }
    "/big-multiframe" => {
      // Multiple data frames whose combined size crosses the threshold.
      let frames = futures_util::stream::iter(vec![
        Ok::<_, std::io::Error>(Frame::data(Bytes::from(vec![1u8; 200 * 1024]))),
        Ok(Frame::data(Bytes::from(vec![2u8; 200 * 1024]))),
        Ok(Frame::data(Bytes::from(vec![3u8; 200 * 1024]))),
      ]);
      Response::builder()
        .status(200)
        .body(SrvBody::new(StreamBody::new(frames)))
        .unwrap()
    }
    "/teapot" => Response::builder()
      .status(418)
      .body(full("teapot"))
      .unwrap(),
    "/echo" => {
      let body = req.into_body().collect().await.unwrap().to_bytes();
      Response::builder().status(200).body(full(body)).unwrap()
    }
    "/reflect-host" => {
      let host = req
        .headers()
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
      Response::builder().status(200).body(full(host)).unwrap()
    }
    "/hang" => {
      tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
      Response::builder().status(200).body(full("")).unwrap()
    }
    _ => Response::builder()
      .status(200)
      .header("x-served", "unix")
      .body(full("hello unix"))
      .unwrap(),
  };
  Ok(resp)
}

/// Starts a unix-socket HTTP/1 backend at a fresh temp path and returns it.
async fn start_unix_backend() -> String {
  let dir = std::env::temp_dir();
  let path = dir.join(format!("aperio-test-{}.sock", uuid::Uuid::new_v4()));
  let path_str = path.to_string_lossy().to_string();
  let _ = std::fs::remove_file(&path);
  let listener = tokio::net::UnixListener::bind(&path).unwrap();
  tokio::spawn(async move {
    while let Ok((stream, _)) = listener.accept().await {
      let io = hyper_util::rt::TokioIo::new(stream);
      tokio::spawn(async move {
        let _ = hyper::server::conn::http1::Builder::new()
          .serve_connection(io, service_fn(unix_handler))
          .await;
      });
    }
  });
  path_str
}

#[tokio::test]
async fn test_unix_buffered_success() {
  let sock = start_unix_backend().await;
  let ctx = base_ctx(&sock, drained_tx());
  let mut r = req("u-ok", "GET", "/");
  // Exercise connection-header stripping and host capture.
  r.headers = vec![
    ("connection".to_string(), "keep-alive".to_string()),
    ("host".to_string(), "visitor.example".to_string()),
    ("x-fwd".to_string(), "1".to_string()),
  ];
  let result = handle_incoming_request_unix(&ctx, r, None, false)
    .await
    .expect("buffered response");
  let TunnelMessage::Response {
    status,
    headers,
    body,
    ..
  } = result
  else {
    panic!("expected Response");
  };
  assert_eq!(status, 200);
  assert!(headers.iter().any(|(k, v)| k == "x-served" && v == "unix"));
  let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
  assert_eq!(String::from_utf8(decoded).unwrap(), "hello unix");
}

#[tokio::test]
async fn test_unix_pass_hostname() {
  let sock = start_unix_backend().await;
  let mut ctx = base_ctx(&sock, drained_tx());
  ctx.pass_hostname = true;
  let mut r = req("u-host", "GET", "/reflect-host");
  r.headers = vec![("host".to_string(), "app.example.com".to_string())];
  let result = handle_incoming_request_unix(&ctx, r, None, false)
    .await
    .expect("buffered response");
  let TunnelMessage::Response { body, .. } = result else {
    panic!("expected Response");
  };
  let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
  assert_eq!(String::from_utf8(decoded).unwrap(), "app.example.com");
}

#[tokio::test]
async fn test_unix_default_host_localhost() {
  let sock = start_unix_backend().await;
  let ctx = base_ctx(&sock, drained_tx());
  // No pass_hostname and no Host header → "localhost" stands in.
  let result =
    handle_incoming_request_unix(&ctx, req("u-host2", "GET", "/reflect-host"), None, false)
      .await
      .expect("buffered response");
  let TunnelMessage::Response { body, .. } = result else {
    panic!("expected Response");
  };
  let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
  assert_eq!(String::from_utf8(decoded).unwrap(), "localhost");
}

#[tokio::test]
async fn test_unix_non_2xx_passthrough() {
  let sock = start_unix_backend().await;
  let ctx = base_ctx(&sock, drained_tx());
  let result = handle_incoming_request_unix(&ctx, req("u-418", "GET", "/teapot"), None, false)
    .await
    .expect("buffered response");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 418);
}

#[tokio::test]
async fn test_unix_echo_body() {
  let sock = start_unix_backend().await;
  let ctx = base_ctx(&sock, drained_tx());
  let mut r = req("u-echo", "POST", "/echo");
  r.body = Some(BASE64_STANDARD.encode(b"unix-body"));
  let result = handle_incoming_request_unix(&ctx, r, None, false)
    .await
    .expect("buffered response");
  let TunnelMessage::Response { body, .. } = result else {
    panic!("expected Response");
  };
  let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
  assert_eq!(String::from_utf8(decoded).unwrap(), "unix-body");
}

#[tokio::test]
async fn test_unix_echo_streamed_request_body() {
  let sock = start_unix_backend().await;
  let ctx = base_ctx(&sock, drained_tx());
  let (btx, brx) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
  btx.send(Ok(b"aaa".to_vec())).await.unwrap();
  btx.send(Ok(b"bbb".to_vec())).await.unwrap();
  drop(btx);
  let result = handle_incoming_request_unix(&ctx, req("u-sreq", "POST", "/echo"), Some(brx), false)
    .await
    .expect("buffered response");
  let TunnelMessage::Response { body, .. } = result else {
    panic!("expected Response");
  };
  let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
  assert_eq!(String::from_utf8(decoded).unwrap(), "aaabbb");
}

#[tokio::test]
async fn test_unix_streams_large_body() {
  let sock = start_unix_backend().await;
  let (tx, mut rx) = mpsc::channel::<Message>(512);
  let ctx = base_ctx(&sock, tx);
  let result = handle_incoming_request_unix(&ctx, req("u-big", "GET", "/big"), None, false).await;
  assert!(result.is_none(), "large body streams");

  let mut got_start = false;
  let mut got_end = false;
  let mut total = 0usize;
  while let Some(Message::Text(json)) = rx.recv().await {
    match serde_json::from_str::<TunnelMessage>(&json).unwrap() {
      TunnelMessage::ResponseStart { status, .. } => {
        assert_eq!(status, 200);
        got_start = true;
      }
      TunnelMessage::ResponseChunk { data, .. } => {
        total += BASE64_STANDARD.decode(data).unwrap().len();
      }
      TunnelMessage::ResponseEnd { .. } => {
        got_end = true;
        break;
      }
      other => panic!("unexpected: {:?}", other),
    }
  }
  assert!(got_start && got_end);
  assert_eq!(total, 600 * 1024);
}

#[tokio::test]
async fn test_unix_streams_multiframe_binary_chunks() {
  let sock = start_unix_backend().await;
  let (tx, mut rx) = mpsc::channel::<Message>(512);
  let ctx = base_ctx(&sock, tx);
  // binary_chunks=true exercises the raw-frame chunk path plus a data frame
  // arriving after streaming has already begun.
  let result =
    handle_incoming_request_unix(&ctx, req("u-mf", "GET", "/big-multiframe"), None, true).await;
  assert!(result.is_none());

  let mut total = 0usize;
  let mut got_end = false;
  while let Some(msg) = rx.recv().await {
    match msg {
      Message::Binary(bytes) => {
        let (_t, _i, payload) = crate::protocol::decode_binary_frame(&bytes).unwrap();
        total += payload.len();
      }
      Message::Text(json) => match serde_json::from_str::<TunnelMessage>(&json).unwrap() {
        TunnelMessage::ResponseStart { .. } => {}
        TunnelMessage::ResponseEnd { .. } => {
          got_end = true;
          break;
        }
        other => panic!("unexpected: {:?}", other),
      },
      other => panic!("unexpected: {:?}", other),
    }
  }
  assert!(got_end);
  assert_eq!(total, 600 * 1024);
}

#[tokio::test]
async fn test_unix_backend_unreachable() {
  // Nonexistent socket path → connection error → 502.
  let ctx = base_ctx("/tmp/aperio-does-not-exist.sock", drained_tx());
  let result = handle_incoming_request_unix(&ctx, req("u-refused", "GET", "/"), None, false)
    .await
    .expect("buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 502);
}

#[tokio::test]
async fn test_unix_missing_socket_is_bug_500() {
  let mut ctx = base_ctx("/tmp/x.sock", drained_tx());
  ctx.unix_socket = None;
  let result = handle_incoming_request_unix(&ctx, req("u-bug", "GET", "/"), None, false)
    .await
    .expect("buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 500);
}

#[tokio::test]
async fn test_unix_invalid_method_400() {
  let ctx = base_ctx("/tmp/x.sock", drained_tx());
  let result = handle_incoming_request_unix(&ctx, req("u-badm", "BAD METHOD", "/"), None, false)
    .await
    .expect("buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 400);
}

#[tokio::test]
async fn test_unix_bad_base64_body_400() {
  let ctx = base_ctx("/tmp/x.sock", drained_tx());
  let mut r = req("u-b64", "POST", "/echo");
  r.body = Some("!!not-base64!!".to_string());
  let result = handle_incoming_request_unix(&ctx, r, None, false)
    .await
    .expect("buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 400);
}

#[tokio::test]
async fn test_unix_timeout() {
  let sock = start_unix_backend().await;
  let mut ctx = base_ctx(&sock, drained_tx());
  ctx.timeout_secs = 1;
  let result = handle_incoming_request_unix(&ctx, req("u-hang", "GET", "/hang"), None, false)
    .await
    .expect("buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 504);
}
