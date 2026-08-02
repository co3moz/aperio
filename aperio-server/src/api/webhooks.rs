use axum::{
  Json,
  extract::{ConnectInfo, Path, Query, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::routing::extract_client_ip;
use crate::state::AppState;
use crate::store::audit::{self};

/// Parses the shared audit query predicates. Timestamps accept unix seconds
/// or a `YYYY-MM-DD` day (interpreted as UTC midnight, and for `to` as the end
/// of that day so a single-day range includes it).
fn audit_filter_from(params: &std::collections::HashMap<String, String>) -> audit::AuditFilter {
  let day_to_ts = |raw: &str, end_of_day: bool| -> Option<u64> {
    if let Ok(secs) = raw.parse::<u64>() {
      return Some(secs);
    }
    let date = chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").ok()?;
    let time = if end_of_day {
      chrono::NaiveTime::from_hms_opt(23, 59, 59)?
    } else {
      chrono::NaiveTime::MIN
    };
    Some(date.and_time(time).and_utc().timestamp().max(0) as u64)
  };
  let non_empty = |key: &str| {
    params
      .get(key)
      .map(|v| v.trim().to_string())
      .filter(|v| !v.is_empty())
  };
  audit::AuditFilter {
    event: non_empty("event"),
    actor: non_empty("actor"),
    contains: non_empty("q"),
    from_ts: non_empty("from").and_then(|v| day_to_ts(&v, false)),
    to_ts: non_empty("to").and_then(|v| day_to_ts(&v, true)),
  }
}

/// Applies the caller's organization fence to a result set. Kept separate
/// from the search filter on purpose: isolation is not a predicate the caller
/// gets to relax.
fn scoped_to_org(events: Vec<audit::AuditEvent>, org: &Option<String>) -> Vec<audit::AuditEvent> {
  events.into_iter().filter(|e| &e.org_id == org).collect()
}

/// Returns audit events (dashboard), scoped to the caller's effective
/// organization: a named user sees only their org's events, and the master
/// super-admin sees the events of whichever org is selected on their session
/// (`None` = the implicit master org, which also owns server-global events).
///
/// With no query parameters this answers from the in-memory ring, exactly as
/// it always did, so the dashboard's polling view costs nothing extra. Any
/// filter (`event`, `actor`, `q`, `from`, `to`) searches the durable log
/// instead: the active file and every rotated generation, which is the only
/// way to reach beyond the last 200 events.
#[utoipa::path(get, path = "/aperio/api/audit", tag = "dashboard",
  description = "Audit events for the caller's organization. Unfiltered: the recent ring. Filtered: a search over the durable log (audit.jsonl and its rotated generations).",
  params(
    ("event" = Option<String>, Query, description = "Exact event kind, e.g. login_success"),
    ("actor" = Option<String>, Query, description = "Exact actor name"),
    ("q" = Option<String>, Query, description = "Case-insensitive substring of the details, event or actor"),
    ("from" = Option<String>, Query, description = "Inclusive start: unix seconds or YYYY-MM-DD (UTC)"),
    ("to" = Option<String>, Query, description = "Inclusive end: unix seconds or YYYY-MM-DD (UTC, end of day)"),
    ("limit" = Option<usize>, Query, description = "Maximum events returned (default 200, max 5000)")),
  responses((status = 200, description = "Audit events, newest first", body = Vec<audit::AuditEvent>)))]
pub(crate) async fn audit_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Vec<audit::AuditEvent>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let filter = audit_filter_from(&params);
  let limit = params
    .get("limit")
    .and_then(|v| v.parse::<usize>().ok())
    .unwrap_or(200)
    .clamp(1, 5000);
  let log = state.audit.lock().await;
  let events = if filter.is_empty() && !params.contains_key("limit") {
    scoped_to_org(log.recent(), &org)
  } else {
    // The org fence is applied to what the search returns, and the search is
    // asked for more than the limit so that fencing cannot silently shrink a
    // full page; the page is then trimmed to the limit.
    let mut found = scoped_to_org(
      log.search(&filter, limit.saturating_mul(4).min(20_000)),
      &org,
    );
    found.truncate(limit);
    found
  };
  Json(events)
}

/// Exports the same query as CSV, for the auditor who wants the rows in a
/// spreadsheet rather than a browser. Always searches the durable log, since
/// an export of only the last 200 events would be a misleading artifact.
#[utoipa::path(get, path = "/aperio/api/export/audit.csv", tag = "dashboard",
  description = "Audit events matching the same filters as /aperio/api/audit, as CSV, from the durable log.",
  responses((status = 200, description = "CSV export")))]
