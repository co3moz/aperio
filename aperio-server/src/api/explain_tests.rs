//! Tests for the dry-run explanation of a request.

use super::*;
use crate::state::MaintenanceFlag;
use crate::store::users::Role;
use crate::test_support::*;
use axum::extract::State;

fn q(hostname: &str, path: Option<&str>) -> Query<ExplainQuery> {
  Query(ExplainQuery {
    hostname: hostname.to_string(),
    path: path.map(str::to_string),
    method: None,
  })
}

async fn explain(
  state: &Arc<AppState>,
  headers: HeaderMap,
  query: Query<ExplainQuery>,
) -> Response {
  explain_handler(State(state.clone()), headers, query).await
}

#[test]
fn a_url_is_accepted_where_a_hostname_is() {
  // What someone has in the clipboard when they come here is a URL.
  assert_eq!(
    split_target("https://app.example.com/api/x?y=1"),
    ("app.example.com".to_string(), Some("/api/x".to_string()))
  );
  assert_eq!(
    split_target(" app.example.com "),
    ("app.example.com".to_string(), None)
  );
}

#[tokio::test]
async fn a_viewer_cannot_ask() {
  let state = Arc::new(test_state());
  let token = seed_session(&state, Role::Viewer, Some("bob"), None).await;
  let resp = explain(&state, cookie_headers(&token), q("app.example.com", None)).await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);

  let resp = explain(&state, HeaderMap::new(), q("app.example.com", None)).await;
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_tenant_cannot_ask_about_another_orgs_hostname() {
  // The report names the clients serving a hostname, which is the thing org
  // isolation exists to hide.
  let state = Arc::new(test_state());
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["acme.example".into()], None)
    .unwrap()
    .id;
  let token = seed_session(&state, Role::Admin, None, Some(org)).await;
  let headers = cookie_headers(&token);

  let resp = explain(&state, headers.clone(), q("other.example", None)).await;
  assert_eq!(resp.status(), StatusCode::FORBIDDEN);
  let resp = explain(&state, headers, q("acme.example", None)).await;
  assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_maintenance_flag_is_named_as_the_thing_answering() {
  // The case this exists for: a 503 with nothing on screen explaining it.
  let state = Arc::new(test_state());
  state.clients.write().await.insert(
    "c1".to_string(),
    mock_client(Some("app.example.com"), None, None, None),
  );
  state.maintenance.lock().await.insert(
    "*.example.com".to_string(),
    MaintenanceFlag {
      reason: Some("database migration".into()),
      actor: "aperio".into(),
      ..MaintenanceFlag::default()
    },
  );
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com", None)).await).await;

  assert_eq!(body["outcome"], "maintenance");
  let summary = body["summary"].as_str().unwrap();
  assert!(summary.contains("503"), "{summary}");
  assert!(summary.contains("database migration"), "{summary}");
  // And the routing stage still reports, because "the route is fine, the
  // flag is what answers" is half the value.
  let routing = body["steps"]
    .as_array()
    .unwrap()
    .iter()
    .find(|s| s["stage"] == "routing")
    .unwrap();
  assert_eq!(routing["verdict"], "passes");
  assert!(routing["detail"].as_str().unwrap().contains("c1"));
}

#[tokio::test]
async fn a_hostname_nothing_serves_says_so() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("nothing.example.com", None)).await).await;
  assert_eq!(body["outcome"], "no_client");
  assert!(body["summary"].as_str().unwrap().contains("504"));
}

#[tokio::test]
async fn a_client_that_could_serve_but_will_not_is_named_with_the_reason() {
  // The question behind most 504s: the client is connected, so why is it not
  // taking the request.
  let state = Arc::new(test_state());
  let mut handle = mock_client(Some("app.example.com"), None, None, None);
  handle.draining = true;
  state.clients.write().await.insert("c1".to_string(), handle);
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com", None)).await).await;

  assert_eq!(body["outcome"], "no_client");
  let routing = body["steps"]
    .as_array()
    .unwrap()
    .iter()
    .find(|s| s["stage"] == "routing")
    .unwrap();
  let detail = routing["detail"].as_str().unwrap();
  assert!(detail.contains("c1"), "{detail}");
  assert!(detail.contains("draining"), "{detail}");
}

#[tokio::test]
async fn a_reachable_route_reports_the_clients_that_would_take_it() {
  let state = Arc::new(test_state());
  state.clients.write().await.insert(
    "c1".to_string(),
    mock_client(Some("app.example.com"), None, None, None),
  );
  let headers = admin_headers(&state).await;
  let body =
    json_body(explain(&state, headers, q("https://app.example.com/api", None)).await).await;

  assert_eq!(body["outcome"], "client");
  assert_eq!(body["path"], "/api", "the path came from the URL");
  assert!(body["summary"].as_str().unwrap().contains("c1"));
}

