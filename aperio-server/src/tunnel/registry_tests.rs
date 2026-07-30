//! Unit tests for tunnel resolution: who may bind, and which path is chosen.

use super::*;
use crate::state::ClientPerms;

/// A dynamic token's permissions, with the two knobs the rules read.
fn perms(token_id: &str, org: Option<&str>, allow_bind: bool) -> ClientPerms {
  ClientPerms {
    master: false,
    hostnames: Vec::new(),
    paths: Vec::new(),
    token_name: Some(format!("tok-{token_id}")),
    token_id: Some(token_id.to_string()),
    allow_public: false,
    allow_bind,
    topics: Vec::new(),
    org_id: org.map(str::to_string),
    org_hostnames: Vec::new(),
    max_connections: None,
  }
}

#[test]
fn master_binds_anything() {
  let owner = perms("owner", Some("org-a"), false);
  assert!(may_bind(&ClientPerms::master(), &owner));
}

#[test]
fn the_same_token_binds_without_the_capability() {
  // The rule that predates organizations; kept so no existing binder breaks.
  let owner = perms("same", Some("org-a"), false);
  let consumer = perms("same", Some("org-a"), false);
  assert!(may_bind(&consumer, &owner));
}

#[test]
fn a_sibling_token_needs_the_capability() {
  let owner = perms("owner", Some("org-a"), false);
  let without = perms("other", Some("org-a"), false);
  let with = perms("other", Some("org-a"), true);
  assert!(
    !may_bind(&without, &owner),
    "allow_bind defaults off, so nothing is granted implicitly"
  );
  assert!(may_bind(&with, &owner));
}

#[test]
fn the_capability_does_not_cross_organizations() {
  let owner = perms("owner", Some("org-a"), false);
  let outsider = perms("other", Some("org-b"), true);
  assert!(!may_bind(&outsider, &owner));
}

#[test]
fn a_capable_token_cannot_reach_the_master_organization() {
  // `None` is the master org; a dynamic token carrying allow_bind must not
  // inherit it just because both sides spell the org as None.
  let owner = ClientPerms::master();
  let outsider = perms("other", Some("org-a"), true);
  assert!(!may_bind(&outsider, &owner));
}

#[test]
fn a_master_org_token_binds_master_org_tunnels() {
  let owner = perms("owner", None, false);
  let consumer = perms("other", None, true);
  assert!(may_bind(&consumer, &owner));
}

// ---------------------------------------------------------------------------
// Resolution: which connection carries a tunnel.
// ---------------------------------------------------------------------------

use crate::protocol::TunnelDecl;
use crate::test_support::test_state;

fn decl(name: &str, protocol: &str) -> TunnelDecl {
  TunnelDecl {
    custom_name: None,
    name: Some(name.to_string()),
    target: "127.0.0.1:5432".to_string(),
    protocol: protocol.to_string(),
    encrypt: false,
    idle_timeout: None,
    expose: None,
  }
}

async fn insert(
  state: &Arc<crate::state::AppState>,
  cid: &str,
  mutate: impl FnOnce(&mut crate::state::ClientHandle),
) {
  let mut c = crate::test_support::mock_client(None, None, None, None);
  mutate(&mut c);
  state.clients.lock().await.insert(cid.to_string(), c);
}

#[tokio::test]
async fn a_name_resolves_without_any_client_id() {
  let state = Arc::new(test_state());
  insert(&state, "conn-abc", |c| {
    c.tunnels = vec![decl("pg_main", "tcp")]
  })
  .await;
  let found = resolve(&state, &ClientPerms::master(), Selector::Name("pg_main"))
    .await
    .expect("resolved by name");
  assert_eq!(found.client_id, "conn-abc");
}

