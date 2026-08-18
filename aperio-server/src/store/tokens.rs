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
  /// May clients using this token ask the server to reach a service's target
  /// itself, instead of relaying through the client (`server_side: true`)?
  /// Defaults to false. The permission only decides whether the client may
  /// ask; where the server may connect is the operator's
  /// `server_side_targets:`, and both have to agree.
  #[serde(default)]
  pub allow_server_side: bool,
  /// May this token bind the tunnels of *other* clients in the same
  /// organization? Defaults to false. Without it a binder needs the very
  /// credential the declaring client connected with, which is also the
  /// credential that publishes services as that client, so reaching a
  /// database for ten minutes meant handing over the ability to serve as
  /// them. This is the capability that separates the two.
  #[serde(default)]
  pub allow_bind: bool,
  /// May this token send OpenTelemetry exports through the server's OTel
  /// bridge? Defaults to false, and for the same reason `topics` defaults to
  /// empty: it is a new capability, and one that switches itself on for every
  /// token that predates it is how a permission model quietly stops meaning
  /// anything. Both this and the server's `otel_bridge` have to be on, so an
  /// operator who has not enabled the bridge cannot be surprised by it.
  #[serde(default)]
  pub allow_otel: bool,
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

/// Everything a token is created with.
///
/// A struct rather than a signature, and the history is the argument. `create`
/// grew to fourteen positional parameters, and the comment that used to sit in
/// the middle of it recorded the trade being made: a *new* argument is named
/// at every call site by the compiler, a *shifted* one is not, which is how
/// `canary` once ended up in `allow_bind`. So each capability was appended
/// rather than filed where it belonged, and the permissions ended up scattered
/// across the signature in the order they were invented.
///
/// With `Default`, a field costs nothing at the call sites that do not care
/// about it (`allow_otel` moved forty-odd of them to add one flag), a call
/// reads as the things it actually sets, and the fields can be ordered by what
/// they mean instead of by when they were added. The safety the old comment
/// was protecting is kept and made stronger: fields are matched by name, so
/// neither adding nor reordering one can silently move a value.
#[derive(Debug, Clone, Default)]
pub struct TokenSpec {
  pub name: String,
  /// What it may serve.
  pub hostnames: Vec<String>,
  pub paths: Vec<String>,
  pub topics: Vec<String>,
  /// Who may present it.
  pub allowed_ips: Vec<String>,
  pub org_id: Option<String>,
  /// How long it lives, and how much it may do.
  pub ttl_seconds: Option<u64>,
  pub max_rps: Option<f64>,
  pub daily_max_bytes: Option<u64>,
  pub max_connections: Option<u32>,
  /// What it is allowed to do beyond serving.
  pub allow_public: bool,
  pub allow_server_side: bool,
  pub allow_bind: bool,
  pub allow_otel: bool,
  /// Routed to only by traffic that opted in.
  pub canary: bool,
}

/// The changes an update makes to a token's scope, each `None` meaning
/// "leave it alone".
///
/// The doubled `Option` on the nullable fields is load-bearing and is why this
/// is not simply `Option<TokenSpec>`: `Some(None)` clears a limit, `None`
/// leaves it as it was, and flattening the two would make "no expiry" and "do
/// not touch the expiry" the same request.
#[derive(Debug, Clone, Default)]
pub struct TokenPatch {
  pub name: Option<String>,
  pub hostnames: Option<Vec<String>>,
  pub paths: Option<Vec<String>>,
  pub topics: Option<Vec<String>>,
  pub allowed_ips: Option<Vec<String>>,
  pub ttl_seconds: Option<Option<u64>>,
  pub max_rps: Option<Option<f64>>,
  pub daily_max_bytes: Option<Option<u64>>,
  pub max_connections: Option<Option<u32>>,
  pub allow_public: Option<bool>,
  pub allow_server_side: Option<bool>,
  pub allow_bind: Option<bool>,
  pub allow_otel: Option<bool>,
  pub canary: Option<bool>,
}

