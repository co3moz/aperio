//! What a dashboard identity may do, per organization.
//!
//! A user record used to carry one role in one organization, and "Admin with
//! no organization" was read everywhere as the super-admin: a named Admin
//! created in master was the built-in `aperio` account under another name,
//! while a Viewer in master saw master alone. Neither is what an operator
//! means by "this person administers two tenants and reads a third".
//!
//! A grant is one `(organization, role)` pair, and an identity holds a list
//! of them. The organization is a child id, `master`, or `*` for every
//! organization present and future. The rules, each pinned by a test in
//! `grants_tests.rs`:
//!
//! - **A specific entry beats `*`.** `{*: viewer, acme: operator}` reads the
//!   way it looks.
//! - **A grant is bounded by the granter's.** Giving a role in an
//!   organization takes Admin there; giving `*` takes `*` Admin. Without this
//!   an organization's admin grants themselves a second organization.
//! - **The old shape converts without a migration.** A row with no `grants`
//!   is one grant in its home organization, except the master Admin, who
//!   becomes `*` Admin, because that is exactly what the row could do before
//!   and narrowing it on an upgrade would lock out whoever runs the server.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub use aperio_config::expose::ExposeAction;

use super::users::Role;

/// The reserved spelling of "every organization".
pub const ALL_ORGS: &str = "*";

/// Where one grant applies.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GrantOrg {
  /// The implicit master organization (`org_id: None` everywhere else).
  Master,
  /// Every organization, present and future.
  All,
  /// One child organization, by id.
  Child(String),
}

impl GrantOrg {
  /// Parses the API spelling: `master` (or empty) for master, `*` for every
  /// organization, anything else is a child id. Whether that id exists is the
  /// caller's question, the store is not reachable from here.
  pub fn parse(raw: &str) -> GrantOrg {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case(super::orgs::MASTER_ID) {
      GrantOrg::Master
    } else if raw == ALL_ORGS {
      GrantOrg::All
    } else {
      GrantOrg::Child(raw.to_string())
    }
  }

  /// The API spelling: `master`, `*`, or the child id.
  pub fn as_str(&self) -> &str {
    match self {
      GrantOrg::Master => super::orgs::MASTER_ID,
      GrantOrg::All => ALL_ORGS,
      GrantOrg::Child(id) => id,
    }
  }

  /// From the `org_id` shape the rest of the server uses (`None` = master).
  pub fn from_org_id(org_id: Option<&str>) -> GrantOrg {
    match org_id {
      None => GrantOrg::Master,
      Some(id) => GrantOrg::Child(id.to_string()),
    }
  }

  /// Whether this grant reaches organization `org` (`None` = master).
  pub fn covers(&self, org: Option<&str>) -> bool {
    match self {
      GrantOrg::All => true,
      GrantOrg::Master => org.is_none(),
      GrantOrg::Child(id) => org == Some(id.as_str()),
    }
  }
}

impl Serialize for GrantOrg {
  fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(self.as_str())
  }
}

impl<'de> Deserialize<'de> for GrantOrg {
  fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    let raw = String::deserialize(d)?;
    Ok(GrantOrg::parse(&raw))
  }
}

/// One `(organization, role)` pair.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Grant {
  pub org: GrantOrg,
  pub role: Role,
  /// Explicit public-listener capabilities in this organization. Old tenant
  /// roles have none; holding Admin there does not grant server sockets.
  #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
  pub expose: BTreeSet<ExposeAction>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub expose_bounds: Option<aperio_config::expose_policy::ExposePolicy>,
  /// Where this grant came from when it was not written by hand: the group
  /// claim value an OIDC login mapped it from (`planned_features.md` #154).
  /// A mapped grant is the directory's to take back at the next login; a
  /// hand-written one is left alone by the map.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub source: Option<String>,
}

impl Grant {
  pub fn new(org: GrantOrg, role: Role) -> Grant {
    Grant {
      org,
      role,
      expose: BTreeSet::new(),
      expose_bounds: None,
      source: None,
    }
  }

  /// A grant an OIDC group claim produced.
  pub fn mapped(org: GrantOrg, role: Role, group: &str) -> Grant {
    Grant {
      org,
      role,
      expose: BTreeSet::new(),
      expose_bounds: None,
      source: Some(group.to_string()),
    }
  }

  /// `acme:operator`, the spelling audit records and the CLI use.
  pub fn label(&self) -> String {
    let mut label = format!("{}:{}", self.org.as_str(), self.role.as_str());
    for action in &self.expose {
      label.push_str("+expose.");
      label.push_str(action.as_str());
    }
    label
  }

  /// Role/capability spelling used by a tenant's OIDC mapping. The tenant
  /// comes from the authenticated management path, never from the IdP text.
  pub fn parse_in_org(org: &str, raw: &str) -> Result<Grant, String> {
    let grant = Self::parse(&format!("{org}:{}", raw.trim()))?;
    if grant.org != GrantOrg::parse(org) {
      return Err("OIDC mapping cannot name another organization".into());
    }
    Ok(grant)
  }

