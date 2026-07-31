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
  state.clients.lock().await.insert(
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
  state.clients.lock().await.insert("c1".to_string(), handle);
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
  state.clients.lock().await.insert(
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
