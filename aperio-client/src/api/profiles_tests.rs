//! What the profile store pins down: a profile's key goes to its own server
//! and nowhere else, whatever the spelling of that server's URL; the file
//! round-trips and is the user's alone; and removing the profile in use
//! leaves another in use rather than none while some remain.

use super::*;

fn profile(server: &str, key: &str) -> Profile {
  Profile {
    server: server.to_string(),
    api_key: key.to_string(),
    key_id: None,
  }
}

#[test]
fn one_server_has_many_spellings_and_another_server_is_another() {
  assert!(same_server("https://t.example.com", "wss://t.example.com/"));
  assert!(same_server(
    "https://T.Example.com/aperio",
    "https://t.example.com"
  ));
  assert!(same_server(
    "https://t.example.com",
    "https://t.example.com:443"
  ));
  assert!(!same_server(
    "https://t.example.com",
    "https://t.example.com:8443"
  ));
  assert!(!same_server(
    "https://t.example.com",
    "http://t.example.com"
  ));
  assert!(!same_server(
    "https://t.example.com",
    "https://evil.example.com"
  ));
}

#[test]
fn the_key_goes_to_its_own_server_only() {
  let mut profiles = Profiles::default();
  assert!(profiles.credential_for(None).is_none(), "not signed in");
  profiles.insert("prod", profile("https://t.example.com", "apk_prod"));
  // Nothing configured: the profile names the server too.
  assert_eq!(
    profiles.credential_for(None),
    Some(("https://t.example.com".to_string(), "apk_prod".to_string()))
  );
  // The same server, spelled the way a tunnel config spells it.
  assert_eq!(
    profiles
      .credential_for(Some("wss://t.example.com"))
      .map(|(_, key)| key),
    Some("apk_prod".to_string())
  );
  // A project pointed at another server is not sent this one's key.
  assert!(
    profiles
      .credential_for(Some("https://other.example.com"))
      .is_none()
  );
}

#[test]
fn removing_the_profile_in_use_leaves_another_in_use() {
  let mut profiles = Profiles::default();
  profiles.insert("b", profile("https://b.example.com", "apk_b"));
  profiles.insert("a", profile("https://a.example.com", "apk_a"));
  assert_eq!(profiles.current.as_deref(), Some("a"), "the last signed in");
  assert_eq!(
    profiles.remove("a").map(|p| p.api_key).as_deref(),
    Some("apk_a")
  );
  assert_eq!(profiles.current.as_deref(), Some("b"));
  assert!(profiles.remove("nope").is_none());
  profiles.remove("b");
  assert!(profiles.current.is_none());
}

#[test]
fn the_file_round_trips_and_a_missing_one_is_no_profiles() {
  let dir = std::env::temp_dir().join(format!("aperio-profiles-{}", uuid::Uuid::new_v4()));
  let path = dir.join("profiles.yaml");
  assert_eq!(Profiles::read(&path).unwrap(), Profiles::default());
  let mut profiles = Profiles::default();
  let mut minted = profile("https://t.example.com", "apk_secret");
  minted.key_id = Some("k-1".into());
  profiles.insert("t.example.com", minted);
  profiles.write(&path).unwrap();
  assert_eq!(Profiles::read(&path).unwrap(), profiles);
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
      mode, 0o600,
      "a file holding admin keys is its owner's alone"
    );
  }
  std::fs::write(&path, "profiles: [not, a, map]").unwrap();
  assert!(
    Profiles::read(&path).is_err(),
    "a broken file is said, not emptied"
  );
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_profile_is_named_after_its_host_unless_told_otherwise() {
  assert_eq!(
    default_name("https://Tunnel.Example.com/aperio"),
    "tunnel.example.com"
  );
  assert_eq!(default_name("wss://t.example.com:8443"), "t.example.com");
}