pub(crate) async fn audit_csv_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
  if crate::auth::dashboard_role(&state, &headers)
    .await
    .is_none()
  {
    return (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
  }
  let org = crate::auth::effective_org(&state, &headers).await;
  let filter = audit_filter_from(&params);
  let limit = params
    .get("limit")
    .and_then(|v| v.parse::<usize>().ok())
    .unwrap_or(5000)
    .clamp(1, 50_000);
  let events = {
    let log = state.audit.lock().await;
    let mut found = scoped_to_org(
      log.search(&filter, limit.saturating_mul(4).min(200_000)),
      &org,
    );
    found.truncate(limit);
    found
  };

  let field = |s: &str| {
    if s.contains([',', '"', '\n', '\r']) {
      format!("\"{}\"", s.replace('"', "\"\""))
    } else {
      s.to_string()
    }
  };
  let mut csv = String::from("timestamp,ts,event,actor,actor_ip,org,details\n");
  for e in &events {
    csv.push_str(&format!(
      "{},{},{},{},{},{},{}\n",
      field(&e.timestamp),
      e.ts,
      field(&e.event),
      field(&e.actor),
      field(&e.actor_ip),
      field(e.org_id.as_deref().unwrap_or("")),
      field(&e.details),
    ));
  }
  (
    StatusCode::OK,
    [
      (
        axum::http::header::CONTENT_TYPE,
        "text/csv; charset=utf-8".to_string(),
      ),
      (
        axum::http::header::CONTENT_DISPOSITION,
        "attachment; filename=\"aperio-audit.csv\"".to_string(),
      ),
    ],
    csv,
  )
    .into_response()
}

/// Verifies the tamper-evident hash chain of the audit log (active file plus
/// rotated generations). Returns `{ok, broken: [{file, line}]}`. Not org-scoped
///, the audit files are server-global, so this is an admin-only integrity
/// check surfaced from the dashboard.
#[utoipa::path(get, path = "/aperio/api/audit/verify", tag = "dashboard",
  description = "Verifies the audit log hash chain across all files; reports any broken line (master admin only).",
  responses(
    (status = 200, description = "Chain verification result", body = serde_json::Value),
    (status = 403, description = "Not the master administrator")))]
pub(crate) async fn audit_verify_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Response {
  // The audit files span every organization, so this answers for the whole
  // server and cannot be scoped to a tenant. It is restricted to the master
  // administrator, as its own description always claimed: any viewer, of any
  // organization, could previously call it and learn whether the server-wide
  // log had been tampered with.
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let broken = state.audit.lock().await.verify();
  let broken_json: Vec<serde_json::Value> = broken
    .into_iter()
    .map(|(file, line)| serde_json::json!({"file": file, "line": line}))
    .collect();
  Json(serde_json::json!({
    "ok": broken_json.is_empty(),
    "broken": broken_json,
  }))
  .into_response()
}

/// Payload for creating a webhook definition.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct WebhookCreateRequest {
  pub(crate) name: String,
  pub(crate) url: String,
  /// Subscribed events; `["*"]` (or empty) = all events.
  #[serde(default)]
  pub(crate) events: Vec<String>,
  /// Optional HMAC signing secret; deliveries then carry
  /// `X-Aperio-Signature` / `X-Aperio-Timestamp` headers.
  #[serde(default)]
  pub(crate) secret: Option<String>,
  /// Delivery payload format: `generic` (default), `slack`, `discord`, or `teams`.
  #[serde(default)]
  pub(crate) format: Option<String>,
}

/// Lists webhook definitions. The signing secret itself is never returned,
/// only whether one is set.
#[utoipa::path(get, path = "/aperio/api/webhooks", tag = "webhooks",
  description = "Lists webhook definitions (signing secrets are never exposed, only a signed flag).",
  responses((status = 200, description = "Webhook definitions", body = serde_json::Value)))]
pub(crate) async fn webhooks_list_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Json<Vec<serde_json::Value>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let hooks = state.webhook_store.lock().await.list().to_vec();
  Json(
    hooks
      .into_iter()
      .filter(|w| w.org_id == org)
      .map(|w| {
        serde_json::json!({
          "id": w.id,
          "name": w.name,
          "url": w.url,
          "events": w.events,
          "enabled": w.enabled,
          "created_at": w.created_at,
          "format": w.format.as_str(),
          "signed": w.secret.is_some(),
        })
      })
      .collect(),
  )
}

