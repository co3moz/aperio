//! The grant rules: a specific entry beats `*`, `*` Admin is the server
//! admin and nothing else is, a grant is bounded by the granter's, what a
//! role may do with public listeners comes with it, and a record written
//! before grants existed keeps exactly the reach it had.

use super::*;

#[test]
fn what_a_role_may_do_with_public_listeners_comes_with_it() {
  let operator_may = [
    ExposeAction::Read,
    ExposeAction::Enable,
    ExposeAction::Disable,
    ExposeAction::Disconnect,
  ];
  for action in ExposeAction::ALL {
    let viewer = may_expose(&[child("acme", Role::Viewer)], Some("acme"), action);
    let operator = may_expose(&[child("acme", Role::Operator)], Some("acme"), action);
    let admin = may_expose(&[child("acme", Role::Admin)], Some("acme"), action);
    assert_eq!(viewer, action == ExposeAction::Read, "viewer {action:?}");
    assert_eq!(
      operator,
      operator_may.contains(&action),
      "operator {action:?}"
    );
    assert!(admin, "admin {action:?}");
    // A role in one organization says nothing about another.
    assert!(!may_expose(
      &[child("acme", Role::Admin)],
      Some("beta"),
      action
    ));
  }
}

#[test]
fn a_specific_entry_beats_the_wildcard_for_listeners_too() {
  let grants = [
    Grant::new(GrantOrg::All, Role::Admin),
    child("acme", Role::Viewer),
  ];
  assert!(may_expose(&grants, Some("acme"), ExposeAction::Read));
  assert!(!may_expose(&grants, Some("acme"), ExposeAction::Create));
  assert!(may_expose(&grants, Some("beta"), ExposeAction::Create));
}

#[test]
fn only_the_wildcard_admin_is_the_server_admin() {
  assert!(is_server_admin(&all_admin()));
  let master_admin = [Grant::new(GrantOrg::Master, Role::Admin)];
  assert!(!is_server_admin(&master_admin));
  assert!(!is_server_admin(&[Grant::new(
    GrantOrg::All,
    Role::Operator
  )]));
  // Master's admin administers master, like any organization's admin.
  assert!(may_grant(
    &master_admin,
    &Grant::new(GrantOrg::Master, Role::Viewer)
  ));
  assert!(!may_grant(&master_admin, &child("acme", Role::Viewer)));
  assert!(!may_grant(
    &master_admin,
    &Grant::new(GrantOrg::All, Role::Viewer)
  ));
  for grant in [
    Grant::new(GrantOrg::All, Role::Admin),
    Grant::new(GrantOrg::Master, Role::Admin),
    child("acme", Role::Operator),
  ] {
    assert!(may_grant(&all_admin(), &grant), "{}", grant.label());
  }
}

#[test]
fn a_row_from_before_0_13_still_reads_and_its_capabilities_are_gone() {
  let old: Grant = serde_json::from_str(
    r#"{"org":"acme","role":"viewer","expose":["read","create","delegate"],"expose_bounds":null}"#,
  )
  .unwrap();
  assert_eq!(old, child("acme", Role::Viewer));
  assert!(!serde_json::to_string(&old).unwrap().contains("expose"));
  let refused = Grant::parse("acme:viewer+expose.create").unwrap_err();
  assert!(refused.contains("part of the role"), "{refused}");
  assert!(refused.contains("write acme:viewer"), "{refused}");
  let grant = child("acme", Role::Operator);
  assert_eq!(Grant::parse(&grant.label()).unwrap(), grant);
  assert_eq!(parse_list("acme:operator,beta:viewer").unwrap().len(), 2);
}

