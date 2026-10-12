//! One route, one policy.
//!
//! Several clients may serve the same route, and they used to be allowed to
//! say different things about it: `public` held only when every one of them
//! said so, a gate applied only when they all declared the same one and fell
//! back to the server's otherwise, and each candidate filtered visitors by its
//! own `allowed_ips`. Every request resolved the disagreement again, and the
//! answer depended on who happened to be connected.
//!
//! Now the disagreement is settled once, when a client declares: a service
//! whose policy for a route differs from one already serving that route is
//! refused, kept out of routing, and told why in its own log. It is asked again
//! on every heartbeat, so it serves as soon as the disagreement is gone, either
//! because it changed or because the other side left. The policy is the
//! route's `access`, `allowed_ips` and `denied`; where a site's `access` covers
//! the route, the site decides who reaches it and only the other two have to
//! agree.

use std::collections::HashMap;

use crate::state::{AppState, ClientHandle, ServiceState};

/// What a service says about who reaches its route.
#[derive(PartialEq)]
struct RoutePolicy<'a> {
  public: bool,
  visitor_auth: Option<&'a str>,
  gate: Option<&'a crate::visitor_auth::Policy>,
  allowed_ips: &'a [String],
  denied: Option<&'a str>,
}

impl<'a> RoutePolicy<'a> {
  fn of(s: &'a ServiceState) -> Self {
    RoutePolicy {
      public: s.public,
      visitor_auth: s.visitor_auth.as_deref(),
      gate: s.visitor_auth_policy.as_ref(),
      allowed_ips: &s.allowed_ips,
      denied: s.denied.as_deref(),
    }
  }

  /// The settings these two disagree on, by name. `access` is left out where
  /// a site decides it.
  fn differences(&self, other: &RoutePolicy, site_decides: bool) -> Vec<&'static str> {
    let mut out = Vec::new();
    if !site_decides
      && (self.public != other.public
        || self.visitor_auth != other.visitor_auth
        || self.gate != other.gate)
    {
      out.push("access");
    }
    if self.allowed_ips != other.allowed_ips {
      out.push("allowed_ips");
    }
    if self.denied != other.denied {
      out.push("denied");
    }
    out
  }
}

/// The route two services both serve, as `host/path`, if they share one: the
/// same path bind, and a hostname in common or neither bound to any.
fn shared_route(a: &ServiceState, b: &ServiceState) -> Option<(Option<String>, String)> {
  let path = a.effective_path_bind();
  if path != b.effective_path_bind() {
    return None;
  }
  let path = path.cloned().unwrap_or_default();
  let (ha, hb) = (a.effective_hostnames(), b.effective_hostnames());
  if ha.is_empty() && hb.is_empty() {
    return Some((None, path));
  }
  ha.iter()
    .find(|h| hb.contains(h))
    .map(|h| (Some((*h).clone()), path))
}

/// A refusal for one service of the connection being settled.
pub(super) struct Refusal {
  pub(super) service: Option<String>,
  pub(super) message: String,
}

/// Settles the services of `client_id` against every other connection's,
/// marking a service that disagrees with what already serves its route and
/// clearing one that no longer does. Answers the refusals that are new, for
/// the caller to tell the client.
pub(super) fn settle(
  state: &AppState,
  clients: &mut HashMap<String, ClientHandle>,
  client_id: &str,
) -> Vec<Refusal> {
  let Some(this) = clients.get(client_id) else {
    return Vec::new();
  };
  let cfg = state.config();
  let verdicts: Vec<Option<String>> = this
    .services
    .iter()
    .map(|s| {
      let mine = RoutePolicy::of(s);
      clients
        .iter()
        .filter(|(id, c)| id.as_str() != client_id && !c.awaiting_declaration())
        .flat_map(|(id, c)| c.services.iter().map(move |t| (id, c, t)))
        // Only a service that is itself serving the route speaks for it.
        .filter(|(_, _, t)| t.policy_conflict.is_none())
        .find_map(|(id, c, t)| {
          let (host, path) = shared_route(s, t)?;
          let site_decides = cfg
            .sites
            .access_for(host.as_deref(), if path.is_empty() { "/" } else { &path })
            .is_some();
          let differ = mine.differences(&RoutePolicy::of(t), site_decides);
          if differ.is_empty() {
            return None;
          }
          let who = t
            .display_name()
            .or_else(|| c.process_name())
            .unwrap_or_else(|| id.clone());
          let route = format!("{}{}", host.as_deref().unwrap_or("every hostname, "), path);
          Some(format!(
            "not serving {route}: its `{}` differs from {who}, which already serves it, and one \
             route has one policy. Write the same `{}` on both, or give this service a hostname \
             or path of its own",
            differ.join("`, `"),
            differ.join("`, `"),
          ))
        })
    })
    .collect();

  let Some(this) = clients.get_mut(client_id) else {
    return Vec::new();
  };
  let mut refusals = Vec::new();
  for (s, verdict) in this.services.iter_mut().zip(verdicts) {
    match (&s.policy_conflict, verdict) {
      (None, Some(why)) => {
        tracing::warn!("Client {client_id}: {why}");
        refusals.push(Refusal {
          service: s.service_name.clone(),
          message: why.clone(),
        });
        s.policy_conflict = Some(why);
      }
      (Some(_), None) => {
        tracing::info!(
          "Client {client_id}: {} agrees with its route again and serves it",
          s.display_name()
            .unwrap_or_else(|| "its service".to_string())
        );
        s.policy_conflict = None;
      }
      (Some(old), Some(why)) if *old != why => {
        refusals.push(Refusal {
          service: s.service_name.clone(),
          message: why.clone(),
        });
        s.policy_conflict = Some(why);
      }
      _ => {}
    }
  }
  refusals
}

#[cfg(test)]
#[path = "agree_tests.rs"]
mod tests;
