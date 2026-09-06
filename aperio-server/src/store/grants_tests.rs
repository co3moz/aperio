//! The grant rules: a specific entry beats `*`, a grant is bounded by the
//! granter's, and a record written before grants existed keeps exactly the
//! reach it had.

use super::*;

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
