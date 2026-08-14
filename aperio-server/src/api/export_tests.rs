//! Tests for the configuration dump export/import dashboard API.

use super::*;
use crate::store::tokens::TokenSpec;
use crate::store::users::Role;
use crate::test_support::*;
use axum::extract::{ConnectInfo, Query, State};

/// The default section set (what `?include=` unset means).
fn all() -> Query<ExportQuery> {
  Query(ExportQuery { include: None })
}

/// `?include=<names>`.
fn include(names: &str) -> Query<ExportQuery> {
  Query(ExportQuery {
    include: Some(names.to_string()),
  })
}

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
    scaling: None,
    statistics: None,
    uptime: None,
    activity: None,
    inbox: None,
    admin_keys: None,
  })
}

// ---- export_handler ----

#[tokio::test]
async fn export_requires_authentication() {
  let state = Arc::new(test_state());
  let resp = export_handler(
    State(state),
    ConnectInfo(test_peer()),
    HeaderMap::new(),
    all(),
  )
  .await;
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
    all(),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn export_empty_state_returns_dump() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = export_handler(State(state), ConnectInfo(test_peer()), headers, all()).await;
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
  state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap();

  let resp = export_handler(State(state), ConnectInfo(test_peer()), headers, all()).await;
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

// ---- Section selection ----

#[tokio::test]
async fn the_default_dump_is_the_configuration_sections() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let body =
    json_body(export_handler(State(state), ConnectInfo(test_peer()), headers, all()).await).await;
  // What this endpoint always wrote, so a script that predates `include`
  // keeps getting it.
  for key in [
    "tokens",
    "webhooks",
    "users",
    "organizations",
    "scaling",
    "settings_overrides",
  ] {
    assert!(!body[key].is_null(), "{key} missing from the default dump");
  }
  // And what it did not: history is opt-in.
  for key in ["statistics", "uptime", "activity", "inbox", "admin_keys"] {
    assert!(body[key].is_null(), "{key} should be opt-in");
  }
}

#[tokio::test]
async fn include_selects_exactly_what_was_asked_for() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let body = json_body(
    export_handler(
      State(state),
      ConnectInfo(test_peer()),
      headers,
      include("statistics, uptime"),
    )
    .await,
  )
  .await;
  assert!(body["statistics"].is_object());
  assert!(body["uptime"].is_object());
  assert!(body["tokens"].is_null());
  assert_eq!(
    body["sections"].as_array().unwrap().len(),
    2,
    "the dump says what it holds"
  );
}

#[tokio::test]
async fn a_misspelled_section_is_refused_rather_than_dropped() {
  // Silently ignoring it would hand back a backup missing exactly the thing
  // that was asked for, which is the one way a backup must not fail.
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = export_handler(
    State(state),
    ConnectInfo(test_peer()),
    headers,
    include("tokens,statistic"),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
  let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
    .await
    .unwrap();
  let text = String::from_utf8(bytes.to_vec()).unwrap();
  assert!(text.contains("statistic"), "{text}");
  assert!(text.contains("statistics"), "names the known ones: {text}");
}

#[tokio::test]
async fn without_organizations_only_masters_rows_travel() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap()
    .id;
  {
    let mut tokens = state.token_store.lock().await;
    tokens.create(TokenSpec {
      name: "master-one".into(),
      ..Default::default()
    });
    tokens.create(TokenSpec {
      name: "acme-one".into(),
      org_id: Some(org.clone()),
      ..Default::default()
    });
  }
  state
    .persistent_stats
    .lock()
    .await
    .record_request(true, 1, 2, 3, Some(&org));

  // Without the organizations section, a child org's rows would land on a
  // server where that organization does not exist.
  let body = json_body(
    export_handler(
      State(state.clone()),
      ConnectInfo(test_peer()),
      headers.clone(),
      include("tokens,statistics"),
    )
    .await,
  )
  .await;
  let tokens = body["tokens"].as_array().unwrap();
  assert_eq!(tokens.len(), 1);
  assert_eq!(tokens[0]["name"], "master-one");
  assert!(
    body["statistics"]["by_org"][&org].is_null(),
    "the org's slice went with it"
  );
  // The global aggregate is this server's own total, not an organization's,
  // so it stays.
  assert_eq!(body["statistics"]["total_requests"], 1);

  // Ask for the organizations too and everything travels.
  let body = json_body(
    export_handler(
      State(state),
      ConnectInfo(test_peer()),
      headers,
      include("tokens,statistics,organizations"),
    )
    .await,
  )
  .await;
  assert_eq!(body["tokens"].as_array().unwrap().len(), 2);
  assert!(body["statistics"]["by_org"][&org].is_object());
}

#[tokio::test]
async fn a_dump_of_history_imports_back() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  state
    .persistent_stats
    .lock()
    .await
    .record_request(true, 10, 20, 30, None);
  let exported = json_body(
    export_handler(
      State(state.clone()),
      ConnectInfo(test_peer()),
      headers.clone(),
      include("statistics"),
    )
    .await,
  )
  .await;

  // A fresh server reads it back and has the history.
  let target = Arc::new(test_state());
  let target_headers = admin_headers(&target).await;
  let dump: ImportDump = serde_json::from_value(exported).unwrap();
  let resp = import_handler(
    State(target.clone()),
    ConnectInfo(test_peer()),
    target_headers,
    Json(dump),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(target.persistent_stats.lock().await.lifetime_requests(), 1);
}

