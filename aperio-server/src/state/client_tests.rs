//! What a connection carries and what its token may do: the routing and
//! health questions each service answers for itself, the seam that decides
//! which fields belong to a service rather than to the connection, and
//! `match_declarations`, which says what a heartbeat's entry is about.

use super::*;

// ----- ClientHandle routing / health helpers -----

#[test]
fn test_client_effective_binds_precedence() {
  use crate::test_support::mock_client;
  // declared path only.
  let c = mock_client(Some("a.local"), Some("/api"), None, None);
  assert_eq!(c.sole().effective_path_bind(), Some(&"/api".to_string()));
  assert!(c.sole().matches_host("a.local"));
  assert!(c.sole().has_hostname_bind());

  // override path wins over declared.
  let c = mock_client(Some("a.local"), Some("/api"), None, Some("/ovr"));
  assert_eq!(c.sole().effective_path_bind(), Some(&"/ovr".to_string()));

  // assigned path used when nothing declared/overridden.
  let mut c = mock_client(None, None, None, None);
  c.sole_mut().assigned_path = Some("/assigned".to_string());
  assert_eq!(
    c.sole().effective_path_bind(),
    Some(&"/assigned".to_string())
  );

  // hostname override replaces the whole set.
  let c = mock_client(Some("a.local"), None, Some("override.local"), None);
  assert_eq!(c.effective_hostnames(), vec![&"override.local".to_string()]);
  assert!(c.sole().matches_host("override.local"));
  assert!(!c.sole().matches_host("a.local"));

  // union of assigned + declared + extra declared hostnames, de-duplicated.
  let mut c = mock_client(Some("declared.local"), None, None, None);
  c.sole_mut().assigned_hostnames =
    vec!["assigned.local".to_string(), "declared.local".to_string()];
  c.sole_mut().declared_hostnames = vec!["extra.local".to_string(), "assigned.local".to_string()];
  let hosts = c.effective_hostnames();
  assert!(hosts.contains(&&"assigned.local".to_string()));
  assert!(hosts.contains(&&"declared.local".to_string()));
  assert!(hosts.contains(&&"extra.local".to_string()));
  assert_eq!(hosts.len(), 3, "duplicates collapse");

  // no binds at all.
  let c = mock_client(None, None, None, None);
  assert!(!c.sole().has_hostname_bind());
  assert!(c.sole().effective_path_bind().is_none());
}

#[test]
fn test_client_health_and_ejection() {
  use crate::test_support::mock_client;
  let now = Instant::now();
  let mut c = mock_client(None, None, None, None);

  // Fresh connection is healthy within the threshold.
  assert!(c.is_healthy(Duration::from_secs(3600)));
  // A zero threshold makes even a just-connected client stale.
  assert!(!c.is_healthy(Duration::from_nanos(0)));

  // Not ejected initially.
  assert!(!c.sole().is_ejected(now));
  // Below the failure threshold: no ejection.
  let window = Duration::from_secs(30);
  let eject_for = Duration::from_secs(30);
  assert!(!c.sole_mut().record_failure(now, window, 3, eject_for));
  assert!(!c.sole_mut().record_failure(now, window, 3, eject_for));
  // The third failure inside the window trips the ejection.
  assert!(c.sole_mut().record_failure(now, window, 3, eject_for));
  assert!(c.sole().is_ejected(now));
  // Failures are cleared once ejected; a repeat call while ejected is a no-op.
  assert!(!c.sole_mut().record_failure(now, window, 3, eject_for));

  // Stale failures outside the window are pruned before counting.
  let mut c2 = mock_client(None, None, None, None);
  let old = now - Duration::from_secs(120);
  c2.sole_mut().recent_failures.push_back(old);
  c2.sole_mut().recent_failures.push_back(old);
  assert!(!c2.sole_mut().record_failure(now, window, 3, eject_for));
  assert_eq!(c2.sole().recent_failures.len(), 1, "old failures pruned");
}