#[tokio::test]
async fn an_invalid_hostname_is_a_bad_request() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let resp = explain(&state, headers, q("not a hostname!", None)).await;
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Runs `f` with an `aperio-server.yaml` in place, so the sections that are
/// only readable from the file (`waf:`, `rate_limits:`, `fallbacks:`) can be
/// exercised. Serialized on the shared config lock, like the other tests that
/// need a file.
fn with_server_config<T>(yaml: &str, f: impl FnOnce() -> T) -> T {
  let _lock = crate::test_support::config_lock();
  struct Cleanup;
  impl Drop for Cleanup {
    fn drop(&mut self) {
      let _ = std::fs::remove_file("aperio-server.yaml");
    }
  }
  let _cleanup = Cleanup;
  std::fs::write("aperio-server.yaml", yaml).unwrap();
  crate::config_file::reload().unwrap();
  f()
}

/// The step for one stage of a report, by name.
fn step<'a>(body: &'a serde_json::Value, stage: &str) -> &'a serde_json::Value {
  body["steps"]
    .as_array()
    .unwrap()
    .iter()
    .find(|s| s["stage"] == stage)
    .unwrap_or_else(|| panic!("no {stage} step in {body}"))
}

// Not `#[tokio::test]`: the config file has to be written and reloaded
// synchronously, under the shared lock, before a runtime exists.
#[test]
fn a_waf_rule_that_would_block_the_path_is_the_answer() {
  let (state, body) = with_server_config("waf:\n  - path: \"^/\\\\.git\"\n", || {
    let rt = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .unwrap();
    rt.block_on(async {
      let mut cfg = test_config();
      cfg.waf = crate::waf::from_config_file();
      let state = Arc::new(test_state_with(cfg));
      let headers = admin_headers(&state).await;
      let body =
        json_body(explain(&state, headers, q("app.example.com/.git/config", None)).await).await;
      (state, body)
    })
  });
  let _ = state;
  assert_eq!(body["outcome"], "waf");
  assert!(body["summary"].as_str().unwrap().contains("403"));
  assert_eq!(step(&body, "waf")["verdict"], "decides");
}

// Not `#[tokio::test]`: the config file has to be written and reloaded
// synchronously, under the shared lock, before a runtime exists.
#[test]
fn a_route_rate_limit_is_reported_without_being_spent() {
  // Asking why a request is refused must not spend the budget it is asking
  // about, so this stage reports the rule rather than consuming a token.
  let body = with_server_config(
    "rate_limits:\n  - hostname: app.example.com\n    path: /api\n    rps: 5\n    burst: 10\n",
    || {
      let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
      rt.block_on(async {
        let mut cfg = test_config();
        cfg.route_limits = crate::route_limits::from_config_file();
        let state = Arc::new(test_state_with(cfg));
        let headers = admin_headers(&state).await;
        json_body(explain(&state, headers, q("app.example.com/api/x", None)).await).await
      })
    },
  );
  let limit = step(&body, "route_rate_limit");
  assert_eq!(limit["verdict"], "passes");
  let detail = limit["detail"].as_str().unwrap();
  assert!(detail.contains("5 rps"), "{detail}");
  assert!(detail.contains("does not spend"), "{detail}");
}

// Not `#[tokio::test]`: the config file has to be written and reloaded
// synchronously, under the shared lock, before a runtime exists.
#[test]
fn a_fallback_answers_where_the_504_would_have_been() {
  let body = with_server_config(
    "fallbacks:\n  - hostname: app.example.com\n    url: https://status.example.com\n",
    || {
      let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
      rt.block_on(async {
        let mut cfg = test_config();
        cfg.fallbacks = crate::fallbacks::from_config_file();
        let state = Arc::new(test_state_with(cfg));
        let headers = admin_headers(&state).await;
        json_body(explain(&state, headers, q("app.example.com", None)).await).await
      })
    },
  );
  assert_eq!(body["outcome"], "fallback");
  let summary = body["summary"].as_str().unwrap();
  assert!(summary.contains("status.example.com"), "{summary}");
  assert_eq!(step(&body, "fallback")["verdict"], "decides");
}