/// Creates a webhook definition. Only http/https URLs are accepted.
#[utoipa::path(post, path = "/aperio/api/webhooks", tag = "webhooks",
  description = "Creates a webhook; an optional HMAC signing secret (16-128 chars) enables signed deliveries.",
  request_body = WebhookCreateRequest,
  responses((status = 200, description = "Created webhook", body = serde_json::Value), (status = 400, description = "Invalid URL/secret")))]
pub(crate) async fn webhooks_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<WebhookCreateRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  let name = payload.name.trim().to_string();
  if name.is_empty() || name.len() > 64 {
    return (
      StatusCode::BAD_REQUEST,
      "Webhook name must be 1-64 characters",
    )
      .into_response();
  }
  let url = payload.url.trim().to_string();
  if !(url.starts_with("http://") || url.starts_with("https://")) {
    return (StatusCode::BAD_REQUEST, "Webhook URL must be http(s)").into_response();
  }
  // Optional outbound policy: refuse a destination the server would decline
  // to deliver to anyway, so the operator hears it here instead of finding
  // refused entries in the delivery log. Deliveries re-check at send time,
  // covering webhooks stored before the policy existed.
  if let Err(reason) = state.config().outbound_policy.check(&url).await {
    return (
      StatusCode::BAD_REQUEST,
      format!("Webhook URL refused: {reason}"),
    )
      .into_response();
  }
  let events: Vec<String> = payload
    .events
    .iter()
    .map(|e| e.trim().to_string())
    .filter(|e| !e.is_empty())
    .collect();
  let secret = payload
    .secret
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_string);
  if secret
    .as_deref()
    .is_some_and(|s| s.len() < 16 || s.len() > 128)
  {
    return (
      StatusCode::BAD_REQUEST,
      "Webhook signing secret must be 16-128 characters",
    )
      .into_response();
  }
  let Some(format) =
    crate::store::webhooks::WebhookFormat::parse(payload.format.as_deref().unwrap_or(""))
  else {
    return (
      StatusCode::BAD_REQUEST,
      "Webhook format must be generic, slack, discord, or teams",
    )
      .into_response();
  };

  // New webhooks belong to the caller's effective organization and fire only
  // for that org's events.
  let org = crate::auth::effective_org(&state, &headers).await;
  let hook = state
    .webhook_store
    .lock()
    .await
    .create(name, url, events, secret, format, org);
  info!("Webhook created: {} -> {}", hook.name, hook.url);
  state
    .audit_session(
      "webhook_created",
      &headers,
      &actor_ip,
      &format!(
        "name={} url={} events={:?}",
        hook.name, hook.url, hook.events
      ),
    )
    .await;
  Json(serde_json::json!({"status": "ok", "id": hook.id})).into_response()
}

/// Deletes a webhook definition.
#[utoipa::path(delete, path = "/aperio/api/webhooks/{id}", tag = "webhooks",
  description = "Deletes a webhook definition.",
  params(("id" = String, Path, description = "Webhook id")),
  responses((status = 200, description = "Deleted"), (status = 404, description = "Unknown id")))]
pub(crate) async fn webhooks_delete_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  // Isolation: only webhooks in the caller's effective org may be deleted.
  let org = crate::auth::effective_org(&state, &headers).await;
  let in_org = state
    .webhook_store
    .lock()
    .await
    .list()
    .iter()
    .any(|w| w.id == id && w.org_id == org);
  if !in_org {
    return (StatusCode::NOT_FOUND, "Webhook not found").into_response();
  }
  if state.webhook_store.lock().await.delete(&id) {
    state
      .audit_session(
        "webhook_deleted",
        &headers,
        &actor_ip,
        &format!("id={}", id),
      )
      .await;
    Json(serde_json::json!({"status": "ok"})).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Webhook not found").into_response()
  }
}

/// Query of the delivery-log listing.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct DeliveriesQuery {
  /// Only this webhook's deliveries.
  pub(crate) webhook_id: Option<String>,
  /// Most-recent rows to return (default 50, max 200).
  pub(crate) limit: Option<usize>,
}

/// Lists recent webhook delivery outcomes, newest first.
#[utoipa::path(get, path = "/aperio/api/webhooks/deliveries", tag = "webhooks",
  description = "Recent webhook delivery outcomes (attempts, status, payload), newest first.",
  responses((status = 200, description = "Delivery log", body = Vec<crate::store::webhooks::Delivery>)))]
