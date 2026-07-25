//! Autoscaling records: the endpoint the server calls when a bind needs
//! capacity it does not have.
//!
//! A record is keyed by the bind it protects (`org|hostname|path`), because
//! that is all a visitor request carries. It deliberately **outlives the
//! client that declared it**: the whole point of `min: 0` is that the server
//! can call the endpoint when nothing is running.
//!
//! Ownership is a *set* of token ids rather than a client id. Eight identical
//! replicas may each hold their own token and each announce the same block on
//! connect; the content hash makes those announcements an idempotent no-op,
//! and the record only disarms once the last owning token is gone.

use serde::{Deserialize, Serialize};
use tracing::info;

/// Default cold-start budget: how long a visitor request may be held while
/// the service starts.
pub const DEFAULT_COLD_START_SECS: u64 = 45;
/// Default pool utilization above which one more instance is requested.
pub const DEFAULT_TARGET_UTILIZATION: f64 = 0.8;
/// Default time utilization must stay above the target before acting.
pub const DEFAULT_WINDOW_SECS: u64 = 15;
/// Default minimum gap between two calls for one bind.
pub const DEFAULT_COOLDOWN_SECS: u64 = 60;
/// Longest cold-start budget accepted; beyond this a visitor is waiting on a
/// deployment, not a cold start.
pub const MAX_COLD_START_SECS: u64 = 300;

/// One armed autoscaling record.
#[derive(Serialize, Deserialize, Clone, Debug, utoipa::ToSchema)]
pub struct ScalingRecord {
  /// `org|hostname|path`, derived from the bind so an upsert is idempotent.
  pub id: String,
  /// Organization the bind belongs to (None = master).
  pub org_id: Option<String>,
  pub hostname: String,
  /// Path bind, when the service claimed one.
  pub path: Option<String>,
  /// Endpoint the server POSTs to. Never rendered to a browser without the
  /// caller being an admin of the owning org.
  pub url: String,
  /// Bearer credential for the outgoing call. Write-only: the API never
  /// returns it and it is never logged.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub secret: Option<String>,
  pub min: u32,
  pub max: u32,
  pub cold_start_secs: u64,
  pub target_utilization: f64,
  pub window_secs: u64,
  pub cooldown_secs: u64,
  /// Token ids that declared this record; it disarms when the set empties.
  #[serde(default)]
  pub owners: Vec<String>,
  /// Hash of the behavioral fields, so a re-announcement of the same config
  /// is recognized as a no-op instead of a change.
  pub config_hash: String,
  pub created_at: u64,
  /// Last time a client re-announced this record; drives TTL pruning.
  pub last_seen: u64,
}

impl ScalingRecord {
  /// Stable key for a bind.
  pub fn key(org_id: Option<&str>, hostname: &str, path: Option<&str>) -> String {
    format!(
      "{}|{}|{}",
      org_id.unwrap_or("master"),
      hostname,
      path.unwrap_or("")
    )
  }

  /// Hash over the fields that change behavior. The owner set, timestamps and
  /// the id are excluded on purpose: N replicas announcing the same block must
  /// hash identically.
  pub fn compute_hash(&self) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(self.url.as_bytes());
    hasher.update([0]);
    hasher.update(self.secret.as_deref().unwrap_or("").as_bytes());
    hasher.update(
      format!(
        "|{}|{}|{}|{}|{}|{}",
        self.min,
        self.max,
        self.cold_start_secs,
        self.target_utilization,
        self.window_secs,
        self.cooldown_secs
      )
      .as_bytes(),
    );
    hasher
      .finalize()
      .iter()
      .take(8)
      .map(|b| format!("{b:02x}"))
      .collect()
  }

  /// True when this record opts the bind into scale-to-zero, i.e. a request
  /// for it may trigger a cold start rather than a 504.
  pub fn cold_start_enabled(&self) -> bool {
    self.min == 0 && self.cold_start_secs > 0
  }
}

/// What an upsert did, so the caller can decide whether to audit it. A
/// re-announcement of an unchanged config is `Unchanged` and stays silent,
/// which is what keeps a fleet of identical replicas from spamming the log.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum Upsert {
  Created,
  Updated,
  Unchanged,
}

