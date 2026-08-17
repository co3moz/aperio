use crate::protocol::TunnelMessage;
use crate::state::AppState;
use axum::extract::ws::Message;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, oneshot};

/// Round-robin group key: (hostname group, path group) of the selected pool.
pub(crate) type RouteGroupKey = (Option<String>, Option<String>);

/// A pending OIDC login: (post-login redirect, bound org id, the callback URL
/// sent to the provider, expiry).
///
/// The callback URL is remembered rather than re-derived: the token exchange
/// has to present the *same* `redirect_uri` the authorization request did, and
/// re-deriving it from the callback request's `Host` would let that header
/// decide what the exchange claims.
pub(crate) type OidcStateEntry = (String, Option<String>, String, Instant);

/// One frame of a streamed response body relayed from the tunnel: data
/// chunks, then optionally one trailer block (e.g. gRPC's `grpc-status`).
/// `Bytes` rather than `Vec<u8>` for the same reason as
/// `TunnelResponse::body_raw`: the WebSocket message the chunk arrived in is
/// refcounted, so the frame is a slice of it rather than a copy of it.
pub(crate) enum BodyFrame {
  Data(axum::body::Bytes),
  Trailers(Vec<(String, String)>),
}

/// Standard response payload returned by tunnel client.
pub(crate) struct TunnelResponse {
  /// HTTP status code.
  pub(crate) status: u16,
  /// List of response headers (preserves duplicates like Set-Cookie).
  pub(crate) headers: Vec<(String, String)>,
  /// Base64 encoded payload body (buffered responses only, peers before v5).
  pub(crate) body: Option<String>,
  /// The same body as bytes, from a v5 full-response frame. When this is set
  /// `body` is not: the point of the frame is that the body never becomes a
  /// base64 string on either side. `Bytes` rather than `Vec<u8>` because the
  /// WebSocket message already owns them refcounted, so this is a slice of
  /// what arrived rather than a copy of it.
  pub(crate) body_raw: Option<axum::body::Bytes>,
  /// HTTP trailers of a buffered response (e.g. `grpc-status` for gRPC).
  pub(crate) trailers: Option<Vec<(String, String)>>,
  /// For streamed responses: receiver of decoded body frames. The proxy
  /// handler turns this into a streaming HTTP body.
  pub(crate) stream_rx: Option<mpsc::Receiver<Result<BodyFrame, std::io::Error>>>,
  /// Client-side stage durations, from a buffered response or from the head
  /// of a streamed one (timing-aware clients; the streamed head reports every
  /// stage except the end of the backend body, which has not happened yet).
  pub(crate) timings: Option<crate::protocol::ClientTimings>,
}

/// High-resolution timeline of one proxied request: microsecond offsets from
/// t0 = the server first receiving the request. Client-side stages are
/// measured on the client's own monotonic clock and anchored here by
/// splitting the unaccounted tunnel transit evenly between the two
/// directions, clocks are never mixed, and the estimate is flagged.
#[derive(Serialize, Clone, Copy)]
pub(crate) struct RequestTimeline {
  /// A connected client was available (end of any wait-for-client). Measured;
  /// `None` when not captured. Sub-boundary of the pre-dispatch phase.
  pub(crate) client_ready_us: Option<u64>,
  /// Admitted past the server-wide concurrency limit. Measured.
  pub(crate) admitted_us: Option<u64>,
  /// A serving client was selected (routing done). Measured.
  pub(crate) selected_us: Option<u64>,
  /// The request left the server into the tunnel (queueing, routing, and
  /// admission all happen before this).
  pub(crate) dispatched_us: u64,
  /// Estimated: the client received the request.
  pub(crate) client_received_us: Option<u64>,
  /// Estimated anchor + measured client offset: backend request sent.
  pub(crate) backend_sent_us: Option<u64>,
  /// ... backend response headers arrived at the client.
  pub(crate) backend_first_byte_us: Option<u64>,
  /// ... backend body fully read by the client.
  pub(crate) backend_done_us: Option<u64>,
  /// ... the client handed the response to the tunnel.
  pub(crate) client_responded_us: Option<u64>,
  /// The server received the response from the tunnel (measured).
  pub(crate) response_received_us: u64,
  /// The response was handed to the visitor connection (measured).
  pub(crate) finished_us: u64,
  /// True when the client stages above are anchored estimates.
  pub(crate) estimated_anchor: bool,
}

