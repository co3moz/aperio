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
  /// Marks this token as a canary/decoy: it is never meant to be used, so any
  /// successful authentication with it is a strong breach signal. Presenting a
  /// canary token emits a `canary_tripped` webhook + audit event.
  #[serde(default)]
  pub canary: bool,
  /// Organization this token belongs to; `None` = the master organization.
  /// A client that connects with this token inherits its organization.
  #[serde(default)]
  pub org_id: Option<String>,
  /// SHA-256 hash of the previous secret after a rotation. Stays accepted
  /// until `prev_expires_at` so existing clients can migrate gracefully.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub prev_token_hash: Option<String>,
  /// Unix timestamp (seconds) when the rotated-out previous secret stops
  /// being accepted (the rotation's grace deadline).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub prev_expires_at: Option<u64>,
  /// Trust-on-first-use device pin: the first client device key seen for this
  /// token (announced in the Ping). When token pinning is enabled, a later
  /// connection that announces a different key is rejected — so a leaked token
  /// replayed from another machine cannot serve. Cleared on rotation.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub pinned_key: Option<String>,
}

/// Result of pinning a client device key to a token.
#[derive(Debug, PartialEq, Eq)]
pub enum PinOutcome {
  /// The token had no pin; this key is now pinned.
  Pinned,
  /// The announced key matches the existing pin.
  Match,
  /// The announced key differs from the existing pin — reject the connection.
  Mismatch,
}

impl ApiToken {
  /// Returns true when the token is past its expiry time.
  pub fn is_expired(&self) -> bool {
    self.expires_at.is_some_and(|exp| now_secs() >= exp)
  }