/// Persistent store of autoscaling records, backed by the `scaling` table of
/// the shared SQLite store.
pub struct ScalingStore {
  conn: rusqlite::Connection,
  records: Vec<ScalingRecord>,
}

impl ScalingStore {
  pub fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let records: Vec<ScalingRecord> = crate::store::load_all(&conn, "scaling");
    if !records.is_empty() {
      info!(
        "Loaded {} autoscaling record(s) from the store",
        records.len()
      );
    }
    ScalingStore { conn, records }
  }

  fn persist(&mut self) {
    let rows: Vec<(String, String)> = self
      .records
      .iter()
      .filter_map(|r| serde_json::to_string(r).ok().map(|j| (r.id.clone(), j)))
      .collect();
    crate::store::replace_all(&mut self.conn, "scaling", &rows);
  }

  pub fn list(&self) -> &[ScalingRecord] {
    &self.records
  }

  /// The record armed for a bind, if any. The request path scans by hostname
  /// instead (a visitor request carries no organization), so this is the
  /// exact-key lookup used by tests and by callers that already know the org.
  #[cfg_attr(not(test), allow(dead_code))]
  pub fn find(
    &self,
    org_id: Option<&str>,
    hostname: &str,
    path: Option<&str>,
  ) -> Option<&ScalingRecord> {
    let key = ScalingRecord::key(org_id, hostname, path);
    self.records.iter().find(|r| r.id == key)
  }

  /// Arms or refreshes a record. An identical config only bumps `last_seen`
  /// and records the owner, so a fleet of replicas announcing the same block
  /// converges on one record without flapping.
  pub fn upsert(&mut self, mut record: ScalingRecord, owner: Option<&str>, now: u64) -> Upsert {
    record.config_hash = record.compute_hash();
    let outcome = match self.records.iter_mut().find(|r| r.id == record.id) {
      Some(existing) => {
        let changed = existing.config_hash != record.config_hash;
        if changed {
          let owners = std::mem::take(&mut existing.owners);
          let created_at = existing.created_at;
          *existing = record;
          existing.owners = owners;
          existing.created_at = created_at;
        }
        existing.last_seen = now;
        if let Some(owner) = owner
          && !existing.owners.iter().any(|o| o == owner)
        {
          existing.owners.push(owner.to_string());
        }
        if changed {
          Upsert::Updated
        } else {
          Upsert::Unchanged
        }
      }
      None => {
        record.created_at = now;
        record.last_seen = now;
        if let Some(owner) = owner {
          record.owners = vec![owner.to_string()];
        }
        self.records.push(record);
        Upsert::Created
      }
    };
    // An unchanged refresh still moves `last_seen`, which the TTL sweep reads,
    // so it has to be persisted like any other update.
    self.persist();
    outcome
  }

  /// Removes a record outright.
  pub fn delete(&mut self, id: &str) -> bool {
    let before = self.records.len();
    self.records.retain(|r| r.id != id);
    let removed = self.records.len() != before;
    if removed {
      self.persist();
    }
    removed
  }

  /// Drops an owner from every record and removes the records left with no
  /// owner at all. Called when a token is revoked or expires, so a record can
  /// never outlive the credential that armed it.
  pub fn disown(&mut self, token_id: &str) -> usize {
    let before = self.records.len();
    // A record armed through the admin API has no owners at all and must
    // survive; only one that *had* owners and just lost its last is dropped.
    self.records.retain_mut(|record| {
      let had_owners = !record.owners.is_empty();
      record.owners.retain(|o| o != token_id);
      !had_owners || !record.owners.is_empty()
    });
    let removed = before - self.records.len();
    if removed > 0 {
      self.persist();
    }
    removed
  }

  /// Drops records nothing has re-announced within `ttl_secs`.
  pub fn prune(&mut self, ttl_secs: u64, now: u64) -> usize {
    let before = self.records.len();
    self
      .records
      .retain(|r| now.saturating_sub(r.last_seen) < ttl_secs);
    let removed = before - self.records.len();
    if removed > 0 {
      self.persist();
    }
    removed
  }

  /// Replaces every record (dump import) and persists.
  pub fn import(&mut self, records: Vec<ScalingRecord>) -> usize {
    self.records = records;
    self.persist();
    self.records.len()
  }
}

#[cfg(test)]
#[path = "scaling_tests.rs"]
mod tests;
