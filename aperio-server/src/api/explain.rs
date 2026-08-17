//! `GET /aperio/api/explain`: why a request to a hostname and path would be
//! answered the way it would.
//!
//! The proxy makes a dozen decisions before a byte reaches a backend, and
//! when the answer surprises someone the only record is a log line that says
//! what happened, not what would happen. "Why is this hostname 503ing" then
//! costs a reproduction, a log grep, and a guess. This walks the same
//! decisions in the same order, on a request nobody sends, and reports which
//! one decides and what the rest saw.
//!
//! It is deliberately a *dry run*: nothing here consumes a rate-limit token,
//! rotates a round-robin cursor, or wakes a scaled-to-zero service. Where a
//! stage's real check is destructive, the report says what the rule is rather
//! than what the check would return, and says so.

use axum::{
  Json,
  extract::{Query, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::state::AppState;

/// The limits that belong to whoever would serve the request, rather than to
/// the server or the route. Read off the first candidate while the client map
/// is open, because every stage below that needs one of these would otherwise
/// reopen it.
struct Serving {
  max_concurrent: Option<u32>,
  cache: bool,
  restricts_source_ips: bool,
  max_request_body: Option<u64>,
}

/// What to explain. `hostname` may also be given as a full URL, which is what
/// someone has in their clipboard when they come here.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct ExplainQuery {
  /// Hostname, or a full URL (`https://app.example.com/api/x`), in which case
  /// the path comes from it too.
  hostname: String,
  /// Request path; defaults to `/`.
  path: Option<String>,
  /// Request method; defaults to GET.
  method: Option<String>,
}

/// What a stage did to the request.
#[derive(Serialize, utoipa::ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Verdict {
  /// This stage answers the request; nothing after it runs.
  Decides,
  /// The stage looked and let the request through.
  Passes,
  /// The stage is switched off, or does not apply to this request.
  Skipped,
}

/// One decision, in the order the proxy makes it.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct Step {
  /// Stable machine-readable stage name.
  pub(crate) stage: &'static str,
  pub(crate) verdict: Verdict,
  /// One sentence for a person, in English.
  ///
  /// Kept for everything that reads this endpoint without a phrasebook: the
  /// `api` commands, a script, a dashboard whose locale has no entry yet. A
  /// caller that can render `code` should prefer it, and fall back here.
  pub(crate) detail: String,
  /// What `detail` says, as something other than English.
  ///
  /// A stage has several possible messages, so this names the message rather
  /// than the stage: `maintenance.flagged` and `maintenance.none` are both
  /// `stage: "maintenance"`. Stable, like `stage` and `verdict`.
  pub(crate) code: &'static str,
  /// The values `detail` interpolates, unformatted: numbers as numbers, lists
  /// as lists. A renderer needs the parts, not a sentence it has to take
  /// apart again.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) params: Option<serde_json::Value>,
  /// Where the rule behind it lives, when it has a home an operator edits.
  ///
  /// A literal config key (`routes:`, `waf:`) where there is one, and a
  /// `setting_code` beside it where the answer is prose rather than a key.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) setting: Option<&'static str>,
  /// Set when `setting` names something in prose rather than a config key, so
  /// a dashboard can translate it. Absent means `setting` is the key itself.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) setting_code: Option<&'static str>,
}

impl Step {
  fn new(
    stage: &'static str,
    verdict: Verdict,
    code: &'static str,
    detail: impl Into<String>,
  ) -> Self {
    Step {
      stage,
      verdict,
      detail: detail.into(),
      code,
      params: None,
      setting: None,
      setting_code: None,
    }
  }

  /// The values behind the sentence.
  fn with(mut self, params: serde_json::Value) -> Self {
    self.params = Some(params);
    self
  }

  /// A config key, which is the same word in every language.
  fn from(mut self, setting: &'static str) -> Self {
    self.setting = Some(setting);
    self
  }

  /// A place named in prose, which is not.
  // `from_*` usually means a constructor; here it is the same builder as
  // `from` above with a second name, and reading `.from_named(...)` next to
  // `.from(...)` is worth more than the convention.
  #[allow(clippy::wrong_self_convention)]
  fn from_named(mut self, setting: &'static str, code: &'static str) -> Self {
    self.setting = Some(setting);
    self.setting_code = Some(code);
    self
  }
}

