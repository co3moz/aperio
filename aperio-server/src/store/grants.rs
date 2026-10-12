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
//! - **`*` Admin is the server admin, and nothing else is.** Master is an
//!   ordinary organization: its Admin administers master's tokens, exposes
//!   and sites, not the server's settings, organizations or users. One
//!   holder of server-wide authority, instead of a master Admin who was the
//!   server admin for most things and not for handing out `*`.
//! - **A grant is bounded by the granter's.** Giving a role in an
//!   organization takes Admin there; giving `*` takes `*` Admin. Without this
//!   an organization's admin grants themselves a second organization.
//! - **What a role may do with public listeners is part of the role.** A
//!   Viewer reads them, an Operator also enables, disables and disconnects,
//!   an Admin manages them; all within the ports the server admin allocated
//!   to the organization, which is the one bound.
//! - **The old shape converts without a migration.** A row with no `grants`
//!   is one grant in its home organization, except the master Admin, who
//!   becomes `*` Admin, because that is exactly what the row could do before
//!   and narrowing it on an upgrade would lock out whoever runs the server.

use serde::{Deserialize, Serialize};

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
///
/// A row written before 0.13.0 may still carry `expose` and `expose_bounds`;
/// they are ignored on read, since what a role may do with public listeners
/// is the role's now, and dropped at the next write.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Grant {
  pub org: GrantOrg,
  pub role: Role,
  /// Where this grant came from when it was not written by hand: the group
  /// claim value an OIDC login mapped it from (`planned_features.md` #154).
  /// A mapped grant is the directory's to take back at the next login; a
  /// hand-written one is left alone by the map.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub source: Option<String>,
  /// Which identity provider's map produced a mapped grant: the child
  /// organization whose own OIDC login did, or `None` for the server's
  /// global provider. Read only when `source` is set. Without it, a login
  /// through one provider took back what the other had mapped, so the same
  /// email signing in through Acme's provider lost the `*` its corporate
  /// login had been granted.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub source_org: Option<String>,
}

impl Grant {
  pub fn new(org: GrantOrg, role: Role) -> Grant {
    Grant {
      org,
      role,
      source: None,
      source_org: None,
    }
  }

  /// A grant an OIDC group claim produced.
  pub fn mapped(org: GrantOrg, role: Role, group: &str) -> Grant {
    Grant {
      org,
      role,
      source: Some(group.to_string()),
      source_org: None,
    }
  }

  /// `acme:operator`, the spelling audit records and the CLI use.
  pub fn label(&self) -> String {
    format!("{}:{}", self.org.as_str(), self.role.as_str())
  }

  /// Role spelling used by a tenant's OIDC mapping. The tenant comes from the
  /// authenticated management path, never from the IdP text.
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
    if let Some((role, _)) = role.split_once('+') {
      return Err(format!(
        "{raw:?}: expose capabilities are part of the role since 0.13.0 (a viewer \
         reads public listeners, an operator also enables, disables and disconnects \
         them, an admin manages them); write {}:{}",
        org.trim(),
        role.trim()
      ));
    }
    let Some(role) = Role::parse(role) else {
      return Err(format!(
        "unknown role {role:?} in {raw:?}: viewer, operator, or admin"
      ));
    };
    if org.trim().is_empty() {
      return Err(format!("a grant is written <org>:<role>, got {raw:?}"));
    }
    Ok(Grant::new(GrantOrg::parse(org), role))
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
/// `provider` is whose map this is: the child organization of a
/// per-organization OIDC login, or `None` for the global one. A map owns what
/// it produced and nothing else: a grant it produces now is written, a grant
/// it produced before and no longer does is removed, and anything else, a
/// grant written by hand (no `source`) or one another provider mapped, is
/// left alone unless this map now names the same organization, where the
/// directory signing in now wins. Returns the new list and what changed,
/// `(next, added, removed)`, for the audit log; nothing here is persisted.
///
/// A grant mapped before `source_org` existed reads as the global
/// provider's. If a per-organization map wrote it, the next global login
/// takes it back, as every login did then, and the next login through that
/// organization writes it again under its own name.
pub fn apply_group_map(
  current: &[Grant],
  mapped: Vec<Grant>,
  provider: Option<&str>,
) -> (Vec<Grant>, Vec<Grant>, Vec<Grant>) {
  let owned = |g: &Grant| g.source.is_some() && g.source_org.as_deref() == provider;
  let mapped: Vec<Grant> = mapped
    .into_iter()
    .map(|mut m| {
      m.source_org = provider.map(str::to_string);
      m
    })
    .collect();
  // Rebuild from this login's groups, then diff the effective grants, so an
  // identical login changes nothing. Two groups naming one organization give
  // the higher of their roles.
  let mut next: Vec<Grant> = current
    .iter()
    .filter(|g| !owned(g) && !mapped.iter().any(|m| m.org == g.org))
    .cloned()
    .collect();
  for m in mapped {
    match next.iter_mut().find(|g| g.org == m.org) {
      Some(have) => {
        if m.role > have.role {
          *have = m;
        }
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

/// True when the list holds `*` at Admin: the server admin, the one holder
/// of server-wide authority (settings, organizations, users, admin keys,
/// port allocations) and the only one who may hand `*` to somebody else.
pub fn is_server_admin(grants: &[Grant]) -> bool {
  grants
    .iter()
    .any(|g| g.org == GrantOrg::All && g.role == Role::Admin)
}

/// Whether a holder of `granter` may give (or take away) `grant`.
///
/// `*` is given only by the server admin. Anything else takes Admin in that
/// organization, which the granter may hold directly or through `*`.
pub fn may_grant(granter: &[Grant], grant: &Grant) -> bool {
  match &grant.org {
    GrantOrg::All => is_server_admin(granter),
    GrantOrg::Master => role_in(granter, None) == Some(Role::Admin),
    GrantOrg::Child(id) => role_in(granter, Some(id)) == Some(Role::Admin),
  }
}

/// What `role` may do with an organization's public listeners. Reading is
/// every role's; switching one on or off and cutting its sessions is the
/// day-to-day work an Operator does; creating, changing and deleting one is
/// administering the organization.
pub fn role_may_expose(role: Role, action: ExposeAction) -> bool {
  match action {
    ExposeAction::Read => true,
    ExposeAction::Enable | ExposeAction::Disable | ExposeAction::Disconnect => {
      role >= Role::Operator
    }
    ExposeAction::Create | ExposeAction::Update | ExposeAction::Delete => role == Role::Admin,
  }
}

/// Whether the role a grant list gives in `org` (`None` = master) permits
/// `action` on its public listeners. Where a listener may be, and how many,
/// is the organization's port allocation, which the caller checks.
pub fn may_expose(grants: &[Grant], org: Option<&str>, action: ExposeAction) -> bool {
  role_in(grants, org).is_some_and(|role| role_may_expose(role, action))
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