#[tokio::test]
async fn an_org_qualified_name_picks_that_organizations_tunnel() {
  // A tunnel name is unique inside an organization and nowhere else. A master
  // binder can see both, so without the qualifier which one it reaches comes
  // down to the order of a hash map.
  let state = Arc::new(test_state());
  let payments = state
    .org_store
    .lock()
    .await
    .create("payments", Vec::new(), None)
    .unwrap()
    .id;
  insert(&state, "conn-master", |c| {
    c.tunnels = vec![decl("pg_main", "tcp")];
  })
  .await;
  insert(&state, "conn-payments", |c| {
    c.tunnels = vec![decl("pg_main", "tcp")];
    c.perms.org_id = Some(payments);
  })
  .await;

  let found = resolve(
    &state,
    &ClientPerms::master(),
    Selector::Name("payments@pg_main"),
  )
  .await
  .expect("resolved inside the named organization");
  assert_eq!(found.client_id, "conn-payments");

  let found = resolve(
    &state,
    &ClientPerms::master(),
    Selector::Name("master@pg_main"),
  )
  .await
  .expect("`master` is the built-in organization");
  assert_eq!(found.client_id, "conn-master");

  // An organization that does not exist resolves to nothing rather than to
  // whichever client happens to carry the name.
  assert!(
    resolve(
      &state,
      &ClientPerms::master(),
      Selector::Name("paymnets@pg_main")
    )
    .await
    .is_err()
  );
}

#[tokio::test]
async fn a_draining_path_is_skipped_for_a_healthy_sibling() {
  // Both connections belong to one process and announce the same tunnel.
  // Answering "unavailable" because the first one happens to be draining is
  // exactly the failure this walk exists to avoid.
  let state = Arc::new(test_state());
  insert(&state, "conn-drain", |c| {
    c.tunnels = vec![decl("pg_main", "tcp")];
    c.instance_group = Some("proc-1".to_string());
    c.draining = true;
  })
  .await;
  insert(&state, "conn-ok", |c| {
    c.tunnels = vec![decl("pg_main", "tcp")];
    c.instance_group = Some("proc-1".to_string());
  })
  .await;

  let found = resolve(&state, &ClientPerms::master(), Selector::Name("pg_main"))
    .await
    .expect("the healthy sibling serves it");
  assert_eq!(found.client_id, "conn-ok");
}

#[tokio::test]
async fn the_raw_client_id_from_the_config_file_resolves() {
  // The suffixed per-service id is an internal artifact; what an operator
  // has is the `client_id:` they wrote, which arrives as the instance group.
  let state = Arc::new(test_state());
  insert(&state, "conn-xyz", |c| {
    c.tunnels = vec![decl("pg_main", "tcp")];
    c.instance_group = Some("3beebfdb-079f-4a00-9e03-1bb6eb9222b4".to_string());
    c.reported_instance_id = Some("3beebfdb-079f-4a00-9e03-1bb6eb9222b4-0".to_string());
  })
  .await;

  for id in [
    "conn-xyz",
    "3beebfdb-079f-4a00-9e03-1bb6eb9222b4",
    "3beebfdb-079f-4a00-9e03-1bb6eb9222b4-0",
  ] {
    let selector = Selector::ClientTarget {
      client: id,
      target: "127.0.0.1:5432",
      protocol: "tcp",
    };
    assert!(
      resolve(&state, &ClientPerms::master(), selector)
        .await
        .is_ok(),
      "{id} should address the same connection"
    );
  }
}

#[tokio::test]
async fn an_unknown_name_and_a_forbidden_one_are_told_apart() {
  let state = Arc::new(test_state());
  insert(&state, "conn-abc", |c| {
    c.tunnels = vec![decl("pg_main", "tcp")];
    c.perms = perms("owner", Some("org-a"), false);
  })
  .await;

  let outsider = perms("other", Some("org-b"), true);
  assert_eq!(
    resolve(&state, &outsider, Selector::Name("pg_main"))
      .await
      .unwrap_err(),
    Rejection::Forbidden
  );
  assert_eq!(
    resolve(&state, &outsider, Selector::Name("nope"))
      .await
      .unwrap_err(),
    Rejection::Unknown
  );
}

#[tokio::test]
async fn an_unavailable_tunnel_is_not_reported_as_missing() {
  let state = Arc::new(test_state());
  insert(&state, "conn-abc", |c| {
    c.tunnels = vec![decl("pg_main", "tcp")];
    c.draining = true;
  })
  .await;
  assert_eq!(
    resolve(&state, &ClientPerms::master(), Selector::Name("pg_main"))
      .await
      .unwrap_err(),
    Rejection::Unavailable
  );
}

