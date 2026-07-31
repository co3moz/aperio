use super::*;

fn temp_dir() -> String {
  let dir =
    crate::test_support::test_temp_root().join(format!("orgs-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir.to_string_lossy().to_string()
}

#[test]
fn test_create_unique_and_reserved() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  let a = store.create("acme", Vec::new(), None).unwrap();
  assert_eq!(store.list().len(), 1);

  // Case-insensitive uniqueness and the reserved name.
  assert!(store.create("acme", Vec::new(), None).is_err());
  assert!(store.create("master", Vec::new(), None).is_err());
  assert!(store.create("  ", Vec::new(), None).is_err());
  // A handle is an identifier: `@` is address syntax, `-` and `.` are
  // reserved for it, capitals and non-English letters are two ways to write
  // one name. Anything to read goes in `custom_name`.
  assert!(store.create("acme@corp", Vec::new(), None).is_err());
  assert!(store.create("Acme", Vec::new(), None).is_err());
  assert!(store.create("acme-corp", Vec::new(), None).is_err());
  assert!(store.create("ödeme", Vec::new(), None).is_err());

  // The display name is free text, and changes without the handle moving.
  let named = store
    .create("payments", Vec::new(), Some("Ödeme Servisi".to_string()))
    .unwrap();
  assert_eq!(named.custom_name.as_deref(), Some("Ödeme Servisi"));
  assert!(store.set_custom_name(&named.id, Some("  Ödeme  ".to_string())));
  assert_eq!(
    store.find(&named.id).unwrap().custom_name.as_deref(),
    Some("Ödeme"),
    "trimmed"
  );
  assert!(store.set_custom_name(&named.id, Some("   ".to_string())));
  assert_eq!(
    store.find(&named.id).unwrap().custom_name,
    None,
    "blank clears it rather than storing an empty label"
  );
  assert_eq!(
    store.find(&named.id).unwrap().name,
    "payments",
    "the handle never moves"
  );
  assert!(!store.set_custom_name("no-such-org", None));

  // Survives a reload (the two created above).
  let reloaded = OrgStore::load(&dir);
  assert_eq!(reloaded.list().len(), 2);

  // Delete.
  let mut store = OrgStore::load(&dir);
  assert!(store.delete(&a.id));
  assert!(!store.delete(&a.id));
  assert_eq!(store.list().len(), 1);
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_set_quota_and_persist() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  let org = store.create("acme", Vec::new(), None).unwrap();
  assert!(org.max_tokens.is_none());

  // Set two quotas; leave the others untouched.
  let updated = store
    .set_quota(&org.id, Some(Some(3)), Some(Some(10)), None, None)
    .unwrap();
  assert_eq!(updated.max_clients, Some(3));
  assert_eq!(updated.max_tokens, Some(10));
  assert!(updated.max_users.is_none());

  // Survives reload; 0 clears a quota.
  let mut reloaded = OrgStore::load(&dir);
  assert_eq!(reloaded.find(&org.id).unwrap().max_tokens, Some(10));
  let cleared = reloaded
    .set_quota(&org.id, Some(Some(0)), None, None, None)
    .unwrap();
  assert!(cleared.max_clients.is_none());
  assert_eq!(cleared.max_tokens, Some(10));

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_set_quota_all_fields_and_users_bytes() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  let org = store.create("acme", Vec::new(), None).unwrap();

  // Exercise the max_users and max_bytes_month branches too.
  let updated = store
    .set_quota(
      &org.id,
      Some(Some(1)),
      Some(Some(2)),
      Some(Some(3)),
      Some(Some(4096)),
    )
    .unwrap();
  assert_eq!(updated.max_users, Some(3));
  assert_eq!(updated.max_bytes_month, Some(4096));

  // Clearing max_bytes_month with 0 and leaving the rest unchanged.
  let cleared = store
    .set_quota(&org.id, None, None, None, Some(Some(0)))
    .unwrap();
  assert!(cleared.max_bytes_month.is_none());
  assert_eq!(cleared.max_users, Some(3));

  // Persisted across reload.
  let reloaded = OrgStore::load(&dir);
  let got = reloaded.find(&org.id).unwrap();
  assert_eq!(got.max_users, Some(3));
  assert!(got.max_bytes_month.is_none());

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_import_replaces_and_persists() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  store.create("existing", Vec::new(), None).unwrap();

  let now = crate::store::tokens::now_secs();
  let mk = |name: &str| Organization {
    custom_name: None,
    id: uuid::Uuid::new_v4().to_string(),
    name: name.to_string(),
    created_at: now,
    max_clients: None,
    max_tokens: None,
    max_users: None,
    max_bytes_month: None,
    hostnames: Vec::new(),
    oidc: None,
  };
  let count = store.import(vec![mk("one"), mk("two"), mk("three")]);
  assert_eq!(count, 3);
  assert_eq!(store.list().len(), 3);
  // The pre-import org is gone (import replaces wholesale).
  assert!(!store.list().iter().any(|o| o.name == "existing"));

  // Import result survives a reload.
  let reloaded = OrgStore::load(&dir);
  assert_eq!(reloaded.list().len(), 3);
  assert!(reloaded.list().iter().any(|o| o.name == "two"));

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_set_oidc_set_and_clear() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  let org = store.create("acme", Vec::new(), None).unwrap();
  assert!(org.oidc.is_none());

  let oidc = OrgOidc {
    issuer: "https://issuer.example".into(),
    client_id: "cid".into(),
    client_secret: "secret".into(),
    allowed_emails: vec!["*@example.com".into()],
  };
  let updated = store.set_oidc(&org.id, Some(oidc)).unwrap();
  assert_eq!(
    updated.oidc.as_ref().unwrap().issuer,
    "https://issuer.example"
  );

  // Persisted across reload.
  let mut reloaded = OrgStore::load(&dir);
  assert_eq!(
    reloaded
      .find(&org.id)
      .unwrap()
      .oidc
      .as_ref()
      .unwrap()
      .client_id,
    "cid"
  );

  // Clearing it removes the override.
  let cleared = reloaded.set_oidc(&org.id, None).unwrap();
  assert!(cleared.oidc.is_none());
  let reloaded = OrgStore::load(&dir);
  assert!(reloaded.find(&org.id).unwrap().oidc.is_none());

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_lookups_on_missing_org_are_none() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  assert!(store.find("does-not-exist").is_none());
  assert!(
    store
      .set_quota("does-not-exist", Some(Some(5)), None, None, None)
      .is_none()
  );
  assert!(store.set_oidc("does-not-exist", None).is_none());
  assert!(!store.delete("does-not-exist"));
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_normalize_org_hostname_pattern() {
  // Exact hostnames and subdomain wildcards, normalized to lowercase without
  // a trailing dot or port.
  assert_eq!(
    normalize_org_hostname_pattern("Acme.COM."),
    Some("acme.com".to_string())
  );
  assert_eq!(
    normalize_org_hostname_pattern(" *.acme.com "),
    Some("*.acme.com".to_string())
  );
  assert_eq!(
    normalize_org_hostname_pattern("app.acme.com:8443"),
    Some("app.acme.com".to_string())
  );
  // A bare `*` means "no fence".
  assert_eq!(normalize_org_hostname_pattern("*"), Some("*".to_string()));
  // A wildcard is only valid as the leading label, and needs a parent domain.
  assert_eq!(normalize_org_hostname_pattern("*.com"), None);
  assert_eq!(normalize_org_hostname_pattern("app.*.com"), None);
  assert_eq!(normalize_org_hostname_pattern("ac*me.com"), None);
  assert_eq!(normalize_org_hostname_pattern(""), None);
  assert_eq!(normalize_org_hostname_pattern("   "), None);
}

#[test]
fn test_hostname_in_org_allowlist() {
  // An empty list fences nothing.
  assert!(hostname_in_org_allowlist("anything.example.com", &[]));

  let list = vec!["acme.com".to_string(), "*.acme.example.com".to_string()];
  // Exact entry.
  assert!(hostname_in_org_allowlist("acme.com", &list));
  assert!(hostname_in_org_allowlist("ACME.com.", &list));
  // Wildcard covers subdomains at any depth, but not the parent itself.
  assert!(hostname_in_org_allowlist("app.acme.example.com", &list));
  assert!(hostname_in_org_allowlist("a.b.acme.example.com", &list));
  assert!(!hostname_in_org_allowlist("acme.example.com", &list));
  // Neighbours that merely share a suffix string are not subdomains.
  assert!(!hostname_in_org_allowlist("evilacme.example.com", &list));
  assert!(!hostname_in_org_allowlist("acme.com.evil.net", &list));
  assert!(!hostname_in_org_allowlist("other.com", &list));

  // An explicit `*` entry is unrestricted.
  assert!(hostname_in_org_allowlist("other.com", &["*".to_string()]));
}

#[test]
fn test_set_hostnames_persists_and_scopes_lookup() {
  let dir = temp_dir();
  let mut store = OrgStore::load(&dir);
  let org = store
    .create("acme", vec!["acme.com".to_string()], None)
    .unwrap();
  assert_eq!(org.hostnames, vec!["acme.com".to_string()]);

  let updated = store
    .set_hostnames(&org.id, vec!["*.acme.com".to_string()])
    .unwrap();
  assert_eq!(updated.hostnames, vec!["*.acme.com".to_string()]);

  // Survives a reload, and the lookup helper resolves it by id.
  let reloaded = OrgStore::load(&dir);
  assert_eq!(
    reloaded.hostnames_of(Some(&org.id)),
    vec!["*.acme.com".to_string()]
  );
  // The master org (None) is never fenced, nor is an unknown id.
  assert!(reloaded.hostnames_of(None).is_empty());
  assert!(reloaded.hostnames_of(Some("nope")).is_empty());

  // An empty list clears the fence.
  let mut store = OrgStore::load(&dir);
  assert!(
    store
      .set_hostnames(&org.id, Vec::new())
      .unwrap()
      .hostnames
      .is_empty()
  );
  assert!(store.set_hostnames("does-not-exist", Vec::new()).is_none());
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_subtree_is_covered_only_by_something_that_owns_the_subtree() {
  // What authorizes "put every subdomain of robogon.com into maintenance".
  assert!(pattern_covers_pattern("*.robogon.com", "*.robogon.com"));
  assert!(pattern_covers_pattern("*.robogon.com", "*.eu.robogon.com"));
  assert!(pattern_covers_pattern("*.robogon.com", "test.robogon.com"));
  assert!(pattern_covers_pattern("*.robogon.com", "a.b.robogon.com"));
  assert!(pattern_covers_pattern("*", "*.robogon.com"));
  assert!(pattern_covers_pattern("robogon.com", "robogon.com"));

  // An exact entry owns one name, so it cannot authorize the subtree, and
  // the apex is not inside its own wildcard.
  assert!(!pattern_covers_pattern("robogon.com", "*.robogon.com"));
  assert!(!pattern_covers_pattern("*.robogon.com", "robogon.com"));
  assert!(!pattern_covers_pattern("*.other.com", "*.robogon.com"));
  assert!(!pattern_covers_pattern("*.eu.robogon.com", "*.robogon.com"));
}

#[test]
fn overlap_is_symmetric_where_coverage_is_not() {
  // The question "is another tenant already inside this subtree" is answered
  // in both directions: a tenant fenced to a *deeper* wildcard is inside a
  // shallower one, and the reverse.
  assert!(patterns_overlap("*.robogon.com", "*.eu.robogon.com"));
  assert!(patterns_overlap("*.eu.robogon.com", "*.robogon.com"));
  assert!(patterns_overlap("*.robogon.com", "test.robogon.com"));
  assert!(patterns_overlap("test.robogon.com", "*.robogon.com"));
  assert!(!patterns_overlap("*.robogon.com", "robogon.com"));
  assert!(!patterns_overlap("*.robogon.com", "*.other.com"));
}

#[test]
fn pattern_matching_survives_the_hostnames_a_visitor_can_send() {
  // The Host header is attacker-controlled, and this now indexes bytes.
  assert!(pattern_matches_host("*.acme.com", "app.acme.com"));
  assert!(pattern_matches_host("*.acme.com", "APP.ACME.COM"));
  assert!(pattern_matches_host("*.acme.com", "app.acme.com."));
  assert!(!pattern_matches_host("*.acme.com", "acme.com"));
  assert!(!pattern_matches_host("*.acme.com", ".acme.com"));
  assert!(!pattern_matches_host("*.acme.com", "notacme.com"));
  assert!(!pattern_matches_host("*.acme.com", "xacme.com"));
  assert!(!pattern_matches_host("*.acme.com", ""));
  assert!(pattern_matches_host("acme.com", "ACME.com."));
  assert!(pattern_matches_host("*", "anything"));
  // Multi-byte input must compare false, not panic on a slice boundary.
  assert!(!pattern_matches_host("*.acme.com", "ü.acme.co"));
  assert!(!pattern_matches_host("*.acme.com", "ünicode"));
  assert!(!pattern_matches_host("üacme.com", "acme.com"));
  // A suffix longer than the host cannot underflow the index arithmetic.
  assert!(!pattern_matches_host(
    "*.a.very.long.suffix.example",
    "short.example"
  ));
}

// --- A partial leftmost label, for a fleet naming convention ---

#[test]
fn a_partial_label_pattern_is_accepted_and_normalized() {
  // What this exists for: an organization that owns every `<name>-pi` box
  // under a domain, and should not have to be handed the whole domain to say
  // so. The shape is the one `random_subdomain` already accepts.
  assert_eq!(
    normalize_org_hostname_pattern(" *-PI.Robogon.com. "),
    Some("*-pi.robogon.com".to_string())
  );
  assert_eq!(
    normalize_org_hostname_pattern("dev-*.robogon.com"),
    Some("dev-*.robogon.com".to_string())
  );
  // The old shapes are untouched.
  assert_eq!(
    normalize_org_hostname_pattern("*.robogon.com"),
    Some("*.robogon.com".to_string())
  );
  assert_eq!(
    normalize_org_hostname_pattern("robogon.com"),
    Some("robogon.com".to_string())
  );
  assert_eq!(normalize_org_hostname_pattern("*"), Some("*".to_string()));
}

#[test]
fn a_placeholder_that_would_not_do_what_it_looks_like_is_refused() {
  // Two placeholders read as if both were free; only the first would be.
  assert_eq!(normalize_org_hostname_pattern("*-pi-*.robogon.com"), None);
  // Outside the leftmost label it is not a label pattern at all.
  assert_eq!(normalize_org_hostname_pattern("pi.*.robogon.com"), None);
  assert_eq!(normalize_org_hostname_pattern("pi.robogon.*"), None);
  // A partial label needs a domain under it, like the subdomain wildcard.
  assert_eq!(normalize_org_hostname_pattern("*-pi.com"), None);
  assert_eq!(normalize_org_hostname_pattern("*-pi"), None);
  // `*.` is `*` with the root label written out, and the trailing dot is
  // trimmed before anything else looks at the string, so it means what it
  // says: unrestricted.
  assert_eq!(normalize_org_hostname_pattern("*."), Some("*".to_string()));
}

#[test]
fn a_partial_label_matches_one_label_and_only_around_the_placeholder() {
  let p = "*-pi.robogon.com";
  assert!(pattern_matches_host(p, "raspberry-pi.robogon.com"));
  assert!(pattern_matches_host(p, "a-pi.robogon.com"));
  assert!(pattern_matches_host(p, "RASPBERRY-PI.Robogon.com"));
  assert!(pattern_matches_host(p, "raspberry-pi.robogon.com."));
  // The placeholder must stand for something.
  assert!(!pattern_matches_host(p, "-pi.robogon.com"));
  // One label: a deeper subdomain is a different host, not this one.
  assert!(!pattern_matches_host(p, "a.raspberry-pi.robogon.com"));
  // The rest of the name is matched exactly.
  assert!(!pattern_matches_host(p, "raspberry-pi.other.com"));
  assert!(!pattern_matches_host(
    p,
    "raspberry-pi.robogon.com.evil.com"
  ));
  // And the suffix has to be the suffix.
  assert!(!pattern_matches_host(p, "raspberry-pie.robogon.com"));
  assert!(!pattern_matches_host(p, "robogon.com"));

  // A prefix pattern is the mirror image.
  let q = "dev-*.robogon.com";
  assert!(pattern_matches_host(q, "dev-1.robogon.com"));
  assert!(!pattern_matches_host(q, "dev-.robogon.com"));
  assert!(!pattern_matches_host(q, "prod-1.robogon.com"));

  // Not UTF-8 boundaries, not a panic: the Host header is attacker-controlled.
  assert!(!pattern_matches_host(p, "ü-pi.robogon.co"));
  assert!(!pattern_matches_host(p, ""));
  assert!(!pattern_matches_host(p, "robogon"));
}

#[test]
fn a_partial_label_sits_under_the_domain_wildcard_and_owns_nothing_wider() {
  // Coverage grants permission, so it says yes only where it can prove it.
  assert!(pattern_covers_pattern("*.robogon.com", "*-pi.robogon.com"));
  assert!(pattern_covers_pattern("*", "*-pi.robogon.com"));
  assert!(pattern_covers_pattern(
    "*-pi.robogon.com",
    "*-pi.robogon.com"
  ));
  assert!(pattern_covers_pattern(
    "*-pi.robogon.com",
    "a-pi.robogon.com"
  ));

  assert!(!pattern_covers_pattern("*-pi.robogon.com", "*.robogon.com"));
  assert!(!pattern_covers_pattern(
    "*-pi.robogon.com",
    "dev-*.robogon.com"
  ));
  assert!(!pattern_covers_pattern("*-pi.robogon.com", "robogon.com"));
  assert!(!pattern_covers_pattern("robogon.com", "*-pi.robogon.com"));
  assert!(!pattern_covers_pattern(
    "*.eu.robogon.com",
    "*-pi.robogon.com"
  ));
  assert!(pattern_covers_pattern(
    "*.robogon.com",
    "*-pi.eu.robogon.com"
  ));
}

#[test]
fn two_partial_labels_on_one_domain_are_treated_as_overlapping() {
  // `dev-pi.robogon.com` matches both, and neither covers the other. Overlap
  // refuses an action rather than granting one, so the unprovable case has to
  // answer yes: master is told to name the hostname instead of the domain.
  assert!(patterns_overlap("*-pi.robogon.com", "dev-*.robogon.com"));
  assert!(patterns_overlap("dev-*.robogon.com", "*-pi.robogon.com"));
  // A different domain cannot share a name with it.
  assert!(!patterns_overlap("*-pi.robogon.com", "dev-*.other.com"));
  // And the ordinary cases still answer as they did.
  assert!(patterns_overlap("*.robogon.com", "*-pi.robogon.com"));
  assert!(!patterns_overlap("*-pi.robogon.com", "robogon.com"));
}

#[test]
fn the_fence_admits_the_fleet_and_refuses_the_domain_around_it() {
  // The question this answers, asked in exactly these words: with the org
  // fenced to `*-pi.robogon.com`, a token of that org binds
  // `test-pi.robogon.com` and cannot bind `test.robogon.com`.
  let fence = vec!["*-pi.robogon.com".to_string()];
  assert!(hostname_in_org_allowlist("test-pi.robogon.com", &fence));
  assert!(!hostname_in_org_allowlist("test.robogon.com", &fence));

  // The neighbours of that answer, so it cannot drift into something looser:
  assert!(!hostname_in_org_allowlist("robogon.com", &fence));
  assert!(!hostname_in_org_allowlist("pi.robogon.com", &fence));
  assert!(!hostname_in_org_allowlist("test-pi.evil.com", &fence));
  assert!(!hostname_in_org_allowlist(
    "test-pi.robogon.com.evil.com",
    &fence
  ));
  assert!(!hostname_in_org_allowlist("a.test-pi.robogon.com", &fence));
  assert!(hostname_in_org_allowlist("TEST-PI.Robogon.com", &fence));
}
