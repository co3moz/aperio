//! An OpenTelemetry relay at the edge.
//!
//! The problem this solves is not "Aperio should have telemetry", it is that
//! an edge host usually has exactly one outbound connection it is allowed to
//! make, the tunnel, and its telemetry has nowhere to go. Shipping spans from
//! there normally means a new firewall rule and a collector credential on a
//! machine that should hold as few of those as possible.
//!
//! So the client runs an OTLP receiver on loopback. Anything next to it
//! exports with `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318`, which is
//! one environment variable and no SDK change, and the payload travels to the
//! server, which forwards it to the collector it is already configured for.
//!
//! ## What this deliberately is not
//!
//! Not a collector. The payload is never inspected, aggregated, batched by
//! content or re-encoded here; it is bytes that go from one place to another.
//! That is what keeps it correct across OTLP versions we have never seen: a
//! relay that parsed the payload would need to understand every field to
//! avoid dropping one.
//!
//! Attribution is the server's job for the same reason it is the server's job
//! everywhere else: a client cannot be trusted to say which client it is.
//!
//! ## Protobuf only
//!
//! `application/x-protobuf`, which is what every SDK sends by default. JSON is
//! refused with a message saying so rather than passed through, because the
//! server injects identity into the payload and doing that for two encodings
//! means two chances to corrupt somebody's telemetry.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

/// One export waiting to be carried to the server.
#[derive(Debug, Clone)]
pub(crate) struct Export {
  /// `traces`, `metrics` or `logs`: the signal path the server appends to the
  /// collector's endpoint.
  pub(crate) signal: &'static str,
  pub(crate) payload: bytes::Bytes,
}

/// Exports dropped because the queue was full, counted so the loss is visible
/// rather than silent.
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// How many exports were dropped for lack of room. Read by the tests and by
/// the periodic report below.
pub(crate) fn dropped() -> u64 {
  DROPPED.load(Ordering::Relaxed)
}

/// Reports the running drop total every few minutes, once there is one.
///
/// A counter nobody reads is not an answer: the operator learns their
/// telemetry is incomplete from this line, or not at all, because the missing
/// spans look identical to spans that were never emitted.
pub(crate) async fn report_drops() {
  let mut last = 0u64;
  loop {
    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
    let now = dropped();
    if now > last {
      warn!(
        "OTel bridge: {} export(s) dropped in the last five minutes ({} in total); \
         the far end is not keeping up",
        now - last,
        now
      );
      last = now;
    }
  }
}

/// Queues an export, dropping the newest rather than waiting.
///
/// Never blocks the exporter: an SDK that cannot hand off its batch blocks the
/// application it is instrumenting, and telemetry that stalls the thing it is
/// measuring has done more harm than the missing spans ever would.
pub(crate) fn offer(tx: &tokio::sync::mpsc::Sender<Export>, export: Export) -> bool {
  if tx.try_send(export).is_err() {
    let n = DROPPED.fetch_add(1, Ordering::Relaxed) + 1;
    if n % 100 == 1 {
      warn!("OTel bridge queue is full; {n} export(s) dropped so far");
    }
    return false;
  }
  true
}

/// The signal a request path names, or `None` when it names none of them.
pub(crate) fn signal_of(path: &str) -> Option<&'static str> {
  match path.trim_end_matches('/') {
    "/v1/traces" => Some("traces"),
    "/v1/metrics" => Some("metrics"),
    "/v1/logs" => Some("logs"),
    _ => None,
  }
}

/// The signal a gRPC method names.
///
/// Matched on the service name rather than the full path so a future OTLP
/// version number in the path does not silently stop being recognized.
pub(crate) fn grpc_signal_of(path: &str) -> Option<&'static str> {
  if !path.ends_with("/Export") {
    return None;
  }
  if path.contains("collector.trace.") {
    return Some("traces");
  }
  if path.contains("collector.metrics.") {
    return Some("metrics");
  }
  if path.contains("collector.logs.") {
    return Some("logs");
  }
  None
}

/// Strips the gRPC length prefix from a body.
///
/// A gRPC message is a one-byte compression flag, a four-byte big-endian
/// length, then the protobuf. The prefix is removed here so what travels is
/// the same OTLP protobuf the HTTP receiver produces and the server has one
/// payload shape to handle.
///
/// A compressed message is refused rather than forwarded: the flag says the
/// bytes are not the protobuf the server will try to read, and forwarding
/// them would corrupt an export in a way that surfaces at the collector.
pub(crate) fn strip_grpc_frame(body: &[u8]) -> Result<bytes::Bytes, &'static str> {
  if body.len() < 5 {
    return Err("a gRPC message is at least five bytes");
  }
  if body[0] != 0 {
    return Err("compressed gRPC messages are not supported by this bridge");
  }
  let len = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
  if body.len() < 5 + len {
    return Err("the gRPC message is shorter than its own length prefix");
  }
  Ok(bytes::Bytes::copy_from_slice(&body[5..5 + len]))
}

