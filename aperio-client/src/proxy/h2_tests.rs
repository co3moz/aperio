//! The HTTP/2 backend path against a real h2c server: which targets it claims,
//! buffered and streamed bodies in both directions, trailers, non-2xx pass-through,
//! and the gRPC health probe.

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
  let h2_client = build_h2_client(&target, None).map(Arc::new);
  ForwardContext {
    client: reqwest::Client::new(),
    h2_client,
    unix_socket: None,
    timeout_secs: 30,
    stream_pauses: Default::default(),
    resilience: crate::proxy::http::BackendResilience::new(1, 100, false, 0, 30),
    target_url: url::Url::parse(&target).ok(),
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
    raw_body: None,
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
  // Building a TLS h2 client needs a rustls crypto provider; production
  // installs it in main(), which unit tests don't run. Install it idempotently
  // so this test doesn't depend on another test having installed it first.
  let _ = rustls::crypto::ring::default_provider().install_default();
  assert!(matches!(
    build_h2_client("h2c://127.0.0.1:1", None),
    Some(H2Client::Cleartext(_))
  ));
  assert!(matches!(
    build_h2_client("h2://example.com", None),
    Some(H2Client::Tls(_))
  ));
  assert!(build_h2_client("http://x", None).is_none());
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
  let (btx, brx) = mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(4);
  btx.send(Ok(b"streamed-".to_vec().into())).await.unwrap();
  btx.send(Ok(b"req".to_vec().into())).await.unwrap();
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

  let mut got_abort = false;
  while let Some(Message::Text(json)) = rx.recv().await {
    if let TunnelMessage::ResponseAbort { .. } =
      serde_json::from_str::<TunnelMessage>(&json).unwrap()
    {
      got_abort = true;
      break;
    }
  }
  assert!(
    got_abort,
    "a truncated stream must terminate with ResponseAbort, not a clean ResponseEnd"
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
  // The TLS client needs a rustls crypto provider (production installs it in
  // main()); install it idempotently so the test is order-independent.
  let _ = rustls::crypto::ring::default_provider().install_default();
  let port = start_h2c_backend().await;
  let target = format!("h2://127.0.0.1:{}", port);
  let h2_client = build_h2_client(&target, None).map(Arc::new);
  let ctx = ForwardContext {
    client: reqwest::Client::new(),
    h2_client,
    unix_socket: None,
    timeout_secs: 5,
    stream_pauses: Default::default(),
    resilience: crate::proxy::http::BackendResilience::new(1, 100, false, 0, 30),
    target_url: url::Url::parse(&target).ok(),
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

// --- gRPC health checking (planned_features #35) ----------------------------

#[test]
fn a_request_for_the_whole_server_is_an_empty_message() {
  let frame = super::health_request_frame("");
  // Compression flag, then a four-byte length of zero: the empty
  // HealthCheckRequest that asks about the server rather than one service.
  assert_eq!(frame.as_ref(), &[0x00, 0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn a_named_service_is_encoded_as_field_one() {
  let frame = super::health_request_frame("my.Service");
  let msg = &frame[5..];
  assert_eq!(msg[0], 0x0a, "field 1, length-delimited");
  assert_eq!(msg[1] as usize, "my.Service".len());
  assert_eq!(&msg[2..], b"my.Service");
  // The frame header must agree with what follows it.
  assert_eq!(
    u32::from_be_bytes(frame[1..5].try_into().unwrap()) as usize,
    msg.len()
  );
}

/// Wraps a protobuf message in the gRPC length prefix, as a server would.
fn framed(msg: &[u8]) -> Vec<u8> {
  let mut out = vec![0u8];
  out.extend_from_slice(&(msg.len() as u32).to_be_bytes());
  out.extend_from_slice(msg);
  out
}

#[test]
fn serving_and_not_serving_are_read_from_the_response() {
  // status = 1 (SERVING)
  assert_eq!(
    super::health_response_status(&framed(&[0x08, 0x01])),
    Some(1)
  );
  // status = 2 (NOT_SERVING)
  assert_eq!(
    super::health_response_status(&framed(&[0x08, 0x02])),
    Some(2)
  );
  // status = 3 (SERVICE_UNKNOWN)
  assert_eq!(
    super::health_response_status(&framed(&[0x08, 0x03])),
    Some(3)
  );
}

#[test]
fn an_unreadable_response_is_not_read_as_serving() {
  // A health check that cannot be parsed has not said the backend is healthy,
  // so every one of these must fail to produce SERVING.
  assert_eq!(super::health_response_status(&[]), None, "empty body");
  assert_eq!(
    super::health_response_status(&[0, 0, 0, 0]),
    None,
    "truncated header"
  );
  assert_eq!(
    super::health_response_status(&framed(&[])),
    None,
    "an empty message names no status"
  );
  assert_eq!(
    super::health_response_status(&framed(&[0x12, 0x01, 0x01])),
    None,
    "a different field number is a message shape we do not understand"
  );
  assert_eq!(
    super::health_response_status(&framed(&[0x08, 0x80])),
    None,
    "a varint that never terminates"
  );
  // None of these equal SERVING, which is what the caller actually asks.
  for body in [vec![], framed(&[]), framed(&[0x08, 0x02])] {
    assert_ne!(super::health_response_status(&body), Some(1));
  }
}

#[test]
fn a_multi_byte_varint_status_is_decoded() {
  // Not a status the protocol defines, but the decoder must not mis-read a
  // continuation byte as a terminator.
  assert_eq!(
    super::health_response_status(&framed(&[0x08, 0xac, 0x02])),
    Some(300)
  );
}

#[tokio::test]
async fn a_health_probe_whose_body_never_arrives_gives_up_on_time() {
  // The head was under the probe's timeout and the body was not, so a backend
  // that answered with headers and then went quiet held the probe forever.
  // A probe that cannot finish cannot fail, and the backend keeps whatever
  // verdict it last had, which is the worst of both: unhealthy and reported
  // healthy.
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    while let Ok((stream, _)) = listener.accept().await {
      tokio::spawn(async move {
        let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
          .serve_connection(
            hyper_util::rt::TokioIo::new(stream),
            service_fn(|_req: Request<hyper::body::Incoming>| async {
              // 200 and a grpc-status the probe accepts, then a body stream
              // that never yields a frame and never ends.
              let frames = futures_util::stream::pending::<Result<Frame<Bytes>, std::io::Error>>();
              Ok::<_, std::convert::Infallible>(
                Response::builder()
                  .status(200)
                  .body(BoxBody::new(StreamBody::new(frames)))
                  .unwrap(),
              )
            }),
          )
          .await;
      });
    }
  });

  let target = format!("h2c://127.0.0.1:{port}");
  let client = super::build_h2_client(&target, None).expect("h2c client");
  // Under an outer deadline on purpose: a regression here does not fail, it
  // hangs, and a test that hangs tells nobody anything.
  let healthy = tokio::time::timeout(
    std::time::Duration::from_secs(5),
    super::grpc_health_check(&client, &target, "", std::time::Duration::from_millis(300)),
  )
  .await
  .expect("the probe never returned: the body is outside the timeout again");
  assert!(!healthy, "a probe that never completes is not a pass");
}

#[tokio::test]
async fn the_h2_path_honours_the_breaker_and_retries() {
  // Both were skipped entirely on this path: handle_incoming_request
  // dispatches to h2 before it reaches either, so a service whose backend
  // spoke HTTP/2 had `retry:` and `circuit_breaker:` in its config doing
  // nothing at all.
  let (tx, _rx) = mpsc::channel::<Message>(64);
  // Nothing listening: every attempt fails to connect.
  let mut ctx = h2_ctx(1, tx);
  ctx.resilience = crate::proxy::http::BackendResilience::new(3, 1, false, 2, 30);

  let started = std::time::Instant::now();
  let result = handle_incoming_request_h2(&ctx, req("r1", "GET", "/"), None, false).await;
  assert!(
    matches!(result, Some(TunnelMessage::Response { status: 502, .. })),
    "got {result:?}"
  );
  // Three attempts happened, not one: the backoff doubles from 1ms, so the
  // only claim made here is that more than one dial was made.
  assert!(started.elapsed() >= std::time::Duration::from_millis(3));

  // Two consecutive failures were configured to open the breaker, and the
  // first call recorded one. The second opens it, and from then on the
  // request is refused without dialing.
  let _ = handle_incoming_request_h2(&ctx, req("r2", "GET", "/"), None, false).await;
  let refused = std::time::Instant::now();
  let result = handle_incoming_request_h2(&ctx, req("r3", "GET", "/"), None, false).await;
  assert!(
    matches!(result, Some(TunnelMessage::Response { status: 502, .. })),
    "got {result:?}"
  );
  assert!(
    refused.elapsed() < std::time::Duration::from_millis(50),
    "an open breaker must answer without dialing, took {:?}",
    refused.elapsed()
  );
}

#[tokio::test]
async fn a_successful_h2_response_closes_the_breaker() {
  let port = start_h2c_backend().await;
  let (tx, _rx) = mpsc::channel::<Message>(64);
  let mut ctx = h2_ctx(port, tx);
  ctx.resilience = crate::proxy::http::BackendResilience::new(1, 1, false, 2, 30);
  let result = handle_incoming_request_h2(&ctx, req("ok", "GET", "/"), None, false).await;
  assert!(
    matches!(result, Some(TunnelMessage::Response { status: 200, .. })),
    "got {result:?}"
  );
  // A reachable backend keeps the breaker shut whatever it answered.
  assert!(matches!(
    ctx.resilience.check(),
    crate::proxy::http::BreakerVerdict::Proceed
  ));
}

#[tokio::test]
async fn a_stalled_h2_backend_is_retried_like_any_other_pre_response_failure() {
  // A timeout on the head answered 504 at once, skipping the retry loop it
  // was standing in, although `retry.attempts` is documented to cover
  // exactly this: a failure before any response arrived. A stalled backend
  // is the case an operator most expects a retry to cover, and it was the
  // one case that did not get one.
  let port = start_h2c_backend().await;
  let (tx, _rx) = mpsc::channel::<Message>(64);
  let mut ctx = h2_ctx(port, tx);
  ctx.timeout_secs = 1;
  ctx.resilience = crate::proxy::http::BackendResilience::new(2, 1, false, 0, 30);

  let started = std::time::Instant::now();
  // `/hang` never answers, so both attempts run out their budget.
  let result = handle_incoming_request_h2(&ctx, req("slow", "GET", "/hang"), None, false).await;
  assert!(
    matches!(result, Some(TunnelMessage::Response { status: 504, .. })),
    "a stalled backend is still a 504 in the end, got {result:?}"
  );
  assert!(
    started.elapsed() >= std::time::Duration::from_secs(2),
    "took {:?}: only one attempt was made, so the timeout skipped the retry",
    started.elapsed()
  );
}

/// This path strips everything every path to a backend strips.
///
/// The shared list lives in `aperio-config` so the assertion runs in the suite
/// a change to *this* file runs, which is the whole point: the server grew a
/// second path to a backend and skipped these, and nothing failed because the
/// only check was on the other side.
#[test]
fn the_h2_path_strips_the_shared_set() {
  aperio_config::hop_by_hop::strips_the_core(is_hop_by_hop).expect("the shared strip");
  aperio_config::hop_by_hop::leaves_ordinary_headers(is_hop_by_hop).expect("ordinary headers");
  // This path's own two, recorded so a change to either is deliberate.
  assert!(
    is_hop_by_hop("host"),
    "HTTP/2 carries the authority as a pseudo-header"
  );
  assert!(
    !is_hop_by_hop("te"),
    "`te: trailers` travels end to end, which gRPC needs"
  );
}