#[tokio::test]
async fn a_static_route_answers_before_any_client_does() {
  use crate::static_routes::{RouteRule, StaticRoutes};
  let mut cfg = test_config();
  cfg.static_routes = StaticRoutes::compile(vec![RouteRule {
    hostname: Some("app.example.com".to_string()),
    path: Some("/old".to_string()),
    redirect: Some("https://new.example.com".to_string()),
    permanent: true,
    preserve_path: false,
    respond: None,
    ..Default::default()
  }])
  .unwrap();
  let state = Arc::new(test_state_with(cfg));
  state.clients.write().await.insert(
    "c1".to_string(),
    mock_client(Some("app.example.com"), None, None, None),
  );
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com/old", None)).await).await;

  assert_eq!(body["outcome"], "static_route");
  assert!(body["summary"].as_str().unwrap().contains("301"));
  // The client is still reported, which is the point of reporting every stage:
  // "the route is fine, a routes: rule is what answers" is a different fix
  // from "no client is connected".
  assert_eq!(step(&body, "routing")["verdict"], "passes");
  assert!(
    step(&body, "routing")["detail"]
      .as_str()
      .unwrap()
      .contains("c1")
  );
}

#[tokio::test]
async fn the_preview_robots_txt_is_named_when_it_would_answer() {
  let mut cfg = test_config();
  cfg.preview_noindex = true;
  cfg.random_subdomain_suffix = Some("*.preview.example.com".to_string());
  let state = Arc::new(test_state_with(cfg));
  let headers = admin_headers(&state).await;

  let body = json_body(
    explain(
      &state,
      headers.clone(),
      q("a1b2c3.preview.example.com/robots.txt", None),
    )
    .await,
  )
  .await;
  assert_eq!(body["outcome"], "preview_noindex");

  // Any other path on the same host is not that answer.
  let body = json_body(
    explain(
      &state,
      headers,
      q("a1b2c3.preview.example.com/index.html", None),
    )
    .await,
  )
  .await;
  assert_ne!(body["outcome"], "preview_noindex");
}

#[tokio::test]
async fn the_visitor_gate_is_reported_when_one_is_configured() {
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:password");
  let state = Arc::new(test_state_with(cfg));
  state.clients.write().await.insert(
    "c1".to_string(),
    mock_client(Some("app.example.com"), None, None, None),
  );
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com", None)).await).await;
  let gate = step(&body, "visitor_gate");
  assert_eq!(gate["verdict"], "passes");
  assert!(gate["detail"].as_str().unwrap().contains("sign in"));
}

#[tokio::test]
async fn a_maintenance_window_and_reason_are_in_the_detail() {
  let state = Arc::new(test_state());
  state.maintenance.lock().await.insert(
    "app.example.com".to_string(),
    MaintenanceFlag {
      org: None,
      reason: Some("db migration".to_string()),
      until: Some(crate::store::tokens::now_secs() + 600),
      since: crate::store::tokens::now_secs(),
      actor: "ops".to_string(),
    },
  );
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com", None)).await).await;
  assert_eq!(body["outcome"], "maintenance");
  let detail = step(&body, "maintenance")["detail"].as_str().unwrap();
  assert!(detail.contains("db migration"), "{detail}");
  assert!(detail.contains("lifting at unix"), "{detail}");
}

#[tokio::test]
async fn a_static_route_that_answers_is_the_decision() {
  let mut cfg = test_config();
  cfg.static_routes =
    crate::static_routes::StaticRoutes::compile(vec![crate::static_routes::RouteRule {
      hostname: Some("old.example.com".to_string()),
      path: None,
      redirect: Some("https://new.example.com".to_string()),
      permanent: true,
      preserve_path: false,
      respond: None,
      ..Default::default()
    }])
    .unwrap();
  let state = Arc::new(test_state_with(cfg));
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers.clone(), q("old.example.com", None)).await).await;
  assert_eq!(body["outcome"], "static_route");
  let detail = step(&body, "static_route")["detail"].as_str().unwrap();
  assert!(detail.contains("301"), "{detail}");
  assert!(detail.contains("https://new.example.com"), "{detail}");

  // A hostname the rules do not answer passes the stage instead.
  let body = json_body(explain(&state, headers, q("other.example.com", None)).await).await;
  assert_eq!(step(&body, "static_route")["verdict"], "passes");
}

// Not `#[tokio::test]`: the config file has to be written and reloaded
// synchronously, under the shared lock, before a runtime exists.
#[test]
fn a_waf_that_matches_nothing_passes_with_the_caveat() {
  let body = with_server_config("waf:\n  - path: \"^/\\\\.git\"\n", || {
    let rt = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .unwrap();
    rt.block_on(async {
      let mut cfg = test_config();
      cfg.waf = crate::waf::from_config_file();
      let state = Arc::new(test_state_with(cfg));
      let headers = admin_headers(&state).await;
      json_body(explain(&state, headers, q("app.example.com/ok", None)).await).await
    })
  });
  let waf = step(&body, "waf");
  assert_eq!(waf["verdict"], "passes");
  assert!(
    waf["detail"]
      .as_str()
      .unwrap()
      .contains("need a real request"),
    "the caveat says header and body rules cannot be dry-run"
  );
}