/// Wraps a protobuf in a gRPC frame, for the empty response every OTLP
/// `Export` returns.
fn grpc_frame(payload: &[u8]) -> bytes::Bytes {
  let mut out = Vec::with_capacity(5 + payload.len());
  out.push(0);
  out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
  out.extend_from_slice(payload);
  out.into()
}

/// Largest export accepted, a fence against a misconfigured exporter filling
/// memory before the queue's own limit is reached.
const MAX_EXPORT_BYTES: usize = 8 * 1024 * 1024;

type BridgeBody = http_body_util::Full<bytes::Bytes>;

fn empty(status: StatusCode) -> Response<BridgeBody> {
  Response::builder()
    .status(status)
    .body(http_body_util::Full::new(bytes::Bytes::new()))
    .unwrap_or_default()
}

fn text(status: StatusCode, message: &str) -> Response<BridgeBody> {
  Response::builder()
    .status(status)
    .header("content-type", "text/plain")
    .body(http_body_util::Full::new(bytes::Bytes::from(
      message.to_string(),
    )))
    .unwrap_or_default()
}

/// Reads a request body, refusing one larger than the fence.
///
/// The fence is applied *while* reading, not after. Collecting first and
/// measuring afterwards means the fence only ever describes what was already
/// in memory: a sender that ignores it, or simply one misconfigured to ship
/// something enormous, is buffered in full before being told no, and several
/// at once multiply that.
async fn read_body(req: Request<hyper::body::Incoming>) -> Result<bytes::Bytes, ()> {
  use http_body_util::BodyExt;
  let limited = http_body_util::Limited::new(req.into_body(), MAX_EXPORT_BYTES);
  let collected = limited.collect().await.map_err(|_| ())?;
  Ok(collected.to_bytes())
}

/// Handles one OTLP/HTTP export.
async fn serve_http(
  req: Request<hyper::body::Incoming>,
  tx: tokio::sync::mpsc::Sender<Export>,
) -> Result<Response<BridgeBody>, std::convert::Infallible> {
  if req.method() != Method::POST {
    return Ok(empty(StatusCode::METHOD_NOT_ALLOWED));
  }
  let Some(signal) = signal_of(req.uri().path()) else {
    return Ok(empty(StatusCode::NOT_FOUND));
  };
  let json = req
    .headers()
    .get("content-type")
    .and_then(|v| v.to_str().ok())
    .is_some_and(|v| v.contains("json"));
  if json {
    return Ok(text(
      StatusCode::UNSUPPORTED_MEDIA_TYPE,
      "this bridge forwards OTLP protobuf only; set OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf",
    ));
  }
  let Ok(payload) = read_body(req).await else {
    return Ok(empty(StatusCode::PAYLOAD_TOO_LARGE));
  };
  if !offer(&tx, Export { signal, payload }) {
    // 429 rather than 503: the exporter's own retry policy treats this as
    // "slow down", which is exactly the situation, and a 5xx would have it
    // retry a batch the bridge has already decided it cannot carry.
    return Ok(empty(StatusCode::TOO_MANY_REQUESTS));
  }
  Ok(empty(StatusCode::OK))
}

/// Handles one OTLP/gRPC export.
///
/// A gRPC server without a gRPC framework: `Export` is a unary call, so the
/// whole of it is one framed protobuf in and one framed protobuf out, plus the
/// `grpc-status` trailer. Pulling in tonic for that would add a dependency to
/// the client for one method.
async fn serve_grpc(
  req: Request<hyper::body::Incoming>,
  tx: tokio::sync::mpsc::Sender<Export>,
) -> Result<Response<BridgeBody>, std::convert::Infallible> {
  let ok = |status: u8, message: &str| -> Response<BridgeBody> {
    Response::builder()
      .status(StatusCode::OK)
      .header("content-type", "application/grpc")
      // The status rides in a header rather than a trailer. A trailer is the
      // usual place, but a header is legal ("Trailers-Only") and is what lets
      // this answer without a streaming body.
      .header("grpc-status", status.to_string())
      .header("grpc-message", message.to_string())
      .body(http_body_util::Full::new(grpc_frame(&[])))
      .unwrap_or_default()
  };
  let Some(signal) = grpc_signal_of(req.uri().path()) else {
    // 12 = UNIMPLEMENTED.
    return Ok(ok(12, "not an OTLP Export method"));
  };
  let Ok(body) = read_body(req).await else {
    // 8 = RESOURCE_EXHAUSTED.
    return Ok(ok(8, "export too large"));
  };
  let payload = match strip_grpc_frame(&body) {
    Ok(p) => p,
    // 3 = INVALID_ARGUMENT.
    Err(e) => return Ok(ok(3, e)),
  };
  if !offer(&tx, Export { signal, payload }) {
    // 8 = RESOURCE_EXHAUSTED, which OTLP exporters treat as retryable.
    return Ok(ok(8, "bridge queue is full"));
  }
  Ok(ok(0, ""))
}

