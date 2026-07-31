use chrono::Local;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

use crate::state::{AppState, RequestLog};

/// Strips the query string from a URI to avoid logging sensitive data
/// (API keys, tokens, PII) that may be carried in query parameters.
pub(crate) fn sanitize_uri(uri: &str) -> &str {
  uri.split('?').next().unwrap_or(uri)
}

/// Appends one JSON line to the access log file when APERIO_ACCESS_LOG is
/// configured. The same data is always emitted as a structured tracing event.
fn append_access_line(state: &AppState, entry: &serde_json::Value) {
  if let Some(file) = &state.access_log {
    use std::io::Write;
    if let Ok(mut f) = file.lock() {
      let _ = writeln!(f, "{}", entry);
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
  // Feed the slowest-endpoints report (recent-window latency per host|path).
  state.endpoint_stats.lock().await.record(
    host,
    safe_uri,
    status,
    duration.as_millis() as u64,
    org.as_deref(),
  );
  // Feed the per-route status trend (dashboard sparklines) and the volume
  // ring behind the activity chart's long view.
  let now_secs = crate::store::tokens::now_secs();
  state
    .route_trends
    .lock()
    .await
    .record(host, status, org.as_deref(), now_secs);
  state
    .activity
    .lock()
    .await
    .record(org.as_deref(), status >= 500, now_secs);
  // One clock read for this request. `Local::now()` resolves the timezone on
  // every call, and this function used to do it twice: once for the dashboard
  // entry, once for the access-log line.
  let now = Local::now();
  {
    let mut logs = state.recent_logs.lock().await;
    if logs.len() >= 100 {
      logs.pop_front();
    }
    // RFC3339 with the UTC offset: the dashboard runs in the visitor's browser,
    // which may be in a different timezone than the server, a naive local
    // string would be re-interpreted in the browser's zone and drift.
    let timestamp = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    let entry = RequestLog {
      id: id.clone(),
      timestamp,
      method: method.to_string(),
      uri: safe_uri.to_string(),
      status: Some(status),
      duration_ms: duration.as_millis(),
      error: None,
      host: host.map(str::to_string),
      org_id: org,
    };
    // Fan out to live dashboard SSE subscribers (ignored when there are none).
    let _ = state.traffic_tx.send(entry.clone());
    logs.push_back(entry);
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
  // A refusal is traffic too, and the chart that leaves it out shows a quiet
  // server at the moment it is turning everything away.
  state
    .activity
    .lock()
    .await
    .record(org.as_deref(), true, crate::store::tokens::now_secs());
  let now = Local::now();
  {
    let mut logs = state.recent_logs.lock().await;
    if logs.len() >= 100 {
      logs.pop_front();
    }
    // RFC3339 with the UTC offset: the dashboard runs in the visitor's browser,
    // which may be in a different timezone than the server, a naive local
    // string would be re-interpreted in the browser's zone and drift.
    let timestamp = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    let entry = RequestLog {
      id: id.clone(),
      timestamp,
      method: method.to_string(),
      uri: safe_uri.to_string(),
      status: Some(status),
      duration_ms: duration.as_millis(),
      error: error.map(|s| s.to_string()),
      host: None,
      org_id: org,
    };
    // Fan out to live dashboard SSE subscribers (ignored when there are none).
    let _ = state.traffic_tx.send(entry.clone());
    logs.push_back(entry);
  }
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
