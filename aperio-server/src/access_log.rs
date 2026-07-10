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
) {
  state.duration_histogram.observe(duration);
  let safe_uri = sanitize_uri(uri);
  {
    let mut logs = state.recent_logs.lock().await;
    if logs.len() >= 100 {
      logs.pop_front();
    }
    // RFC3339 with the UTC offset: the dashboard runs in the visitor's browser,
    // which may be in a different timezone than the server — a naive local
    // string would be re-interpreted in the browser's zone and drift.
    let timestamp = Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    let entry = RequestLog {
      id: id.clone(),
      timestamp,
      method: method.to_string(),
      uri: safe_uri.to_string(),
      status: Some(status),
      duration_ms: duration.as_millis(),
      error: None,
    };
    // Fan out to live dashboard SSE subscribers (ignored when there are none).
    let _ = state.traffic_tx.send(entry.clone());
    logs.push_back(entry);
  }
  // Structured access event: with the JSON log format every field below
  // becomes a top-level key, directly usable by log pipelines.
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
  append_access_line(
    state,
    &serde_json::json!({
      "ts": Local::now().to_rfc3339(),
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
) {
  state.duration_histogram.observe(duration);
  let safe_uri = sanitize_uri(uri);
  let id = uuid::Uuid::new_v4().to_string();
  {
    let mut logs = state.recent_logs.lock().await;
    if logs.len() >= 100 {
      logs.pop_front();
    }
    // RFC3339 with the UTC offset: the dashboard runs in the visitor's browser,
    // which may be in a different timezone than the server — a naive local
    // string would be re-interpreted in the browser's zone and drift.
    let timestamp = Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    let entry = RequestLog {
      id: id.clone(),
      timestamp,
      method: method.to_string(),
      uri: safe_uri.to_string(),
      status: Some(status),
      duration_ms: duration.as_millis(),
      error: error.map(|s| s.to_string()),
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
  append_access_line(
    state,
    &serde_json::json!({
      "ts": Local::now().to_rfc3339(),
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
