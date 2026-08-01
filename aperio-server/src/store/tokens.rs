use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

/// A dynamic API token created from the dashboard. The secret itself is never
/// stored, only its SHA-256 hash. Permissions restrict which hostname/path
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
  /// Parallel connections a client using this token may open for one service.
  /// `None` = the server's own `max_connections_per_service`. A value above
  /// the server's is not an error, it simply cannot take effect: the
  /// effective ceiling is the lower of the two.
  #[serde(default)]
  pub max_connections: Option<u32>,
  /// May clients using this token publish services as public (skipping the
  /// server's visitor auth gate)? Defaults to false.
  #[serde(default)]
  pub allow_public: bool,
  /// May this token bind the tunnels of *other* clients in the same
  /// organization? Defaults to false. Without it a binder needs the very
  /// credential the declaring client connected with, which is also the
  /// credential that publishes services as that client, so reaching a
  /// database for ten minutes meant handing over the ability to serve as
  /// them. This is the capability that separates the two.
  #[serde(default)]
  pub allow_bind: bool,
  /// Topic filters this token may publish to and subscribe to, for messages
  /// between the clients of its organization. Empty = messaging is not
  /// permitted, `#` = everything the organization can see.
  ///
  /// Note the convention differs from `hostnames`/`paths` above, where empty
  /// means unrestricted. It is deliberate: those fence a capability every
  /// token already had, while this one is new, and a new capability that
  /// switches itself on for every token that predates it is how a permission
  /// model quietly stops meaning anything.
  #[serde(default)]
  pub topics: Vec<String>,
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
  /// connection that announces a different key is rejected, so a leaked token
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
  /// The announced key differs from the existing pin, reject the connection.
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

  /// Rewrites the tokens table. Returns whether the write succeeded.
  fn persist(&mut self) -> bool {
    let rows: Vec<(String, String)> = self
      .tokens
      .iter()
      .filter_map(|t| {
        serde_json::to_string(t)
          .ok()
          .map(|json| (t.id.clone(), json))
      })
      .collect();
    crate::store::replace_all(&mut self.conn, "tokens", &rows)
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
    allow_bind: bool,
    canary: bool,
    org_id: Option<String>,
    // Appended rather than filed beside `hostnames`/`paths` where it belongs
    // semantically: this signature is ten positional arguments long, and
    // inserting one in the middle is how `canary` once ended up in
    // `allow_bind`. The compiler names every call site for an added argument;
    // it cannot see a shifted one.
    topics: Vec<String>,
    max_connections: Option<u32>,
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
      max_connections: max_connections.filter(|v| *v > 0),
      allow_public,
      allow_bind,
      topics,
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
    allow_bind: Option<bool>,
    canary: Option<bool>,
    topics: Option<Vec<String>>,
    max_connections: Option<Option<u32>>,
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
    if let Some(conns) = max_connections {
      token.max_connections = conns.filter(|v| *v > 0);
    }
    if let Some(p) = allow_public {
      token.allow_public = p;
    }
    if let Some(b) = allow_bind {
      token.allow_bind = b;
    }
    if let Some(t) = topics {
      token.topics = t;
    }
    if let Some(c) = canary {
      token.canary = c;
    }
    let updated = token.clone();
    self.persist();
    Some(updated)
  }

  /// Removes a token by ID. Returns true when a token was actually removed
  /// *and durably persisted*. On a persist failure the in-memory removal is
  /// reverted so memory matches disk, otherwise a "revoked" token would come
  /// back on the next restart, and `false` is returned so the caller reports
  /// the failure rather than a false success.
  pub fn revoke(&mut self, id: &str) -> bool {
    let Some(pos) = self.tokens.iter().position(|t| t.id == id) else {
      return false;
    };
    let removed = self.tokens.remove(pos);
    if self.persist() {
      true
    } else {
      self.tokens.insert(pos, removed);
      false
    }
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
  /// unknown, already expired, or the token has no TTL (nothing to refresh,
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
#[path = "tokens_tests.rs"]
mod tests;
