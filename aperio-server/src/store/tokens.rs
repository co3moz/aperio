use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

/// A dynamic API token created from the dashboard. The secret itself is never
/// stored — only its SHA-256 hash. Permissions restrict which hostname/path
/// binds a client authenticated with this token may claim.
#[derive(Serialize, Deserialize, Clone, utoipa::ToSchema)]
pub struct ApiToken {
  /// Unique token record ID (UUID).
  pub id: String,
  /// Human-readable label chosen at creation time.
  pub name: String,
  /// Hex-encoded SHA-256 hash of the token secret.
  pub token_hash: String,
  /// First characters of the secret, kept for display purposes only.
  pub token_prefix: String,
  /// Hostnames this token may bind to. `["*"]` or empty = unrestricted.
  /// Specific entries are auto-bound to the client on connect.
  pub hostnames: Vec<String>,
  /// Path binds this token may claim. `["*"]` or empty = unrestricted.
  pub paths: Vec<String>,
  /// Client source IPs (plain or CIDR) allowed to connect with this token.
  /// Empty or containing "0.0.0.0/0" (or "*") = any IP.
  #[serde(default)]
  pub allowed_ips: Vec<String>,
  /// Unix timestamp (seconds) of creation.
  pub created_at: u64,
  /// Optional unix timestamp (seconds) after which the token is rejected.
  pub expires_at: Option<u64>,
  /// The lifetime (seconds) the token was created/updated with, remembered so
  /// a refresh can reset the expiry to the same window. `None` = never expires.
  #[serde(default)]
  pub ttl_seconds: Option<u64>,
  /// Optional request rate limit (requests/second, token bucket) applied to
  /// traffic served by clients authenticated with this token.
  #[serde(default)]
  pub max_rps: Option<f64>,
  /// Optional daily byte quota (request + response payload) for traffic
  /// served by clients authenticated with this token.
  #[serde(default)]
  pub daily_max_bytes: Option<u64>,
  /// May clients using this token publish services as public (skipping the
  /// server's visitor auth gate)? Defaults to false.
  #[serde(default)]
  pub allow_public: bool,
}

impl ApiToken {
  /// Returns true when the token is past its expiry time.
  pub fn is_expired(&self) -> bool {
    self.expires_at.is_some_and(|exp| now_secs() >= exp)
  }
}

/// Persistent store for dynamic API tokens, backed by the `tokens` table of
/// the shared SQLite store (`<data_dir>/aperio.db`).
pub struct TokenStore {
  conn: rusqlite::Connection,
  tokens: Vec<ApiToken>,
}

