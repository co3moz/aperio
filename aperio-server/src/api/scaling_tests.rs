//! Tests for the autoscaling API and the declaration validator.

use super::*;
use crate::store::users::Role;
use crate::test_support::*;
use axum::extract::{ConnectInfo, Path, State};

fn decl(url: &str) -> ScalingDecl {
  ScalingDecl {
    url: url.to_string(),
    secret: None,
    min: 0,
    max: 4,
    cold_start: None,
    target_utilization: None,
    window: None,
    cooldown: None,
  }
}

#[test]
fn test_parse_duration_secs() {
  assert_eq!(parse_duration_secs("45s"), Ok(45));
  assert_eq!(parse_duration_secs("2m"), Ok(120));
  assert_eq!(parse_duration_secs("1h"), Ok(3_600));
  assert_eq!(parse_duration_secs("90"), Ok(90));
  assert!(parse_duration_secs("soon").is_err());
  assert!(parse_duration_secs("").is_err());
}

#[test]
fn test_record_from_decl_applies_defaults() {
  let record = record_from_decl(
    &decl("https://api.example/scale"),
    None,
    "app.example.com",
    None,
  )
  .expect("valid declaration");
  assert_eq!(record.id, "master|app.example.com|");
  assert_eq!(record.cold_start_secs, DEFAULT_COLD_START_SECS);
  assert_eq!(record.target_utilization, DEFAULT_TARGET_UTILIZATION);
  assert_eq!(record.window_secs, DEFAULT_WINDOW_SECS);
  assert_eq!(record.cooldown_secs, DEFAULT_COOLDOWN_SECS);
  assert!(!record.config_hash.is_empty());
  // min 0 with a budget is what opts a bind into scale-to-zero.
  assert!(record.cold_start_enabled());
}

#[test]
fn test_record_from_decl_parses_durations_and_clamps_the_budget() {
  let mut d = decl("https://api.example/scale");
  d.cold_start = Some("2m".into());
  d.window = Some("30s".into());
  d.cooldown = Some("5m".into());
  let record = record_from_decl(&d, None, "app.example.com", Some("/api")).unwrap();
  assert_eq!(record.cold_start_secs, 120);
  assert_eq!(record.window_secs, 30);
  assert_eq!(record.cooldown_secs, 300);
  assert_eq!(record.path.as_deref(), Some("/api"));

  // A visitor must never be held for an unbounded time.
  d.cold_start = Some("1h".into());
  let record = record_from_decl(&d, None, "app.example.com", None).unwrap();
  assert_eq!(record.cold_start_secs, MAX_COLD_START_SECS);
}

#[test]
fn test_record_from_decl_rejects_bad_declarations() {
  // No URL at all.
  assert!(record_from_decl(&decl("  "), None, "app.example.com", None).is_err());
  // Not a URL, and not an http(s) one.
  assert!(record_from_decl(&decl("nonsense"), None, "app.example.com", None).is_err());
  assert!(record_from_decl(&decl("ftp://x/y"), None, "app.example.com", None).is_err());

  // Utilization outside (0, 1].
  let mut d = decl("https://api.example/scale");
  d.target_utilization = Some(1.5);
  assert!(record_from_decl(&d, None, "app.example.com", None).is_err());
  d.target_utilization = Some(0.0);
  assert!(record_from_decl(&d, None, "app.example.com", None).is_err());
  d.target_utilization = Some(0.5);
  assert!(record_from_decl(&d, None, "app.example.com", None).is_ok());

  // A ceiling below the floor is a configuration mistake, not a policy.
  let mut d = decl("https://api.example/scale");
  d.min = 3;
  d.max = 2;
  assert!(record_from_decl(&d, None, "app.example.com", None).is_err());
  // max 0 means "cold starts only" and is legal with any min.
  d.max = 0;
  assert!(record_from_decl(&d, None, "app.example.com", None).is_ok());

  // A malformed duration is rejected rather than silently defaulted.
  let mut d = decl("https://api.example/scale");
  d.cold_start = Some("soon".into());
  assert!(record_from_decl(&d, None, "app.example.com", None).is_err());
}

#[tokio::test]
async fn list_is_org_scoped_and_never_returns_the_secret() {
  let state = Arc::new(test_state());
  let mut mine = record_from_decl(
    &decl("https://api.example/a"),
    None,
    "mine.example.com",
    None,
  )
  .unwrap();
  mine.secret = Some("super-secret".into());
  let theirs = record_from_decl(
    &decl("https://api.example/b"),
    Some("acme".into()),
    "theirs.example.com",
    None,
  )
  .unwrap();
  {
    let mut store = state.scaling_store.lock().await;
    store.upsert(mine, Some("t1"), 1);
    store.upsert(theirs, Some("t2"), 1);
  }

  let headers = admin_headers(&state).await;
  let resp = scaling_list_handler(State(state.clone()), headers).await;
  assert_eq!(resp.status(), StatusCode::OK);
  let body = json_body(resp).await;
  let rows = body.as_array().unwrap();
  assert_eq!(rows.len(), 1, "only the master org's record");
  assert_eq!(rows[0]["hostname"], "mine.example.com");
  // The secret is never rendered, only the fact that one is set.
  assert_eq!(rows[0]["authenticated"], true);
  assert!(body.to_string().find("super-secret").is_none());
  // Live pool figures ride along for the operator.
  assert_eq!(rows[0]["instances"], 0);
  assert_eq!(rows[0]["disarmed"], false);
}

#[tokio::test]
async fn delete_disarms_and_hides_other_orgs() {
  let state = Arc::new(test_state());
  let record = record_from_decl(
    &decl("https://api.example/a"),
    None,
    "mine.example.com",
    None,
  )
  .unwrap();
  let id = record.id.clone();
  let theirs = record_from_decl(
    &decl("https://api.example/b"),
    Some("acme".into()),
    "theirs.example.com",
    None,
  )
  .unwrap();
  let their_id = theirs.id.clone();
  {
    let mut store = state.scaling_store.lock().await;
    store.upsert(record, Some("t1"), 1);
    store.upsert(theirs, Some("t2"), 1);
  }
  let headers = admin_headers(&state).await;

  // Another org's record is indistinguishable from an unknown one.
  let resp = scaling_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path(their_id),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
  assert_eq!(state.scaling_store.lock().await.list().len(), 2);

  let resp = scaling_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path(id),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::OK);
  assert_eq!(state.scaling_store.lock().await.list().len(), 1);
}

#[tokio::test]
async fn delete_unknown_record_is_404() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = scaling_delete_handler(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers,
    Path("nope".to_string()),
  )
  .await;
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_for_a_viewer_of_another_org_shows_nothing() {
  let state = Arc::new(test_state());
  let record = record_from_decl(
    &decl("https://api.example/a"),
    None,
    "mine.example.com",
    None,
  )
  .unwrap();
  state.scaling_store.lock().await.upsert(record, None, 1);

  // A session bound to a child org sees only that org's records.
  let token = seed_session(&state, Role::Admin, None, Some("acme".to_string())).await;
  let resp = scaling_list_handler(State(state.clone()), cookie_headers(&token)).await;
  let body = json_body(resp).await;
  assert!(body.as_array().unwrap().is_empty());
}
