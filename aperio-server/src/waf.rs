//! WAF-lite: a small request firewall (the `waf:` section of
//! `aperio-server.yaml`).
//!
//! A short list of deny rules evaluated before a request is dispatched to a
//! client. Each rule ANDs the conditions it specifies (path regex, method,
//! header match); a rule with a `max_body` is a size limit instead of a plain
//! deny. Matching a deny rule answers `403 Forbidden`; exceeding a `max_body`
//! rule answers `413 Payload Too Large`. This is a coarse first line of
//! defense — path/method/header/body-size filtering — not a full WAF; pair it
//! with per-IP, per-token and per-route rate limiting.
//!
//! ```yaml
//! waf:
//!   - path: "^/\\.git"          # block probes for exposed repos
//!   - path: "^/admin"
//!     methods: [POST, PUT, DELETE]
//!   - header:
//!       name: user-agent
//!       regex: "(?i)sqlmap|nikto"
//!   - path: "^/upload"
//!     max_body: 1048576          # 1 MiB cap on this path (413)
//! ```
//!
//! (Re)loaded at startup and on config hot-reload; a malformed section or an
//! invalid regex logs an error and drops the offending rule rather than
//! breaking proxying.

use axum::http::HeaderMap;
use regex::Regex;
use serde::Deserialize;

/// A header match condition in a `waf:` rule.
#[derive(Deserialize)]
pub(crate) struct HeaderMatchRaw {
  name: String,
  regex: String,
}

/// One `waf:` entry as written in the file.
#[derive(Deserialize)]
pub(crate) struct WafRuleRaw {
  path: Option<String>,
  methods: Option<Vec<String>>,
  header: Option<HeaderMatchRaw>,
  max_body: Option<usize>,
}

/// One compiled WAF rule.
#[derive(Clone)]
struct WafRule {
  path: Option<Regex>,
  /// Uppercased HTTP methods; None = any method.
  methods: Option<Vec<String>>,
  /// (lowercased header name, value regex).
  header: Option<(String, Regex)>,
  /// When set, this is a body-size rule (413) instead of a deny rule (403).
  max_body: Option<usize>,
  /// Human-readable description for logging.
  desc: String,
}

impl WafRule {
  /// True when every condition this rule specifies matches the request.
  fn conditions_match(&self, method: &str, path: &str, headers: &HeaderMap) -> bool {
    if let Some(re) = &self.path
      && !re.is_match(path)
    {
      return false;
    }
    if let Some(methods) = &self.methods
      && !methods.iter().any(|m| m == method)
    {
      return false;
    }
    if let Some((name, re)) = &self.header {
      let matched = headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| re.is_match(v));
      if !matched {
        return false;
      }
    }
    true
  }
}

/// Compiled `waf:` rules carried in the server configuration.
#[derive(Default, Clone)]
pub(crate) struct WafRules {
  rules: Vec<WafRule>,
}

impl WafRules {
  /// True when no WAF rules are configured (the fast path).
  pub(crate) fn is_empty(&self) -> bool {
    self.rules.is_empty()
  }

  /// The description of the first deny rule (no `max_body`) that matches the
  /// request, if any — the caller answers 403. Evaluated before the body is
  /// read, so header/method/path attacks are rejected early.
  pub(crate) fn deny_reason(&self, method: &str, path: &str, headers: &HeaderMap) -> Option<&str> {
    self
      .rules
      .iter()
      .find(|r| r.max_body.is_none() && r.conditions_match(method, path, headers))
      .map(|r| r.desc.as_str())
  }

  /// The description of the first `max_body` rule the request matches *and*
  /// exceeds, if any — the caller answers 413. Evaluated after the body length
  /// is known.
  pub(crate) fn body_reason(
    &self,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body_len: usize,
  ) -> Option<&str> {
    self
      .rules
      .iter()
      .find(|r| {
        r.max_body
          .is_some_and(|limit| body_len > limit && r.conditions_match(method, path, headers))
      })
      .map(|r| r.desc.as_str())
  }
}

/// Reads and compiles the `waf:` section of `aperio-server.yaml`. Called at
/// startup and again on hot-reload; a bad section disables the feature.
pub(crate) fn from_config_file() -> WafRules {
  let Some(section) = crate::config_file::structured("waf") else {
    return WafRules::default();
  };
  let raw: Vec<WafRuleRaw> = match serde_yaml::from_value(section) {
    Ok(rules) => rules,
    Err(err) => {
      tracing::error!("invalid `waf:` section in aperio-server.yaml: {err} — WAF disabled");
      return WafRules::default();
    }
  };
  WafRules {
    rules: compile(raw),
  }
}