#[tokio::test]
async fn the_visitor_gate_names_the_servers_password() {
  let mut cfg = test_config();
  cfg.visitor_auth = crate::visitor_auth::Policy::from_credentials("user:pw");
  let state = Arc::new(test_state_with(cfg));
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com", None)).await).await;
  let gate = step(&body, "visitor_gate");
  assert_eq!(gate["verdict"], "passes");
  assert!(
    gate["detail"]
      .as_str()
      .unwrap()
      .contains("server's visitor password"),
    "{gate}"
  );
  assert_eq!(gate["setting"], "server_auth");
}

#[tokio::test]
async fn every_ineligible_reason_is_named() {
  // The question behind most 504s: a client is connected, so why is nothing
  // serving? Each way a client can be passed over is spelled out.
  let state = Arc::new(test_state());
  {
    let mut clients = state.clients.write().await;
    let mut draining = mock_client(Some("app.example.com"), None, None, None);
    draining.draining = true;
    clients.insert("c-drain".to_string(), draining);
    let mut sick = mock_client(Some("app.example.com"), None, None, None);
    sick.sole_mut().backend_healthy = false;
    clients.insert("c-sick".to_string(), sick);
    let mut wrong_path = mock_client(Some("app.example.com"), Some("/api"), None, None);
    wrong_path.sole_mut().declared_path = Some("/api".to_string());
    clients.insert("c-path".to_string(), wrong_path);
  }
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com/other", None)).await).await;
  let detail = step(&body, "routing")["detail"].as_str().unwrap();
  assert!(detail.contains("c-drain (draining)"), "{detail}");
  assert!(
    detail.contains("c-sick (its backend health probe is failing)"),
    "{detail}"
  );
  assert!(
    detail.contains("c-path (its path bind does not match)"),
    "{detail}"
  );
}

// Not `#[tokio::test]`: the config file has to be written and reloaded
// synchronously, under the shared lock, before a runtime exists.
#[test]
fn an_armed_cold_start_and_a_fallback_are_reported_in_that_order() {
  let body = with_server_config(
    "fallbacks:\n  - hostname: app.example.com\n    url: https://status.example.com\n",
    || {
      let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
      rt.block_on(async {
        let mut cfg = test_config();
        cfg.scaling_enabled = true;
        cfg.fallbacks = crate::fallbacks::from_config_file();
        let state = Arc::new(test_state_with(cfg));
        {
          let mut store = state.scaling_store.lock().await;
          let record = crate::store::scaling::ScalingRecord {
            id: crate::store::scaling::ScalingRecord::key(None, "app.example.com", None),
            org_id: None,
            hostname: "app.example.com".to_string(),
            path: None,
            url: "https://api.provider.example/scale".to_string(),
            secret: None,
            min: 0,
            max: 4,
            cold_start_secs: 45,
            target_utilization: 0.8,
            window_secs: 15,
            cooldown_secs: 60,
            owners: vec!["tok".to_string()],
            config_hash: String::new(),
            created_at: 0,
            last_seen: 0,
          };
          store.upsert(record, Some("tok"), crate::store::tokens::now_secs());
        }
        let headers = admin_headers(&state).await;
        json_body(explain(&state, headers, q("app.example.com", None)).await).await
      })
    },
  );
  let cold = step(&body, "cold_start");
  assert!(
    cold["detail"]
      .as_str()
      .unwrap()
      .contains("held while capacity"),
    "{cold}"
  );
  assert_eq!(body["outcome"], "fallback");
  let fb = step(&body, "fallback")["detail"].as_str().unwrap();
  assert!(fb.contains("302"), "{fb}");
  assert!(fb.contains("https://status.example.com"), "{fb}");
}

#[tokio::test]
async fn every_step_carries_a_code_beside_its_sentence() {
  // The dashboard renders the code, not the sentence, so a step that ships
  // without one is a step that shows up untranslated.
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com/x", None)).await).await;
  assert!(!body["summary_code"].as_str().unwrap().is_empty());
  for s in body["steps"].as_array().unwrap() {
    let code = s["code"]
      .as_str()
      .unwrap_or_else(|| panic!("no code on {s}"));
    // A code is a message name, not a sentence someone forgot to replace.
    assert!(code.contains('.') && !code.contains(' '), "{code}");
  }
}

