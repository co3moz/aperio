//! Resolving a tunnel: who may reach it, and which connection carries it.
//!
//! A tunnel is addressed by its **name** within an organization. That is the
//! only handle an operator writes: the server-side connection id is a fresh
//! UUID per connection, and the client suffixes its own id per service
//! (`<client_id>-<index>`), so neither is something a configuration file can
//! name and keep naming across a restart.
//!
//! One process may hold several connections (parallel `connections:`, a
//! `services:` list) and every one of them announces the same `tunnels:`
//! list. Those are not competing declarations, they are several **paths** to
//! one tunnel, which is why resolution picks an available path rather than
//! the first match and then discovering it is draining.

use std::sync::Arc;
use tokio::sync::mpsc;

use crate::protocol::TunnelDecl;
use crate::state::{AppState, ClientHandle, ClientPerms};

/// The name a tunnel is addressed by: what it declared, or one derived from
/// its target and protocol.
///
/// Mirrors `aperio_config::tunnel_name` for the wire type. The two must agree
/// exactly, since the client writes the name into its configuration and the
/// server is what resolves it; a shared test pins them together.
pub(crate) fn name_of(decl: &TunnelDecl) -> String {
  aperio_config::tunnel_name(&aperio_config::TunnelDecl {
    name: decl.name.clone(),
    custom_name: decl.custom_name.clone(),
    target: decl.target.clone(),
    protocol: decl.protocol.clone(),
    encrypt: decl.encrypt,
    psk: None,
    proxy_protocol: false,
    idle_timeout: decl.idle_timeout,
    expose: decl.expose.clone(),
  })
}

/// Splits the `<org>@<name>` spelling of a tunnel handle.
///
/// A tunnel name is unique inside an organization and nowhere else, so
/// wherever a name is written down without one, a server's `expose:`, a
/// binder run with the master token, it is only unambiguous by accident.
/// Qualifying it says which organization is meant, and reads the way the
/// dashboard shows it: `master@dns`, `payments@postgres`. An organization
/// name cannot contain `@`, which is what keeps the split unambiguous.
pub(crate) fn split_qualified(raw: &str) -> (Option<&str>, &str) {
  match raw.trim().split_once('@') {
    Some((org, name)) => (Some(org.trim()), name.trim()),
    None => (None, raw.trim()),
  }
}

/// The `org_id` clients of the organization named `name` carry.
///
/// `master` (and the empty name) is the built-in organization, whose clients
/// carry `None`, the same convention the rest of the admin API uses. An
/// unknown name is an error rather than a silent match against everything:
/// a typo in a server file must not open a port to whoever answers first.
pub(crate) async fn org_id_for_name(
  state: &Arc<AppState>,
  name: &str,
) -> Result<Option<String>, String> {
  let name = name.trim();
  if name.is_empty() || name.eq_ignore_ascii_case("master") {
    return Ok(None);
  }
  state
    .org_store
    .lock()
    .await
    .list()
    .iter()
    .find(|o| o.name.eq_ignore_ascii_case(name))
    .map(|o| Some(o.id.clone()))
    .ok_or_else(|| format!("no organization named '{name}'"))
}

/// Why a tunnel could not be resolved. Each variant is a distinct answer to
/// "why did nothing happen", which is the whole point of separating them:
/// the previous code collapsed several of these into one 404.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Rejection {
  /// No tunnel of that name (or no client of that id) is connected.
  Unknown,
  /// The tunnel exists but this caller may not bind it.
  Forbidden,
  /// The tunnel exists and is permitted, but no connection can serve it now.
  Unavailable,
}

/// A resolved tunnel: which connection to send through, and what it declares.
#[derive(Debug)]
pub(crate) struct Resolved {
  /// Server-side connection id of the chosen path.
  pub(crate) client_id: String,
  /// Sender of that connection.
  pub(crate) tx: mpsc::Sender<axum::extract::ws::Message>,
  /// The declaration, so callers do not look it up a second time.
  pub(crate) decl: crate::protocol::TunnelDecl,
}

