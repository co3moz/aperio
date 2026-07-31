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
  /// One sentence for a person.
  pub(crate) detail: String,
  /// Where the rule behind it lives, when it has a home an operator edits.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) setting: Option<&'static str>,
}

impl Step {
  fn new(stage: &'static str, verdict: Verdict, detail: impl Into<String>) -> Self {
    Step {
      stage,
      verdict,
      detail: detail.into(),
      setting: None,
    }
  }

  fn from(mut self, setting: &'static str) -> Self {
    self.setting = Some(setting);
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
  /// The one-line answer, the thing someone came here to read.
  pub(crate) summary: String,
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
  let mut outcome: Option<(String, String)> = None;
  // Once something decides, later stages still report what they see, marked
  // as not reached: half the value here is "the route is fine, the
  // maintenance flag is what is answering".
  let decided = |outcome: &mut Option<(String, String)>, stage: &str, summary: String| {
    if outcome.is_none() {
      *outcome = Some((stage.to_string(), summary));
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
      decided(&mut outcome, "maintenance", detail.clone());
      steps.push(Step::new("maintenance", Verdict::Decides, detail).from("maintenance mode"));
    }
    None => steps.push(Step::new(
      "maintenance",
      Verdict::Passes,
      "no maintenance flag covers this hostname",
    )),
  }

  // 2. Client-less routes.
  if cfg.static_routes.is_empty() {
    steps.push(Step::new(
      "static_route",
      Verdict::Skipped,
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
    decided(&mut outcome, "static_route", detail.clone());
    steps.push(Step::new("static_route", Verdict::Decides, detail).from("routes:"));
  } else {
    steps.push(Step::new(
      "static_route",
      Verdict::Passes,
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
    decided(&mut outcome, "preview_noindex", detail.clone());
    steps.push(Step::new("preview_noindex", Verdict::Decides, detail).from("preview_noindex"));
  }

  // 4. WAF deny rules. Header rules cannot be judged without a real request,
  // so the report says so rather than implying the path is clean.
  if cfg.waf.is_empty() {
    steps.push(Step::new(
      "waf",
      Verdict::Skipped,
      "no waf: rules configured",
    ));
  } else if let Some(reason) = cfg.waf.deny_reason(&method, &path, &HeaderMap::new()) {
    let detail = format!("403: blocked by a waf: rule ({reason})");
    decided(&mut outcome, "waf", detail.clone());
    steps.push(Step::new("waf", Verdict::Decides, detail).from("waf:"));
  } else {
    steps.push(
      Step::new(
        "waf",
        Verdict::Passes,
        "no waf: rule matches this method and path (header and body rules need a real request)",
      )
      .from("waf:"),
    );
  }

  // 5. Per-route rate limit. Reported as the rule, never consumed: asking
  // why a request is refused must not spend the budget it is asking about.
  match cfg.route_limits.matched(Some(&hostname), &path) {
    Some(rule) => steps.push(
      Step::new(
        "route_rate_limit",
        Verdict::Passes,
        format!(
          "a rate_limits: rule covers this path ({} rps, burst {}); this dry run does not spend from it",
          rule.rps, rule.burst
        ),
      )
      .from("rate_limits"),
    ),
    None => steps.push(Step::new(
      "route_rate_limit",
      Verdict::Skipped,
      "no rate_limits: rule covers this path",
    )),
  }

  // 6. The visitor gate.
  if crate::routing::host_has_visitor_auth(&state, Some(&hostname)).await {
    steps.push(
      Step::new(
        "visitor_gate",
        Verdict::Passes,
        "visitors must sign in (or carry a share link) before this reaches a client",
      )
      .from("server_auth / OIDC"),
    );
  } else {
    steps.push(Step::new(
      "visitor_gate",
      Verdict::Skipped,
      "this hostname is served without a visitor gate",
    ));
  }

  // 7. Routing: which clients could take it, and why the others could not.
  let (pool, ineligible) = {
    let clients = state.clients.lock().await;
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
    // Every connected client that serves this hostname but would not take
    // the request, with the reason, which is the question behind most 504s.
    let ineligible: Vec<String> = clients
      .iter()
      .filter(|(id, c)| !pool.contains(id) && c.matches_host(&hostname))
      .map(|(id, c)| {
        let why = if !c.admin_enabled {
          "disabled from the dashboard"
        } else if c.draining {
          "draining"
        } else if !c.backend_healthy {
          "its backend health probe is failing"
        } else if !c.is_healthy(down_threshold) {
          "missed heartbeats"
        } else {
          "its path bind does not match"
        };
        format!("{id} ({why})")
      })
      .collect();
    (pool, ineligible)
  };

  if !pool.is_empty() {
    let detail = format!(
      "{} client(s) would take it: {}",
      pool.len(),
      pool.join(", ")
    );
    if outcome.is_none() {
      decided(
        &mut outcome,
        "client",
        format!("the request reaches a tunnel client ({})", pool.join(", ")),
      );
    }
    steps.push(Step::new("routing", Verdict::Passes, detail).from("hostname/path binds"));
  } else {
    let mut detail = String::from("no connected client serves this hostname and path");
    if !ineligible.is_empty() {
      detail.push_str(&format!(
        ", though {} could: {}",
        ineligible.len(),
        ineligible.join(", ")
      ));
    }
    steps.push(Step::new("routing", Verdict::Passes, detail).from("hostname/path binds"));

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
      decided(&mut outcome, "fallback", detail.clone());
      steps.push(Step::new("fallback", Verdict::Decides, detail).from("fallbacks:"));
    } else {
      let detail = "504: nothing serves this route, and no fallbacks: rule covers it".to_string();
      decided(&mut outcome, "no_client", detail.clone());
      steps.push(Step::new("no_client", Verdict::Decides, detail));
    }
  }

  let (outcome, summary) = outcome.unwrap_or_else(|| {
    (
      "client".to_string(),
      "the request reaches a tunnel client".to_string(),
    )
  });
  Json(Explanation {
    hostname,
    path,
    method,
    outcome,
    summary,
    steps,
  })
  .into_response()
}

#[cfg(test)]
#[path = "explain_tests.rs"]
mod tests;
