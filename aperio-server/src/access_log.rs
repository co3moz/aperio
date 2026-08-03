use chrono::Local;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::state::{AppState, RequestLog};

/// Fixed-point accumulator behind `access_log_sample_rate`.
///
/// Sampling is deterministic rather than random: each request adds the rate to
/// an accumulator and a line is written whenever it crosses one. At 0.1 that
/// is exactly one line in ten, where a coin flip would give ten in a hundred
/// only on average, and the whole point of turning the volume down is knowing
/// what the volume now is.
static SAMPLE_ACCUMULATOR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Denominator of the fixed-point accumulator.
const SAMPLE_SCALE: u64 = 1_000_000;

/// Whether this request gets an access line.
///
/// A server error is always logged whatever the rate says: the line somebody
/// goes looking for is precisely the one that went wrong, and a sampled-away
/// 502 turns an incident into a mystery. Sampling covers the routine traffic
/// it was turned on for.
fn sampled_in(rate: f64, status: u16) -> bool {
  if rate >= 1.0 || status >= 500 {
    return true;
  }
  if rate <= 0.0 {
    return false;
  }
  let step = (rate * SAMPLE_SCALE as f64).round() as u64;
  let previous = SAMPLE_ACCUMULATOR.fetch_add(step, std::sync::atomic::Ordering::Relaxed);
  // Crossing a multiple of the scale is one whole line's worth of credit.
  previous % SAMPLE_SCALE + step >= SAMPLE_SCALE
}

/// Strips the query string from a URI to avoid logging sensitive data
/// (API keys, tokens, PII) that may be carried in query parameters.
pub(crate) fn sanitize_uri(uri: &str) -> &str {
  uri.split('?').next().unwrap_or(uri)
}

/// Writes one telemetry event into the bookkeeping structures. Called from
/// the collector task in production; called inline when the queue is full or
/// no collector runs (unit tests), so behavior is identical either way, only
/// who pays the lock waits differs.
pub(crate) async fn record_telemetry(state: &AppState, ev: crate::state::TelemetryEvent) {
  let host = ev.log.host.as_deref();
  let org = ev.log.org_id.as_deref();
  if ev.success {
    state
      .endpoint_stats
      .lock()
      .await
      .record(host, &ev.log.uri, ev.status, ev.duration_ms, org);
    state
      .route_trends
      .lock()
      .await
      .record(host, ev.status, org, ev.now_secs);
  }
  state
    .activity
    .lock()
    .await
    .record(org, !ev.success || ev.status >= 500, ev.now_secs);
  let mut logs = state.recent_logs.lock().await;
  if logs.len() >= 100 {
    logs.pop_front();
  }
  // Fan out to live dashboard SSE subscribers (ignored when there are none).
  let _ = state.traffic_tx.send(ev.log.clone());
  logs.push_back(ev.log);
}

/// Hands one event to the collector, or does the work inline when the queue
/// cannot take it: full means the collector is behind (rare, and inline is
/// exactly the old behavior), closed means no collector runs at all (unit
/// tests, which need the write to be visible the moment the call returns).
async fn submit_telemetry(state: &AppState, ev: crate::state::TelemetryEvent) {
  if let Err(err) = state.telemetry_tx.try_send(ev) {
    let ev = match err {
      tokio::sync::mpsc::error::TrySendError::Full(ev)
      | tokio::sync::mpsc::error::TrySendError::Closed(ev) => ev,
    };
    record_telemetry(state, ev).await;
  }
}

/// Runs the collector for the life of the process: the state it holds keeps
/// a sender, so the channel never closes and the loop never ends, which is
/// the intended shape for a process-lifetime service.
pub(crate) fn spawn_telemetry_collector(
  state: Arc<AppState>,
  mut rx: mpsc::Receiver<crate::state::TelemetryEvent>,
) {
  tokio::spawn(async move {
    while let Some(ev) = rx.recv().await {
      record_telemetry(&state, ev).await;
    }
  });
}

/// What the request path hands the access-log writer task.
pub(crate) enum AccessLogCmd {
  /// One rendered JSON line to append.
  Line(String),
  /// Rewrite the file, dropping every line the filter matches; the writer
  /// owns the only handle, so a rewrite here can never race an append. The
  /// reply is how many lines were dropped.
  Rewrite {
    drop_line: Box<dyn Fn(&str) -> bool + Send>,
    reply: tokio::sync::oneshot::Sender<usize>,
  },
}

