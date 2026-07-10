use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use tracing::{error, info, warn};

/// Maximum number of audit events kept in memory for the dashboard.
const AUDIT_RECENT_CAP: usize = 200;

/// `prev` value of the first event in a brand-new log (no predecessor).
const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A single administrative/security event.
#[derive(Serialize, Deserialize, Clone, utoipa::ToSchema)]
pub struct AuditEvent {
  /// Unix timestamp in seconds.
  pub ts: u64,
  /// Human-readable timestamp.
  pub timestamp: String,
  /// Event kind, e.g. "login_success", "token_created", "client_connected".
  pub event: String,
  /// IP address of the actor that triggered the event.
  pub actor_ip: String,
  /// Free-form details (token name, client id, hostname, ...).
  pub details: String,
  /// SHA-256 (hex) of the previous line exactly as written to the file,
  /// forming a tamper-evident hash chain. All-zeros for the first event;
  /// empty on lines written before the chain existed.
  #[serde(default)]
  pub prev: String,
}

/// Hex SHA-256 of a raw audit line (without the trailing newline).
fn line_hash(line: &str) -> String {
  let mut hasher = Sha256::default();
  hasher.update(line.as_bytes());
  hasher
    .finalize()
    .iter()
    .map(|b| format!("{:02x}", b))
    .collect()
}

/// Append-only audit log: events go to `<data_dir>/audit.jsonl` and a bounded
/// in-memory ring buffer serves the dashboard.
///
/// The file is size-rotated so long-lived installations cannot fill the disk:
/// once it exceeds `max_size` bytes it is renamed to `audit.jsonl.1` (shifting
/// older generations up to `max_files`, the oldest is dropped) and a fresh
/// file is started.
pub struct AuditLog {
  path: PathBuf,
  recent: VecDeque<AuditEvent>,
  /// Rotation threshold in bytes; 0 disables rotation.
  max_size: u64,
  /// Number of rotated generations kept (`audit.jsonl.1` .. `.N`).
  max_files: usize,
  /// Size of the active file, tracked to avoid a stat per event.
  current_size: u64,
  /// Hash of the last line written, carried into the next event's `prev`
  /// field. Survives rotation so the chain spans generations.
  last_hash: String,
}

impl AuditLog {
  /// Opens the audit log and pre-loads the most recent events from disk so
  /// the dashboard has history right after a restart.
  ///
  /// `max_size` = rotation threshold in bytes (0 disables rotation);
  /// `max_files` = rotated generations to keep.
  pub fn load(data_dir: &str, max_size: u64, max_files: usize) -> Self {
    let path = PathBuf::from(data_dir).join("audit.jsonl");
    let mut recent = VecDeque::with_capacity(AUDIT_RECENT_CAP);
    let mut last_hash = GENESIS_HASH.to_string();
    if let Ok(raw) = std::fs::read_to_string(&path) {
      for line in raw.lines().rev().take(AUDIT_RECENT_CAP) {
        if let Ok(ev) = serde_json::from_str::<AuditEvent>(line) {
          recent.push_front(ev);
        }
      }
      if let Some(last) = raw.lines().rfind(|l| !l.trim().is_empty()) {
        last_hash = line_hash(last);
      }
    }
    if last_hash == GENESIS_HASH {
      // Active file empty (e.g. restart right after rotation): continue the
      // chain from the newest rotated generation instead of restarting it.
      if let Ok(raw) = std::fs::read_to_string(format!("{}.1", path.display()))
        && let Some(last) = raw.lines().rfind(|l| !l.trim().is_empty())
      {
        last_hash = line_hash(last);
      }
    }
    if !recent.is_empty() {
      info!(
        "Loaded {} recent audit events from {:?}",
        recent.len(),
        path
      );
    }
    if let Ok(Some(broken)) = verify_chain(&path) {
      warn!(
        "Audit log hash chain broken at line {} of {:?} — the file may have been tampered with",
        broken, path
      );
    }
    let current_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    AuditLog {
      path,
      recent,
      max_size,
      max_files,
      current_size,
      last_hash,
    }
  }

