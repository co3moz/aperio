//! Public listener control: every read and mutation is scoped by explicit
//! expose capabilities, independently of the caller's general dashboard role.

// Handler guards return ready Axum responses, matching api/tokens.rs.
#![allow(clippy::result_large_err)]

use crate::{auth::Caller, state::AppState};
use aperio_config::expose::*;
use aperio_config::expose_policy::ExposePolicy;
use axum::{
  Json,
  extract::{ConnectInfo, Path, Query, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;

/// Keep malformed JSON/type errors in the same machine-readable contract as
/// semantic validation, rather than Axum's default plain-text rejection.
pub(crate) struct ExposeJson<T>(pub T);
impl<S, T> axum::extract::FromRequest<S> for ExposeJson<T>
where
  S: Send + Sync,
  T: serde::de::DeserializeOwned,
{
  type Rejection = Response;
  async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
    Json::<T>::from_request(req, state)
      .await
      .map(|Json(value)| Self(value))
      .map_err(|error| failure(error.status(), "validation", error.body_text()))
  }
}

fn failure(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
  (
    status,
    Json(serde_json::json!({ "code": code, "message": message.into() })),
  )
    .into_response()
}
fn runtime_error(message: String) -> Response {
  let (status, code) = if message.starts_with("persistence:") {
    (StatusCode::SERVICE_UNAVAILABLE, "persistence_failed")
  } else if message.contains("bind") || message.contains("port conflict") {
    (StatusCode::CONFLICT, "port_conflict")
  } else if message.contains("quota") {
    (StatusCode::CONFLICT, "quota_exceeded")
  } else if message.contains("allocation") {
    (StatusCode::FORBIDDEN, "port_policy")
  } else {
    (StatusCode::INTERNAL_SERVER_ERROR, "apply_failed")
  };
  failure(status, code, message)
}
async fn caller(state: &AppState, headers: &HeaderMap) -> Result<Caller, Response> {
  crate::auth::resolve_caller(state, headers)
    .await
    .ok_or_else(|| {
      failure(
        StatusCode::UNAUTHORIZED,
        "authentication_required",
        "Sign in with a user session or admin API key",
      )
    })
}
fn permits(caller: &Caller, org: &str, action: ExposeAction) -> bool {
  caller
    .expose_actions(if org == "master" { None } else { Some(org) })
    .contains(&action)
}
fn authorized(
  caller: &Caller,
  resource: &ExposeResource,
  action: ExposeAction,
) -> Result<(), Response> {
  if !permits(caller, &resource.spec.org_id, ExposeAction::Read) {
    return Err(not_found());
  }
  if !permits(caller, &resource.spec.org_id, action) {
    return Err(failure(
      StatusCode::FORBIDDEN,
      "forbidden",
      "This expose action is not granted",
    ));
  }
  if action != ExposeAction::Read
    && caller
      .expose_bounds(&resource.spec.org_id)
      .is_some_and(|b| !b.allows(&resource.spec))
  {
    return Err(failure(
      StatusCode::FORBIDDEN,
      "delegation_bounds",
      "Listener is outside your delegated allocation",
    ));
  }
  if action != ExposeAction::Read && resource.source == ExposeSource::File {
    return Err(failure(
      StatusCode::CONFLICT,
      "file_owned",
      "Edit this rule in the server configuration file",
    ));
  }
  Ok(())
}
fn bounded_change(
  caller: &Caller,
  spec: &ExposeSpec,
  document: &crate::store::exposes::ExposeDocument,
) -> Result<(), Response> {
  if let Some(bounds) = caller.expose_bounds(&spec.org_id)
    && (!bounds.allows(spec)
      || !bounds.fits(
        document
          .resources
          .iter()
          .map(|r| &r.spec)
          .filter(|s| s.org_id == spec.org_id && bounds.allows(s)),
      ))
  {
    return Err(failure(
      StatusCode::FORBIDDEN,
      "delegation_bounds",
      "Change exceeds your delegated address, port or resource bounds",
    ));
  }
  Ok(())
}
fn not_found() -> Response {
  failure(
    StatusCode::NOT_FOUND,
    "not_found",
    "Unknown expose resource",
  )
}
fn revision(resource: &ExposeResource, expected: u64) -> Result<(), Response> {
  if resource.revision == expected {
    Ok(())
  } else {
    Err(failure(
      StatusCode::CONFLICT,
      "revision_conflict",
      "The resource changed; reload it before saving",
    ))
  }
}
async fn validate(state: &Arc<AppState>, spec: &ExposeSpec) -> Result<(), Response> {
  let errors = spec.validate();
  if !errors.is_empty() {
    return Err(
      (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"code":"validation", "errors": errors})),
      )
        .into_response(),
    );
  }
  if spec.org_id != "master" && state.org_store.lock().await.find(&spec.org_id).is_none() {
    return Err(failure(
      StatusCode::BAD_REQUEST,
      "invalid_org",
      "Unknown organization",
    ));
  }
  let rule = crate::expose_manager::ManagedRule {
    resource: ExposeResource {
      id: String::new(),
      revision: 1,
      source: ExposeSource::Api,
      spec: spec.clone(),
    },
    legacy: None,
  };
  if matches!(
    crate::expose_manager::resolve::target(state, &rule).await,
    Err(ExposeTargetState::Incompatible)
  ) {
    return Err(failure(
      StatusCode::BAD_REQUEST,
      "incompatible_target",
      "The declared target is encrypted or does not serve this protocol",
    ));
  }
  Ok(())
}
async fn changed(
  state: &Arc<AppState>,
  headers: &HeaderMap,
  peer: SocketAddr,
  action: &str,
  resource: &ExposeResource,
  before: Option<&ExposeResource>,
  session_id: Option<&str>,
) {
  let actor = state.session_actor(headers).await;
  let org = (resource.spec.org_id != "master").then(|| resource.spec.org_id.clone());
  let details = serde_json::json!({"id": resource.id, "revision": resource.revision, "action": action,
    "outcome":"success", "before": before, "after": if action == "delete" { None } else { Some(resource) }, "session_id": session_id});
  state
    .audit_in(
      "expose_changed",
      &actor,
      &peer.ip().to_string(),
      org,
      &details.to_string(),
    )
    .await;
}

