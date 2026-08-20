use axum::{
  Json,
  extract::{ConnectInfo, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::auth::valid_ip_entry;
use crate::routing::{extract_client_ip, normalize_hostname_bind, normalize_path_bind};
use crate::state::AppState;
use crate::store::tokens::{NotWritten, TokenPatch, TokenSpec};

/// The answer when a change was undone because it could not be saved.
///
/// A 500 rather than an "ok": the record is not in the store, so telling the
/// caller it worked would leave them with a token that stops existing at the
/// next restart. The cause is almost always a full disk, and the server's log
/// carries the underlying error from SQLite.
pub(crate) fn not_persisted() -> axum::response::Response {
  (
    StatusCode::INTERNAL_SERVER_ERROR,
    "The change could not be saved to the store and was rolled back; see the server log",
  )
    .into_response()
}

/// Public view of a dynamic token record (never includes hash or secret).
#[derive(Serialize)]
pub(crate) struct TokenView {
  pub(crate) id: String,
  pub(crate) name: String,
  pub(crate) token_prefix: String,
  pub(crate) hostnames: Vec<String>,
  pub(crate) paths: Vec<String>,
  pub(crate) allowed_ips: Vec<String>,
  pub(crate) created_at: u64,
  pub(crate) expires_at: Option<u64>,
  pub(crate) expired: bool,
  pub(crate) max_rps: Option<f64>,
  pub(crate) daily_max_bytes: Option<u64>,
  pub(crate) max_connections: Option<u32>,
  pub(crate) allow_public: bool,
  pub(crate) allow_server_side: bool,
  pub(crate) allow_bind: bool,
  pub(crate) allow_otel: bool,
  pub(crate) topics: Vec<String>,
  pub(crate) canary: bool,
}

/// Lists dynamic API tokens (metadata only, secrets are never returned).
#[utoipa::path(get, path = "/aperio/api/tokens", tag = "tokens",
  description = "Lists dynamic API tokens (hashes stripped; only the display prefix is exposed).",
  responses((status = 200, description = "Token records", body = serde_json::Value)))]
pub(crate) async fn tokens_list_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Json<Vec<TokenView>> {
  let org = crate::auth::effective_org(&state, &headers).await;
  let store = state.token_store.lock().await;
  let views = store
    .list()
    .iter()
    .filter(|t| t.org_id == org)
    .map(|t| TokenView {
      id: t.id.clone(),
      name: t.name.clone(),
      token_prefix: t.token_prefix.clone(),
      hostnames: t.hostnames.clone(),
      paths: t.paths.clone(),
      allowed_ips: t.allowed_ips.clone(),
      created_at: t.created_at,
      expires_at: t.expires_at,
      expired: t.is_expired(),
      max_rps: t.max_rps,
      daily_max_bytes: t.daily_max_bytes,
      max_connections: t.max_connections,
      allow_public: t.allow_public,
      allow_server_side: t.allow_server_side,
      allow_bind: t.allow_bind,
      allow_otel: t.allow_otel,
      topics: t.topics.clone(),
      canary: t.canary,
    })
    .collect();
  Json(views)
}

