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
fn test_hash_chain_links_events() {
  let (dir, dir_str) = temp_dir();
  let active = dir.join("audit.jsonl");

  let mut log = AuditLog::load(&dir_str, 0, 0);
  log.record("a", "1.2.3.4", "first");
  log.record("b", "1.2.3.4", "second");
  log.record("c", "1.2.3.4", "third");

  let raw = std::fs::read_to_string(&active).unwrap();
  let lines: Vec<&str> = raw.lines().collect();
  let first: AuditEvent = serde_json::from_str(lines[0]).unwrap();
  assert_eq!(first.prev, GENESIS_HASH);
  let second: AuditEvent = serde_json::from_str(lines[1]).unwrap();
  assert_eq!(second.prev, line_hash(lines[0]));
  assert_eq!(verify_chain(&active).unwrap(), None);

  // Chain continues across a restart.
  let mut log2 = AuditLog::load(&dir_str, 0, 0);
  log2.record("d", "1.2.3.4", "fourth");
  assert_eq!(verify_chain(&active).unwrap(), None);

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_hash_chain_detects_tampering() {
  let (dir, dir_str) = temp_dir();
  let active = dir.join("audit.jsonl");

  let mut log = AuditLog::load(&dir_str, 0, 0);
  for i in 0..5 {
    log.record("event", "1.2.3.4", &format!("i={}", i));
  }

  let raw = std::fs::read_to_string(&active).unwrap();

  // Modifying a line breaks the next line's prev.
  let tampered = raw.replace("i=2", "i=X");
  std::fs::write(&active, &tampered).unwrap();
  assert_eq!(verify_chain(&active).unwrap(), Some(4));

  // Deleting a line is also detected.
  let deleted: Vec<&str> = raw.lines().filter(|l| !l.contains("i=2")).collect();
  std::fs::write(&active, deleted.join("\n")).unwrap();
  assert_eq!(verify_chain(&active).unwrap(), Some(3));

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_hash_chain_spans_rotation() {
  let (dir, dir_str) = temp_dir();

  let mut log = AuditLog::load(&dir_str, 300, 2);
  for i in 0..20 {
    log.record("event", "1.2.3.4", &format!("i={}", i));
  }
  // If the last record triggered a rotation the active file does not exist
  // yet; write one more so there is a line to link across the boundary.
  if !dir.join("audit.jsonl").exists() {
    log.record("event", "1.2.3.4", "post-rotation");
  }
  drop(log);

  // Each file's internal chain is intact...
  assert_eq!(verify_chain(&dir.join("audit.jsonl")).unwrap(), None);
  assert_eq!(verify_chain(&dir.join("audit.jsonl.1")).unwrap(), None);
  // ...and the active file's first line links to generation 1's last line.
  let gen1 = std::fs::read_to_string(dir.join("audit.jsonl.1")).unwrap();
  let last_gen1 = gen1.lines().last().unwrap();
  let active = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
  let first: AuditEvent = serde_json::from_str(active.lines().next().unwrap()).unwrap();
  assert_eq!(first.prev, line_hash(last_gen1));

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_hash_chain_accepts_pre_chain_lines() {
  let (dir, dir_str) = temp_dir();
  let active = dir.join("audit.jsonl");

  // Simulate a log written before the chain existed (no `prev` field).
  std::fs::write(
    &active,
    "{\"ts\":1,\"timestamp\":\"t\",\"event\":\"old\",\"actor_ip\":\"1.2.3.4\",\"details\":\"legacy\"}\n",
  )
  .unwrap();
  assert_eq!(verify_chain(&active).unwrap(), None);

  // New events chain onto the legacy line.
  let mut log = AuditLog::load(&dir_str, 0, 0);
  log.record("new", "1.2.3.4", "chained");
  assert_eq!(verify_chain(&active).unwrap(), None);
  assert_eq!(log.recent().len(), 2);

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
