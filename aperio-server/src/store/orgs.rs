//! Organizations (multi-tenancy). Each organization groups the users and
//! tokens created under it; a connected client belongs to the organization of
//! the token it authenticated with.
//!
//! The **master** organization is implicit and is *not* a row here: it is
//! represented by `org_id: None` on users and tokens. The built-in `aperio`
//! admin, the master token, and the dashboard password all act within master
//! and can switch into any child organization. Only the child organizations
//! created through master are stored in this table.

use serde::{Deserialize, Serialize};
use tracing::info;

/// The reserved id the API uses to refer to the implicit master organization
/// (which has no row of its own). Never a valid child-org id (child ids are
/// UUIDs).
pub(crate) const MASTER_ID: &str = "master";

/// One child organization.
#[derive(Serialize, Deserialize, Clone, utoipa::ToSchema)]
pub struct Organization {
  /// Unique id (UUID).
  pub id: String,
  /// Handle: the identifier the organization is addressed by, in
  /// `payments@postgres`, in an `expose:` rule and in the API. Fixed at
  /// creation, because everything that names it would otherwise be pointing
  /// at nothing.
  pub name: String,
  /// What to call it on screen. Free text, any language, and editable at any
  /// time precisely because nothing addresses it.
  #[serde(default)]
  pub custom_name: Option<String>,
  /// Unix seconds of creation.
  pub created_at: u64,
  /// Max concurrently-connected clients in this org (None = unlimited).
  #[serde(default)]
  pub max_clients: Option<u64>,
  /// Max dynamic tokens in this org (None = unlimited).
  #[serde(default)]
  pub max_tokens: Option<u64>,
  /// Max dashboard users in this org (None = unlimited).
  #[serde(default)]
  pub max_users: Option<u64>,
  /// Max proxied bytes (in + out) this org may serve per calendar month
  /// (None = unlimited). Enforced against the month's per-org stats bucket.
  #[serde(default)]
  pub max_bytes_month: Option<u64>,
  /// Hostname patterns this organization may claim (empty = unrestricted).
  /// Every hostname bind created inside the org, whether as a token
  /// permission or declared by a connecting client, must match one of these,
  /// so a tenant can never claim a hostname it does not own. Entries are
  /// either an exact hostname (`acme.com`) or a subdomain wildcard
  /// (`*.acme.com`).
  #[serde(default)]
  pub hostnames: Vec<String>,
  /// Per-organization OIDC SSO override. When set, `/aperio/oidc/login?org=<id>`
  /// authenticates against this issuer and binds the session to the org.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub oidc: Option<OrgOidc>,
}

/// Normalizes one entry of an organization's hostname allowlist: an exact
/// hostname (`acme.com`) or a subdomain wildcard (`*.acme.com`), lowercased
/// and without a trailing dot or port. `*` on its own means "unrestricted"
/// and normalizes to `*`. Returns None for anything else, including a
/// wildcard anywhere but the leading label.
pub fn normalize_org_hostname_pattern(raw: &str) -> Option<String> {
  let trimmed = raw.trim().trim_end_matches('.').to_ascii_lowercase();
  if trimmed.is_empty() {
    return None;
  }
  if trimmed == "*" {
    return Some("*".to_string());
  }
  let (wildcard, host) = match trimmed.strip_prefix("*.") {
    Some(rest) => (true, rest.to_string()),
    None => (false, trimmed),
  };
  // The remainder must be a plain hostname: reuse the bind normalizer so the
  // allowlist can never hold something a bind could not match anyway.
  let normalized = crate::routing::normalize_hostname_bind(&host)?;
  // A wildcard needs something to be a subdomain *of*, so a bare TLD is out.
  if wildcard && !normalized.contains('.') {
    return None;
  }
  Some(if wildcard {
    format!("*.{normalized}")
  } else {
    normalized
  })
}

/// True when one allowlist pattern (`*`, `*.acme.com`, or an exact hostname)
/// covers `host`.
///
/// Allocation-free on purpose: this runs per request on the proxy's hot path
/// once any maintenance flag exists, and the obvious spelling cost a
/// lowercased copy of the hostname plus a `format!` per pattern, per request.
/// Compared as bytes rather than as `str` so a Host header that is not UTF-8
/// where the suffix begins cannot panic on a slice boundary.
pub fn pattern_matches_host(pattern: &str, host: &str) -> bool {
  if pattern == "*" {
    return true;
  }
  let host = host.trim_end_matches('.').as_bytes();
  match pattern.strip_prefix("*.") {
    Some(suffix) => {
      let (n, m) = (host.len(), suffix.len());
      // A label of at least one character, then a dot, then the suffix.
      n > m + 1 && host[n - m - 1] == b'.' && host[n - m..].eq_ignore_ascii_case(suffix.as_bytes())
    }
    None => host.eq_ignore_ascii_case(pattern.as_bytes()),
  }
}