/// Payload for creating a dynamic token from the dashboard.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct TokenCreateRequest {
  pub(crate) name: String,
  /// Allowed hostnames; `["*"]` (or empty) = all hostnames.
  #[serde(default)]
  pub(crate) hostnames: Vec<String>,
  /// Allowed path binds; `["*"]` (or empty) = all paths.
  #[serde(default)]
  pub(crate) paths: Vec<String>,
  /// Source IPs/CIDRs allowed to connect. Defaults to `["0.0.0.0/0"]` (any).
  #[serde(default)]
  pub(crate) allowed_ips: Vec<String>,
  /// Optional lifetime in seconds; omitted = never expires.
  pub(crate) ttl_seconds: Option<u64>,
  /// Optional request rate limit (requests/second) for proxied traffic
  /// served through this token.
  pub(crate) max_rps: Option<f64>,
  /// Optional daily byte quota (request + response payload).
  pub(crate) daily_max_bytes: Option<u64>,
  /// Parallel connections a client using this token may open for one service.
  /// Absent = the server's own ceiling; a larger value cannot raise it.
  pub(crate) max_connections: Option<u32>,
  /// May clients using this token publish services as public (skipping the
  /// server's visitor auth gate)? Defaults to false.
  #[serde(default)]
  pub(crate) allow_public: bool,
  /// May clients using this token ask the server to reach a service's target
  /// itself (`server_side: true`)? Defaults to false. Where the server may
  /// connect is the operator's `server_side_targets:`; this only decides
  /// whether the client may ask.
  #[serde(default)]
  pub(crate) allow_server_side: bool,
  /// May clients using this token bind another client's tunnels within the
  /// same organization? Defaults to false.
  #[serde(default)]
  pub(crate) allow_bind: bool,
  /// May clients using this token send OpenTelemetry exports through the
  /// server's OTel bridge? Defaults to false, and the server's own
  /// `otel_bridge` has to be on as well.
  #[serde(default)]
  pub(crate) allow_otel: bool,
  /// Topic filters this token may publish to and subscribe to, for messages
  /// between the clients of its organization. Empty (the default) = no
  /// messaging; `["#"]` = everything the organization can see.
  #[serde(default)]
  pub(crate) topics: Vec<String>,
  /// Mark this token as a canary/decoy: any successful auth with it fires a
  /// `canary_tripped` alert. Defaults to false.
  #[serde(default)]
  pub(crate) canary: bool,
}

/// Payload for editing an existing token's scope without changing the secret.
/// Absent fields are left untouched; `ttl_seconds: 0` clears the expiry.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct TokenUpdateRequest {
  pub(crate) name: Option<String>,
  pub(crate) hostnames: Option<Vec<String>>,
  pub(crate) paths: Option<Vec<String>>,
  pub(crate) allowed_ips: Option<Vec<String>>,
  /// Some(0) = never expires; Some(n) = expires n seconds from now.
  pub(crate) ttl_seconds: Option<u64>,
  /// Some(0.0) clears the rate limit; Some(n) sets it to n req/s.
  pub(crate) max_rps: Option<f64>,
  /// Some(0) clears the quota; Some(n) sets it to n bytes/day.
  pub(crate) daily_max_bytes: Option<u64>,
  /// Some(0) clears the per-service connection ceiling (back to the server's);
  /// Some(n) sets it to n.
  pub(crate) max_connections: Option<u32>,
  /// Absent = keep; true/false sets whether public publishing is permitted.
  pub(crate) allow_public: Option<bool>,
  pub(crate) allow_server_side: Option<bool>,
  /// Absent = keep; true/false sets whether this token may bind other
  /// clients' tunnels in its organization.
  pub(crate) allow_bind: Option<bool>,
  /// Absent = keep; true/false sets whether this token may use the OTel
  /// bridge.
  pub(crate) allow_otel: Option<bool>,
  /// Replaces the token's topic filters. `[]` withdraws messaging entirely.
  #[serde(default)]
  pub(crate) topics: Option<Vec<String>>,
  /// Absent = keep; true/false toggles the canary/decoy flag.
  pub(crate) canary: Option<bool>,
}

/// Normalized (hostnames, paths, allowed_ips) permission lists.
type TokenPermLists = (Vec<String>, Vec<String>, Vec<String>);

/// The 403 to return when the organization has already used up its
/// `max_tokens` quota, or `None` when it may mint another one.
///
/// Every endpoint that mints a credential into the token store has to call
/// this, the tunnel-provisioning endpoint mints real, if short-lived, tokens
/// into the same store, so an organization that skipped it could hold any
/// number of credentials regardless of its cap. Call it while holding the
/// store lock the insert will use: counting before taking the lock lets two
/// concurrent creates both pass the check and overshoot the cap.
pub(crate) fn org_token_quota_reached(
  store: &crate::store::tokens::TokenStore,
  org: Option<&str>,
  quota_max: Option<u64>,
) -> Option<Response> {
  let max = quota_max?;
  let count = store
    .list()
    .iter()
    .filter(|t| t.org_id.as_deref() == org)
    .count() as u64;
  (count >= max).then(|| {
    (
      StatusCode::FORBIDDEN,
      format!("organization token quota reached ({max})"),
    )
      .into_response()
  })
}

