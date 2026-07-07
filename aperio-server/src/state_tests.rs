use super::*;

fn perms(hostnames: &[&str], paths: &[&str]) -> ClientPerms {
  ClientPerms {
    master: false,
    hostnames: hostnames.iter().map(|s| s.to_string()).collect(),
    paths: paths.iter().map(|s| s.to_string()).collect(),
    token_name: Some("t".to_string()),
    token_id: Some("id".to_string()),
    allow_public: false,
  }
}

#[test]
fn master_perms_allow_everything() {
  let m = ClientPerms::master();
  assert!(m.master);
  assert!(m.allow_public);
  assert!(m.hostname_allowed("anything.example.com"));
  assert!(m.path_allowed("/whatever"));
}

#[test]
fn empty_lists_are_unrestricted() {
  let p = perms(&[], &[]);
  assert!(p.hostname_allowed("a.example.com"));
  assert!(p.path_allowed("/api"));
}

#[test]
fn wildcard_entry_is_unrestricted() {
  let p = perms(&["*"], &["*"]);
  assert!(p.hostname_allowed("a.example.com"));
  assert!(p.path_allowed("/anything"));
}

#[test]
fn specific_entries_gate_exact_values() {
  let p = perms(&["a.example.com"], &["/api"]);
  assert!(p.hostname_allowed("a.example.com"));
  assert!(!p.hostname_allowed("b.example.com"));
  assert!(p.path_allowed("/api"));
  assert!(!p.path_allowed("/other"));
}

#[test]
fn granted_hostnames_excludes_wildcard() {
  let p = perms(&["a.example.com", "*", "b.example.com"], &[]);
  assert_eq!(
    p.granted_hostnames(),
    vec!["a.example.com".to_string(), "b.example.com".to_string()]
  );
}

#[test]
fn granted_path_is_first_specific() {
  let p = perms(&[], &["*", "/api", "/v2"]);
  assert_eq!(p.granted_path(), Some("/api".to_string()));

  // Only a wildcard → no specific grant.
  let wild = perms(&[], &["*"]);
  assert_eq!(wild.granted_path(), None);
}
