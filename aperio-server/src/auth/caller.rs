//! Who is calling, and what they hold: one resolution of a request's
//! credential into an identity with grants, from which every other question
//! in `scope.rs` is answered.
//!
//! Before this, the session carried a flat role and "Admin with no
//! organization" meant the super-admin, so a named Admin in master was the
//! built-in account under another name. Now the role is a function of the
//! organization being acted in, read from the user's grants on every request
//! (`planned_features.md` #153): a revoked grant must not survive on an open
//! session for one more request, for the same reason a disabled account's
//! sessions grant nothing.

use axum::http::HeaderMap;

use super::*;
use crate::server::panel::request_host;
use crate::state::AppState;
use crate::store::grants::{self, Grant, GrantOrg};

/// The credential behind a request, resolved.
#[derive(Clone, Debug)]
pub(crate) enum Identity {
  /// The built-in `aperio` account: master token or dashboard password. The
  /// session's role is Admin from those logins; carried rather than assumed,
  /// so a session that says less is read as less.
  BuiltIn { role: Role },
  /// A named dashboard user, with the grants on its row.
  Named {
    username: String,
    grants: Vec<Grant>,
  },
  /// An OIDC identity with no user row, holding the role its login recorded.
  /// Fixed to one organization by a per-organization login; a global login
  /// is master's Admin, which is what it always was, and `planned_features.md`
  /// #154 gives it a record to read instead.
  Oidc {
    email: String,
    bound_org: Option<String>,
    role: Role,
  },
  /// A programmatic admin key: one role, one scope, no session to switch.
  Key {
    name: String,
    role: Role,
    scope: GrantOrg,
  },
}

/// An identity plus the organization its session has selected.
#[derive(Clone, Debug)]
pub(crate) struct Caller {
  pub(crate) identity: Identity,
  /// The organization selected on the session, if any (`None` = nothing
  /// selected, or master). Honoured only when a grant reaches it.
  pub(crate) selected: Option<String>,
}

impl Caller {
  /// What this caller holds, per organization.
  pub(crate) fn grants(&self) -> Vec<Grant> {
    match &self.identity {
      // The pre-grants reading of "this role, in master": Admin reached
      // everything, anything else reached master alone.
      Identity::BuiltIn { role } => grants::legacy_grants(*role, None).0,
      Identity::Named { grants, .. } => grants.clone(),
      Identity::Oidc {
        bound_org, role, ..
      } => match bound_org {
        Some(org) => vec![Grant::new(GrantOrg::Child(org.clone()), *role)],
        None => grants::legacy_grants(*role, None).0,
      },
      Identity::Key { role, scope, .. } => vec![Grant::new(scope.clone(), *role)],
    }
  }

  /// The role held in organization `org` (`None` = master).
  pub(crate) fn role_in(&self, org: Option<&str>) -> Option<Role> {
    grants::role_in(&self.grants(), org)
  }

  /// Admin of the master organization: the gate on everything that is a
  /// property of the server rather than of one tenant. Held directly, or
  /// through `*` Admin.
  pub(crate) fn is_master_admin(&self) -> bool {
    self.role_in(None) == Some(Role::Admin)
  }

  /// Holds `*` Admin: the only caller that may hand `*` to somebody else.
  pub(crate) fn holds_all_admin(&self) -> bool {
    grants::holds_all_admin(&self.grants())
  }

  /// Whether this caller may give, or take away, `grant`.
  pub(crate) fn may_grant(&self, grant: &Grant) -> bool {
    grants::may_grant(&self.grants(), grant)
  }

  /// The organization the caller is acting in: the one selected on the
  /// session when a grant reaches it, else the default. A key has no session
  /// and acts in its own organization; a `*` key acts in master.
  pub(crate) fn effective_org(&self) -> Option<String> {
    match &self.identity {
      Identity::Key { scope, .. } => match scope {
        GrantOrg::Child(id) => Some(id.clone()),
        GrantOrg::Master | GrantOrg::All => None,
      },
      Identity::Oidc {
        bound_org: Some(org),
        ..
      } => Some(org.clone()),
      _ => {
        if let Some(sel) = &self.selected
          && self.role_in(Some(sel)).is_some()
        {
          return Some(sel.clone());
        }
        self.default_org()
      }
    }
  }

