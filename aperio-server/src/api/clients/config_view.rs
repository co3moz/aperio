//! Rendering one service's effective configuration back as yaml.
//!
//! The point is the difference between what a file asked for and what the
//! service is running: every line that resolved to something else carries the
//! declared value and the reason beside it, which is the question this view
//! exists to answer.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use super::*;
use crate::state::AppState;

/// One setting whose effective value differs from what was configured.
#[derive(serde::Serialize, utoipa::ToSchema, Clone)]
pub(crate) struct ConfigNoteView {
  /// Config key the note is about (`bandwidth`, `connections`, `cache`, …).
  pub(crate) field: String,
  /// What was configured, as written (empty = nothing was configured).
  pub(crate) declared: String,
  /// What is actually in effect.
  pub(crate) effective: String,
  /// Why the two differ, one sentence.
  pub(crate) reason: String,
  /// `client` = the client resolved it before announcing it (only the client
  /// knows both sides, so it reports these itself); `server` = it was
  /// announced as configured but the server is not honoring it.
  pub(crate) source: String,
}

/// A connection's effective configuration for the dashboard's config view.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct ClientConfigView {
  /// The effective configuration as a YAML document, with each adjusted
  /// setting carrying its note as a trailing comment.
  pub(crate) yaml: String,
  /// Every setting whose effective value differs from what was configured.
  pub(crate) notes: Vec<ConfigNoteView>,
}

