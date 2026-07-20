//! Per-hostname fallback URLs (the `fallbacks:` section of
//! `aperio-server.yaml`).
//!
//! When no client is connected to serve a hostname the visitor would normally
//! get a `504`. A fallback turns that into a graceful redirect (default `302`,
//! or `301` with `permanent: true`) to an origin/status URL instead — a
//! maintenance page, a static origin, a "come back soon" site. A `*` hostname
//! is the catch-all applied to any otherwise-unclaimed host.
//!
//! ```yaml
//! fallbacks:
//!   - hostname: app.example.com
//!     url: https://status.example.com
//!   - hostname: "*"
//!     url: https://www.example.com
//!     preserve_path: true
//! ```
//!
//! (Re)loaded at startup and on config hot-reload; a malformed section logs an
//! error and disables the feature.

use serde::Deserialize;

use crate::routing::normalize_hostname_bind;

/// One `fallbacks:` entry as written in the file.
#[derive(Deserialize)]
pub(crate) struct FallbackRuleRaw {
  hostname: String,
  url: String,
  #[serde(default)]
  permanent: bool,
  #[serde(default)]
  preserve_path: bool,
}

/// One compiled fallback rule.
#[derive(Clone, Debug)]
pub(crate) struct FallbackRule {
  /// Normalized hostname, or `*` for the catch-all.
  pub(crate) hostname: String,
  pub(crate) url: String,
  pub(crate) permanent: bool,
  pub(crate) preserve_path: bool,
}

/// Compiled `fallbacks:` rules carried in the server configuration.
#[derive(Default, Clone)]
pub(crate) struct Fallbacks {
  rules: Vec<FallbackRule>,
}

impl Fallbacks {
  pub(crate) fn is_empty(&self) -> bool {
    self.rules.is_empty()
  }

  /// The fallback for a request host: an exact match wins over the `*`
  /// catch-all. `None` when nothing applies.
  pub(crate) fn matched(&self, host: Option<&str>) -> Option<&FallbackRule> {
    let host = host.map(|h| h.to_ascii_lowercase());
    self
      .rules
      .iter()
      .find(|r| host.as_deref() == Some(r.hostname.as_str()))
      .or_else(|| self.rules.iter().find(|r| r.hostname == "*"))
  }
}

/// Reads and compiles the `fallbacks:` section of `aperio-server.yaml`.
pub(crate) fn from_config_file() -> Fallbacks {
  let Some(section) = crate::config_file::structured("fallbacks") else {
    return Fallbacks::default();
  };
  let raw: Vec<FallbackRuleRaw> = match serde_yaml::from_value(section) {
    Ok(rules) => rules,
    Err(err) => {
      tracing::error!("invalid `fallbacks:` section in aperio-server.yaml: {err} — disabled");
      return Fallbacks::default();
    }
  };
  Fallbacks {
    rules: compile(raw),
  }
}

/// Compiles raw rules (shared with the config lint). Drops entries with a bad
/// hostname or a non-http(s) URL.
pub(crate) fn compile(raw: Vec<FallbackRuleRaw>) -> Vec<FallbackRule> {
  let mut out = Vec::with_capacity(raw.len());
  for (i, rule) in raw.into_iter().enumerate() {
    let host = rule.hostname.trim();
    let hostname = if host == "*" {
      "*".to_string()
    } else {
      match normalize_hostname_bind(host) {
        Some(h) => h,
        None => {
          tracing::error!(
            "`fallbacks:` entry #{} has an invalid hostname; ignored",
            i + 1
          );
          continue;
        }
      }
    };
    let url = rule.url.trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
      tracing::error!(
        "`fallbacks:` entry #{} url must be an absolute http(s) URL; ignored",
        i + 1
      );
      continue;
    }
    out.push(FallbackRule {
      hostname,
      url,
      permanent: rule.permanent,
      preserve_path: rule.preserve_path,
    });
  }
  out
}

/// Builds the redirect `Location` for a matched fallback, appending the request
/// path + query when `preserve_path` is set.
pub(crate) fn redirect_location(rule: &FallbackRule, path: &str, query: Option<&str>) -> String {
  if !rule.preserve_path {
    return rule.url.clone();
  }
  let mut loc = format!("{}{}", rule.url.trim_end_matches('/'), path);
  if let Some(q) = query {
    loc.push('?');
    loc.push_str(q);
  }
  loc
}

#[cfg(test)]
mod tests {
  use super::*;

  fn rules(yaml: &str) -> Fallbacks {
    Fallbacks {
      rules: compile(serde_yaml::from_str(yaml).unwrap()),
    }
  }

  #[test]
  fn exact_match_wins_over_catch_all() {
    let f = rules(
      "- hostname: app.example.com\n  url: https://status.example.com\n- hostname: \"*\"\n  url: https://www.example.com\n",
    );
    assert_eq!(
      f.matched(Some("app.example.com")).unwrap().url,
      "https://status.example.com"
    );
    // Any other host falls to the catch-all.
    assert_eq!(
      f.matched(Some("other.example.com")).unwrap().url,
      "https://www.example.com"
    );
  }

  #[test]
  fn no_catch_all_means_no_match() {
    let f = rules("- hostname: app.example.com\n  url: https://s.example.com\n");
    assert!(f.matched(Some("nope.com")).is_none());
    // Invalid (non-http) URLs are dropped at compile.
    let bad = rules("- hostname: a.com\n  url: ftp://x\n");
    assert!(bad.is_empty());
  }

  #[test]
  fn redirect_location_preserves_path_when_asked() {
    let rule = FallbackRule {
      hostname: "*".into(),
      url: "https://origin.example.com/".into(),
      permanent: false,
      preserve_path: true,
    };
    assert_eq!(
      redirect_location(&rule, "/a/b", Some("q=1")),
      "https://origin.example.com/a/b?q=1"
    );
    let plain = FallbackRule {
      preserve_path: false,
      ..rule
    };
    assert_eq!(redirect_location(&plain, "/a/b", Some("q=1")), plain.url);
  }
}