/// True when `host` is covered by an organization's hostname allowlist. An
/// empty list (or one containing `*`) is unrestricted. `*.acme.com` matches
/// any subdomain of `acme.com` at any depth, but not `acme.com` itself, so an
/// operator who wants both lists both.
pub fn hostname_in_org_allowlist(host: &str, patterns: &[String]) -> bool {
  if patterns.is_empty() {
    return true;
  }
  patterns
    .iter()
    .any(|pattern| pattern_matches_host(pattern, host))
}

/// The suffix of a subdomain wildcard (`*.acme.com` -> `acme.com`), or None
/// for an exact hostname.
fn wildcard_suffix(pattern: &str) -> Option<&str> {
  pattern.strip_prefix("*.")
}

/// True when the names `outer` covers include every name `inner` covers, both
/// being allowlist patterns (`*`, `*.acme.com`, or an exact hostname).
///
/// This is the question a *wildcard* operation asks: putting `*.acme.com`
/// into maintenance is a claim over a whole subtree, and only an entry that
/// owns the subtree can authorize it. An exact `acme.com` cannot, since it
/// covers one name and the request covers all the others.
pub fn pattern_covers_pattern(outer: &str, inner: &str) -> bool {
  if outer == "*" {
    return true;
  }
  match (wildcard_suffix(outer), wildcard_suffix(inner)) {
    // `*.acme.com` covers `*.acme.com` and `*.eu.acme.com`.
    (Some(outer), Some(inner)) => inner == outer || inner.ends_with(&format!(".{outer}")),
    // `*.acme.com` covers the names under it, one at a time too.
    (Some(_), None) => hostname_in_org_allowlist(inner, &[outer.to_string()]),
    // An exact entry covers nothing but itself, and never a subtree.
    (None, Some(_)) => false,
    (None, None) => outer.eq_ignore_ascii_case(inner),
  }
}

/// True when two allowlist patterns have any hostname in common. Asymmetric
/// coverage is not enough here: `*.acme.com` and `*.eu.acme.com` overlap in
/// both directions of the question "is someone else already inside this".
pub fn patterns_overlap(a: &str, b: &str) -> bool {
  pattern_covers_pattern(a, b) || pattern_covers_pattern(b, a)
}

/// Per-organization OIDC single sign-on configuration.
#[derive(Serialize, Deserialize, Clone, utoipa::ToSchema)]
pub struct OrgOidc {
  pub issuer: String,
  pub client_id: String,
  pub client_secret: String,
  /// Allowed email patterns (exact, `*@domain`, or `*`).
  pub allowed_emails: Vec<String>,
}

/// Persistent store of child organizations, backed by the `organizations`
/// table of the shared SQLite store.
pub struct OrgStore {
  conn: rusqlite::Connection,
  orgs: Vec<Organization>,
}

impl OrgStore {
  pub fn load(data_dir: &str) -> Self {
    let conn = crate::store::open_db(data_dir);
    let orgs: Vec<Organization> = crate::store::load_all(&conn, "organizations");
    if !orgs.is_empty() {
      info!("Loaded {} organization(s) from the store", orgs.len());
    }
    OrgStore { conn, orgs }
  }

  fn persist(&mut self) {
    let rows: Vec<(String, String)> = self
      .orgs
      .iter()
      .filter_map(|o| serde_json::to_string(o).ok().map(|j| (o.id.clone(), j)))
      .collect();
    crate::store::replace_all(&mut self.conn, "organizations", &rows);
  }

  /// Replaces every org record (dump import) and persists.
  pub fn import(&mut self, orgs: Vec<Organization>) -> usize {
    self.orgs = orgs;
    self.persist();
    self.orgs.len()
  }