/// Checks a token's topic filters. A filter that cannot match anything is a
/// typo that looks like it works, so it is refused at mint time rather than
/// discovered when a message quietly never arrives.
fn validate_topics(topics: &[String]) -> Result<Vec<String>, String> {
  let mut out = Vec::new();
  for raw in topics {
    let filter = raw.trim();
    if filter.is_empty() {
      continue;
    }
    // `*` is what the hostname and path lists spell "everything" as, and
    // someone will write it here out of habit. Accept it as `#` rather than
    // minting a token whose one filter matches a topic literally named `*`.
    let filter = if filter == "*" { "#" } else { filter };
    aperio_config::validate_topic_filter(filter)?;
    if !out.iter().any(|f| f == filter) {
      out.push(filter.to_string());
    }
  }
  Ok(out)
}

/// Validates and normalizes token permission lists. Returns an error message
/// when an entry is invalid.
pub(crate) fn validate_token_perms(
  hostnames: &[String],
  paths: &[String],
  allowed_ips: &[String],
) -> Result<TokenPermLists, String> {
  let mut out_hosts = Vec::new();
  for h in hostnames {
    let trimmed = h.trim();
    if trimmed.is_empty() {
      continue;
    }
    if trimmed == "*" {
      out_hosts.push("*".to_string());
      continue;
    }
    match normalize_hostname_bind(trimmed) {
      Some(valid) => out_hosts.push(valid),
      None => return Err(format!("Invalid hostname permission: {}", trimmed)),
    }
  }
  let mut out_paths = Vec::new();
  for p in paths {
    let trimmed = p.trim();
    if trimmed.is_empty() {
      continue;
    }
    if trimmed == "*" {
      out_paths.push("*".to_string());
      continue;
    }
    match normalize_path_bind(trimmed) {
      Some(valid) => out_paths.push(valid),
      None => return Err(format!("Invalid path permission: {}", trimmed)),
    }
  }
  let mut out_ips = Vec::new();
  for entry in allowed_ips {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
      continue;
    }
    if !valid_ip_entry(trimmed) {
      return Err(format!("Invalid IP/CIDR entry: {}", trimmed));
    }
    out_ips.push(trimmed.to_string());
  }
  if out_ips.is_empty() {
    out_ips.push("0.0.0.0/0".to_string());
  }
  Ok((out_hosts, out_paths, out_ips))
}

/// Creates a dynamic token. The plaintext secret is returned exactly once.
#[utoipa::path(post, path = "/aperio/api/tokens", tag = "tokens",
  description = "Creates a dynamic token; the plaintext secret is returned exactly once.",
  request_body = TokenCreateRequest,
  responses((status = 200, description = "Created token + secret", body = serde_json::Value), (status = 400, description = "Invalid permissions"), (status = 500, description = "The change could not be saved and was rolled back")))]
