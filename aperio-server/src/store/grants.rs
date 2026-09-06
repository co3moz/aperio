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
}

impl Grant {
  pub fn new(org: GrantOrg, role: Role) -> Grant {
    Grant { org, role }
  }

  /// `acme:operator`, the spelling audit records and the CLI use.
  pub fn label(&self) -> String {
    format!("{}:{}", self.org.as_str(), self.role.as_str())
  }
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
  match &grant.org {
    GrantOrg::All => holds_all_admin(granter),
    GrantOrg::Master => role_in(granter, None) == Some(Role::Admin),
    GrantOrg::Child(id) => role_in(granter, Some(id)) == Some(Role::Admin),
  }
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