  /// Where a session lands before it selects anything: master when a grant
  /// reaches it, else the first child organization granted, by id, so two
  /// requests agree.
  fn default_org(&self) -> Option<String> {
    if self.role_in(None).is_some() {
      return None;
    }
    let mut children: Vec<String> = self
      .grants()
      .into_iter()
      .filter_map(|g| match g.org {
        GrantOrg::Child(id) => Some(id),
        _ => None,
      })
      .collect();
    children.sort_unstable();
    children.into_iter().next()
  }

  /// Whether the caller may select organization `org` on its session.
  pub(crate) fn may_select(&self, org: Option<&str>) -> bool {
    match &self.identity {
      Identity::Key { .. } => false,
      Identity::Oidc {
        bound_org: Some(_), ..
      } => false,
      _ => self.role_in(org).is_some(),
    }
  }

  /// The name an audit record files the action under.
  pub(crate) fn actor(&self) -> String {
    match &self.identity {
      Identity::BuiltIn { .. } => "aperio".to_string(),
      Identity::Named { username, .. } => username.clone(),
      Identity::Oidc { email, .. } => email.clone(),
      Identity::Key { name, .. } => format!("key:{name}"),
    }
  }
}

/// Resolves the request's credential: a valid admin-plane session cookie
/// first, then a programmatic admin key. `None` when neither is presented or
/// neither is valid, which includes the session of a disabled account.
pub(crate) async fn resolve_caller(state: &AppState, headers: &HeaderMap) -> Option<Caller> {
  if let Some(token) = session_cookie(headers, state.config().secure_cookies) {
    let fenced = state.config().fenced_login;
    let host = request_host(headers);
    let session = {
      let sessions = state.sessions.lock().await;
      sessions.get(token).and_then(|info| {
        let live = info.expires_at > crate::store::sessions::now_secs()
          && info.scope_host.is_none()
          && info.plane == crate::store::sessions::Plane::Admin
          // Under `fenced_login`, only on the hostname it was minted on.
          && info.usable_on(fenced, host.as_deref());
        live.then(|| {
          (
            info.username.clone(),
            info.selected_org.clone(),
            info.bound_org.clone(),
            info.role,
          )
        })
      })
    };
    if let Some((username, selected, bound_org, role)) = session {
      // A session fixed to an organization is fixed whatever else it says:
      // that is the per-organization OIDC login. Its record, when it has one,
      // is read for the role in that organization and nothing else, so a
      // row sharing the name cannot widen it past the organization whose
      // identity provider vouched for it.
      let identity = if let Some(org) = bound_org {
        let users = state.users.lock().await;
        match username
          .as_deref()
          .and_then(|name| users.find_by_username(name))
        {
          Some(user) => Some(Identity::Named {
            username: user.username.clone(),
            grants: user
              .role_in(Some(&org))
              .map(|role| vec![Grant::new(GrantOrg::Child(org.clone()), role)])
              .unwrap_or_default(),
          }),
          None
            if username
              .as_deref()
              .is_some_and(|name| users.is_disabled_username(name)) =>
          {
            None
          }
          None => Some(Identity::Oidc {
            email: username.unwrap_or_default(),
            bound_org: Some(org),
            role,
          }),
        }
      } else {
        match username {
          None => Some(Identity::BuiltIn { role }),
          Some(name) => {
            let users = state.users.lock().await;
            match users.find_by_username(&name) {
              Some(user) => Some(Identity::Named {
                username: user.username.clone(),
                grants: user.grants.clone(),
              }),
              // A row exists and is disabled: the session grants nothing,
              // and an admin key presented alongside it is still read. No
              // row at all is a global OIDC identity.
              None if users.is_disabled_username(&name) => None,
              None => Some(Identity::Oidc {
                email: name,
                bound_org: None,
                role,
              }),
            }
          }
        }
      };
      if let Some(identity) = identity {
        return Some(Caller { identity, selected });
      }
    }
  }
  let presented = extract_token(headers)?;
  let store = state.admin_key_store.lock().await;
  let key = store.verify(&presented)?;
  Some(Caller {
    identity: Identity::Key {
      name: key.name.clone(),
      role: key.role,
      scope: key.scope(),
    },
    selected: None,
  })
}

#[cfg(test)]
#[path = "caller_tests.rs"]
mod tests;