#[test]
fn two_groups_naming_one_organization_give_the_higher_role() {
  let mut ops = child("acme", Role::Operator);
  ops.source = Some("ops".into());
  let mut admins = child("acme", Role::Admin);
  admins.source = Some("admins".into());
  let (both, _, _) = apply_group_map(&[], vec![ops.clone(), admins.clone()], None);
  assert_eq!(role_in(&both, Some("acme")), Some(Role::Admin));
  let (same, added, removed) = apply_group_map(&both, vec![ops.clone(), admins], None);
  assert_eq!(same, both);
  assert!(added.is_empty() && removed.is_empty());
  let (next, added, removed) = apply_group_map(&both, vec![ops.clone()], None);
  assert_eq!(next, vec![ops]);
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
    None,
  );
  assert_eq!(next, vec![by_hand.clone(), ops.clone(), aud.clone()]);
  assert_eq!(added, vec![ops.clone(), aud.clone()]);
  assert!(removed.is_empty());

  // Second login: out of the ops group. The mapped grant goes, the hand one
  // stays, and the unchanged mapped one is neither added nor removed.
  let (next, added, removed) = apply_group_map(&next, vec![aud.clone()], None);
  assert_eq!(next, vec![by_hand.clone(), aud.clone()]);
  assert!(added.is_empty());
  assert_eq!(removed, vec![ops.clone()]);

  // The directory wins over a hand-written grant in the organization it
  // names, and says so as a removal plus an addition.
  let mapped_master = Grant::mapped(GrantOrg::Master, Role::Admin, "aperio-admins");
  let (next, added, removed) =
    apply_group_map(&next, vec![aud.clone(), mapped_master.clone()], None);
  assert_eq!(next, vec![aud.clone(), mapped_master.clone()]);
  assert_eq!(added, vec![mapped_master.clone()]);
  assert_eq!(removed, vec![by_hand.clone()]);

  // Two groups naming one organization: the higher role wins.
  let low = Grant::mapped(GrantOrg::Child("acme".into()), Role::Viewer, "acme-all");
  let high = Grant::mapped(GrantOrg::Child("acme".into()), Role::Admin, "acme-admins");
  let (next, _, _) = apply_group_map(&[], vec![low.clone(), high.clone()], None);
  assert_eq!(next, vec![high.clone()]);
  let (next, _, _) = apply_group_map(&[], vec![high.clone(), low.clone()], None);
  assert_eq!(next, vec![high]);
}

#[test]
fn a_provider_owns_only_what_its_own_map_produced() {
  // The corporate directory maps `*` Admin. The same email then signs in
  // through Acme's own provider, whose map can only name Acme.
  let star = Grant::mapped(GrantOrg::All, Role::Admin, "ops");
  let (after_global, _, _) = apply_group_map(&[], vec![star.clone()], None);
  let acme_staff = Grant::mapped(GrantOrg::Child("acme".into()), Role::Operator, "staff");
  let (after_acme, added, removed) =
    apply_group_map(&after_global, vec![acme_staff.clone()], Some("acme"));
  assert!(removed.is_empty(), "Acme's login took back {removed:?}");
  assert_eq!(added.len(), 1);
  assert_eq!(added[0].source_org.as_deref(), Some("acme"));
  assert_eq!(role_in(&after_acme, None), Some(Role::Admin));
  assert_eq!(role_in(&after_acme, Some("acme")), Some(Role::Operator));

  // The global login again, same groups: Acme's grant is not its to take.
  let (after_global, added, removed) = apply_group_map(&after_acme, vec![star.clone()], None);
  assert!(added.is_empty() && removed.is_empty());
  assert_eq!(after_global.len(), after_acme.len());

  // Out of Acme's group: Acme's login takes back its own grant only.
  let (after_acme, added, removed) = apply_group_map(&after_global, Vec::new(), Some("acme"));
  assert!(added.is_empty());
  assert_eq!(removed.len(), 1);
  assert_eq!(removed[0].org, GrantOrg::Child("acme".into()));
  assert_eq!(role_in(&after_acme, None), Some(Role::Admin));

  // Both directories naming the same organization is a real conflict, and
  // the one signing in now wins it.
  let global_acme = Grant::mapped(GrantOrg::Child("acme".into()), Role::Admin, "acme-ops");
  let (both, _, _) = apply_group_map(&after_acme, vec![star, global_acme], None);
  let (next, _, _) = apply_group_map(&both, vec![acme_staff], Some("acme"));
  assert_eq!(role_in(&next, Some("acme")), Some(Role::Operator));
  assert_eq!(role_in(&next, None), Some(Role::Admin));
}

#[test]
fn a_grant_mapped_before_provenance_reads_as_the_global_providers() {
  let old: Grant =
    serde_json::from_str(r#"{"org":"acme","role":"viewer","source":"staff"}"#).unwrap();
  assert_eq!(old.source_org, None);
  // Not Acme's to take back, and the global map no longer produces it.
  let (_, _, removed) = apply_group_map(std::slice::from_ref(&old), Vec::new(), Some("acme"));
  assert!(removed.is_empty());
  let (_, _, removed) = apply_group_map(std::slice::from_ref(&old), Vec::new(), None);
  assert_eq!(removed, vec![old]);
}