  /// Replaces the rotation policy at runtime (dashboard settings). Takes
  /// effect from the next recorded event.
  pub fn set_rotation(&mut self, max_size: u64, max_files: usize) {
    self.max_size = max_size;
    self.max_files = max_files;
  }

  /// Records an event: appends a JSON line to the file and to the ring buffer.
  pub fn record(&mut self, event: &str, actor_ip: &str, details: &str) {
    let now = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap_or_default()
      .as_secs();
    let ev = AuditEvent {
      ts: now,
      // RFC3339 with offset so the display timestamp is unambiguous across zones.
      timestamp: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
      event: event.to_string(),
      actor_ip: actor_ip.to_string(),
      details: details.to_string(),
      prev: self.last_hash.clone(),
    };
    if self.recent.len() >= AUDIT_RECENT_CAP {
      self.recent.pop_front();
    }
    self.recent.push_back(ev.clone());
    match serde_json::to_string(&ev) {
      Ok(line) => {
        let res = std::fs::OpenOptions::new()
          .create(true)
          .append(true)
          .open(&self.path)
          .and_then(|mut f| writeln!(f, "{}", line));
        match res {
          Ok(()) => {
            self.last_hash = line_hash(&line);
            self.current_size += line.len() as u64 + 1;
            if self.max_size > 0 && self.current_size >= self.max_size {
              self.rotate();
            }
          }
          Err(e) => error!("Failed to append audit event to {:?}: {}", self.path, e),
        }
      }
      Err(e) => error!("Failed to serialize audit event: {}", e),
    }
  }

  /// Rotates the active file: `audit.jsonl` becomes `audit.jsonl.1`, older
  /// generations shift up, anything beyond `max_files` is dropped. With
  /// `max_files == 0` the active file is simply truncated.
  fn rotate(&mut self) {
    if self.max_files == 0 {
      if let Err(e) = std::fs::remove_file(&self.path) {
        warn!("Audit rotation failed to truncate {:?}: {}", self.path, e);
      }
    } else {
      let generation = |n: usize| PathBuf::from(format!("{}.{}", self.path.display(), n));
      let _ = std::fs::remove_file(generation(self.max_files));
      for n in (1..self.max_files).rev() {
        let _ = std::fs::rename(generation(n), generation(n + 1));
      }
      if let Err(e) = std::fs::rename(&self.path, generation(1)) {
        warn!("Audit rotation failed to rename {:?}: {}", self.path, e);
        return;
      }
      info!(
        "Rotated audit log at {} bytes ({} generation(s) kept)",
        self.current_size, self.max_files
      );
    }
    self.current_size = 0;
  }

  /// Recent events, oldest first.
  pub fn recent(&self) -> Vec<AuditEvent> {
    self.recent.iter().cloned().collect()
  }
}

/// Verifies the hash chain of one audit file: every line's `prev` must equal
/// the SHA-256 of the line before it. Returns the 1-based number of the first
/// line that breaks the chain (tampered, reordered, or deleted predecessor),
/// or `None` if the chain is intact. Pre-chain lines (empty `prev`) and a
/// leading genesis/rotation boundary are accepted.
pub fn verify_chain(path: &std::path::Path) -> std::io::Result<Option<usize>> {
  let raw = std::fs::read_to_string(path)?;
  let mut prev_line: Option<&str> = None;
  for (idx, line) in raw.lines().filter(|l| !l.trim().is_empty()).enumerate() {
    let Ok(ev) = serde_json::from_str::<AuditEvent>(line) else {
      return Ok(Some(idx + 1));
    };
    // Lines written before the chain existed carry no `prev`; skip them but
    // still let them anchor the next line's hash.
    if !ev.prev.is_empty() {
      match prev_line {
        Some(p) if ev.prev != line_hash(p) => return Ok(Some(idx + 1)),
        // First line of the file: genesis for a fresh log, or the hash of a
        // rotated-away predecessor — not checkable from this file alone.
        None => {}
        _ => {}
      }
    }
    prev_line = Some(line);
  }
  Ok(None)
}

#[cfg(test)]
#[path = "audit_tests.rs"]
mod tests;