impl RequestTimeline {
  /// Assembles the timeline from the server's own measurements and the
  /// client-reported stage durations (when present).
  pub(crate) fn assemble(
    dispatched_us: u64,
    response_received_us: u64,
    finished_us: u64,
    client: Option<crate::protocol::ClientTimings>,
  ) -> RequestTimeline {
    let anchored = client.map(|c| {
      // Whatever part of dispatch->response the client did not spend
      // processing is tunnel transit; split it evenly per direction.
      let round_trip = response_received_us.saturating_sub(dispatched_us);
      let transit = round_trip.saturating_sub(c.respond_us);
      let anchor = dispatched_us + transit / 2;
      (
        anchor,
        anchor + c.backend_sent_us,
        anchor + c.backend_first_byte_us,
        // Absent on the head of a streamed response, where the body has not
        // finished arriving at the client. Every other stage is measured the
        // same way it is for a buffered one, so the hole is exactly one row
        // wide rather than the whole waterfall.
        c.backend_done_us.map(|us| anchor + us),
        anchor + c.respond_us,
      )
    });
    RequestTimeline {
      client_ready_us: None,
      admitted_us: None,
      selected_us: None,
      dispatched_us,
      client_received_us: anchored.map(|a| a.0),
      backend_sent_us: anchored.map(|a| a.1),
      backend_first_byte_us: anchored.map(|a| a.2),
      backend_done_us: anchored.and_then(|a| a.3),
      client_responded_us: anchored.map(|a| a.4),
      response_received_us,
      finished_us,
      estimated_anchor: anchored.is_some(),
    }
  }
}

/// Default backlog at which the producer of a pumped stream is asked to
/// pause (`StreamPause`, `APERIO_STREAM_PAUSE_BYTES`): enough to ride out
/// short consumer hiccups without ever involving the client, small enough
/// that a slow visitor costs little memory.
pub(crate) const STREAM_PAUSE_BYTES: usize = 2 * 1024 * 1024;
/// Default backlog below which a paused producer is asked to resume
/// (`APERIO_STREAM_RESUME_BYTES`). Well under the pause mark so the pair
/// does not flap on every forwarded chunk.
pub(crate) const STREAM_RESUME_BYTES: usize = 512 * 1024;
/// Default hard per-stream backlog cap (`APERIO_STREAM_BACKLOG_LIMIT`): the
/// stream is dropped beyond it. Only reachable when the producer cannot be
/// paused (a pre-v3 client) or ignores the pause; a pausing client stops
/// with at most its frames already on the wire outstanding, far below the
/// gap between this and the pause mark.
pub(crate) const STREAM_BACKLOG_LIMIT: usize = 16 * 1024 * 1024;

/// The flow-control watermarks one pumped stream runs with, snapshotted from
/// the live config when the stream starts (a later settings change applies
/// to new streams only).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StreamLimits {
  /// Backlog bytes at which the producer is asked to pause.
  pub(crate) pause_bytes: usize,
  /// Backlog bytes under which a paused producer is asked to resume.
  pub(crate) resume_bytes: usize,
  /// Hard per-stream backlog cap in bytes.
  pub(crate) backlog_limit: usize,
  /// Bytes per second a consumer must take **while data is waiting for it**,
  /// or the stream is ended (`0` = no floor). See [`MIN_THROUGHPUT_WINDOW`].
  pub(crate) min_throughput: u64,
}

impl Default for StreamLimits {
  fn default() -> Self {
    StreamLimits {
      pause_bytes: STREAM_PAUSE_BYTES,
      resume_bytes: STREAM_RESUME_BYTES,
      backlog_limit: STREAM_BACKLOG_LIMIT,
      min_throughput: 0,
    }
  }
}

