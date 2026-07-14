use axum::{
  Router,
  body::Body,
  extract::ws::Message,
  http::StatusCode,
  response::Response,
  routing::{any, get},
};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, watch};
use tracing::{error, info, warn};

mod access_log;
mod alerts;
mod api;
mod auth;
mod cache;
mod config_file;
mod expose;
mod headers;
mod oidc;
mod protocol;
mod proxy;
mod routing;
mod settings;
mod share;
mod state;
mod static_routes;
mod store;
mod telemetry;
mod totp;
mod tunnel;
mod webauthn;

use crate::api::clients::{
  client_enabled_handler, client_override_handler, live_stream_handler, logs_handler,
  stats_handler, stats_history_handler, uptime_handler,
};
use crate::api::inspector::{request_detail_handler, request_replay_handler};
use crate::api::maintenance::{maintenance_list_handler, maintenance_set_handler};
use crate::api::metrics::metrics_handler;
use crate::api::settings::{settings_get_handler, settings_put_handler};
use crate::api::tokens::{
  tokens_create_handler, tokens_list_handler, tokens_refresh_handler, tokens_revoke_handler,
  tokens_update_handler,
};
use crate::api::tunnels::{tunnels_create_handler, tunnels_delete_handler};
use crate::api::webhooks::{
  audit_handler, webhooks_create_handler, webhooks_delete_handler, webhooks_list_handler,
};
use crate::api::{dashboard_asset_handler, dashboard_handler, health_handler};
use crate::auth::{
  auth_login_handler, auth_logout_handler, auth_page_handler, auth_session_handler,
  oidc_callback_handler, oidc_login_handler, safe_redirect_path,
};
use crate::protocol::TunnelMessage;
use crate::proxy::proxy_handler;
use crate::routing::normalize_random_subdomain_pattern;
use crate::settings::{
  FailoverMode, LbStrategy, ServerConfig, SettingsOverrides, apply_settings_overrides,
  override_keys, parse_failover_mode, parse_lb_strategy,
};
use crate::share::share_create_handler;
use crate::state::{
  AppState, CAPTURE_MAX_ENTRIES, ConnectionState, DurationHistogram, ServerStats,
};
use crate::store::audit::AuditLog;
use crate::store::stats::StatsStore;
use crate::store::tokens::TokenStore;
use crate::store::webhooks::WebhookStore;
use crate::tunnel::tcp::{tcp_ws_handler, tunnels_list_handler, udp_ws_handler};
use crate::tunnel::ws::ws_handler;

/// Entry point for the Aperio server.
/// Loads `aperio-server.yaml` into the environment while still
/// single-threaded, then hands over to the async server on a multi-thread
/// runtime.
fn main() {
  // Pin the process-wide rustls provider to ring. The dependency tree pulls
  // rustls with both `ring` and `aws-lc-rs` enabled (workspace feature
  // unification), and with two providers rustls refuses to auto-select one —
  // the first outbound TLS call (webhooks, OIDC, OTLP) would panic without
  // this.
  let _ = rustls::crypto::ring::default_provider().install_default();

  // `aperio-server --version` must print and exit instead of starting the
  // server (used by installers and packaging).
  if matches!(
    std::env::args().nth(1).as_deref(),
    Some("--version" | "-V" | "version")
  ) {
    println!("aperio-server {}", env!("CARGO_PKG_VERSION"));
    return;
  }

  // Must happen before the runtime exists: the loader writes environment
  // variables, which is only sound while no other thread can read them.
  config_file::load();

  tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("failed to build the tokio runtime")
    .block_on(async_main());
}

