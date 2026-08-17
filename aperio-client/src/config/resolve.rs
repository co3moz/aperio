//! Layering the four sources into the settings the client runs on: CLI, then
//! `./aperio.yaml`, then the environment, then `~/.aperio.yaml`.
//!
//! One function, whole because the single column is the point, not because it
//! resists being cut. It was measured: three values cross any boundary inside
//! it (`local`, `home`, and the `nonempty` closure), the lowest count of
//! anything in this repo, and the mechanism for lifting a field out already
//! exists five times over, `resolve_scaling`, `resolve_hostnames`,
//! `resolve_connections`, `resolve_otel_bridge` and `resolve_metrics_labels`
//! each return one field's value. No partial structs, nothing to merge.
//!
//! It stays whole because sixty-six settings, each showing its four sources on
//! one line, is a table an operator and a reader can scan top to bottom. Lift
//! ten of them into helpers and the table has ten holes in it. The five that
//! were lifted are the ones that build a sub-object (`ScalingDecl`,
//! `OtelBridge`), which is a different thing from a setting.

use super::*;
use aperio_config::FileConfig;

/// Resolves every client setting from the four layers. Called at startup and
/// again on config hot-reload (with the freshly parsed files).
pub(crate) fn resolve_settings(
  cli: &CliArgs,
  home: &FileConfig,
  local: &FileConfig,
) -> Result<ClientSettings, String> {
  let o = &cli.opts;
  let nonempty = |s: String| {
    let t = s.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
  };
  // Reported rather than fatal: this runs on hot-reload too, where the
  // contract is to warn and keep the previous configuration. Exiting here
  // meant a typo saved into aperio.yaml killed a client that was serving
  // traffic, while every other invalid setting was merely rejected.
  let idle_timeout = match layered(
    None,
    local.idle_timeout.clone(),
    env_str("APERIO_IDLE_TIMEOUT"),
    home.idle_timeout.clone(),
  ) {
    None => None,
    Some(raw) => match crate::api::parse_duration(&raw) {
      Ok(0) => None,
      Ok(secs) => Some(secs),
      Err(e) => return Err(format!("invalid idle_timeout: {e}")),
    },
  };
  Ok(ClientSettings {
    token: layered(
      o.server_token.clone(),
      local.server_token(),
      env_str("APERIO_SERVER_TOKEN"),
      home.server_token(),
    ),
    scaling: resolve_scaling(local, home),
    idle_timeout,
    config_version: layered(
      None,
      local.version.clone(),
      env_str("APERIO_VERSION"),
      home.version.clone(),
    )
    .and_then(nonempty),
    server_urls: layered(
      None,
      local.server_urls(),
      env_str("APERIO_SERVER_URLS").map(|raw| split_csv(&raw)),
      home.server_urls(),
    )
    .unwrap_or_default(),
    serve_spa: layered(
      None,
      local.serve_spa,
      env_bool("APERIO_SERVE_SPA"),
      home.serve_spa,
    )
    .unwrap_or(false),
    serve_404: layered(
      None,
      local.serve_404.clone(),
      env_str("APERIO_SERVE_404"),
      home.serve_404.clone(),
    ),
    device_key: layered(
      None,
      local.device_key.clone(),
      env_str("APERIO_DEVICE_KEY"),
      home.device_key.clone(),
    ),
    device_key_file: layered(
      None,
      local.device_key_file.clone(),
      env_str("APERIO_DEVICE_KEY_FILE"),
      home.device_key_file.clone(),
    ),
    api_key: layered(
      o.api_key.clone(),
      local.server_api_key(),
      env_str("APERIO_API_KEY"),
      home.server_api_key(),
    ),
    server: layered(
      o.server_url.clone(),
      local.server_url(),
      env_str("APERIO_SERVER_URL"),
      home.server_url(),
    ),
    target: layered(
      cli.target.clone(),
      local.target.clone(),
      env_str("APERIO_TARGET"),
      home.target.clone(),
    ),
    serve: layered(
      o.serve.clone(),
      local.serve.clone(),
      env_str("APERIO_SERVE"),
      home.serve.clone(),
    )
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty()),
    hostnames: resolve_hostnames(o, local, home),
    path: layered(
      o.path.clone(),
      local.path.clone(),
      env_str("APERIO_PATH"),
      home.path.clone(),
    ),
    trim_bind: layered(
      None,
      local.trim_bind,
      env_bool("APERIO_TRIM_BIND"),
      home.trim_bind,
    ),
    pass_hostname: o.pass_hostname
      || layered(
        None,
        local.pass_hostname,
        env_bool("APERIO_PASS_HOSTNAME"),
        home.pass_hostname,
      )
      .unwrap_or(false),
    max_response_body: layered(
      None,
      local.max_response_body,
      env_parse("APERIO_MAX_RESPONSE_BODY"),
      home.max_response_body,
    )
    .unwrap_or(50 * 1024 * 1024),
    reload_drain_secs: layered(
      None,
      local.reload_drain,
      env_parse("APERIO_RELOAD_DRAIN"),
      home.reload_drain,
    )
    .unwrap_or(10),
    retry_attempts: layered(
      None,
      local.retry.as_ref().and_then(|r| r.attempts),
      env_parse("APERIO_RETRY_ATTEMPTS"),
      home.retry.as_ref().and_then(|r| r.attempts),
    )
    .unwrap_or(1)
    .clamp(1, 10),
    retry_backoff_ms: layered(
      None,
      local.retry.as_ref().and_then(|r| r.backoff),
      env_parse("APERIO_RETRY_BACKOFF"),
      home.retry.as_ref().and_then(|r| r.backoff),
    )
    .unwrap_or(100),
    retry_all_methods: layered(
      None,
      local.retry.as_ref().and_then(|r| r.all_methods),
      env_bool("APERIO_RETRY_ALL_METHODS"),
      home.retry.as_ref().and_then(|r| r.all_methods),
    )
    .unwrap_or(false),
    breaker_failures: layered(
      None,
      local.circuit_breaker.as_ref().and_then(|b| b.failures),
      env_parse("APERIO_BREAKER_FAILURES"),
      home.circuit_breaker.as_ref().and_then(|b| b.failures),
    )
    .unwrap_or(0),
    breaker_open_for_secs: layered(
      None,
      local.circuit_breaker.as_ref().and_then(|b| b.open_for),
      env_parse("APERIO_BREAKER_OPEN_FOR"),
      home.circuit_breaker.as_ref().and_then(|b| b.open_for),
    )
    .unwrap_or(30)
    .max(1),
    max_request_body: layered(
      None,
      local.max_request_body,
      env_parse("APERIO_MAX_REQUEST_BODY"),
      home.max_request_body,
    ),
    response_timeout: layered(
      None,
      local.response_timeout,
      env_parse("APERIO_RESPONSE_TIMEOUT"),
      home.response_timeout,
    ),
    timeout_secs: layered(
      None,
      local.timeout,
      env_parse("APERIO_TIMEOUT"),
      home.timeout,
    )
    .unwrap_or(30),
    max_concurrent: layered(
      o.max_concurrent,
      local.max_concurrent,
      env_parse("APERIO_MAX_CONCURRENT"),
      home.max_concurrent,
    )
    .filter(|n| *n > 0),
    connections: resolve_connections(local, home),
    multiplex: layered(
      None,
      local.multiplex,
      env_bool("APERIO_MULTIPLEX"),
      home.multiplex,
    )
    .unwrap_or(false),
    connect_timeout: layered(
      None,
      local.connect_timeout,
      env_parse("APERIO_CONNECT_TIMEOUT"),
      home.connect_timeout,
    )
    .filter(|n| *n > 0),
    min_tls_version: layered(
      None,
      local.min_tls_version.clone(),
      env_str("APERIO_MIN_TLS_VERSION"),
      home.min_tls_version.clone(),
    ),
    adaptive_concurrency: layered(
      None,
      local.adaptive_concurrency,
      env_bool("APERIO_ADAPTIVE_CONCURRENCY"),
      home.adaptive_concurrency,
    )
    .unwrap_or(false),
    otel_bridge: resolve_otel_bridge(local, home),
    startup_delay: layered(
      None,
      local.startup_delay,
      env_parse("APERIO_STARTUP_DELAY"),
      home.startup_delay,
    ),
    pid_file: layered(
      None,
      local.pid_file.clone(),
      env_str("APERIO_PID_FILE"),
      home.pid_file.clone(),
    ),
    metrics_labels: resolve_metrics_labels(local, home),
    priority: layered(
      o.priority,
      local.priority,
      env_parse("APERIO_PRIORITY"),
      home.priority,
    )
    .unwrap_or(0),
    bandwidth: layered(
      None,
      local.bandwidth.clone(),
      env_str("APERIO_BANDWIDTH"),
      home.bandwidth.clone(),
    ),
    max_message_size: layered(
      None,
      local.max_message_size,
      env_parse("APERIO_MAX_MESSAGE_SIZE"),
      home.max_message_size,
    )
    .unwrap_or(32 * 1024 * 1024),
    max_redirects: layered(
      None,
      local.max_redirects,
      env_parse("APERIO_MAX_REDIRECTS"),
      home.max_redirects,
    )
    .unwrap_or(5),
    tcp_target: layered(
      None,
      local.tcp_target.clone(),
      env_str("APERIO_TCP_TARGET"),
      home.tcp_target.clone(),
    )
    .and_then(nonempty),
    custom_name: layered(
      None,
      local.custom_name.clone(),
      env_str("APERIO_CUSTOM_NAME"),
      home.custom_name.clone(),
    )
    .and_then(nonempty),
    target_health: layered(
      None,
      local.target_health.clone(),
      env_str("APERIO_TARGET_HEALTH"),
      home.target_health.clone(),
    )
    .and_then(nonempty),
    wait_for_backend: layered(
      None,
      local.wait_for_backend,
      env_bool("APERIO_WAIT_FOR_BACKEND"),
      home.wait_for_backend,
    )
    .unwrap_or(false),
    health_interval: layered(
      None,
      local.health_interval,
      env_parse("APERIO_HEALTH_INTERVAL"),
      home.health_interval,
    )
    .unwrap_or(10)
    .max(1),
    health_timeout: layered(
      None,
      local.health_timeout,
      env_parse("APERIO_HEALTH_TIMEOUT"),
      home.health_timeout,
    )
    .unwrap_or(5)
    .max(1),
    health_threshold: layered(
      None,
      local.health_threshold,
      env_parse("APERIO_HEALTH_THRESHOLD"),
      home.health_threshold,
    )
    .unwrap_or(2)
    .max(1),
    public: o.public
      || layered(None, local.public, env_bool("APERIO_PUBLIC"), home.public).unwrap_or(false),
    // The CLI flag and the environment variable are scalars, so they can only
    // ever mean `basic` with one credential; a file may say more than that.
    visitor_auth: layered(
      o.visitor_auth
        .clone()
        .map(aperio_config::AuthSetting::Credentials),
      local.auth.clone(),
      env_str("APERIO_VISITOR_AUTH").map(aperio_config::AuthSetting::Credentials),
      home.auth.clone(),
    )
    .filter(|a| !matches!(a, aperio_config::AuthSetting::Credentials(c) if c.trim().is_empty())),
    allowed_ips: layered(
      o.allowed_ips.clone().map(|s| split_ip_list(&s)),
      local.allowed_ips.clone(),
      env_str("APERIO_ALLOWED_IPS").map(|s| split_ip_list(&s)),
      home.allowed_ips.clone(),
    )
    .unwrap_or_default(),
    headers: local.headers.clone().or_else(|| home.headers.clone()),
    security_headers: local
      .security_headers
      .clone()
      .or_else(security_headers_from_env)
      .or_else(|| home.security_headers.clone()),
    cache: layered(None, local.cache, env_bool("APERIO_CACHE"), home.cache).unwrap_or(false),
    resilience: o.resilience
      || layered(
        None,
        local.resilience,
        env_bool("APERIO_RESILIENCE"),
        home.resilience,
      )
      .unwrap_or(false),
    // On by default, so the layers carry the opt-out: `--no-capture`, or
    // `capture: false`, or `APERIO_CAPTURE=0`.
    capture: !o.no_capture
      && layered(
        None,
        local.capture,
        env_bool("APERIO_CAPTURE"),
        home.capture,
      )
      .unwrap_or(true),
    webhook_inbox: layered(
      None,
      local.webhook_inbox,
      env_bool("APERIO_WEBHOOK_INBOX"),
      home.webhook_inbox,
    )
    .unwrap_or(false),
    denied: layered(
      None,
      local.denied.clone(),
      env_str("APERIO_DENIED"),
      home.denied.clone(),
    )
    .and_then(nonempty),
    ip_family: crate::dial::IpFamily::parse(
      layered(
        o.ip_family.clone(),
        local.ip_family.clone(),
        env_str("APERIO_IP_FAMILY"),
        home.ip_family.clone(),
      )
      .as_deref(),
    ),
    // Unlike its neighbours this one can fail the resolution, because the
    // value is a floor: see `TlsPolicy::parse`. On hot-reload that means the
    // edit is refused and the previous configuration keeps serving, which is
    // what every other unresolvable file does here.
    tls_policy: crate::dial::TlsPolicy::parse(
      layered(
        None,
        local.tls_min_version.clone(),
        env_str("APERIO_TLS_MIN_VERSION"),
        home.tls_min_version.clone(),
      )
      .as_deref(),
      layered(
        None,
        local.tls_cipher_suites.clone(),
        env_str("APERIO_TLS_CIPHER_SUITES"),
        home.tls_cipher_suites.clone(),
      )
      .as_deref(),
    )?,
    // Refused rather than defaulted, on the same reasoning as the TLS floor
    // above: a client told to go through a proxy is on a network where going
    // direct does not work, so quietly ignoring a value it could not read
    // would produce a connection failure whose cause is a typo three layers
    // away.
    egress_proxy: match layered(
      o.egress_proxy.clone(),
      local.egress_proxy.clone(),
      env_str("APERIO_EGRESS_PROXY"),
      home.egress_proxy.clone(),
    )
    .and_then(nonempty)
    {
      Some(raw) => Some(crate::egress::EgressProxy::parse(&raw)?),
      None => None,
    },
    // The three list/map sections layer like every other key: a local file
    // that declares one replaces the home file's, and a home file alone is
    // used as written. They used to be read from the local file only, so a
    // `services:` (or `tunnels:`/`bind-tunnels:`) block in ~/.aperio.yaml was
    // silently ignored while every neighbouring key was honoured.
    services: local
      .services
      .clone()
      .or_else(|| home.services.clone())
      .unwrap_or_default(),
    client_id: layered(
      o.client_id.clone(),
      local.client_id.clone(),
      env_str("APERIO_CLIENT_ID"),
      home.client_id.clone(),
    )
    .and_then(nonempty),
    tunnels: local
      .tunnels
      .clone()
      .or_else(|| home.tunnels.clone())
      .unwrap_or_default(),
    bind_tunnels: local
      .bind_tunnels
      .clone()
      .or_else(|| home.bind_tunnels.clone())
      .unwrap_or_default(),
    subscribe: layered(
      None,
      local.subscribe.clone(),
      // The environment can only carry filters: a command to run is not
      // something to pick up from an env var, and the file is where an
      // operator writes down what may execute here.
      env_str("APERIO_SUBSCRIBE").map(|v| {
        split_ip_list(&v)
          .into_iter()
          .map(aperio_config::SubscribeValue::Filter)
          .collect()
      }),
      home.subscribe.clone(),
    )
    .unwrap_or_default()
    .iter()
    .map(aperio_config::SubscribeValue::entry)
    .collect(),
    messages_listen: layered(
      None,
      local.messages_listen.clone(),
      env_str("APERIO_MESSAGES_LISTEN"),
      home.messages_listen.clone(),
    )
    .and_then(nonempty),
    messages_mqtt_listen: layered(
      None,
      local.messages_mqtt_listen.clone(),
      env_str("APERIO_MESSAGES_MQTT_LISTEN"),
      home.messages_mqtt_listen.clone(),
    )
    .and_then(nonempty),
  })
}
