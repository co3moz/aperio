//! Client-less routes (the `routes:` section of `aperio-server.yaml`).
//!
//! A route binds a hostname and/or path prefix directly to a server-produced
//! answer, a redirect or a fixed response, without any tunnel client
//! involved. Typical uses: vanity redirects (`old.example.com` →
//! `https://new.example.com`), a "coming soon" page on a hostname whose
//! client is not deployed yet, or a fixed `/robots.txt`.
//!
//! Routes are matched before client routing (first match wins, in file
//! order) and are always public: they carry operator-authored content, so
//! the visitor gate does not apply.
//!
//! An entry with neither action is the section's second kind, a *policy*
//! rule: it does not answer anything, it configures the proxied traffic that
//! matches it (`timeout`, `headers`, `rate_limit`), so per-route settings live
//! next to the route they govern. The two kinds share one list and one
//! matcher but are searched independently, `answer` skipping policy rules and
//! `policy_for` skipping answer rules, so neither can hide the other.

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

/// A `rate_limit:` block inside a route: the same token bucket a
/// `rate_limits:` rule builds, without repeating the hostname and path.
#[derive(Deserialize, Clone, Debug)]
pub(crate) struct RouteRateLimit {
  pub(crate) rps: f64,
  pub(crate) burst: Option<f64>,
  /// Methods the limit applies to, uppercased by `compile` (None = all).
  pub(crate) methods: Option<Vec<String>>,
}

/// Splitting a route's traffic between two versions of a service
/// (planned_features #51).
///
/// Weighted routing and a header-based canary are the same mechanism seen from
/// two angles, which is why they are one block: `weight` is who gets the new
/// version by default, `header` is how somebody opts in regardless.
///
/// ## The split is per visitor, not per request
///
/// Decided by hashing the visitor's address rather than by counting requests.
/// A per-request coin flip would send one page load's twenty assets to both
/// versions, which is not a canary, it is a mixture, and the first thing it
/// breaks is the thing being tested. Hashing means a visitor stays where they
/// landed for as long as their address holds.
///
/// The cost is that the split is only as even as the addresses are spread. At
/// low traffic, or behind one large NAT, twenty percent may not look like
/// twenty percent. That is the right trade for a canary, where consistency
/// per visitor matters more than precision in aggregate.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanaryRule {
  /// The service name traffic is sent to. A `services:` entry's `name`.
  pub(crate) service: String,
  /// Percentage of visitors sent there without asking. `0` = nobody, which
  /// with a `header` set is the "opt-in only" shape.
  #[serde(default)]
  pub(crate) weight: u8,
  /// Request header that sends this visitor there whatever the weight says.
  pub(crate) header: Option<String>,
  /// Value that header must carry. Unset = any non-empty value.
  pub(crate) value: Option<String>,
}

/// Which side of a canary split a request belongs on.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub(crate) enum Side {
  /// Serve from the canary service only.
  Canary,
  /// Serve from anything except the canary service.
  Stable,
}

impl CanaryRule {
  /// Which side this request lands on.
  ///
  /// The header wins outright when it matches: it is somebody asking, and a
  /// weight is only what happens to those who did not.
  pub(crate) fn side_for(
    &self,
    header_value: Option<&str>,
    visitor: Option<std::net::IpAddr>,
  ) -> Side {
    if let Some(name) = &self.header
      && !name.is_empty()
      && let Some(sent) = header_value
    {
      let matches = match &self.value {
        Some(expected) => sent.eq_ignore_ascii_case(expected),
        None => !sent.trim().is_empty(),
      };
      if matches {
        return Side::Canary;
      }
    }
    if self.weight == 0 {
      return Side::Stable;
    }
    if self.weight >= 100 {
      return Side::Canary;
    }
    // No address to be consistent for: a visitor whose address cannot be
    // read gets the stable side rather than a coin flip, because an
    // inconsistent canary is worse than a small one.
    let Some(ip) = visitor else {
      return Side::Stable;
    };
    if bucket_of(ip) < self.weight {
      Side::Canary
    } else {
      Side::Stable
    }
  }
}

/// A visitor's stable position in 0..100.
///
/// FNV-1a rather than the default hasher: `DefaultHasher` is randomly seeded
/// per process, so two servers behind a load balancer would disagree about
/// which visitors are in the canary, and the same visitor would move on every
/// restart. A canary has to be the same answer everywhere, every time.
fn bucket_of(ip: std::net::IpAddr) -> u8 {
  let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
  let octets = match ip {
    std::net::IpAddr::V4(v4) => v4.octets().to_vec(),
    std::net::IpAddr::V6(v6) => v6.octets().to_vec(),
  };
  for byte in octets {
    hash ^= byte as u64;
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
  }
  (hash % 100) as u8
}