/// The asynchronous server proper: sets up logging, reads env config,
/// registers paths/middleware, and binds the TCP listener.
async fn async_main() {
  // Initialize tracing with structured JSON output (pino.js style), plus the
  // optional OpenTelemetry OTLP export layer (APERIO_OTEL). The returned guard
  // flushes buffered spans on graceful shutdown.
  let log_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
    let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::EnvFilter::new(level)
  });
  let otel_guard = telemetry::init(log_filter);

  info!("Starting Aperio Server...");

  // Enforce APERIO_SERVER_TOKEN environment variable
  let token = std::env::var("APERIO_SERVER_TOKEN").unwrap_or_else(|_| {
    error!("CRITICAL SECURITY ERROR: APERIO_SERVER_TOKEN environment variable must be set!");
    std::process::exit(1);
  });
  if token.trim().is_empty() {
    error!("CRITICAL SECURITY ERROR: APERIO_SERVER_TOKEN cannot be empty!");
    std::process::exit(1);
  }

  let gateway_timeout_secs = std::env::var("APERIO_SERVER_GATEWAY_TIMEOUT")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .unwrap_or(10);

  let gateway_response_timeout_secs = std::env::var("APERIO_SERVER_GATEWAY_RESPONSE_TIMEOUT")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .unwrap_or(30);

  // Limit on max request body size (default: 10MB)
  let max_body_size = std::env::var("APERIO_MAX_BODY_SIZE")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(10 * 1024 * 1024);

  // Concurrency limit on tunnel requests (default: 100 concurrent)
  let max_concurrent_requests = std::env::var("APERIO_MAX_CONCURRENT_REQUESTS")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(100);

  // Max connected tunnel clients limit (default: 10 active clients)
  let max_tunnels = std::env::var("APERIO_MAX_TUNNELS")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(10);

  // Max IP token bucket capacity burst (default: 100 requests)
  let ip_limit_max = std::env::var("APERIO_IP_LIMIT_MAX")
    .ok()
    .and_then(|val| val.parse::<f64>().ok())
    .unwrap_or(100.0);

  // IP token bucket refill rate per second (default: 5.0 requests/sec, which is 300 req/min)
  let ip_limit_refill = std::env::var("APERIO_IP_LIMIT_REFILL")
    .ok()
    .and_then(|val| val.parse::<f64>().ok())
    .unwrap_or(5.0);

  // Optional Basic Auth credentials for proxied requests ("username:password")
  let auth_credentials = std::env::var("APERIO_SERVER_AUTH").ok();

  // Trust proxy headers (X-Forwarded-For / X-Real-IP) for client IP resolution.
  // Only enable when running behind a trusted reverse proxy that overwrites
  // these headers; otherwise clients can spoof them to bypass rate limiting.
  let trust_proxy = std::env::var("APERIO_TRUST_PROXY")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);

  // When enabled, the server ignores any client-declared visitor password
  // override and keeps full control of the visitor gate with its own settings.
  let ignore_client_auth = std::env::var("APERIO_IGNORE_CLIENT_AUTH")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  if ignore_client_auth {
    info!(
      "APERIO_IGNORE_CLIENT_AUTH is set: client-declared visitor password overrides are ignored"
    );
  }

  // Optional real-IP header consulted before X-Forwarded-For (only with
  // trust_proxy). Needed behind CDN → proxy chains where the proxy resets
  // XFF to the CDN edge address, e.g. APERIO_REAL_IP_HEADER=CF-Connecting-IP.
  // APERIO_TRUST_CF_HEADER=1 is shorthand for the common Cloudflare chain: it
  // resolves to APERIO_REAL_IP_HEADER=CF-Connecting-IP (an explicit
  // APERIO_REAL_IP_HEADER still wins). Deliberately opt-in — any visitor can
  // send that header, so trusting it automatically would let clients spoof
  // their IP for rate limiting, audit logs, and token IP allowlists on
  // deployments that are not actually behind Cloudflare.
  let trust_cf_header = std::env::var("APERIO_TRUST_CF_HEADER")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let real_ip_header = std::env::var("APERIO_REAL_IP_HEADER")
    .ok()
    .map(|v| v.trim().to_ascii_lowercase())
    .filter(|v| !v.is_empty())
    .or_else(|| trust_cf_header.then(|| "cf-connecting-ip".to_string()));
  // Trusted proxy/CDN egress ranges (comma-separated IPs/CIDRs). When set,
  // the client IP is resolved by walking the X-Forwarded-For chain from the
  // nearest hop backwards past trusted addresses — the CDN-agnostic model
  // that works for any proxy chain. Implies trust_proxy.
  let trusted_proxies = match std::env::var("APERIO_TRUSTED_PROXIES") {
    Ok(raw) => match crate::routing::parse_trusted_proxies(&raw) {
      Ok(list) => list,
      Err(e) => {
        error!(
          "APERIO_TRUSTED_PROXIES is invalid ({e}); refusing to start with a partial trusted set"
        );
        return;
      }
    },
    Err(_) => Vec::new(),
  };
  let trust_proxy = trust_proxy || !trusted_proxies.is_empty();
  if !trusted_proxies.is_empty() {
    info!(
      "Trusted proxy ranges configured ({} entries): client IPs resolve via the X-Forwarded-For chain walk",
      trusted_proxies.len()
    );
  }
  if let Some(ref h) = real_ip_header {
    if trust_proxy {
      info!("Real client IP is read from the '{}' header", h);
    } else {
      warn!(
        "APERIO_REAL_IP_HEADER / APERIO_TRUST_CF_HEADER is set but APERIO_TRUST_PROXY is off; the header is ignored"
      );
    }
  }

  // When true, session cookies include the `Secure` flag (HTTPS-only).
  // Defaults to `trust_proxy` since a TLS-terminating reverse proxy implies HTTPS.
  let secure_cookies = std::env::var("APERIO_SECURE_COOKIES")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(trust_proxy);

  // When enabled, clients that did not declare a hostname bind (and were not
  // given one via dashboard overrule) are excluded from load balancing.
  let require_hostname_bind = std::env::var("APERIO_REQUIRE_HOSTNAME_BIND")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);

  // Prometheus metrics endpoint (default: disabled). Auth is always required:
  // either APERIO_METRICS_TOKEN, or a random token generated once and
  // persisted in the data directory (a truly public metrics endpoint brings
  // no benefit and leaks operational details).
  let metrics_enabled = std::env::var("APERIO_METRICS")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let metrics_token = std::env::var("APERIO_METRICS_TOKEN")
    .ok()
    .filter(|t| !t.trim().is_empty());

  // Server-side GET response cache (default: disabled). Only effective for
  // clients that announce `cache: true`, and strictly Cache-Control-driven.
  let cache_enabled = std::env::var("APERIO_CACHE")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  // Mark random-subdomain (preview) services as non-indexable.
  let preview_noindex = std::env::var("APERIO_PREVIEW_NOINDEX")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let cache_max_bytes = std::env::var("APERIO_CACHE_MAX_BYTES")
    .ok()
    .and_then(|v| v.trim().parse::<u64>().ok())
    .filter(|v| *v > 0)
    .unwrap_or(64 * 1024 * 1024);
  // Serve-stale window for resilient services (#69 semantics): how long an
  // expired cached response may still answer visitors during an outage.
  let cache_max_stale = std::env::var("APERIO_CACHE_MAX_STALE")
    .ok()
    .and_then(|v| v.trim().parse::<u64>().ok())
    .unwrap_or(3600);
  if cache_enabled {
    info!(
      "Response cache is enabled ({} byte budget) for services that opt in with cache: true",
      cache_max_bytes
    );
  }

  // Tunnel frame compression (zlib). Offered to clients on connect; enabled
  // per connection once the client acknowledges support.
  let tunnel_compression = std::env::var("APERIO_TUNNEL_COMPRESSION")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  if tunnel_compression {
    info!("Tunnel compression is enabled (zlib per-message)");
  }

  // Optional custom 504 error page (e.g. APERIO_504_PAGE=/app/error_504.html).
  // Loaded once at startup; on read failure the default plain-text 504 is kept.
  let custom_504_page =
    std::env::var("APERIO_504_PAGE")
      .ok()
      .and_then(|path| match std::fs::read_to_string(&path) {
        Ok(html) => {
          info!("Custom 504 page loaded from {}", path);
          Some(html)
        }
        Err(e) => {
          error!(
            "Failed to read APERIO_504_PAGE {}: {} — using default 504 text",
            path, e
          );
          None
        }
      });

  // Structured access log: APERIO_ACCESS_LOG=<path> appends one JSON line
  // per proxied request to the file (in addition to the structured
  // aperio_access tracing events that always go to stdout).
  let access_log = std::env::var("APERIO_ACCESS_LOG")
    .ok()
    .map(|p| p.trim().to_string())
    .filter(|p| !p.is_empty())
    .and_then(|path| {
      match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
      {
        Ok(file) => {
          info!("Structured access log enabled: {}", path);
          Some(std::sync::Mutex::new(file))
        }
        Err(e) => {
          error!(
            "Failed to open APERIO_ACCESS_LOG {}: {} — access log file disabled",
            path, e
          );
          None
        }
      }
    });

  // Optional custom maintenance page (APERIO_503_PAGE=/app/maintenance.html).
  let custom_503_page =
    std::env::var("APERIO_503_PAGE")
      .ok()
      .and_then(|path| match std::fs::read_to_string(&path) {
        Ok(html) => {
          info!("Custom 503 maintenance page loaded from {}", path);
          Some(html)
        }
        Err(e) => {
          error!(
            "Failed to read APERIO_503_PAGE {}: {} — using default 503 text",
            path, e
          );
          None
        }
      });

  // Load-balancing strategy applied after routing narrows the pool.
  let lb_strategy_raw = std::env::var("APERIO_LB_STRATEGY").unwrap_or_default();
  let lb_strategy = parse_lb_strategy(&lb_strategy_raw).unwrap_or_else(|| {
    warn!(
      "Unknown APERIO_LB_STRATEGY '{}' (expected 'round-robin', 'primary-standby' or 'sticky'); using round-robin",
      lb_strategy_raw
    );
    LbStrategy::RoundRobin
  });
  if lb_strategy != LbStrategy::RoundRobin {
    info!("Load balancing strategy: {:?}", lb_strategy);
  }

  // In-flight failover: what to do when a client dies mid-request.
  let failover_raw = std::env::var("APERIO_FAILOVER").unwrap_or_default();
  let failover_mode = parse_failover_mode(&failover_raw).unwrap_or_else(|| {
    warn!(
      "Unknown APERIO_FAILOVER '{}' (expected 'fail', 'retry', 'wait' or 'retry-wait'); using fail",
      failover_raw
    );
    FailoverMode::Fail
  });
  let failover_max_jumps = std::env::var("APERIO_FAILOVER_MAX_JUMPS")
    .ok()
    .and_then(|val| val.parse::<u32>().ok())
    .unwrap_or(2);
  let failover_window = Duration::from_secs(
    std::env::var("APERIO_FAILOVER_WINDOW")
      .ok()
      .and_then(|val| val.parse::<u64>().ok())
      .unwrap_or(15),
  );
  let failover_all_methods = std::env::var("APERIO_FAILOVER_ALL_METHODS")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  if failover_mode != FailoverMode::Fail {
    info!(
      "In-flight failover enabled: {:?} (max {} jumps, {}s window{})",
      failover_mode,
      failover_max_jumps,
      failover_window.as_secs(),
      if failover_all_methods {
        ", all methods"
      } else {
        ", idempotent methods only"
      }
    );
  }

  // Heartbeat-based health: clients whose last Ping is older than this many
  // seconds are treated as down and excluded from load balancing.
  let client_down_threshold_secs = std::env::var("APERIO_CLIENT_DOWN_THRESHOLD")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .filter(|n| *n > 0)
    .unwrap_or(15);

  // Random subdomain assignment: APERIO_RANDOM_SUBDOMAIN="*.example.com"
  // gives every connecting client a random hostname under that suffix.
  let random_subdomain_suffix = std::env::var("APERIO_RANDOM_SUBDOMAIN")
    .ok()
    .and_then(|val| {
      match normalize_random_subdomain_pattern(&val) {
        Some(s) => Some(s),
        None => {
          error!(
            "Invalid APERIO_RANDOM_SUBDOMAIN value '{}' (expected e.g. \"example.com\", \"*.example.com\", or \"*-test.example.com\"); ignoring",
            val
          );
          None
        }
      }
    });
  if let Some(ref pattern) = random_subdomain_suffix {
    info!(
      "Random subdomain assignment enabled: every client gets {} (* = random label)",
      pattern
    );
  }

  // Data directory for persisted state (dynamic tokens, etc.). In Docker,
  // mount a volume here (e.g. ./data:/app/data) so tokens survive restarts.
  let data_dir = std::env::var("APERIO_DATA_DIR").unwrap_or_else(|_| "./data".to_string());
  let token_store = TokenStore::load(&data_dir);

  // Resolve the effective metrics token: env var wins; otherwise generate a
  // random token once and persist it so every restart uses the same value.
  let metrics_token = if metrics_enabled && metrics_token.is_none() {
    let path = std::path::Path::new(&data_dir).join("metrics_token");
    let persisted = std::fs::read_to_string(&path)
      .ok()
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty());
    match persisted {
      Some(tok) => {
        warn!(
          "APERIO_METRICS_TOKEN not set; using the persisted random metrics token from {:?}. \
           Scrape with /aperio/metrics?token=<token> or an Authorization: Bearer header.",
          path
        );
        Some(tok)
      }
      None => {
        let tok = format!("mtr_{}", uuid::Uuid::new_v4().simple());
        if let Err(e) = std::fs::write(&path, &tok) {
          error!(
            "Failed to persist generated metrics token to {:?}: {}",
            path, e
          );
        }
        warn!(
          "APERIO_METRICS_TOKEN not set; generated a random metrics token: {} (persisted in {:?}). \
           Scrape with /aperio/metrics?token=<token>. This value is logged only on first generation.",
          tok, path
        );
        Some(tok)
      }
    }
  } else {
    metrics_token
  };

  let config = ServerConfig {
    token: token.clone(),
    gateway_timeout: Duration::from_secs(gateway_timeout_secs),
    gateway_response_timeout: Duration::from_secs(gateway_response_timeout_secs),
    max_body_size,
    max_tunnels,
    ip_limit_max,
    ip_limit_refill,
    auth_credentials,
    trust_proxy,
    ignore_client_auth,
    real_ip_header,
    trusted_proxies,
    secure_cookies,
    require_hostname_bind,
    metrics_token,
    random_subdomain_suffix,
    client_down_threshold: Duration::from_secs(client_down_threshold_secs),
    tunnel_compression,
    custom_504_page,
    custom_503_page,
    lb_strategy,
    failover_mode,
    failover_max_jumps,
    failover_window,
    failover_all_methods,
    cache_enabled,
    cache_max_bytes,
    cache_max_stale,
    max_concurrent_requests,
    login_lockout_threshold: std::env::var("APERIO_LOGIN_LOCKOUT_THRESHOLD")
      .ok()
      .and_then(|v| v.parse::<u32>().ok())
      .unwrap_or(5),
    login_lockout_secs: std::env::var("APERIO_LOGIN_LOCKOUT_SECS")
      .ok()
      .and_then(|v| v.parse::<u64>().ok())
      .unwrap_or(60),
    // Audit log rotation: the active audit.jsonl is rotated once it exceeds
    // this size in bytes (0 = never rotate), keeping the configured number
    // of older generations (audit.jsonl.1 ..).
    audit_max_size: std::env::var("APERIO_AUDIT_MAX_SIZE")
      .ok()
      .and_then(|v| v.parse::<u64>().ok())
      .unwrap_or(10 * 1024 * 1024),
    audit_max_files: std::env::var("APERIO_AUDIT_MAX_FILES")
      .ok()
      .and_then(|v| v.parse::<usize>().ok())
      .unwrap_or(3),
    ui_language: std::env::var("APERIO_UI_LANGUAGE")
      .ok()
      .map(|v| v.trim().to_ascii_lowercase())
      .filter(|v| crate::settings::UI_LANGUAGES.contains(&v.as_str()))
      .unwrap_or_else(|| "en".to_string()),
    header_rules: headers::from_config_file(),
    static_routes: static_routes::from_config_file(),
    preview_noindex,
  };

  // Dashboard-editable settings: env-derived values are the defaults, and
  // overrides persisted from earlier dashboard edits apply on top.
  let settings_path = std::path::PathBuf::from(&data_dir).join("settings.json");
  let settings_overrides = std::fs::read_to_string(&settings_path)
    .ok()
    .and_then(
      |raw| match serde_json::from_str::<SettingsOverrides>(&raw) {
        Ok(o) => Some(o),
        Err(e) => {
          error!(
            "Failed to parse {:?}: {} — ignoring persisted settings",
            settings_path, e
          );
          None
        }
      },
    )
    .unwrap_or_default();
  let overridden = override_keys(&settings_overrides);
  if !overridden.is_empty() {
    info!(
      "Applying persisted dashboard settings from {:?} (overridden: {:?})",
      settings_path, overridden
    );
  }
  let config_env_defaults = Arc::new(config);
  let config = apply_settings_overrides(&config_env_defaults, &settings_overrides);

  if require_hostname_bind {
    info!(
      "Hostname bind requirement is ENABLED: clients without a hostname bind will not receive traffic."
    );
  }

  // OIDC SSO configuration (optional).
  let oidc_runtime = oidc::load_from_env().await;

  // Copied out before config moves into the state (values needed by the
  // live structures below).
  let lockout_threshold = config.login_lockout_threshold;
  let lockout_secs = config.login_lockout_secs;
  let audit_max_size = config.audit_max_size;
  let audit_max_files = config.audit_max_files;

  // Dashboard defaults to enabled. Set APERIO_DASHBOARD=0 to disable.
  let dashboard_enabled = !std::env::var("APERIO_DASHBOARD")
    .map(|val| val == "0" || val.to_lowercase() == "false")
    .unwrap_or(false);

  let (client_connected_tx, _) = watch::channel(false);
  let (shutdown_tx, _) = watch::channel(false);
  // Live traffic fan-out to dashboard SSE subscribers. A bounded buffer means a
  // slow/absent subscriber can only fall behind (RecvError::Lagged, skipped on
  // the read side), never apply backpressure to request handling.
  let (traffic_tx, _) = tokio::sync::broadcast::channel(256);

  let state = Arc::new(AppState {
    clients: Mutex::new(HashMap::new()),
    client_connected: client_connected_tx,
    dashboard_enabled,
    shutdown: shutdown_tx,
    connection_state: Mutex::new(ConnectionState {
      connected: false,
      last_disconnect: None,
    }),
    server_start_time: Instant::now(),
    pending_requests: Mutex::new(HashMap::new()),
    stats: Mutex::new(ServerStats {
      total_requests: 0,
      successful_requests: 0,
      failed_requests: 0,
      total_bytes_transferred: 0,
    }),
    recent_logs: Mutex::new(VecDeque::with_capacity(100)),
    traffic_tx,
    config_store: std::sync::RwLock::new(Arc::new(config)),
    config_env_defaults,
    settings_overrides: Mutex::new(settings_overrides),
    settings_path,
    active_proxied_requests: Arc::new(AtomicUsize::new(0)),
    path_rr: Mutex::new(HashMap::new()),
    sessions: Mutex::new(crate::store::sessions::SessionStore::load(&data_dir)),
    rate_limiter: Mutex::new(HashMap::new()),
    login_lockout: Mutex::new(crate::auth::LockoutTracker::new(
      lockout_threshold,
      Duration::from_secs(lockout_secs),
    )),
    token_rate: Mutex::new(HashMap::new()),
    token_daily_bytes: Mutex::new(HashMap::new()),
    last_session_gc: Mutex::new(Instant::now()),
    last_rate_gc: Mutex::new(Instant::now()),
    active_tunnel_count: AtomicUsize::new(0),
    ws_streams: Mutex::new(HashMap::new()),
    pending_upgrades: Mutex::new(HashMap::new()),
    token_store: Mutex::new(token_store),
    users: Mutex::new(crate::store::users::UserStore::load(&data_dir)),
    response_streams: Mutex::new(HashMap::new()),
    captured_requests: Mutex::new(VecDeque::with_capacity(CAPTURE_MAX_ENTRIES)),
    audit: Mutex::new(AuditLog::load(&data_dir, audit_max_size, audit_max_files)),
    persistent_stats: Mutex::new(StatsStore::load(&data_dir)),
    webhook_store: Mutex::new(WebhookStore::load(&data_dir)),
    webauthn: crate::webauthn::build_webauthn(),
    webauthn_ceremonies: Mutex::new(crate::webauthn::WebauthnCeremonies::default()),
    uptime: Mutex::new(crate::store::uptime::UptimeStore::load(&data_dir)),
    oidc: oidc_runtime,
    oidc_states: Mutex::new(HashMap::new()),
    tcp_streams: Mutex::new(HashMap::new()),
    udp_streams: Mutex::new(HashMap::new()),
    response_cache: Mutex::new(crate::cache::ResponseCache::default()),
    maintenance: Mutex::new(std::collections::HashSet::new()),
    access_log,
    duration_histogram: DurationHistogram::default(),
  });

  let mut app = Router::new().fallback(any(proxy_handler));

  if dashboard_enabled {
    let dashboard_auth = std::env::var("APERIO_DASHBOARD_AUTH").ok();
    if dashboard_auth.is_none() || dashboard_auth.as_ref().unwrap().trim().is_empty() {
      warn!(
        "APERIO_DASHBOARD is enabled but APERIO_DASHBOARD_AUTH is not set! \
           The dashboard can still be accessed using aperio:<APERIO_SERVER_TOKEN>."
      );
    }

    let mut dash_router = Router::new()
      .route("/", get(dashboard_handler))
      .route("/api/stats", get(stats_handler))
      .route("/api/stats/history", get(stats_history_handler))
      .route("/api/uptime", get(uptime_handler))
      .route("/api/logs", get(logs_handler))
      .route("/api/stream", get(live_stream_handler))
      .route("/api/session", get(auth_session_handler))
      .route(
        "/api/clients/:id/override",
        axum::routing::post(client_override_handler),
      )
      .route(
        "/api/clients/:id/enabled",
        axum::routing::post(client_enabled_handler),
      )
      .route(
        "/api/tokens",
        get(tokens_list_handler).post(tokens_create_handler),
      )
      .route(
        "/api/tokens/:id",
        axum::routing::put(tokens_update_handler).delete(tokens_revoke_handler),
      )
      .route("/api/requests/:id", get(request_detail_handler))
      .route(
        "/api/requests/:id/replay",
        axum::routing::post(request_replay_handler),
      )
      .route("/api/audit", get(audit_handler))
      .route(
        "/api/maintenance",
        get(maintenance_list_handler).post(maintenance_set_handler),
      )
      .route("/api/share", axum::routing::post(share_create_handler))
      .route(
        "/api/settings",
        get(settings_get_handler).put(settings_put_handler),
      )
      .route("/api/export", get(crate::api::export::export_handler))
      .route(
        "/api/import",
        axum::routing::post(crate::api::export::import_handler),
      )
      .route(
        "/api/webhooks",
        get(webhooks_list_handler).post(webhooks_create_handler),
      )
      .route(
        "/api/webhooks/:id",
        axum::routing::delete(webhooks_delete_handler),
      )
      .route(
        "/api/openapi.json",
        get(crate::api::openapi::openapi_handler),
      )
      .route(
        "/api/users",
        get(crate::api::users::users_list_handler).post(crate::api::users::users_create_handler),
      )
      .route(
        "/api/users/:id/totp",
        axum::routing::delete(crate::api::users::totp_admin_reset_handler),
      )
      .route(
        "/api/me/totp/setup",
        axum::routing::post(crate::api::users::totp_setup_handler),
      )
      .route(
        "/api/me/totp/enable",
        axum::routing::post(crate::api::users::totp_enable_handler),
      )
      .route(
        "/api/me/totp",
        axum::routing::delete(crate::api::users::totp_disable_handler),
      )
      .route(
        "/api/me/passkeys",
        get(crate::webauthn::passkeys_list_handler),
      )
      .route(
        "/api/me/passkeys/register/start",
        axum::routing::post(crate::webauthn::passkey_register_start_handler),
      )
      .route(
        "/api/me/passkeys/register/finish",
        axum::routing::post(crate::webauthn::passkey_register_finish_handler),
      )
      .route(
        "/api/me/passkeys/:id",
        axum::routing::delete(crate::webauthn::passkey_delete_handler),
      )
      .route(
        "/api/users/:id",
        axum::routing::put(crate::api::users::users_update_handler)
          .delete(crate::api::users::users_delete_handler),
      );

    let state_clone = state.clone();
    dash_router = dash_router.layer(axum::middleware::from_fn(
      move |req: axum::extract::Request, next: axum::middleware::Next| {
        let state = state_clone.clone();
        async move {
          // Check for valid session cookie, then enforce the role floor of
          // the route: user management and settings are admin-only, any
          // other mutation needs operator, reads are open to viewers.
          if let Some(role) = crate::auth::dashboard_role(&state, req.headers()).await {
            let required = required_role(req.uri().path(), req.method());
            if role >= required {
              return next.run(req).await;
            }
            return Response::builder()
              .status(StatusCode::FORBIDDEN)
              .body(Body::from(format!(
                "This action requires the {} role (you are {})",
                required.as_str(),
                role.as_str()
              )))
              .unwrap();
          }
          // Redirect to login page, preserving the original path. The nested
          // router sees the path with the /aperio prefix stripped ("/" for
          // the dashboard itself), so the prefix must be re-added or the
          // post-login redirect lands on the proxied site instead.
          let nested_path = req.uri().path();
          let full_path = if nested_path == "/" {
            "/aperio".to_string()
          } else {
            format!("/aperio{}", nested_path)
          };
          let redirect_url = format!("/aperio/auth?redirect={}", safe_redirect_path(&full_path));
          Response::builder()
            .status(StatusCode::FOUND)
            .header("Location", redirect_url)
            .body(Body::empty())
            .unwrap()
        }
      },
    ));

    // Static assets are registered after the session layer on purpose: they
    // are public, because the login page needs them before any session exists.
    dash_router = dash_router.route("/assets/*path", get(dashboard_asset_handler));

    app = app.nest("/aperio", dash_router);
  } else {
    // Even with the dashboard disabled the login page (used by
    // APERIO_SERVER_AUTH-protected proxied sites) still needs its assets.
    app = app.nest(
      "/aperio",
      Router::new().route("/assets/*path", get(dashboard_asset_handler)),
    );
  }

  // Health endpoint is intentionally registered outside the dashboard auth
  // middleware so that external load balancers / monitoring tools can probe
  // server liveness without dashboard credentials.
  app = app.route("/aperio/health", get(health_handler));
  app = app.route(
    "/aperio/auth",
    get(auth_page_handler).post(auth_login_handler),
  );
  // Logout clears the session server-side and expires the cookie. Registered
  // outside the dashboard session middleware so it works with any cookie state.
  app = app.route(
    "/aperio/auth/logout",
    axum::routing::post(auth_logout_handler),
  );
  // Passkey (WebAuthn) sign-in: challenge + finish live next to the login
  // form, outside the session middleware (they create the session).
  app = app.route(
    "/aperio/auth/passkey",
    get(crate::webauthn::passkey_available_handler),
  );
  app = app.route(
    "/aperio/auth/passkey/start",
    axum::routing::post(crate::webauthn::passkey_login_start_handler),
  );
  app = app.route(
    "/aperio/auth/passkey/finish",
    axum::routing::post(crate::webauthn::passkey_login_finish_handler),
  );
  // Programmatic tunnel provisioning. Registered outside the dashboard
  // session middleware on purpose: it authenticates with the master token in
  // a header (or a session cookie), so CI jobs can mint ephemeral tunnels
  // even when the dashboard is disabled.
  // Token self-refresh. Also outside the session middleware: it authenticates
  // with the token secret itself, so a CI job or client can keep its
  // short-lived token alive without dashboard credentials.
  app = app.route(
    "/aperio/api/tokens/refresh",
    axum::routing::post(tokens_refresh_handler),
  );
  app = app.route(
    "/aperio/api/tunnels",
    axum::routing::post(tunnels_create_handler),
  );
  app = app.route(
    "/aperio/api/tunnels/:id",
    axum::routing::delete(tunnels_delete_handler),
  );
  app = app.route("/aperio/ws", get(ws_handler));
  app = app.route("/aperio/tcp", get(tcp_ws_handler));
  app = app.route("/aperio/udp", get(udp_ws_handler));
  // Tunnel discovery for --bind-tunnels consumers: same token the client
  // connected with (or master), explicit client id — never a listing.
  app = app.route("/aperio/tunnels/:client_id", get(tunnels_list_handler));
  app = app.route("/aperio/oidc/login", get(oidc_login_handler));
  app = app.route("/aperio/oidc/callback", get(oidc_callback_handler));

  // Prometheus metrics endpoint, registered outside the dashboard session
  // middleware. Access control is handled by APERIO_METRICS_TOKEN if set.
  if metrics_enabled {
    app = app.route("/aperio/metrics", get(metrics_handler));
    info!("Prometheus metrics endpoint enabled at /aperio/metrics");
  }

  // Flush persistent stats periodically and once more on shutdown.
  let stats_flush_state = state.clone();
  tokio::spawn(async move {
    loop {
      tokio::time::sleep(Duration::from_secs(30)).await;
      stats_flush_state
        .persistent_stats
        .lock()
        .await
        .save_if_dirty();
      stats_flush_state.uptime.lock().await.save_if_dirty();
    }
  });

  // Availability ticker: observe every service entity and accrue elapsed
  // time into the uptime history (APERIO_UPTIME_TICK_SECS, default 10).
  let uptime_state = state.clone();
  let uptime_tick_secs = std::env::var("APERIO_UPTIME_TICK_SECS")
    .ok()
    .and_then(|v| v.parse::<u64>().ok())
    .filter(|v| *v >= 1)
    .unwrap_or(10);
  tokio::spawn(async move {
    loop {
      let live = observe_service_availability(&uptime_state).await;
      let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
      uptime_state.uptime.lock().await.tick(now, live);
      tokio::time::sleep(Duration::from_secs(uptime_tick_secs)).await;
    }
  });
  // Threshold alerting (APERIO_ALERT_*): error-rate and client-down rules
  // evaluated by a background ticker, emitted as webhook/audit events.
  if let Some(alert_cfg) = alerts::AlertConfig::from_env() {
    alerts::spawn(state.clone(), alert_cfg);
  }

  // Token expiry early-warning ticker: emits one `token_expiring`
  // webhook/audit event per token (per expiry window) once its remaining
  // lifetime drops under APERIO_TOKEN_EXPIRY_WARNING seconds (default 24 h,
  // 0 disables). The warned set is in-memory: a restart re-arms warnings,
  // and a refresh (new expires_at) re-arms them too.
  let expiry_warning_secs = std::env::var("APERIO_TOKEN_EXPIRY_WARNING")
    .ok()
    .and_then(|v| v.parse::<u64>().ok())
    .unwrap_or(24 * 3600);
  if expiry_warning_secs > 0 {
    let warn_state = state.clone();
    tokio::spawn(async move {
      let mut warned: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();
      loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let now = std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .map(|d| d.as_secs())
          .unwrap_or(0);
        let expiring: Vec<(String, String, u64)> = {
          let store = warn_state.token_store.lock().await;
          store
            .list()
            .iter()
            .filter_map(|t| {
              let exp = t.expires_at?;
              let expires_within = exp > now && exp - now <= expiry_warning_secs;
              (expires_within && !warned.contains(&(t.id.clone(), exp)))
                .then(|| (t.id.clone(), t.name.clone(), exp))
            })
            .collect()
        };
        for (id, name, exp) in expiring {
          warned.insert((id.clone(), exp));
          warn!(
            "Token '{}' expires in {} minutes (at unix {})",
            name,
            (exp - now) / 60,
            exp
          );
          warn_state
            .audit(
              "token_expiring",
              "system",
              &format!("name={} expires_at={}", name, exp),
            )
            .await;
          warn_state
            .emit_event(
              "token_expiring",
              serde_json::json!({
                "id": id,
                "name": name,
                "expires_at": exp,
                "seconds_left": exp - now,
              }),
            )
            .await;
        }
        // Drop warned entries whose expiry has passed or moved (refresh).
        warned.retain(|(_, exp)| *exp > now);
      }
    });
  }

  let shutdown_state = state.clone();

  let app = app.with_state(state);

  let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

  let port = std::env::var("PORT")
    .ok()
    .and_then(|p| p.parse::<u16>().ok())
    .unwrap_or(8080);

  // Experimental public TCP expose ports (aperio-server.yaml `expose:`).
  expose::spawn_listeners(shutdown_state.clone(), &host, expose::from_config_file());

  let listener = tokio::net::TcpListener::bind(format!("{}:{}", host, port))
    .await
    .unwrap();

  info!(
    "Aperio Server v{} listening on {}:{} with connection info tracing enabled",
    env!("CARGO_PKG_VERSION"),
    host,
    port
  );

  axum::serve(
    listener,
    app.into_make_service_with_connect_info::<SocketAddr>(),
  )
  .with_graceful_shutdown(shutdown_signal(shutdown_state.clone()))
  .await
  .unwrap();

  // Final stats flush so nothing recorded since the last tick is lost.
  shutdown_state.persistent_stats.lock().await.save_if_dirty();
  shutdown_state.uptime.lock().await.save_if_dirty();

  // Flush any buffered OTLP spans before exit.
  otel_guard.shutdown();
}

