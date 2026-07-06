use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use tracing::{error, info};

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
pub struct AuditLog {
  path: PathBuf,
  recent: VecDeque<AuditEvent>,
}

impl AuditLog {
  /// Opens the audit log and pre-loads the most recent events from disk so
  /// the dashboard has history right after a restart.
  pub fn load(data_dir: &str) -> Self {
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
    AuditLog { path, recent }
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
        if let Err(e) = res {
          error!("Failed to append audit event to {:?}: {}", self.path, e);
        }
      }
      Err(e) => error!("Failed to serialize audit event: {}", e),
    }
  }

  /// Recent events, oldest first.
  pub fn recent(&self) -> Vec<AuditEvent> {
    self.recent.iter().cloned().collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_record_and_reload() {
    let dir = std::env::temp_dir().join(format!("aperio-audit-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_string_lossy().to_string();

    let mut log = AuditLog::load(&dir_str);
    log.record("token_created", "1.2.3.4", "name=test");
    log.record("login_success", "1.2.3.4", "user=aperio");
    assert_eq!(log.recent().len(), 2);

    let log2 = AuditLog::load(&dir_str);
    assert_eq!(log2.recent().len(), 2);
    assert_eq!(log2.recent()[0].event, "token_created");
    assert_eq!(log2.recent()[1].actor_ip, "1.2.3.4");

    let _ = std::fs::remove_dir_all(&dir);
  }
}