/// One `routes:` entry, of either kind.
///
/// An *answer* rule carries `redirect` or `respond` and ends the request at
/// the server. A *policy* rule carries neither and instead annotates proxied
/// traffic that matches it: a response timeout, header edits, a rate limit.
/// Mixing the two on one entry is refused at startup, because a static answer
/// never reaches a backend and so can have no backend policy.
#[derive(Deserialize, Clone, Debug, Default)]
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
  /// Policy: seconds to wait for the serving client's answer on this route.
  pub(crate) timeout: Option<u64>,
  /// Policy: header edits for this route, as written in the file.
  pub(crate) headers: Option<crate::headers::HeaderRules>,
  /// Policy: rate limit for this route.
  pub(crate) rate_limit: Option<RouteRateLimit>,
  /// Policy: split this route's traffic between two versions of a service.
  pub(crate) canary: Option<CanaryRule>,
  /// `headers` compiled once by `compile`, so the request path only applies.
  #[serde(skip)]
  pub(crate) header_transforms: crate::headers::HeaderTransforms,
  /// Stable bucket key for `rate_limit`, assigned by `compile`. Derived from
  /// the entry's position and match, so two routes never share a bucket and a
  /// reload keeps the same key for the same entry.
  #[serde(skip)]
  pub(crate) rate_key: String,
}

impl RouteRule {
  /// True when this rule answers the request itself.
  pub(crate) fn is_answer(&self) -> bool {
    self.redirect.is_some() || self.respond.is_some()
  }

  /// True when this rule carries policy for proxied traffic.
  fn is_policy(&self) -> bool {
    self.timeout.is_some()
      || self.headers.is_some()
      || self.rate_limit.is_some()
      || self.canary.is_some()
  }

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
      if rule.redirect.is_some() && rule.respond.is_some() {
        return Err(format!(
          "route #{}: `redirect` and `respond` are mutually exclusive",
          i + 1
        ));
      }
      if rule.is_answer() && rule.is_policy() {
        return Err(format!(
          "route #{}: `timeout`, `headers` and `rate_limit` apply to proxied traffic, so they \
           cannot sit on a route that answers with `redirect` or `respond`; split them into a \
           second entry with the same hostname and path",
          i + 1
        ));
      }
      if !rule.is_answer() && !rule.is_policy() {
        return Err(format!(
          "route #{}: needs `redirect` or `respond` to answer, or one of `timeout`, `headers`, \
           `rate_limit` to carry policy",
          i + 1
        ));
      }
      if let Some(ref h) = rule.hostname {
        rule.hostname =
          Some(normalize_hostname_bind(h).ok_or(format!("route #{}: invalid hostname", i + 1))?);
      }
      if let Some(ref p) = rule.path {
        rule.path =
          Some(normalize_path_bind(p).ok_or(format!("route #{}: invalid path bind", i + 1))?);
      }
      if let Some(0) = rule.timeout {
        return Err(format!(
          "route #{}: `timeout` must be at least 1 second (omit it to inherit the global one)",
          i + 1
        ));
      }
      if let Some(ref mut rl) = rule.rate_limit {
        if !(rl.rps.is_finite() && rl.rps > 0.0) {
          return Err(format!(
            "route #{}: `rate_limit.rps` must be positive",
            i + 1
          ));
        }
        if let Some(b) = rl.burst
          && !(b.is_finite() && b > 0.0)
        {
          return Err(format!(
            "route #{}: `rate_limit.burst` must be positive",
            i + 1
          ));
        }
        // Uppercased once here so the request path compares against the
        // method verb it already has without allocating per request.
        if let Some(ref mut methods) = rl.methods {
          if methods.is_empty() {
            return Err(format!(
              "route #{}: `rate_limit.methods` is empty, which would match nothing; omit it to \
               limit every method",
              i + 1
            ));
          }
          for m in methods.iter_mut() {
            *m = m.trim().to_ascii_uppercase();
          }
        }
      }
      rule.header_transforms = match rule.headers {
        Some(ref h) => crate::headers::HeaderTransforms::compile(h),
        None => Default::default(),
      };
      rule.rate_key = format!(
        "route#{}:{}:{}",
        i + 1,
        rule.hostname.as_deref().unwrap_or("*"),
        rule.path.as_deref().unwrap_or("*")
      );
    }
    Ok(StaticRoutes {
      rules: std::sync::Arc::new(rules),
    })
  }

  /// Returns the configured answer for the first matching *answer* route, if
  /// any. Policy rules are skipped here: they annotate proxied traffic and
  /// must never terminate a request.
  pub(crate) fn answer(
    &self,
    host: Option<&str>,
    path: &str,
    query: Option<&str>,
  ) -> Option<Response> {
    self
      .rules
      .iter()
      .find(|r| r.is_answer() && r.matches(host, path))
      .map(|r| r.respond(path, query))
  }

  /// The first matching *policy* route, if any: the entry whose `timeout`,
  /// `headers` and `rate_limit` apply to this proxied request.
  pub(crate) fn policy_for(&self, host: Option<&str>, path: &str) -> Option<&RouteRule> {
    if self.rules.is_empty() {
      return None;
    }
    self
      .rules
      .iter()
      .find(|r| !r.is_answer() && r.matches(host, path))
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
