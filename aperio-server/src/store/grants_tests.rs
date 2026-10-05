//! The grant rules: a specific entry beats `*`, a grant is bounded by the
//! granter's, and a record written before grants existed keeps exactly the
//! reach it had.

use super::*;

fn exposed(org: GrantOrg, role: Role, actions: &[ExposeAction]) -> Grant {
  let mut grant = Grant::new(org, role);
  grant.expose.extend(actions.iter().copied());
  grant
}

#[test]
fn expose_is_explicit_for_tenants_and_master_keeps_server_authority() {
  for role in [Role::Viewer, Role::Operator, Role::Admin] {
    assert!(!may_expose(
      &[child("acme", role)],
      Some("acme"),
      ExposeAction::Create
    ));
  }
  for action in ExposeAction::ALL {
    assert!(may_expose(&all_admin(), Some("acme"), action));
  }
  let grant = exposed(
    GrantOrg::Child("acme".into()),
    Role::Viewer,
    &[ExposeAction::Create],
  );
  assert!(may_expose(
    std::slice::from_ref(&grant),
    Some("acme"),
    ExposeAction::Read
  ));
  assert!(may_expose(
    std::slice::from_ref(&grant),
    Some("acme"),
    ExposeAction::Create
  ));
  assert!(!may_expose(
    std::slice::from_ref(&grant),
    Some("acme"),
    ExposeAction::Delete
  ));
  assert!(!may_expose(&[grant], Some("beta"), ExposeAction::Create));
}

#[test]
fn explicit_empty_expose_capabilities_override_the_wildcard() {
  let grants = [
    exposed(GrantOrg::All, Role::Operator, &[ExposeAction::Create]),
    child("acme", Role::Admin),
  ];
  assert!(!may_expose(&grants, Some("acme"), ExposeAction::Read));
  assert!(may_expose(&grants, Some("beta"), ExposeAction::Create));
}

#[test]
fn expose_delegation_cannot_widen_or_create_itself() {
  let org = GrantOrg::Child("acme".into());
  let wanted = exposed(org.clone(), Role::Viewer, &[ExposeAction::Create]);
  let mut granter = exposed(org.clone(), Role::Admin, &[ExposeAction::Create]);
  assert!(!may_grant(std::slice::from_ref(&granter), &wanted));
  granter.expose.insert(ExposeAction::Delegate);
  assert!(may_grant(std::slice::from_ref(&granter), &wanted));
  assert!(!may_grant(
    std::slice::from_ref(&granter),
    &exposed(org.clone(), Role::Admin, &[ExposeAction::Delete])
  ));
  assert!(!may_grant(
    &[child("acme", Role::Admin)],
    &exposed(org, Role::Admin, &[ExposeAction::Delegate])
  ));
  assert!(!may_grant(
    &[granter],
    &exposed(GrantOrg::All, Role::Viewer, &[ExposeAction::Create])
  ));
}

