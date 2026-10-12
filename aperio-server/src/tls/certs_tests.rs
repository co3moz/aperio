//! Which certificate a handshake gets: an issued one for its exact name,
//! then a file's exact name, then a file's wildcard, and the first file's
//! when it names nothing; and that files are read again when they change,
//! keeping the last good one when they cannot be.

use super::*;

/// A self-signed certificate and key for `names`, as PEM.
pub(crate) fn self_signed(names: &[&str]) -> (String, String) {
  let key = rcgen::KeyPair::generate().unwrap();
  let params =
    rcgen::CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>()).unwrap();
  let cert = params.self_signed(&key).unwrap();
  (cert.pem(), key.serialize_pem())
}

fn write_pair(dir: &Path, stem: &str, names: &[&str]) -> CertificateFiles {
  let (cert, key) = self_signed(names);
  let cert_path = dir.join(format!("{stem}.pem"));
  let key_path = dir.join(format!("{stem}.key"));
  std::fs::write(&cert_path, cert).unwrap();
  std::fs::write(&key_path, key).unwrap();
  CertificateFiles {
    cert: cert_path.to_string_lossy().into_owned(),
    key: key_path.to_string_lossy().into_owned(),
  }
}

fn temp_dir(name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("aperio-tls-{name}-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

/// Which names the presented certificate carries.
fn names_of(key: &Arc<CertifiedKey>) -> Vec<String> {
  names_and_expiry(&key.cert[0]).unwrap().0
}

#[test]
fn a_wildcard_covers_one_label_and_nothing_else() {
  assert!(covers("*.example.com", "app.example.com"));
  assert!(!covers("*.example.com", "example.com"));
  assert!(!covers("*.example.com", "a.b.example.com"));
  assert!(covers("example.com", "example.com"));
  assert!(!covers("example.com", "app.example.com"));
}

#[test]
fn the_most_specific_certificate_is_presented() {
  let dir = temp_dir("lookup");
  let wildcard = write_pair(&dir, "wild", &["*.example.com"]);
  let exact = write_pair(&dir, "exact", &["api.example.com"]);
  let store = CertStore::new(&[wildcard, exact], Some(dir.join("issued")));

  assert_eq!(
    names_of(&store.lookup(Some("api.example.com")).unwrap()),
    ["api.example.com"]
  );
  assert_eq!(
    names_of(&store.lookup(Some("APP.example.com")).unwrap()),
    ["*.example.com"]
  );
  assert!(store.lookup(Some("example.org")).is_none());
  // No name in the handshake: the first file's, as a default server.
  assert_eq!(names_of(&store.lookup(None).unwrap()), ["*.example.com"]);

  // An issued certificate wins over a file's for its exact name.
  let (cert, key) = self_signed(&["app.example.com"]);
  store.store_issued("app.example.com", &cert, &key).unwrap();
  assert_eq!(
    names_of(&store.lookup(Some("app.example.com")).unwrap()),
    ["app.example.com"]
  );
  let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_issued_certificate_is_kept_across_a_restart() {
  let dir = temp_dir("issued");
  let issued = dir.join("issued");
  let store = CertStore::new(&[], Some(issued.clone()));
  let (cert, key) = self_signed(&["app.example.com"]);
  let loaded = store.store_issued("app.example.com", &cert, &key).unwrap();
  assert!(loaded.not_after > chrono::Utc::now().timestamp());

  let again = CertStore::new(&[], Some(issued));
  assert!(again.lookup(Some("app.example.com")).is_some());
  assert_eq!(
    again.issued("app.example.com").unwrap().names,
    ["app.example.com"]
  );
  let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_certificate_due_for_renewal_is_named() {
  let dir = temp_dir("due");
  let store = CertStore::new(&[], Some(dir.clone()));
  let (cert, key) = self_signed(&["app.example.com"]);
  let loaded = store.store_issued("app.example.com", &cert, &key).unwrap();
  // rcgen's default validity runs to the year 4096.
  assert!(
    store
      .due(loaded.not_after - 10, 30)
      .contains(&"app.example.com".to_string())
  );
  assert!(
    store
      .due(chrono::Utc::now().timestamp(), 30 * 86_400)
      .is_empty()
  );
  let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_changed_file_is_read_again_and_a_broken_one_keeps_the_last_good() {
  let dir = temp_dir("refresh");
  let files = write_pair(&dir, "site", &["one.example.com"]);
  let store = CertStore::new(std::slice::from_ref(&files), None);
  assert!(store.lookup(Some("one.example.com")).is_some());

  // A renewal by another tool: new names, a later modification time.
  std::thread::sleep(std::time::Duration::from_millis(20));
  let (cert, key) = self_signed(&["two.example.com"]);
  std::fs::write(&files.cert, cert).unwrap();
  std::fs::write(&files.key, key).unwrap();
  store.refresh_files();
  assert!(store.lookup(Some("two.example.com")).is_some());

  // Caught half-written: the working certificate stays.
  std::thread::sleep(std::time::Duration::from_millis(20));
  std::fs::write(&files.cert, "not a certificate").unwrap();
  store.refresh_files();
  assert!(store.lookup(Some("two.example.com")).is_some());
  let _ = std::fs::remove_dir_all(dir);
}
