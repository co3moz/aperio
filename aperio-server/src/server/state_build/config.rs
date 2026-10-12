//! Turning the settings document into the [`ServerConfig`] the request path
//! reads.
//!
//! One function, [`build_config`], used for both moments a document becomes
//! the server's: at startup, and when a change is applied from the dashboard
//! or the API. It has no side effects beyond reading (and, once, generating)
//! the metrics token file, so the change path can build a config, and refuse
//! the change with the message, before anything is swapped in.

use std::time::Duration;

use aperio_config::server_settings::{Drain, ServerSettings};
use aperio_config::units::Span;
use tracing::{info, warn};

use crate::settings::ServerConfig;
use crate::store::tokens::TokenStore;

/// What startup builds beside the config: the stores opened from the data
/// directory, and the access log writer when one is configured.
pub(crate) struct Resolved {
  pub(crate) config: ServerConfig,
  pub(crate) access_log: Option<tokio::sync::mpsc::Sender<crate::access_log::AccessLogCmd>>,
  pub(crate) admin_key_store: crate::store::admin_keys::AdminKeyStore,
  pub(crate) inbox_store: crate::store::inbox::InboxStore,
  pub(crate) token_store: TokenStore,
}

/// Opens what startup needs and builds the config. `Err` refuses the start
/// with the message.
pub(crate) fn resolve(
  settings: &ServerSettings,
  token: &str,
  data_dir: &str,
) -> Result<Resolved, String> {
  let config = build_config(settings, token, data_dir)?;
  log_posture(&config);
  // The variables that used to route callbacks through a proxy, and are read
  // by nothing now: a route that worked yesterday must not vanish unsaid.
  if config.outbound_policy.egress.proxy.is_none() {
    let stale = crate::outbound::proxy_env_vars();
    if !stale.is_empty() {
      warn!(
        "{} set in the environment, and not used: outbound callbacks go where outbound.proxy says, \
         and direct when it says nothing. Set outbound.proxy to the same value to keep the route \
         you had.",
        stale.join(", ")
      );
    }
  }
  let access_log = settings.logging.access_log.as_ref().and_then(|path| {
    match std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(path)
    {
      Ok(file) => {
        info!("Structured access log enabled: {path}");
        Some(crate::access_log::spawn_writer(path.clone(), file))
      }
      Err(e) => {
        tracing::error!("Failed to open the access log {path}: {e}; the access log file is off");
        None
      }
    }
  });
  Ok(Resolved {
    config,
    access_log,
    admin_key_store: crate::store::admin_keys::AdminKeyStore::load(data_dir),
    inbox_store: crate::store::inbox::InboxStore::load(data_dir),
    token_store: TokenStore::load(data_dir),
  })
}

/// The section as the rule modules read it: `None` for an empty list.
///
/// An unset optional field serializes as `null`, and the rule modules read
/// their sections the way an operator writes them, where an absent key and a
/// `null` are not the same thing (`redirect: ~` is not a string). So the nulls
/// are left out, which is what the document said.
fn section<T: serde::Serialize>(rules: &[T]) -> Option<serde_yaml::Value> {
  if rules.is_empty() {
    return None;
  }
  serde_yaml::to_value(rules).ok().map(without_nulls)
}

fn without_nulls(value: serde_yaml::Value) -> serde_yaml::Value {
  match value {
    serde_yaml::Value::Mapping(map) => serde_yaml::Value::Mapping(
      map
        .into_iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k, without_nulls(v)))
        .collect(),
    ),
    serde_yaml::Value::Sequence(items) => {
      serde_yaml::Value::Sequence(items.into_iter().map(without_nulls).collect())
    }
    other => other,
  }
}

/// A list of addresses as the parsers take them.
fn ranges(field: &str, entries: &[String]) -> Result<Vec<(std::net::IpAddr, u32)>, String> {
  if entries.iter().all(|e| e.trim().is_empty()) {
    return Ok(Vec::new());
  }
  crate::routing::parse_trusted_proxies(&entries.join(",")).map_err(|e| format!("{field}: {e}"))
}

fn secs(span: Span) -> Duration {
  span.0
}