/// Every field of the wire's `ServiceDecl` is accounted for on the handle.
///
/// The table above `ClientHandle` is the only written record of which of its
/// fields are service-scoped and therefore become many when #46 splits
/// identity into `(connection, service)`. A record like that is worth exactly
/// as much as the thing that stops it going stale: a field added to the wire
/// without a line here would be a service setting nobody classified, and the
/// split would silently leave it on the connection, which is the same as
/// giving every service the last one's value.
#[test]
fn the_wire_says_what_a_service_is_and_the_handle_accounts_for_all_of_it() {
  let declared = struct_fields(include_str!("../protocol.rs"), "ServiceDecl");
  assert!(
    !declared.is_empty(),
    "the protocol still declares ServiceDecl"
  );

  let mapped: Vec<&str> = SERVICE_DECL_IN_SERVICE_STATE
    .iter()
    .map(|(w, _)| *w)
    .collect();

  let unclassified: Vec<&String> = declared
    .iter()
    .filter(|f| !mapped.contains(&f.as_str()))
    .collect();
  assert!(
    unclassified.is_empty(),
    "ServiceDecl gained {unclassified:?} and SERVICE_DECL_IN_SERVICE_STATE does not say where \
     it lands. Add a line, with None if it does not reach the handle at all."
  );

  let invented: Vec<&&str> = mapped
    .iter()
    .filter(|w| !declared.contains(&w.to_string()))
    .collect();
  assert!(
    invented.is_empty(),
    "SERVICE_DECL_IN_SERVICE_STATE names {invented:?}, which the wire no longer has."
  );

  let mut seen = std::collections::HashSet::new();
  for w in &mapped {
    assert!(seen.insert(*w), "{w} is listed twice");
  }
}

/// And every field the table points at actually exists, in `ServiceState`.
///
/// The other direction of the same drift: a rename would leave the table
/// pointing at nothing, and it would still read as authority.
#[test]
fn every_field_the_table_points_at_is_a_field_the_service_has() {
  let service = struct_fields(include_str!("client.rs"), "ServiceState");
  assert!(!service.is_empty(), "ServiceState is still a struct");

  let dangling: Vec<&str> = SERVICE_DECL_IN_SERVICE_STATE
    .iter()
    .filter_map(|(_, h)| *h)
    .filter(|h| !service.contains(&h.to_string()))
    .collect();
  assert!(
    dangling.is_empty(),
    "SERVICE_DECL_IN_SERVICE_STATE points at {dangling:?}, which ServiceState does not have. \
     A rename has to be made in both places."
  );
}

/// The two structs divide the fields the way the three lists say they should.
///
/// The compiler already stops a service field from being read off a
/// connection, which is the half a type can do. It cannot say the division is
/// the right one: a field put in the wrong struct compiles, and the mistake
/// only shows later as one value shared by services that should each have had
/// their own, or as a warn-once flag that silences the second service because
/// the first already warned. Neither is a compile error and neither fails any
/// other test, so this is the only thing standing between the seam and a
/// quiet drift back across it.
#[test]
fn the_two_structs_divide_the_fields_the_way_the_seam_says() {
  let src = include_str!("client.rs");
  let handle = struct_fields(src, "ClientHandle");
  let service = struct_fields(src, "ServiceState");
  assert!(!handle.is_empty() && !service.is_empty());

  let mut want_service: Vec<&str> = SERVICE_DECL_IN_SERVICE_STATE
    .iter()
    .filter_map(|(_, h)| *h)
    .collect();
  want_service.extend(SERVICE_SCOPED_DERIVED.iter().copied());

  let mut seen = std::collections::HashSet::new();
  for f in &want_service {
    assert!(seen.insert(*f), "{f} is claimed twice by the service side");
  }

  let mut stray: Vec<&String> = service
    .iter()
    .filter(|f| !want_service.contains(&f.as_str()))
    .collect();
  assert!(
    stray.is_empty(),
    "ServiceState carries {stray:?}, which the seam does not call service-scoped. \
     Either it belongs on ClientHandle, or a list has to say why it is here."
  );

  let missing: Vec<&&str> = want_service
    .iter()
    .filter(|f| !service.contains(&f.to_string()))
    .collect();
  assert!(
    missing.is_empty(),
    "the seam calls {missing:?} service-scoped, but they are not in ServiceState."
  );

  // The connection side, and the one field that joins the two.
  let mut want_handle: Vec<&str> = CONNECTION_SCOPED.to_vec();
  want_handle.push("services");
  stray = handle
    .iter()
    .filter(|f| !want_handle.contains(&f.as_str()))
    .collect();
  assert!(
    stray.is_empty(),
    "ClientHandle gained {stray:?} and nothing says whether it belongs to the connection \
     or to the service. Put it in CONNECTION_SCOPED, or in ServiceState."
  );
  let missing: Vec<&&str> = want_handle
    .iter()
    .filter(|f| !handle.contains(&f.to_string()))
    .collect();
  assert!(
    missing.is_empty(),
    "the seam calls {missing:?} connection-scoped, but ClientHandle does not have them."
  );
}

