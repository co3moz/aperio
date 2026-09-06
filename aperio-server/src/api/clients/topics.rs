//! The topics a dashboard may add to its event stream (`planned_features.md`
//! #167), so the pages that used to poll their endpoint every few seconds get
//! the same document pushed over the one connection the dashboard already
//! holds.
//!
//! Each topic is one of the polled endpoints, spelled the way its page asked
//! for it: `tokens` is `GET /aperio/api/tokens`, and the event the stream
//! sends under that name carries exactly the JSON that endpoint returns, for
//! the same caller, through the same organization fence. The handler itself
//! is what produces it, so there is one answer to "what does this list
//! contain" and not a second copy of the query.
//!
//! Two shapes of topic. A **change-driven** one is pushed when the store
//! behind it is written, which the server already knows since every write is
//! an audit event ([`Topic::touched_by`] is the map from event to topic); a
//! table then updates the moment a colleague edits it rather than up to
//! fifteen seconds later. A **tick-driven** one rides the stream's two-second
//! tick at a coarser cadence of its own, because what it shows changes
//! continuously and a push per change would be a push per request. A few are
//! both.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use std::sync::Arc;

use crate::state::AppState;
use crate::store::users::Role;

/// A topic of the stream, one per polled endpoint the dashboard has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Topic {
  Tokens,
  Users,
  Sessions,
  AdminKeys,
  Webhooks,
  Deliveries,
  Inbox,
  Maintenance,
  Scaling,
  Orgs,
  Subscribers,
  Topology,
  StageStats,
  Uptime,
  RouteTrends,
  SlowEndpoints,
  Tunnels,
  Audit,
  CacheStats,
  SelfHealth,
  /// The caller's own session: sent on connect, again when the users or
  /// sessions of its organization change (a grant taken away, a revocation),
  /// and on a slow cadence so the expiry the page shows stays honest.
  Session,
}

/// How a topic's document gets sent: on a store change, every `n` ticks of
/// the two-second timer, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cadence {
  OnChange,
  Ticks(u64),
  Both(u64),
}

/// A store changed under a topic. Sent on [`AppState::changes_tx`] by the
/// audit path and the few writes that are not audited; every open stream
/// that subscribed the topic re-sends its document.
#[derive(Debug, Clone)]
pub(crate) struct Change {
  pub(crate) topic: Topic,
  /// The organization whose rows changed (`None` = master). A stream sees
  /// the change when this is its own organization, or when `everyone` is
  /// set.
  pub(crate) org: Option<String>,
  /// The change concerns every organization's view (the organization list
  /// itself), not one organization's rows.
  pub(crate) everyone: bool,
}