pub(crate) async fn tokens_create_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<TokenCreateRequest>,
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
      "Token name must be 1-64 characters",
    )
      .into_response();
  }

  let (hostnames, paths, allowed_ips) =
    match validate_token_perms(&payload.hostnames, &payload.paths, &payload.allowed_ips) {
      Ok(v) => v,
      Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };

  if payload.max_rps.is_some_and(|v| !v.is_finite() || v < 0.0) {
    return (StatusCode::BAD_REQUEST, "max_rps must be a positive number").into_response();
  }

  let topics = match validate_topics(&payload.topics) {
    Ok(v) => v,
    Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
  };

  // New tokens belong to the caller's currently effective organization.
  let org = crate::auth::effective_org(&state, &headers).await;
  // Organization fence: a permission outside the org's hostname allowlist is
  // refused outright, so the fence holds at mint time and not only at connect.
  if let Err(resp) = check_org_hostname_fence(&state, org.as_deref(), &hostnames).await {
    return resp;
  }
  // Enforce the org token quota atomically with the create: hold the
  // token_store lock across the count and the insert so two concurrent creates
  // can't both pass the check and overshoot the cap (the cap value itself comes
  // from the org store and is fetched first).
  let quota_max = state
    .org_quota(org.as_deref())
    .await
    .and_then(|q| q.max_tokens);
  let created = {
    let mut store = state.token_store.lock().await;
    if let Some(resp) = org_token_quota_reached(&store, org.as_deref(), quota_max) {
      return resp;
    }
    store.create(TokenSpec {
      name,
      hostnames,
      paths,
      topics,
      allowed_ips,
      org_id: org,
      ttl_seconds: payload.ttl_seconds,
      max_rps: payload.max_rps.filter(|v| *v > 0.0),
      daily_max_bytes: payload.daily_max_bytes.filter(|v| *v > 0),
      max_connections: payload.max_connections.filter(|v| *v > 0),
      allow_public: payload.allow_public,
      allow_server_side: payload.allow_server_side,
      allow_bind: payload.allow_bind,
      allow_otel: payload.allow_otel,
      canary: payload.canary,
    })
  };
  let Ok((record, secret)) = created else {
    return not_persisted();
  };
  info!(
    "Dynamic token created: {} (id={}, hostnames={:?}, paths={:?}, ips={:?}, expires_at={:?})",
    record.name, record.id, record.hostnames, record.paths, record.allowed_ips, record.expires_at
  );
  state
    .audit_session(
      "token_created",
      &headers,
      &actor_ip,
      &format!(
        "name={} id={} hostnames={:?} paths={:?} ips={:?} expires_at={:?}",
        record.name,
        record.id,
        record.hostnames,
        record.paths,
        record.allowed_ips,
        record.expires_at
      ),
    )
    .await;
  state
    .emit_event_in(
      "token_created",
      serde_json::json!({"id": record.id, "name": record.name}),
      record.org_id.clone(),
    )
    .await;
  (
    StatusCode::OK,
    Json(serde_json::json!({
      "id": record.id,
      "name": record.name,
      "token": secret,
      "hostnames": record.hostnames,
      "paths": record.paths,
      "allowed_ips": record.allowed_ips,
      "expires_at": record.expires_at,
    })),
  )
    .into_response()
}

/// Rejects token hostname permissions that fall outside the organization's
/// allowlist, so a fenced tenant can never mint a token for a hostname it does
/// not own. A wildcard (`*`, or an empty list) stays allowed: it then means
/// "any hostname within the org's fence", which the connect-time check
/// narrows.
// The `Err` is the HTTP answer, which is the whole point: these helpers say
// "stop, and send this" and the caller writes `?`. `result_large_err` measures
// a 128-byte `axum::Response` against a 128-byte threshold and asks for it to
// be boxed, which would put an allocation on the refusal path and a `Box` at
// every call site to save moving one cache line. Allowed where the pattern is
// deliberate rather than boxed everywhere it appears.
#[allow(clippy::result_large_err)]
async fn check_org_hostname_fence(
  state: &Arc<AppState>,
  org: Option<&str>,
  hostnames: &[String],
) -> Result<(), Response> {
  let allowlist = state.org_store.lock().await.hostnames_of(org);
  if allowlist.is_empty() {
    return Ok(());
  }
  for host in hostnames {
    if host == "*" {
      continue;
    }
    if !crate::store::orgs::hostname_in_org_allowlist(host, &allowlist) {
      return Err(
        (
          StatusCode::FORBIDDEN,
          format!(
            "hostname {} is outside this organization's allowlist ({})",
            host,
            allowlist.join(", ")
          ),
        )
          .into_response(),
      );
    }
  }
  Ok(())
}

/// Whether a token id exists and belongs to the caller's effective org. Used
/// to gate by-id mutations so one org cannot touch another's tokens.
async fn token_in_effective_org(state: &Arc<AppState>, headers: &HeaderMap, id: &str) -> bool {
  let org = crate::auth::effective_org(state, headers).await;
  state
    .token_store
    .lock()
    .await
    .list()
    .iter()
    .any(|t| t.id == id && t.org_id == org)
}