#[test]
fn expose_capabilities_round_trip_and_old_rows_default_to_none() {
  let old: Grant = serde_json::from_str(r#"{"org":"acme","role":"admin"}"#).unwrap();
  assert!(old.expose.is_empty());
  let grant = exposed(
    GrantOrg::Child("acme".into()),
    Role::Operator,
    &[
      ExposeAction::Read,
      ExposeAction::Create,
      ExposeAction::Delegate,
    ],
  );
  assert_eq!(Grant::parse(&grant.label()).unwrap(), grant);
  assert_eq!(
    parse_list(&format!("{},beta:viewer", grant.label()))
      .unwrap()
      .len(),
    2
  );
  assert_eq!(
    serde_json::from_value::<Grant>(serde_json::to_value(&grant).unwrap()).unwrap(),
    grant
  );
  assert!(Grant::parse("acme:admin+expose.everything").is_err());
  assert!(
    serde_json::from_str::<Grant>(r#"{"org":"acme","role":"admin","expose":["everything"]}"#)
      .is_err()
  );
}

#[test]
fn oidc_capability_changes_are_recomputed_not_accumulated() {
  let mut first = exposed(
    GrantOrg::Child("acme".into()),
    Role::Operator,
    &[ExposeAction::Create],
  );
  first.source = Some("ops".into());
  let mut second = exposed(
    GrantOrg::Child("acme".into()),
    Role::Admin,
    &[ExposeAction::Delete],
  );
  second.source = Some("admins".into());
  let (both, _, _) = apply_group_map(&[], vec![first.clone(), second.clone()]);
  assert!(may_expose(&both, Some("acme"), ExposeAction::Delete));
  assert!(may_expose(&both, Some("acme"), ExposeAction::Create));
  let (same, added, removed) = apply_group_map(&both, vec![first.clone(), second]);
  assert_eq!(same, both);
  assert!(added.is_empty() && removed.is_empty());
  let (next, added, removed) = apply_group_map(&both, vec![first.clone()]);
  assert_eq!(next, vec![first]);
  assert!(!may_expose(&next, Some("acme"), ExposeAction::Delete));
  assert_eq!(added.len(), 1);
  assert_eq!(removed.len(), 1);
}

fn child(id: &str, role: Role) -> Grant {
  Grant::new(GrantOrg::Child(id.to_string()), role)
}

#[test]
fn the_api_spelling_round_trips() {
  assert_eq!(GrantOrg::parse("master"), GrantOrg::Master);
  assert_eq!(GrantOrg::parse(""), GrantOrg::Master);
  assert_eq!(GrantOrg::parse(" MASTER "), GrantOrg::Master);
  assert_eq!(GrantOrg::parse("*"), GrantOrg::All);
  assert_eq!(GrantOrg::parse("org-1"), GrantOrg::Child("org-1".into()));
  for org in [GrantOrg::Master, GrantOrg::All, GrantOrg::Child("x".into())] {
    assert_eq!(GrantOrg::parse(org.as_str()), org);
    let json = serde_json::to_string(&org).unwrap();
    assert_eq!(serde_json::from_str::<GrantOrg>(&json).unwrap(), org);
  }
  assert_eq!(GrantOrg::from_org_id(None), GrantOrg::Master);
  assert_eq!(
    GrantOrg::from_org_id(Some("acme")),
    GrantOrg::Child("acme".into())
  );
  assert_eq!(
    Grant::new(GrantOrg::Child("acme".into()), Role::Operator).label(),
    "acme:operator"
  );
}

#[test]
fn covers_is_exact_except_for_all() {
  assert!(GrantOrg::All.covers(None));
  assert!(GrantOrg::All.covers(Some("acme")));
  assert!(GrantOrg::Master.covers(None));
  assert!(!GrantOrg::Master.covers(Some("acme")));
  assert!(GrantOrg::Child("acme".into()).covers(Some("acme")));
  assert!(!GrantOrg::Child("acme".into()).covers(Some("beta")));
  assert!(!GrantOrg::Child("acme".into()).covers(None));
}

#[test]
fn a_specific_entry_beats_star() {
  let grants = vec![
    Grant::new(GrantOrg::All, Role::Viewer),
    child("acme", Role::Operator),
  ];
  assert_eq!(role_in(&grants, Some("acme")), Some(Role::Operator));
  assert_eq!(role_in(&grants, Some("beta")), Some(Role::Viewer));
  assert_eq!(role_in(&grants, None), Some(Role::Viewer));
  // And the other way round: `*` Admin narrowed in one organization.
  let grants = vec![
    Grant::new(GrantOrg::All, Role::Admin),
    child("acme", Role::Viewer),
  ];
  assert_eq!(role_in(&grants, Some("acme")), Some(Role::Viewer));
  assert_eq!(role_in(&grants, None), Some(Role::Admin));
}

#[test]
fn no_grant_means_no_role() {
  let grants = vec![child("acme", Role::Admin)];
  assert_eq!(role_in(&grants, Some("beta")), None);
  assert_eq!(role_in(&grants, None), None);
  assert_eq!(role_in(&[], None), None);
}

#[test]
fn star_is_given_only_by_star_admin() {
  let star = Grant::new(GrantOrg::All, Role::Viewer);
  assert!(may_grant(&all_admin(), &star));
  assert!(!may_grant(
    &[Grant::new(GrantOrg::All, Role::Operator)],
    &star
  ));
  // Master Admin is the server-global admin and still cannot hand out `*`.
  assert!(!may_grant(
    &[Grant::new(GrantOrg::Master, Role::Admin)],
    &star
  ));
}

#[test]
fn a_grant_takes_admin_in_that_organization() {
  let acme_admin = vec![child("acme", Role::Admin)];
  assert!(may_grant(&acme_admin, &child("acme", Role::Viewer)));
  assert!(may_grant(&acme_admin, &child("acme", Role::Admin)));
  // Not in an organization where the granter is not Admin, and not one
  // they do not hold at all.
  assert!(!may_grant(
    &[child("acme", Role::Operator)],
    &child("acme", Role::Viewer)
  ));
  assert!(!may_grant(&acme_admin, &child("beta", Role::Viewer)));
  assert!(!may_grant(
    &acme_admin,
    &Grant::new(GrantOrg::Master, Role::Viewer)
  ));
  // `*` Admin reaches every organization, master included.
  assert!(may_grant(&all_admin(), &child("beta", Role::Admin)));
  assert!(may_grant(
    &all_admin(),
    &Grant::new(GrantOrg::Master, Role::Admin)
  ));
  // And a specific narrowing takes that reach away.
  let narrowed = vec![
    Grant::new(GrantOrg::All, Role::Admin),
    child("acme", Role::Viewer),
  ];
  assert!(!may_grant(&narrowed, &child("acme", Role::Viewer)));
}

#[test]
fn a_legacy_master_admin_keeps_everything_and_says_so() {
  let (grants, widened) = legacy_grants(Role::Admin, None);
  assert_eq!(grants, all_admin());
  assert!(widened);
  let (grants, widened) = legacy_grants(Role::Viewer, None);
  assert_eq!(grants, vec![Grant::new(GrantOrg::Master, Role::Viewer)]);
  assert!(!widened);
  let (grants, widened) = legacy_grants(Role::Admin, Some("acme"));
  assert_eq!(grants, vec![child("acme", Role::Admin)]);
  assert!(!widened);
}

#[test]
fn normalize_dedupes_orders_and_refuses_empty() {
  let out = normalize(vec![
    child("beta", Role::Viewer),
    child("acme", Role::Viewer),
    Grant::new(GrantOrg::Master, Role::Operator),
    child("acme", Role::Admin),
    Grant::new(GrantOrg::All, Role::Viewer),
  ])
  .unwrap();
  assert_eq!(
    out,
    vec![
      Grant::new(GrantOrg::All, Role::Viewer),
      Grant::new(GrantOrg::Master, Role::Operator),
      child("acme", Role::Admin),
      child("beta", Role::Viewer),
    ]
  );
  assert!(normalize(Vec::new()).is_err());
}

#[test]
fn diff_reports_a_role_change_as_remove_plus_add() {
  let before = vec![child("acme", Role::Viewer), child("beta", Role::Admin)];
  let after = vec![child("acme", Role::Operator), child("beta", Role::Admin)];
  let (added, removed) = diff(&before, &after);
  assert_eq!(added, vec![child("acme", Role::Operator)]);
  assert_eq!(removed, vec![child("acme", Role::Viewer)]);
  let (added, removed) = diff(&after, &after);
  assert!(added.is_empty() && removed.is_empty());
}

// ---------------------------------------------------------------------------
// the text spellings and the login map (planned_features.md #154)
// ---------------------------------------------------------------------------

#[test]
fn a_grant_parses_from_org_colon_role_and_nothing_else() {
  assert_eq!(
    Grant::parse("acme:operator").unwrap(),
    child("acme", Role::Operator)
  );
  assert_eq!(
    Grant::parse(" master:admin ").unwrap(),
    Grant::new(GrantOrg::Master, Role::Admin)
  );
  assert_eq!(
    Grant::parse("*:viewer").unwrap(),
    Grant::new(GrantOrg::All, Role::Viewer)
  );
  for bad in ["acme", "acme:", ":admin", "acme:root", ""] {
    assert!(Grant::parse(bad).is_err(), "{bad:?} parsed");
  }
  assert_eq!(
    parse_list(" master:viewer, acme:admin ,").unwrap(),
    vec![
      Grant::new(GrantOrg::Master, Role::Viewer),
      child("acme", Role::Admin)
    ]
  );
  assert!(parse_list("").unwrap().is_empty());
  assert!(parse_list("acme").is_err());
}

#[test]
fn a_group_map_parses_group_equals_grant() {
  let map = parse_group_map("aperio-admins=master:admin, acme-ops=acme:operator,auditors=*:viewer")
    .unwrap();
  assert_eq!(map.len(), 3);
  assert_eq!(map[0].0, "aperio-admins");
  assert_eq!(map[1].1, child("acme", Role::Operator));
  assert_eq!(map[2].1, Grant::new(GrantOrg::All, Role::Viewer));
  assert!(parse_group_map("").unwrap().is_empty());
  for bad in ["ops", "=acme:admin", "ops=acme", "ops=acme:root"] {
    assert!(parse_group_map(bad).is_err(), "{bad:?} parsed");
  }
}

#[test]
fn the_map_owns_what_it_produced_and_leaves_hand_written_grants_alone() {
  let by_hand = Grant::new(GrantOrg::Master, Role::Viewer);
  let ops = Grant::mapped(GrantOrg::Child("acme".into()), Role::Admin, "ops");
  let aud = Grant::mapped(GrantOrg::All, Role::Viewer, "aud");

  // First login: both groups.
  let (next, added, removed) = apply_group_map(
    std::slice::from_ref(&by_hand),
    vec![ops.clone(), aud.clone()],
  );
  assert_eq!(next, vec![by_hand.clone(), ops.clone(), aud.clone()]);
  assert_eq!(added, vec![ops.clone(), aud.clone()]);
  assert!(removed.is_empty());

  // Second login: out of the ops group. The mapped grant goes, the hand one
  // stays, and the unchanged mapped one is neither added nor removed.
  let (next, added, removed) = apply_group_map(&next, vec![aud.clone()]);
  assert_eq!(next, vec![by_hand.clone(), aud.clone()]);
  assert!(added.is_empty());
  assert_eq!(removed, vec![ops.clone()]);

  // The directory wins over a hand-written grant in the organization it
  // names, and says so as a removal plus an addition.
  let mapped_master = Grant::mapped(GrantOrg::Master, Role::Admin, "aperio-admins");
  let (next, added, removed) = apply_group_map(&next, vec![aud.clone(), mapped_master.clone()]);
  assert_eq!(next, vec![aud.clone(), mapped_master.clone()]);
  assert_eq!(added, vec![mapped_master.clone()]);
  assert_eq!(removed, vec![by_hand.clone()]);

  // Two groups naming one organization: the higher role wins.
  let low = Grant::mapped(GrantOrg::Child("acme".into()), Role::Viewer, "acme-all");
  let high = Grant::mapped(GrantOrg::Child("acme".into()), Role::Admin, "acme-admins");
  let (next, _, _) = apply_group_map(&[], vec![low.clone(), high.clone()]);
  assert_eq!(next, vec![high.clone()]);
  let (next, _, _) = apply_group_map(&[], vec![high.clone(), low.clone()]);
  assert_eq!(next, vec![high]);
}