/// Field names of a struct, read from source. Reading them avoids the one
/// alternative, a second hand-written list, which is the thing being guarded
/// against in the first place.
#[cfg(test)]
fn struct_fields(source: &str, name: &str) -> Vec<String> {
  let Some(start) = source.find(&format!("struct {name} {{")) else {
    return Vec::new();
  };
  let mut out = Vec::new();
  for line in source[start..].lines().skip(1) {
    let line = line.trim();
    if line == "}" {
      break;
    }
    let Some(rest) = line
      .strip_prefix("pub(crate) ")
      .or_else(|| line.strip_prefix("pub "))
    else {
      continue;
    };
    if let Some((field, _)) = rest.split_once(':')
      && field.chars().all(|c| c.is_ascii_lowercase() || c == '_')
      && !field.is_empty()
    {
      out.push(field.to_string());
    }
  }
  out
}

/// `sole` and `sole_mut` address the same service.
///
/// They are two methods over a list, and nothing but this says they agree.
/// If one ever reached for the first entry and the other for the last, every
/// call site would still compile and every test would still pass while the
/// length is one, which it is everywhere today. The bug would appear on the
/// day a second service arrives, in the form of writes landing somewhere the
/// reads do not look, and it would appear in four hundred places at once.
///
/// So the list is given a second entry here, which is the only place in the
/// tree that does it, precisely because that is the condition under which
/// the two could disagree.
#[test]
fn the_one_service_written_to_is_the_one_read_back() {
  let mut handle = crate::test_support::mock_client(Some("first.example"), None, None, None);
  let second = crate::test_support::mock_client(Some("second.example"), None, None, None);
  handle.services.extend(second.services);
  assert_eq!(handle.services.len(), 2, "the case worth testing");

  handle.sole_mut().response_timeout = Some(77);
  assert_eq!(
    handle.sole().response_timeout,
    Some(77),
    "a write through sole_mut is visible through sole"
  );
  assert_eq!(
    handle.services[1].response_timeout, None,
    "and it went to one service, not to every service"
  );
}

/// A handle carries at least one service, which is what lets `sole` return a
/// reference instead of an `Option`.
///
/// Pinned at the constructor the tests themselves use, because an invariant
/// that only holds in production is an invariant the tests will break first.
#[test]
fn a_handle_is_never_built_without_a_service() {
  let handle = crate::test_support::mock_client(None, None, None, None);
  assert!(!handle.services.is_empty());
}

/// A routing predicate answers for the service it is called on.
///
/// This is the whole point of moving them off `ClientHandle`. There they read
/// `sole()`, so on a connection carrying two services both would have
/// answered for the first, and routing would have sent every request for the
/// second service to the first one's backend. The methods look identical
/// either way and nothing else in the tree can tell the difference yet,
/// because nothing else builds a two-service handle.
#[test]
fn each_service_answers_the_routing_questions_for_itself() {
  let mut handle = crate::test_support::mock_client(Some("first.example"), Some("/a"), None, None);
  let second = crate::test_support::mock_client(Some("second.example"), Some("/b"), None, None);
  handle.services.extend(second.services);

  assert!(handle.services[0].matches_host("first.example"));
  assert!(!handle.services[0].matches_host("second.example"));
  assert!(handle.services[1].matches_host("second.example"));
  assert!(!handle.services[1].matches_host("first.example"));

  assert_eq!(
    handle.services[0].effective_path_bind().map(String::as_str),
    Some("/a")
  );
  assert_eq!(
    handle.services[1].effective_path_bind().map(String::as_str),
    Some("/b")
  );

  // And the connection's own view is still the first, which is what every
  // caller that has not been taught to pick a service still gets.
  assert!(handle.sole().matches_host("first.example"));
}

// ----- match_declarations: which service a Ping entry updates ---------------

