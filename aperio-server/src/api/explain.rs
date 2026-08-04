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

/// What a client's own configuration calls it, if it says anything.
///
/// The same order the clients table uses: the `custom_name` an operator gave
/// it, else the `name` of its `services:` entry.
fn display_name(client: &crate::state::ClientHandle) -> Option<String> {
  client
    .service_custom_name
    .clone()
    .or_else(|| client.service_name.clone())
}

/// What to call each client on screen.
///
/// The id is unique and unreadable; the service name is readable and not
/// unique, since a client can hold several connections and two clients can
/// serve one service. So a name is used as it stands when it belongs to one
/// entry, and carries the head of its id when it does not, which is the only
/// case where the id is worth the room it takes.
fn labels(entries: &[(String, Option<String>)]) -> Vec<String> {
  let mut seen: HashMap<&str, usize> = HashMap::new();
  for (_, name) in entries {
    if let Some(name) = name {
      *seen.entry(name.as_str()).or_default() += 1;
    }
  }
  entries
    .iter()
    .map(|(id, name)| match name {
      Some(name) if seen.get(name.as_str()) == Some(&1) => name.clone(),
      Some(name) => format!("{name} ({})", &id[..id.len().min(8)]),
      None => id.clone(),
    })
    .collect()
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
    None => steps.push(Step::new(
      "route_rate_limit",
      Verdict::Skipped,
      "route_rate_limit.none",
      "no rate_limits: rule covers this path",
    )),
  }

  // 6. The visitor gate. Three things can raise it, and naming which one is
  // the whole value: a client's own password is the service's, the server's
  // password and OIDC are the operator's, and they are configured in
  // different places.
  let client_gate = crate::routing::host_has_visitor_auth(&state, Some(&hostname)).await;
  let server_gate = cfg.auth_credentials.is_some();
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

  // 7. Routing: which clients could take it, and why the others could not.
  let (pool, ineligible) = {
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
    let pool: Vec<(String, Option<String>)> = pool
      .into_iter()
      .map(|id| {
        let name = clients.get(&id).and_then(display_name);
        (id, name)
      })
      .collect();
    // Every connected client that serves this hostname but would not take
    // the request, with the reason, which is the question behind most 504s.
    let ineligible: Vec<(String, Option<String>, &'static str, &'static str)> = clients
      .iter()
      .filter(|(id, c)| !pool.iter().any(|(p, _)| p == *id) && c.matches_host(&hostname))
      .map(|(id, c)| {
        let (why, code) = if !c.admin_enabled {
          ("disabled from the dashboard", "ineligible.disabled")
        } else if c.draining {
          ("draining", "ineligible.draining")
        } else if !c.backend_healthy {
          (
            "its backend health probe is failing",
            "ineligible.backend_unhealthy",
          )
        } else if !c.is_healthy(down_threshold) {
          ("missed heartbeats", "ineligible.missed_heartbeats")
        } else {
          ("its path bind does not match", "ineligible.path_mismatch")
        };
        (id.clone(), display_name(c), why, code)
      })
      .collect();
    (pool, ineligible)
  };
  // The same lists twice: once as a sentence for `detail`, once as data.
  // `label` is what a person reads and `id` is what addresses the client, so
  // both travel; the caller renders the first and can still act on the second.
  let pool_labels = labels(&pool);
  let pool_data: Vec<serde_json::Value> = pool
    .iter()
    .zip(&pool_labels)
    .map(|((id, _), label)| serde_json::json!({ "id": id, "label": label }))
    .collect();
  let ineligible_labels = labels(
    &ineligible
      .iter()
      .map(|(id, name, _, _)| (id.clone(), name.clone()))
      .collect::<Vec<_>>(),
  );
  let ineligible_text: Vec<String> = ineligible
    .iter()
    .zip(&ineligible_labels)
    .map(|((_, _, why, _), label)| format!("{label} ({why})"))
    .collect();
  let ineligible_data: Vec<serde_json::Value> = ineligible
    .iter()
    .zip(&ineligible_labels)
    .map(
      |((id, _, _, code), label)| serde_json::json!({ "id": id, "label": label, "reason": code }),
    )
    .collect();

  if !pool.is_empty() {
    let detail = format!(
      "{} client(s) would take it: {}",
      pool.len(),
      pool_labels.join(", ")
    );
    if outcome.is_none() {
      decided(
        &mut outcome,
        "client",
        format!(
          "the request reaches a tunnel client ({})",
          pool_labels.join(", ")
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
          "count": ineligible_data.len(),
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