/// Minimum dashboard role a route requires. User management and server
/// settings can change who controls the server — admin only. Everything
/// else: reads for viewers, mutations for operators.
fn required_role(path: &str, method: &axum::http::Method) -> crate::store::users::Role {
  use crate::store::users::Role;
  // Self-service routes (own TOTP enrollment): any signed-in role.
  if path.starts_with("/api/me/") {
    return Role::Viewer;
  }
  if path.starts_with("/api/users")
    || path == "/api/settings"
    // The dump contains token/password hashes and TOTP secrets, and an
    // import replaces them — admin only, even for the GET.
    || path == "/api/export"
    || path == "/api/import"
  {
    return Role::Admin;
  }
  if matches!(*method, axum::http::Method::GET | axum::http::Method::HEAD) {
    Role::Viewer
  } else {
    Role::Operator
  }
}

/// Graceful shutdown listener for receiving SIGINT or SIGTERM signals.
/// Before handing control back to axum (which drops the tunnel sockets), a
/// `ServerShutdown` message is broadcast to every connected client so they
/// reconnect aggressively instead of waiting out their normal backoff.
async fn shutdown_signal(state: Arc<AppState>) {
  let ctrl_c = async {
    tokio::signal::ctrl_c()
      .await
      .expect("Failed to install Ctrl+C handler");
  };

  #[cfg(unix)]
  let terminate = async {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
      .expect("Failed to install SIGTERM handler")
      .recv()
      .await;
  };

  #[cfg(not(unix))]
  let terminate = std::future::pending::<()>();

  tokio::select! {
      _ = ctrl_c => {},
      _ = terminate => {},
  }

  info!("Shutdown signal received, closing Aperio Server connections...");

  if let Ok(json) = serde_json::to_string(&TunnelMessage::ServerShutdown {}) {
    let clients = state.clients.lock().await;
    let notified = clients.len();
    for client in clients.values() {
      // try_send: a client with a full queue must not stall the shutdown.
      let _ = client.tx.try_send(Message::Text(json.clone()));
    }
    drop(clients);
    if notified > 0 {
      info!("Notified {} tunnel client(s) of the shutdown", notified);
      // Give the writer tasks a moment to flush the frame out.
      tokio::time::sleep(Duration::from_millis(200)).await;
    }
  }

  // Graceful shutdown only completes once every connection has ended, and
  // long-lived ones never end on their own. End them actively: dashboard SSE
  // streams watch this flag, and each tunnel read loop honors its disconnect
  // notify.
  let _ = state.shutdown.send(true);
  {
    let clients = state.clients.lock().await;
    for client in clients.values() {
      client.disconnect.notify_waiters();
    }
  }

  // Last resort: anything still holding a connection open (a proxied
  // WebSocket/TCP/UDP relay, a stalled peer) must not keep the process alive
  // forever. Flush what matters and exit.
  let fallback = state.clone();
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_secs(10)).await;
    warn!("Graceful shutdown timed out after 10s; forcing exit");
    fallback.persistent_stats.lock().await.save_if_dirty();
    fallback.uptime.lock().await.save_if_dirty();
    std::process::exit(0);
  });
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;

