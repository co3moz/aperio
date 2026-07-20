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
  /// Stable key identifying this rule's shared bucket in the rate map.
  pub(crate) key: String,
}

/// Compiled `rate_limits:` rules carried in the server configuration.
#[derive(Default, Clone)]
pub(crate) struct RouteLimits {
  rules: Vec<RateLimitRule>,
}

impl RouteLimits {
  /// True when no route limits are configured (the fast path).
  pub(crate) fn is_empty(&self) -> bool {
    self.rules.is_empty()
  }

  /// The first rule matching a request's host and path (first-match, file
  /// order), if any.
  pub(crate) fn matched(&self, host: Option<&str>, path: &str) -> Option<&RateLimitRule> {
    self.rules.iter().find(|r| {
      let host_ok = match &r.hostname {
        None => true,
        Some(h) => host.is_some_and(|rh| rh.eq_ignore_ascii_case(h)),
      };
      let path_ok = match &r.path {
        None => true,
        Some(p) => path_matches_bind(path, p),
      };
      host_ok && path_ok
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
        "invalid `rate_limits:` section in aperio-server.yaml: {err} — per-route rate limiting disabled"
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
    let key = format!(
      "{}|{}",
      hostname.as_deref().unwrap_or("*"),
      path.as_deref().unwrap_or("*")
    );
    compiled.push(RateLimitRule {
      hostname,
      path,
      rps: rule.rps,
      burst,
      key,
    });
  }
  compiled
}

#[cfg(test)]
mod tests {
  use super::*;

  fn rules_from(yaml: &str) -> RouteLimits {
    let raw: Vec<RateLimitRuleRaw> = serde_yaml::from_str(yaml).unwrap();
    RouteLimits {
      rules: compile(raw),
    }
  }

  #[test]
  fn matches_first_rule_by_host_and_path() {
    let limits = rules_from(
      "- hostname: app.example.com\n  path: /login\n  rps: 5\n- path: /export\n  rps: 1\n",
    );
    // Host + path specific rule.
    let r = limits.matched(Some("app.example.com"), "/login").unwrap();
    assert_eq!(r.rps, 5.0);
    assert_eq!(r.burst, 5.0);
    // Path-only rule matches any host on a segment boundary.
    assert!(limits.matched(Some("other.com"), "/export/data").is_some());
    // No rule for an unrelated path.
    assert!(limits.matched(Some("app.example.com"), "/other").is_none());
    // Host-specific rule does not fire for a different host.
    assert!(limits.matched(Some("nope.com"), "/login").is_none());
  }

  #[test]
  fn burst_defaults_to_rps_and_invalid_rules_dropped() {
    let limits = rules_from("- path: /a\n  rps: 3\n- path: /b\n  rps: 0\n");
    assert_eq!(limits.matched(None, "/a").unwrap().burst, 3.0);
    // rps 0 rule is dropped.
    assert!(limits.matched(None, "/b").is_none());
  }
}