/// Lines dropped because the writer's queue was full, counted so the loss is
/// visible; warned about once per thousand rather than once per line.
static DROPPED_LINES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Spawns the one task that touches the access-log file, and returns its
/// queue. Everything the request path used to do inline, the JSON render
/// aside, moves behind it: a synchronous `write` on a slow disk used to block
/// the tokio worker mid-request, and the mutex around it serialized every
/// request in the process on one file descriptor.
///
/// The trade this makes at shutdown: lines still queued at the instant the
/// process exits are lost, where the old synchronous write was not. The
/// window is the writer's lag behind the queue, normally microseconds, and
/// losing a telemetry line beats a request waiting on its own log, which is
/// the same judgement the full-queue drop below makes.
pub(crate) fn spawn_writer(path: String, file: std::fs::File) -> mpsc::Sender<AccessLogCmd> {
  let (tx, mut rx) = mpsc::channel::<AccessLogCmd>(4096);
  tokio::spawn(async move {
    let mut file = file;
    while let Some(cmd) = rx.recv().await {
      match cmd {
        AccessLogCmd::Line(line) => {
          use std::io::Write;
          let _ = writeln!(file, "{}", line);
        }
        AccessLogCmd::Rewrite { drop_line, reply } => {
          let removed = rewrite_file(&path, &mut file, drop_line.as_ref());
          let _ = reply.send(removed);
        }
      }
    }
  });
  tx
}

/// The rewrite itself: filter the lines, replace the file atomically, reopen
/// the append handle past the truncated content. Runs on the writer task
/// only, which is what makes it safe without a lock.
fn rewrite_file(path: &str, file: &mut std::fs::File, drop_line: &dyn Fn(&str) -> bool) -> usize {
  let Ok(raw) = std::fs::read_to_string(path) else {
    return 0;
  };
  let mut kept = String::with_capacity(raw.len());
  let mut removed = 0usize;
  for line in raw.lines() {
    if drop_line(line) {
      removed += 1;
    } else {
      kept.push_str(line);
      kept.push('\n');
    }
  }
  if removed > 0 {
    if crate::store::atomic_write(std::path::Path::new(path), kept.as_bytes()).is_err() {
      warn!("Failed to rewrite the access log {}", path);
      return 0;
    }
    match std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(path)
    {
      Ok(f) => *file = f,
      Err(e) => warn!("Failed to reopen the access log {}: {}", path, e),
    }
  }
  removed
}

/// Asks the writer to rewrite the file, dropping matching lines. `None` when
/// no access log is configured or the writer is gone.
pub(crate) async fn rewrite(
  state: &AppState,
  drop_line: Box<dyn Fn(&str) -> bool + Send>,
) -> Option<usize> {
  let tx = state.access_log.as_ref()?;
  let (reply, rx) = tokio::sync::oneshot::channel();
  tx.send(AccessLogCmd::Rewrite { drop_line, reply })
    .await
    .ok()?;
  rx.await.ok()
}

/// The relay log's way in, so a connection-level line lands in the same file
/// as a request line and one pipeline ingests both.
pub(crate) fn append_relay_line(state: &AppState, entry: &serde_json::Value) {
  append_access_line(state, entry);
}

/// Whether this relay connection gets a line, under the same rate as an HTTP
/// request. Separate accumulator so a busy HTTP surface cannot starve the
/// relay log of its share, and vice versa: they are different populations and
/// one in ten of each is what the setting promises.
pub(crate) fn relay_sampled_in(rate: f64) -> bool {
  if rate >= 1.0 {
    return true;
  }
  if rate <= 0.0 {
    return false;
  }
  static RELAY_ACCUMULATOR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
  let step = (rate * SAMPLE_SCALE as f64).round() as u64;
  let previous = RELAY_ACCUMULATOR.fetch_add(step, std::sync::atomic::Ordering::Relaxed);
  previous % SAMPLE_SCALE + step >= SAMPLE_SCALE
}

