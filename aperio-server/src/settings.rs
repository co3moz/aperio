use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::time::Duration;

use crate::routing::normalize_random_subdomain_pattern;

/// Configuration settings for the Aperio server.
#[derive(Clone)]
pub(crate) struct ServerConfig {
  pub(crate) token: String,
  pub(crate) gateway_timeout: Duration,
  pub(crate) gateway_response_timeout: Duration,
  pub(crate) max_body_size: usize,
  pub(crate) max_tunnels: usize,
  pub(crate) ip_limit_max: f64,
  pub(crate) ip_limit_refill: f64,
  pub(crate) auth_credentials: Option<String>,
  /// When true, the server trusts `X-Forwarded-For` / `X-Real-IP` headers for
  /// client IP resolution. Only enable when running behind a trusted reverse
  /// proxy, otherwise clients can spoof these headers to bypass rate limiting.
  pub(crate) trust_proxy: bool,
  /// When true, the server ignores any client-declared visitor password
  /// override (Ping `visitor_auth`) and keeps full control of the visitor gate
  /// with its own APERIO_SERVER_AUTH / OIDC. Env-only (APERIO_IGNORE_CLIENT_AUTH).
  pub(crate) ignore_client_auth: bool,
  /// Header consulted first for the real client IP when trust_proxy is on
  /// (APERIO_REAL_IP_HEADER, e.g. `CF-Connecting-IP` behind Cloudflare).
  pub(crate) real_ip_header: Option<String>,
  /// Trusted reverse-proxy / CDN egress ranges (APERIO_TRUSTED_PROXIES, a
  /// comma-separated list of IPs or CIDRs). When set (and `trust_proxy` is on),
  /// the real client IP is resolved by walking the `X-Forwarded-For` chain plus
  /// the direct socket peer from right to left and taking the first address
  /// that is NOT one of these — the standard "trust proxy" model that works for
  /// any CDN/proxy chain, not just Cloudflare. Empty = legacy behavior (first
  /// XFF entry).
  pub(crate) trusted_proxies: Vec<(IpAddr, u32)>,
  /// When true, session cookies include the `Secure` flag so browsers only
  /// send them over HTTPS connections. Defaults to the value of `trust_proxy`
  /// (i.e. enabled when running behind a TLS-terminating reverse proxy).
  pub(crate) secure_cookies: bool,
  /// When true, only clients that declared (or were overruled with) a
  /// hostname bind participate in load balancing. Clients without a hostname
  /// bind never receive proxied traffic.
  pub(crate) require_hostname_bind: bool,
  /// Optional bearer token required to scrape the `/aperio/metrics` endpoint.
  pub(crate) metrics_token: Option<String>,
  /// Canonical pattern for automatic random subdomains (from
  /// `APERIO_RANDOM_SUBDOMAIN`): a hostname whose leftmost label contains a
  /// `*` placeholder, e.g. `*.example.com` or `*-test.example.com`. When
  /// set, every connecting client is assigned the pattern with `*` replaced
  /// by a random label, in addition to any token-granted or declared
  /// hostname binds.
  pub(crate) random_subdomain_suffix: Option<String>,
  /// A client whose last heartbeat is older than this is considered down and
  /// removed from the load-balancing pool until it pings again.
  pub(crate) client_down_threshold: Duration,
  /// When true, the server offers zlib compression to connecting clients;
  /// tunnel frames are compressed once the client acknowledges.
  pub(crate) tunnel_compression: bool,
  /// Custom HTML page served on 504 gateway-timeout responses
  /// (loaded once from APERIO_504_PAGE at startup).
  pub(crate) custom_504_page: Option<String>,
  /// Custom HTML page served while a hostname is in maintenance mode
  /// (loaded once from APERIO_503_PAGE at startup).
  pub(crate) custom_503_page: Option<String>,
  /// How a client is picked from the routed pool (APERIO_LB_STRATEGY).
  pub(crate) lb_strategy: LbStrategy,
  /// What to do when a client is lost while a request is in flight.
  pub(crate) failover_mode: FailoverMode,
  /// Max re-dispatch attempts per request (APERIO_FAILOVER_MAX_JUMPS).
  pub(crate) failover_max_jumps: u32,
  /// Total time budget for waiting on candidates across all jumps
  /// (APERIO_FAILOVER_WINDOW, seconds).
  pub(crate) failover_window: Duration,
  /// Allow failover for non-idempotent methods too
  /// (APERIO_FAILOVER_ALL_METHODS).
  pub(crate) failover_all_methods: bool,
}