impl TokenStore {
  /// Opens the shared store and loads all token records.
  pub fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let tokens: Vec<ApiToken> = crate::store::load_all(&conn, "tokens");
    if !tokens.is_empty() {
      info!(
        "Loaded {} dynamic API token(s) from the store",
        tokens.len()
      );
    }
    TokenStore { conn, tokens }
  }

  /// Writes the current token list back to the store (one transaction).
  fn persist(&mut self) {
    let rows: Vec<(String, String)> = self
      .tokens
      .iter()
      .filter_map(|t| {
        serde_json::to_string(t)
          .ok()
          .map(|json| (t.id.clone(), json))
      })
      .collect();
    crate::store::replace_all(&mut self.conn, "tokens", &rows);
  }

  /// Creates a new token, persists it, and returns the record together with
  /// the plaintext secret. The secret is only available at creation time.
  #[allow(clippy::too_many_arguments)]
  pub fn create(
    &mut self,
    name: String,
    hostnames: Vec<String>,
    paths: Vec<String>,
    allowed_ips: Vec<String>,
    ttl_seconds: Option<u64>,
    max_rps: Option<f64>,
    daily_max_bytes: Option<u64>,
    allow_public: bool,
  ) -> (ApiToken, String) {
    let secret = format!(
      "apr_{}{}",
      uuid::Uuid::new_v4().simple(),
      uuid::Uuid::new_v4().simple()
    );
    let record = ApiToken {
      id: uuid::Uuid::new_v4().to_string(),
      name,
      token_hash: hash_token(&secret),
      token_prefix: secret.chars().take(12).collect(),
      hostnames,
      paths,
      allowed_ips,
      created_at: now_secs(),
      expires_at: ttl_seconds.map(|ttl| now_secs().saturating_add(ttl)),
      ttl_seconds,
      max_rps,
      daily_max_bytes,
      allow_public,
    };
    self.tokens.push(record.clone());
    self.persist();
    (record, secret)
  }

  /// Updates a token's scope (permissions/expiry) in place without touching
  /// the secret. Returns the updated record, or None when the ID is unknown.
  #[allow(clippy::too_many_arguments)]
  pub fn update(
    &mut self,
    id: &str,
    name: Option<String>,
    hostnames: Option<Vec<String>>,
    paths: Option<Vec<String>>,
    allowed_ips: Option<Vec<String>>,
    ttl_seconds: Option<Option<u64>>,
    max_rps: Option<Option<f64>>,
    daily_max_bytes: Option<Option<u64>>,
    allow_public: Option<bool>,
  ) -> Option<ApiToken> {
    let token = self.tokens.iter_mut().find(|t| t.id == id)?;
    if let Some(n) = name {
      token.name = n;
    }
    if let Some(h) = hostnames {
      token.hostnames = h;
    }
    if let Some(p) = paths {
      token.paths = p;
    }
    if let Some(ips) = allowed_ips {
      token.allowed_ips = ips;
    }
    if let Some(ttl) = ttl_seconds {
      token.expires_at = ttl.map(|t| now_secs().saturating_add(t));
      token.ttl_seconds = ttl;
    }
    if let Some(rps) = max_rps {
      token.max_rps = rps.filter(|v| *v > 0.0);
    }
    if let Some(quota) = daily_max_bytes {
      token.daily_max_bytes = quota.filter(|v| *v > 0);
    }
    if let Some(p) = allow_public {
      token.allow_public = p;
    }
    let updated = token.clone();
    self.persist();
    Some(updated)
  }

  /// Removes a token by ID. Returns true when a token was actually removed.
  pub fn revoke(&mut self, id: &str) -> bool {
    let before = self.tokens.len();
    self.tokens.retain(|t| t.id != id);
    let removed = self.tokens.len() != before;
    if removed {
      self.persist();
    }
    removed
  }

  /// Returns all token records (hashes included; strip before exposing).
  pub fn list(&self) -> &[ApiToken] {
    &self.tokens
  }

  /// Verifies a presented secret against the store. Returns the matching
  /// non-expired token record, if any. The stored/derived hashes are compared
  /// in constant time; comparing SHA-256 hashes (not the secret) is already low
  /// risk, but this keeps the comparison consistent with the master-token path
  /// and avoids a future timing regression.
  pub fn verify(&self, secret: &str) -> Option<&ApiToken> {
    let hash = hash_token(secret);
    self
      .tokens
      .iter()
      .find(|t| crate::auth::constant_time_eq_str(&t.token_hash, &hash) && !t.is_expired())
  }

  /// Slides the expiry of the (non-expired) token matching `secret` forward by
  /// its own creation TTL, so a short-lived token stays valid while its holder
  /// keeps using it. Returns the refreshed record. `None` when the secret is
  /// unknown, already expired, or the token has no TTL (nothing to refresh —
  /// it never expires).
  pub fn refresh(&mut self, secret: &str) -> Option<ApiToken> {
    let hash = hash_token(secret);
    let token = self
      .tokens
      .iter_mut()
      .find(|t| crate::auth::constant_time_eq_str(&t.token_hash, &hash) && !t.is_expired())?;
    let ttl = token.ttl_seconds?;
    token.expires_at = Some(now_secs().saturating_add(ttl));
    let refreshed = token.clone();
    self.persist();
    Some(refreshed)
  }
}