impl Topic {
  pub(crate) const ALL: &'static [Topic] = &[
    Topic::Tokens,
    Topic::Users,
    Topic::Sessions,
    Topic::AdminKeys,
    Topic::Webhooks,
    Topic::Deliveries,
    Topic::Inbox,
    Topic::Maintenance,
    Topic::Scaling,
    Topic::Orgs,
    Topic::Subscribers,
    Topic::Topology,
    Topic::StageStats,
    Topic::Uptime,
    Topic::RouteTrends,
    Topic::SlowEndpoints,
    Topic::Tunnels,
    Topic::Audit,
    Topic::CacheStats,
    Topic::SelfHealth,
    Topic::Session,
  ];

  /// The name on the wire: the query parameter and the SSE event name.
  pub(crate) fn name(self) -> &'static str {
    match self {
      Topic::Tokens => "tokens",
      Topic::Users => "users",
      Topic::Sessions => "sessions",
      Topic::AdminKeys => "admin_keys",
      Topic::Webhooks => "webhooks",
      Topic::Deliveries => "deliveries",
      Topic::Inbox => "inbox",
      Topic::Maintenance => "maintenance",
      Topic::Scaling => "scaling",
      Topic::Orgs => "orgs",
      Topic::Subscribers => "subscribers",
      Topic::Topology => "topology",
      Topic::StageStats => "stage_stats",
      Topic::Uptime => "uptime",
      Topic::RouteTrends => "route_trends",
      Topic::SlowEndpoints => "slow_endpoints",
      Topic::Tunnels => "tunnels",
      Topic::Audit => "audit",
      Topic::CacheStats => "cache_stats",
      Topic::SelfHealth => "self_health",
      Topic::Session => "session",
    }
  }

  pub(crate) fn parse(name: &str) -> Option<Topic> {
    let name = name.trim();
    Topic::ALL.iter().copied().find(|t| t.name() == name)
  }

  /// The polled endpoint this topic stands in for, as the dashboard router
  /// sees it (without the `/aperio` prefix), which is what decides the role
  /// floor: the stream may not show a caller what the endpoint would refuse.
  pub(crate) fn path(self) -> &'static str {
    match self {
      Topic::Tokens => "/api/tokens",
      Topic::Users => "/api/users",
      Topic::Sessions => "/api/sessions",
      Topic::AdminKeys => "/api/admin-keys",
      Topic::Webhooks => "/api/webhooks",
      Topic::Deliveries => "/api/webhooks/deliveries",
      Topic::Inbox => "/api/inbox",
      Topic::Maintenance => "/api/maintenance",
      Topic::Scaling => "/api/scaling",
      Topic::Orgs => "/api/orgs",
      Topic::Subscribers => "/api/subscribers",
      Topic::Topology => "/api/topology",
      Topic::StageStats => "/api/stage-stats",
      Topic::Uptime => "/api/uptime",
      Topic::RouteTrends => "/api/route-trends",
      Topic::SlowEndpoints => "/api/slow-endpoints",
      Topic::Tunnels => "/api/tunnels",
      Topic::Audit => "/api/audit",
      Topic::CacheStats => "/api/cache/stats",
      Topic::SelfHealth => "/api/self-health",
      Topic::Session => "/api/session",
    }
  }

  /// Topics whose endpoint is the master super-admin's, over and above the
  /// role floor (the handlers check it too; this refuses at the upgrade with
  /// the topic named rather than sending nothing).
  pub(crate) fn master_only(self) -> bool {
    matches!(
      self,
      Topic::Orgs | Topic::AdminKeys | Topic::CacheStats | Topic::SelfHealth
    )
  }

  /// The role a caller needs for the topic: the same floor the dashboard
  /// router puts on the endpoint.
  pub(crate) fn required_role(self) -> Role {
    crate::required_role(self.path(), &axum::http::Method::GET)
  }

  pub(crate) fn cadence(self) -> Cadence {
    match self {
      Topic::Tokens
      | Topic::Users
      | Topic::Sessions
      | Topic::AdminKeys
      | Topic::Webhooks
      | Topic::Inbox
      | Topic::Maintenance
      | Topic::Orgs
      | Topic::Audit => Cadence::OnChange,
      // Written by the delivery worker, off the audit path; ten seconds is
      // what the page polled at.
      Topic::Deliveries => Cadence::Ticks(5),
      // Records change on an audited action and on every heartbeat that
      // re-arms one, and the utilization beside them moves continuously.
      Topic::Scaling => Cadence::Both(5),
      // Subscriptions come and go with connections, which are not audited.
      Topic::Subscribers => Cadence::Ticks(2),
      Topic::Topology => Cadence::Ticks(2),
      Topic::StageStats => Cadence::Ticks(2),
      Topic::Uptime => Cadence::Ticks(7),
      Topic::RouteTrends => Cadence::Ticks(7),
      Topic::SlowEndpoints => Cadence::Ticks(7),
      Topic::Tunnels => Cadence::Ticks(5),
      Topic::CacheStats => Cadence::Ticks(5),
      Topic::SelfHealth => Cadence::Ticks(5),
      // Once a minute keeps the expiry honest; changes arrive through
      // `users` and `sessions`.
      Topic::Session => Cadence::Both(30),
    }
  }

  /// Whether a tick numbered `tick` (0 on connect) sends this topic.
  pub(crate) fn due_on(self, tick: u64) -> bool {
    match self.cadence() {
      Cadence::OnChange => tick == 0,
      Cadence::Ticks(n) | Cadence::Both(n) => tick.is_multiple_of(n),
    }
  }

  /// The topics an audit event of this name invalidates.
  ///
  /// Every store write is audited, so the event name is the one place the
  /// server already says what changed. `audit` itself is touched by every
  /// event: the ring the page shows gained a row.
  pub(crate) fn touched_by(event: &str) -> Vec<Topic> {
    let mut out = vec![Topic::Audit];
    let mut add = |t: Topic| {
      if !out.contains(&t) {
        out.push(t);
      }
    };
    match event {
      "token_created" | "token_updated" | "token_revoked" | "token_rotated" | "token_refreshed"
      | "tunnel_created" | "tunnel_deleted" => add(Topic::Tokens),
      "user_created"
      | "user_updated"
      | "user_deleted"
      | "user_grant_added"
      | "user_grant_removed"
      | "grants_widened_on_upgrade"
      | "totp_enabled"
      | "totp_disabled"
      | "totp_admin_reset"
      | "passkey_registered"
      | "passkey_deleted" => add(Topic::Users),
      "login_success" | "oidc_login_success" | "session_revoked" | "sessions_cleared" => {
        add(Topic::Sessions)
      }
      "admin_key_created" | "admin_key_revoked" => add(Topic::AdminKeys),
      "webhook_created" | "webhook_deleted" => add(Topic::Webhooks),
      "webhook_tested" | "webhook_redelivered" => add(Topic::Deliveries),
      "webhook_refired" => add(Topic::Inbox),
      "maintenance_on" | "maintenance_off" => add(Topic::Maintenance),
      "scaling_requested" | "scaling_failed" | "scaling_disarmed" => add(Topic::Scaling),
      "org_created" | "org_renamed" | "org_deleted" | "org_hostnames_set" | "org_panel_set"
      | "org_quota_updated" | "org_oidc_updated" => add(Topic::Orgs),
      "import_applied" => {
        for t in [
          Topic::Tokens,
          Topic::Users,
          Topic::Webhooks,
          Topic::Orgs,
          Topic::Scaling,
          Topic::Inbox,
          Topic::AdminKeys,
        ] {
          add(t);
        }
      }
      "cache_purged" => add(Topic::CacheStats),
      _ => {}
    }
    out
  }

  /// Whether a change to this topic is every organization's business.
  pub(crate) fn changes_everyone(self) -> bool {
    matches!(self, Topic::Orgs)
  }

  /// The document the topic's endpoint would answer this caller with, or
  /// `None` when the endpoint would refuse (the stream then sends nothing,
  /// the caller having been admitted at the upgrade already).
  pub(crate) async fn payload(
    self,
    state: &Arc<AppState>,
    headers: &HeaderMap,
  ) -> Option<serde_json::Value> {
    let s = State(state.clone());
    let h = headers.clone();
    match self {
      Topic::Tokens => to_value(crate::api::tokens::tokens_list_handler(s, h).await.0),
      Topic::Users => to_value(crate::api::users::users_list_handler(s, h).await.0),
      Topic::Sessions => to_value(crate::api::users::sessions_list_handler(s, h).await.0),
      Topic::AdminKeys => {
        body_json(crate::api::admin_keys::admin_keys_list_handler(s, h).await).await
      }
      Topic::Webhooks => to_value(crate::api::webhooks::webhooks_list_handler(s, h).await.0),
      Topic::Deliveries => to_value(
        crate::api::webhooks::webhook_deliveries_handler(
          s,
          h,
          axum::extract::Query(Default::default()),
        )
        .await
        .0,
      ),
      Topic::Inbox => to_value(crate::api::inbox::inbox_list_handler(s, h).await.0),
      Topic::Maintenance => to_value(
        crate::api::maintenance::maintenance_list_handler(s, h)
          .await
          .0,
      ),
      Topic::Scaling => body_json(crate::api::scaling::scaling_list_handler(s, h).await).await,
      Topic::Orgs => body_json(crate::api::orgs::orgs_list_handler(s, h).await).await,
      Topic::Subscribers => to_value(crate::api::publish::subscribers_handler(s, h).await.0),
      Topic::Topology => to_value(crate::api::topology::topology_handler(s, h).await.0),
      Topic::StageStats => to_value(crate::api::metrics::stage_stats_handler(s, h).await.0),
      Topic::Uptime => to_value(super::numbers::uptime_handler(s, h).await.0),
      Topic::RouteTrends => to_value(crate::api::metrics::route_trends_handler(s, h).await.0),
      Topic::SlowEndpoints => to_value(crate::api::metrics::slow_endpoints_handler(s, h).await.0),
      // The endpoint prices itself from the caller's IP bucket before auth,
      // which is right for a request and wrong for a document sent every ten
      // seconds to a caller the stream already admitted; the listing itself
      // is what the page shows.
      Topic::Tunnels => {
        let org = crate::auth::effective_org(state, headers).await;
        to_value(crate::tunnel::registry::visible_in_org(state, org.as_deref()).await)
      }
      Topic::Audit => to_value(
        crate::api::webhooks::audit_handler(s, h, axum::extract::Query(Default::default()))
          .await
          .0,
      ),
      Topic::CacheStats => body_json(crate::api::purge::cache_stats_handler(s, h).await).await,
      Topic::SelfHealth => body_json(crate::api::observe::self_health_handler(s, h).await).await,
      Topic::Session => body_json(crate::auth::session::auth_session_handler(s, h).await).await,
    }
  }
}

