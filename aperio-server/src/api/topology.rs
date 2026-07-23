//! The routing map (`GET /api/topology`).
//!
//! Unlike the Clients table — which lists *connected* tunnel clients — this
//! endpoint answers "how is a request routed", including routing the server
//! itself owns with no tunnel client behind it: the client-less static
//! `routes:` (redirect / fixed response) and the experimental public `expose:`
//! TCP ports. Live clients are scoped to the caller's organization exactly like
//! `/api/stats`; the client-less, server-level routing is master-only.

use axum::{Json, extract::State, http::HeaderMap};
use serde::Serialize;
use std::sync::Arc;

use crate::state::{AppState, ClientDetail};

/// A client-less static route (the `routes:` section): a hostname/path that
/// resolves to a server-produced redirect or fixed response, no client involved.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct TopoStaticRoute {
  /// Hostname matched exactly (None = any host).
  pub(crate) hostname: Option<String>,
  /// Path prefix matched (None = any path).
  pub(crate) path: Option<String>,
  /// The action this route takes: `redirect` or `respond`.
  pub(crate) action: String,
  /// Redirect target URL (`redirect` action only).
  pub(crate) target: Option<String>,
  /// HTTP status the route answers with (301/302 for a redirect, the configured
  /// status for a fixed response).
  pub(crate) status: u16,
}

/// An experimental public TCP expose port (the `expose:` section). The shared
/// key is never serialized — only whether a connected client currently serves it.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct TopoExpose {
  /// Public port the server listens on.
  pub(crate) port: u16,
  /// Transport (`tcp` while experimental).
  pub(crate) protocol: String,
  /// True when a live, healthy client declares a tunnel with this expose key.
  pub(crate) served: bool,
  /// Id of the client connection currently serving this port (if any).
  pub(crate) served_by: Option<String>,
}

/// A hostname/path a token is permitted to bind, but which no live client
/// currently serves — a service that is declared (granted) yet offline.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct TopoOffline {
  /// The granted bind (hostname or path prefix).
  pub(crate) bind: String,
  /// Whether `bind` is a `hostname` or a `path`.
  pub(crate) kind: String,
  /// Name of a token that grants this bind.
  pub(crate) token_name: String,
}

/// The routing map: live tunnel clients plus the client-less routing the server
/// owns (static routes and public expose ports).
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct TopologyGraph {
  /// Connected tunnel clients, scoped to the caller's organization.
  pub(crate) clients: Vec<ClientDetail>,
  /// Client-less static routes (master organization only).
  pub(crate) routes: Vec<TopoStaticRoute>,
  /// Public TCP expose ports (master organization only).
  pub(crate) exposes: Vec<TopoExpose>,
  /// Token-granted binds no live client currently serves (declared but offline).
  pub(crate) offline: Vec<TopoOffline>,
}

/// Handler for the routing-map view.
#[utoipa::path(
  get,
  path = "/aperio/api/topology",
  tag = "dashboard",
  responses((status = 200, description = "Routing map: clients + client-less routes", body = TopologyGraph)))]
pub(crate) async fn topology_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
) -> Json<TopologyGraph> {
  let org = crate::auth::effective_org(&state, &headers).await;

  // Live tunnel clients, scoped to the caller's org (same rule as /api/stats).
  let mut snapshot = crate::api::clients::compute_stats(&state).await;
  crate::api::clients::filter_stats_for_org(&mut snapshot, &org);
  let clients = snapshot.active_clients;

  // Client-less routing is server-level (master) infrastructure; organization
  // dashboards only ever see their own tunnel clients.
  let (routes, exposes) = if org.is_none() {
    let cfg = state.config();

    let routes = cfg
      .static_routes
      .rules()
      .iter()
      .map(|r| {
        let (action, target, status) = if let Some(t) = &r.redirect {
          (
            "redirect",
            Some(t.clone()),
            if r.permanent { 301 } else { 302 },
          )
        } else {
          let status = r.respond.as_ref().map(|x| x.status).unwrap_or(200);
          ("respond", None, status)
        };
        TopoStaticRoute {
          hostname: r.hostname.clone(),
          path: r.path.clone(),
          action: action.to_string(),
          target,
          status,
        }
      })
      .collect();

    // Match each expose key to a currently-serving client — mirroring
    // `expose::find_declarer` — without ever leaking the key itself.
    let threshold = cfg.client_down_threshold;
    let live = state.clients.lock().await;
    let exposes = crate::expose::configured_rules()
      .into_iter()
      .map(|e| {
        let served_by = live.iter().find_map(|(cid, c)| {
          let serving = c.admin_enabled
            && !c.draining
            && c.is_healthy(threshold)
            && c
              .tunnels
              .iter()
              .any(|d| d.protocol == "tcp" && !d.encrypt && d.expose.as_deref() == Some(&e.key));
          serving.then(|| cid.clone())
        });
        TopoExpose {
          port: e.port,
          protocol: e.protocol,
          served: served_by.is_some(),
          served_by,
        }
      })
      .collect();

    (routes, exposes)
  } else {
    (Vec::new(), Vec::new())
  };

  // Declared-but-offline: hostnames/paths a token in this org may bind but that
  // no live client currently serves. Scoped per-org like the client list.
  let mut served: std::collections::HashSet<String> = std::collections::HashSet::new();
  for c in &clients {
    served.extend(c.hostname_binds.iter().cloned());
    if let Some(h) = &c.override_hostname_bind {
      served.insert(h.clone());
    }
    if let Some(p) = &c.path_bind {
      served.insert(p.clone());
    }
    if let Some(p) = &c.override_path_bind {
      served.insert(p.clone());
    }
  }
  let mut offline: Vec<TopoOffline> = Vec::new();
  let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
  {
    let store = state.token_store.lock().await;
    for tok in store
      .list()
      .iter()
      .filter(|t| t.org_id == org && !t.is_expired())
    {
      for (binds, kind) in [(&tok.hostnames, "hostname"), (&tok.paths, "path")] {
        for b in binds {
          // "*"/empty are wildcard grants, not a concrete expected service.
          if b == "*" || b.is_empty() || served.contains(b) {
            continue;
          }
          if seen.insert(format!("{kind}:{b}")) {
            offline.push(TopoOffline {
              bind: b.clone(),
              kind: kind.to_string(),
              token_name: tok.name.clone(),
            });
          }
        }
      }
    }
  }

  Json(TopologyGraph {
    clients,
    routes,
    exposes,
    offline,
  })
}