/// Edits an existing token's scope (name, hostnames, paths, allowed IPs,
/// expiry) without changing the secret. Live connections are unaffected.
#[utoipa::path(put, path = "/aperio/api/tokens/{id}", tag = "tokens",
  description = "Edits a token's scope/limits/expiry in place without changing the secret. A narrowed `topics` applies to the clients already connected under the token: every live subscription the new list no longer covers is withdrawn at once and reported to that client.",
  params(("id" = String, Path, description = "Token record id")),
  request_body = TokenUpdateRequest,
  responses((status = 200, description = "Updated record", body = serde_json::Value), (status = 404, description = "Unknown token id"), (status = 500, description = "The change could not be saved and was rolled back")))]
pub(crate) async fn tokens_update_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<TokenUpdateRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();

  if let Some(ref n) = payload.name {
    let n = n.trim();
    if n.is_empty() || n.len() > 64 {
      return (
        StatusCode::BAD_REQUEST,
        "Token name must be 1-64 characters",
      )
        .into_response();
    }
  }
  let (hostnames, paths, allowed_ips) = match validate_token_perms(
    payload.hostnames.as_deref().unwrap_or(&[]),
    payload.paths.as_deref().unwrap_or(&[]),
    payload.allowed_ips.as_deref().unwrap_or(&[]),
  ) {
    Ok(v) => v,
    Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
  };

  // ttl_seconds: absent = keep; 0 = never expires; n = now + n.
  let ttl = payload
    .ttl_seconds
    .map(|n| if n == 0 { None } else { Some(n) });

  if payload.max_rps.is_some_and(|v| !v.is_finite() || v < 0.0) {
    return (StatusCode::BAD_REQUEST, "max_rps must be a positive number").into_response();
  }

  let topics = match payload.topics.as_deref().map(validate_topics).transpose() {
    Ok(v) => v,
    Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
  };

  // Organization fence, same rule as at creation: an edit can never widen a
  // token beyond the hostnames its org may claim.
  if payload.hostnames.is_some() {
    let org = crate::auth::effective_org(&state, &headers).await;
    if let Err(resp) = check_org_hostname_fence(&state, org.as_deref(), &hostnames).await {
      return resp;
    }
  }

  // Isolation: a caller may only edit tokens in their effective org. Unknown
  // and cross-org ids are indistinguishable (both 404) so existence never leaks.
  if !token_in_effective_org(&state, &headers, &id).await {
    return (StatusCode::NOT_FOUND, "Token not found").into_response();
  }

  let updated = state.token_store.lock().await.update(
    &id,
    TokenPatch {
      name: payload.name.map(|n| n.trim().to_string()),
      hostnames: payload.hostnames.map(|_| hostnames),
      paths: payload.paths.map(|_| paths),
      topics,
      allowed_ips: payload.allowed_ips.map(|_| allowed_ips),
      ttl_seconds: ttl,
      // max_rps / daily_max_bytes: absent = keep; 0 = clear; n = set.
      max_rps: payload.max_rps.map(Some),
      daily_max_bytes: payload.daily_max_bytes.map(Some),
      max_connections: payload.max_connections.map(Some),
      allow_public: payload.allow_public,
      allow_server_side: payload.allow_server_side,
      allow_bind: payload.allow_bind,
      allow_otel: payload.allow_otel,
      canary: payload.canary,
    },
  );

  match updated {
    Ok(record) => {
      // A narrowed `topics` has to reach the connections already holding
      // subscriptions under the old grant, or it only takes effect whenever
      // each client next reconnects. Runs on any edit that names `topics`,
      // including a widening, so the cached copy never drifts from the store.
      if payload.topics.is_some() {
        let withdrawn = state.apply_token_topics(&record.id, &record.topics).await;
        if withdrawn > 0 {
          info!(
            "Token {} lost {} live subscription(s) to its narrowed topics",
            record.id, withdrawn
          );
        }
      }
      info!(
        "Dynamic token updated: {} (id={}, hostnames={:?}, paths={:?}, ips={:?}, expires_at={:?})",
        record.name,
        record.id,
        record.hostnames,
        record.paths,
        record.allowed_ips,
        record.expires_at
      );
      state
        .audit_session(
          "token_updated",
          &headers,
          &actor_ip,
          &format!(
            "name={} id={} hostnames={:?} paths={:?} ips={:?} expires_at={:?}",
            record.name,
            record.id,
            record.hostnames,
            record.paths,
            record.allowed_ips,
            record.expires_at
          ),
        )
        .await;
      Json(serde_json::json!({"status": "ok"})).into_response()
    }
    Err(NotWritten::NoSuchRecord) => (StatusCode::NOT_FOUND, "Token not found").into_response(),
    Err(NotWritten::NotPersisted) => not_persisted(),
  }
}