#[tokio::test]
async fn the_values_a_sentence_interpolates_come_back_as_data() {
  // The point of the whole shape: a translator writes the sentence, the
  // server supplies what goes in it, and neither has to parse the other.
  let state = Arc::new(test_state());
  {
    let mut clients = state.clients.write().await;
    let mut draining = mock_client(Some("app.example.com"), None, None, None);
    draining.draining = true;
    clients.insert("c-drain".to_string(), draining);
  }
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com/x", None)).await).await;

  let routing = step(&body, "routing");
  assert_eq!(routing["code"], "routing.none_ineligible");
  let ineligible = routing["params"]["ineligible"].as_array().unwrap();
  assert_eq!(ineligible.len(), 1);
  assert_eq!(ineligible[0]["label"], "master@c-drain");
  assert_eq!(ineligible[0]["ids"][0], "c-drain");
  assert_eq!(ineligible[0]["reason"], "ineligible.draining");

  // Nothing serves it and no fallback covers it, so the 504 is the answer,
  // and the summary says so in both registers.
  assert_eq!(body["summary_code"], "no_client.504");
  assert_eq!(step(&body, "no_client")["code"], "no_client.504");
}

#[tokio::test]
async fn a_maintenance_flag_reports_its_actor_and_reason_as_fields() {
  let state = Arc::new(test_state());
  state.maintenance.lock().await.insert(
    "app.example.com".to_string(),
    MaintenanceFlag {
      actor: "alice".into(),
      reason: Some("db migration".into()),
      until: Some(1_800_000_000),
      ..MaintenanceFlag::default()
    },
  );
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com", None)).await).await;
  let m = step(&body, "maintenance");
  assert_eq!(m["code"], "maintenance.flagged_reason_until");
  assert_eq!(m["params"]["actor"], "alice");
  assert_eq!(m["params"]["reason"], "db migration");
  assert_eq!(m["params"]["until"], 1_800_000_000_i64);
  assert_eq!(body["summary_code"], "maintenance.flagged_reason_until");
  assert_eq!(body["summary_params"]["actor"], "alice");
}

#[tokio::test]
async fn a_client_is_named_by_its_service_rather_than_its_id() {
  // A connection id is a uuid. Nobody recognizes one, and the answer to
  // "which client is that" is on the screen next to this one.
  let state = Arc::new(test_state());
  {
    let mut clients = state.clients.write().await;
    let mut named = mock_client(Some("app.example.com"), None, None, None);
    named.sole_mut().service_name = Some("payments".to_string());
    clients.insert("11111111-aaaa".to_string(), named);
  }
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com/x", None)).await).await;

  let routing = step(&body, "routing");
  assert!(
    routing["detail"].as_str().unwrap().contains("payments"),
    "{routing}"
  );
  // The id still travels, because it is what an action addresses.
  let clients = routing["params"]["clients"].as_array().unwrap();
  assert_eq!(clients[0]["label"], "master@payments");
  assert_eq!(clients[0]["count"], 1);
  assert_eq!(clients[0]["ids"][0], "11111111-aaaa");
  assert_eq!(
    body["summary_params"]["clients"][0]["label"],
    "master@payments"
  );
}

#[tokio::test]
async fn two_connections_of_one_service_are_counted_not_repeated() {
  // A name is readable and not unique: `connections: 2` puts the same
  // service on the list twice, and "payments, payments" answers nothing.
  let state = Arc::new(test_state());
  {
    let mut clients = state.clients.write().await;
    for id in ["aaaaaaaabbbb", "ccccccccdddd"] {
      let mut c = mock_client(Some("app.example.com"), None, None, None);
      c.sole_mut().service_name = Some("payments".to_string());
      clients.insert(id.to_string(), c);
    }
  }
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com/x", None)).await).await;
  let routing = step(&body, "routing");
  let clients = routing["params"]["clients"].as_array().unwrap();
  assert_eq!(clients.len(), 1, "one service, one entry: {clients:?}");
  assert_eq!(clients[0]["label"], "master@payments");
  assert_eq!(clients[0]["count"], 2);
  // The ids are still all there, because they are what an action addresses.
  let ids: Vec<&str> = clients[0]["ids"]
    .as_array()
    .unwrap()
    .iter()
    .map(|i| i.as_str().unwrap())
    .collect();
  assert!(
    ids.contains(&"aaaaaaaabbbb") && ids.contains(&"ccccccccdddd"),
    "{ids:?}"
  );
  // And the sentence counts rather than repeats.
  assert!(
    routing["detail"]
      .as_str()
      .unwrap()
      .contains("master@payments \u{00d7}2"),
    "{routing}"
  );
}

