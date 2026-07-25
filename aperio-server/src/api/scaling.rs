//! Autoscaling API: inspect and remove the records clients have armed.
//!
//! Records are created by clients announcing a `scaling:` block, never through
//! this API: the declaration belongs next to the service it scales. What an
//! operator needs here is visibility (what is armed, what is it doing) and a
//! way to disarm something, which is what this exposes.

use axum::{
  Json,
  extract::{ConnectInfo, Path, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use std::net::SocketAddr;
use std::sync::Arc;

use aperio_config::ScalingDecl;

use crate::routing::extract_client_ip;
use crate::state::AppState;
use crate::store::scaling::{
  DEFAULT_COLD_START_SECS, DEFAULT_COOLDOWN_SECS, DEFAULT_TARGET_UTILIZATION, DEFAULT_WINDOW_SECS,
  MAX_COLD_START_SECS, ScalingRecord,
};

/// Parses a human duration (`45s`, `2m`, `1h`) or a bare number of seconds.
/// Mirrors the client's own parser so a value that is valid in `aperio.yaml`
/// is valid on the wire.
pub(crate) fn parse_duration_secs(raw: &str) -> Result<u64, String> {
  let value = raw.trim().to_ascii_lowercase();
  if value.is_empty() {
    return Err("empty duration".to_string());
  }
  let (number, multiplier) = match value.chars().last() {
    Some('s') => (&value[..value.len() - 1], 1u64),
    Some('m') => (&value[..value.len() - 1], 60),
    Some('h') => (&value[..value.len() - 1], 3_600),
    Some('d') => (&value[..value.len() - 1], 86_400),
    _ => (value.as_str(), 1),
  };
  number
    .parse::<u64>()
    .map_err(|_| format!("invalid duration '{raw}'"))?
    .checked_mul(multiplier)
    .ok_or_else(|| format!("duration '{raw}' is too large"))
}

/// Validates a client-announced declaration and turns it into a record for one
/// bind. Every bound is applied here, so a malformed or hostile declaration is
/// rejected before it can ever be persisted or acted on.
pub(crate) fn record_from_decl(
  decl: &ScalingDecl,
  org_id: Option<String>,
  hostname: &str,
  path: Option<&str>,
) -> Result<ScalingRecord, String> {
  let url = decl.url.trim();
  if url.is_empty() {
    return Err("scaling.url is required".to_string());
  }
  // Parsed here as well as at call time: a URL that can never be called is
  // not worth persisting.
  let parsed = url::Url::parse(url).map_err(|e| format!("scaling.url is not a URL: {e}"))?;
  if !matches!(parsed.scheme(), "http" | "https") {
    return Err(format!(
      "scaling.url must be http(s), got {}",
      parsed.scheme()
    ));
  }
  let cold_start_secs = match decl.cold_start.as_deref() {
    Some(raw) => parse_duration_secs(raw)?.min(MAX_COLD_START_SECS),
    None => DEFAULT_COLD_START_SECS,
  };
  let window_secs = match decl.window.as_deref() {
    Some(raw) => parse_duration_secs(raw)?.max(1),
    None => DEFAULT_WINDOW_SECS,
  };
  let cooldown_secs = match decl.cooldown.as_deref() {
    Some(raw) => parse_duration_secs(raw)?.max(1),
    None => DEFAULT_COOLDOWN_SECS,
  };
  let target_utilization = match decl.target_utilization {
    Some(v) if v.is_finite() && v > 0.0 && v <= 1.0 => v,
    Some(v) => {
      return Err(format!(
        "scaling.target_utilization must be in (0, 1], got {v}"
      ));
    }
    None => DEFAULT_TARGET_UTILIZATION,
  };
  if decl.max > 0 && decl.max < decl.min {
    return Err(format!(
      "scaling.max ({}) is below scaling.min ({})",
      decl.max, decl.min
    ));
  }
  let mut record = ScalingRecord {
    id: ScalingRecord::key(org_id.as_deref(), hostname, path),
    org_id,
    hostname: hostname.to_string(),
    path: path.map(str::to_string),
    url: url.to_string(),
    secret: decl.secret.clone().filter(|s| !s.trim().is_empty()),
    min: decl.min,
    max: decl.max,
    cold_start_secs,
    target_utilization,
    window_secs,
    cooldown_secs,
    owners: Vec::new(),
    config_hash: String::new(),
    created_at: 0,
    last_seen: 0,
  };
  record.config_hash = record.compute_hash();
  Ok(record)
}

/// Renders a record for the API. The secret is never included, only whether
/// one is set, matching how webhook signing secrets are surfaced.
async fn render(state: &Arc<AppState>, record: &ScalingRecord) -> serde_json::Value {
  let capacity = crate::scaling::measure(state, &record.hostname, record.path.as_deref()).await;
  let disarmed = state.scaling_runtime.lock().await.is_disarmed(&record.id);
  serde_json::json!({
    "id": record.id,
    "hostname": record.hostname,
    "path": record.path,
    "url": record.url,
    "authenticated": record.secret.is_some(),
    "min": record.min,
    "max": record.max,
    "cold_start_secs": record.cold_start_secs,
    "target_utilization": record.target_utilization,
    "window_secs": record.window_secs,
    "cooldown_secs": record.cooldown_secs,
    "owners": record.owners.len(),
    "created_at": record.created_at,
    "last_seen": record.last_seen,
    "disarmed": disarmed,
    "instances": capacity.instances,
    "capacity": capacity.capacity,
    "inflight": capacity.inflight,
    "utilization": capacity.utilization,
  })
}

/// Lists the autoscaling records of the caller's organization, each with the
/// live state of its pool.
#[utoipa::path(get, path = "/aperio/api/scaling", tag = "dashboard",
  description = "Autoscaling records armed for this organization, with live pool capacity and utilization.",
  responses((status = 200, description = "Armed records", body = serde_json::Value)))]