/// Quotes a value as a YAML double-quoted scalar.
fn yaml_str(v: &str) -> String {
  format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Appends `key: value`, with the note for that key (when there is one) as a
/// trailing comment, so the document still explains itself once copied out.
fn push_line(out: &mut String, notes: &[ConfigNoteView], key: &str, value: &str) {
  out.push_str(key);
  out.push_str(": ");
  out.push_str(value);
  if let Some(note) = notes.iter().find(|n| n.field == key) {
    let declared = if note.declared.is_empty() {
      "not set".to_string()
    } else {
      note.declared.clone()
    };
    out.push_str(&format!("  # declared {}: {}", declared, note.reason));
  }
  out.push('\n');
}

/// Builds the effective configuration of one connection: what the client
/// announced over its heartbeat, plus what the server applies on top.
///
/// This is deliberately not the client's `aperio.yaml`. Settings the client
/// never announces (its target, request timeouts, header rules, health probe)
/// are not knowable here and are left out rather than guessed at, which the
/// document's own header says.
fn client_config_view(
  id: &str,
  handle: &crate::state::ClientHandle,
  service: &crate::state::ServiceState,
  cache_enabled: bool,
) -> ClientConfigView {
  // What the client resolved differently before announcing it: only it knows
  // both sides, so it reports these in its Ping.
  let mut notes: Vec<ConfigNoteView> = service
    .config_notes
    .iter()
    .map(|n| ConfigNoteView {
      field: n.field.clone(),
      declared: n.declared.clone(),
      effective: n.effective.clone(),
      reason: n.reason.clone(),
      source: "client".to_string(),
    })
    .collect();

  // What the server is not honoring, though the client announced it as
  // configured. Each of these already logs a warning server-side; the point
  // here is that an operator should not have to find that line.
  let mut server_note = |field: &str, declared: &str, effective: &str, reason: &str| {
    notes.push(ConfigNoteView {
      field: field.to_string(),
      declared: declared.to_string(),
      effective: effective.to_string(),
      reason: reason.to_string(),
      source: "server".to_string(),
    })
  };
  if service.cache && !cache_enabled {
    server_note(
      "cache",
      "true",
      "false",
      "the server's response cache is disabled (APERIO_CACHE off), so the opt-in does nothing",
    );
  }
  if service.public_denied_warned {
    server_note(
      "public",
      "true",
      "false",
      "this client's token does not permit publishing public services, so the visitor auth gate stays on",
    );
  }
  if service.visitor_auth_denied_warned {
    server_note(
      "auth",
      "set",
      "ignored",
      "the token does not permit controlling the visitor gate, the value was not user:password, or the server sets APERIO_IGNORE_CLIENT_AUTH",
    );
  }
  let declared_hostnames = declared_hostnames_of(service);
  if !service.override_hostname_binds.is_empty() {
    let mut before = declared_hostnames.clone();
    for h in &service.assigned_hostnames {
      if !before.contains(h) {
        before.push(h.clone());
      }
    }
    server_note(
      "hostname",
      &before.join(", "),
      &service.override_hostname_binds.join(", "),
      "a dashboard overrule is in effect; it is in-memory only and reverts when the client reconnects",
    );
  }
  if let Some(path) = &service.override_path_bind {
    server_note(
      "path",
      service
        .declared_path
        .as_deref()
        .or(service.assigned_path.as_deref())
        .unwrap_or(""),
      path,
      "a dashboard overrule is in effect; it is in-memory only and reverts when the client reconnects",
    );
  }

  let mut y = String::new();
  y.push_str(&format!(
    "# Effective configuration of connection {}. Settings a client never\n\
     # announces (target, timeouts, header rules, health probes) are not shown.\n",
    id
  ));
  if let Some(name) = &service.service_name {
    push_line(&mut y, &notes, "name", &yaml_str(name));
  }
  if let Some(iid) = &handle.reported_instance_id {
    push_line(&mut y, &notes, "client_id", &yaml_str(iid));
  }
  // An elastic pool is written as the range the file wrote, with the size it
  // is running right now beside it. Printing only the current size read as a
  // fixed `connections: 3` next to four live connections, because the number
  // was a snapshot of a pool that had since grown.
  match (service.connections_min, service.connections_max) {
    (Some(min), Some(max)) => {
      let open = service
        .connections
        .map(|n| format!("  # {n} open right now"))
        .unwrap_or_default();
      push_line(
        &mut y,
        &notes,
        "connections",
        &format!("{{ min: {min}, max: {max} }}{open}"),
      );
    }
    _ => {
      if let Some(n) = service.connections {
        push_line(&mut y, &notes, "connections", &n.to_string());
      }
    }
  }

  // Hostnames, each labeled with where it came from; an active overrule
  // replaces the set, exactly as routing does.
  let effective_hosts: Vec<String> = service.effective_hostnames().into_iter().cloned().collect();
  if effective_hosts.is_empty() {
    push_line(&mut y, &notes, "hostname", "[]");
  } else {
    push_line(&mut y, &notes, "hostname", "");
    // `hostname:` above ends with the note comment, so the list follows it.
    for host in &effective_hosts {
      let origin = if !service.override_hostname_binds.is_empty() {
        "dashboard overrule"
      } else if declared_hostnames.contains(host) {
        "requested by the client"
      } else if service.random_hostname.as_deref() == Some(host.as_str()) {
        "random subdomain, assigned by the server"
      } else {
        "granted by the token"
      };
      y.push_str(&format!("  - {}  # {}\n", yaml_str(host), origin));
    }
  }
  // This service's bind, like every other line in this view. Read off the
  // connection it showed the first service's path for all of them, in the one
  // place an operator goes to check what a service is actually configured as.
  if let Some(path) = service.effective_path_bind() {
    push_line(&mut y, &notes, "path", &yaml_str(path));
  }
  if let Some(n) = service.max_concurrent {
    push_line(&mut y, &notes, "max_concurrent", &n.to_string());
  }
  match service.bandwidth_bps.load(Ordering::Relaxed) {
    0 => {}
    bps => push_line(
      &mut y,
      &notes,
      "bandwidth",
      &yaml_str(&format_bandwidth(bps)),
    ),
  }
  if service.priority > 0 {
    push_line(&mut y, &notes, "priority", &service.priority.to_string());
  }
  if service.public {
    push_line(&mut y, &notes, "public", "true");
  }
  if service.visitor_auth.is_some() {
    // The credentials themselves never leave the server.
    y.push_str("auth: \"<set by the client>\"\n");
  }
  if !service.allowed_ips.is_empty() {
    push_line(
      &mut y,
      &notes,
      "allowed_ips",
      &format!(
        "[{}]",
        service
          .allowed_ips
          .iter()
          .map(|ip| yaml_str(ip))
          .collect::<Vec<_>>()
          .join(", ")
      ),
    );
  }
  if let Some(denied) = &service.denied {
    push_line(&mut y, &notes, "denied", &yaml_str(denied));
  }
  if service.cache {
    push_line(&mut y, &notes, "cache", "true");
  }
  if service.resilience {
    push_line(&mut y, &notes, "resilience", "true");
  }
  if service.webhook_inbox {
    push_line(&mut y, &notes, "webhook_inbox", "true");
  }
  if let Some(n) = service.max_request_body {
    push_line(&mut y, &notes, "max_request_body", &n.to_string());
  }
  if let Some(n) = service.response_timeout {
    push_line(&mut y, &notes, "response_timeout", &n.to_string());
  }
  if service.tcp_enabled {
    y.push_str("tcp_target: \"<set by the client>\"\n");
  }
  if !service.tunnels.is_empty() {
    y.push_str("tunnels:\n");
    for t in &service.tunnels {
      y.push_str(&format!(
        "  - target: {}\n    protocol: {}\n",
        yaml_str(&t.target),
        yaml_str(&t.protocol)
      ));
      if t.encrypt {
        y.push_str("    encrypt: true\n");
      }
    }
  }

  ClientConfigView { yaml: y, notes }
}

/// Returns the effective configuration of one connected client.
#[utoipa::path(get, path = "/aperio/api/clients/{id}/config", tag = "dashboard",
  description = "Effective configuration of one connection as a YAML document, plus every setting whose effective value differs from what was configured (a bandwidth budget divided across parallel connections, a cache opt-in the server ignores, an active overrule).",
  params(("id" = String, Path, description = "Client connection id")),
  responses((status = 200, description = "Effective configuration", body = ClientConfigView),
    (status = 404, description = "No such client")))]
pub(crate) async fn client_config_handler(
  State(state): State<Arc<AppState>>,
  axum::extract::Path(client_id): axum::extract::Path<String>,
  axum::extract::Query(which): axum::extract::Query<ServiceQuery>,
  headers: HeaderMap,
) -> Response {
  // Org isolation, as everywhere else: a cross-org client is a 404, so its
  // existence never leaks.
  let org = crate::auth::effective_org(&state, &headers).await;
  let cache_enabled = state.config().cache_enabled;
  let clients = state.clients.read().await;
  match clients.get(&client_id) {
    Some(handle) if handle.perms.org_id == org => {
      // An index past the end is a service this connection does not carry,
      // which is a 404 for the same reason an unknown id is: the answer to
      // "show me that" is that there is no that.
      match handle.services.get(which.service) {
        Some(service) => Json(client_config_view(
          &client_id,
          handle,
          service,
          cache_enabled,
        ))
        .into_response(),
        None => (StatusCode::NOT_FOUND, "No such service on this client").into_response(),
      }
    }
    _ => (StatusCode::NOT_FOUND, "Client not found").into_response(),
  }
}

#[cfg(test)]
#[path = "config_view_tests.rs"]
mod tests;