/// Refreshes a short-lived dynamic token: the caller presents the token
/// secret itself (Bearer / x-auth-token) and, when the token was created with
/// a TTL, its expiry slides forward by that same TTL. Registered outside the
/// dashboard session middleware on purpose, the typical caller is a CI job
/// or long-running client that only holds the token, not a dashboard session.
/// Tokens without a TTL are not refreshable (they never expire), and an
/// already-expired token cannot resurrect itself.
#[utoipa::path(post, path = "/aperio/api/tokens/refresh", tag = "tokens",
  description = "Slides a TTL-token's expiry forward by its creation TTL. Authenticates with the token secret itself (Bearer); no dashboard session needed. Rate-limited per IP.",
  responses((status = 200, description = "New expiry", body = serde_json::Value), (status = 401, description = "Unknown/expired secret"), (status = 409, description = "Token never expires"), (status = 500, description = "The change could not be saved and was rolled back")))]
pub(crate) async fn tokens_refresh_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  // Rate limit before touching the store so secrets cannot be guessed faster
  // than the normal per-IP budget.
  // A refresh presents a secret, so a wrong answer is a guess somebody made.
  if !state
    .check_rate_limit_cost(actor_ip, crate::state::RateCost::Guessable)
    .await
  {
    return StatusCode::TOO_MANY_REQUESTS.into_response();
  }
  let Some(secret) = crate::auth::extract_token(&headers) else {
    return (
      StatusCode::UNAUTHORIZED,
      "Present the token secret as a Bearer token",
    )
      .into_response();
  };
  let refreshed = state.token_store.lock().await.refresh(&secret);
  match refreshed {
    Ok(record) => {
      info!(
        "Dynamic token refreshed: {} (id={}, new expires_at={:?})",
        record.name, record.id, record.expires_at
      );
      // Refresh authenticates with the token secret itself (no session), so the
      // event is filed under the token's own organization.
      state
        .audit_in(
          "token_refreshed",
          &state.session_actor(&headers).await,
          &actor_ip.to_string(),
          record.org_id.clone(),
          &format!("id={} expires_at={:?}", record.id, record.expires_at),
        )
        .await;
      Json(serde_json::json!({
        "status": "ok",
        "id": record.id,
        "expires_at": record.expires_at,
      }))
      .into_response()
    }
    Err(NotWritten::NoSuchRecord) => (
      StatusCode::UNAUTHORIZED,
      "Unknown, expired, or non-expiring token",
    )
      .into_response(),
    Err(NotWritten::NotPersisted) => not_persisted(),
  }
}

/// Payload for rotating a token's secret.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct TokenRotateRequest {
  /// Seconds the old secret stays accepted after the rotation (0 or absent =
  /// immediate cutover).
  #[serde(default)]
  pub(crate) grace_seconds: u64,
}

/// Rotates a token's secret: a fresh secret becomes current and is returned
/// exactly once; the old secret keeps working for the requested grace window
/// so running clients and CI jobs can migrate without a hard cutover.
/// Permissions, limits and expiry are untouched.
#[utoipa::path(post, path = "/aperio/api/tokens/{id}/rotate", tag = "tokens",
  description = "Rotates a token's secret; the old secret stays valid for grace_seconds. The new secret is returned exactly once.",
  params(("id" = String, Path, description = "Token record id")),
  request_body = TokenRotateRequest,
  responses((status = 200, description = "New secret + grace deadline", body = serde_json::Value), (status = 404, description = "Unknown token id"), (status = 500, description = "The change could not be saved and was rolled back")))]