  /// The inverse of [`Grant::label`]: `<org>:<role>`, where `<org>` is a
  /// child id or handle, `master`, or `*`. The role is the last segment, so
  /// nothing an organization is called may contain a colon, which a handle
  /// cannot anyway.
  pub fn parse(raw: &str) -> Result<Grant, String> {
    let Some((org, role)) = raw.trim().rsplit_once(':') else {
      return Err(format!("a grant is written <org>:<role>, got {raw:?}"));
    };
    let mut parts = role.split('+');
    let Some(role) = Role::parse(parts.next().unwrap_or_default()) else {
      return Err(format!(
        "unknown role {role:?} in {raw:?}: viewer, operator, or admin"
      ));
    };
    if org.trim().is_empty() {
      return Err(format!("a grant is written <org>:<role>, got {raw:?}"));
    }
    let mut grant = Grant::new(GrantOrg::parse(org), role);
    for part in parts {
      let action = part
        .strip_prefix("expose.")
        .and_then(|name| ExposeAction::ALL.into_iter().find(|a| a.as_str() == name));
      let Some(action) = action else {
        return Err(format!("unknown grant capability {part:?}"));
      };
      grant.expose.insert(action);
    }
    Ok(grant)
  }
}

/// A comma-separated list of `<org>:<role>` grants, as an environment
/// variable carries it. Empty in, empty out.
pub fn parse_list(raw: &str) -> Result<Vec<Grant>, String> {
  raw
    .split(',')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(Grant::parse)
    .collect()
}

/// A comma-separated list of `<group>=<org>:<role>` entries: what a value of
/// the OIDC groups claim means. The group is everything before the first
/// `=`, so a group name may not contain one.
pub fn parse_group_map(raw: &str) -> Result<Vec<(String, Grant)>, String> {
  raw
    .split(',')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(|entry| {
      let Some((group, grant)) = entry.split_once('=') else {
        return Err(format!(
          "a group mapping is written <group>=<org>:<role>, got {entry:?}"
        ));
      };
      let group = group.trim();
      if group.is_empty() {
        return Err(format!(
          "a group mapping is written <group>=<org>:<role>, got {entry:?}"
        ));
      }
      Grant::parse(grant).map(|g| (group.to_string(), g))
    })
    .collect()
}

/// Applies what a login's group claims mapped to onto a record's grants.
///
/// The map owns what it produced: a grant it produces now is written, a grant
/// it produced before and no longer does is removed, and a grant written by
/// hand (no `source`) is left alone unless the map now names the same
/// organization, where the directory wins. Returns the new list and what
/// changed, `(next, added, removed)`, for the audit log; nothing here is
/// persisted.
pub fn apply_group_map(
  current: &[Grant],
  mapped: Vec<Grant>,
) -> (Vec<Grant>, Vec<Grant>, Vec<Grant>) {
  // Rebuild from this login's groups, then diff the effective grants. In
  // particular, two groups contributing different expose actions must not
  // accumulate revoked actions or generate changes on every identical login.
  let mut next: Vec<Grant> = current
    .iter()
    .filter(|g| g.source.is_none() && !mapped.iter().any(|m| m.org == g.org))
    .cloned()
    .collect();
  for m in mapped {
    match next.iter_mut().find(|g| g.org == m.org) {
      Some(have) => {
        let combined = have.expose.union(&m.expose).copied().collect();
        let bounds = match (&have.expose_bounds, &m.expose_bounds) {
          (Some(a), Some(b)) => Some(a.intersection(b)),
          (a, b) => a.clone().or_else(|| b.clone()),
        };
        if m.role > have.role {
          *have = m;
        }
        have.expose = combined;
        have.expose_bounds = bounds;
      }
      None => next.push(m),
    }
  }
  let (added, removed) = diff(current, &next);
  (next, added, removed)
}

/// The single grant `*` Admin: what the built-in account holds, and what a
/// pre-grants master Admin becomes.
pub fn all_admin() -> Vec<Grant> {
  vec![Grant::new(GrantOrg::All, Role::Admin)]
}

/// The role a grant list gives in organization `org` (`None` = master). A
/// grant naming the organization wins over `*`; no grant reaches it means
/// `None`.
pub fn role_in(grants: &[Grant], org: Option<&str>) -> Option<Role> {
  let exact = grants
    .iter()
    .find(|g| g.org != GrantOrg::All && g.org.covers(org));
  if let Some(g) = exact {
    return Some(g.role);
  }
  grants
    .iter()
    .find(|g| g.org == GrantOrg::All)
    .map(|g| g.role)
}

/// True when the list holds `*` at Admin: the only holder that may hand `*`
/// to somebody else.
pub fn holds_all_admin(grants: &[Grant]) -> bool {
  grants
    .iter()
    .any(|g| g.org == GrantOrg::All && g.role == Role::Admin)
}

