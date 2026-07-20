use super::*;

use crate::proxy::http::HeaderTransform;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::Frame;
use hyper::service::service_fn;
use hyper::{Request, Response};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::protocol::Message;

/// Backend response body type used by the test HTTP/2 server.
type SrvBody = BoxBody<Bytes, std::io::Error>;

/// Wraps bytes into a single-frame backend body.
fn full(b: impl Into<Bytes>) -> SrvBody {
  BoxBody::new(Full::new(b.into()).map_err(|never| match never {}))
}

/// Router for the in-test HTTP/2 backend. Paths select the behaviour each
/// test needs (plain body, trailers, large body, custom status, echo).
async fn h2_handler(
  req: Request<hyper::body::Incoming>,
) -> Result<Response<SrvBody>, std::convert::Infallible> {
  let path = req.uri().path().to_string();
  let resp = match path.as_str() {
    "/trailer" => {
      // gRPC-style: body followed by a trailers frame.
      let mut trailers = hyper::HeaderMap::new();
      trailers.insert("grpc-status", "0".parse().unwrap());
      let frames = futures_util::stream::iter(vec![
        Ok::<_, std::io::Error>(Frame::data(Bytes::from_static(b"grpc-body"))),
        Ok(Frame::trailers(trailers)),
      ]);
      Response::builder()
        .status(200)
        .body(BoxBody::new(StreamBody::new(frames)))
        .unwrap()
    }
    "/big" => {
      // Larger than STREAM_THRESHOLD (256 KiB) → forces streaming.
      let payload = vec![0x5Au8; 600 * 1024];
      Response::builder().status(200).body(full(payload)).unwrap()
    }
    "/big-trailer" => {
      // Large body plus trailers → trailers must ride on ResponseEnd.
      let mut trailers = hyper::HeaderMap::new();
      trailers.insert("grpc-status", "7".parse().unwrap());
      let payload = vec![0x11u8; 600 * 1024];
      let frames = futures_util::stream::iter(vec![
        Ok::<_, std::io::Error>(Frame::data(Bytes::from(payload))),
        Ok(Frame::trailers(trailers)),
      ]);
      Response::builder()
        .status(200)
        .body(BoxBody::new(StreamBody::new(frames)))
        .unwrap()
    }
    "/big-multiframe" => {
      // Several data frames whose running total crosses the stream threshold,
      // so later frames arrive while already streaming.
      let frames = futures_util::stream::iter(vec![
        Ok::<_, std::io::Error>(Frame::data(Bytes::from(vec![1u8; 200 * 1024]))),
        Ok(Frame::data(Bytes::from(vec![2u8; 200 * 1024]))),
        Ok(Frame::data(Bytes::from(vec![3u8; 200 * 1024]))),
      ]);
      Response::builder()
        .status(200)
        .body(BoxBody::new(StreamBody::new(frames)))
        .unwrap()
    }
    "/teapot" => Response::builder().status(418).body(full("nope")).unwrap(),
    "/echo" => {
      let bytes = req.into_body().collect().await.unwrap().to_bytes();
      Response::builder().status(200).body(full(bytes)).unwrap()
    }
    "/hang" => {
      // Never responds: exercises the client-side request timeout.
      tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
      Response::builder().status(200).body(full("")).unwrap()
    }
    _ => Response::builder()
      .status(200)
      .header("x-backend", "h2")
      .body(full("hello h2"))
      .unwrap(),
  };
  Ok(resp)
}

/// Starts an h2c (prior-knowledge cleartext HTTP/2) backend and returns its
/// port.
async fn start_h2c_backend() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    while let Ok((stream, _)) = listener.accept().await {
      let io = hyper_util::rt::TokioIo::new(stream);
      tokio::spawn(async move {
        let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
          .serve_connection(io, service_fn(h2_handler))
          .await;
      });
    }
  });
  port
}

/// Drains the tunnel channel in the background (buffered-response tests).
fn drained_tx() -> mpsc::Sender<Message> {
  let (tx, mut rx) = mpsc::channel::<Message>(256);
  tokio::spawn(async move { while rx.recv().await.is_some() {} });
  tx
}

