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
  log.record("token_created", "tester", "1.2.3.4", None, "name=test");
  log.record("login_success", "tester", "1.2.3.4", None, "user=aperio");
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
    log.record("event", "tester", "1.2.3.4", None, &format!("i={}", i));
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
  log.record("a", "tester", "1.2.3.4", None, "first");
  log.record("b", "tester", "1.2.3.4", None, "second");
  log.record("c", "tester", "1.2.3.4", None, "third");

  let raw = std::fs::read_to_string(&active).unwrap();
  let lines: Vec<&str> = raw.lines().collect();
  let first: AuditEvent = serde_json::from_str(lines[0]).unwrap();
  assert_eq!(first.prev, GENESIS_HASH);
  let second: AuditEvent = serde_json::from_str(lines[1]).unwrap();
  assert_eq!(second.prev, line_hash(lines[0]));
  assert_eq!(verify_chain(&active).unwrap(), None);

  // Chain continues across a restart.
  let mut log2 = AuditLog::load(&dir_str, 0, 0);
  log2.record("d", "tester", "1.2.3.4", None, "fourth");
  assert_eq!(verify_chain(&active).unwrap(), None);

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_hash_chain_detects_tampering() {
  let (dir, dir_str) = temp_dir();
  let active = dir.join("audit.jsonl");

  let mut log = AuditLog::load(&dir_str, 0, 0);
  for i in 0..5 {
    log.record("event", "tester", "1.2.3.4", None, &format!("i={}", i));
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
    log.record("event", "tester", "1.2.3.4", None, &format!("i={}", i));
  }
  // If the last record triggered a rotation the active file does not exist
  // yet; write one more so there is a line to link across the boundary.
  if !dir.join("audit.jsonl").exists() {
    log.record("event", "tester", "1.2.3.4", None, "post-rotation");
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
  log.record("new", "tester", "1.2.3.4", None, "chained");
  assert_eq!(verify_chain(&active).unwrap(), None);
  assert_eq!(log.recent().len(), 2);

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_rotation_zero_files_truncates() {
  let (dir, dir_str) = temp_dir();
  let mut log = AuditLog::load(&dir_str, 200, 0);
  for i in 0..20 {
    log.record("event", "tester", "1.2.3.4", None, &format!("i={}", i));
  }
  let active_size = std::fs::metadata(dir.join("audit.jsonl"))
    .map(|m| m.len())
    .unwrap_or(0);
  assert!(active_size < 400, "got {} bytes", active_size);
  assert!(!dir.join("audit.jsonl.1").exists());
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_prune_older_than_keeps_chain_verifiable() {
  let dir = std::env::temp_dir().join(format!("aperio-audit-prune-{}", uuid::Uuid::new_v4()));
  let _ = std::fs::create_dir_all(&dir);
  let dir_str = dir.to_string_lossy().to_string();

  let mut log = AuditLog::load(&dir_str, 0, 3);
  for i in 0..6 {
    log.record(&format!("event_{i}"), "system", "-", None, "d");
  }
  // Age the first 3 lines in place so a cutoff can bite (events were just
  // written with the current timestamp).
  let path = dir.join("audit.jsonl");
  let raw = std::fs::read_to_string(&path).unwrap();
  let now = crate::store::tokens::now_secs();
  // Only the doomed lines are rewritten; the surviving suffix keeps its
  // exact byte form so its hash chain stays intact after the prune.
  let aged: Vec<String> = raw
    .lines()
    .enumerate()
    .map(|(i, l)| {
      if i < 3 {
        let mut ev: serde_json::Value = serde_json::from_str(l).unwrap();
        ev["ts"] = serde_json::json!(now - 10 * 24 * 3600);
        serde_json::to_string(&ev).unwrap()
      } else {
        l.to_string()
      }
    })
    .collect();
  std::fs::write(&path, format!("{}\n", aged.join("\n"))).unwrap();

  let mut log = AuditLog::load(&dir_str, 0, 3);
  let removed = log.prune_older_than(now - 24 * 3600);
  assert_eq!(removed, 3);

  let raw = std::fs::read_to_string(&path).unwrap();
  assert_eq!(raw.lines().count(), 3);
  // The surviving suffix still verifies (first line exempt by design).
  assert_eq!(verify_chain(&path).unwrap(), None);

  // Idempotent: nothing left to prune.
  assert_eq!(log.prune_older_than(now - 24 * 3600), 0);
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_load_continues_chain_from_rotated_generation() {
  // A restart right after a rotation leaves an empty/absent active file; the
  // chain must resume from the newest rotated generation, not restart.
  let (dir, dir_str) = temp_dir();
  let active = dir.join("audit.jsonl");

  let mut log = AuditLog::load(&dir_str, 0, 0);
  log.record("a", "tester", "1.2.3.4", None, "first");
  log.record("b", "tester", "1.2.3.4", None, "second");
  drop(log);

  // Simulate the active file having been rotated away to generation 1.
  std::fs::rename(&active, dir.join("audit.jsonl.1")).unwrap();

  // Reload with no active file: last_hash comes from generation 1's last line.
  let mut log2 = AuditLog::load(&dir_str, 0, 0);
  log2.record("c", "tester", "1.2.3.4", None, "third");

  let gen1 = std::fs::read_to_string(dir.join("audit.jsonl.1")).unwrap();
  let last_gen1 = gen1.lines().last().unwrap();
  let active_raw = std::fs::read_to_string(&active).unwrap();
  let first: AuditEvent = serde_json::from_str(active_raw.lines().next().unwrap()).unwrap();
  assert_eq!(first.prev, line_hash(last_gen1));

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_recent_ring_evicts_oldest() {
  // The in-memory ring is capped; older events fall off the front.
  let (dir, dir_str) = temp_dir();
  let mut log = AuditLog::load(&dir_str, 0, 0);
  for i in 0..(AUDIT_RECENT_CAP + 5) {
    log.record("event", "tester", "1.2.3.4", None, &format!("i={}", i));
  }
  assert_eq!(log.recent().len(), AUDIT_RECENT_CAP);
  // The very first event was evicted; the newest is retained.
  assert_eq!(log.recent().last().unwrap().details, "i=204");
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_verify_flags_broken_rotated_generation() {
  let (dir, dir_str) = temp_dir();
  // Small threshold so several rotated generations accumulate.
  let mut log = AuditLog::load(&dir_str, 250, 5);
  for i in 0..30 {
    log.record("event", "tester", "1.2.3.4", None, &format!("i={}", i));
  }
  let gen1 = dir.join("audit.jsonl.1");
  assert!(gen1.exists(), "expected a rotated generation to exist");

  // Tamper the first line of generation 1 so the next line's `prev` breaks.
  let raw = std::fs::read_to_string(&gen1).unwrap();
  let mut lines: Vec<String> = raw.lines().map(str::to_string).collect();
  assert!(
    lines.len() >= 2,
    "rotated generation needs >=2 lines to detect"
  );
  let mut ev: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
  ev["details"] = serde_json::json!("tampered");
  lines[0] = serde_json::to_string(&ev).unwrap();
  std::fs::write(&gen1, format!("{}\n", lines.join("\n"))).unwrap();

  let broken = log.verify();
  assert!(
    broken.iter().any(|(name, _)| name == "audit.jsonl.1"),
    "verify() should flag the tampered rotated generation, got {:?}",
    broken
  );
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_prune_deletes_old_rotated_generations() {
  let (dir, dir_str) = temp_dir();
  let mut log = AuditLog::load(&dir_str, 250, 5);
  for i in 0..30 {
    log.record("event", "tester", "1.2.3.4", None, &format!("i={}", i));
  }
  drop(log);

  let now = crate::store::tokens::now_secs();
  // Age every rotated generation well past the cutoff so they get dropped.
  let mut aged_lines = 0usize;
  for n in 1..=5 {
    let gen_path = dir.join(format!("audit.jsonl.{n}"));
    let Ok(raw) = std::fs::read_to_string(&gen_path) else {
      break;
    };
    let aged: Vec<String> = raw
      .lines()
      .map(|l| {
        aged_lines += 1;
        let mut ev: serde_json::Value = serde_json::from_str(l).unwrap();
        ev["ts"] = serde_json::json!(now - 10 * 24 * 3600);
        serde_json::to_string(&ev).unwrap()
      })
      .collect();
    std::fs::write(&gen_path, format!("{}\n", aged.join("\n"))).unwrap();
  }
  assert!(aged_lines > 0, "expected rotated generations to age");
  assert!(dir.join("audit.jsonl.1").exists());

  let mut log = AuditLog::load(&dir_str, 250, 5);
  let removed = log.prune_older_than(now - 24 * 3600);
  assert_eq!(
    removed, aged_lines,
    "all aged rotated lines should be pruned"
  );
  assert!(
    !dir.join("audit.jsonl.1").exists(),
    "the aged rotated generation should be deleted whole"
  );
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_verify_chain_flags_unparseable_line() {
  let (dir, _dir_str) = temp_dir();
  let path = dir.join("audit.jsonl");
  std::fs::write(&path, "this is not json at all\n").unwrap();
  assert_eq!(verify_chain(&path).unwrap(), Some(1));
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_set_rotation_takes_effect() {
  // Rotation is disabled at load, then enabled via set_rotation; the next
  // events must trigger a rotation once the threshold is crossed.
  let (dir, dir_str) = temp_dir();
  let mut log = AuditLog::load(&dir_str, 0, 2);
  log.record("event", "tester", "1.2.3.4", None, "before");
  assert!(!dir.join("audit.jsonl.1").exists());

  log.set_rotation(200, 2);
  for i in 0..10 {
    log.record("event", "tester", "1.2.3.4", None, &format!("i={}", i));
  }
  assert!(
    dir.join("audit.jsonl.1").exists(),
    "set_rotation should enable size-based rotation"
  );
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_verify_reports_clean_and_tampered() {
  let (dir, dir_str) = temp_dir();
  let mut log = AuditLog::load(&dir_str, 0, 0);
  for i in 0..4 {
    log.record("event", "tester", "1.2.3.4", None, &format!("i={}", i));
  }
  // A clean log verifies with no broken files.
  assert!(log.verify().is_empty());

  // Tamper with the active file on disk; verify() must flag it by name.
  let active = dir.join("audit.jsonl");
  let raw = std::fs::read_to_string(&active).unwrap();
  std::fs::write(&active, raw.replace("i=2", "i=X")).unwrap();
  let broken = log.verify();
  assert_eq!(broken.len(), 1);
  assert_eq!(broken[0].0, "audit.jsonl");

  let _ = std::fs::remove_dir_all(&dir);
}