#[tokio::test]
async fn an_ineligible_client_is_named_the_same_way() {
  let state = Arc::new(test_state());
  {
    let mut clients = state.clients.write().await;
    let mut draining = mock_client(Some("app.example.com"), None, None, None);
    draining.draining = true;
    draining.sole_mut().service_custom_name = Some("checkout (blue)".to_string());
    clients.insert("22222222-bbbb".to_string(), draining);
  }
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com/x", None)).await).await;
  let routing = step(&body, "routing");
  assert!(
    routing["detail"]
      .as_str()
      .unwrap()
      .contains("master@checkout (blue) (draining)"),
    "{routing}"
  );
  let first = &routing["params"]["ineligible"][0];
  assert_eq!(first["label"], "master@checkout (blue)");
  assert_eq!(first["count"], 1);
  assert_eq!(first["ids"][0], "22222222-bbbb");
  assert_eq!(first["reason"], "ineligible.draining");
}

#[tokio::test]
async fn a_tenant_is_not_told_who_else_serves_a_hostname_its_fence_covers() {
  // The fence is on the hostname, and a wildcard fence can cover a name that
  // somebody else's client also serves. Being allowed to ask about the
  // hostname is not being allowed to learn whose deployment answers it: the
  // client is counted, so the 504 still explains itself, and nothing else
  // about it is said.
  let state = Arc::new(test_state());
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["*.example.com".into()], None)
    .unwrap()
    .id;
  {
    let mut clients = state.clients.write().await;
    let mut foreign = mock_client(Some("app.example.com"), None, None, None);
    foreign.sole_mut().service_name = Some("axum".to_string());
    clients.insert("master-conn".to_string(), foreign);
  }
  let token = seed_session(&state, Role::Admin, None, Some(org)).await;
  let body =
    json_body(explain(&state, cookie_headers(&token), q("app.example.com", None)).await).await;

  let routing = step(&body, "routing");
  let rendered = format!("{routing}{}", body["summary"]);
  assert!(
    !rendered.contains("axum"),
    "the service name leaked: {rendered}"
  );
  assert!(
    !rendered.contains("master-conn"),
    "the connection id leaked: {rendered}"
  );
  let clients = routing["params"]["clients"].as_array().unwrap();
  assert_eq!(clients.len(), 1);
  assert_eq!(clients[0]["label_code"], "client.other_org");
  assert_eq!(clients[0]["count"], 1);
  assert!(clients[0]["label"].is_null() && clients[0]["ids"].is_null());
}

#[tokio::test]
async fn a_foreign_client_that_cannot_serve_keeps_its_reason_to_itself() {
  // "draining" is a fact about someone else's deployment. The caller asked
  // why their own request would not be served, and the count answers that.
  let state = Arc::new(test_state());
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["*.example.com".into()], None)
    .unwrap()
    .id;
  {
    let mut clients = state.clients.write().await;
    let mut foreign = mock_client(Some("app.example.com"), None, None, None);
    foreign.draining = true;
    foreign.sole_mut().service_name = Some("axum".to_string());
    clients.insert("master-conn".to_string(), foreign);
  }
  let token = seed_session(&state, Role::Admin, None, Some(org)).await;
  let body =
    json_body(explain(&state, cookie_headers(&token), q("app.example.com", None)).await).await;

  let routing = step(&body, "routing");
  let first = &routing["params"]["ineligible"][0];
  assert_eq!(first["label_code"], "client.other_org");
  assert!(first["reason"].is_null(), "the reason leaked: {routing}");
  assert!(
    !routing["detail"].as_str().unwrap().contains("draining"),
    "{routing}"
  );
}

#[tokio::test]
async fn a_tenant_sees_its_own_clients_named_and_qualified() {
  // The fence only hides what is somebody else's. Inside it the report is
  // the report, and the name carries the organization for the same reason a
  // tunnel handle does: it is unique in there and nowhere else.
  let state = Arc::new(test_state());
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["acme.example".into()], None)
    .unwrap()
    .id;
  {
    let mut clients = state.clients.write().await;
    let mut mine = mock_client(Some("acme.example"), None, None, None);
    mine.sole_mut().service_name = Some("axum".to_string());
    mine.perms.org_id = Some(org.clone());
    clients.insert("tenant-conn".to_string(), mine);
  }
  let token = seed_session(&state, Role::Admin, None, Some(org)).await;
  let body =
    json_body(explain(&state, cookie_headers(&token), q("acme.example", None)).await).await;
  let clients = step(&body, "routing")["params"]["clients"]
    .as_array()
    .unwrap();
  assert_eq!(clients[0]["label"], "acme@axum");
  assert_eq!(clients[0]["ids"][0], "tenant-conn");
}

