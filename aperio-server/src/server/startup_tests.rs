//! What has to work before the server exists: binding the listener in both
//! modes, and the `--verify-audit` command over the audit hash chain.

use crate::store::audit::AuditLog;

// ---------------------------------------------------------------------------
// bind_listener, plain and SO_REUSEPORT TCP binding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_bind_listener_plain_and_reuseport() {
  use crate::bind_listener;

  // Plain listener on an ephemeral port.
  let l = bind_listener("127.0.0.1", 0, false)
    .await
    .expect("plain bind");
  assert!(l.local_addr().unwrap().port() > 0);

  // SO_REUSEPORT path over IPv4 (Domain::IPV4 branch).
  let l = bind_listener("127.0.0.1", 0, true)
    .await
    .expect("reuseport v4 bind");
  assert!(l.local_addr().unwrap().ip().is_ipv4());

  // SO_REUSEPORT path over IPv6 (Domain::IPV6 branch). Skipped gracefully on
  // hosts without a loopback ::1.
  if let Ok(l) = bind_listener("::1", 0, true).await {
    assert!(l.local_addr().unwrap().ip().is_ipv6());
  }

  // An unresolvable host returns an error instead of panicking.
  assert!(
    bind_listener("no.such.host.invalid.", 0, true)
      .await
      .is_err()
  );
}

// ---------------------------------------------------------------------------
// verify_audit, the --verify-audit CLI over the audit hash chain
// ---------------------------------------------------------------------------

/// Serializes the tests below that read/write the process-global
/// APERIO_DATA_DIR environment variable.
static AUDIT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn restore_data_dir(prev: Option<String>) {
  match prev {
    Some(v) => unsafe { std::env::set_var("APERIO_DATA_DIR", v) },
    None => unsafe { std::env::remove_var("APERIO_DATA_DIR") },
  }
}

#[test]
fn test_verify_audit_intact_and_missing() {
  use crate::verify_audit;
  let _lock = AUDIT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let prev = std::env::var("APERIO_DATA_DIR").ok();

  // A freshly written, well-formed audit log verifies intact → exit 0.
  let dir =
    crate::test_support::test_temp_root().join(format!("verify-ok-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  {
    let mut log = AuditLog::load(&dir.to_string_lossy(), 10 * 1024 * 1024, 3);
    log.record("login", "admin", "127.0.0.1", None, "ok");
    log.record("logout", "admin", "127.0.0.1", None, "bye");
  }
  unsafe { std::env::set_var("APERIO_DATA_DIR", &dir) };
  assert_eq!(verify_audit(), 0);

  // A directory with no audit log → nothing to verify → exit 0.
  let empty =
    crate::test_support::test_temp_root().join(format!("verify-empty-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&empty).unwrap();
  unsafe { std::env::set_var("APERIO_DATA_DIR", &empty) };
  assert_eq!(verify_audit(), 0);

  restore_data_dir(prev);
}

#[test]
fn test_verify_audit_detects_tampering_across_generations() {
  use crate::verify_audit;
  let _lock = AUDIT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let prev = std::env::var("APERIO_DATA_DIR").ok();

  let dir =
    crate::test_support::test_temp_root().join(format!("verify-bad-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  {
    let mut log = AuditLog::load(&dir.to_string_lossy(), 10 * 1024 * 1024, 3);
    log.record("login", "admin", "127.0.0.1", None, "a");
    log.record("login", "admin", "127.0.0.1", None, "b");
  }
  // Keep an intact rotated generation, then tamper the active file so the
  // verifier walks both files and reports exactly one broken chain → exit 1.
  std::fs::copy(dir.join("audit.jsonl"), dir.join("audit.jsonl.1")).unwrap();
  std::fs::write(
    dir.join("audit.jsonl"),
    "{\"not\":\"a valid chained audit line\"}\n",
  )
  .unwrap();
  unsafe { std::env::set_var("APERIO_DATA_DIR", &dir) };
  assert_eq!(verify_audit(), 1);

  restore_data_dir(prev);
}

#[test]
fn test_verify_audit_reports_unreadable_file() {
  use crate::verify_audit;
  let _lock = AUDIT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let prev = std::env::var("APERIO_DATA_DIR").ok();

  // An audit.jsonl that is actually a directory cannot be read as a file, so
  // the verifier reports it as unreadable (the `Err` arm) → exit 1.
  let dir =
    crate::test_support::test_temp_root().join(format!("verify-unread-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(dir.join("audit.jsonl")).unwrap();
  unsafe { std::env::set_var("APERIO_DATA_DIR", &dir) };
  assert_eq!(verify_audit(), 1);

  restore_data_dir(prev);
}