fn to_value<T: serde::Serialize>(value: T) -> Option<serde_json::Value> {
  serde_json::to_value(value).ok()
}

/// The JSON body of a successful response; `None` for a refusal.
async fn body_json(resp: Response) -> Option<serde_json::Value> {
  if !resp.status().is_success() {
    return None;
  }
  // The largest of these documents is the audit ring; eight megabytes is a
  // ceiling nothing here approaches, and a bound is what keeps a handler
  // that answered with the world from being buffered whole.
  let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
    .await
    .ok()?;
  serde_json::from_slice(&bytes).ok()
}

/// Reads the topics a stream request asks for: `?topics=a,b` and the bare
/// `?a&b` spelling, either or both. Unknown names are the error, by name.
pub(crate) fn topics_from_query(
  params: &std::collections::HashMap<String, String>,
) -> Result<Vec<Topic>, String> {
  let mut out = Vec::new();
  let mut unknown = Vec::new();
  let mut take = |raw: &str| {
    let raw = raw.trim();
    if raw.is_empty() {
      return;
    }
    match Topic::parse(raw) {
      Some(t) => {
        if !out.contains(&t) {
          out.push(t);
        }
      }
      None => unknown.push(raw.to_string()),
    }
  };
  if let Some(list) = params.get("topics") {
    for raw in list.split(',') {
      take(raw);
    }
  }
  for key in params.keys() {
    if key != "topics" {
      take(key);
    }
  }
  if unknown.is_empty() {
    Ok(out)
  } else {
    Err(format!(
      "unknown stream topic(s): {}; the topics are {}",
      unknown.join(", "),
      Topic::ALL
        .iter()
        .map(|t| t.name())
        .collect::<Vec<_>>()
        .join(", ")
    ))
  }
}

#[cfg(test)]
#[path = "topics_tests.rs"]
mod tests;