impl StreamLimits {
  /// Repairs an inconsistent trio so the mechanism cannot be configured into
  /// nonsense: the resume mark must sit below the pause mark (else pausing
  /// and resuming race on the same chunk) and the hard cap must sit above it
  /// (else every stream is cut before it can be paused). Out-of-order values
  /// are pulled back relative to `pause_bytes`, which is taken as the
  /// operator's intent.
  pub(crate) fn sanitized(
    pause_bytes: usize,
    resume_bytes: usize,
    backlog_limit: usize,
    min_throughput: u64,
  ) -> Self {
    let pause_bytes = pause_bytes.max(64 * 1024);
    let resume_bytes = if resume_bytes >= pause_bytes {
      pause_bytes / 4
    } else {
      resume_bytes
    };
    let backlog_limit = backlog_limit.max(pause_bytes.saturating_mul(2));
    StreamLimits {
      pause_bytes,
      resume_bytes,
      backlog_limit,
      min_throughput,
    }
  }
}

/// What one queued item costs against a pumped stream's byte backlog.
pub(crate) trait PumpCost {
  fn cost(&self) -> usize;
}
impl PumpCost for Result<BodyFrame, std::io::Error> {
  fn cost(&self) -> usize {
    match self {
      Ok(BodyFrame::Data(bytes)) => bytes.len(),
      _ => 0,
    }
  }
}
impl PumpCost for TcpConsumerMsg {
  fn cost(&self) -> usize {
    match self {
      TcpConsumerMsg::Data(bytes) => bytes.len(),
      TcpConsumerMsg::Close => 0,
    }
  }
}
impl PumpCost for WsStreamMessage {
  fn cost(&self) -> usize {
    match self {
      WsStreamMessage::Data(Message::Text(t)) => t.len(),
      WsStreamMessage::Data(Message::Binary(b)) => b.len(),
      _ => 0,
    }
  }
}

/// Producer-side flow control of one pumped stream: tracks the bytes queued
/// between the tunnel read loop and the consumer, and asks the producing
/// client to pause/resume around the watermarks (protocol v3). For older
/// clients only the hard backlog cap applies.
pub(crate) struct StreamFlow {
  /// The stream's id on the wire (request id or stream id).
  stream_id: String,
  /// The producing client's tunnel writer, for pause/resume messages.
  client_tx: mpsc::Sender<Message>,
  /// Whether the client announced protocol v3+ when the stream started.
  supports_pause: bool,
  /// The watermarks this stream runs with.
  limits: StreamLimits,
  /// Bytes enqueued but not yet forwarded to the consumer.
  backlog: std::sync::atomic::AtomicUsize,
  /// True while a `StreamPause` is outstanding.
  paused: AtomicBool,
}

impl StreamFlow {
  /// The floor this stream's consumer is held to, in bytes per second.
  pub(crate) fn min_throughput(&self) -> u64 {
    self.limits.min_throughput
  }

  pub(crate) fn stream_id(&self) -> &str {
    &self.stream_id
  }

  pub(crate) fn new(
    stream_id: String,
    client_tx: mpsc::Sender<Message>,
    supports_pause: bool,
    limits: StreamLimits,
  ) -> Self {
    StreamFlow {
      stream_id,
      client_tx,
      supports_pause,
      limits,
      backlog: std::sync::atomic::AtomicUsize::new(0),
      paused: AtomicBool::new(false),
    }
  }

  /// Sends `msg` on the client's tunnel without blocking; used from contexts
  /// (read loop, Drop) that must never wait on the writer.
  fn try_notify(&self, msg: &TunnelMessage) -> bool {
    match serde_json::to_string(msg) {
      Ok(json) => self.client_tx.try_send(Message::Text(json.into())).is_ok(),
      Err(_) => false,
    }
  }

  /// Accounts newly enqueued bytes and pauses the producer past the high
  /// watermark. Called on the tunnel read loop, so it never blocks.
  fn on_enqueued(&self, cost: usize) {
    let backlog = self.backlog.fetch_add(cost, Ordering::SeqCst) + cost;
    if self.supports_pause
      && backlog >= self.limits.pause_bytes
      && !self.paused.swap(true, Ordering::SeqCst)
    {
      // A full writer channel just means the pause is retried on the next
      // chunk; the flag is put back so that retry happens.
      let sent = self.try_notify(&TunnelMessage::StreamPause {
        id: self.stream_id.clone(),
      });
      if !sent {
        self.paused.store(false, Ordering::SeqCst);
      }
    }
  }