/// Runs the two receivers. Returns once both listeners are gone, which only
/// happens if neither could be bound.
pub(crate) async fn run(
  http_addr: Option<String>,
  grpc_addr: Option<String>,
  tx: tokio::sync::mpsc::Sender<Export>,
) {
  let mut tasks = Vec::new();
  if let Some(addr) = http_addr {
    tasks.push(tokio::spawn(listen(addr, tx.clone(), false)));
  }
  if let Some(addr) = grpc_addr {
    tasks.push(tokio::spawn(listen(addr, tx.clone(), true)));
  }
  for task in tasks {
    let _ = task.await;
  }
}

/// One listener. gRPC is served over HTTP/2 with prior knowledge, which is
/// what a gRPC client speaks to a cleartext endpoint; OTLP/HTTP is served over
/// HTTP/1.1, which is what every SDK's HTTP exporter sends.
async fn listen(addr: String, tx: tokio::sync::mpsc::Sender<Export>, grpc: bool) {
  let listener = match TcpListener::bind(&addr).await {
    Ok(l) => l,
    Err(e) => {
      error!("OTel bridge: cannot listen on {addr} ({e}); exports to it will be refused by the OS");
      return;
    }
  };
  info!(
    "OTel bridge: accepting OTLP/{} on {}",
    if grpc { "gRPC" } else { "HTTP" },
    addr
  );
  loop {
    let Ok((stream, _)) = listener.accept().await else {
      continue;
    };
    let io = TokioIo::new(stream);
    let tx = tx.clone();
    tokio::spawn(async move {
      if grpc {
        let service = service_fn(move |req| serve_grpc(req, tx.clone()));
        let _ = hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
          .serve_connection(io, service)
          .await;
      } else {
        let service = service_fn(move |req| serve_http(req, tx.clone()));
        let _ = hyper::server::conn::http1::Builder::new()
          .serve_connection(io, service)
          .await;
      }
    });
  }
}

/// Ships queued exports to the server over ordinary HTTPS.
///
/// The alternative transport, and the simpler one: the edge can reach the
/// server by definition, that is how the tunnel got there. What it gives up is
/// the property that makes the whole feature worth having on a locked-down
/// host, so it is not the default.
pub(crate) async fn run_https_forwarder(
  mut rx: tokio::sync::mpsc::Receiver<Export>,
  // Read per export rather than captured, so a configuration reload that
  // moves the server or rotates the token reaches this too. Captured, it
  // went on posting to the old address, or was refused by a token that had
  // been replaced, while the tunnel itself had already followed the change.
  credentials: tokio::sync::watch::Receiver<(String, String)>,
) {
  crate::ensure_crypto_provider();
  let client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(30))
    .build()
    .unwrap_or_default();
  while let Some(export) = rx.recv().await {
    let (server, token) = credentials.borrow().clone();
    let base = server.trim_end_matches('/');
    let url = format!("{base}/aperio/otlp/v1/{}", export.signal);
    let sent = client
      .post(&url)
      .bearer_auth(&token)
      .header("content-type", "application/x-protobuf")
      .body(export.payload)
      .send()
      .await;
    match sent {
      Ok(r) if r.status().is_success() => {}
      Ok(r) => warn!(
        "OTel bridge: the server answered {} for an export",
        r.status()
      ),
      Err(e) => warn!("OTel bridge: an export could not be delivered ({e})"),
    }
  }
}

/// The queue both transports read from, and the sender receivers write to.
pub(crate) fn channel(
  capacity: usize,
) -> (
  tokio::sync::mpsc::Sender<Export>,
  tokio::sync::mpsc::Receiver<Export>,
) {
  tokio::sync::mpsc::channel(capacity.max(1))
}

/// Shared handle to the queue, so the tunnel transport can pick exports up
/// from inside a service task without the two being wired together at start.
pub(crate) type Queue = Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Export>>>;

#[cfg(test)]
#[path = "otel_bridge_tests.rs"]
mod tests;