/// Compiles raw rules, dropping any with an invalid regex or no condition.
fn compile(raw: Vec<WafRuleRaw>) -> Vec<WafRule> {
  compile_reported(raw).0
}

fn compile_reported(raw: Vec<WafRuleRaw>) -> (Vec<WafRule>, usize) {
  let mut compiled = Vec::with_capacity(raw.len());
  let mut dropped = 0;
  for (i, rule) in raw.into_iter().enumerate() {
    let n = i + 1;
    if rule.path.is_none()
      && rule.methods.is_none()
      && rule.header.is_none()
      && rule.max_body.is_none()
    {
      tracing::error!("`waf:` entry #{n} has no conditions; ignored");
      dropped += 1;
      continue;
    }
    let path = match rule.path.as_deref().map(Regex::new).transpose() {
      Ok(p) => p,
      Err(e) => {
        tracing::error!("`waf:` entry #{n} has an invalid path regex: {e}; ignored");
        dropped += 1;
        continue;
      }
    };
    let header = match rule.header {
      None => None,
      Some(h) => match Regex::new(&h.regex) {
        Ok(re) => Some((h.name.trim().to_ascii_lowercase(), re)),
        Err(e) => {
          tracing::error!("`waf:` entry #{n} has an invalid header regex: {e}; ignored");
          dropped += 1;
          continue;
        }
      },
    };
    let methods = rule
      .methods
      .map(|m| m.iter().map(|s| s.trim().to_ascii_uppercase()).collect());
    let desc = describe(&rule.path, &methods, &header, rule.max_body, n);
    compiled.push(WafRule {
      path,
      methods,
      header,
      max_body: rule.max_body,
      desc,
    });
  }
  (compiled, dropped)
}

/// Count of rules that fail to compile (for the config lint).
pub(crate) fn count_dropped(raw: Vec<WafRuleRaw>) -> usize {
  compile_reported(raw).1
}

fn describe(
  path: &Option<String>,
  methods: &Option<Vec<String>>,
  header: &Option<(String, Regex)>,
  max_body: Option<usize>,
  n: usize,
) -> String {
  let mut parts = Vec::new();
  if let Some(p) = path {
    parts.push(format!("path~{p}"));
  }
  if let Some(m) = methods {
    parts.push(format!("methods={m:?}"));
  }
  if let Some((name, _)) = header {
    parts.push(format!("header={name}"));
  }
  if let Some(limit) = max_body {
    parts.push(format!("max_body={limit}"));
  }
  format!("waf#{n} ({})", parts.join(", "))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn rules_from(yaml: &str) -> WafRules {
    let raw: Vec<WafRuleRaw> = serde_yaml::from_str(yaml).unwrap();
    WafRules {
      rules: compile(raw),
    }
  }

  #[test]
  fn deny_matches_path_method_and_header() {
    let waf = rules_from(
      "- path: \"^/admin\"\n  methods: [POST]\n- header:\n    name: user-agent\n    regex: \"(?i)sqlmap\"\n",
    );
    let mut h = HeaderMap::new();
    // Path+method deny.
    assert!(waf.deny_reason("POST", "/admin/x", &h).is_some());
    // Wrong method → no match on the first rule.
    assert!(waf.deny_reason("GET", "/admin/x", &h).is_none());
    // Header rule.
    h.insert("user-agent", "sqlMAP/1.0".parse().unwrap());
    assert!(waf.deny_reason("GET", "/", &h).is_some());
  }

  #[test]
  fn body_rule_only_trips_over_limit() {
    let waf = rules_from("- path: \"^/upload\"\n  max_body: 100\n");
    let h = HeaderMap::new();
    // A body rule is not a deny rule.
    assert!(waf.deny_reason("POST", "/upload", &h).is_none());
    // Under the limit is fine; over trips 413.
    assert!(waf.body_reason("POST", "/upload", &h, 50).is_none());
    assert!(waf.body_reason("POST", "/upload", &h, 500).is_some());
    // Different path is unaffected.
    assert!(waf.body_reason("POST", "/other", &h, 500).is_none());
  }

  #[test]
  fn invalid_regex_rule_is_dropped() {
    let (rules, dropped) =
      compile_reported(serde_yaml::from_str("- path: \"(unclosed\"\n- path: \"^/ok\"\n").unwrap());
    assert_eq!(rules.len(), 1);
    assert_eq!(dropped, 1);
  }
}
