use super::*;

fn temp_dir() -> (PathBuf, String) {
  let dir = std::env::temp_dir().join(format!("aperio-audit-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let s = dir.to_string_lossy().to_string();
  (dir, s)
}

#[test]
fn test_record_and_reload() {
  let (dir, dir_str) = temp_dir();

  let mut log = AuditLog::load(&dir_str, 0, 0);
  log.record("token_created", "1.2.3.4", "name=test");
  log.record("login_success", "1.2.3.4", "user=aperio");
  assert_eq!(log.recent().len(), 2);

  let log2 = AuditLog::load(&dir_str, 0, 0);
  assert_eq!(log2.recent().len(), 2);
  assert_eq!(log2.recent()[0].event, "token_created");
  assert_eq!(log2.recent()[1].actor_ip, "1.2.3.4");

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_rotation_by_size() {
  let (dir, dir_str) = temp_dir();
  let active = dir.join("audit.jsonl");

  // Tiny threshold: every few events trigger a rotation; keep 2 generations.
  let mut log = AuditLog::load(&dir_str, 300, 2);
  for i in 0..50 {
    log.record("event", "1.2.3.4", &format!("i={}", i));
  }

  let active_size = std::fs::metadata(&active).map(|m| m.len()).unwrap_or(0);
  assert!(
    active_size < 600,
    "active file should stay near the threshold, got {} bytes",
    active_size
  );
  assert!(dir.join("audit.jsonl.1").exists(), "generation 1 missing");
  assert!(dir.join("audit.jsonl.2").exists(), "generation 2 missing");
  assert!(
    !dir.join("audit.jsonl.3").exists(),
    "generation beyond max_files must be dropped"
  );

  // In-memory ring is unaffected by rotation.
  assert_eq!(log.recent().len(), 50);

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_rotation_zero_files_truncates() {
  let (dir, dir_str) = temp_dir();
  let mut log = AuditLog::load(&dir_str, 200, 0);
  for i in 0..20 {
    log.record("event", "1.2.3.4", &format!("i={}", i));
  }
  let active_size = std::fs::metadata(dir.join("audit.jsonl"))
    .map(|m| m.len())
    .unwrap_or(0);
  assert!(active_size < 400, "got {} bytes", active_size);
  assert!(!dir.join("audit.jsonl.1").exists());
  let _ = std::fs::remove_dir_all(&dir);
}
