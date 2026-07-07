use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use tracing::{error, info, warn};

/// Maximum number of audit events kept in memory for the dashboard.
const AUDIT_RECENT_CAP: usize = 200;

/// A single administrative/security event.
#[derive(Serialize, Deserialize, Clone)]
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
    if let Ok(raw) = std::fs::read_to_string(&path) {
      for line in raw.lines().rev().take(AUDIT_RECENT_CAP) {
        if let Ok(ev) = serde_json::from_str::<AuditEvent>(line) {
          recent.push_front(ev);
        }
      }
    }
    if !recent.is_empty() {
      info!(
        "Loaded {} recent audit events from {:?}",
        recent.len(),
        path
      );
    }
    let current_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    AuditLog {
      path,
      recent,
      max_size,
      max_files,
      current_size,
    }
  }

  /// Records an event: appends a JSON line to the file and to the ring buffer.
  pub fn record(&mut self, event: &str, actor_ip: &str, details: &str) {
    let now = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap_or_default()
      .as_secs();
    let ev = AuditEvent {
      ts: now,
      timestamp: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
      event: event.to_string(),
      actor_ip: actor_ip.to_string(),
      details: details.to_string(),
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

#[cfg(test)]
#[path = "audit_tests.rs"]
mod tests;
