use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};

/// A dynamic API token created from the dashboard. The secret itself is never
/// stored — only its SHA-256 hash. Permissions restrict which hostname/path
/// binds a client authenticated with this token may claim.
#[derive(Serialize, Deserialize, Clone)]
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
  /// Optional request rate limit (requests/second, token bucket) applied to
  /// traffic served by clients authenticated with this token.
  #[serde(default)]
  pub max_rps: Option<f64>,
  /// Optional daily byte quota (request + response payload) for traffic
  /// served by clients authenticated with this token.
  #[serde(default)]
  pub daily_max_bytes: Option<u64>,
}

impl ApiToken {
  /// Returns true when the token is past its expiry time.
  pub fn is_expired(&self) -> bool {
    self.expires_at.is_some_and(|exp| now_secs() >= exp)
  }
}

#[derive(Serialize, Deserialize, Default)]
struct TokenFile {
  tokens: Vec<ApiToken>,
}

/// Persistent store for dynamic API tokens, backed by a JSON file inside the
/// data directory (`APERIO_DATA_DIR`, default `./data`).
pub struct TokenStore {
  path: PathBuf,
  tokens: Vec<ApiToken>,
}

impl TokenStore {
  /// Loads the token store from `<data_dir>/tokens.json`, creating the data
  /// directory when missing. A corrupt file is treated as empty (with a log).
  pub fn load(data_dir: &str) -> Self {
    let dir = PathBuf::from(data_dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
      warn!("Could not create data directory {:?}: {}", dir, e);
    }
    let path = dir.join("tokens.json");
    let tokens = match std::fs::read_to_string(&path) {
      Ok(raw) => match serde_json::from_str::<TokenFile>(&raw) {
        Ok(file) => file.tokens,
        Err(e) => {
          error!(
            "Failed to parse {:?}: {} — starting with empty token store",
            path, e
          );
          Vec::new()
        }
      },
      Err(_) => Vec::new(),
    };
    if !tokens.is_empty() {
      info!(
        "Loaded {} dynamic API token(s) from {:?}",
        tokens.len(),
        path
      );
    }
    TokenStore { path, tokens }
  }

  /// Writes the current token list back to disk.
  fn persist(&self) {
    let file = TokenFile {
      tokens: self.tokens.clone(),
    };
    match serde_json::to_string_pretty(&file) {
      Ok(json) => {
        if let Err(e) = std::fs::write(&self.path, json) {
          error!("Failed to persist token store to {:?}: {}", self.path, e);
        }
      }
      Err(e) => error!("Failed to serialize token store: {}", e),
    }
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
      max_rps,
      daily_max_bytes,
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
    }
    if let Some(rps) = max_rps {
      token.max_rps = rps.filter(|v| *v > 0.0);
    }
    if let Some(quota) = daily_max_bytes {
      token.daily_max_bytes = quota.filter(|v| *v > 0);
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
  /// non-expired token record, if any.
  pub fn verify(&self, secret: &str) -> Option<&ApiToken> {
    let hash = hash_token(secret);
    self
      .tokens
      .iter()
      .find(|t| t.token_hash == hash && !t.is_expired())
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
    );
    // ttl 0 → expires_at == now → already expired
    assert!(store.verify(&secret).is_none());
    let _ = std::fs::remove_dir_all(&dir);
  }
}