/// Forwarding context wired to an h2c target on the given port.
fn h2_ctx(port: u16, tunnel_tx: mpsc::Sender<Message>) -> ForwardContext {
  let target = format!("h2c://127.0.0.1:{}", port);
  let h2_client = build_h2_client(&target).map(Arc::new);
  ForwardContext {
    client: reqwest::Client::new(),
    h2_client,
    unix_socket: None,
    timeout_secs: 30,
    target,
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

#[test]
fn test_is_h2_target() {
  assert!(is_h2_target("h2c://127.0.0.1:50051"));
  assert!(is_h2_target("h2://example.com"));
  assert!(!is_h2_target("http://localhost:3000"));
  assert!(!is_h2_target("unix:///var/run/app.sock"));
}

#[test]
fn test_build_h2_client_variants() {
  assert!(matches!(
    build_h2_client("h2c://127.0.0.1:1"),
    Some(H2Client::Cleartext(_))
  ));
  assert!(matches!(
    build_h2_client("h2://example.com"),
    Some(H2Client::Tls(_))
  ));
  assert!(build_h2_client("http://x").is_none());
}

#[tokio::test]
async fn test_h2_buffered_success() {
  let port = start_h2c_backend().await;
  let ctx = h2_ctx(port, drained_tx());
  // Include connection-control and `te` headers to exercise the strip and the
  // `te: trailers` keep/skip branches.
  let mut r = req("h2-ok", "GET", "/");
  r.headers = vec![
    ("connection".to_string(), "keep-alive".to_string()),
    ("host".to_string(), "ignored".to_string()),
    ("te".to_string(), "trailers".to_string()),
    ("te".to_string(), "gzip".to_string()),
    ("x-fwd".to_string(), "yes".to_string()),
  ];
  let result = handle_incoming_request_h2(&ctx, r, None, false)
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
  assert!(headers.iter().any(|(k, v)| k == "x-backend" && v == "h2"));
  let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
  assert_eq!(String::from_utf8(decoded).unwrap(), "hello h2");
}

#[tokio::test]
async fn test_h2_trailers_buffered() {
  let port = start_h2c_backend().await;
  let ctx = h2_ctx(port, drained_tx());
  let result = handle_incoming_request_h2(&ctx, req("h2-tr", "GET", "/trailer"), None, false)
    .await
    .expect("buffered response");
  let TunnelMessage::Response {
    status,
    body,
    trailers,
    ..
  } = result
  else {
    panic!("expected Response");
  };
  assert_eq!(status, 200);
  let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
  assert_eq!(String::from_utf8(decoded).unwrap(), "grpc-body");
  let trailers = trailers.expect("trailers present");
  assert!(trailers.iter().any(|(k, v)| k == "grpc-status" && v == "0"));
}

#[tokio::test]
async fn test_h2_non_2xx_passthrough() {
  let port = start_h2c_backend().await;
  let ctx = h2_ctx(port, drained_tx());
  let result = handle_incoming_request_h2(&ctx, req("h2-418", "GET", "/teapot"), None, false)
    .await
    .expect("buffered response");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 418);
}

#[tokio::test]
async fn test_h2_echo_body() {
  let port = start_h2c_backend().await;
  let ctx = h2_ctx(port, drained_tx());
  let mut r = req("h2-echo", "POST", "/echo");
  r.body = Some(BASE64_STANDARD.encode(b"ping-body"));
  let result = handle_incoming_request_h2(&ctx, r, None, false)
    .await
    .expect("buffered response");
  let TunnelMessage::Response { body, .. } = result else {
    panic!("expected Response");
  };
  let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
  assert_eq!(String::from_utf8(decoded).unwrap(), "ping-body");
}

#[tokio::test]
async fn test_h2_echo_streamed_request_body() {
  let port = start_h2c_backend().await;
  let ctx = h2_ctx(port, drained_tx());
  // Feed the request body through the streamed-body channel.
  let (btx, brx) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
  btx.send(Ok(b"streamed-".to_vec())).await.unwrap();
  btx.send(Ok(b"req".to_vec())).await.unwrap();
  drop(btx);
  let result = handle_incoming_request_h2(&ctx, req("h2-sreq", "POST", "/echo"), Some(brx), false)
    .await
    .expect("buffered response");
  let TunnelMessage::Response { body, .. } = result else {
    panic!("expected Response");
  };
  let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
  assert_eq!(String::from_utf8(decoded).unwrap(), "streamed-req");
}

#[tokio::test]
async fn test_h2_streams_large_body() {
  let port = start_h2c_backend().await;
  let (tx, mut rx) = mpsc::channel::<Message>(512);
  let ctx = h2_ctx(port, tx);
  let result = handle_incoming_request_h2(&ctx, req("h2-big", "GET", "/big"), None, false).await;
  assert!(result.is_none(), "large body streams (returns None)");

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
async fn test_h2_streams_large_body_with_trailers_binary() {
  let port = start_h2c_backend().await;
  let (tx, mut rx) = mpsc::channel::<Message>(512);
  let ctx = h2_ctx(port, tx);
  // binary_chunks=true → chunks come back as raw binary frames.
  let result =
    handle_incoming_request_h2(&ctx, req("h2-bigtr", "GET", "/big-trailer"), None, true).await;
  assert!(result.is_none());

  let mut got_end_trailers = None;
  let mut total = 0usize;
  while let Some(msg) = rx.recv().await {
    match msg {
      Message::Binary(bytes) => {
        let (_tag, _id, payload) = crate::protocol::decode_binary_frame(&bytes).unwrap();
        total += payload.len();
      }
      Message::Text(json) => match serde_json::from_str::<TunnelMessage>(&json).unwrap() {
        TunnelMessage::ResponseStart { .. } => {}
        TunnelMessage::ResponseEnd { trailers, .. } => {
          got_end_trailers = Some(trailers);
          break;
        }
        other => panic!("unexpected: {:?}", other),
      },
      other => panic!("unexpected: {:?}", other),
    }
  }
  assert_eq!(total, 600 * 1024);
  let trailers = got_end_trailers.unwrap().expect("trailers on end");
  assert!(trailers.iter().any(|(k, v)| k == "grpc-status" && v == "7"));
}

#[tokio::test]
async fn test_h2_stream_truncated_at_limit() {
  let port = start_h2c_backend().await;
  let (tx, mut rx) = mpsc::channel::<Message>(512);
  let mut ctx = h2_ctx(port, tx);
  // Cap below the full 600 KiB payload so streaming truncates mid-body.
  ctx.max_response_body_size = 300 * 1024;
  let result =
    handle_incoming_request_h2(&ctx, req("h2-trunc", "GET", "/big-multiframe"), None, false).await;
  assert!(result.is_none(), "streams then truncates");

  let mut got_end = false;
  while let Some(Message::Text(json)) = rx.recv().await {
    if let TunnelMessage::ResponseEnd { .. } = serde_json::from_str::<TunnelMessage>(&json).unwrap()
    {
      got_end = true;
      break;
    }
  }
  assert!(
    got_end,
    "truncated stream still terminates with ResponseEnd"
  );
}

#[tokio::test]
async fn test_h2_backend_unreachable() {
  // Port 1 has no listener → connection refused → 502.
  let ctx = h2_ctx(1, drained_tx());
  let result = handle_incoming_request_h2(&ctx, req("h2-refused", "GET", "/"), None, false)
    .await
    .expect("buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 502);
}

#[tokio::test]
async fn test_h2_timeout() {
  let port = start_h2c_backend().await;
  let mut ctx = h2_ctx(port, drained_tx());
  ctx.timeout_secs = 1;
  let result = handle_incoming_request_h2(&ctx, req("h2-hang", "GET", "/hang"), None, false)
    .await
    .expect("buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 504);
}

#[tokio::test]
async fn test_h2_missing_client_is_bug_500() {
  let mut ctx = h2_ctx(1, drained_tx());
  ctx.h2_client = None;
  let result = handle_incoming_request_h2(&ctx, req("h2-nobug", "GET", "/"), None, false)
    .await
    .expect("buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 500);
}

#[tokio::test]
async fn test_h2_invalid_method_400() {
  let ctx = h2_ctx(1, drained_tx());
  // Space is not a valid method token.
  let result = handle_incoming_request_h2(&ctx, req("h2-badm", "BAD METHOD", "/"), None, false)
    .await
    .expect("buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 400);
}

#[tokio::test]
async fn test_h2_bad_base64_body_400() {
  let ctx = h2_ctx(1, drained_tx());
  let mut r = req("h2-b64", "POST", "/echo");
  r.body = Some("!!not-base64!!".to_string());
  let result = handle_incoming_request_h2(&ctx, r, None, false)
    .await
    .expect("buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 400);
}

#[tokio::test]
async fn test_h2_tls_target_handshake_fails_502() {
  // An h2:// (TLS) target pointed at a plaintext port: the TLS client dials,
  // the handshake fails, and the error maps to 502. Exercises the TLS request
  // arm and the h2:// wire-URL branch.
  let port = start_h2c_backend().await;
  let target = format!("h2://127.0.0.1:{}", port);
  let h2_client = build_h2_client(&target).map(Arc::new);
  let ctx = ForwardContext {
    client: reqwest::Client::new(),
    h2_client,
    unix_socket: None,
    timeout_secs: 5,
    target,
    pass_hostname: false,
    path_bind: None,
    trim_bind: false,
    max_response_body_size: 10 * 1024 * 1024,
    tunnel_tx: drained_tx(),
    request_headers: HeaderTransform::default(),
    response_headers: HeaderTransform::default(),
  };
  let result = handle_incoming_request_h2(&ctx, req("h2-tls", "GET", "/"), None, false)
    .await
    .expect("buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  // 502 (handshake refused) or 504 (timeout) both indicate the TLS path ran.
  assert!(status == 502 || status == 504, "got {status}");
}

#[tokio::test]
async fn test_h2_unparsable_incoming_uri_400() {
  // The incoming URI is spliced into `http://localhost<uri>`; an invalid port
  // makes that URL unparsable → build_dest_url returns 400.
  let ctx = h2_ctx(1, drained_tx());
  let result = handle_incoming_request_h2(&ctx, req("h2-badpath", "GET", ":notaport"), None, false)
    .await
    .expect("buffered error");
  let TunnelMessage::Response { status, .. } = result else {
    panic!("expected Response");
  };
  assert_eq!(status, 400);
}