/// What happens when a tunnel client is lost while a request is in flight
/// and no response bytes have reached the visitor yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FailoverMode {
  /// Fail immediately with 502 (default).
  Fail,
  /// Re-dispatch to another currently available candidate; fail when none.
  Retry,
  /// Wait for the same client instance to reconnect and re-dispatch (any
  /// candidate qualifies when the instance is unknown).
  Wait,
  /// Re-dispatch to another candidate right away; when none exists, wait
  /// for one to appear.
  RetryWait,
}

/// Dashboard-editable configuration overrides, persisted as
/// `<data_dir>/settings.json`. Every field is optional: `None` (or a missing
/// key) keeps the environment-derived default; empty strings clear optional
/// values (pages, subdomain suffix, visitor auth). The master token,
/// HOST/PORT, proxy trust, cookie security and OIDC remain env-only.
#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct SettingsOverrides {
  pub(crate) gateway_timeout_secs: Option<u64>,
  pub(crate) gateway_response_timeout_secs: Option<u64>,
  pub(crate) max_body_size: Option<usize>,
  pub(crate) max_tunnels: Option<usize>,
  pub(crate) require_hostname_bind: Option<bool>,
  pub(crate) lb_strategy: Option<String>,
  pub(crate) failover_mode: Option<String>,
  pub(crate) failover_max_jumps: Option<u32>,
  pub(crate) failover_window_secs: Option<u64>,
  pub(crate) failover_all_methods: Option<bool>,
  pub(crate) client_down_threshold_secs: Option<u64>,
  pub(crate) ip_limit_max: Option<f64>,
  pub(crate) ip_limit_refill: Option<f64>,
  pub(crate) tunnel_compression: Option<bool>,
  pub(crate) random_subdomain_suffix: Option<String>,
  /// Raw HTML (not a file path, unlike APERIO_504_PAGE).
  pub(crate) custom_504_page: Option<String>,
  /// Raw HTML (not a file path, unlike APERIO_503_PAGE).
  pub(crate) custom_503_page: Option<String>,
  /// Visitor password in `user:password` form ("" = disabled).
  pub(crate) auth_credentials: Option<String>,
}

/// Parses an `APERIO_LB_STRATEGY`-style value.
pub(crate) fn parse_lb_strategy(raw: &str) -> Option<LbStrategy> {
  match raw.trim().to_ascii_lowercase().replace('_', "-").as_str() {
    "" | "round-robin" => Some(LbStrategy::RoundRobin),
    "primary-standby" | "failover" => Some(LbStrategy::PrimaryStandby),
    "sticky" => Some(LbStrategy::Sticky),
    _ => None,
  }
}

/// Parses an `APERIO_FAILOVER`-style value.
pub(crate) fn parse_failover_mode(raw: &str) -> Option<FailoverMode> {
  match raw.trim().to_ascii_lowercase().replace('_', "-").as_str() {
    "" | "fail" => Some(FailoverMode::Fail),
    "retry" => Some(FailoverMode::Retry),
    "wait" => Some(FailoverMode::Wait),
    "retry-wait" => Some(FailoverMode::RetryWait),
    _ => None,
  }
}