// ---------------------------------------------------------------------------
// The explainer against the request path it claims to walk
// ---------------------------------------------------------------------------

/// Every refusal the proxy can hand out has a stage here that names it.
///
/// This is the whole reason the endpoint is trustworthy. It is a second
/// implementation of the request path's decision order, and a second
/// implementation drifts: when it was written it covered one of the eight
/// limits, and the other seven were missing for months without a single test
/// going red. `ALL_LIMITS` exists so a counter can iterate the kinds; it works
/// just as well as the list this has to cover.
///
/// The anchor is `Limit::setting()` rather than a stage name, because that is
/// the string an operator searches for when they want the number changed, and
/// a stage that cannot name where the number lives is not an explanation.
#[tokio::test]
async fn every_limit_the_proxy_can_refuse_with_is_explained() {
  let state = Arc::new(test_state());
  // A client, so the stages that only exist once routing has picked somebody
  // are reached: without one the report stops at the 504.
  state.clients.write().await.insert(
    "c1".to_string(),
    mock_client(Some("app.example.com"), None, None, None),
  );
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com", None)).await).await;

  let settings: Vec<&str> = body["steps"]
    .as_array()
    .expect("steps is a list")
    .iter()
    .filter_map(|s| s["setting"].as_str())
    .collect();

  for limit in crate::limits::ALL_LIMITS {
    assert!(
      settings.contains(&limit.setting()),
      "no stage names {:?}, whose setting is `{}`; the proxy can refuse a \
       request with it and this report would not say so. Add a stage for it \
       rather than relaxing this test.",
      limit,
      limit.setting()
    );
  }
}

/// The dry run stays a dry run: asking does not spend the visitor's bucket.
///
/// The per-IP limit is the one stage that could not be reported by doing what
/// the proxy does, because doing it is what the proxy charges for. Somebody
/// debugging a 429 by refreshing this page would otherwise be feeding it.
#[tokio::test]
async fn explaining_a_route_does_not_spend_the_rate_limit_it_reports() {
  let mut cfg = test_config();
  cfg.ip_limit_max = 2.0;
  cfg.ip_limit_refill = 0.0;
  let state = Arc::new(test_state_with(cfg));
  let headers = admin_headers(&state).await;
  for _ in 0..5 {
    let _ = explain(&state, headers.clone(), q("app.example.com", None)).await;
  }
  let ip = test_peer().ip();
  assert!(
    state.check_rate_limit(ip).await && state.check_rate_limit(ip).await,
    "five explanations must leave a bucket of two untouched"
  );
}

/// A stage that reports a rule rather than a verdict says which it is doing.
///
/// The deny list, the token quotas and the per-IP bucket cannot be evaluated
/// for a request nobody sends, and reporting them as `passes` without saying
/// so would be the confident wrong answer this endpoint exists to prevent.
#[tokio::test]
async fn the_stages_that_cannot_be_evaluated_say_what_they_are_reporting() {
  let state = Arc::new(test_state());
  let headers = admin_headers(&state).await;
  let body = json_body(explain(&state, headers, q("app.example.com", None)).await).await;
  let step = |stage: &str| -> String {
    body["steps"]
      .as_array()
      .unwrap()
      .iter()
      .find(|s| s["stage"] == stage)
      .unwrap_or_else(|| panic!("no {stage} stage"))["detail"]
      .as_str()
      .unwrap()
      .to_string()
  };
  assert!(
    step("rate_limit_ip").contains("does not spend"),
    "the bucket stage must say it is not charging: {}",
    step("rate_limit_ip")
  );
  assert!(
    step("token_quota").contains("not a property of this route"),
    "the token stage must say why it cannot answer: {}",
    step("token_quota")
  );
}