/// Builds a connection's service list from names, `None` for a nameless one.
fn services_named(names: &[Option<&str>]) -> Vec<ServiceState> {
  names
    .iter()
    .map(|n| {
      let mut s = crate::test_support::mock_client(None, None, None, None)
        .services
        .remove(0);
      s.service_name = n.map(str::to_string);
      s
    })
    .collect()
}

fn names(v: &[Option<&str>]) -> Vec<Option<String>> {
  v.iter().map(|n| n.map(str::to_string)).collect()
}

#[test]
fn a_named_declaration_finds_its_own_service_however_the_list_is_ordered() {
  // The case position-matching gets wrong, and the reason this function
  // exists: the client reordered its `services:` block. Nothing about the
  // services changed, so nothing may move between them.
  let existing = services_named(&[Some("api"), Some("web")]);
  let got = match_declarations(&existing, &names(&[Some("web"), Some("api")])).unwrap();
  assert_eq!(got, vec![Some(1), Some(0)]);
}

#[test]
fn a_service_this_connection_does_not_carry_yet_is_reported_as_new() {
  let existing = services_named(&[Some("api")]);
  let got = match_declarations(&existing, &names(&[Some("api"), Some("jobs")])).unwrap();
  assert_eq!(got, vec![Some(0), None]);
}

#[test]
fn nameless_declarations_match_nameless_services_in_order() {
  // A client that names nothing is every client before #46, so this path has
  // to keep behaving exactly like the single-service one it replaces.
  let existing = services_named(&[None, None]);
  let got = match_declarations(&existing, &names(&[None, None])).unwrap();
  assert_eq!(got, vec![Some(0), Some(1)]);
}

#[test]
fn a_nameless_declaration_never_claims_a_named_service() {
  // Otherwise adding a name to one entry of a two-service config would hand
  // the other entry that service's history.
  let existing = services_named(&[Some("api"), None]);
  let got = match_declarations(&existing, &names(&[None])).unwrap();
  assert_eq!(got, vec![Some(1)]);
}

#[test]
fn a_named_declaration_adopts_a_service_that_has_no_name_yet() {
  // Not the mirror of the rule above, and the first draft of this had it
  // backwards. A connection is created carrying one nameless placeholder,
  // and it is the first Ping that names it. Refusing the adoption would mean
  // every client that names its service gets a second one appended beside
  // the empty one it meant to fill, on its very first heartbeat.
  //
  // It is also the kinder answer for a client that adds a `name:` to a
  // service it had been running without one: same service, new label, and no
  // reason to lose its counters over it. The named-first pass means this can
  // only fire when no service of that name exists, so it never steals one.
  let existing = services_named(&[None]);
  let got = match_declarations(&existing, &names(&[Some("api")])).unwrap();
  assert_eq!(got, vec![Some(0)]);
}

#[test]
fn no_two_declarations_land_on_the_same_service() {
  // Two nameless entries against one nameless service: the second is new,
  // not a second writer of the first one's state.
  let existing = services_named(&[None]);
  let got = match_declarations(&existing, &names(&[None, None])).unwrap();
  assert_eq!(got, vec![Some(0), None]);
}

#[test]
fn a_repeated_name_is_refused_rather_than_resolved() {
  // Either answer is wrong. Taking the first silently drops the second
  // service; taking the last silently drops the first. Both leave a client
  // serving less than its config says with nothing to read about it.
  let existing = services_named(&[Some("api")]);
  let err = match_declarations(&existing, &names(&[Some("api"), Some("api")])).unwrap_err();
  assert_eq!(err, "api");
}

#[test]
fn a_service_that_stopped_being_declared_is_simply_unclaimed() {
  // Nothing here removes it; the caller does. What this has to get right is
  // that its absence does not shift the others onto each other.
  let existing = services_named(&[Some("api"), Some("web"), Some("jobs")]);
  let got = match_declarations(&existing, &names(&[Some("jobs"), Some("api")])).unwrap();
  assert_eq!(got, vec![Some(2), Some(0)]);
}