  /// Accounts bytes handed to the consumer and resumes a paused producer
  /// once the backlog has drained below the low watermark.
  fn on_forwarded(&self, cost: usize) {
    let backlog = self.backlog.fetch_sub(cost, Ordering::SeqCst) - cost;
    if backlog <= self.limits.resume_bytes
      && self.paused.load(Ordering::SeqCst)
      && self.try_notify(&TunnelMessage::StreamResume {
        id: self.stream_id.clone(),
      })
    {
      self.paused.store(false, Ordering::SeqCst);
    }
  }

  fn over_limit(&self, cost: usize) -> bool {
    self.backlog.load(Ordering::SeqCst) + cost > self.limits.backlog_limit
  }
}

impl Drop for StreamFlow {
  fn drop(&mut self) {
    // A stream torn down while its producer is paused must not leave that
    // producer waiting forever (the client also has its own resume-timeout
    // safety net for the case where this best-effort send is lost).
    if self.paused.load(Ordering::SeqCst) {
      let _ = self.try_notify(&TunnelMessage::StreamResume {
        id: self.stream_id.clone(),
      });
    }
  }
}

/// Why a pumped stream refused a chunk; both end the stream.
#[derive(Debug, PartialEq)]
pub(crate) enum PumpPushError {
  /// The pump ended: the consumer vanished or stalled beyond the timeout.
  ConsumerGone,
  /// The backlog cap was hit: the producer cannot be paused (or ignored it).
  BacklogFull,
}

/// The read loop's handle to a pumped stream: a non-blocking enqueue with
/// byte accounting and flow control.
pub(crate) struct PumpedSender<T> {
  feed: mpsc::UnboundedSender<T>,
  flow: Arc<StreamFlow>,
}

impl<T> Clone for PumpedSender<T> {
  fn clone(&self) -> Self {
    PumpedSender {
      feed: self.feed.clone(),
      flow: self.flow.clone(),
    }
  }
}

impl<T: PumpCost> PumpedSender<T> {
  /// Enqueues one item without ever blocking the tunnel read loop.
  pub(crate) fn push(&self, item: T) -> Result<(), PumpPushError> {
    let cost = item.cost();
    if self.flow.over_limit(cost) {
      return Err(PumpPushError::BacklogFull);
    }
    self
      .feed
      .send(item)
      .map_err(|_| PumpPushError::ConsumerGone)?;
    self.flow.on_enqueued(cost);
    Ok(())
  }
}

#[cfg(test)]
impl StreamFlow {
  /// A flow handle detached from any tunnel client, for tests: the dummy
  /// writer channel means a pause can never be delivered, so only the
  /// backlog cap and the stall timeout apply.
  pub(crate) fn detached(stream_id: &str) -> Self {
    Self::new(
      stream_id.to_string(),
      mpsc::channel(1).0,
      false,
      StreamLimits::default(),
    )
  }
}

/// Test convenience: a pumped stream with a detached flow and a long stall
/// timeout, for tests that only need the sender type.
#[cfg(test)]
pub(crate) fn test_pump<T: PumpCost + Send + 'static>(out: mpsc::Sender<T>) -> PumpedSender<T> {
  spawn_consumer_pump(out, Duration::from_secs(30), StreamFlow::detached("test"))
}

/// Puts a forwarding task between the tunnel read loop and one public
/// consumer, and returns the sender the read loop should hold.
///
/// A tunnel connection has a single read loop, shared by every request,
/// upgrade and raw stream on it, so it must never wait on one consumer. A
/// visitor that stops reading its download fills that stream's channel, and
/// waiting there stalls the other visitors' responses, the TCP data and even
/// the Ping handling of that whole tunnel; a stall outlasting
/// `client_down_threshold` drops the client from routing and 504s visitors
/// with nothing to do with it. The read loop therefore only ever pushes
/// into the returned queue, and this task does the waiting on its behalf,
/// giving up on a consumer stalled beyond `stall_timeout`.
///
/// Backpressure reaches the producer instead of piling up here: past
/// `STREAM_PAUSE_BYTES` of backlog the producing client is asked to stop
/// reading the stream's source until the backlog drains (protocol v3). A
/// producer that cannot be paused is cut off at `STREAM_BACKLOG_LIMIT`,
/// and a consumer that accepts nothing for `stall_timeout` ends the stream
/// exactly as the old blocking send's timeout did.
/// How long a consumer is measured over before the throughput floor applies.
///
/// Long enough that a browser pausing a video, or a phone changing network,
/// is not read as an attack; short enough that a stream held open on purpose
/// does not survive a shift.
pub(crate) const MIN_THROUGHPUT_WINDOW: Duration = Duration::from_secs(30);

