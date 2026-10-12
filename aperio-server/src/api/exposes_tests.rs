//! Handler-level authorization/revision acceptance; runtime sockets are tested
//! by expose_manager::tests and router guards by the dashboard integration suite.
//! What is pinned: a caller's role in the listener's organization decides what
//! it may do, and the organization's port allocation is the one bound on what
//! anyone but the server admin opens.
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
/// Replaces every organization's allocation with `allocations`.
async fn allocate(state: &Arc<AppState>, allocations: Vec<ExposePolicy>) {
  let mut manager = state.exposes.lock().await;
  let mut document = manager.store.document.clone();
  document.policies = allocations;
  manager.replace(state, document, false).await.unwrap();
}
/// A named user holding `role` in organization `org` (`master` for master),
/// signed in.
async fn user(state: &Arc<AppState>, name: &str, org: &str, role: Role) -> (String, HeaderMap) {
  let home = (org != "master").then(|| org.to_string());
  let id = state
    .users
    .lock()
    .await
    .create_with_grants(
      name,
      "long-password",
      home.clone(),
      vec![Grant::new(GrantOrg::parse(org), role)],
    )
    .unwrap()
    .id;
  let session = seed_session(state, Role::Viewer, Some(name), home).await;
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
async fn the_allocation_keeps_a_draining_listeners_reservation() {
  let state = Arc::new(test_state());
  let org = tenant(&state, "draining").await;
  let mut allocation = policy(&org, 1, u16::MAX);
  allocation.max_tcp_connections = 256;
  allocate(&state, vec![allocation]).await;
  let (_, headers) = user(&state, "publisher", &org, Role::Admin).await;
  let (tx, mut rx) = tokio::sync::mpsc::channel(16);
  let mut client = crate::test_support::mock_client(None, None);
  client.tx = tx;
  client.perms.org_id = Some(org.clone());
  client.sole_mut().tunnels = vec![crate::protocol::TunnelDecl {
    name: Some("offline".into()),
    custom_name: None,
    target: "127.0.0.1:9000".into(),
    protocol: "tcp".into(),
    encrypt: false,
    idle_timeout: None,
  }];
  state
    .clients
    .write()
    .await
    .insert("declaring-client".into(), client);
  let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = socket.local_addr().unwrap().port();
  drop(socket);
  let mut first = spec(&org, port);
  first.enabled = true;
  let created = make(&state, &headers, first, None).await;
  assert_eq!(created.status(), StatusCode::CREATED);
  let id = json_body(created).await["id"].as_str().unwrap().to_string();
  let visitor = tokio::net::TcpStream::connect(("127.0.0.1", port))
    .await
    .unwrap();
  tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
    .await
    .unwrap()
    .unwrap();
  let drained = operate(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path(id),
    ExposeJson(Operation {
      revision: 1,
      action: "drain".into(),
      session_id: None,
      drain_seconds: Some(60),
    }),
  )
  .await;
  assert_eq!(drained.status(), StatusCode::OK);
  let mut second = spec(&org, if port == 65535 { port - 1 } else { port + 1 });
  second.enabled = true;
  let denied = make(&state, &headers, second, None).await;
  assert_eq!(denied.status(), StatusCode::FORBIDDEN);
  assert_eq!(json_body(denied).await["code"], "outside_allocation");
  state.exposes.lock().await.shutdown().await;
  drop(visitor);
}

#[tokio::test]
async fn reused_identity_does_not_disclose_another_organizations_session_history() {
  let state = Arc::new(test_state());
  let original = tenant(&state, "original").await;
  let recipient = tenant(&state, "recipient").await;
  let (_, owner) = user(&state, "owner", &original, Role::Admin).await;
  let (_, publisher) = user(&state, "publisher", &recipient, Role::Admin).await;
  let (_, viewer) = user(&state, "reader", &recipient, Role::Viewer).await;
  let id = uuid::Uuid::new_v4().to_string();
  assert_eq!(
    make(&state, &owner, spec(&original, 20000), Some(id.clone()))
      .await
      .status(),
    StatusCode::CREATED
  );
  let mut history = crate::expose_manager::SessionView {
    id: uuid::Uuid::new_v4().to_string(),
    expose_id: id.clone(),
    org_id: original,
    revision: 1,
    protocol: ExposeProtocol::Tcp,
    peer: "192.0.2.10:4567".parse().unwrap(),
    client_id: "private-client".into(),
    target: "10.0.0.10:5432".into(),
    started_at: crate::store::tokens::now_secs(),
    up_bytes: 10,
    down_bytes: 20,
    up_packets: 1,
    down_packets: 1,
    idle_seconds: 0,
    ended_at: Some(crate::store::tokens::now_secs()),
    reason: Some("relay_closed".into()),
  };
  state.exposes.lock().await.store.record_session(&history);
  assert_eq!(
    delete(
      State(state.clone()),
      ConnectInfo(test_peer()),
      owner,
      Path(id.clone()),
      ExposeJson(Revision { revision: 1 })
    )
    .await
    .status(),
    StatusCode::NO_CONTENT
  );
  assert_eq!(
    make(
      &state,
      &publisher,
      spec(&recipient, 20000),
      Some(id.clone())
    )
    .await
    .status(),
    StatusCode::CREATED
  );
  let response = sessions(
    State(state.clone()),
    viewer.clone(),
    Path(id.clone()),
    Query(Page {
      history: Some(true),
      ..Default::default()
    }),
  )
  .await;
  assert_eq!(response.status(), StatusCode::OK);
  let body = json_body(response).await;
  assert_eq!(body["total"], 0);
  assert_eq!(body["items"], serde_json::json!([]));

  history.id = uuid::Uuid::new_v4().to_string();
  history.org_id = recipient;
  state.exposes.lock().await.store.record_session(&history);
  let body = json_body(
    sessions(
      State(state.clone()),
      viewer,
      Path(id),
      Query(Page {
        history: Some(true),
        limit: Some(1),
        ..Default::default()
      }),
    )
    .await,
  )
  .await;
  assert_eq!(body["total"], 1);
  assert_eq!(body["items"][0]["id"], history.id);
  assert_eq!(state.exposes.lock().await.store.session_history().len(), 2);
}

#[tokio::test]
async fn a_viewer_reads_an_admin_creates_and_another_tenant_sees_nothing() {
  let state = Arc::new(test_state());
  let org = tenant(&state, "acme").await;
  let other = tenant(&state, "beta").await;
  let (_, publisher) = user(&state, "publisher", &org, Role::Admin).await;
  let (_, operator) = user(&state, "operator", &org, Role::Operator).await;
  let (_, reader) = user(&state, "reader", &org, Role::Viewer).await;
  let (_, foreign) = user(&state, "foreign", &other, Role::Admin).await;
  let response = make(&state, &publisher, spec(&org, 20000), None).await;
  assert_eq!(response.status(), StatusCode::CREATED);
  let body = json_body(response).await;
  let id = body["id"].as_str().unwrap().to_string();
  let page = || {
    Query(Page {
      offset: Some(0),
      limit: Some(1),
      ..Default::default()
    })
  };
  assert_eq!(
    detail(State(state.clone()), foreign.clone(), Path(id.clone()))
      .await
      .status(),
    StatusCode::NOT_FOUND
  );
  let response = list(State(state.clone()), foreign.clone(), page()).await;
  assert_eq!(json_body(response).await["total"], 0);
  // Every role in the organization reads its listeners.
  assert_eq!(
    detail(State(state.clone()), reader.clone(), Path(id.clone()))
      .await
      .status(),
    StatusCode::OK
  );
  let listed = json_body(list(State(state.clone()), reader.clone(), page()).await).await;
  assert_eq!(listed["total"], 1);
  assert_eq!(listed["items"][0]["actions"], serde_json::json!(["read"]));
  // Creating one is administering the organization.
  for headers in [&reader, &operator] {
    assert_eq!(
      make(&state, headers, spec(&org, 20001), None)
        .await
        .status(),
      StatusCode::FORBIDDEN
    );
  }
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
  let (user_id, headers) = user(&state, "publisher", &org, Role::Admin).await;
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
  // Demoted to Viewer: still reads, no longer changes, at the next request.
  state
    .users
    .lock()
    .await
    .set_grants(
      &user_id,
      vec![Grant::new(GrantOrg::Child(org.clone()), Role::Viewer)],
    )
    .unwrap();
  assert_eq!(
    detail(State(state.clone()), headers.clone(), Path(id.clone()))
      .await
      .status(),
    StatusCode::OK
  );
  let refused = update(
    State(state.clone()),
    ConnectInfo(test_peer()),
    headers.clone(),
    Path(id.clone()),
    ExposeJson(Update {
      revision: 2,
      spec: spec(&org, 20003),
    }),
  )
  .await;
  assert_eq!(refused.status(), StatusCode::FORBIDDEN);
  // Granted elsewhere only: the organization's listeners are not theirs.
  state
    .users
    .lock()
    .await
    .set_grants(&user_id, vec![Grant::new(GrantOrg::Master, Role::Viewer)])
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
async fn the_organizations_allocation_bounds_a_user_and_a_key_alike() {
  let state = Arc::new(test_state());
  let org = tenant(&state, "acme").await;
  allocate(&state, vec![policy(&org, 20000, 20000)]).await;
  let (_, session) = user(&state, "bounded", &org, Role::Admin).await;
  let (_, secret) = state
    .admin_key_store
    .lock()
    .await
    .create(
      "bounded-key".into(),
      Role::Admin,
      GrantOrg::Child(org.clone()),
      None,
    )
    .unwrap();
  let mut key = HeaderMap::new();
  key.insert("authorization", format!("Bearer {secret}").parse().unwrap());
  for headers in [session, key] {
    let denied = make(&state, &headers, spec(&org, 20001), None).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(denied).await["code"], "outside_allocation");
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

#[tokio::test]
async fn masters_admin_is_bound_by_masters_allocation_and_the_server_admin_is_not() {
  let state = Arc::new(test_state());
  let (_, master_admin) = user(&state, "mara", "master", Role::Admin).await;
  let denied = make(&state, &master_admin, spec("master", 20000), None).await;
  assert_eq!(denied.status(), StatusCode::FORBIDDEN);
  assert_eq!(json_body(denied).await["code"], "outside_allocation");
  let server_admin = crate::test_support::admin_headers(&state).await;
  assert_eq!(
    make(&state, &server_admin, spec("master", 20000), None)
      .await
      .status(),
    StatusCode::CREATED
  );
}