#[tokio::test]
async fn the_listing_folds_a_process_into_one_entry_per_name() {
  let state = Arc::new(test_state());
  for cid in ["conn-0", "conn-1", "conn-2"] {
    insert(&state, cid, |c| {
      c.tunnels = vec![decl("pg_main", "tcp"), decl("dns", "udp")];
      c.instance_group = Some("proc-1".to_string());
    })
    .await;
  }
  let listed = visible(&state, &ClientPerms::master()).await;
  assert_eq!(listed.len(), 2, "two names, not six declarations");
  let pg = listed.iter().find(|v| v.name == "pg_main").unwrap();
  assert_eq!(pg.paths, 3, "three ways in");
  assert!(pg.available);
}

#[tokio::test]
async fn the_listing_shows_only_what_the_caller_may_bind() {
  let state = Arc::new(test_state());
  insert(&state, "mine", |c| {
    c.tunnels = vec![decl("mine", "tcp")];
    c.perms = perms("owner", Some("org-a"), false);
  })
  .await;
  insert(&state, "theirs", |c| {
    c.tunnels = vec![decl("theirs", "tcp")];
    c.perms = perms("stranger", Some("org-b"), false);
  })
  .await;

  let caller = perms("other", Some("org-a"), true);
  let listed = visible(&state, &caller).await;
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].name, "mine");
}

#[tokio::test]
async fn a_combined_tunnel_resolves_on_both_transports() {
  // One name, one declaration, addressable from the tcp and the udp endpoint.
  let state = Arc::new(test_state());
  insert(&state, "conn-abc", |c| {
    c.tunnels = vec![decl("dns", "tcp/udp")];
  })
  .await;

  let found = resolve(&state, &ClientPerms::master(), Selector::Name("dns"))
    .await
    .expect("resolved by name");
  assert_eq!(found.decl.protocol, "tcp/udp");

  // And through the older client/target addressing, for either transport.
  for protocol in ["tcp", "udp"] {
    let selector = Selector::ClientTarget {
      client: "conn-abc",
      target: "127.0.0.1:5432",
      protocol,
    };
    assert!(
      resolve(&state, &ClientPerms::master(), selector)
        .await
        .is_ok(),
      "a tcp/udp tunnel must answer the {protocol} endpoint"
    );
  }
}

#[tokio::test]
async fn a_single_transport_tunnel_still_refuses_the_other() {
  let state = Arc::new(test_state());
  insert(&state, "conn-abc", |c| {
    c.tunnels = vec![decl("pg_main", "tcp")];
  })
  .await;
  let selector = Selector::ClientTarget {
    client: "conn-abc",
    target: "127.0.0.1:5432",
    protocol: "udp",
  };
  assert_eq!(
    resolve(&state, &ClientPerms::master(), selector)
      .await
      .unwrap_err(),
    Rejection::Unknown
  );
}

#[tokio::test]
async fn the_listing_names_the_process_not_one_of_its_connections() {
  // With several paths, reporting a per-connection id would mean the value
  // depends on iteration order and could differ between two calls describing
  // the same tunnel. It is also the suffixed form, which carries a service
  // index the operator never wrote.
  let state = Arc::new(test_state());
  for (cid, index) in [("conn-a", "0"), ("conn-b", "1")] {
    insert(&state, cid, |c| {
      c.tunnels = vec![decl("dns", "udp")];
      c.instance_group = Some("dae0d524-3408-4a1a-bbda-304c7502d3ce".to_string());
      c.reported_instance_id = Some(format!("dae0d524-3408-4a1a-bbda-304c7502d3ce-{index}"));
    })
    .await;
  }

  let listed = visible(&state, &ClientPerms::master()).await;
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].paths, 2);
  assert_eq!(
    listed[0].client_id.as_deref(),
    Some("dae0d524-3408-4a1a-bbda-304c7502d3ce"),
    "the id shown is the one written in the config file"
  );
}

#[tokio::test]
async fn an_older_client_still_reports_an_id() {
  // A client that predates the `x-aperio-instance` header has no group, so
  // the per-connection id is better than nothing.
  let state = Arc::new(test_state());
  insert(&state, "conn-a", |c| {
    c.tunnels = vec![decl("dns", "udp")];
    c.instance_group = None;
    c.reported_instance_id = Some("legacy-id".to_string());
  })
  .await;

  let listed = visible(&state, &ClientPerms::master()).await;
  assert_eq!(listed[0].client_id.as_deref(), Some("legacy-id"));
}