/// Tracks whether a consumer is taking data fast enough to keep its stream.
///
/// ## What the existing defense already covers, and what it does not
///
/// The pump gives up when a single item cannot be handed over within the
/// gateway timeout, so a consumer that reads *nothing* is already ended. The
/// hole is the one in between: a reader that accepts one chunk every twenty
/// nine seconds resets that timeout forever and holds the stream, the
/// client-side `max_concurrent` slot behind it, and megabytes of server-side
/// buffer, for as long as it likes. Flow control is what made this possible,
/// by making the server well-behaved and therefore patient.
///
/// ## Why it only counts while data is waiting
///
/// A stream can be idle because the *backend* has nothing to send, which is
/// ordinary for server-sent events and long polling and is not the consumer's
/// fault. Measuring wall-clock throughput would end those. So the window only
/// accumulates while the pump has something to hand over: what is measured is
/// "data was ready and you did not take it", which is the actual accusation.
struct ThroughputGuard {
  floor: u64,
  window_started: Instant,
  bytes_this_window: u64,
  /// Time in this window during which the pump had an item to deliver.
  waiting: Duration,
}

impl ThroughputGuard {
  fn new(floor: u64) -> Self {
    ThroughputGuard {
      floor,
      window_started: Instant::now(),
      bytes_this_window: 0,
      waiting: Duration::ZERO,
    }
  }

  /// Records one delivery and says whether the stream should be ended.
  fn record(&mut self, bytes: u64, waited: Duration, now: Instant) -> bool {
    if self.floor == 0 {
      return false;
    }
    self.bytes_this_window += bytes;
    self.waiting += waited;
    if now.duration_since(self.window_started) < MIN_THROUGHPUT_WINDOW {
      return false;
    }
    // Judged against the time the consumer actually kept data waiting, not
    // against the window: a stream that was mostly idle because the backend
    // was quiet has a small denominator and passes on a small numerator.
    let owed = (self.floor as f64 * self.waiting.as_secs_f64()) as u64;
    let starved = self.bytes_this_window < owed;
    self.window_started = now;
    self.bytes_this_window = 0;
    self.waiting = Duration::ZERO;
    starved
  }
}

pub(crate) fn spawn_consumer_pump<T: PumpCost + Send + 'static>(
  out: mpsc::Sender<T>,
  stall_timeout: Duration,
  flow: StreamFlow,
) -> PumpedSender<T> {
  let flow = Arc::new(flow);
  let (feed_tx, mut feed_rx) = mpsc::unbounded_channel::<T>();
  let pump_flow = flow.clone();
  let floor = flow.min_throughput();
  let stream_id = flow.stream_id().to_string();
  tokio::spawn(async move {
    let mut guard = ThroughputGuard::new(floor);
    while let Some(item) = feed_rx.recv().await {
      let cost = item.cost();
      // A stalled or vanished consumer ends the pump; dropping `feed_rx`
      // closes the queue, so the read loop's next push reports the stream
      // as gone and removes it.
      let handing_over = Instant::now();
      if !matches!(
        tokio::time::timeout(stall_timeout, out.send(item)).await,
        Ok(Ok(()))
      ) {
        break;
      }
      pump_flow.on_forwarded(cost);
      // The time spent inside `send` above is time this item was ready and
      // the consumer had not taken it, which is exactly the denominator the
      // floor is judged against.
      let now = Instant::now();
      if guard.record(cost as u64, now.duration_since(handing_over), now) {
        tracing::warn!(
          "Stream {stream_id} ended: the consumer took less than {floor} bytes/second while data \
           was waiting for it"
        );
        break;
      }
    }
  });
  PumpedSender {
    feed: feed_tx,
    flow,
  }
}