  /// True while the rotated-out previous secret is still inside its grace
  /// window (false when the token was never rotated).
  pub fn prev_secret_valid(&self) -> bool {
    self.prev_token_hash.is_some() && self.prev_expires_at.is_some_and(|exp| now_secs() < exp)
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
  /// Replaces every token record with the given list (dump import) and
  /// persists. Returns how many records are now stored.
  pub fn import(&mut self, tokens: Vec<ApiToken>) -> usize {
    self.tokens = tokens;
    self.persist();
    self.tokens.len()
  }

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
    canary: bool,
    org_id: Option<String>,
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
      canary,
      org_id,
      prev_token_hash: None,
      prev_expires_at: None,
      pinned_key: None,
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
    canary: Option<bool>,
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
    if let Some(c) = canary {
      token.canary = c;
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
    self.tokens.iter().find(|t| {
      if t.is_expired() {
        return false;
      }
      // The current secret always matches; after a rotation the previous
      // secret keeps matching until its grace window closes.
      crate::auth::constant_time_eq_str(&t.token_hash, &hash)
        || (t.prev_secret_valid()
          && t
            .prev_token_hash
            .as_deref()
            .is_some_and(|prev| crate::auth::constant_time_eq_str(prev, &hash)))
    })
  }

  /// Trust-on-first-use pin: records `key` as the token's device pin when it
  /// has none (persisting), reports a match when it equals the existing pin,
  /// or a mismatch otherwise. Returns None for an unknown token id.
  pub fn pin_key(&mut self, id: &str, key: &str) -> Option<PinOutcome> {
    let token = self.tokens.iter_mut().find(|t| t.id == id)?;
    let outcome = match token.pinned_key.as_deref() {
      None => {
        token.pinned_key = Some(key.to_string());
        PinOutcome::Pinned
      }
      Some(existing) if existing == key => PinOutcome::Match,
      Some(_) => PinOutcome::Mismatch,
    };
    if outcome == PinOutcome::Pinned {
      self.persist();
    }
    Some(outcome)
  }

  /// Rotates a token's secret in place: a fresh secret becomes current and
  /// the old one stays accepted for `grace_seconds` (0 = immediate cutover).
  /// Permissions, limits and expiry are untouched. Returns the updated
  /// record together with the new plaintext secret.
  pub fn rotate(&mut self, id: &str, grace_seconds: u64) -> Option<(ApiToken, String)> {
    let token = self.tokens.iter_mut().find(|t| t.id == id)?;
    let secret = format!(
      "apr_{}{}",
      uuid::Uuid::new_v4().simple(),
      uuid::Uuid::new_v4().simple()
    );
    if grace_seconds > 0 {
      token.prev_token_hash = Some(token.token_hash.clone());
      token.prev_expires_at = Some(now_secs().saturating_add(grace_seconds));
    } else {
      token.prev_token_hash = None;
      token.prev_expires_at = None;
    }
    token.token_hash = hash_token(&secret);
    token.token_prefix = secret.chars().take(12).collect();
    // A rotated secret is a fresh trust anchor: drop the device pin so the
    // next connecting client re-pins (e.g. after moving the token to a new box).
    token.pinned_key = None;
    let rotated = token.clone();
    self.persist();
    Some((rotated, secret))
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
      false,
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
      false,
      None,
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
      false,
      None,
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
      false,
      None,
    );
    assert!(store.refresh(&dead).is_none());

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn test_rotate_with_grace_period() {
    let dir = temp_dir();
    let mut store = TokenStore::load(&dir);
    let (record, old_secret) = store.create(
      "rotate-me".to_string(),
      vec![],
      vec![],
      vec![],
      None,
      None,
      None,
      false,
      false,
      None,
    );

    // Rotation with a grace window: both secrets verify to the same record.
    let (rotated, new_secret) = store.rotate(&record.id, 3600).expect("rotate");
    assert_ne!(new_secret, old_secret);
    assert!(rotated.prev_expires_at.is_some());
    assert_eq!(store.verify(&new_secret).unwrap().id, record.id);
    assert_eq!(store.verify(&old_secret).unwrap().id, record.id);

    // The rotation survives a reload.
    let store2 = TokenStore::load(&dir);
    assert!(store2.verify(&old_secret).is_some());
    assert!(store2.verify(&new_secret).is_some());

    // A second rotation with grace 0 cuts the old secrets off immediately.
    let mut store3 = TokenStore::load(&dir);
    let (_, newest) = store3.rotate(&record.id, 0).expect("rotate");
    assert!(store3.verify(&newest).is_some());
    assert!(store3.verify(&new_secret).is_none());
    assert!(store3.verify(&old_secret).is_none());

    // Unknown ids rotate nothing.
    assert!(store3.rotate("nope", 60).is_none());

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn test_canary_flag_create_update_persist() {
    let dir = temp_dir();
    let mut store = TokenStore::load(&dir);
    let (record, _secret) = store.create(
      "decoy".to_string(),
      vec![],
      vec![],
      vec![],
      None,
      None,
      None,
      false,
      true,
      None,
    );
    assert!(record.canary);

    // Survives reload.
    let store2 = TokenStore::load(&dir);
    assert!(store2.list()[0].canary);

    // Can be toggled off in place.
    let mut store3 = TokenStore::load(&dir);
    let updated = store3
      .update(
        &record.id,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(false),
      )
      .unwrap();
    assert!(!updated.canary);

    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn test_pin_key_tofu_and_clear_on_rotate() {
    let dir = temp_dir();
    let mut store = TokenStore::load(&dir);
    let (record, _secret) = store.create(
      "pinned".to_string(),
      vec![],
      vec![],
      vec![],
      None,
      None,
      None,
      false,
      false,
      None,
    );

    // First key pins; the same key matches; a different key is a mismatch.
    assert_eq!(store.pin_key(&record.id, "devA"), Some(PinOutcome::Pinned));
    assert_eq!(store.pin_key(&record.id, "devA"), Some(PinOutcome::Match));
    assert_eq!(
      store.pin_key(&record.id, "devB"),
      Some(PinOutcome::Mismatch)
    );
    // The pin survives a reload.
    let store2 = TokenStore::load(&dir);
    assert_eq!(store2.list()[0].pinned_key.as_deref(), Some("devA"));

    // Rotating the secret clears the pin so a new device can re-pin.
    let mut store3 = TokenStore::load(&dir);
    store3.rotate(&record.id, 0).unwrap();
    assert!(store3.list()[0].pinned_key.is_none());
    assert_eq!(store3.pin_key(&record.id, "devB"), Some(PinOutcome::Pinned));

    // Unknown ids pin nothing.
    assert_eq!(store3.pin_key("nope", "x"), None);

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
      false,
      None,
    );
    // ttl 0 → expires_at == now → already expired
    assert!(store.verify(&secret).is_none());
    let _ = std::fs::remove_dir_all(&dir);
  }
}
