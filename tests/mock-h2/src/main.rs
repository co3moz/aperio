//! E2E helper for the h2c tunnel path. Two modes:
//!
//! `mock-h2 server <port>` — cleartext HTTP/2 (prior knowledge) echo server:
//! answers 200 with `content-type: application/grpc`, body
//! `h2-echo:<request body>`, and trailers `grpc-status: 0`,
//! `grpc-message: ok`.
//!
//! `mock-h2 client <url> [body]` — prior-knowledge h2c POST that prints
//! `status=<n>`, `body=<text>`, and `trailer <name>=<value>` lines, so a
//! shell test can assert on trailer relay.

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use std::convert::Infallible;

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), BoxErr> {
  let args: Vec<String> = std::env::args().collect();
  match args.get(1).map(String::as_str) {
    Some("server") => server(args.get(2).ok_or("port required")?.parse()?).await,
    Some("client") => {
      client(
        args.get(2).ok_or("url required")?,
        args.get(3).cloned().unwrap_or_default(),
      )
      .await
    }
    _ => Err("usage: mock-h2 server <port> | mock-h2 client <url> [body]".into()),
  }
}

async fn server(port: u16) -> Result<(), BoxErr> {
  let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
  eprintln!("mock-h2 listening on 127.0.0.1:{port}");
  loop {
    let (stream, _) = listener.accept().await?;
    tokio::spawn(async move {
      let io = hyper_util::rt::TokioIo::new(stream);
      let service = hyper::service::service_fn(handle);
      let _ = hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
        .serve_connection(io, service)
        .await;
    });
  }
}

type FrameIter =
  futures_util::stream::Iter<std::vec::IntoIter<Result<hyper::body::Frame<Bytes>, Infallible>>>;

async fn handle(
  req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<StreamBody<FrameIter>>, Infallible> {
  let body = req
    .into_body()
    .collect()
    .await
    .map(|c| c.to_bytes())
    .unwrap_or_default();
  let mut echo = b"h2-echo:".to_vec();
  echo.extend_from_slice(&body);

  let mut trailers = hyper::HeaderMap::new();
  trailers.insert("grpc-status", hyper::header::HeaderValue::from_static("0"));
  trailers.insert(
    "grpc-message",
    hyper::header::HeaderValue::from_static("ok"),
  );
  let frames = vec![
    Ok(Frame::data(Bytes::from(echo))),
    Ok(Frame::trailers(trailers)),
  ];
  let response = hyper::Response::builder()
    .status(200)
    .header("content-type", "application/grpc")
    .body(StreamBody::new(futures_util::stream::iter(frames)))
    .unwrap();
  Ok(response)
}

async fn client(url: &str, body: String) -> Result<(), BoxErr> {
  let client: hyper_util::client::legacy::Client<_, Full<Bytes>> =
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
      .http2_only(true)
      .build(hyper_util::client::legacy::connect::HttpConnector::new());
  let req = hyper::Request::builder()
    .method("POST")
    .uri(url)
    .header("content-type", "application/grpc")
    .header("te", "trailers")
    .body(Full::new(Bytes::from(body)))?;
  let res = client.request(req).await?;
  println!("status={}", res.status().as_u16());
  let mut body = res.into_body();
  let mut data: Vec<u8> = Vec::new();
  while let Some(frame) = body.frame().await {
    let frame = frame?;
    if frame.is_data() {
      data.extend_from_slice(&frame.into_data().unwrap_or_default());
    } else if let Ok(trailers) = frame.into_trailers() {
      for (k, v) in trailers.iter() {
        println!("trailer {}={}", k, v.to_str().unwrap_or(""));
      }
    }
  }
  println!("body={}", String::from_utf8_lossy(&data));
  Ok(())
}