/// Applies persisted/dashboard overrides on top of the env-derived defaults,
/// producing the effective configuration. Invalid values are skipped.
pub(crate) fn apply_settings_overrides(base: &ServerConfig, o: &SettingsOverrides) -> ServerConfig {
  let mut c = base.clone();
  if let Some(v) = o.gateway_timeout_secs {
    c.gateway_timeout = Duration::from_secs(v.max(1));
  }
  if let Some(v) = o.gateway_response_timeout_secs {
    c.gateway_response_timeout = Duration::from_secs(v.max(1));
  }
  if let Some(v) = o.max_body_size {
    c.max_body_size = v.max(1024);
  }
  if let Some(v) = o.max_tunnels {
    c.max_tunnels = v.max(1);
  }
  if let Some(v) = o.require_hostname_bind {
    c.require_hostname_bind = v;
  }
  if let Some(ref s) = o.lb_strategy
    && let Some(v) = parse_lb_strategy(s)
  {
    c.lb_strategy = v;
  }
  if let Some(ref s) = o.failover_mode
    && let Some(v) = parse_failover_mode(s)
  {
    c.failover_mode = v;
  }
  if let Some(v) = o.failover_max_jumps {
    c.failover_max_jumps = v;
  }
  if let Some(v) = o.failover_window_secs {
    c.failover_window = Duration::from_secs(v.max(1));
  }
  if let Some(v) = o.failover_all_methods {
    c.failover_all_methods = v;
  }
  if let Some(v) = o.client_down_threshold_secs {
    c.client_down_threshold = Duration::from_secs(v.max(1));
  }
  if let Some(v) = o.ip_limit_max
    && v > 0.0
  {
    c.ip_limit_max = v;
  }
  if let Some(v) = o.ip_limit_refill
    && v >= 0.0
  {
    c.ip_limit_refill = v;
  }
  if let Some(v) = o.tunnel_compression {
    c.tunnel_compression = v;
  }
  if let Some(ref s) = o.random_subdomain_suffix {
    c.random_subdomain_suffix = if s.trim().is_empty() {
      None
    } else {
      // An invalid pattern keeps the previous value rather than breaking
      // hostname generation mid-flight.
      normalize_random_subdomain_pattern(s).or_else(|| c.random_subdomain_suffix.clone())
    };
  }
  if let Some(ref html) = o.custom_504_page {
    c.custom_504_page = if html.is_empty() {
      None
    } else {
      Some(html.clone())
    };
  }
  if let Some(ref html) = o.custom_503_page {
    c.custom_503_page = if html.is_empty() {
      None
    } else {
      Some(html.clone())
    };
  }
  if let Some(ref creds) = o.auth_credentials {
    c.auth_credentials = if creds.is_empty() {
      None
    } else {
      Some(creds.clone())
    };
  }
  c
}

/// JSON view of the dashboard-editable subset of a configuration.
pub(crate) fn settings_view(c: &ServerConfig) -> serde_json::Value {
  serde_json::json!({
    "gateway_timeout_secs": c.gateway_timeout.as_secs(),
    "gateway_response_timeout_secs": c.gateway_response_timeout.as_secs(),
    "max_body_size": c.max_body_size,
    "max_tunnels": c.max_tunnels,
    "require_hostname_bind": c.require_hostname_bind,
    "lb_strategy": match c.lb_strategy {
      LbStrategy::RoundRobin => "round-robin",
      LbStrategy::PrimaryStandby => "primary-standby",
      LbStrategy::Sticky => "sticky",
    },
    "failover_mode": match c.failover_mode {
      FailoverMode::Fail => "fail",
      FailoverMode::Retry => "retry",
      FailoverMode::Wait => "wait",
      FailoverMode::RetryWait => "retry-wait",
    },
    "failover_max_jumps": c.failover_max_jumps,
    "failover_window_secs": c.failover_window.as_secs(),
    "failover_all_methods": c.failover_all_methods,
    "client_down_threshold_secs": c.client_down_threshold.as_secs(),
    "ip_limit_max": c.ip_limit_max,
    "ip_limit_refill": c.ip_limit_refill,
    "tunnel_compression": c.tunnel_compression,
    "random_subdomain_suffix": c.random_subdomain_suffix,
    "custom_504_page": c.custom_504_page,
    "custom_503_page": c.custom_503_page,
    "auth_credentials": c.auth_credentials,
  })
}

/// Load-balancing behavior applied after routing narrows the candidate pool.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LbStrategy {
  /// Rotate through the whole pool (default).
  RoundRobin,
  /// Only the clients sharing the lowest announced priority receive traffic;
  /// higher-priority-number standbys take over when every more-primary
  /// client drops out of the pool (rotation still applies within a tier).
  PrimaryStandby,
  /// Round-robin, but visitors stick to the client that served them first
  /// via an `aperio_affinity` cookie (falling back to rotation when that
  /// client leaves the pool).
  Sticky,
}

/// Names of the fields a SettingsOverrides actually overrides.
pub(crate) fn override_keys(o: &SettingsOverrides) -> Vec<String> {
  match serde_json::to_value(o) {
    Ok(serde_json::Value::Object(map)) => map
      .into_iter()
      .filter(|(_, v)| !v.is_null())
      .map(|(k, _)| k)
      .collect(),
    _ => Vec::new(),
  }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
