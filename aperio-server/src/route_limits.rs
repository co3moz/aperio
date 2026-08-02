//! Per-route request rate limiting (the `rate_limits:` section of
//! `aperio-server.yaml`).
//!
//! Complements the per-IP and per-token limits with a cap on a specific
//! hostname + path prefix, so an expensive endpoint (login, export, search)
//! cannot be hammered even by many distinct visitors or tokens. Each rule owns
//! one shared token bucket, so `rps`/`burst` bound the *aggregate* rate to that
//! route across all callers.
//!
//! ```yaml
//! rate_limits:
//!   - hostname: app.example.com
//!     path: /login
//!     rps: 5
//!     burst: 10
//!   - path: /export      # any hostname
//!     rps: 1
//! ```
//!
//! Rules match first-match in file order (`hostname` unset = any host, `path`
//! unset = any path). A request that would drain an empty bucket is answered
//! with `429 Too Many Requests`. The section is (re)loaded at startup and on
//! config hot-reload; a malformed section logs an error and disables the
//! feature rather than breaking proxying.

use serde::Deserialize;

use crate::routing::{normalize_hostname_bind, normalize_path_bind, path_matches_bind};

/// One `rate_limits:` entry as written in the file.
#[derive(Deserialize)]
pub(crate) struct RateLimitRuleRaw {
  hostname: Option<String>,
  path: Option<String>,
  rps: f64,
  burst: Option<f64>,
  methods: Option<Vec<String>>,
}

/// One compiled rate-limit rule.
#[derive(Clone, Debug)]
pub(crate) struct RateLimitRule {
  /// Normalized hostname to match (None = any host).
  pub(crate) hostname: Option<String>,
  /// Normalized path prefix bind to match (None = any path).
  pub(crate) path: Option<String>,
  /// Sustained requests per second allowed to the route.
  pub(crate) rps: f64,
  /// Token-bucket burst capacity.
  pub(crate) burst: f64,
  /// Uppercased methods the rule applies to (None = every method).
  pub(crate) methods: Option<Vec<String>>,
  /// Stable key identifying this rule's shared bucket in the rate map.
  pub(crate) key: String,
}

/// True when a rule's method filter admits this request's verb. An absent
/// filter admits everything, and an absent verb (the config explainer, which
/// reasons about a route rather than a request) is not filtered out.
pub(crate) fn method_matches(filter: Option<&Vec<String>>, method: Option<&str>) -> bool {
  match (filter, method) {
    (None, _) => true,
    (Some(_), None) => true,
    (Some(list), Some(m)) => list.iter().any(|allowed| allowed.eq_ignore_ascii_case(m)),
  }
}

/// Compiled `rate_limits:` rules carried in the server configuration.
#[derive(Default, Clone)]
pub(crate) struct RouteLimits {
  pub(crate) rules: Vec<RateLimitRule>,
}

impl RouteLimits {
  /// True when no route limits are configured (the fast path).
  pub(crate) fn is_empty(&self) -> bool {
    self.rules.is_empty()
  }

  /// The first rule matching a request's host, path and method (first-match,
  /// file order), if any. `method: None` ignores method filters, for callers
  /// reasoning about a route rather than about one request.
  pub(crate) fn matched(
    &self,
    host: Option<&str>,
    path: &str,
    method: Option<&str>,
  ) -> Option<&RateLimitRule> {
    self.rules.iter().find(|r| {
      let host_ok = match &r.hostname {
        None => true,
        Some(h) => host.is_some_and(|rh| rh.eq_ignore_ascii_case(h)),
      };
      let path_ok = match &r.path {
        None => true,
        Some(p) => path_matches_bind(path, p),
      };
      host_ok && path_ok && method_matches(r.methods.as_ref(), method)
    })
  }
}

/// Reads and compiles the `rate_limits:` section of `aperio-server.yaml`.
/// Called at startup and again on hot-reload; a bad section disables the
/// feature instead of breaking proxying.
pub(crate) fn from_config_file() -> RouteLimits {
  let Some(section) = crate::config_file::structured("rate_limits") else {
    return RouteLimits::default();
  };
  let raw: Vec<RateLimitRuleRaw> = match serde_yaml::from_value(section) {
    Ok(rules) => rules,
    Err(err) => {
      tracing::error!(
        "invalid `rate_limits:` section in aperio-server.yaml: {err}, per-route rate limiting disabled"
      );
      return RouteLimits::default();
    }
  };
  RouteLimits {
    rules: compile(raw),
  }
}

/// Compiles raw rules into normalized, validated rules (shared by the loader
/// and the config lint).
pub(crate) fn compile(raw: Vec<RateLimitRuleRaw>) -> Vec<RateLimitRule> {
  let mut compiled = Vec::with_capacity(raw.len());
  for (i, rule) in raw.into_iter().enumerate() {
    if rule.rps <= 0.0 || rule.rps.is_nan() {
      tracing::error!(
        "`rate_limits:` entry #{} has a non-positive rps; ignored",
        i + 1
      );
      continue;
    }
    let hostname = rule.hostname.as_deref().and_then(normalize_hostname_bind);
    let path = rule.path.as_deref().and_then(normalize_path_bind);
    // Floor the burst to at least one token, otherwise a sub-1.0 burst can
    // never reach the 1-token gate and the route would 429 every request.
    let burst = rule.burst.filter(|b| *b > 0.0).unwrap_or(rule.rps).max(1.0);
    // An empty list would match no method at all, which is never what the
    // operator meant; treat it as "every method", like an absent filter.
    let methods = rule
      .methods
      .map(|list| {
        list
          .into_iter()
          .map(|m| m.trim().to_ascii_uppercase())
          .collect::<Vec<_>>()
      })
      .filter(|list| !list.is_empty());
    // The method set joins the key so two rules on the same route but
    // different verbs do not share one bucket.
    let key = format!(
      "{}|{}|{}",
      hostname.as_deref().unwrap_or("*"),
      path.as_deref().unwrap_or("*"),
      methods
        .as_ref()
        .map(|m| m.join(","))
        .unwrap_or_else(|| "*".to_string())
    );
    compiled.push(RateLimitRule {
      hostname,
      path,
      rps: rule.rps,
      burst,
      methods,
      key,
    });
  }
  compiled
}

#[cfg(test)]
#[path = "route_limits_tests.rs"]
mod tests;