pub(crate) async fn webhook_deliveries_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Query(q): Query<DeliveriesQuery>,
) -> Json<Vec<crate::store::webhooks::Delivery>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let limit = q.limit.unwrap_or(50).min(200);
  // Fetch a wider window, then keep only this org's deliveries up to the limit.
  let rows = state
    .webhook_deliveries
    .lock()
    .await
    .list(q.webhook_id.as_deref(), 500);
  Json(
    rows
      .into_iter()
      .filter(|d| d.org_id == org)
      .take(limit)
      .collect(),
  )
}

/// Re-sends a logged delivery's exact payload to its webhook.
#[utoipa::path(post, path = "/aperio/api/webhooks/deliveries/{id}/redeliver", tag = "webhooks",
  description = "Queues a redelivery of the logged payload to the webhook's current URL (fresh signature, normal retry policy); the outcome lands in the delivery log as a new row.",
  responses(
    (status = 202, description = "Redelivery queued"),
    (status = 404, description = "Unknown delivery or deleted webhook")))]
pub(crate) async fn webhook_redeliver_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
) -> Response {
  let Some(delivery) = state.webhook_deliveries.lock().await.get(&id).cloned() else {
    return (StatusCode::NOT_FOUND, "unknown delivery id").into_response();
  };
  // Isolation: only deliveries in the caller's effective org may be redelivered.
  let org = crate::auth::effective_org(&state, &headers).await;
  if delivery.org_id != org {
    return (StatusCode::NOT_FOUND, "unknown delivery id").into_response();
  }
  let Some(hook) = state
    .webhook_store
    .lock()
    .await
    .list()
    .iter()
    .find(|w| w.id == delivery.webhook_id)
    .cloned()
  else {
    return (
      StatusCode::NOT_FOUND,
      "the webhook this delivery belonged to no longer exists",
    )
      .into_response();
  };
  let ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  state
    .audit_session(
      "webhook_redelivered",
      &headers,
      &ip.to_string(),
      &format!("webhook={} event={}", hook.name, delivery.event),
    )
    .await;
  info!(
    "Redelivering event {} to webhook '{}' on operator request",
    delivery.event, hook.name
  );
  let log = state.webhook_deliveries.clone();
  let policy = state.config().outbound_policy.clone();
  tokio::spawn(async move {
    crate::store::webhooks::deliver_with_retries(hook, delivery.event, delivery.body, log, policy)
      .await;
  });
  (
    StatusCode::ACCEPTED,
    Json(serde_json::json!({"queued": true})),
  )
    .into_response()
}

/// Sends a synthetic event to one webhook and reports the outcome, so a
/// webhook that was configured wrong is discovered now rather than the next
/// time something actually happens, which is exactly the wrong moment.
#[utoipa::path(post, path = "/aperio/api/webhooks/{id}/test", tag = "webhooks",
  description = "Sends one synthetic `webhook_test` event through the real delivery path (outbound policy, signature, timeout) and returns what the receiver answered. One attempt, no retries: the caller is waiting for the answer.",
  params(("id" = String, Path, description = "Webhook id")),
  responses(
    (status = 200, description = "The delivery outcome", body = serde_json::Value),
    (status = 404, description = "Unknown webhook id")))]
pub(crate) async fn webhook_test_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
) -> Response {
  let org = crate::auth::effective_org(&state, &headers).await;
  let Some(hook) = state
    .webhook_store
    .lock()
    .await
    .list()
    .iter()
    .find(|w| w.id == id && w.org_id == org)
    .cloned()
  else {
    return (StatusCode::NOT_FOUND, "unknown webhook id").into_response();
  };
  let ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  state
    .audit_session(
      "webhook_tested",
      &headers,
      &ip.to_string(),
      &format!("webhook={}", hook.name),
    )
    .await;

  // The body says what it is, in the same envelope every real event uses, so
  // a receiver that switches on `event` can ignore it rather than acting on
  // a deploy that never happened.
  let body = serde_json::json!({
    "event": crate::store::webhooks::TEST_EVENT,
    "timestamp": chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
    "data": {
      "message": "Test delivery from the Aperio dashboard. Nothing happened; this event exists to prove the webhook is reachable.",
      "webhook": hook.name,
    }
  })
  .to_string();
  let outcome = crate::store::webhooks::deliver_test(
    hook,
    body,
    state.webhook_deliveries.clone(),
    state.config().outbound_policy.clone(),
  )
  .await;
  Json(serde_json::json!({
    "ok": outcome.success,
    "status": outcome.status,
    "error": outcome.error,
    "duration_ms": outcome.duration_ms,
    "delivery_id": outcome.id,
  }))
  .into_response()
}

#[cfg(test)]
#[path = "webhooks_tests.rs"]
mod tests;