/// Hex-encoded SHA-256 of a token secret.
pub fn hash_token(secret: &str) -> String {
  let mut hasher = Sha256::default();
  hasher.update(secret.as_bytes());
  hasher
    .finalize()
    .iter()
    .map(|b| format!("{:02x}", b))
    .collect()
}

/// Current unix time in seconds.
pub fn now_secs() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temp_dir() -> String {
    let dir = std::env::temp_dir().join(format!("aperio-tokens-test-{}", uuid::Uuid::new_v4()));
    dir.to_string_lossy().to_string()
  }

  #[test]
  fn test_create_verify_revoke_persist() {
    let dir = temp_dir();
    let mut store = TokenStore::load(&dir);
    assert!(store.list().is_empty());

    let (record, secret) = store.create(
      "ci-token".to_string(),
      vec!["a.example.com".to_string()],
      vec!["*".to_string()],
      vec![],
      None,
      None,
      None,
      false,
    );
    assert!(secret.starts_with("apr_"));
    assert_eq!(store.verify(&secret).unwrap().id, record.id);
    assert!(store.verify("apr_wrong").is_none());

    // Reload from disk → token persisted
    let store2 = TokenStore::load(&dir);
    assert_eq!(store2.list().len(), 1);
    assert_eq!(store2.verify(&secret).unwrap().name, "ci-token");

    // Revoke
    let mut store3 = TokenStore::load(&dir);
    assert!(store3.revoke(&record.id));
    assert!(store3.verify(&secret).is_none());
    let store4 = TokenStore::load(&dir);
    assert!(store4.list().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn test_corrupt_db_is_backed_up_not_discarded() {
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let path = std::path::PathBuf::from(&dir).join("aperio.db");
    std::fs::write(&path, "this is not a sqlite database at all").unwrap();

    // Loading a corrupt store starts empty but preserves the bad file.
    let store = TokenStore::load(&dir);
    assert!(store.list().is_empty());

    // The original file was renamed aside as aperio.db.corrupt.<epoch>.
    let backups: Vec<_> = std::fs::read_dir(&dir)
      .unwrap()
      .filter_map(|e| e.ok())
      .filter(|e| {
        e.file_name()
          .to_string_lossy()
          .starts_with("aperio.db.corrupt.")
      })
      .collect();
    assert_eq!(backups.len(), 1, "corrupt file should be preserved");

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn test_refresh_slides_expiry_by_creation_ttl() {
    let dir = temp_dir();
    let mut store = TokenStore::load(&dir);
    let (record, secret) = store.create(
      "ci".to_string(),
      vec![],
      vec![],
      vec![],
      Some(3600),
      None,
      None,
      false,
    );
    let first_expiry = record.expires_at.unwrap();

    // Refresh answers with a new expiry >= the original (now + same TTL).
    let refreshed = store.refresh(&secret).expect("refresh should succeed");
    assert!(refreshed.expires_at.unwrap() >= first_expiry);
    assert_eq!(refreshed.ttl_seconds, Some(3600));

    // A wrong secret refreshes nothing.
    assert!(store.refresh("apr_wrong").is_none());

    // A never-expiring token has nothing to refresh.
    let (_, forever) = store.create(
      "forever".to_string(),
      vec![],
      vec![],
      vec![],
      None,
      None,
      None,
      false,
    );
    assert!(store.refresh(&forever).is_none());

    // An already-expired token cannot resurrect itself.
    let (_, dead) = store.create(
      "dead".to_string(),
      vec![],
      vec![],
      vec![],
      Some(0),
      None,
      None,
      false,
    );
    assert!(store.refresh(&dead).is_none());

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn test_expired_token_rejected() {
    let dir = temp_dir();
    let mut store = TokenStore::load(&dir);
    let (_, secret) = store.create(
      "short".to_string(),
      vec![],
      vec![],
      vec![],
      Some(0),
      None,
      None,
      false,
    );
    // ttl 0 → expires_at == now → already expired
    assert!(store.verify(&secret).is_none());
    let _ = std::fs::remove_dir_all(&dir);
  }
}