#[derive(Deserialize, Default)]
pub(crate) struct Page {
  pub offset: Option<usize>,
  pub limit: Option<usize>,
  pub org: Option<String>,
  pub protocol: Option<String>,
  pub search: Option<String>,
  pub history: Option<bool>,
  pub sort: Option<String>,
  pub descending: Option<bool>,
  pub client: Option<String>,
}

#[utoipa::path(get, path = "/aperio/api/exposes", description = "List public TCP/UDP exposes visible through current organization capabilities, with actual listener state.", params(("offset" = Option<usize>, Query), ("limit" = Option<usize>, Query), ("org" = Option<String>, Query), ("protocol" = Option<String>, Query), ("search" = Option<String>, Query)), tag = "exposes", responses((status = 200, description = "Current result", body = serde_json::Value), (status = 403, description = "Not authorized"), (status = 409, description = "Revision, port or quota conflict")))]
pub(crate) async fn list(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Query(page): Query<Page>,
) -> Response {
  let caller = match caller(&state, &headers).await {
    Ok(c) => c,
    Err(r) => return r,
  };
  let mut manager = state.exposes.lock().await;
  let mut filtered: Vec<_> = manager.views(&state).await.into_iter().filter(|v|
    permits(&caller, &v.resource.spec.org_id, ExposeAction::Read)
      && page.client.as_ref().is_none_or(|client| v.served_by.as_ref() == Some(client))
      && page.org.as_ref().is_none_or(|org| *org == v.resource.spec.org_id)
      && page.protocol.as_ref().is_none_or(|p| p == v.resource.spec.listener.protocol.as_str())
      && page.search.as_ref().is_none_or(|q| v.resource.spec.tunnel.to_lowercase().contains(&q.to_lowercase()) || v.resource.id.contains(q)))
    .map(|view| {
      let actions: Vec<_> = caller.expose_actions(if view.resource.spec.org_id == "master" { None } else { Some(&view.resource.spec.org_id) })
        .into_iter().filter(|a| *a == ExposeAction::Read || (view.resource.source == ExposeSource::Api
          && caller.expose_bounds(&view.resource.spec.org_id).is_none_or(|b| b.allows(&view.resource.spec)))).collect();
      let policy = manager.policy(&view.resource.spec.org_id);
      serde_json::json!({"bounds": caller.expose_bounds(&view.resource.spec.org_id), "resource": view, "actions": actions, "policy": policy})
    }).collect();
  let sort = page.sort.as_deref().unwrap_or("tunnel");
  if !matches!(
    sort,
    "tunnel" | "port" | "state" | "sessions" | "traffic" | "org"
  ) {
    return failure(
      StatusCode::BAD_REQUEST,
      "invalid_sort",
      "Sort must be tunnel, port, state, sessions, traffic or org",
    );
  }
  filtered.sort_by(|a, b| {
    let a = &a["resource"];
    let b = &b["resource"];
    let order = match sort {
      "port" => a["spec"]["listener"]["port"]
        .as_u64()
        .cmp(&b["spec"]["listener"]["port"].as_u64()),
      "sessions" => a["sessions"].as_u64().cmp(&b["sessions"].as_u64()),
      "traffic" => a["up_bytes"]
        .as_u64()
        .unwrap_or(0)
        .saturating_add(a["down_bytes"].as_u64().unwrap_or(0))
        .cmp(
          &b["up_bytes"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(b["down_bytes"].as_u64().unwrap_or(0)),
        ),
      "state" => a["state"].as_str().cmp(&b["state"].as_str()),
      "org" => a["spec"]["org_id"]
        .as_str()
        .cmp(&b["spec"]["org_id"].as_str()),
      _ => a["spec"]["tunnel"]
        .as_str()
        .cmp(&b["spec"]["tunnel"].as_str()),
    }
    .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()));
    if page.descending.unwrap_or(false) {
      order.reverse()
    } else {
      order
    }
  });
  let total = filtered.len();
  Json(serde_json::json!({"items": filtered.into_iter().skip(page.offset.unwrap_or(0)).take(page.limit.unwrap_or(100).min(500)).collect::<Vec<_>>(),
    "total": total, "volatile": manager.store.volatile,
    "history_error": manager.store.history_error.as_ref().map(|_| "Recent session history could not be persisted"),
    "store_error": if caller.is_master_admin() { manager.store.load_error.clone() } else { None },
    "file_error": if caller.is_master_admin() { manager.file_error.clone() } else { None }})).into_response()
}