/// Whether a holder of `granter` may give (or take away) `grant`.
///
/// `*` is given only by `*` Admin. Anything else takes Admin in that
/// organization, which the granter may hold directly or through `*`.
pub fn may_grant(granter: &[Grant], grant: &Grant) -> bool {
  let role_allowed = match &grant.org {
    GrantOrg::All => holds_all_admin(granter),
    GrantOrg::Master => role_in(granter, None) == Some(Role::Admin),
    GrantOrg::Child(id) => role_in(granter, Some(id)) == Some(Role::Admin),
  };
  if !role_allowed || grant.expose.is_empty() {
    return role_allowed;
  }
  // Master admins already administer global server resources. A tenant
  // admin needs explicit delegation and every capability they hand out.
  if role_in(granter, None) == Some(Role::Admin) {
    return true;
  }
  let org = match &grant.org {
    GrantOrg::Child(id) => Some(id.as_str()),
    GrantOrg::Master => None,
    GrantOrg::All => return false,
  };
  let bounded = match (expose_bounds(granter, org), &grant.expose_bounds) {
    (None, _) => true,
    (Some(parent), Some(child)) => parent.contains(child),
    (Some(_), None) => false,
  };
  bounded
    && may_expose(granter, org, ExposeAction::Delegate)
    && grant
      .expose
      .iter()
      .all(|action| may_expose(granter, org, *action))
}

pub fn expose_bounds(
  grants: &[Grant],
  org: Option<&str>,
) -> Option<aperio_config::expose_policy::ExposePolicy> {
  if role_in(grants, None) == Some(Role::Admin) {
    return None;
  }
  grants
    .iter()
    .find(|g| g.org != GrantOrg::All && g.org.covers(org))
    .or_else(|| grants.iter().find(|g| g.org == GrantOrg::All))
    .and_then(|g| g.expose_bounds.clone())
}

/// The exact organization's capability set replaces the wildcard's, even
/// when empty. Master Admin retains its existing server-global authority.
pub fn may_expose(grants: &[Grant], org: Option<&str>, action: ExposeAction) -> bool {
  if role_in(grants, None) == Some(Role::Admin) {
    return true;
  }
  let grant = grants
    .iter()
    .find(|g| g.org != GrantOrg::All && g.org.covers(org))
    .or_else(|| grants.iter().find(|g| g.org == GrantOrg::All));
  grant.is_some_and(|g| {
    g.expose.contains(&action) || (action == ExposeAction::Read && !g.expose.is_empty())
  })
}

/// The grants a record written before this field existed stands for: one
/// grant in its home organization, except the master Admin, who could do
/// everything and keeps being able to. The `widened` flag says which case
/// this was, so the start can say so in the audit log.
pub fn legacy_grants(role: Role, org_id: Option<&str>) -> (Vec<Grant>, bool) {
  if role == Role::Admin && org_id.is_none() {
    (all_admin(), true)
  } else {
    (vec![Grant::new(GrantOrg::from_org_id(org_id), role)], false)
  }
}

/// Puts a list in canonical order and drops repeated organizations, keeping
/// the last spelling of each, so `[acme:viewer, acme:admin]` means Admin.
/// Rejects an empty list: a user nothing reaches cannot sign in anywhere,
/// and is better refused at the form than discovered at the login page.
pub fn normalize(grants: Vec<Grant>) -> Result<Vec<Grant>, String> {
  let mut out: Vec<Grant> = Vec::new();
  for g in grants {
    if let Some(bounds) = &g.expose_bounds
      && (matches!(g.org, GrantOrg::All)
        || bounds.org_id != g.org.as_str()
        || !bounds.validate().is_empty())
    {
      return Err("expose bounds must be valid and name the grant's concrete organization".into());
    }
    out.retain(|have| have.org != g.org);
    out.push(g);
  }
  if out.is_empty() {
    return Err("at least one grant is required".into());
  }
  // `*` first, then master, then children by id: the order the picker shows
  // and the order a reader expects.
  out.sort_by(|a, b| {
    let rank = |o: &GrantOrg| match o {
      GrantOrg::All => 0,
      GrantOrg::Master => 1,
      GrantOrg::Child(_) => 2,
    };
    rank(&a.org)
      .cmp(&rank(&b.org))
      .then_with(|| a.org.as_str().cmp(b.org.as_str()))
  });
  Ok(out)
}

/// The grants in `after` that are not in `before`, and the ones in `before`
/// that are not in `after`, as `(added, removed)`. A role change in one
/// organization is one removal and one addition, so both halves are checked
/// against the granter.
pub fn diff(before: &[Grant], after: &[Grant]) -> (Vec<Grant>, Vec<Grant>) {
  let added = after
    .iter()
    .filter(|g| !before.contains(g))
    .cloned()
    .collect();
  let removed = before
    .iter()
    .filter(|g| !after.contains(g))
    .cloned()
    .collect();
  (added, removed)
}

#[cfg(test)]
#[path = "grants_tests.rs"]
mod tests;