/// The full report.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct Explanation {
  pub(crate) hostname: String,
  pub(crate) path: String,
  pub(crate) method: String,
  /// The stage that decides, or `none` when the request would reach a client.
  pub(crate) outcome: String,
  /// The one-line answer, the thing someone came here to read, in English.
  pub(crate) summary: String,
  /// The same answer as a message name, for a caller that renders its own.
  pub(crate) summary_code: &'static str,
  /// The values `summary` interpolates.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) summary_params: Option<serde_json::Value>,
  pub(crate) steps: Vec<Step>,
}

/// Splits a hostname-or-URL into a hostname and, when it carried one, a path.
fn split_target(raw: &str) -> (String, Option<String>) {
  let trimmed = raw.trim();
  let without_scheme = trimmed
    .strip_prefix("https://")
    .or_else(|| trimmed.strip_prefix("http://"))
    .unwrap_or(trimmed);
  match without_scheme.find('/') {
    Some(i) => {
      let (host, path) = without_scheme.split_at(i);
      let path = path.split('?').next().unwrap_or(path);
      (host.to_string(), Some(path.to_string()))
    }
    None => (without_scheme.to_string(), None),
  }
}

/// The name of the organization a client belongs to, as it is addressed.
/// `None` is the master organization, which is spelled out rather than left
/// blank: `master@axum` is an address, `@axum` is a typo.
fn org_label(org_id: Option<&str>, names: &HashMap<String, String>) -> String {
  org_id
    .and_then(|id| names.get(id).cloned())
    .unwrap_or_else(|| "master".to_string())
}

/// One connection, as far as this report is concerned.
struct Conn {
  id: String,
  name: Option<String>,
  org: Option<String>,
}

/// One line of the report: what a client is called, how many connections
/// answer to that name, and which ones they are.
struct Named {
  /// `org@name`, or `None` for a client the caller may not be told about.
  label: Option<String>,
  count: usize,
  /// Empty when the caller may not name the client: an id identifies it as
  /// surely as a name does.
  ids: Vec<String>,
}

impl Named {
  /// The entry as it reads in `detail`: bare when it is one connection.
  fn text(&self) -> String {
    let label = self
      .label
      .clone()
      .unwrap_or_else(|| "another organization's client".to_string());
    if self.count == 1 {
      label
    } else {
      format!("{label} \u{00d7}{}", self.count)
    }
  }
}

/// Collapses the connections into the things a person would count.
///
/// Two jobs, and they are the same walk. `connections: 3` is one service
/// holding three sockets, so equal names become one entry with a count
/// rather than the same word three times; and a client outside the caller's
/// organization is counted without being named, because a hostname the
/// caller's fence covers is not permission to learn who else serves it.
///
/// A client that announced no name is keyed by its id, which makes it a
/// group of one. `viewer` of `None` is the master organization, which may
/// see everything.
fn group(
  entries: &[Conn],
  viewer: Option<&str>,
  org_names: &HashMap<String, String>,
) -> Vec<Named> {
  let mut out: Vec<Named> = Vec::new();
  for conn in entries {
    let may_name = viewer.is_none() || viewer == conn.org.as_deref();
    let label = may_name.then(|| {
      format!(
        "{}@{}",
        org_label(conn.org.as_deref(), org_names),
        conn.name.clone().unwrap_or_else(|| conn.id.clone())
      )
    });
    match out.iter_mut().find(|n| n.label == label) {
      Some(existing) => {
        existing.count += 1;
        if may_name {
          existing.ids.push(conn.id.clone());
        }
      }
      None => out.push(Named {
        count: 1,
        ids: if may_name {
          vec![conn.id.clone()]
        } else {
          Vec::new()
        },
        label,
      }),
    }
  }
  out
}

/// One grouped entry as the caller receives it.
fn named_json(n: &Named) -> serde_json::Value {
  match &n.label {
    Some(label) => serde_json::json!({ "label": label, "count": n.count, "ids": n.ids }),
    // No label, no ids: the count is the whole of what may be said.
    None => serde_json::json!({ "label_code": "client.other_org", "count": n.count }),
  }
}

