//! Client-less routes (the `routes:` section of `aperio-server.yaml`).
//!
//! A route binds a hostname and/or path prefix directly to a server-produced
//! answer — a redirect or a fixed response — without any tunnel client
//! involved. Typical uses: vanity redirects (`old.example.com` →
//! `https://new.example.com`), a "coming soon" page on a hostname whose
//! client is not deployed yet, or a fixed `/robots.txt`.
//!
//! Routes are matched before client routing (first match wins, in file
//! order) and are always public: they carry operator-authored content, so
//! the visitor gate does not apply.

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use crate::routing::{normalize_hostname_bind, normalize_path_bind, path_matches_bind};

/// A fixed response served straight from the server.
#[derive(Deserialize, Clone, Debug)]
pub(crate) struct RespondRule {
  /// HTTP status of the response (default 200).
  #[serde(default = "default_status")]
  pub(crate) status: u16,
  /// `Content-Type` header (default `text/html; charset=utf-8`).
  #[serde(default = "default_content_type")]
  pub(crate) content_type: String,
  /// Response body.
  #[serde(default)]
  pub(crate) body: String,
}

fn default_status() -> u16 {
  200
}

fn default_content_type() -> String {
  "text/html; charset=utf-8".to_string()
}

/// One client-less route: a hostname and/or path-prefix match paired with
/// either a `redirect` or a `respond` action.
#[derive(Deserialize, Clone, Debug)]
pub(crate) struct RouteRule {
  /// Hostname to match exactly (unset = any hostname).
  pub(crate) hostname: Option<String>,
  /// Path prefix to match, with bind semantics (unset = any path).
  pub(crate) path: Option<String>,
  /// Redirect target; answers 302 (or 301 with `permanent: true`).
  pub(crate) redirect: Option<String>,
  /// Use a permanent 301 instead of the default 302.
  #[serde(default)]
  pub(crate) permanent: bool,
  /// Append the request's path and query to the redirect target.
  #[serde(default)]
  pub(crate) preserve_path: bool,
  /// Serve a fixed response instead of redirecting.
  pub(crate) respond: Option<RespondRule>,
}

impl RouteRule {
  /// True when this rule matches the request's host and path.
  fn matches(&self, host: Option<&str>, path: &str) -> bool {
    if let Some(ref rule_host) = self.hostname {
      let Some(host) = host else { return false };
      if !host.eq_ignore_ascii_case(rule_host) {
        return false;
      }
    }
    if let Some(ref bind) = self.path
      && !path_matches_bind(path, bind)
    {
      return false;
    }
    true
  }

  /// Builds the configured answer for a matched request.
  fn respond(&self, path: &str, query: Option<&str>) -> Response {
    if let Some(ref target) = self.redirect {
      let status = if self.permanent {
        StatusCode::MOVED_PERMANENTLY
      } else {
        StatusCode::FOUND
      };
      let mut location = target.clone();
      if self.preserve_path {
        location = format!("{}{}", location.trim_end_matches('/'), path);
        if let Some(q) = query {
          location.push('?');
          location.push_str(q);
        }
      }
      return Response::builder()
        .status(status)
        .header("location", location)
        .body(Body::empty())
        .unwrap_or_default();
    }
    let respond = self.respond.clone().unwrap_or(RespondRule {
      status: 200,
      content_type: default_content_type(),
      body: String::new(),
    });
    Response::builder()
      .status(StatusCode::from_u16(respond.status).unwrap_or(StatusCode::OK))
      .header("content-type", respond.content_type)
      .body(Body::from(respond.body))
      .unwrap_or_default()
  }
}

/// The compiled route list carried in the server configuration.
#[derive(Default, Clone)]
pub(crate) struct StaticRoutes {
  rules: std::sync::Arc<Vec<RouteRule>>,
}

impl StaticRoutes {
  /// Validates and compiles parsed rules; returns a message for a rule that
  /// could never fire (no action, or both actions).
  pub(crate) fn compile(mut rules: Vec<RouteRule>) -> Result<Self, String> {
    for (i, rule) in rules.iter_mut().enumerate() {
      match (&rule.redirect, &rule.respond) {
        (None, None) => return Err(format!("route #{}: needs `redirect` or `respond`", i + 1)),
        (Some(_), Some(_)) => {
          return Err(format!(
            "route #{}: `redirect` and `respond` are mutually exclusive",
            i + 1
          ));
        }
        _ => {}
      }
      if let Some(ref h) = rule.hostname {
        rule.hostname =
          Some(normalize_hostname_bind(h).ok_or(format!("route #{}: invalid hostname", i + 1))?);
      }
      if let Some(ref p) = rule.path {
        rule.path =
          Some(normalize_path_bind(p).ok_or(format!("route #{}: invalid path bind", i + 1))?);
      }
    }
    Ok(StaticRoutes {
      rules: std::sync::Arc::new(rules),
    })
  }

  /// Returns the configured answer for the first matching route, if any.
  pub(crate) fn answer(
    &self,
    host: Option<&str>,
    path: &str,
    query: Option<&str>,
  ) -> Option<Response> {
    self
      .rules
      .iter()
      .find(|r| r.matches(host, path))
      .map(|r| r.respond(path, query))
  }

  /// True when no routes are configured (the fast path).
  pub(crate) fn is_empty(&self) -> bool {
    self.rules.is_empty()
  }

  /// The compiled rules, for display (the topology map).
  pub(crate) fn rules(&self) -> &[RouteRule] {
    &self.rules
  }
}

/// Reads and compiles the `routes:` section of `aperio-server.yaml`.
/// Like `headers:`, a malformed section is a startup error.
pub(crate) fn from_config_file() -> StaticRoutes {
  let Some(section) = crate::config_file::structured("routes") else {
    return StaticRoutes::default();
  };
  let parsed: Result<Vec<RouteRule>, _> = serde_yaml::from_value(section);
  let compiled = parsed
    .map_err(|e| e.to_string())
    .and_then(StaticRoutes::compile);
  match compiled {
    Ok(routes) => routes,
    Err(err) => {
      tracing::error!("invalid `routes:` section in aperio-server.yaml: {err}");
      std::process::exit(1);
    }
  }
}

#[cfg(test)]
#[path = "static_routes_tests.rs"]
mod tests;