  /// Creates a child organization. Names are unique (case-insensitive);
  /// `master` is reserved. `hostnames` is the optional allowlist fencing every
  /// bind made inside the org (already normalized by the caller); empty means
  /// unrestricted.
  pub fn create(
    &mut self,
    name: &str,
    hostnames: Vec<String>,
    custom_name: Option<String>,
  ) -> Result<Organization, String> {
    let name = name.trim();
    if name.is_empty() {
      return Err("organization name is required".into());
    }
    if name.eq_ignore_ascii_case("master") {
      return Err("\"master\" is reserved for the built-in organization".into());
    }
    // The handle is an identifier, not a label: it is written in a server's
    // `expose:`, in a binder's config and in `payments@postgres`, by people
    // who are not looking at this screen. `custom_name` is where anything
    // human belongs.
    aperio_config::validate_name("organization", name)?;
    if self.orgs.iter().any(|o| o.name.eq_ignore_ascii_case(name)) {
      return Err(format!("an organization named \"{name}\" already exists"));
    }
    let org = Organization {
      id: uuid::Uuid::new_v4().to_string(),
      name: name.to_string(),
      custom_name: normalize_custom_name(custom_name),
      created_at: crate::store::tokens::now_secs(),
      max_clients: None,
      max_tokens: None,
      max_users: None,
      max_bytes_month: None,
      hostnames,
      oidc: None,
    };
    self.orgs.push(org.clone());
    self.persist();
    Ok(org)
  }

  /// Renames what the organization is *called*, never what it *is*.
  ///
  /// The handle stays: an `expose:` rule, a binder's config and every
  /// `<org>@<tunnel>` written down elsewhere point at it, and none of those
  /// can be updated from here. `None` (or blank) goes back to showing the
  /// handle.
  pub fn set_custom_name(&mut self, id: &str, custom_name: Option<String>) -> bool {
    let Some(org) = self.orgs.iter_mut().find(|o| o.id == id) else {
      return false;
    };
    org.custom_name = normalize_custom_name(custom_name);
    self.persist();
    true
  }

  /// Removes an org by id. Returns whether one was removed.
  pub fn delete(&mut self, id: &str) -> bool {
    let before = self.orgs.len();
    self.orgs.retain(|o| o.id != id);
    let removed = self.orgs.len() != before;
    if removed {
      self.persist();
    }
    removed
  }

  pub fn list(&self) -> &[Organization] {
    &self.orgs
  }

  /// Looks up an org by id.
  pub fn find(&self, id: &str) -> Option<&Organization> {
    self.orgs.iter().find(|o| o.id == id)
  }

  /// Updates an org's quotas in place. `Some(None)` clears a quota, `Some(v)`
  /// sets it, `None` leaves it unchanged. Returns the updated record.
  pub fn set_quota(
    &mut self,
    id: &str,
    max_clients: Option<Option<u64>>,
    max_tokens: Option<Option<u64>>,
    max_users: Option<Option<u64>>,
    max_bytes_month: Option<Option<u64>>,
  ) -> Option<Organization> {
    let org = self.orgs.iter_mut().find(|o| o.id == id)?;
    if let Some(v) = max_clients {
      org.max_clients = v.filter(|n| *n > 0);
    }
    if let Some(v) = max_tokens {
      org.max_tokens = v.filter(|n| *n > 0);
    }
    if let Some(v) = max_users {
      org.max_users = v.filter(|n| *n > 0);
    }
    if let Some(v) = max_bytes_month {
      org.max_bytes_month = v.filter(|n| *n > 0);
    }
    let updated = org.clone();
    self.persist();
    Some(updated)
  }

  /// Replaces an org's hostname allowlist (empty = unrestricted). Entries are
  /// expected to be normalized by the caller. Returns the updated record.
  pub fn set_hostnames(&mut self, id: &str, hostnames: Vec<String>) -> Option<Organization> {
    let org = self.orgs.iter_mut().find(|o| o.id == id)?;
    org.hostnames = hostnames;
    let updated = org.clone();
    self.persist();
    Some(updated)
  }

  /// The hostname allowlist of an org (empty = unrestricted, and the master
  /// org is always unrestricted).
  pub fn hostnames_of(&self, id: Option<&str>) -> Vec<String> {
    id.and_then(|id| self.find(id))
      .map(|o| o.hostnames.clone())
      .unwrap_or_default()
  }

  /// Sets or clears an org's OIDC override. Returns the updated record.
  pub fn set_oidc(&mut self, id: &str, oidc: Option<OrgOidc>) -> Option<Organization> {
    let org = self.orgs.iter_mut().find(|o| o.id == id)?;
    org.oidc = oidc;
    let updated = org.clone();
    self.persist();
    Some(updated)
  }
}

#[cfg(test)]
#[path = "orgs_tests.rs"]
mod tests;

/// A display name that is blank, or the same as the handle, is no display
/// name: storing it would leave two things to keep in step for no gain.
fn normalize_custom_name(raw: Option<String>) -> Option<String> {
  raw.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
}
