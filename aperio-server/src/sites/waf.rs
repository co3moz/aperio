//! A site's `waf`: a small request firewall evaluated before dispatch.
//!
//! A short list of rules, each of which ANDs the conditions it sets (a path
//! regex, a method, a header match); a rule with a `max_body` is a size limit
//! instead of a plain deny. Matching a deny rule answers `403 Forbidden`;
//! exceeding a `max_body` rule answers `413 Payload Too Large`. This is a
//! coarse first line of defense, not a full WAF; pair it with the per-IP,
//! per-token and per-site rate limits.
//!
//! A rule that cannot work (no condition, a regex that does not compile) is
//! refused when the settings are written, naming it. Dropping it instead,
//! which is what the old `waf:` section did, leaves an operator believing a
//! deny rule is up that is not.

use axum::http::HeaderMap;
use regex::Regex;

/// One compiled rule.
#[derive(Clone, Debug)]
pub(crate) struct WafRule {
  path: Option<Regex>,
  /// Uppercased HTTP methods; None = any method.
  methods: Option<Vec<String>>,
  /// (lowercased header name, value regex).
  header: Option<(String, Regex)>,
  /// When set, this is a body-size rule (413) instead of a deny rule (403).
  pub(crate) max_body: Option<usize>,
  /// Human-readable description, for the log and the `x-aperio-limit` reason.
  pub(crate) desc: String,
}

impl WafRule {
  /// Compiles one rule; `at` names it in an error and its description
  /// (`waf[0]`), and the site puts its own name in front.
  pub(crate) fn compile(rule: &aperio_config::WafRule, at: &str) -> Result<WafRule, String> {
    if rule.path.is_none()
      && rule.methods.is_none()
      && rule.header.is_none()
      && rule.max_body.is_none()
    {
      return Err(format!(
        "{at}: sets no condition, so it would refuse every request; give it a `path`, \
         `methods`, `header` or `max_body`"
      ));
    }
    let path = rule
      .path
      .as_deref()
      .map(Regex::new)
      .transpose()
      .map_err(|e| format!("{at}.path: not a regular expression: {e}"))?;
    let header = match &rule.header {
      None => None,
      Some(h) => Some((
        h.name.trim().to_ascii_lowercase(),
        Regex::new(&h.regex)
          .map_err(|e| format!("{at}.header.regex: not a regular expression: {e}"))?,
      )),
    };
    let methods: Option<Vec<String>> = rule
      .methods
      .as_ref()
      .map(|m| m.iter().map(|s| s.trim().to_ascii_uppercase()).collect());
    if methods.as_ref().is_some_and(Vec::is_empty) {
      return Err(format!(
        "{at}.methods: is empty, which would match nothing; leave it out to match every method"
      ));
    }
    let max_body = rule.max_body.map(|b| b.0 as usize);
    let desc = describe(rule.path.as_deref(), &methods, &header, max_body, at);
    Ok(WafRule {
      path,
      methods,
      header,
      max_body,
      desc,
    })
  }

  /// True when every condition this rule sets matches the request.
  pub(crate) fn conditions_match(&self, method: &str, path: &str, headers: &HeaderMap) -> bool {
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

  /// True when this is a deny rule (no `max_body`) matching the request.
  pub(crate) fn denies(&self, method: &str, path: &str, headers: &HeaderMap) -> bool {
    self.max_body.is_none() && self.conditions_match(method, path, headers)
  }

  /// True when this is a size rule the request matches and a body of
  /// `body_len` exceeds.
  pub(crate) fn refuses_body(
    &self,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body_len: usize,
  ) -> bool {
    self
      .max_body
      .is_some_and(|limit| body_len > limit && self.conditions_match(method, path, headers))
  }
}

fn describe(
  path: Option<&str>,
  methods: &Option<Vec<String>>,
  header: &Option<(String, Regex)>,
  max_body: Option<usize>,
  at: &str,
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
  format!("{at} ({})", parts.join(", "))
}

#[cfg(test)]
#[path = "waf_tests.rs"]
mod tests;