pub(crate) async fn scaling_list_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  let org = crate::auth::effective_org(&state, &headers).await;
  let records: Vec<ScalingRecord> = state
    .scaling_store
    .lock()
    .await
    .list()
    .iter()
    .filter(|r| r.org_id == org)
    .cloned()
    .collect();
  let mut out = Vec::with_capacity(records.len());
  for record in &records {
    out.push(render(&state, record).await);
  }
  Json(out).into_response()
}

/// Disarms one record. The client will re-arm it on its next heartbeat if it
/// is still running and still declares the block, which is the intended way to
/// undo an accidental delete.
#[utoipa::path(delete, path = "/aperio/api/scaling/{id}", tag = "dashboard",
  description = "Disarms an autoscaling record. A running client that still declares the block re-arms it on its next heartbeat.",
  params(("id" = String, Path, description = "Record id")),
  responses((status = 200, description = "Disarmed"), (status = 404, description = "Unknown record")))]
pub(crate) async fn scaling_delete_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
) -> Response {
  let org = crate::auth::effective_org(&state, &headers).await;
  // Isolation: an id outside the caller's org is indistinguishable from an
  // unknown one, so a record's existence never leaks across organizations.
  let owned = state
    .scaling_store
    .lock()
    .await
    .list()
    .iter()
    .any(|r| r.id == id && r.org_id == org);
  // `find` keys on the bind; here the caller supplies the id directly, so the
  // scan above is the equivalent lookup with the org check folded in.
  if !owned {
    return (StatusCode::NOT_FOUND, "unknown scaling record").into_response();
  }
  state.scaling_store.lock().await.delete(&id);
  state.scaling_runtime.lock().await.forget(&id);
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  state
    .audit_session("scaling_disarmed", &headers, &actor_ip, &format!("id={id}"))
    .await;
  Json(serde_json::json!({"status": "ok"})).into_response()
}

#[cfg(test)]
#[path = "scaling_tests.rs"]
mod tests;