/// One tunnel as discovery reports it.
#[derive(serde::Serialize, Clone, Debug, utoipa::ToSchema)]
pub(crate) struct TunnelView {
  /// Handle to bind, unique within the organization.
  pub(crate) name: String,
  /// What the declaring client calls it on screen, when it named one.
  /// Never addressed by anything: the handle above is.
  pub(crate) custom_name: Option<String>,
  /// `tcp`, `udp`, or `tcp/udp` for a tunnel that serves both.
  pub(crate) protocol: String,
  /// Address the declaring client dials locally.
  pub(crate) target: String,
  /// The declaring process's own `client_id`, as written in its config file.
  ///
  /// Deliberately not the per-connection id: that one carries a service index
  /// the operator never sees, and with several paths it would be whichever
  /// connection happened to be visited first, so it could change between two
  /// calls that describe the same tunnel.
  pub(crate) client_id: Option<String>,
  /// What that client calls itself, when it named a service. Shown instead
  /// of the id, which is a uuid unless an operator wrote `client_id:`.
  pub(crate) client_name: Option<String>,
  /// How many connections of that process announce this tunnel.
  pub(crate) paths: usize,
  /// True when at least one of those paths can serve a bind right now.
  pub(crate) available: bool,
  /// End-to-end encrypted between the two clients (TCP only).
  pub(crate) encrypt: bool,
  /// UDP relay expiry in seconds, when the declaration set one.
  pub(crate) idle_timeout: Option<u64>,
  /// Name of the token the declaring client connected with (None = master).
  pub(crate) token_name: Option<String>,
  /// Organization that owns it, by name. `None` is the master organization.
  /// A tunnel name is unique inside an organization and nowhere else, so this
  /// is what makes `payments@postgres` a complete address.
  pub(crate) org: Option<String>,
}

/// May a consumer authenticated as `consumer` bind a tunnel declared by a
/// client that connected as `owner`?
///
/// Three ways in, widest last:
/// - the master token, which is unrestricted everywhere;
/// - the very same token the declaring client used, which is the rule that
///   predates organizations and is kept so no existing binder breaks;
/// - a token in the same organization carrying `allow_bind`, which is the
///   point of the capability: reaching a database for ten minutes should not
///   require holding the credential that publishes services as that client.
pub(crate) fn may_bind(consumer: &ClientPerms, owner: &ClientPerms) -> bool {
  if consumer.master {
    return true;
  }
  if consumer.token_id.is_some() && consumer.token_id == owner.token_id {
    return true;
  }
  // `None` is the master organization on both sides; a dynamic token without
  // `allow_bind` never reaches here, so this cannot widen an existing setup.
  consumer.allow_bind && consumer.org_id == owner.org_id
}

/// True when this connection can carry a bind right now.
fn serviceable(handle: &ClientHandle, down_threshold: std::time::Duration) -> bool {
  handle.sole().admin_enabled && !handle.draining && handle.is_healthy(down_threshold)
}

/// Does this connection belong to the process (or the connection) `selector`
/// names? Matches the server-side connection id, the self-reported instance
/// id, and the process-wide instance group, the last being the raw
/// `client_id` from the configuration file, which is what an operator has.
fn matches_client(id: &str, handle: &ClientHandle, selector: &str) -> bool {
  id == selector
    || handle.reported_instance_id.as_deref() == Some(selector)
    || handle.instance_group.as_deref() == Some(selector)
}

/// How a request names the tunnel it wants.
pub(crate) enum Selector<'a> {
  /// `?tunnel=<name>`: the organization-wide handle, no client id needed.
  Name(&'a str),
  /// `?client=<id>&target=<host:port>`: the original addressing, kept
  /// working. The client id may be a connection id, a reported instance id
  /// or the process's raw `client_id`.
  ClientTarget {
    client: &'a str,
    target: &'a str,
    protocol: &'a str,
  },
}

