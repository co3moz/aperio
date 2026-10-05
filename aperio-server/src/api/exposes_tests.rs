//! Handler-level authorization/revision acceptance; runtime sockets are tested
//! by expose_manager::tests and router guards by the dashboard integration suite.
use super::*;
use crate::store::{
  grants::{Grant, GrantOrg},
  users::Role,
};
use crate::test_support::{cookie_headers, json_body, seed_session, test_peer, test_state};
use aperio_config::expose_policy::ExposeAllocation;

fn policy(org: &str, first: u16, last: u16) -> ExposePolicy {
  ExposePolicy {
    org_id: org.into(),
    revision: 1,
    allocations: vec![ExposeAllocation {
      address: "127.0.0.1".parse().unwrap(),
      protocol: ExposeProtocol::Tcp,
      first_port: first,
      last_port: last,
    }],
    reserved: vec![],
    max_rules: 4,
    max_tcp_connections: 1024,
    max_udp_sessions: 1024,
    ingress_bytes_per_second: 100_000_000,
    egress_bytes_per_second: 100_000_000,
  }
}
fn spec(org: &str, port: u16) -> ExposeSpec {
  ExposeSpec {
    org_id: org.into(),
    tunnel: "offline".into(),
    listener: ExposeListener {
      address: "127.0.0.1".parse().unwrap(),
      port,
      protocol: ExposeProtocol::Tcp,
    },
    enabled: false,
    limits: ExposeLimits::defaults(ExposeProtocol::Tcp),
    allowed_ips: vec![],
    advertised_host: None,
  }
}
async fn tenant(state: &Arc<AppState>, name: &str) -> String {
  let org = state
    .org_store
    .lock()
    .await
    .create(name, vec![], None)
    .unwrap()
    .id;
  let mut manager = state.exposes.lock().await;
  let mut doc = manager.store.document.clone();
  doc.policies.push(policy(&org, 20000, 20010));
  manager.replace(state, doc, false).await.unwrap();
  org
}
async fn user(
  state: &Arc<AppState>,
  name: &str,
  org: &str,
  actions: &[ExposeAction],
  bounds: Option<ExposePolicy>,
) -> (String, HeaderMap) {
  let mut grant = Grant::new(GrantOrg::Child(org.into()), Role::Viewer);
  grant.expose = actions.iter().copied().collect();
  grant.expose_bounds = bounds;
  let id = state
    .users
    .lock()
    .await
    .create_with_grants(name, "long-password", Some(org.into()), vec![grant])
    .unwrap()
    .id;
  let session = seed_session(state, Role::Viewer, Some(name), Some(org.into())).await;
  (id, cookie_headers(&session))
}
async fn make(
  state: &Arc<AppState>,
  headers: &HeaderMap,
  spec: ExposeSpec,
  id: Option<String>,
) -> Response {
  create(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    ExposeJson(Create { spec, id }),
  )
  .await
}

#[tokio::test]
async fn delegated_viewer_creates_but_plain_viewer_and_other_tenant_cannot_access() {
  let state = Arc::new(test_state());
  let org = tenant(&state, "acme").await;
  let other = tenant(&state, "beta").await;
  let (_, publisher) = user(&state, "publisher", &org, &[ExposeAction::Create], None).await;
  let (_, plain) = user(&state, "plain", &org, &[], None).await;
  let (_, foreign) = user(
    &state,
    "foreign",
    &other,
    &[ExposeAction::Read, ExposeAction::Update],
    None,
  )
  .await;
  let response = make(&state, &publisher, spec(&org, 20000), None).await;
  assert_eq!(response.status(), StatusCode::CREATED);
  let body = json_body(response).await;
  let id = body["id"].as_str().unwrap().to_string();
  for headers in [plain.clone(), foreign.clone()] {
    assert_eq!(
      detail(State(state.clone()), headers.clone(), Path(id.clone()))
        .await
        .status(),
      StatusCode::NOT_FOUND
    );
    let response = list(
      State(state.clone()),
      headers,
      Query(Page {
        offset: Some(0),
        limit: Some(1),
        ..Default::default()
      }),
    )
    .await;
    assert_eq!(json_body(response).await["total"], 0);
  }
  assert_eq!(
    make(&state, &plain, spec(&org, 20001), None).await.status(),
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    make(&state, &publisher, spec(&other, 20001), None)
      .await
      .status(),
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    update(
      State(state.clone()),
      ConnectInfo(test_peer()),
      foreign,
      Path(id.clone()),
      ExposeJson(Update {
        revision: 1,
        spec: spec(&other, 20000)
      })
    )
    .await
    .status(),
    StatusCode::NOT_FOUND
  );
  let own = detail(State(state.clone()), publisher, Path(id)).await;
  assert_eq!(json_body(own).await["target_state"], "waiting");
}