/// Snapshot of every service entity's availability, keyed by service name or
/// stable client id: `up` when at least one connection is heartbeat-healthy,
/// routable, and its backend probe passes; `degraded` when connected but not
/// serving (backend unhealthy, draining, or disabled); absent entities are
/// treated as `down` by the uptime store.
pub(crate) async fn observe_service_availability(
  state: &AppState,
) -> std::collections::HashMap<String, crate::store::uptime::Availability> {
  use crate::store::uptime::Availability;
  let down_threshold = state.config().client_down_threshold;
  let clients = state.clients.lock().await;
  let mut out: std::collections::HashMap<String, Availability> = std::collections::HashMap::new();
  for (conn_id, handle) in clients.iter() {
    let key = handle
      .service_name
      .clone()
      .or_else(|| handle.reported_instance_id.clone())
      .unwrap_or_else(|| conn_id.clone());
    let status = if !handle.is_healthy(down_threshold) {
      Availability::Down
    } else if handle.backend_healthy && handle.admin_enabled && !handle.draining {
      Availability::Up
    } else {
      Availability::Degraded
    };
    // Several connections may serve one entity; the best state wins.
    let entry = out.entry(key).or_insert(Availability::Down);
    let rank = |s: &Availability| match s {
      Availability::Up => 2,
      Availability::Degraded => 1,
      Availability::Down => 0,
    };
    if rank(&status) > rank(entry) {
      *entry = status;
    }
  }
  out
}