pub(crate) async fn tokens_rotate_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(id): axum::extract::Path<String>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(payload): Json<TokenRotateRequest>,
) -> Response {
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();
  // A year is plenty for any migration window; larger values are typos.
  if payload.grace_seconds > 365 * 24 * 3600 {
    return (
      StatusCode::BAD_REQUEST,
      "grace_seconds must be at most one year",
    )
      .into_response();
  }
  // Isolation: only tokens in the caller's effective org may be rotated.
  if !token_in_effective_org(&state, &headers, &id).await {
    return (StatusCode::NOT_FOUND, "Token not found").into_response();
  }
  let rotated = state
    .token_store
    .lock()
    .await
    .rotate(&id, payload.grace_seconds);
  match rotated {
    Ok((record, secret)) => {
      info!(
        "Dynamic token rotated: {} (id={}, grace_seconds={}, prev_expires_at={:?})",
        record.name, record.id, payload.grace_seconds, record.prev_expires_at
      );
      state
        .audit_session(
          "token_rotated",
          &headers,
          &actor_ip,
          &format!(
            "name={} id={} grace_seconds={} prev_expires_at={:?}",
            record.name, record.id, payload.grace_seconds, record.prev_expires_at
          ),
        )
        .await;
      state
        .emit_event_in(
          "token_rotated",
          serde_json::json!({
            "id": record.id,
            "name": record.name,
            "grace_seconds": payload.grace_seconds,
            "prev_expires_at": record.prev_expires_at,
          }),
          record.org_id.clone(),
        )
        .await;
      (
        StatusCode::OK,
        Json(serde_json::json!({
          "id": record.id,
          "name": record.name,
          "token": secret,
          "prev_expires_at": record.prev_expires_at,
          "expires_at": record.expires_at,
        })),
      )
        .into_response()
    }
    Err(NotWritten::NoSuchRecord) => (StatusCode::NOT_FOUND, "Token not found").into_response(),
    Err(NotWritten::NotPersisted) => not_persisted(),
  }
}

/// Revokes (deletes) a dynamic token and drops any tunnel connections that are
/// currently using it, a revoked token could otherwise keep serving traffic
/// until the client next reconnected (when it would be rejected anyway).
#[utoipa::path(delete, path = "/aperio/api/tokens/{id}", tag = "tokens",
  description = "Revokes a token and immediately drops any tunnel connections using it.",
  params(("id" = String, Path, description = "Token record id")),
  responses((status = 200, description = "Revoked"), (status = 404, description = "Unknown token id"), (status = 500, description = "The change could not be saved and was rolled back")))]
pub(crate) async fn tokens_revoke_handler(
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
  // Isolation: only tokens in the caller's effective org may be revoked.
  if !token_in_effective_org(&state, &headers, &id).await {
    return (StatusCode::NOT_FOUND, "Token not found").into_response();
  }
  let revoked = state.token_store.lock().await.revoke(&id);
  if revoked == Err(NotWritten::NotPersisted) {
    return not_persisted();
  }
  if revoked.is_ok() {
    info!("Dynamic token revoked: {}", id);
    // An autoscaling record must never outlive the credential that armed it:
    // otherwise a revoked token would keep the server calling a scaling
    // endpoint on its behalf.
    let disowned = state.scaling_store.lock().await.disown(&id);
    if disowned > 0 {
      info!(
        "Disarmed {} autoscaling record(s) owned only by the revoked token {}",
        disowned, id
      );
    }
    let dropped = state.disconnect_token_clients(&id).await;
    if dropped > 0 {
      info!(
        "Disconnecting {} live client(s) using the revoked token {}",
        dropped, id
      );
    }
    state
      .audit_session(
        "token_revoked",
        &headers,
        &actor_ip,
        &format!("id={} disconnected_clients={}", id, dropped),
      )
      .await;
    // The token was gated to the caller's effective org above.
    let org = crate::auth::effective_org(&state, &headers).await;
    state
      .emit_event_in("token_revoked", serde_json::json!({"id": id}), org)
      .await;
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
  } else {
    (StatusCode::NOT_FOUND, "Token not found").into_response()
  }
}

#[cfg(test)]
#[path = "tokens_tests.rs"]
mod tests;