#[tokio::test]
async fn idempotency_revision_and_live_revocation_are_enforced() {
  let state = Arc::new(test_state());
  let org = tenant(&state, "acme").await;
  let (user_id, headers) = user(
    &state,
    "publisher",
    &org,
    &[ExposeAction::Create, ExposeAction::Update],
    None,
  )
  .await;
  let id = uuid::Uuid::new_v4().to_string();
  for expected in [StatusCode::CREATED, StatusCode::OK] {
    assert_eq!(
      make(&state, &headers, spec(&org, 20000), Some(id.clone()))
        .await
        .status(),
      expected
    );
  }
  assert_eq!(state.exposes.lock().await.store.document.resources.len(), 1);
  assert_eq!(
    make(&state, &headers, spec(&org, 20001), Some(id.clone()))
      .await
      .status(),
    StatusCode::CONFLICT
  );
  let response = update(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path(id.clone()),
    ExposeJson(Update {
      revision: 1,
      spec: spec(&org, 20001),
    }),
  )
  .await;
  assert_eq!(response.status(), StatusCode::OK);
  let stale = update(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path(id.clone()),
    ExposeJson(Update {
      revision: 1,
      spec: spec(&org, 20002),
    }),
  )
  .await;
  assert_eq!(stale.status(), StatusCode::CONFLICT);
  assert_eq!(json_body(stale).await["code"], "revision_conflict");
  state
    .users
    .lock()
    .await
    .set_grants(
      &user_id,
      vec![Grant::new(GrantOrg::Child(org), Role::Viewer)],
    )
    .unwrap();
  assert_eq!(
    detail(State(state.clone()), headers, Path(id.clone()))
      .await
      .status(),
    StatusCode::NOT_FOUND
  );
  assert!(
    state.exposes.lock().await.resource(&id).is_some(),
    "revocation removes control, not org resources"
  );
}

#[tokio::test]
async fn user_and_scoped_key_obey_identical_delegated_port_bounds() {
  let state = Arc::new(test_state());
  let org = tenant(&state, "acme").await;
  let bounds = policy(&org, 20000, 20000);
  let (_, session) = user(
    &state,
    "bounded",
    &org,
    &[ExposeAction::Create],
    Some(bounds.clone()),
  )
  .await;
  let (_, secret) = state
    .admin_key_store
    .lock()
    .await
    .create_with_bounds(
      "bounded-key".into(),
      Role::Viewer,
      GrantOrg::Child(org.clone()),
      None,
      [ExposeAction::Create].into_iter().collect(),
      Some(bounds),
    )
    .unwrap();
  let mut key = HeaderMap::new();
  key.insert("authorization", format!("Bearer {secret}").parse().unwrap());
  for headers in [session, key] {
    let denied = make(&state, &headers, spec(&org, 20001), None).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(denied).await["code"], "delegation_bounds");
    let mut alternate = spec(&org, 20000);
    alternate.listener.address = "0.0.0.0".parse().unwrap();
    assert_eq!(
      make(&state, &headers, alternate, None).await.status(),
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      make(&state, &headers, spec(&org, 20000), None)
        .await
        .status(),
      StatusCode::CREATED
    );
  }
}