/// Every code this endpoint can send has an entry in the dashboard's
/// catalogue.
///
/// There is already a test for this, in TypeScript, reading this same Rust
/// file. It is in the right place for one direction and the wrong place for
/// the other, and the wrong one is what happened: adding eleven stages here is
/// a Rust change, so the work ran `cargo test`, `clippy` and `e2e` and never
/// ran `vitest`. The check stayed red across five commits with every gate
/// anybody looked at green.
///
/// So the same assertion lives on both sides now, because the two catch
/// different mistakes and only from their own side. This one catches a code
/// added here with no entry there, which is a Rust change. The TypeScript one
/// catches an entry there for a code no longer sent from here, which is a
/// dashboard concern and a dashboard change. Neither is redundant; what was
/// redundant was having only one of them.
///
/// A missing entry is silent by design, the dashboard falls back to the
/// server's English `detail`, so the screen reads correctly in one language
/// and wrongly in seven.
#[test]
fn every_message_code_has_a_dashboard_entry() {
  const HANDLER: &str = include_str!("explain.rs");
  let catalogue = std::fs::read_to_string("../aperio-dashboard/src/lib/explainMessages.ts")
    .expect("the dashboard's explain catalogue");

  // A code is written as a `"stage.message"` literal. The `#[path = "..."]`
  // attribute wiring the test module in matches that shape and is not one.
  let codes: Vec<&str> = HANDLER
    .match_indices('"')
    .filter_map(|(i, _)| {
      let rest = &HANDLER[i + 1..];
      let end = rest.find('"')?;
      let lit = &rest[..end];
      let looks_like_a_code = lit.split_once('.').is_some_and(|(a, b)| {
        !a.is_empty()
          && !b.is_empty()
          && a.chars().all(|c| c.is_ascii_lowercase() || c == '_')
          && b
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
      });
      (looks_like_a_code && !lit.ends_with(".rs")).then_some(lit)
    })
    .collect();
  assert!(
    codes.len() > 20,
    "only {} codes found; the scan is looking in the wrong place",
    codes.len()
  );

  let mut missing: Vec<&str> = codes
    .into_iter()
    .filter(|c| !catalogue.contains(&format!("'{c}'")))
    .collect();
  missing.sort_unstable();
  missing.dedup();
  assert!(
    missing.is_empty(),
    "codes with no entry in aperio-dashboard/src/lib/explainMessages.ts: {}.\n\n\
     A code with no entry falls back to the server's English sentence, so the \
     screen still reads and only a reader in another language notices. Add \
     them to EXPLAIN_MESSAGES (or EXPLAIN_INELIGIBLE / EXPLAIN_SETTINGS), and \
     to the seven dictionaries in aperio-dashboard/src/i18n.",
    missing.join(", ")
  );
}

/// A tenant is told the server's concurrency rule, never its live load.
///
/// Every other stage in this report is configuration, which an operator is
/// entitled to read. The in-flight count is not: it is the whole server's
/// load this instant, across every organization, so a tenant polling this
/// endpoint could read the traffic of tenants they cannot otherwise see. That
/// number appears nowhere else an org-scoped caller can reach, and it arrived
/// here with the limits stages, so it left again the same way.
#[tokio::test]
async fn a_tenant_is_not_told_the_servers_live_load() {
  let state = Arc::new(test_state());
  let org = state
    .org_store
    .lock()
    .await
    .create("acme", vec!["acme.example".into()], None)
    .unwrap()
    .id;
  // A *named* user belonging to the organization, which is what a tenant is.
  // A master admin who merely selected an org is still a master and is meant
  // to keep seeing the number; the session's selection is a view, not a
  // boundary.
  state
    .users
    .lock()
    .await
    .create("dan", "password1", Role::Operator, Some(org.clone()))
    .unwrap();
  let tenant = cookie_headers(&seed_session(&state, Role::Operator, Some("dan"), Some(org)).await);
  let body = json_body(explain(&state, tenant, q("acme.example", None)).await).await;
  let step = body["steps"]
    .as_array()
    .unwrap()
    .iter()
    .find(|s| s["stage"] == "server_concurrency")
    .expect("the stage is still reported");
  assert_eq!(step["code"], "server_concurrency.ceiling");
  assert!(
    step["params"]["in_flight"].is_null(),
    "a tenant must not be handed the live count: {}",
    step["params"]
  );
  assert!(
    !step["detail"].as_str().unwrap().contains("right now"),
    "the sentence must not describe the moment either: {}",
    step["detail"]
  );

  // The master sees it, because the number is only useful to somebody who can
  // act on it and they are already entitled to every organization's traffic.
  // A hostname no organization claims, since the master's own view is the
  // master organization and the stage is server-wide either way.
  let master = admin_headers(&state).await;
  let body = json_body(explain(&state, master, q("unclaimed.example", None)).await).await;
  let step = body["steps"]
    .as_array()
    .unwrap()
    .iter()
    .find(|s| s["stage"] == "server_concurrency")
    .unwrap();
  assert_eq!(step["code"], "server_concurrency.headroom");
  assert!(step["params"]["in_flight"].is_number());
}