/// A connection's hostnames are every service's, not the first one's.
///
/// The organization fence asks this question to decide whether one org may
/// mint a share link for, or act on, a hostname another org is currently
/// serving. Answering from the first service only leaves a hostname served by
/// the second invisible to the fence, which is a tenant boundary with a hole
/// in it rather than a display bug.
#[test]
fn a_connection_reports_the_hostnames_of_all_its_services() {
  let mut handle = crate::test_support::mock_client(Some("first.example"), None, None, None);
  let second = crate::test_support::mock_client(Some("second.example"), None, None, None);
  handle.services.extend(second.services);

  let hosts: Vec<&str> = handle
    .effective_hostnames()
    .into_iter()
    .map(String::as_str)
    .collect();
  assert!(hosts.contains(&"first.example"));
  assert!(
    hosts.contains(&"second.example"),
    "the second service's hostname is served, so the fence has to see it"
  );
}

// ---------------------------------------------------------------------------
// A connection carrying several services is asked about all of them (#122)
// ---------------------------------------------------------------------------

/// A two-service handle: the first serves `first`, the second `second`.
fn multiplexed_handle(first: &str, second: &str) -> ClientHandle {
  let mut handle = crate::test_support::mock_client(Some(first), None, None, None);
  handle.services[0].declared_hostnames = vec![first.to_string()];
  // Built the way `on_ping` builds one: a fresh service sharing the
  // connection's pacer cell, then given its own binds.
  let pacer = handle.services[0].bandwidth_bps.clone();
  let mut extra = crate::state::ServiceState::newly_declared(pacer);
  extra.declared_hostname = Some(second.to_string());
  extra.declared_hostnames = vec![second.to_string()];
  handle.services.push(extra);
  handle
}

#[tokio::test]
async fn the_org_fence_sees_a_hostname_held_by_a_later_service() {
  // The fence is a tenant boundary: narrowing an organization's allowlist has
  // to drop any connection still serving a name that left it. It asked the
  // first service only, so a multiplexed connection whose *second* service
  // held the revoked hostname passed the check and went on serving it, which
  // is the same hole `effective_hostnames` was fixed for and reachable the
  // moment a client could carry two services.
  let state = crate::test_support::test_state();
  let mut handle = multiplexed_handle("kept.example.com", "revoked.example.com");
  handle.perms.org_id = Some("acme".to_string());
  state.clients.write().await.insert("c1".to_string(), handle);

  let dropped = state
    .apply_org_hostnames("acme", &["kept.example.com".to_string()])
    .await;
  assert_eq!(
    dropped, 1,
    "the connection serves a name outside the allowlist and has to go"
  );
}

#[tokio::test]
async fn a_connection_serving_only_permitted_names_is_left_alone() {
  // The other half of the same check: iterating every service must not turn
  // the fence into something that drops connections it has no quarrel with.
  let state = crate::test_support::test_state();
  let mut handle = multiplexed_handle("a.example.com", "b.example.com");
  handle.perms.org_id = Some("acme".to_string());
  state.clients.write().await.insert("c1".to_string(), handle);

  let dropped = state
    .apply_org_hostnames(
      "acme",
      &["a.example.com".to_string(), "b.example.com".to_string()],
    )
    .await;
  assert_eq!(dropped, 0);
}

#[test]
fn process_scoped_answers_are_about_the_process_not_its_first_service() {
  let threshold = std::time::Duration::from_secs(30);
  let mut handle = multiplexed_handle("a.example.com", "b.example.com");
  assert!(handle.serves_process_scoped(threshold));

  // A raw `tunnels:` open and an `expose` lookup are about the client process.
  // Reading the first service's kill switch meant disabling `a` from the
  // dashboard silently took away a tunnel the process declared and served
  // just as well through `b`.
  handle.services[0].admin_enabled = false;
  assert!(
    handle.serves_process_scoped(threshold),
    "one disabled service does not take the process's tunnels away"
  );
  handle.services[1].admin_enabled = false;
  assert!(
    !handle.serves_process_scoped(threshold),
    "with nothing enabled there is no process left to serve them"
  );
}

#[test]
fn a_process_is_named_by_every_service_it_carries() {
  let mut handle = multiplexed_handle("a.example.com", "b.example.com");
  handle.services[0].service_name = Some("web".to_string());
  handle.services[1].service_name = Some("api".to_string());
  assert_eq!(handle.process_name().as_deref(), Some("web, api"));

  // One service reads exactly as it did, which is every deployment before
  // multiplexing.
  handle.services.truncate(1);
  assert_eq!(handle.process_name().as_deref(), Some("web"));
}
