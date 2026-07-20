//! Tests for the configuration dump export/import dashboard API.

use super::*;
use crate::store::users::Role;
use crate::test_support::*;
use axum::extract::{ConnectInfo, State};

fn import_dump(
  format_version: u32,
  tokens: Option<Vec<ApiToken>>,
  webhooks: Option<Vec<Webhook>>,
  users: Option<Vec<User>>,
  organizations: Option<Vec<Organization>>,
  settings_overrides: Option<SettingsOverrides>,
) -> Json<ImportDump> {
  Json(ImportDump {
    format_version,
    tokens,
    webhooks,
    users,
    settings_overrides,
    organizations,
  })
}

// ---- export_handler ----

#[tokio::test]
async fn export_requires_authentication() {
  let state = Arc::new(test_state());
  let resp = export_handler(State(state), ConnectInfo(test_peer()), HeaderMap::new()).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn export_forbidden_for_non_master_admin() {
  let state = Arc::new(test_state());
  let token = seed_session(&state, Role::Viewer, Some("bob"), None).await;
  let resp = export_handler(
    State(state),
    ConnectInfo(test_peer()),
    cookie_headers(&token),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn export_empty_state_returns_dump() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = export_handler(State(state), ConnectInfo(test_peer()), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);

  // Headers: JSON content-type and an attachment filename.
  let headers = resp.headers();
  assert_eq!(headers["content-type"], "application/json");
  let cd = headers["content-disposition"].to_str().unwrap();
  assert!(
    cd.starts_with("attachment; filename=\"aperio-export-"),
    "{cd}"
  );
  assert!(cd.ends_with(".json\""), "{cd}");

  let body = json_body(resp).await;
  assert_eq!(body["format_version"], FORMAT_VERSION);
  assert_eq!(body["server_version"], env!("CARGO_PKG_VERSION"));
  assert!(body["exported_at"].is_string());
  assert_eq!(body["tokens"].as_array().unwrap().len(), 0);
  assert_eq!(body["webhooks"].as_array().unwrap().len(), 0);
  assert_eq!(body["users"].as_array().unwrap().len(), 0);
  assert_eq!(body["organizations"].as_array().unwrap().len(), 0);
  assert!(body["settings_overrides"].is_object());
}

#[tokio::test]
async fn export_includes_seeded_data() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  // Seed an organization so the dump has a non-empty section.
  state.org_store.lock().await.create("acme").unwrap();

  let resp = export_handler(State(state), ConnectInfo(test_peer()), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let orgs = body["organizations"].as_array().unwrap();
  assert_eq!(orgs.len(), 1);
  assert_eq!(orgs[0]["name"], "acme");
}

// ---- import_handler ----

#[tokio::test]
async fn import_requires_authentication() {
  let state = Arc::new(test_state());
  let resp = import_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    import_dump(FORMAT_VERSION, None, None, None, None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn import_forbidden_for_non_master_admin() {
  let state = Arc::new(test_state());
  let token = seed_session(&state, Role::Viewer, Some("bob"), None).await;
  let resp = import_handler(
    State(state),
    ConnectInfo(test_peer()),
    cookie_headers(&token),
    import_dump(FORMAT_VERSION, None, None, None, None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn import_rejects_unsupported_format_version() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = import_handler(
    State(state),
    ConnectInfo(test_peer()),
    headers,
    import_dump(FORMAT_VERSION + 1, None, None, None, None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
  let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
    .await
    .unwrap();
  let text = String::from_utf8(bytes.to_vec()).unwrap();
  assert!(text.contains("Unsupported format_version"), "{text}");
}

#[tokio::test]
async fn import_rejects_invalid_settings_overrides() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let overrides = SettingsOverrides {
    lb_strategy: Some("not-a-strategy".to_string()),
    ..Default::default()
  };
  let resp = import_handler(
    State(state),
    ConnectInfo(test_peer()),
    headers,
    import_dump(FORMAT_VERSION, None, None, None, None, Some(overrides)),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
  let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
    .await
    .unwrap();
  let text = String::from_utf8(bytes.to_vec()).unwrap();
  assert!(text.contains("settings_overrides rejected"), "{text}");
}

#[tokio::test]
async fn import_no_sections_is_ok_with_empty_counts() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = import_handler(
    State(state),
    ConnectInfo(test_peer()),
    headers,
    import_dump(FORMAT_VERSION, None, None, None, None, None),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  assert_eq!(body["imported"].as_object().unwrap().len(), 0);
}

#[tokio::test]
async fn import_all_sections_applies_and_reports_counts() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  // A valid settings override plus every store section (empty vectors still
  // exercise each `if let Some(..)` import branch and record a count key).
  let overrides = SettingsOverrides {
    max_tunnels: Some(4),
    ..Default::default()
  };
  let resp = import_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    import_dump(
      FORMAT_VERSION,
      Some(Vec::new()),
      Some(Vec::new()),
      Some(Vec::new()),
      Some(Vec::new()),
      Some(overrides),
    ),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let imported = body["imported"].as_object().unwrap();
  assert_eq!(imported["tokens"], 0);
  assert_eq!(imported["webhooks"], 0);
  assert_eq!(imported["users"], 0);
  assert_eq!(imported["organizations"], 0);
}