/// Sender half of an in-flight streamed response body, kept so the tunnel
/// read loop can push chunks and so disconnect cleanup can drop it.
pub(crate) struct ResponseStreamHandle {
  pub(crate) tx: PumpedSender<Result<BodyFrame, std::io::Error>>,
  pub(crate) client_id: String,
}

/// Message relayed from the tunnel to a public TCP consumer WebSocket.
/// `Bytes` for the same zero-copy reason as `BodyFrame::Data`.
pub(crate) enum TcpConsumerMsg {
  Data(axum::body::Bytes),
  Close,
}

/// Handle to an active TCP tunnel stream (consumer side).
pub(crate) struct TcpStreamHandle {
  pub(crate) tx: PumpedSender<TcpConsumerMsg>,
  pub(crate) client_id: String,
}

/// Handle to an active UDP relay stream (consumer side). Unlike the pumped
/// TCP/WS/response streams, UDP is best-effort by contract: a congested
/// consumer drops datagrams instead of buffering or pausing the producer,
/// so the handle keeps a plain bounded sender.
pub(crate) struct UdpStreamHandle {
  pub(crate) tx: mpsc::Sender<TcpConsumerMsg>,
  pub(crate) client_id: String,
}

/// Structure tracking requests waiting for client execution.
pub(crate) struct PendingRequest {
  /// Oneshot channel sender to return client response to proxy handler thread.
  pub(crate) tx: oneshot::Sender<TunnelResponse>,
  /// Target client UUID.
  pub(crate) client_id: String,
}

/// Which of the two waiting-for-a-client maps a [`PendingGuard`] belongs to.
#[derive(Clone, Copy)]
pub(crate) enum PendingMap {
  Requests,
  Upgrades,
}

/// Removes a pending entry when the handler that registered it goes away.
///
/// A proxied request registers itself, dispatches, and awaits the answer. If
/// the visitor's connection drops, axum drops the handler future mid-await:
/// the timeout is dropped with it and nothing removes the entry. The only
/// sweep that ever reached it ran when the *serving client* disconnected, so
/// under a long-lived client the map grew with every visitor that hung up,
/// which is also an alert metric (`pending_requests`), so the leak reported
/// itself as load.
///
/// Drop cannot await, so it takes the fast path when it can: `try_lock`
/// succeeds in the ordinary case, and removing an id that the response path
/// already took is a lookup that finds nothing. Only genuine contention pays
/// for a task, and if there is no runtime left to spawn on the process is
/// ending anyway.
pub(crate) struct PendingGuard {
  state: Option<Arc<AppState>>,
  map: PendingMap,
  id: String,
}

impl PendingGuard {
  pub(crate) fn new(state: Arc<AppState>, map: PendingMap, id: String) -> PendingGuard {
    PendingGuard {
      state: Some(state),
      map,
      id,
    }
  }
}

impl Drop for PendingGuard {
  fn drop(&mut self) {
    let Some(state) = self.state.take() else {
      return;
    };
    let map = self.map;
    let id = std::mem::take(&mut self.id);
    fn slot(s: &AppState, map: PendingMap) -> &Mutex<HashMap<String, PendingRequest>> {
      match map {
        PendingMap::Requests => &s.pending_requests,
        PendingMap::Upgrades => &s.pending_upgrades,
      }
    }
    if let Ok(mut pending) = slot(&state, map).try_lock() {
      pending.remove(&id);
      return;
    }
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
      handle.spawn(async move {
        slot(&state, map).lock().await.remove(&id);
      });
    }
  }
}

/// Registered relay for a proxied public WebSocket: the sender that pushes
/// tunnel frames to the public side, tagged with the serving client's id so a
/// `WsData`/`WsClose` frame can be verified to come from the owning client.
pub(crate) struct WsStreamHandle {
  pub(crate) tx: PumpedSender<WsStreamMessage>,
  pub(crate) client_id: String,
}

/// A WebSocket frame relayed from the tunnel client, to be forwarded to the public WS.
pub(crate) enum WsStreamMessage {
  /// A data frame (text or binary) to forward to the public WebSocket.
  Data(Message),
  /// Close the public WebSocket stream.
  Close,
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