/// Builds the config a settings document describes, or says which setting
/// it cannot accept.
pub(crate) fn build_config(
  s: &ServerSettings,
  token: &str,
  data_dir: &str,
) -> Result<ServerConfig, String> {
  let trusted_proxies = ranges("proxy.trusted", &s.proxy.trusted)?;
  let trust_proxy = s.proxy.trust || !trusted_proxies.is_empty();
  let real_ip_header = s
    .proxy
    .real_ip_header
    .as_ref()
    .map(|v| v.trim().to_ascii_lowercase())
    .filter(|v| !v.is_empty());
  let admin_allowed_ips = ranges("security.admin_allowed_ips", &s.security.admin_allowed_ips)?;
  let denied_ips = crate::deny_list::from_list(&s.security.denied_ips)?;

  let dashboard_hostname = match s.dashboard.hostname.as_deref().map(str::trim) {
    Some(raw) if !raw.is_empty() => Some(
      crate::store::orgs::normalize_panel_hostname(raw)
        .ok_or_else(|| format!("dashboard.hostname: `{raw}` is not one exact hostname"))?,
    ),
    _ => None,
  };

  let server_side_targets = if s.security.server_side_targets.is_empty() {
    Vec::new()
  } else {
    crate::outbound::parse_patterns(&s.security.server_side_targets.join(","))
      .map_err(|e| format!("security.server_side_targets: {e}"))?
  };

  let outbound_policy = {
    let allowlist = if s.outbound.allowlist.is_empty() {
      Vec::new()
    } else {
      crate::outbound::parse_patterns(&s.outbound.allowlist.join(","))
        .map_err(|e| format!("outbound.allowlist: {e}"))?
    };
    let proxy = match s.outbound.proxy.as_deref().map(str::trim) {
      Some(raw) if !raw.is_empty() => Some(
        aperio_config::egress::EgressProxy::parse(raw)
          .map_err(|e| format!("outbound.proxy: {e}"))?,
      ),
      _ => None,
    };
    let bypass = aperio_config::egress::EgressBypass::parse(&s.outbound.no_proxy.join(","));
    crate::outbound::OutboundPolicy {
      allowlist,
      block_private: s.outbound.block_private,
      egress: crate::outbound::Egress { proxy, bypass },
    }
  };

  let random_subdomain_suffix = match s.routing.random_subdomain.as_deref().map(str::trim) {
    Some(raw) if !raw.is_empty() => Some(
      crate::routing::normalize_random_subdomain_pattern(raw).ok_or_else(|| {
        format!(
          "routing.random_subdomain: `{raw}` is not a pattern (write example.com, *.example.com or *-test.example.com)"
        )
      })?,
    ),
    _ => None,
  };

  let ui_language = s.dashboard.ui_language.trim().to_ascii_lowercase();
  if !crate::settings::UI_LANGUAGES.contains(&ui_language.as_str()) {
    return Err(format!(
      "dashboard.ui_language: `{}` is not one of {}",
      s.dashboard.ui_language,
      crate::settings::UI_LANGUAGES.join(", ")
    ));
  }

  let request_id_header = s.request_id.header.trim().to_ascii_lowercase();
  if request_id_header.parse::<axum::http::HeaderName>().is_err() {
    return Err(format!(
      "request_id.header: `{}` is not a header name",
      s.request_id.header
    ));
  }

  for (field, page) in [
    ("pages.unavailable", &s.pages.unavailable),
    ("pages.maintenance", &s.pages.maintenance),
  ] {
    if page.as_ref().is_some_and(|html| html.len() > 512 * 1024) {
      return Err(format!("{field}: exceeds 512KB"));
    }
  }

  for (field, rate) in [
    ("logging.sample_rate", s.logging.sample_rate),
    ("otel.sample_rate", s.otel.sample_rate),
  ] {
    if !(rate.is_finite() && (0.0..=1.0).contains(&rate)) {
      return Err(format!("{field}: {rate} is not between 0.0 and 1.0"));
    }
  }
  if !(s.limits.ip_rate.burst.is_finite() && s.limits.ip_rate.burst > 0.0) {
    return Err("limits.ip_rate.burst: must be positive".to_string());
  }
  if !(s.limits.ip_rate.per_second.is_finite() && s.limits.ip_rate.per_second > 0.0) {
    return Err("limits.ip_rate.per_second: must be positive".to_string());
  }
  if s.cache.size.0 == 0 {
    return Err("cache.size: must be positive (turn the cache off with cache.enabled)".to_string());
  }

  crate::oidc::grant_policy(&s.oidc)?;

  let visitor_auth = match &s.access.gate {
    Some(gate) => {
      crate::visitor_auth::validate_gate(gate)?;
      crate::visitor_auth::Policy::compile(gate)
    }
    None => crate::visitor_auth::Policy::default(),
  };

  let metrics_token = if s.metrics.enabled {
    match s.metrics.token.as_deref().map(str::trim) {
      Some(t) if !t.is_empty() => Some(t.to_string()),
      _ => Some(persisted_metrics_token(data_dir)),
    }
  } else {
    s.metrics.token.clone().filter(|t| !t.trim().is_empty())
  };

  let (shutdown_drain, shutdown_drain_auto) = match s.shutdown.drain {
    Drain::Auto => (None, true),
    Drain::For(span) => (Some(span.as_secs()), false),
  };

  Ok(ServerConfig {
    token: token.to_string(),
    gateway_timeout: secs(s.gateway.timeout),
    gateway_response_timeout: secs(s.gateway.response_timeout),
    max_body_size: s.limits.request_body.0 as usize,
    max_tunnels: s.limits.tunnels as usize,
    max_connections_per_service: s.limits.connections_per_service.max(1),
    inspector: s.inspector.enabled,
    access_events: s.logging.access_events,
    ip_limit_max: s.limits.ip_rate.burst,
    ip_limit_refill: s.limits.ip_rate.per_second,
    visitor_auth,
    trust_proxy,
    ignore_client_auth: s.access.ignore_client_auth,
    real_ip_header,
    trusted_proxies,
    admin_allowed_ips,
    dashboard_hostname,
    fenced_login: s.dashboard.fenced_login,
    secure_cookies: s.security.secure_cookies.unwrap_or(trust_proxy),
    require_hostname_bind: s.routing.require_hostname_bind,
    default_access: s.access.default,
    metrics_token,
    scaling_enabled: s.scaling.enabled,
    scaling_allow_http: s.scaling.allow_http,
    scaling_allow_private: s.scaling.allow_private,
    scaling_record_ttl: secs(s.scaling.record_ttl),
    edge_token: s.edge.token.clone().filter(|t| !t.trim().is_empty()),
    edge_service_url: s.edge.service_url.clone().filter(|v| !v.trim().is_empty()),
    edge_entrypoints: s
      .edge
      .entrypoints
      .iter()
      .map(|v| v.trim().to_string())
      .filter(|v| !v.is_empty())
      .collect(),
    edge_cert_resolver: s
      .edge
      .cert_resolver
      .clone()
      .filter(|v| !v.trim().is_empty()),
    edge_include_offline: s.edge.include_offline,
    random_subdomain_suffix,
    client_down_threshold: secs(s.gateway.client_down_threshold).max(Duration::from_secs(1)),
    tunnel_compression: s.tunnel.compression,
    custom_504_page: s.pages.unavailable.clone().filter(|v| !v.is_empty()),
    custom_503_page: s.pages.maintenance.clone().filter(|v| !v.is_empty()),
    lb_strategy: s.routing.lb_strategy,
    failover_mode: s.failover.mode,
    failover_max_jumps: s.failover.max_jumps,
    failover_window: secs(s.failover.window),
    failover_all_methods: s.failover.all_methods,
    retry_on_5xx: s.failover.retry_on_5xx,
    retry_statuses: s.failover.retry_statuses.clone(),
    outlier_ejection: s.outlier.enabled,
    outlier_max_failures: s.outlier.max_failures.max(1),
    outlier_window: secs(s.outlier.window).max(Duration::from_secs(1)),
    outlier_eject: secs(s.outlier.eject_for).max(Duration::from_secs(1)),
    cache_enabled: s.cache.enabled,
    cache_max_bytes: s.cache.size.0,
    cache_max_stale: s.cache.max_stale.as_secs(),
    stream_min_throughput: s.stream.min_throughput.0,
    stream_pause_bytes: s.stream.pause_at.0 as usize,
    stream_resume_bytes: s.stream.resume_at.0 as usize,
    stream_backlog_limit: s.stream.backlog_limit.0 as usize,
    server_side_targets,
    outbound_policy,
    max_concurrent_requests: s.limits.concurrent_requests as usize,
    max_ws_connections: match s.limits.websockets {
      0 => usize::MAX,
      n => n as usize,
    },
    login_lockout_threshold: s.security.login_lockout.threshold,
    login_lockout_secs: s.security.login_lockout.duration.as_secs(),
    audit_max_size: s.audit.max_size.0,
    audit_max_files: s.audit.max_files as usize,
    ui_language,
    header_rules: crate::headers::from_section(
      serde_yaml::to_value(&s.headers).ok().map(without_nulls),
    )?,
    static_routes: crate::static_routes::from_section(section(&s.routes))?,
    error_pages: crate::error_pages::from_section(section(&s.error_pages))?,
    route_limits: crate::route_limits::from_section(section(&s.rate_limits))?,
    fallbacks: crate::fallbacks::from_section(section(&s.fallbacks))?,
    waf: crate::waf::from_section(section(&s.waf))?,
    alert_rules: crate::alert_rules::from_section(section(&s.alerts.rules))?,
    maintenance_windows: crate::maintenance_windows::from_section(section(&s.maintenance_windows))?,
    denied_ips,
    identity_headers: s.identity_headers.serving,
    visitor_identity_headers: s.identity_headers.visitor,
    access_log_sample_rate: s.logging.sample_rate,
    alternate_servers: crate::tunnel::ws::parse_alternates(&s.routing.alternate_servers.join(",")),
    max_streams_per_ip: s.limits.streams_per_ip,
    otel_bridge: s.otel.bridge,
    shutdown_drain,
    shutdown_drain_auto,
    shutdown_timeout: s.shutdown.timeout.as_secs().max(1),
    request_id_enabled: s.request_id.enabled,
    request_id_header,
    request_id_trust_inbound: s.request_id.trust_inbound,
    token_pinning: s.security.token_pinning,
    preview_noindex: s.routing.preview_noindex,
  })
}