pub use crate::store::NotWritten;

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
  /// Bookkeeping: the dump-restore path, whose caller reports on the whole
  /// import rather than on one row. See `store::replace_all`.
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

  /// Runs `change`, saves, and puts the records back when the save failed.
  ///
  /// **Every mutation in this file goes through here**, and the reason is the
  /// one `revoke` was already written by hand for: a change that is in memory
  /// and not on disk is a change that disappears at the next restart, and a
  /// caller told it succeeded has been told something false. On a full disk
  /// that was a token which existed until the process was restarted; for a
  /// revocation it would have been a credential coming back from the dead.
  ///
  /// The snapshot is the whole record list, which is O(n) per mutation, and
  /// that is affordable precisely because these are administrative actions on
  /// a list of tokens rather than anything on the request path.
  fn commit<R>(&mut self, change: impl FnOnce(&mut Self) -> R) -> Result<R, NotWritten> {
    let snapshot = self.tokens.clone();
    let out = change(self);
    if self.persist() {
      Ok(out)
    } else {
      self.tokens = snapshot;
      Err(NotWritten::NotPersisted)
    }
  }

  /// Creates a new token, persists it, and returns the record together with
  /// the plaintext secret. The secret is only available at creation time.
  pub fn create(&mut self, spec: TokenSpec) -> Result<(ApiToken, String), NotWritten> {
    let secret = format!(
      "apr_{}{}",
      uuid::Uuid::new_v4().simple(),
      uuid::Uuid::new_v4().simple()
    );
    let record = ApiToken {
      id: uuid::Uuid::new_v4().to_string(),
      name: spec.name,
      token_hash: hash_token(&secret),
      token_prefix: secret.chars().take(12).collect(),
      hostnames: spec.hostnames,
      paths: spec.paths,
      allowed_ips: spec.allowed_ips,
      created_at: now_secs(),
      expires_at: spec.ttl_seconds.map(|ttl| now_secs().saturating_add(ttl)),
      ttl_seconds: spec.ttl_seconds,
      max_rps: spec.max_rps,
      daily_max_bytes: spec.daily_max_bytes,
      max_connections: spec.max_connections.filter(|v| *v > 0),
      allow_public: spec.allow_public,
      allow_server_side: spec.allow_server_side,
      allow_bind: spec.allow_bind,
      allow_otel: spec.allow_otel,
      topics: spec.topics,
      canary: spec.canary,
      org_id: spec.org_id,
      prev_token_hash: None,
      prev_expires_at: None,
      pinned_key: None,
    };
    self.commit(|store| {
      store.tokens.push(record.clone());
      (record, secret)
    })
  }

  /// Updates a token's scope (permissions/expiry) in place without touching
  /// the secret. Returns the updated record, or None when the ID is unknown.
  pub fn update(&mut self, id: &str, patch: TokenPatch) -> Result<ApiToken, NotWritten> {
    let TokenPatch {
      name,
      hostnames,
      paths,
      allowed_ips,
      ttl_seconds,
      max_rps,
      daily_max_bytes,
      max_connections,
      allow_public,
      allow_server_side,
      allow_bind,
      allow_otel,
      canary,
      topics,
    } = patch;
    if !self.tokens.iter().any(|t| t.id == id) {
      return Err(NotWritten::NoSuchRecord);
    }
    self.commit(|store| {
      let token = store
        .tokens
        .iter_mut()
        .find(|t| t.id == id)
        .expect("checked just above, and nothing else holds the store");
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
      if let Some(p) = allow_server_side {
        token.allow_server_side = p;
      }
      if let Some(b) = allow_bind {
        token.allow_bind = b;
      }
      if let Some(o) = allow_otel {
        token.allow_otel = o;
      }
      if let Some(t) = topics {
        token.topics = t;
      }
      if let Some(c) = canary {
        token.canary = c;
      }
      token.clone()
    })
  }

  /// Removes a token by ID. Returns true when a token was actually removed
  /// *and durably persisted*. On a persist failure the in-memory removal is
  /// reverted so memory matches disk, otherwise a "revoked" token would come
  /// back on the next restart, and `false` is returned so the caller reports
  /// the failure rather than a false success.
  pub fn revoke(&mut self, id: &str) -> Result<(), NotWritten> {
    let Some(pos) = self.tokens.iter().position(|t| t.id == id) else {
      return Err(NotWritten::NoSuchRecord);
    };
    self.commit(|store| {
      store.tokens.remove(pos);
    })
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
  pub fn pin_key(&mut self, id: &str, key: &str) -> Result<PinOutcome, NotWritten> {
    let Some(token) = self.tokens.iter().find(|t| t.id == id) else {
      return Err(NotWritten::NoSuchRecord);
    };
    // Only a *new* pin is a change, so the two answers that read the existing
    // pin never touch the disk and cannot fail on it.
    match token.pinned_key.as_deref() {
      Some(existing) if existing == key => return Ok(PinOutcome::Match),
      Some(_) => return Ok(PinOutcome::Mismatch),
      None => {}
    }
    self.commit(|store| {
      if let Some(token) = store.tokens.iter_mut().find(|t| t.id == id) {
        token.pinned_key = Some(key.to_string());
      }
      PinOutcome::Pinned
    })
  }

  /// Rotates a token's secret in place: a fresh secret becomes current and
  /// the old one stays accepted for `grace_seconds` (0 = immediate cutover).
  /// Permissions, limits and expiry are untouched. Returns the updated
  /// record together with the new plaintext secret.
  pub fn rotate(&mut self, id: &str, grace_seconds: u64) -> Result<(ApiToken, String), NotWritten> {
    if !self.tokens.iter().any(|t| t.id == id) {
      return Err(NotWritten::NoSuchRecord);
    }
    self.commit(|store| {
      let token = store
        .tokens
        .iter_mut()
        .find(|t| t.id == id)
        .expect("checked above");
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
      (token.clone(), secret)
    })
  }

  /// Slides the expiry of the (non-expired) token matching `secret` forward by
  /// its own creation TTL, so a short-lived token stays valid while its holder
  /// keeps using it. Returns the refreshed record. `None` when the secret is
  /// unknown, already expired, or the token has no TTL (nothing to refresh,
  /// it never expires).
  pub fn refresh(&mut self, secret: &str) -> Result<ApiToken, NotWritten> {
    let hash = hash_token(secret);
    let Some(pos) = self
      .tokens
      .iter()
      .position(|t| crate::auth::constant_time_eq_str(&t.token_hash, &hash) && !t.is_expired())
    else {
      return Err(NotWritten::NoSuchRecord);
    };
    // No TTL is "nothing to refresh", not a failure to write one.
    let Some(ttl) = self.tokens[pos].ttl_seconds else {
      return Err(NotWritten::NoSuchRecord);
    };
    self.commit(|store| {
      store.tokens[pos].expires_at = Some(now_secs().saturating_add(ttl));
      store.tokens[pos].clone()
    })
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