#[utoipa::path(get, path = "/aperio/api/exposes/{id}", params(("id" = String, Path)), description = "Inspect one authorized public listener.", tag = "exposes", responses((status = 200, description = "Current result", body = serde_json::Value), (status = 403, description = "Not authorized"), (status = 409, description = "Revision, port or quota conflict")))]
pub(crate) async fn detail(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Path(id): Path<String>,
) -> Response {
  let caller = match caller(&state, &headers).await {
    Ok(c) => c,
    Err(r) => return r,
  };
  let mut manager = state.exposes.lock().await;
  let Some(resource) = manager.resource(&id) else {
    return not_found();
  };
  if let Err(r) = authorized(&caller, resource, ExposeAction::Read) {
    return r;
  }
  let view = manager
    .views(&state)
    .await
    .into_iter()
    .find(|v| v.resource.id == id);
  Json(view).into_response()
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Create {
  pub id: Option<String>,
  pub spec: ExposeSpec,
}

#[utoipa::path(post, path = "/aperio/api/exposes", request_body = Create, description = "Create a persistent expose using {id?: UUID, spec}. The optional id makes retries idempotent; no success is returned before bind and storage succeed.", tag = "exposes", responses((status = 201, description = "Created resource", body = ExposeResource), (status = 200, description = "Identical idempotent replay", body = ExposeResource), (status = 403, description = "Not authorized"), (status = 409, description = "Revision, port or quota conflict")))]
pub(crate) async fn create(
  State(state): State<Arc<AppState>>,
  ConnectInfo(peer): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  ExposeJson(payload): ExposeJson<Create>,
) -> Response {
  let caller = match caller(&state, &headers).await {
    Ok(c) => c,
    Err(r) => return r,
  };
  if !permits(&caller, &payload.spec.org_id, ExposeAction::Create) {
    return failure(
      StatusCode::FORBIDDEN,
      "forbidden",
      "Expose creation is not granted in this organization",
    );
  }
  if let Err(r) = validate(&state, &payload.spec).await {
    return r;
  }
  let id = payload
    .id
    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
  if uuid::Uuid::parse_str(&id).is_err() {
    return failure(
      StatusCode::BAD_REQUEST,
      "invalid_id",
      "An idempotency id must be a UUID",
    );
  }
  let resource = ExposeResource {
    id,
    revision: 1,
    source: ExposeSource::Api,
    spec: payload.spec,
  };
  let mut manager = state.exposes.lock().await;
  if let Some(existing) = manager.resource(&resource.id) {
    if let Err(response) = authorized(&caller, existing, ExposeAction::Read) {
      return response;
    }
    if existing.spec == resource.spec {
      return Json(existing).into_response();
    }
    return failure(
      StatusCode::CONFLICT,
      "id_conflict",
      "The request id has already been used",
    );
  }
  let mut document = manager.store.document.clone();
  document.resources.push(resource.clone());
  if let Err(response) = bounded_change(&caller, &resource.spec, &document) {
    return response;
  }
  if let Err(error) = manager.replace(&state, document, false).await {
    return runtime_error(error);
  }
  drop(manager);
  changed(&state, &headers, peer, "create", &resource, None, None).await;
  (StatusCode::CREATED, Json(resource)).into_response()
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Update {
  pub revision: u64,
  pub spec: ExposeSpec,
}

#[utoipa::path(put, path = "/aperio/api/exposes/{id}", params(("id" = String, Path)), request_body = Update, description = "Replace a public expose using {revision, spec}; stale edits return revision_conflict. File rules are read-only.", tag = "exposes", responses((status = 200, description = "Current result", body = serde_json::Value), (status = 403, description = "Not authorized"), (status = 409, description = "Revision, port or quota conflict")))]
pub(crate) async fn update(
  State(state): State<Arc<AppState>>,
  ConnectInfo(peer): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
  ExposeJson(payload): ExposeJson<Update>,
) -> Response {
  let caller = match caller(&state, &headers).await {
    Ok(c) => c,
    Err(r) => return r,
  };
  let mut manager = state.exposes.lock().await;
  let Some(old) = manager.resource(&id).cloned() else {
    return not_found();
  };
  if let Err(r) =
    authorized(&caller, &old, ExposeAction::Update).and_then(|_| revision(&old, payload.revision))
  {
    return r;
  }
  if payload.spec.org_id != old.spec.org_id {
    return failure(
      StatusCode::BAD_REQUEST,
      "immutable_org",
      "An expose cannot be moved to another organization",
    );
  }
  if old.spec.enabled != payload.spec.enabled
    && let Err(r) = authorized(
      &caller,
      &old,
      if payload.spec.enabled {
        ExposeAction::Enable
      } else {
        ExposeAction::Disable
      },
    )
  {
    return r;
  }
  if let Err(r) = validate(&state, &payload.spec).await {
    return r;
  }
  let resource = ExposeResource {
    revision: old.revision + 1,
    spec: payload.spec,
    ..old.clone()
  };
  let mut document = manager.store.document.clone();
  if let Some(row) = document.resources.iter_mut().find(|r| r.id == id) {
    *row = resource.clone();
  }
  if let Err(response) = bounded_change(&caller, &resource.spec, &document) {
    return response;
  }
  if let Err(error) = manager.replace(&state, document, false).await {
    return runtime_error(error);
  }
  drop(manager);
  changed(
    &state,
    &headers,
    peer,
    "update",
    &resource,
    Some(&old),
    None,
  )
  .await;
  Json(resource).into_response()
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Revision {
  pub revision: u64,
}
#[utoipa::path(delete, path = "/aperio/api/exposes/{id}", params(("id" = String, Path)), request_body = Revision, description = "Delete a public expose using {revision}, releasing its socket and sessions.", tag = "exposes", responses((status = 200, description = "Current result", body = serde_json::Value), (status = 403, description = "Not authorized"), (status = 409, description = "Revision, port or quota conflict")))]
pub(crate) async fn delete(
  State(state): State<Arc<AppState>>,
  ConnectInfo(peer): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
  ExposeJson(payload): ExposeJson<Revision>,
) -> Response {
  let caller = match caller(&state, &headers).await {
    Ok(c) => c,
    Err(r) => return r,
  };
  let mut manager = state.exposes.lock().await;
  let Some(old) = manager.resource(&id).cloned() else {
    return not_found();
  };
  if let Err(r) =
    authorized(&caller, &old, ExposeAction::Delete).and_then(|_| revision(&old, payload.revision))
  {
    return r;
  }
  let mut document = manager.store.document.clone();
  document.resources.retain(|r| r.id != id);
  if let Err(error) = manager.replace(&state, document, true).await {
    return runtime_error(error);
  }
  drop(manager);
  changed(&state, &headers, peer, "delete", &old, Some(&old), None).await;
  StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Operation {
  pub revision: u64,
  pub action: String,
  pub session_id: Option<String>,
  pub drain_seconds: Option<u32>,
}
#[utoipa::path(post, path = "/aperio/api/exposes/{id}/actions", params(("id" = String, Path)), request_body = Operation, description = "Run enable, disable, retry, drain, or disconnect with {revision, action, session_id?, drain_seconds?}. Each action is capability checked.", tag = "exposes", responses((status = 200, description = "Current result", body = serde_json::Value), (status = 403, description = "Not authorized"), (status = 409, description = "Revision, port or quota conflict")))]
pub(crate) async fn operate(
  State(state): State<Arc<AppState>>,
  ConnectInfo(peer): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(id): Path<String>,
  ExposeJson(payload): ExposeJson<Operation>,
) -> Response {
  let caller = match caller(&state, &headers).await {
    Ok(c) => c,
    Err(r) => return r,
  };
  let required = match payload.action.as_str() {
    "enable" | "retry" => ExposeAction::Enable,
    "disable" | "drain" => ExposeAction::Disable,
    "disconnect" => ExposeAction::Disconnect,
    _ => {
      return failure(
        StatusCode::BAD_REQUEST,
        "invalid_action",
        "Unknown expose operation",
      );
    }
  };
  let mut manager = state.exposes.lock().await;
  let Some(mut resource) = manager.resource(&id).cloned() else {
    return not_found();
  };
  if let Err(r) =
    authorized(&caller, &resource, required).and_then(|_| revision(&resource, payload.revision))
  {
    return r;
  }
  let before = resource.clone();
  let session_id = payload.session_id.clone();
  match payload.action.as_str() {
    "disconnect" => {
      let Some(session_id) = payload.session_id else {
        return failure(
          StatusCode::BAD_REQUEST,
          "session_required",
          "Select a session",
        );
      };
      if !manager.disconnect(&id, &session_id) {
        return not_found();
      }
    }
    "drain" => {
      if let Err(r) = authorized(&caller, &resource, ExposeAction::Disconnect) {
        return r;
      }
      let duration = payload.drain_seconds.unwrap_or(30);
      if duration == 0 || duration > 3600 {
        return failure(
          StatusCode::BAD_REQUEST,
          "invalid_deadline",
          "Drain deadline must be 1–3600 seconds",
        );
      }
      if let Err(error) = manager.drain(&id, std::time::Duration::from_secs(duration.into())) {
        return runtime_error(error);
      }
      resource = manager
        .resource(&id)
        .expect("drained resource remains registered")
        .clone();
    }
    _ => {
      resource.spec.enabled = payload.action != "disable";
      resource.revision += 1;
      let mut document = manager.store.document.clone();
      if let Some(row) = document.resources.iter_mut().find(|r| r.id == id) {
        *row = resource.clone();
      }
      if payload.action != "disable"
        && let Err(response) = bounded_change(&caller, &resource.spec, &document)
      {
        return response;
      }
      if let Err(error) = manager
        .replace(&state, document, payload.action == "disable")
        .await
      {
        return runtime_error(error);
      }
    }
  }
  drop(manager);
  changed(
    &state,
    &headers,
    peer,
    &payload.action,
    &resource,
    Some(&before),
    session_id.as_deref(),
  )
  .await;
  Json(resource).into_response()
}

#[utoipa::path(get, path = "/aperio/api/exposes/{id}/sessions", params(("id" = String, Path), ("offset" = Option<usize>, Query), ("limit" = Option<usize>, Query), ("history" = Option<bool>, Query)), description = "List active and optionally recent terminated sessions for one authorized expose.", tag = "exposes", responses((status = 200, description = "Current result", body = serde_json::Value), (status = 403, description = "Not authorized"), (status = 409, description = "Revision, port or quota conflict")))]
pub(crate) async fn sessions(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Path(id): Path<String>,
  Query(page): Query<Page>,
) -> Response {
  let caller = match caller(&state, &headers).await {
    Ok(c) => c,
    Err(r) => return r,
  };
  let manager = state.exposes.lock().await;
  let Some(resource) = manager.resource(&id) else {
    return not_found();
  };
  if let Err(r) = authorized(&caller, resource, ExposeAction::Read) {
    return r;
  }
  let mut sessions: Vec<_> = manager
    .sessions(page.history.unwrap_or(false))
    .into_iter()
    .filter(|s| s.expose_id == id)
    .collect();
  sessions.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(a.id.cmp(&b.id)));
  let total = sessions.len();
  Json(serde_json::json!({"total": total, "items": sessions.into_iter().skip(page.offset.unwrap_or(0)).take(page.limit.unwrap_or(100).min(500)).collect::<Vec<_>>()})).into_response()
}

#[utoipa::path(get, path = "/aperio/api/exposes/policies", description = "List visible organization port allocations and resource ceilings.", tag = "exposes", responses((status = 200, description = "Current result", body = serde_json::Value), (status = 403, description = "Not authorized"), (status = 409, description = "Revision, port or quota conflict")))]
pub(crate) async fn policies(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
  let caller = match caller(&state, &headers).await {
    Ok(c) => c,
    Err(r) => return r,
  };
  let manager = state.exposes.lock().await;
  Json(
    manager
      .store
      .document
      .policies
      .iter()
      .filter(|p| permits(&caller, &p.org_id, ExposeAction::Read))
      .cloned()
      .collect::<Vec<_>>(),
  )
  .into_response()
}

#[utoipa::path(put, path = "/aperio/api/exposes/policies/{org}", params(("org" = String, Path)), request_body = ExposePolicy, description = "Set an organization port policy with optimistic revision (0 for new); server administrators only. Incompatible listeners are suspended.", tag = "exposes", responses((status = 200, description = "Current result", body = serde_json::Value), (status = 403, description = "Not authorized"), (status = 409, description = "Revision, port or quota conflict")))]
pub(crate) async fn set_policy(
  State(state): State<Arc<AppState>>,
  ConnectInfo(peer): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Path(org): Path<String>,
  ExposeJson(mut policy): ExposeJson<ExposePolicy>,
) -> Response {
  let caller = match caller(&state, &headers).await {
    Ok(c) => c,
    Err(r) => return r,
  };
  if !caller.is_master_admin() {
    return failure(
      StatusCode::FORBIDDEN,
      "forbidden",
      "Only server administrators allocate public ports",
    );
  }
  if policy.org_id != org || !policy.validate().is_empty() {
    return failure(
      StatusCode::BAD_REQUEST,
      "validation",
      "Invalid expose policy",
    );
  }
  if org != "master" && state.org_store.lock().await.find(&org).is_none() {
    return failure(
      StatusCode::BAD_REQUEST,
      "invalid_org",
      "Unknown organization",
    );
  }
  let mut manager = state.exposes.lock().await;
  if manager.policy(&org).map_or(0, |p| p.revision) != policy.revision {
    return failure(
      StatusCode::CONFLICT,
      "revision_conflict",
      "Policy changed; reload it before saving",
    );
  }
  let before = manager.policy(&org).cloned();
  policy.revision += 1;
  let mut document = manager.store.document.clone();
  document.policies.retain(|p| p.org_id != org);
  document.policies.push(policy.clone());
  if let Err(error) = manager.replace(&state, document, true).await {
    return runtime_error(error);
  }
  drop(manager);
  state.audit_in("expose_policy_changed", &caller.actor(), &peer.ip().to_string(), (org != "master").then_some(org.clone()),
    &serde_json::json!({"org":org,"revision":policy.revision,"before":before,"after":policy,"outcome":"success"}).to_string()).await;
  Json(policy).into_response()
}

/// Configuration only: no sockets, traffic, credentials or file-owned rules.
#[derive(Deserialize, serde::Serialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExposeBackup {
  pub version: u32,
  pub resources: Vec<ExposeResource>,
  #[serde(default)]
  pub policies: Vec<ExposePolicy>,
}

#[utoipa::path(get, path = "/aperio/api/exposes/export", description = "Export authorized API-owned desired rules and organization policies, excluding sessions and legacy secrets.", tag = "exposes", responses((status = 200, description = "Expose backup", body = serde_json::Value)))]
pub(crate) async fn export(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
  let caller = match caller(&state, &headers).await {
    Ok(c) => c,
    Err(r) => return r,
  };
  let manager = state.exposes.lock().await;
  if manager.store.load_error.is_some() {
    return failure(
      StatusCode::CONFLICT,
      "store_recovery",
      "Expose store requires recovery; refusing an incomplete backup",
    );
  }
  Json(ExposeBackup {
    version: 1,
    resources: manager
      .store
      .document
      .resources
      .iter()
      .filter(|r| permits(&caller, &r.spec.org_id, ExposeAction::Read))
      .cloned()
      .collect(),
    policies: manager
      .store
      .document
      .policies
      .iter()
      .filter(|p| permits(&caller, &p.org_id, ExposeAction::Read))
      .cloned()
      .collect(),
  })
  .into_response()
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Restore {
  pub backup: ExposeBackup,
  #[serde(default)]
  pub org_map: std::collections::BTreeMap<String, String>,
  #[serde(default = "preview_default")]
  pub preview: bool,
  pub revision: Option<u64>,
}
fn preview_default() -> bool {
  true
}

#[utoipa::path(post, path = "/aperio/api/exposes/import", request_body = Restore, description = "Validate and preview {backup, org_map?, preview:true}. Commit with preview:false and the returned target revision. Existing IDs may only be repeated identically; conflicts are never overwritten. Policies require server administrator rights.", tag = "exposes", responses((status = 200, description = "Preview or import result", body = serde_json::Value), (status = 409, description = "Revision, identity, allocation or bind conflict")))]
pub(crate) async fn import(
  State(state): State<Arc<AppState>>,
  ConnectInfo(peer): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  ExposeJson(mut request): ExposeJson<Restore>,
) -> Response {
  let caller = match caller(&state, &headers).await {
    Ok(c) => c,
    Err(r) => return r,
  };
  if request.backup.version != 1 {
    return failure(
      StatusCode::BAD_REQUEST,
      "backup_version",
      "Unsupported expose backup version",
    );
  }
  if request.backup.resources.len() > 10000 || request.backup.policies.len() > 10000 {
    return failure(
      StatusCode::BAD_REQUEST,
      "backup_size",
      "An expose import is limited to 10000 resources and policies",
    );
  }
  for resource in &mut request.backup.resources {
    if resource.source != ExposeSource::Api {
      return failure(
        StatusCode::BAD_REQUEST,
        "file_owned",
        "File rules cannot be imported through the API",
      );
    }
    if let Some(org) = request.org_map.get(&resource.spec.org_id) {
      resource.spec.org_id = org.clone();
    }
    if !permits(&caller, &resource.spec.org_id, ExposeAction::Create) {
      return failure(
        StatusCode::FORBIDDEN,
        "forbidden",
        "Expose import requires create in every destination organization",
      );
    }
    if let Err(r) = validate(&state, &resource.spec).await {
      return r;
    }
  }
  for policy in &mut request.backup.policies {
    if let Some(org) = request.org_map.get(&policy.org_id) {
      policy.org_id = org.clone();
    }
    if !caller.is_master_admin() {
      return failure(
        StatusCode::FORBIDDEN,
        "forbidden",
        "Only server administrators may import port policies; omit policies to use existing allocations",
      );
    }
    if policy.org_id != "master" && state.org_store.lock().await.find(&policy.org_id).is_none() {
      return failure(
        StatusCode::BAD_REQUEST,
        "invalid_org",
        "Unknown policy destination organization",
      );
    }
  }
  let mut manager = state.exposes.lock().await;
  if !request.preview && request.revision != Some(manager.store.document.revision) {
    return failure(
      StatusCode::CONFLICT,
      "revision_conflict",
      "Preview again and supply the current destination revision",
    );
  }
  let mut document = manager.store.document.clone();
  let mut imported = 0;
  let imported_specs: Vec<_> = request
    .backup
    .resources
    .iter()
    .map(|r| r.spec.clone())
    .collect();
  for mut resource in request.backup.resources {
    if let Some(existing) = document.resources.iter().find(|r| r.id == resource.id) {
      if !permits(&caller, &existing.spec.org_id, ExposeAction::Read) {
        return failure(
          StatusCode::CONFLICT,
          "id_conflict",
          "An imported resource identity is unavailable",
        );
      }
      if existing.spec != resource.spec {
        return failure(
          StatusCode::CONFLICT,
          "id_conflict",
          "An imported identity has a different specification; use a new UUID or explicitly update the existing resource",
        );
      }
      continue;
    }
    if uuid::Uuid::parse_str(&resource.id).is_err() {
      return failure(
        StatusCode::BAD_REQUEST,
        "invalid_id",
        "Imported resource IDs must be UUIDs",
      );
    }
    resource.revision = 1;
    document.resources.push(resource);
    imported += 1;
  }
  for mut policy in request.backup.policies {
    if let Some(existing) = document.policies.iter().find(|p| p.org_id == policy.org_id) {
      policy.revision = existing.revision;
      if *existing != policy {
        return failure(
          StatusCode::CONFLICT,
          "policy_conflict",
          "Destination policy differs; edit it explicitly before importing",
        );
      }
    } else {
      policy.revision = 1;
      document.policies.push(policy);
    }
  }
  for spec in &imported_specs {
    if let Err(response) = bounded_change(&caller, spec, &document) {
      return response;
    }
  }
  if let Err(error) = manager.preview(&document) {
    return runtime_error(error);
  }
  let before = manager.store.document.revision;
  if request.preview {
    return Json(serde_json::json!({"preview": true, "revision": before, "resources_to_add": imported, "volatile": manager.store.volatile})).into_response();
  }
  if let Err(error) = manager.replace(&state, document, false).await {
    return runtime_error(error);
  }
  let revision = manager.store.document.revision;
  drop(manager);
  state
    .audit_session(
      "expose_changed",
      &headers,
      &peer.ip().to_string(),
      &format!("action=import resources={imported} revision={revision}"),
    )
    .await;
  Json(serde_json::json!({"preview": false, "revision": revision, "resources_added": imported}))
    .into_response()
}

#[utoipa::path(get, path = "/aperio/api/exposes/{id}/events", params(("id" = String, Path), ("offset" = Option<usize>, Query), ("limit" = Option<usize>, Query)), description = "Bounded recent mutation and runtime audit history for one authorized expose, scoped to its owner organization.", tag = "exposes", responses((status = 200, description = "Audit events", body = serde_json::Value)))]
pub(crate) async fn events(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Path(id): Path<String>,
  Query(page): Query<Page>,
) -> Response {
  let caller = match caller(&state, &headers).await {
    Ok(c) => c,
    Err(r) => return r,
  };
  let manager = state.exposes.lock().await;
  let Some(resource) = manager.resource(&id) else {
    return not_found();
  };
  if let Err(response) = authorized(&caller, resource, ExposeAction::Read) {
    return response;
  }
  let org = (resource.spec.org_id != "master").then(|| resource.spec.org_id.clone());
  drop(manager);
  let filter = crate::store::audit::AuditFilter {
    contains: Some(id.clone()),
    ..Default::default()
  };
  let events: Vec<_> = state
    .audit
    .lock()
    .await
    .search(&filter, 5000)
    .into_iter()
    .filter(|e| {
      e.org_id == org
        && matches!(
          e.event.as_str(),
          "expose_changed" | "expose_runtime" | "expose_failed"
        )
        && serde_json::from_str::<serde_json::Value>(&e.details)
          .is_ok_and(|v| v["id"].as_str() == Some(id.as_str()))
    })
    .collect();
  Json(serde_json::json!({"total":events.len(), "items": events.into_iter().skip(page.offset.unwrap_or(0)).take(page.limit.unwrap_or(100).min(500)).collect::<Vec<_>>()})).into_response()
}

/// Record rejected mutations without copying credentials, request bodies or
/// arbitrary paths into the audit log. Cross-organization resource identities
/// are only attached when the caller was allowed to read that resource.
pub(crate) async fn audit_failures(
  State(state): State<Arc<AppState>>,
  req: axum::extract::Request,
  next: axum::middleware::Next,
) -> Response {
  let path = req.uri().path();
  let expose_path = path
    .strip_prefix("/api/exposes")
    .or_else(|| path.strip_prefix("/aperio/api/exposes"));
  let Some(suffix) = expose_path.filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
  else {
    return next.run(req).await;
  };
  if !matches!(
    *req.method(),
    axum::http::Method::POST
      | axum::http::Method::PUT
      | axum::http::Method::DELETE
      | axum::http::Method::PATCH
  ) {
    return next.run(req).await;
  }
  let Some(caller) = crate::auth::resolve_caller(&state, req.headers()).await else {
    return next.run(req).await;
  };
  let method = req.method().to_string();
  let actor = state.session_actor(req.headers()).await;
  let peer = req
    .extensions()
    .get::<ConnectInfo<SocketAddr>>()
    .map(|peer| peer.0.ip().to_string())
    .unwrap_or_default();
  let mut org = caller.effective_org();
  let mut id = None;
  if let Some(candidate) = suffix.strip_prefix('/').and_then(|s| s.split('/').next()) {
    let manager = state.exposes.lock().await;
    if let Some(resource) = manager.resource(candidate)
      && permits(&caller, &resource.spec.org_id, ExposeAction::Read)
    {
      org = (resource.spec.org_id != "master").then(|| resource.spec.org_id.clone());
      id = Some(resource.id.clone());
    }
  }
  let response = next.run(req).await;
  if response.status().is_client_error() || response.status().is_server_error() {
    let details = serde_json::json!({"id":id,"method":method,"status":response.status().as_u16(),"outcome":"failure"});
    state
      .audit_in("expose_failed", &actor, &peer, org, &details.to_string())
      .await;
  }
  response
}

#[cfg(test)]
#[path = "exposes_tests.rs"]
mod tests;