/// Walks the proxy's decisions for a request nobody sends.
#[utoipa::path(get, path = "/aperio/api/explain", tag = "dashboard",
  description = "Dry run: which rule would answer a request to this hostname and path, and what every other stage saw. Consumes no rate limit and wakes nothing.",
  params(
    ("hostname" = String, Query, description = "Hostname, or a full URL"),
    ("path" = Option<String>, Query, description = "Request path (default /)"),
    ("method" = Option<String>, Query, description = "Request method (default GET)"),
  ),
  responses((status = 200, description = "The decision chain", body = Explanation),
            (status = 400, description = "Unusable hostname"),
            (status = 403, description = "Outside the caller's organization")))]
pub(crate) async fn explain_handler(
  State(state): State<Arc<AppState>>,
  headers: HeaderMap,
  Query(query): Query<ExplainQuery>,
) -> Response {
  // Operator and up: the report names the clients serving a hostname and the
  // rules around it, which is more than a viewer's read-only traffic view.
  let role = crate::auth::dashboard_role(&state, &headers).await;
  match role {
    None => {
      return (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
    }
    Some(role) if role < crate::store::users::Role::Operator => {
      return (StatusCode::FORBIDDEN, "Operator role required").into_response();
    }
    Some(_) => {}
  }
  let (raw_host, path_from_url) = split_target(&query.hostname);
  let Some(hostname) = crate::routing::normalize_hostname_bind(&raw_host) else {
    return (
      StatusCode::BAD_REQUEST,
      format!("Invalid hostname: {}", raw_host),
    )
      .into_response();
  };
  let path = query
    .path
    .or(path_from_url)
    .map(|p| p.trim().to_string())
    .filter(|p| p.starts_with('/'))
    .unwrap_or_else(|| "/".to_string());
  let method = query
    .method
    .unwrap_or_else(|| "GET".into())
    .trim()
    .to_ascii_uppercase();

  // A tenant may only ask about its own hostnames: the answer names the
  // clients serving them, which is exactly what org isolation hides.
  let org = crate::auth::effective_org(&state, &headers).await;
  if !state
    .org_may_claim_hostname(org.as_deref(), &hostname)
    .await
  {
    return (
      StatusCode::FORBIDDEN,
      "that hostname is not served by your organization",
    )
      .into_response();
  }

  // Read once: every client named below needs the name of its organization,
  // and the report is one pass over the client map.
  let org_names: HashMap<String, String> = state
    .org_store
    .lock()
    .await
    .list()
    .iter()
    .map(|o| (o.id.clone(), o.name.clone()))
    .collect();

  let mut steps: Vec<Step> = Vec::new();
  let mut outcome: Option<Decision> = None;
  // Once something decides, later stages still report what they see, marked
  // as not reached: half the value here is "the route is fine, the
  // maintenance flag is what is answering".
  type Decision = (String, String, &'static str, Option<serde_json::Value>);
  let decided = |outcome: &mut Option<Decision>,
                 stage: &str,
                 summary: String,
                 code: &'static str,
                 params: Option<serde_json::Value>| {
    if outcome.is_none() {
      *outcome = Some((stage.to_string(), summary, code, params));
    }
  };
  let cfg = state.config();

  // 0. The source-IP deny list, which is middleware in `server/router.rs` and
  // so runs before any of this. Reported as a rule rather than a verdict: this
  // report is about a hostname and a path, and the deny list is about who is
  // asking, which nobody is here.
  if cfg.denied_ips.is_empty() {
    steps.push(Step::new(
      "denied_ips",
      Verdict::Skipped,
      "denied_ips.none",
      "no source-IP deny list is configured",
    ));
  } else {
    steps.push(
      Step::new(
        "denied_ips",
        Verdict::Passes,
        "denied_ips.configured",
        format!(
          "a source-IP deny list is in force ({} entr{}); a request from a listed address is refused before anything below runs, whatever this report says",
          cfg.denied_ips.len(),
          if cfg.denied_ips.len() == 1 { "y" } else { "ies" }
        ),
      )
      .with(serde_json::json!({ "count": cfg.denied_ips.len() }))
      .from("denied_ips:"),
    );
  }

  // 1. Maintenance mode, which wins over everything.
  match state.maintenance_for(Some(&hostname)).await {
    Some(flag) => {
      let mut detail = format!(
        "503: a maintenance flag set by {} covers this hostname",
        flag.actor
      );
      if let Some(reason) = &flag.reason {
        detail.push_str(&format!(" ({reason})"));
      }
      match flag.until {
        Some(until) => detail.push_str(&format!(", lifting at unix {until}")),
        None => detail.push_str(", until someone turns it off"),
      }
      let code = match (flag.reason.is_some(), flag.until.is_some()) {
        (true, true) => "maintenance.flagged_reason_until",
        (true, false) => "maintenance.flagged_reason",
        (false, true) => "maintenance.flagged_until",
        (false, false) => "maintenance.flagged",
      };
      let params = serde_json::json!({
        "actor": flag.actor,
        "reason": flag.reason,
        "until": flag.until,
      });
      decided(
        &mut outcome,
        "maintenance",
        detail.clone(),
        code,
        Some(params.clone()),
      );
      steps.push(
        Step::new("maintenance", Verdict::Decides, code, detail)
          .with(params)
          .from_named("maintenance mode", "setting.maintenance_mode"),
      );
    }
    None => steps.push(Step::new(
      "maintenance",
      Verdict::Passes,
      "maintenance.none",
      "no maintenance flag covers this hostname",
    )),
  }

  // 2. Client-less routes.
  if cfg.static_routes.is_empty() {
    steps.push(Step::new(
      "static_route",
      Verdict::Skipped,
      "static_route.none_configured",
      "no routes: rules configured",
    ));
  } else if let Some(answer) = cfg.static_routes.answer(Some(&hostname), &path, None) {
    let status = answer.status();
    let location = answer
      .headers()
      .get("location")
      .and_then(|v| v.to_str().ok())
      .map(|l| format!(" to {l}"))
      .unwrap_or_default();
    let detail = format!("{status}: a routes: rule answers this path{location}");
    let target = answer
      .headers()
      .get("location")
      .and_then(|v| v.to_str().ok());
    let code = match target {
      Some(_) => "static_route.answers_location",
      None => "static_route.answers",
    };
    let params = serde_json::json!({ "status": status.as_u16(), "location": target });
    decided(
      &mut outcome,
      "static_route",
      detail.clone(),
      code,
      Some(params.clone()),
    );
    steps.push(
      Step::new("static_route", Verdict::Decides, code, detail)
        .with(params)
        .from("routes:"),
    );
  } else {
    steps.push(Step::new(
      "static_route",
      Verdict::Passes,
      "static_route.no_match",
      "no routes: rule matches this hostname and path",
    ));
  }

  // 3. Preview noindex, which only ever answers /robots.txt.
  let noindex = cfg.preview_noindex
    && path == "/robots.txt"
    && cfg
      .random_subdomain_suffix
      .as_deref()
      .is_some_and(|pattern| crate::routing::host_matches_random_pattern(&hostname, pattern));
  if noindex {
    let detail = "200: a disallow-all robots.txt, because this is a random-subdomain host and preview_noindex is on".to_string();
    decided(
      &mut outcome,
      "preview_noindex",
      detail.clone(),
      "preview_noindex.robots",
      None,
    );
    steps.push(
      Step::new(
        "preview_noindex",
        Verdict::Decides,
        "preview_noindex.robots",
        detail,
      )
      .from("preview_noindex"),
    );
  }

  // 4. WAF deny rules. Header rules cannot be judged without a real request,
  // so the report says so rather than implying the path is clean.
  if cfg.waf.is_empty() {
    steps.push(Step::new(
      "waf",
      Verdict::Skipped,
      "waf.none_configured",
      "no waf: rules configured",
    ));
  } else if let Some(reason) = cfg.waf.deny_reason(&method, &path, &HeaderMap::new()) {
    let detail = format!("403: blocked by a waf: rule ({reason})");
    let params = serde_json::json!({ "reason": reason });
    decided(
      &mut outcome,
      "waf",
      detail.clone(),
      "waf.denied",
      Some(params.clone()),
    );
    steps.push(
      Step::new("waf", Verdict::Decides, "waf.denied", detail)
        .with(params)
        .from("waf:"),
    );
  } else {
    steps.push(
      Step::new(
        "waf",
        Verdict::Passes,
        "waf.no_match",
        "no waf: rule matches this method and path (header and body rules need a real request)",
      )
      .from("waf:"),
    );
  }

  // 5. Per-route rate limit. Reported as the rule, never consumed: asking
  // why a request is refused must not spend the budget it is asking about.
  match cfg.route_limits.matched(Some(&hostname), &path, None) {
    Some(rule) => steps.push(
      Step::new(
        "route_rate_limit",
        Verdict::Passes,
        match rule.methods.as_ref() {
          Some(_) => "route_rate_limit.covered_methods",
          None => "route_rate_limit.covered",
        },
        format!(
          "a rate_limits: rule covers this path ({} rps, burst {}{}); this dry run does not spend from it",
          rule.rps,
          rule.burst,
          match rule.methods.as_ref() {
            Some(m) => format!(", {} only", m.join("/")),
            None => String::new(),
          }
        ),
      )
      .with(serde_json::json!({
        "rps": rule.rps,
        "burst": rule.burst,
        "methods": rule.methods.as_ref().map(|m| m.join("/")),
      }))
      .from("rate_limits"),
    ),
    // Still names the key it looked in. A stage that says "nothing here"
    // without saying where it looked leaves the reader to guess which of the
    // eight limits it was about.
    None => steps.push(
      Step::new(
        "route_rate_limit",
        Verdict::Skipped,
        "route_rate_limit.none",
        "no rate_limits: rule covers this path",
      )
      .from(crate::limits::Limit::Route.setting()),
    ),
  }

  // 5b. The per-visitor token bucket. Reported, never spent: charging this
  // dry run would make the explainer part of the thing it explains, and
  // somebody debugging a 429 would be adding to it with every refresh.
  steps.push(
    Step::new(
      "rate_limit_ip",
      Verdict::Passes,
      "rate_limit_ip.configured",
      format!(
        "every visitor IP gets a bucket of {} with {}/s refill; this dry run does not spend from it",
        cfg.ip_limit_max, cfg.ip_limit_refill
      ),
    )
    .with(serde_json::json!({
      "max": cfg.ip_limit_max,
      "refill": cfg.ip_limit_refill,
    }))
    .from(crate::limits::Limit::Ip.setting()),
  );

  // 6. The visitor gate. Three things can raise it, and naming which one is
  // the whole value: a client's own password is the service's, the server's
  // password and OIDC are the operator's, and they are configured in
  // different places.
  let client_gate = crate::routing::host_has_visitor_auth(&state, Some(&hostname)).await;
  let server_gate = cfg.visitor_auth.gates();
  let oidc_gate = state.oidc.is_some();
  if client_gate || server_gate || oidc_gate {
    let (why, code, setting, setting_code) = if client_gate {
      (
        "the serving client declared a visitor password for this route, which supersedes the server's own gate",
        "visitor_gate.client_password",
        "auth: on the service",
        "setting.service_auth",
      )
    } else if server_gate && oidc_gate {
      (
        "the server's visitor gate is on, and OIDC is configured",
        "visitor_gate.server_password_and_oidc",
        "server_auth / OIDC",
        "setting.server_auth_oidc",
      )
    } else if server_gate {
      (
        "the server's visitor password is set",
        "visitor_gate.server_password",
        "server_auth",
        "setting.server_auth",
      )
    } else {
      (
        "OIDC is configured for visitors",
        "visitor_gate.oidc",
        "OIDC",
        "setting.oidc",
      )
    };
    steps.push(
      Step::new(
        "visitor_gate",
        Verdict::Passes,
        code,
        format!(
          "visitors must sign in (or carry a share link) before this reaches a client: {why}"
        ),
      )
      .from_named(setting, setting_code),
    );
  } else {
    steps.push(Step::new(
      "visitor_gate",
      Verdict::Skipped,
      "visitor_gate.open",
      "this hostname is served without a visitor gate",
    ));
  }

  // 6b. The server's own in-flight ceiling, across every service. Unlike the
  // rules above this is a live number, so the report is what it is right now
  // rather than what it would be when somebody actually sends the request.
  {
    let in_flight = state
      .active_proxied_requests
      .load(std::sync::atomic::Ordering::Relaxed);
    steps.push(
      Step::new(
        "server_concurrency",
        Verdict::Passes,
        "server_concurrency.headroom",
        format!(
          "{} of {} server-wide request slots are in use right now; a request arriving with none free is refused rather than queued",
          in_flight, cfg.max_concurrent_requests
        ),
      )
      .with(serde_json::json!({
        "in_flight": in_flight,
        "max": cfg.max_concurrent_requests,
      }))
      .from(crate::limits::Limit::ServerConcurrency.setting()),
    );
  }

  // 7. Routing: which clients could take it, and why the others could not.
  let (pool, ineligible, serving) = {
    let clients = state.clients.read().await;
    let down_threshold = cfg.client_down_threshold;
    let pool = crate::routing::select_client_pool(
      &clients,
      &path,
      Some(&hostname),
      cfg.require_hostname_bind,
      down_threshold,
    )
    .map(|(ids, _)| ids)
    .unwrap_or_default();
    let pool: Vec<Conn> = pool
      .into_iter()
      .map(|r| {
        // The name is the service's, the organization the connection's.
        Conn {
          name: r.get(&clients).and_then(|s| s.display_name()),
          org: r.connection(&clients).and_then(|c| c.perms.org_id.clone()),
          id: r.client,
        }
      })
      .collect();
    // Every connected client that serves this hostname but would not take
    // the request, with the reason, which is the question behind most 504s.
    //
    // The reason is asked of the *service* that serves the hostname, not of
    // the connection: on a connection carrying several, the first one's kill
    // switch and backend health answered for a service that had neither
    // problem, which is precisely the wrong answer to give somebody who came
    // here to find out why their route is not working.
    let ineligible: Vec<(Conn, &'static str, &'static str)> = clients
      .iter()
      .filter_map(|(id, c)| {
        if pool.iter().any(|p| p.id == *id) {
          return None;
        }
        let service = c.services.iter().find(|s| s.matches_host(&hostname))?;
        let (why, code) = if !service.admin_enabled {
          ("disabled from the dashboard", "ineligible.disabled")
        } else if c.draining {
          ("draining", "ineligible.draining")
        } else if !service.backend_healthy {
          (
            "its backend health probe is failing",
            "ineligible.backend_unhealthy",
          )
        } else if !c.is_healthy(down_threshold) {
          ("missed heartbeats", "ineligible.missed_heartbeats")
        } else {
          ("its path bind does not match", "ineligible.path_mismatch")
        };
        Some((
          Conn {
            id: id.clone(),
            name: service.display_name(),
            org: c.perms.org_id.clone(),
          },
          why,
          code,
        ))
      })
      .collect();
    // What the stages below need from whoever would serve this, taken while
    // the map is already open. The first candidate stands for the pool: a
    // service's own limits are the same on every connection it holds, and
    // where they are not, the pool is a load-balancing set whose members are
    // meant to be interchangeable anyway.
    let serving = pool.first().and_then(|p| {
      clients
        .get(&p.id)?
        .services
        .iter()
        .find(|s| s.matches_host(&hostname))
        .map(|s| Serving {
          max_concurrent: s.max_concurrent,
          cache: s.cache,
          restricts_source_ips: !s.allowed_ips.is_empty(),
          max_request_body: s.max_request_body,
        })
    });
    (pool, ineligible, serving)
  };
  // The same lists twice: once as a sentence for `detail`, once as data.
  // `label` is what a person reads and `id` is what addresses the client, so
  // both travel; the caller renders the first and can still act on the second.
  let pool_named = group(&pool, org.as_deref(), &org_names);
  let pool_text: Vec<String> = pool_named.iter().map(Named::text).collect();
  let pool_data: Vec<serde_json::Value> = pool_named.iter().map(named_json).collect();
  // Grouped by reason as well as by name: one service can hold a draining
  // connection and an unhealthy one, and calling both "draining" would be a
  // lie told for the sake of a shorter list.
  // A foreign client is counted without a reason as well as without a name:
  // "draining" is a fact about someone else's deployment, and the caller
  // asked why *their* request would not be served, which the count answers.
  let mut ineligible_named: Vec<(Named, Option<(&'static str, &'static str)>)> = Vec::new();
  for (conn, why, code) in &ineligible {
    let grouped = group(std::slice::from_ref(conn), org.as_deref(), &org_names);
    let one = grouped.into_iter().next().expect("one in, one out");
    let reason = one.label.is_some().then_some((*why, *code));
    match ineligible_named
      .iter_mut()
      .find(|(n, r)| n.label == one.label && *r == reason)
    {
      Some((n, _)) => {
        n.count += 1;
        n.ids.extend(one.ids);
      }
      None => ineligible_named.push((one, reason)),
    }
  }
  let ineligible_text: Vec<String> = ineligible_named
    .iter()
    .map(|(n, reason)| match reason {
      Some((why, _)) => format!("{} ({why})", n.text()),
      None => n.text(),
    })
    .collect();
  let ineligible_data: Vec<serde_json::Value> = ineligible_named
    .iter()
    .map(|(n, reason)| {
      let mut value = named_json(n);
      if let Some((_, code)) = reason {
        value["reason"] = serde_json::json!(code);
      }
      value
    })
    .collect();

  if !pool.is_empty() {
    let detail = format!(
      "{} client(s) would take it: {}",
      pool.len(),
      pool_text.join(", ")
    );
    if outcome.is_none() {
      decided(
        &mut outcome,
        "client",
        format!(
          "the request reaches a tunnel client ({})",
          pool_text.join(", ")
        ),
        "client.reached",
        Some(serde_json::json!({ "clients": pool_data })),
      );
    }
    steps.push(
      Step::new("routing", Verdict::Passes, "routing.candidates", detail)
        .with(serde_json::json!({ "count": pool.len(), "clients": pool_data }))
        .from_named("hostname/path binds", "setting.host_path_binds"),
    );
  } else {
    let mut detail = String::from("no connected client serves this hostname and path");
    if !ineligible.is_empty() {
      detail.push_str(&format!(
        ", though {} could: {}",
        ineligible.len(),
        ineligible_text.join(", ")
      ));
    }
    let code = if ineligible.is_empty() {
      "routing.none"
    } else {
      "routing.none_ineligible"
    };
    steps.push(
      Step::new("routing", Verdict::Passes, code, detail)
        .with(serde_json::json!({
          // Connections, matching the sentence, not groups.
          "count": ineligible.len(),
          "ineligible": ineligible_data,
        }))
        .from_named("hostname/path binds", "setting.host_path_binds"),
    );

    // 8. What answers instead: a cold start, a fallback, or the 504.
    let armed = {
      let store = state.scaling_store.lock().await;
      store
        .list()
        .iter()
        .any(|r| r.hostname == hostname && r.path.as_deref().is_none_or(|p| path.starts_with(p)))
    };
    if cfg.scaling_enabled && armed {
      steps.push(
        Step::new(
          "cold_start",
          Verdict::Passes,
          "cold_start.armed",
          "an autoscaling record is armed for this bind, so the request would be held while capacity is asked for, rather than answered at once",
        )
        .from("scaling:"),
      );
    }
    if let Some(rule) = cfg.fallbacks.matched(Some(&hostname)) {
      let detail = format!(
        "{}: the fallbacks: rule for this hostname redirects to {} instead of a 504",
        if rule.permanent { 301 } else { 302 },
        rule.url
      );
      let params = serde_json::json!({
        "status": if rule.permanent { 301 } else { 302 },
        "url": rule.url,
      });
      decided(
        &mut outcome,
        "fallback",
        detail.clone(),
        "fallback.redirect",
        Some(params.clone()),
      );
      steps.push(
        Step::new("fallback", Verdict::Decides, "fallback.redirect", detail)
          .with(params)
          .from("fallbacks:"),
      );
    } else {
      let detail = "504: nothing serves this route, and no fallbacks: rule covers it".to_string();
      decided(
        &mut outcome,
        "no_client",
        detail.clone(),
        "no_client.504",
        None,
      );
      steps.push(Step::new(
        "no_client",
        Verdict::Decides,
        "no_client.504",
        detail,
      ));
    }
  }

  // 9. What the serving client and the visitor's token bring to it. These run
  // after routing because every one of them is a property of whoever would
  // take the request, and there is nobody to ask until routing has picked one.
  if let Some(serving) = &serving {
    if serving.restricts_source_ips {
      steps.push(
        Step::new(
          "allowed_ips",
          Verdict::Passes,
          "allowed_ips.restricted",
          "the serving service declares allowed_ips, so a visitor outside that list is routed nowhere and gets the 504 below rather than a refusal naming the list",
        )
        .from("allowed_ips"),
      );
    }
    let body_limit =
      crate::proxy::effective_body_limit(cfg.max_body_size, serving.max_request_body);
    steps.push(
      Step::new(
        "body_limit",
        Verdict::Passes,
        "body_limit.effective",
        format!(
          "a request body over {body_limit} bytes is refused with 413; the service may tighten the server's limit but never widen it"
        ),
      )
      .with(serde_json::json!({
        "effective": body_limit,
        "server": cfg.max_body_size,
        "service": serving.max_request_body,
      }))
      .from("max_body_size"),
    );
    match serving.max_concurrent {
      Some(max) => steps.push(
        Step::new(
          "client_concurrency",
          Verdict::Passes,
          "client_concurrency.declared",
          format!(
            "the serving client admits {max} at a time; past that a request waits for a slot and is refused if none frees before the gateway timeout"
          ),
        )
        .with(serde_json::json!({ "max": max }))
        .from(crate::limits::Limit::ClientConcurrency.setting()),
      ),
      None => steps.push(
        Step::new(
          "client_concurrency",
          Verdict::Skipped,
          "client_concurrency.unlimited",
          "the serving client declares no concurrency limit of its own",
        )
        .from(crate::limits::Limit::ClientConcurrency.setting()),
      ),
    }
    if cfg.cache_enabled && serving.cache {
      steps.push(
        Step::new(
          "cache",
          Verdict::Passes,
          "cache.eligible",
          "this route is cacheable, so a fresh entry would answer here without the request reaching a client at all",
        )
        .from("cache"),
      );
    } else {
      steps.push(Step::new(
        "cache",
        Verdict::Skipped,
        "cache.off",
        if cfg.cache_enabled {
          "the serving service does not opt into caching, so every request goes to the backend"
        } else {
          "response caching is off server-wide"
        },
      ));
    }
  }

  // 10. The quotas, which belong to the credential rather than to the route.
  // The token is the one thing this report cannot know: it explains a
  // hostname and a path, and which token a visitor arrives with is not a
  // property of either.
  steps.push(
    Step::new(
      "token_quota",
      Verdict::Skipped,
      "token_quota.depends_on_token",
      "a dynamic token carries its own requests-per-second limit and daily byte quota; which token a request arrives with is not a property of this route, so neither is checked here",
    )
    .from(crate::limits::Limit::TokenRate.setting()),
  );
  steps.push(
    Step::new(
      "token_quota",
      Verdict::Skipped,
      "token_quota.daily",
      "the same token's daily byte quota is likewise a property of the credential",
    )
    .from(crate::limits::Limit::TokenQuota.setting()),
  );
  match org.as_deref() {
    Some(id) => {
      let over = state.org_over_month_bytes(Some(id)).await;
      steps.push(
        Step::new(
          "org_quota",
          if over { Verdict::Decides } else { Verdict::Passes },
          if over {
            "org_quota.exhausted"
          } else {
            "org_quota.within"
          },
          if over {
            "429: this organization is over its monthly byte quota, so every request it serves is refused until the month turns"
          } else {
            "this organization is within its monthly byte quota"
          },
        )
        .from(crate::limits::Limit::OrgQuota.setting()),
      );
      if over {
        decided(
          &mut outcome,
          "org_quota",
          "429: this organization is over its monthly byte quota".to_string(),
          "org_quota.exhausted",
          None,
        );
      }
    }
    None => steps.push(
      Step::new(
        "org_quota",
        Verdict::Skipped,
        "org_quota.master",
        "the master organization has no monthly byte quota",
      )
      .from(crate::limits::Limit::OrgQuota.setting()),
    ),
  }

  // 11. Streamed responses already open for this visitor. A ceiling on
  // concurrency per IP, so like the bucket above it is a rule here rather than
  // a count: the report has no visitor to count for.
  if cfg.max_streams_per_ip == 0 {
    steps.push(
      Step::new(
        "streams_per_ip",
        Verdict::Skipped,
        "streams_per_ip.unlimited",
        "no ceiling on concurrently open streamed responses per visitor",
      )
      .from(crate::limits::Limit::StreamsPerIp.setting()),
    );
  } else {
    steps.push(
      Step::new(
        "streams_per_ip",
        Verdict::Passes,
        "streams_per_ip.capped",
        format!(
          "one visitor IP may hold {} streamed responses open at once; a streamed answer past that is refused",
          cfg.max_streams_per_ip
        ),
      )
      .with(serde_json::json!({ "max": cfg.max_streams_per_ip }))
      .from(crate::limits::Limit::StreamsPerIp.setting()),
    );
  }

  let (outcome, summary, summary_code, summary_params) = outcome.unwrap_or_else(|| {
    (
      "client".to_string(),
      "the request reaches a tunnel client".to_string(),
      "client.reached",
      None,
    )
  });
  Json(Explanation {
    hostname,
    path,
    method,
    outcome,
    summary,
    summary_code,
    summary_params,
    steps,
  })
  .into_response()
}

#[cfg(test)]
#[path = "explain_tests.rs"]
mod tests;