/// Resolves a tunnel for a caller, choosing a path that can serve it.
///
/// Walks every connection rather than stopping at the first match, because a
/// process holds several and only some of them may be draining; answering
/// "unavailable" while a healthy sibling sits next to it is the bug this
/// replaces.
pub(crate) async fn resolve(
  state: &Arc<AppState>,
  perms: &ClientPerms,
  selector: Selector<'_>,
) -> Result<Resolved, Rejection> {
  // `<org>@<name>` narrows the search to one organization before anything is
  // matched. Resolved here, before the client map is locked, since it reads a
  // different store.
  let mut qualifier = None;
  if let Selector::Name(raw) = selector
    && let (Some(org), _) = split_qualified(raw)
  {
    match org_id_for_name(state, org).await {
      Ok(id) => qualifier = Some(id),
      Err(_) => return Err(Rejection::Unknown),
    }
  }
  let down_threshold = state.config().client_down_threshold;
  let clients = state.clients.read().await;

  let mut seen = Rejection::Unknown;
  for (cid, handle) in clients.iter() {
    if let Some(org_id) = &qualifier
      && handle.perms.org_id != *org_id
    {
      continue;
    }
    let decl = match selector {
      Selector::Name(name) => {
        let (_, name) = split_qualified(name);
        handle.sole().tunnels.iter().find(|d| name_of(d) == name)
      }
      Selector::ClientTarget {
        client,
        target,
        protocol,
      } => {
        if !matches_client(cid, handle, client.trim()) {
          continue;
        }
        handle.sole().tunnels.iter().find(|d| {
          d.target == target.trim() && aperio_config::protocol_serves(&d.protocol, protocol)
        })
      }
    };
    let Some(decl) = decl else { continue };
    // The tunnel exists somewhere; from here on `Unknown` would be a lie.
    if !may_bind(perms, &handle.perms) {
      seen = Rejection::Forbidden;
      continue;
    }
    if serviceable(handle, down_threshold) {
      return Ok(Resolved {
        client_id: cid.clone(),
        tx: handle.tx.clone(),
        decl: decl.clone(),
      });
    }
    // Keep looking: a sibling connection of the same process is usually fine.
    if seen != Rejection::Forbidden {
      seen = Rejection::Unavailable;
    }
  }
  Err(seen)
}

/// Every tunnel declared by the clients of one organization, for the
/// dashboard's read-only view. `None` is the master organization, which sees
/// everything, matching how the rest of the admin API scopes.
pub(crate) async fn visible_in_org(state: &Arc<AppState>, org: Option<&str>) -> Vec<TunnelView> {
  collect(state, |perms| {
    org.is_none() || perms.org_id.as_deref() == org
  })
  .await
}

/// Every tunnel this caller may bind, one entry per name.
pub(crate) async fn visible(state: &Arc<AppState>, perms: &ClientPerms) -> Vec<TunnelView> {
  collect(state, |owner| may_bind(perms, owner)).await
}

/// Folds the connected clients admitted by `include` into one entry per
/// tunnel name. Several connections of one process announce the same list,
/// so they are paths to one tunnel rather than several tunnels.
async fn collect(state: &Arc<AppState>, include: impl Fn(&ClientPerms) -> bool) -> Vec<TunnelView> {
  let down_threshold = state.config().client_down_threshold;
  // Read before the client map is locked, and once rather than per tunnel.
  let org_names: std::collections::HashMap<String, String> = state
    .org_store
    .lock()
    .await
    .list()
    .iter()
    .map(|o| (o.id.clone(), o.name.clone()))
    .collect();
  let clients = state.clients.read().await;
  let mut out: Vec<TunnelView> = Vec::new();
  for handle in clients.values() {
    if !include(&handle.perms) {
      continue;
    }
    let usable = serviceable(handle, down_threshold);
    for decl in &handle.sole().tunnels {
      let name = name_of(decl);
      let org = handle
        .perms
        .org_id
        .as_deref()
        .and_then(|id| org_names.get(id).cloned());
      // Two organizations may each declare `postgres`; they are two tunnels,
      // not two paths to one, so the entry is keyed by both.
      if let Some(existing) = out.iter_mut().find(|v| v.name == name && v.org == org) {
        existing.paths += 1;
        existing.available |= usable;
        continue;
      }
      out.push(TunnelView {
        name,
        custom_name: decl.custom_name.clone(),
        protocol: decl.protocol.clone(),
        target: decl.target.clone(),
        // The process handle, falling back to the per-connection id for a
        // client too old to announce `x-aperio-instance`.
        client_id: handle
          .instance_group
          .clone()
          .or_else(|| handle.reported_instance_id.clone()),
        client_name: handle.display_name(),
        paths: 1,
        available: usable,
        encrypt: decl.encrypt,
        idle_timeout: decl.idle_timeout,
        token_name: handle.perms.token_name.clone(),
        org,
      });
    }
  }
  out.sort_by(|a, b| (&a.org, &a.name).cmp(&(&b.org, &b.name)));
  out
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