/// A minimal inbox row for the history-section tests.
fn inbox_entry(id: &str) -> crate::store::inbox::InboxEntry {
  crate::store::inbox::InboxEntry {
    id: id.to_string(),
    timestamp: chrono::Local::now().to_rfc3339(),
    method: "POST".to_string(),
    uri: "/hook".to_string(),
    host: None,
    headers: Vec::new(),
    body: None,
    body_truncated: false,
    status: 200,
    service: None,
    org_id: None,
  }
}

#[tokio::test]
async fn the_history_sections_travel_and_stay_org_fenced() {
  // The three history sections nobody exports until a migration: uptime,
  // inbox and admin keys. Each carries an org_id, so leaving organizations
  // out of the dump must drop the tenant rows, not orphan them.
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", Vec::new(), None)
    .unwrap()
    .id;
  {
    let mut live = std::collections::HashMap::new();
    live.insert(
      "host:master.example".to_string(),
      (crate::store::uptime::Availability::Up, None),
    );
    live.insert(
      "host:acme.example".to_string(),
      (crate::store::uptime::Availability::Up, Some(org.clone())),
    );
    state
      .uptime
      .lock()
      .await
      .tick(crate::store::tokens::now_secs(), live);
  }
  {
    let mut inbox = state.inbox_store.lock().await;
    let mut master_row = inbox_entry("m1");
    master_row.org_id = None;
    inbox.insert(master_row);
    let mut org_row = inbox_entry("a1");
    org_row.org_id = Some(org.clone());
    inbox.insert(org_row);
  }
  {
    let mut keys = state.admin_key_store.lock().await;
    keys.create("master-key".into(), Role::Admin, None, None);
    keys.create("acme-key".into(), Role::Admin, Some(org.clone()), None);
  }

  // With organizations: both sides of every section travel.
  let full = json_body(
    export_handler(
      State(state.clone()),
      ConnectInfo(test_peer()),
      headers.clone(),
      include("organizations,uptime,inbox,admin_keys"),
    )
    .await,
  )
  .await;
  assert_eq!(full["uptime"].as_object().unwrap().len(), 2);
  assert_eq!(full["inbox"].as_array().unwrap().len(), 2);
  assert_eq!(full["admin_keys"].as_array().unwrap().len(), 2);

  // Without them: only master's rows.
  let fenced = json_body(
    export_handler(
      State(state.clone()),
      ConnectInfo(test_peer()),
      headers,
      include("uptime,inbox,admin_keys"),
    )
    .await,
  )
  .await;
  assert_eq!(fenced["uptime"].as_object().unwrap().len(), 1);
  assert_eq!(fenced["inbox"].as_array().unwrap().len(), 1);
  assert_eq!(fenced["admin_keys"].as_array().unwrap().len(), 1);

  // And a fresh server imports the full dump back, every section applied.
  let target = Arc::new(test_state());
  let target_headers = admin_headers(&target).await;
  let dump: ImportDump = serde_json::from_value(full).unwrap();
  let resp = import_handler(
    State(target.clone()),
    ConnectInfo(test_peer()),
    target_headers,
    Json(dump),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(target.uptime.lock().await.snapshot().len(), 2);
  assert_eq!(target.inbox_store.lock().await.list_all().len(), 2);
  assert_eq!(target.admin_key_store.lock().await.list().len(), 2);
}

#[tokio::test]
async fn the_activity_rings_travel_with_a_dump() {
  // They are history, like the statistics and the uptime beside them, and a
  // restore that carries those and not this leaves the two-hour and one-day
  // charts blank on a server whose every other number came across.
  let state = Arc::new(test_state());
  let now = crate::store::tokens::now_secs();
  {
    let mut activity = state.activity.lock().await;
    activity.record(None, false, now);
    activity.record(None, true, now);
    activity.record(Some("acme"), false, now);
  }
  let headers = admin_headers(&state).await;
  let body = json_body(
    export_handler(
      State(state.clone()),
      ConnectInfo(test_peer()),
      headers,
      include("activity, organizations"),
    )
    .await,
  )
  .await;
  assert!(body["activity"].is_object(), "got {body}");

  // Into a server that has served nothing.
  let target = Arc::new(test_state());
  let dump: ImportDump = serde_json::from_value(body).unwrap();
  let headers = admin_headers(&target).await;
  let resp = import_handler(
    State(target.clone()),
    ConnectInfo(test_peer()),
    headers,
    Json(dump),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);

  let restored = target.activity.lock().await;
  let totals = |org: Option<&str>| {
    restored
      .series(org, crate::state::ActivityRange::TwoHours, now)
      .iter()
      .map(|b| b.total)
      .sum::<u32>()
  };
  assert_eq!(totals(None), 2, "master's traffic came across");
  assert_eq!(totals(Some("acme")), 1, "and the organization's did too");
  // The fine ring is deliberately not carried: fifteen minutes of five-second
  // slices is the view of *right now*, which a dump cannot hold.
  assert_eq!(
    restored
      .series(None, crate::state::ActivityRange::Quarter, now)
      .iter()
      .map(|b| b.total)
      .sum::<u32>(),
    0
  );
}