/// The metrics token kept in the data directory when none is configured,
/// generated on the first call and the same on every one after.
fn persisted_metrics_token(data_dir: &str) -> String {
  let path = std::path::Path::new(data_dir).join("metrics_token");
  if let Some(token) = std::fs::read_to_string(&path)
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
  {
    return token;
  }
  let token = format!("mtr_{}", uuid::Uuid::new_v4().simple());
  if let Err(e) = std::fs::write(&path, &token) {
    tracing::error!("Failed to persist the generated metrics token to {path:?}: {e}");
  }
  warn!(
    "No metrics.token set; generated one and kept it in {path:?}. Scrape with \
     /aperio/metrics?token=<token> or an Authorization: Bearer header."
  );
  token
}

/// The startup lines an operator reads to see what posture the server is in.
fn log_posture(c: &ServerConfig) {
  if !c.trusted_proxies.is_empty() {
    info!(
      "Trusted proxy ranges configured ({} entries): client IPs resolve via the X-Forwarded-For chain walk",
      c.trusted_proxies.len()
    );
  }
  if let Some(h) = &c.real_ip_header {
    if c.trust_proxy {
      info!("Real client IP is read from the '{h}' header");
    } else {
      warn!("proxy.real_ip_header is set but no proxy is trusted; the header is ignored");
    }
  }
  if !c.admin_allowed_ips.is_empty() {
    info!(
      "Admin surface IP allowlist active ({} entries)",
      c.admin_allowed_ips.len()
    );
  }
  if !c.denied_ips.is_empty() {
    info!(
      "Source IP deny list active ({} entries)",
      c.denied_ips.len()
    );
  }
  if c.default_access == crate::settings::DefaultAccess::Deny {
    info!(
      "Closed by default: a proxied route is served only where a gate admits the visitor or the route is declared public"
    );
  }
  if c.cache_enabled {
    info!(
      "Response cache is enabled ({} byte budget) for services that opt in with cache: true",
      c.cache_max_bytes
    );
  }
  if c.failover_mode != crate::settings::FailoverMode::Fail {
    info!(
      "In-flight failover: {:?} (max {} jumps, {}s window)",
      c.failover_mode,
      c.failover_max_jumps,
      c.failover_window.as_secs()
    );
  }
  if let Some(p) = &c.random_subdomain_suffix {
    info!("Random subdomain assignment enabled: every client gets {p} (* = random label)");
  }
  let policy = &c.outbound_policy;
  if let Some(proxy) = &policy.egress.proxy {
    info!(
      "Outbound callbacks go through the proxy {}{}{}",
      proxy.redacted(),
      if proxy.has_credentials() {
        " (with a credential)"
      } else {
        ""
      },
      if policy.egress.bypass.is_empty() {
        String::new()
      } else {
        format!(", except {} destination(s)", policy.egress.bypass.len())
      }
    );
  }
  if policy.restricted() {
    info!(
      "Outbound callback policy active: {} allowlist entr{}, block_private={}",
      policy.allowlist.len(),
      if policy.allowlist.len() == 1 {
        "y"
      } else {
        "ies"
      },
      policy.block_private
    );
    // What a proxy takes away, said once and plainly. The policy decides by
    // resolving a destination here and looking at the addresses; through a
    // proxy the name is resolved elsewhere, so that half judges nothing and
    // is not run. An operator who set this believes something is gated, and
    // the honest thing is to name the part that is not.
    if policy.egress.proxy.is_some() {
      let cidrs = policy
        .allowlist
        .iter()
        .filter(|p| matches!(p, crate::outbound::OutboundPattern::Cidr(..)))
        .count();
      warn!(
        "Through the proxy this policy covers what the URL says as text: a literal address, \
         and hostname or *.suffix allowlist entries. It cannot cover a hostname's resolved \
         addresses, because the proxy resolves the name on its own network: \
         block_private={} applies to literal addresses only, and {cidrs} CIDR allowlist \
         entr{} cannot admit a named destination. Destinations on outbound.no_proxy are \
         dialed by this server and keep the whole policy.",
        policy.block_private,
        if cidrs == 1 { "y" } else { "ies" }
      );
    }
  }
  if c.ignore_client_auth {
    info!("access.ignore_client_auth is on: client-declared visitor gates are ignored");
  }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