fn append_access_line(state: &AppState, entry: &serde_json::Value) {
  let Some(tx) = &state.access_log else {
    return;
  };
  // try_send, never send: a full queue means the disk is not keeping up, and
  // the request being served must not wait for it. The dropped line is
  // counted rather than silent.
  if tx.try_send(AccessLogCmd::Line(entry.to_string())).is_err() {
    let dropped = DROPPED_LINES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if dropped % 1000 == 1 {
      warn!(
        "Access log writer is not keeping up; {} line(s) dropped so far",
        dropped
      );
    }
  }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn log_request_success(
  state: &Arc<AppState>,
  id: String,
  method: &str,
  uri: &str,
  status: u16,
  duration: Duration,
  host: Option<&str>,
  client_id: Option<&str>,
  token: Option<&str>,
  org: Option<String>,
) {
  state.duration_histogram.observe(duration);
  let safe_uri = sanitize_uri(uri);
  // One clock read for this request. `Local::now()` resolves the timezone on
  // every call, and this function used to do it twice: once for the dashboard
  // entry, once for the access-log line.
  let now = Local::now();
  // Everything the dashboard's bookkeeping wants, in one event for the
  // collector task: the endpoint report, the sparkline trend, the activity
  // ring and the recent-log entry used to be four sequential global lock
  // waits on the request's own back.
  //
  // RFC3339 with the UTC offset: the dashboard runs in the visitor's
  // browser, which may be in a different timezone than the server, a naive
  // local string would be re-interpreted in the browser's zone and drift.
  submit_telemetry(
    state,
    crate::state::TelemetryEvent {
      log: RequestLog {
        id: id.clone(),
        timestamp: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        method: method.to_string(),
        uri: safe_uri.to_string(),
        status: Some(status),
        duration_ms: duration.as_millis(),
        error: None,
        host: host.map(str::to_string),
        org_id: org,
      },
      status,
      duration_ms: duration.as_millis() as u64,
      success: true,
      now_secs: crate::store::tokens::now_secs(),
    },
  )
  .await;
  // Sampling is decided after the telemetry above, deliberately: the
  // dashboard's counters, the histogram and the rate charts stay exact, and
  // only the per-request line, the thing there can be a million of, is
  // thinned out.
  if !sampled_in(state.config().access_log_sample_rate, status) {
    return;
  }
  // Structured access event: with the JSON log format every field below
  // becomes a top-level key, directly usable by log pipelines. Skipped
  // wholesale when `access_events` is off, which is not the same as lowering
  // the log level: this silences one event per request and leaves warnings
  // and errors where they are.
  if state.config().access_events {
    info!(
      target: "aperio_access",
      request_id = %id,
      method,
      uri = %safe_uri,
      status,
      duration_ms = duration.as_millis() as u64,
      host = host.unwrap_or(""),
      client_id = client_id.unwrap_or(""),
      token = token.unwrap_or("master"),
      "proxy success"
    );
  }
  // Built only when there is somewhere to put it. This ran on every request
  // whether or not an access log was configured: ten fields, a timestamp and
  // a serde_json::Value tree, allocated and dropped for nothing.
  if state.access_log.is_none() {
    return;
  }
  append_access_line(
    state,
    &serde_json::json!({
      "ts": now.to_rfc3339(),
      "request_id": id,
      "method": method,
      "uri": safe_uri,
      "status": status,
      "duration_ms": duration.as_millis() as u64,
      "host": host,
      "client_id": client_id,
      "token": token.unwrap_or("master"),
      "error": null,
    }),
  );
}

pub(crate) async fn log_request_failure(
  state: &Arc<AppState>,
  method: &str,
  uri: &str,
  status: u16,
  duration: Duration,
  error: Option<&str>,
  org: Option<String>,
) {
  state.duration_histogram.observe(duration);
  let safe_uri = sanitize_uri(uri);
  let id = uuid::Uuid::new_v4().to_string();
  let now = Local::now();
  // A refusal is traffic too, and the chart that leaves it out shows a quiet
  // server at the moment it is turning everything away; `success: false`
  // makes the collector count it as exactly that.
  submit_telemetry(
    state,
    crate::state::TelemetryEvent {
      log: RequestLog {
        id: id.clone(),
        timestamp: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        method: method.to_string(),
        uri: safe_uri.to_string(),
        status: Some(status),
        duration_ms: duration.as_millis(),
        error: error.map(|s| s.to_string()),
        host: None,
        org_id: org,
      },
      status,
      duration_ms: duration.as_millis() as u64,
      success: false,
      now_secs: crate::store::tokens::now_secs(),
    },
  )
  .await;
  warn!(
    target: "aperio_access",
    request_id = %id,
    method,
    uri = %safe_uri,
    status,
    duration_ms = duration.as_millis() as u64,
    error = error.unwrap_or(""),
    "proxy failure"
  );
  if state.access_log.is_none() {
    return;
  }
  append_access_line(
    state,
    &serde_json::json!({
      "ts": now.to_rfc3339(),
      "request_id": id,
      "method": method,
      "uri": safe_uri,
      "status": status,
      "duration_ms": duration.as_millis() as u64,
      "host": null,
      "client_id": null,
      "token": null,
      "error": error,
    }),
  );
}

#[cfg(test)]
#[path = "access_log_tests.rs"]
mod tests;
