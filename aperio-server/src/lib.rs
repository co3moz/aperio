use axum::serve::ListenerExt;
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
mod alert_rules;
mod alerts;
mod api;
mod auth;
mod backup;
mod cache;
mod capacity;
mod check_config;
mod config_file;
mod consumers;
mod deny_list;
mod error_pages;
mod expose;
mod fallbacks;
mod headers;
mod limits;
mod maintenance_windows;
mod metrics_labels;
mod oidc;
mod otlp_identity;
mod outbound;
mod print_config;
mod protocol;
mod proxy;
mod redact;
mod relay_log;
mod retention;
mod route_limits;
mod routing;
mod scaling;
mod settings;
mod share;
mod state;
mod static_routes;
mod store;
mod supervise;
mod telemetry;
mod totp;
mod tunnel;
mod waf;
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
  tokens_rotate_handler, tokens_update_handler,
};
use crate::api::tunnels::{
  tunnels_create_handler, tunnels_declared_handler, tunnels_delete_handler,
};
use crate::api::webhooks::{
  audit_handler, audit_verify_handler, webhook_deliveries_handler, webhook_redeliver_handler,
  webhooks_create_handler, webhooks_delete_handler, webhooks_list_handler,
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
use crate::tunnel::tcp::{
  tcp_ws_handler, tunnels_discovery_handler, tunnels_list_handler, udp_ws_handler,
};
use crate::tunnel::ws::ws_handler;

/// Entry point for the Aperio server, called by the thin binary in
/// `main.rs`. Handles the diagnostic subcommands, loads `aperio-server.yaml`
/// into the environment while still single-threaded, then hands over to the
/// async server on a multi-thread runtime.
pub fn run() {
  // Route every panic through structured logging before the runtime contains
  // it (see `install_panic_logger`), so a panic in a spawned task or a
  // background thread is visible in the server's own log stream instead of
  // only reaching stderr with no task context.
  install_panic_logger();

  // Pin the process-wide rustls provider to ring. The dependency tree pulls
  // rustls with both `ring` and `aws-lc-rs` enabled (workspace feature
  // unification), and with two providers rustls refuses to auto-select one,
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

  // `aperio-server --print-schema` prints the JSON Schema for
  // `aperio-server.yaml` (the file is the primary configuration surface; env
  // vars are the fallback) and exits. Point an editor's `yaml.schemas` at the
  // output for autocompletion and validation. Needs no config load.
  if std::env::args().nth(1).as_deref() == Some("--print-schema") {
    println!("{}", aperio_config::server_schema_json());
    return;
  }

  // Must happen before the runtime exists: the loader writes environment
  // variables, which is only sound while no other thread can read them.
  config_file::load();

  // Upgrade safety: compare the version the file declares against this build
  // and report every recorded config-format change in between. Runs before
  // the diagnostic subcommands so they inherit the same verdict, and before
  // anything binds a port, since a security-relevant change must stop the
  // start rather than be noticed afterwards.
  report_config_upgrade();
  refuse_removed_settings();

  // `aperio-server --check-config` lints the layered configuration (file +
  // environment) and exits without starting the server.
  if std::env::args().nth(1).as_deref() == Some("--check-config") {
    std::process::exit(check_config::run());
  }

  // `aperio-server --print-config` prints the effective configuration, which
  // `APERIO_*` values are set and whether each came from the environment, the
  // `aperio-server.yaml` file, or a persisted dashboard override, and exits
  // without starting the server.
  if std::env::args().nth(1).as_deref() == Some("--print-config") {
    std::process::exit(print_config::run());
  }

  // `aperio-server --verify-audit` verifies the tamper-evident hash chain of
  // the audit log and exits without starting the server.
  if std::env::args().nth(1).as_deref() == Some("--verify-audit") {
    std::process::exit(verify_audit());
  }

  tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("failed to build the tokio runtime")
    .block_on(async_main());
}

/// Installs a process-wide panic hook that logs every panic through `tracing`
/// (message, source location, thread, and a backtrace when `RUST_BACKTRACE` is
/// set) before the runtime contains it.
///
/// This changes observability, not control flow. Under the default `unwind`
/// strategy a panic still only unwinds its own task/connection (or is turned
/// into a 500 by the catch-panic layer), the process keeps running. But such a
/// contained panic is otherwise easy to miss: its `JoinHandle` is never
/// awaited, so the only trace is an unstructured stderr line with no task
/// context. Routing it through `tracing` puts it in the server's normal log
/// pipeline (including JSON logs and any OTLP export) so it can be alerted on.
fn install_panic_logger() {
  std::panic::set_hook(Box::new(|info| {
    let message = info
      .payload()
      .downcast_ref::<&str>()
      .map(|s| (*s).to_string())
      .or_else(|| info.payload().downcast_ref::<String>().cloned())
      .unwrap_or_else(|| "<non-string panic payload>".to_string());
    let location = info
      .location()
      .map(|l| l.to_string())
      .unwrap_or_else(|| "<unknown>".to_string());
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("<unnamed>").to_string();
    let backtrace = std::backtrace::Backtrace::capture();
    tracing::error!(
      target: "aperio_server::panic",
      panic_message = %message,
      panic_location = %location,
      panic_thread = %thread,
      panic_backtrace = %backtrace,
      "panic caught, the task/connection is unwound; the process continues"
    );
  }));
}

/// Settings that no longer exist, and what to do instead.
///
/// A removed key that is merely ignored is the worst outcome: the file still
/// says the dashboard has its own password, the server no longer agrees, and
/// nobody finds out until someone tries to sign in. Refusing to start turns a
/// silent authentication change into an obvious one, at the only moment the
/// operator is watching.
///
/// [`CONFIG_CHANGES`] covers the same ground for a file that declares a
/// `version:`, and does it with a fuller explanation. This check is what
/// catches the two cases that has no answer for: a file with no `version:`,
/// and an environment-only deployment with no file at all.
const REMOVED_SETTINGS: &[(&str, &str)] = &[(
  "APERIO_DASHBOARD_AUTH",
  "the separate dashboard password was removed. Sign in as `aperio:<APERIO_SERVER_TOKEN>`, \
   or create a dashboard user (Users page) or an organization for anyone who used it. \
   Remove `dashboard_auth:` / `dashboard.auth:` from the configuration to start.",
)];

/// Refuses to start when the configuration still sets a removed key.
///
/// Runs before the runtime exists, so it prints rather than logs, like the
/// upgrade check beside it. Every spelling is covered by checking the
/// environment variable: the file loader materializes both the flat key and
/// the block child into it.
fn refuse_removed_settings() {
  let mut refused = false;
  for (var, guidance) in REMOVED_SETTINGS {
    let set = std::env::var(var)
      .map(|v| !v.trim().is_empty())
      .unwrap_or(false);
    if set {
      eprintln!("aperio-server: {var} is set, but {guidance}");
      refused = true;
    }
  }
  if refused {
    std::process::exit(1);
  }
}

/// The keys `aperio-server.yaml` actually writes, so a change that only
/// reaches files using a particular key is not reported to files that do not.
/// An environment-only deployment has no document and therefore no keys, which
/// is correct: such a change cannot be about a key it never wrote.
pub(crate) fn declared_config_keys() -> aperio_config::compat::ConfigKeys {
  match crate::config_file::document() {
    Some(doc) => aperio_config::compat::ConfigKeys::from_mapping(&doc),
    None => aperio_config::compat::ConfigKeys::default(),
  }
}

/// The Aperio version the configuration declares (`version:` in
/// `aperio-server.yaml`, or `APERIO_VERSION`), if any.
fn declared_config_version() -> Option<String> {
  std::env::var("APERIO_VERSION")
    .ok()
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty())
}

/// Compares the declared config version against this build and reports what
/// changed in between, refusing the start when a change has security
/// consequences.
///
/// Runs before the runtime exists, so it prints rather than logs: tracing is
/// not initialized yet, and an operator watching a container start must see
/// the reason it refused.
fn report_config_upgrade() {
  use aperio_config::compat::{CONFIG_CHANGES, ConfigSurface, check_upgrade, report_lines};

  let declared = declared_config_version();

  let report = match check_upgrade(
    declared.as_deref(),
    env!("CARGO_PKG_VERSION"),
    ConfigSurface::Server,
    CONFIG_CHANGES,
    &declared_config_keys(),
  ) {
    Ok(report) => report,
    Err(e) => {
      eprintln!("aperio-server: {e}");
      std::process::exit(1);
    }
  };
  if report.declared.is_none() {
    eprintln!(
      "aperio-server: no `version:` in the configuration, so upgrade checks are off. Add `version: {}` to be warned when a future upgrade changes how this file is read.",
      report.current
    );
    return;
  }
  for line in report_lines(&report) {
    eprintln!("aperio-server: {line}");
  }
  if report.must_refuse() {
    eprintln!(
      "aperio-server: refusing to start under a configuration whose security-relevant settings changed meaning. Review the above, then set `version: {}` to acknowledge them.",
      report.current
    );
    std::process::exit(1);
  }
}

/// `aperio-server --verify-audit`: verifies the tamper-evident hash chain of
/// the audit log, the active `audit.jsonl` plus every rotated generation,
/// and returns the process exit code (0 = intact, 1 = a broken/tampered line
/// was found). Each file is checked independently; its first line is a
/// rotation boundary and is not checkable against a rotated-away predecessor.
fn verify_audit() -> i32 {
  let data_dir = std::env::var("APERIO_DATA_DIR").unwrap_or_else(|_| "./data".to_string());
  let base = std::path::PathBuf::from(&data_dir).join("audit.jsonl");

  // Collect rotated generations (.1 newest .. .N oldest), then order oldest →
  // active so the output reads chronologically.
  let mut generations: Vec<std::path::PathBuf> = Vec::new();
  let mut n = 1usize;
  loop {
    let gen_path = std::path::PathBuf::from(format!("{}.{}", base.display(), n));
    if gen_path.exists() {
      generations.push(gen_path);
      n += 1;
    } else {
      break;
    }
  }
  generations.reverse();
  generations.push(base);

  println!("Verifying audit log hash chain ({data_dir})");
  let mut broken = 0usize;
  let mut checked = 0usize;
  for f in &generations {
    if !f.exists() {
      continue;
    }
    checked += 1;
    match crate::store::audit::verify_chain(f) {
      Ok(None) => println!("  ok    {}", f.display()),
      Ok(Some(line)) => {
        broken += 1;
        println!(
          "  FAIL  {}, hash chain breaks at line {}",
          f.display(),
          line
        );
      }
      Err(e) => {
        broken += 1;
        println!("  FAIL  {}, cannot read: {}", f.display(), e);
      }
    }
  }

  println!();
  if checked == 0 {
    println!("No audit log found in {data_dir} (nothing to verify)");
    return 0;
  }
  if broken > 0 {
    println!("Audit verification FAILED: {broken} file(s) with a broken chain");
    1
  } else {
    println!("Audit verification OK ({checked} file(s) intact)");
    0
  }
}

/// Binds the main TCP listener. With `reuseport`, the socket is created with
/// `SO_REUSEADDR` + `SO_REUSEPORT` so multiple server processes can share the
/// same port for zero-downtime restarts; otherwise a plain listener is used.
async fn bind_listener(
  host: &str,
  port: u16,
  reuseport: bool,
) -> std::io::Result<tokio::net::TcpListener> {
  if !reuseport {
    return tokio::net::TcpListener::bind(format!("{host}:{port}")).await;
  }
  use socket2::{Domain, Protocol, Socket, Type};
  use std::net::ToSocketAddrs;
  let addr = format!("{host}:{port}")
    .to_socket_addrs()?
    .next()
    .ok_or_else(|| {
      std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("could not resolve {host}:{port}"),
      )
    })?;
  let domain = if addr.is_ipv6() {
    Domain::IPV6
  } else {
    Domain::IPV4
  };
  let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
  socket.set_reuse_address(true)?;
  #[cfg(unix)]
  socket.set_reuse_port(true)?;
  socket.set_nonblocking(true)?;
  socket.bind(&addr.into())?;
  socket.listen(1024)?;
  tokio::net::TcpListener::from_std(socket.into())
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

  let Some(StartupBundle {
    state,
    metrics_enabled,
  }) = build_state().await
  else {
    // The refusal has been logged by build_state with its reason.
    return;
  };

  // Once, at startup, before anything is served: what the file asks for
  // against what this machine can give. It never changes a setting, only says
  // so, because a number that silently changes because the host changed is
  // exactly what the configuration work was spent on preventing.
  {
    let cfg = state.config();
    crate::capacity::warn_if_beyond_the_machine(
      cfg.max_ws_connections,
      cfg.max_tunnels,
      cfg.cache_max_bytes,
    );
  }

  let app = build_router(state.clone(), metrics_enabled);

  let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
  spawn_background(&state, &host);

  serve_until_shutdown(state.clone(), app).await;

  // Final stats flush so nothing recorded since the last tick is lost.
  state.persistent_stats.lock().await.save_if_dirty();
  state.uptime.lock().await.save_if_dirty();
  state.activity.lock().await.save_if_dirty();

  // Flush any buffered OTLP spans before exit.
  otel_guard.shutdown();
}

/// Everything the server resolves before it can exist: the environment (with
/// the yaml file already folded in by `config_file::load`), the persisted
/// stores, the settings-override layering, and the assembled `AppState`.
///
/// `None` means "refuse to start", and the reason has already been logged:
/// an invalid trusted-proxy list, admin allowlist, or outbound allowlist.
/// Split out of `async_main` (planned_features #21) so startup can be
/// exercised in-process instead of only as a spawned server.
pub(crate) async fn build_state() -> Option<StartupBundle> {
  // Enforce APERIO_SERVER_TOKEN environment variable
  let token = std::env::var("APERIO_SERVER_TOKEN").unwrap_or_else(|_| {
    error!("CRITICAL SECURITY ERROR: APERIO_SERVER_TOKEN environment variable must be set!");
    std::process::exit(1);
  });
  if token.trim().is_empty() {
    error!("CRITICAL SECURITY ERROR: APERIO_SERVER_TOKEN cannot be empty!");
    std::process::exit(1);
  }

  let gateway_timeout_secs = std::env::var("APERIO_GATEWAY_TIMEOUT")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .unwrap_or(10);

  let gateway_response_timeout_secs = std::env::var("APERIO_GATEWAY_RESPONSE_TIMEOUT")
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

  // Max concurrently-live proxied public WebSockets. WebSockets are long-lived,
  // so they get their own ceiling separate from the (short-lived) HTTP request
  // limit above; the default is generous enough to never touch normal use while
  // still capping a pathological pile-up. 0 is treated as "no cap".
  let max_ws_connections = std::env::var("APERIO_MAX_WS_CONNECTIONS")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .map(|v| if v == 0 { usize::MAX } else { v })
    .unwrap_or(10_000);

  // Max connected tunnel clients limit (default: 10 active clients)
  let max_tunnels = std::env::var("APERIO_MAX_TUNNELS")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(10);

  // Parallel connections one client may open for a single service. 16 is what
  // the client used to clamp to on its own, so an unset server keeps exactly
  // the behaviour that was there before this became the server's decision.
  // Both default on: they are what makes the dashboard useful, and a server
  // that is not saturated should not have to know they exist. Same spelling
  // as the other on-by-default flags: `0`/`false` turns one off.
  let opt_out = |key: &str| {
    std::env::var(key)
      .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
      .unwrap_or(true)
  };
  let inspector = opt_out("APERIO_INSPECTOR");
  let access_events = opt_out("APERIO_ACCESS_EVENTS");

  let max_connections_per_service = std::env::var("APERIO_MAX_CONNECTIONS_PER_SERVICE")
    .ok()
    .and_then(|val| val.parse::<u32>().ok())
    .filter(|v| *v > 0)
    .unwrap_or(16);

  // Max IP token bucket capacity burst (default: 100 requests)
  // Only a finite, strictly positive bucket size is meaningful: 0, a negative,
  // NaN or infinity would silently wedge the limiter (never/always throttling),
  // so reject those and fall back to the default, mirroring the dashboard
  // settings validation (`v > 0.0`).
  let ip_limit_max = std::env::var("APERIO_IP_LIMIT_MAX")
    .ok()
    .and_then(|val| val.parse::<f64>().ok())
    .filter(|v| v.is_finite() && *v > 0.0)
    .unwrap_or(100.0);

  // IP token bucket refill rate per second (default: 5.0 requests/sec, which is 300 req/min)
  let ip_limit_refill = std::env::var("APERIO_IP_LIMIT_REFILL")
    .ok()
    .and_then(|val| val.parse::<f64>().ok())
    .filter(|v| v.is_finite() && *v > 0.0)
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
  // APERIO_REAL_IP_HEADER still wins). Deliberately opt-in, any visitor can
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
  // nearest hop backwards past trusted addresses, the CDN-agnostic model
  // that works for any proxy chain. Implies trust_proxy.
  let trusted_proxies = match std::env::var("APERIO_TRUSTED_PROXIES") {
    Ok(raw) => match crate::routing::parse_trusted_proxies(&raw) {
      Ok(list) => list,
      Err(e) => {
        error!(
          "APERIO_TRUSTED_PROXIES is invalid ({e}); refusing to start with a partial trusted set"
        );
        return None;
      }
    },
    Err(_) => Vec::new(),
  };
  // Source IPs/CIDRs allowed to reach the authenticated admin surface
  // (`/aperio` dashboard + `/aperio/api/*`). Empty = no network restriction.
  let admin_allowed_ips = match std::env::var("APERIO_ADMIN_ALLOWED_IPS") {
    Ok(raw) => match crate::routing::parse_trusted_proxies(&raw) {
      Ok(list) => list,
      Err(e) => {
        error!(
          "APERIO_ADMIN_ALLOWED_IPS is invalid ({e}); refusing to start with a partial allowlist"
        );
        return None;
      }
    },
    Err(_) => Vec::new(),
  };
  if !admin_allowed_ips.is_empty() {
    info!(
      "Admin surface IP allowlist active ({} entries): only matching client IPs may reach the dashboard and its API",
      admin_allowed_ips.len()
    );
  }
  // The deny list is read from the live config document (so it hot-reloads),
  // falling back to the environment. A malformed entry refuses the start
  // rather than applying a partial block list: an operator who wrote a deny
  // list believes those addresses cannot reach the server.
  let denied_ips_config = match std::env::var("APERIO_DENIED_IPS") {
    Ok(raw) if crate::config_file::structured("denied_ips").is_none() && !raw.trim().is_empty() => {
      match crate::deny_list::DenyList::parse(&raw) {
        Ok(list) => list,
        Err(e) => {
          error!("APERIO_DENIED_IPS is invalid ({e}); refusing to start with a partial deny list");
          return None;
        }
      }
    }
    _ => crate::deny_list::from_config(),
  };
  if !denied_ips_config.is_empty() {
    info!(
      "Source IP deny list active ({} entries): matching addresses are refused before anything else",
      denied_ips_config.len()
    );
  }
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

  // Autoscaling (default: disabled). A client's `scaling:` block is ignored
  // entirely unless the operator turns the feature on.
  let scaling_enabled = std::env::var("APERIO_SCALING")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let scaling_allow_http = std::env::var("APERIO_SCALING_ALLOW_HTTP")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let scaling_allow_private = std::env::var("APERIO_SCALING_ALLOW_PRIVATE")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let scaling_record_ttl = Duration::from_secs(
    std::env::var("APERIO_SCALING_RECORD_TTL")
      .ok()
      .and_then(|v| v.parse::<u64>().ok())
      .unwrap_or(30 * 24 * 3600),
  );

  // Edge integration (default: disabled). The token is the on/off switch:
  // without it the `/aperio/api/edge/*` routes are not registered at all.
  let edge_token = std::env::var("APERIO_EDGE_TOKEN")
    .ok()
    .filter(|t| !t.trim().is_empty());
  let edge_service_url = std::env::var("APERIO_EDGE_SERVICE_URL")
    .ok()
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty());
  let edge_entrypoints: Vec<String> = std::env::var("APERIO_EDGE_ENTRYPOINTS")
    .unwrap_or_default()
    .split(',')
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty())
    .collect();
  let edge_cert_resolver = std::env::var("APERIO_EDGE_CERT_RESOLVER")
    .ok()
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty());
  let edge_include_offline = std::env::var("APERIO_EDGE_INCLUDE_OFFLINE")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);

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

  // Optional outbound-callback policy (webhooks, autoscaling hooks): an
  // allowlist of host/CIDR patterns and/or a block on private destinations.
  // Empty/off keeps today's permissive behaviour. An invalid entry refuses
  // startup rather than applying a partial allowlist.
  let outbound_policy = {
    let allowlist = match std::env::var("APERIO_OUTBOUND_ALLOWLIST") {
      Ok(raw) => match crate::outbound::parse_patterns(&raw) {
        Ok(list) => list,
        Err(e) => {
          error!(
            "APERIO_OUTBOUND_ALLOWLIST is invalid ({e}); refusing to start with a partial allowlist"
          );
          return None;
        }
      },
      Err(_) => Vec::new(),
    };
    let block_private = std::env::var("APERIO_OUTBOUND_BLOCK_PRIVATE")
      .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
      .unwrap_or(false);
    let policy = crate::outbound::OutboundPolicy {
      allowlist,
      block_private,
    };
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
    }
    policy
  };

  // Per-stream flow-control watermarks (protocol v3). Invalid combinations
  // are repaired by StreamLimits::sanitized with a warning.
  let stream_pause_bytes = std::env::var("APERIO_STREAM_PAUSE_BYTES")
    .ok()
    .and_then(|v| v.trim().parse::<usize>().ok())
    .filter(|v| *v > 0)
    .unwrap_or(crate::state::STREAM_PAUSE_BYTES);
  let stream_resume_bytes = std::env::var("APERIO_STREAM_RESUME_BYTES")
    .ok()
    .and_then(|v| v.trim().parse::<usize>().ok())
    .filter(|v| *v > 0)
    .unwrap_or(crate::state::STREAM_RESUME_BYTES);
  let stream_min_throughput = std::env::var("APERIO_STREAM_MIN_THROUGHPUT")
    .ok()
    .and_then(|v| v.trim().parse::<u64>().ok())
    .unwrap_or(0);
  let stream_backlog_limit = std::env::var("APERIO_STREAM_BACKLOG_LIMIT")
    .ok()
    .and_then(|v| v.trim().parse::<usize>().ok())
    .filter(|v| *v > 0)
    .unwrap_or(crate::state::STREAM_BACKLOG_LIMIT);

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
            "Failed to read APERIO_504_PAGE {}: {}, using default 504 text",
            path, e
          );
          None
        }
      });

  // Structured access log: APERIO_ACCESS_LOG=<path> appends one JSON line
  // per proxied request to the file (in addition to the structured
  // aperio_access tracing events that always go to stdout).
  let access_log_configured = std::env::var("APERIO_ACCESS_LOG")
    .ok()
    .map(|p| p.trim().to_string())
    .filter(|p| !p.is_empty());
  let access_log = access_log_configured.as_ref().and_then(|path| {
    match std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(path)
    {
      Ok(file) => {
        info!("Structured access log enabled: {}", path);
        // One writer task owns the file from here on: the request path
        // queues lines instead of taking a process-wide mutex around a
        // synchronous write.
        Some(crate::access_log::spawn_writer(path.clone(), file))
      }
      Err(e) => {
        error!(
          "Failed to open APERIO_ACCESS_LOG {}: {}, access log file disabled",
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
            "Failed to read APERIO_503_PAGE {}: {}, using default 503 text",
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
  let admin_key_store = crate::store::admin_keys::AdminKeyStore::load(&data_dir);
  let inbox_store = crate::store::inbox::InboxStore::load(&data_dir);

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
    max_connections_per_service,
    inspector,
    access_events,
    ip_limit_max,
    ip_limit_refill,
    auth_credentials,
    trust_proxy,
    ignore_client_auth,
    real_ip_header,
    trusted_proxies,
    admin_allowed_ips,
    secure_cookies,
    require_hostname_bind,
    metrics_token,
    scaling_enabled,
    scaling_allow_http,
    scaling_allow_private,
    scaling_record_ttl,
    edge_token,
    edge_service_url,
    edge_entrypoints,
    edge_cert_resolver,
    edge_include_offline,
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
    retry_on_5xx: std::env::var("APERIO_RETRY_ON_5XX")
      .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
      .unwrap_or(false),
    retry_statuses: std::env::var("APERIO_RETRY_STATUSES")
      .ok()
      .map(|raw| {
        raw
          .split(',')
          .filter_map(|s| s.trim().parse::<u16>().ok())
          .collect()
      })
      .unwrap_or_default(),
    outlier_ejection: std::env::var("APERIO_OUTLIER_EJECTION")
      .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
      .unwrap_or(false),
    outlier_max_failures: std::env::var("APERIO_OUTLIER_MAX_FAILURES")
      .ok()
      .and_then(|v| v.trim().parse::<u32>().ok())
      .filter(|n| *n > 0)
      .unwrap_or(5),
    outlier_window: Duration::from_secs(
      std::env::var("APERIO_OUTLIER_WINDOW")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(30),
    ),
    outlier_eject: Duration::from_secs(
      std::env::var("APERIO_OUTLIER_EJECT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(30),
    ),
    cache_enabled,
    cache_max_bytes,
    cache_max_stale,
    stream_min_throughput,
    stream_pause_bytes,
    stream_resume_bytes,
    stream_backlog_limit,
    outbound_policy,
    max_concurrent_requests,
    max_ws_connections,
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
    error_pages: error_pages::from_config_file(),
    route_limits: route_limits::from_config_file(),
    waf: waf::from_config_file(),
    alert_rules: alert_rules::from_config_file(),
    maintenance_windows: maintenance_windows::from_config_file(),
    denied_ips: denied_ips_config,
    alternate_servers: crate::tunnel::ws::parse_alternates(
      &std::env::var("APERIO_ALTERNATE_SERVERS").unwrap_or_default(),
    ),
    max_streams_per_ip: std::env::var("APERIO_MAX_STREAMS_PER_IP")
      .ok()
      .and_then(|v| v.trim().parse::<u32>().ok())
      .unwrap_or(0),
    otel_bridge: std::env::var("APERIO_OTEL_BRIDGE")
      .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
      .unwrap_or(false),
    shutdown_drain: {
      let raw = std::env::var("APERIO_SHUTDOWN_DRAIN").unwrap_or_default();
      raw.trim().parse::<u64>().ok()
    },
    shutdown_drain_auto: std::env::var("APERIO_SHUTDOWN_DRAIN")
      .map(|v| v.trim().eq_ignore_ascii_case("auto"))
      .unwrap_or(false),
    shutdown_timeout: std::env::var("APERIO_SHUTDOWN_TIMEOUT")
      .ok()
      .and_then(|v| v.trim().parse::<u64>().ok())
      .filter(|v| *v > 0)
      .unwrap_or(10),
    access_log_sample_rate: std::env::var("APERIO_ACCESS_LOG_SAMPLE_RATE")
      .ok()
      .and_then(|v| v.trim().parse::<f64>().ok())
      .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
      .unwrap_or(1.0),
    identity_headers: std::env::var("APERIO_IDENTITY_HEADERS")
      .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
      .unwrap_or(false),
    request_id_enabled: std::env::var("APERIO_REQUEST_ID")
      .map(|v| !matches!(v.trim(), "0" | "false" | "no"))
      .unwrap_or(true),
    request_id_header: std::env::var("APERIO_REQUEST_ID_HEADER")
      .ok()
      .map(|v| v.trim().to_ascii_lowercase())
      .filter(|v| !v.is_empty() && v.parse::<axum::http::HeaderName>().is_ok())
      .unwrap_or_else(|| "x-request-id".to_string()),
    request_id_trust_inbound: std::env::var("APERIO_REQUEST_ID_TRUST_INBOUND")
      .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
      .unwrap_or(false),
    fallbacks: fallbacks::from_config_file(),
    token_pinning: std::env::var("APERIO_TOKEN_PINNING")
      .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
      .unwrap_or(false),
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
            "Failed to parse {:?}: {}, ignoring persisted settings",
            settings_path, e
          );
          None
        }
      },
    )
    .unwrap_or_default();
  // The file wins over a stored dashboard override for the same key, and the
  // override is dropped rather than out-voted. Both directions of the old
  // behaviour were wrong: the file said one thing and the server did another,
  // and the override survived to come back the day the key left the file.
  let file_layer_for_pruning = crate::settings::file_overrides();
  let mut settings_overrides = settings_overrides;
  let dropped = crate::settings::drop_conflicting(&file_layer_for_pruning, &mut settings_overrides);
  if !dropped.is_empty() {
    warn!(
      "Dropped {} dashboard override(s) that aperio-server.yaml also sets, the file wins: {:?}. \
       Set them in the file if you meant them; they are gone from {:?}.",
      dropped.len(),
      dropped,
      settings_path
    );
    match serde_json::to_string_pretty(&settings_overrides) {
      Ok(json) => {
        if let Err(e) = crate::api::settings::write_owner_only(&settings_path, json.as_bytes()) {
          error!(
            "Failed to rewrite {:?} without the dropped overrides: {}",
            settings_path, e
          );
        }
      }
      Err(e) => error!("Failed to serialize the pruned settings: {}", e),
    }
  }
  let overridden = override_keys(&settings_overrides);
  if !overridden.is_empty() {
    info!(
      "Applying persisted dashboard settings from {:?} (overridden: {:?})",
      settings_path, overridden
    );
  }
  let config_env_defaults = Arc::new(config);
  // Layer: env defaults -> aperio-server.yaml live settings -> dashboard
  // overrides. The file's scalar values were also folded into the env
  // defaults at startup; layering them explicitly is what lets hot-reload
  // change them later without touching the environment.
  let file_layer = crate::settings::file_overrides();
  let file_based = apply_settings_overrides(&config_env_defaults, &file_layer);
  let config = apply_settings_overrides(&file_based, &settings_overrides);

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
  // Server events for the dashboard's notification bell. A far smaller buffer
  // than traffic: these arrive at human pace, not request pace, and a burst
  // large enough to overrun 64 is one nobody reads item by item anyway.
  let (events_tx, _) = tokio::sync::broadcast::channel(64);

  // The telemetry collector: one task owns the per-request bookkeeping
  // writes, the request path only queues. Sized generously; a full queue
  // falls back to inline writes rather than losing the event.
  let (telemetry_tx, telemetry_rx) = tokio::sync::mpsc::channel(8192);

  let state = Arc::new(AppState {
    clients: tokio::sync::RwLock::new(HashMap::new()),
    consumers: tokio::sync::Mutex::new(Default::default()),
    stream_counts: Arc::new(std::sync::Mutex::new(HashMap::new())),
    telemetry_tx,
    pending_messages: Mutex::new(HashMap::new()),
    message_metrics: Default::default(),
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
    events_tx,
    config_store: std::sync::RwLock::new(Arc::new(config)),
    config_env_defaults,
    settings_overrides: Mutex::new(settings_overrides),
    settings_path,
    active_proxied_requests: Arc::new(AtomicUsize::new(0)),
    active_ws_connections: Arc::new(AtomicUsize::new(0)),
    path_rr: Mutex::new(HashMap::new()),
    sessions: Mutex::new(crate::store::sessions::SessionStore::load(&data_dir)),
    rate_limiter: Mutex::new(HashMap::new()),
    login_lockout: Mutex::new(crate::auth::LockoutTracker::new(
      lockout_threshold,
      Duration::from_secs(lockout_secs),
    )),
    token_rate: Mutex::new(HashMap::new()),
    token_daily_bytes: Mutex::new(HashMap::new()),
    token_seen_ips: Mutex::new(HashMap::new()),
    route_rate: Mutex::new(HashMap::new()),
    active_tunnel_count: AtomicUsize::new(0),
    ws_streams: Mutex::new(HashMap::new()),
    pending_upgrades: Mutex::new(HashMap::new()),
    token_store: Mutex::new(token_store),
    admin_key_store: Mutex::new(admin_key_store),
    inbox_store: Mutex::new(inbox_store),
    users: Mutex::new(crate::store::users::UserStore::load(&data_dir)),
    response_streams: Mutex::new(HashMap::new()),
    captured_requests: Mutex::new(VecDeque::with_capacity(CAPTURE_MAX_ENTRIES)),
    audit: Mutex::new(AuditLog::load(&data_dir, audit_max_size, audit_max_files)),
    persistent_stats: Mutex::new(StatsStore::load(&data_dir)),
    scaling_store: Mutex::new(crate::store::scaling::ScalingStore::load(&data_dir)),
    scaling_runtime: Mutex::new(crate::scaling::ScalingRuntime::default()),
    scaling_calls: crate::scaling::call_semaphore(),
    webhook_store: Mutex::new(WebhookStore::load(&data_dir)),
    org_store: Mutex::new(crate::store::orgs::OrgStore::load(&data_dir)),
    webhook_deliveries: std::sync::Arc::new(Mutex::new(crate::store::webhooks::DeliveryLog::load(
      &data_dir,
    ))),
    webauthn: crate::webauthn::build_webauthn(),
    webauthn_ceremonies: Mutex::new(crate::webauthn::WebauthnCeremonies::default()),
    uptime: Mutex::new(crate::store::uptime::UptimeStore::load(&data_dir)),
    oidc: oidc_runtime,
    org_oidc: Mutex::new(HashMap::new()),
    oidc_states: Mutex::new(HashMap::new()),
    tcp_streams: Mutex::new(HashMap::new()),
    udp_streams: Mutex::new(HashMap::new()),
    response_cache: Mutex::new(crate::cache::ResponseCache::default()),
    cache_inflight: std::sync::Mutex::new(std::collections::HashMap::new()),
    endpoint_stats: Mutex::new(crate::state::EndpointStats::default()),
    route_trends: Mutex::new(crate::state::RouteTrends::default()),
    activity: Mutex::new(crate::state::Activity::load(
      &data_dir,
      crate::store::tokens::now_secs(),
    )),
    stage_stats: Mutex::new(crate::state::StageStats::default()),
    maintenance: Mutex::new(std::collections::HashMap::new()),
    access_log,
    duration_histogram: DurationHistogram::default(),
    limit_counters: Default::default(),
  });

  crate::access_log::spawn_telemetry_collector(state.clone(), telemetry_rx);

  // Recorded once the audit log exists: a dropped override changed how this
  // server behaves, and the operator who set it from a browser is not the one
  // reading the startup log.
  if !dropped.is_empty() {
    state
      .audit(
        "settings_override_dropped",
        "system",
        "system",
        &format!(
          "aperio-server.yaml also sets {}; the file wins and the stored override was removed",
          dropped.join(", ")
        ),
      )
      .await;
  }

  Some(StartupBundle {
    state,
    metrics_enabled,
  })
}

/// What `build_state` hands to the router: the state itself plus the one
/// resolved flag that is not stored on it.
pub(crate) struct StartupBundle {
  pub(crate) state: Arc<AppState>,
  pub(crate) metrics_enabled: bool,
}

/// Assembles the whole HTTP surface: the proxy fallback, the dashboard and
/// its API (when enabled), the auth and tunnel endpoints, the admin 404
/// fence, and the outermost catch-panic layer. Pure assembly: nothing here
/// spawns, binds, or reads the environment, so a test can drive the result
/// with `tower::ServiceExt::oneshot`.
pub(crate) fn build_router(state: Arc<AppState>, metrics_enabled: bool) -> Router {
  let mut app = Router::new().fallback(any(proxy_handler));

  let dashboard_enabled = state.dashboard_enabled;
  if dashboard_enabled {
    let mut dash_router = Router::new()
      .route("/", get(dashboard_handler))
      .route("/api/stats", get(stats_handler))
      .route("/api/stats/history", get(stats_history_handler))
      .route("/api/uptime", get(uptime_handler))
      .route("/api/topology", get(crate::api::topology::topology_handler))
      .route("/api/logs", get(logs_handler))
      .route("/api/stream", get(live_stream_handler))
      .route("/api/session", get(auth_session_handler))
      .route(
        "/api/clients/{id}/config",
        get(crate::api::clients::client_config_handler),
      )
      .route(
        "/api/clients/{id}/override",
        axum::routing::post(client_override_handler),
      )
      .route(
        "/api/clients/{id}/enabled",
        axum::routing::post(client_enabled_handler),
      )
      .route(
        "/api/tokens",
        get(tokens_list_handler).post(tokens_create_handler),
      )
      .route(
        "/api/tokens/{id}",
        axum::routing::put(tokens_update_handler).delete(tokens_revoke_handler),
      )
      .route(
        "/api/tokens/{id}/rotate",
        axum::routing::post(tokens_rotate_handler),
      )
      .route(
        "/api/purge",
        axum::routing::post(crate::api::purge::purge_handler),
      )
      .route(
        "/api/slow-endpoints",
        get(crate::api::metrics::slow_endpoints_handler),
      )
      .route(
        "/api/bandwidth",
        get(crate::api::metrics::bandwidth_handler),
      )
      .route(
        "/api/route-trends",
        get(crate::api::metrics::route_trends_handler),
      )
      .route("/api/activity", get(crate::api::metrics::activity_handler))
      .route(
        "/api/cache/purge",
        axum::routing::post(crate::api::purge::cache_purge_handler),
      )
      .route(
        "/api/publish",
        axum::routing::post(crate::api::publish::publish_handler),
      )
      .route(
        "/api/subscribers",
        get(crate::api::publish::subscribers_handler),
      )
      .route(
        "/api/cache/stats",
        get(crate::api::purge::cache_stats_handler),
      )
      .route(
        "/api/inbox",
        get(crate::api::inbox::inbox_list_handler).delete(crate::api::inbox::inbox_clear_handler),
      )
      .route(
        "/api/inbox/{id}",
        get(crate::api::inbox::inbox_detail_handler)
          .delete(crate::api::inbox::inbox_delete_handler),
      )
      .route(
        "/api/inbox/{id}/refire",
        axum::routing::post(crate::api::inbox::inbox_refire_handler),
      )
      .route("/api/requests/{id}", get(request_detail_handler))
      .route(
        "/api/requests/{id}/replay",
        axum::routing::post(request_replay_handler),
      )
      .route("/api/audit", get(audit_handler))
      .route(
        "/api/export/audit.csv",
        get(crate::api::webhooks::audit_csv_handler),
      )
      .route("/api/audit/verify", get(audit_verify_handler))
      .route(
        "/api/self-health",
        get(crate::api::observe::self_health_handler),
      )
      .route(
        "/api/export/traffic.csv",
        get(crate::api::observe::traffic_csv_handler),
      )
      .route(
        "/api/admin-keys",
        get(crate::api::admin_keys::admin_keys_list_handler)
          .post(crate::api::admin_keys::admin_keys_create_handler),
      )
      .route(
        "/api/admin-keys/{id}",
        axum::routing::delete(crate::api::admin_keys::admin_keys_revoke_handler),
      )
      .route(
        "/api/maintenance",
        get(maintenance_list_handler).post(maintenance_set_handler),
      )
      .route("/api/explain", get(crate::api::explain::explain_handler))
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
        "/api/webhooks/{id}",
        axum::routing::delete(webhooks_delete_handler),
      )
      .route("/api/webhooks/deliveries", get(webhook_deliveries_handler))
      .route(
        "/api/webhooks/deliveries/{id}/redeliver",
        axum::routing::post(webhook_redeliver_handler),
      )
      .route(
        "/api/webhooks/{id}/test",
        axum::routing::post(crate::api::webhooks::webhook_test_handler),
      )
      .route(
        "/api/openapi.json",
        get(crate::api::openapi::openapi_handler),
      )
      .route(
        "/api/config/schema/{kind}",
        get(crate::api::config_schema::config_schema_handler),
      )
      .route(
        "/api/stage-stats",
        get(crate::api::metrics::stage_stats_handler),
      )
      .route(
        "/api/orgs",
        get(crate::api::orgs::orgs_list_handler).post(crate::api::orgs::orgs_create_handler),
      )
      .route(
        "/api/orgs/{id}",
        axum::routing::delete(crate::api::orgs::orgs_delete_handler),
      )
      .route(
        "/api/orgs/{id}/quota",
        axum::routing::put(crate::api::orgs::orgs_quota_handler),
      )
      .route(
        "/api/scaling",
        get(crate::api::scaling::scaling_list_handler),
      )
      .route(
        "/api/scaling/{id}",
        axum::routing::delete(crate::api::scaling::scaling_delete_handler),
      )
      .route(
        "/api/orgs/{id}/custom-name",
        axum::routing::put(crate::api::orgs::orgs_custom_name_handler),
      )
      .route(
        "/api/orgs/{id}/hostnames",
        axum::routing::put(crate::api::orgs::orgs_hostnames_handler),
      )
      .route(
        "/api/orgs/{id}/usage",
        get(crate::api::orgs::orgs_usage_handler),
      )
      .route(
        "/api/orgs/{id}/oidc",
        axum::routing::put(crate::api::orgs::orgs_oidc_handler),
      )
      .route(
        "/api/orgs/select",
        axum::routing::post(crate::api::orgs::orgs_select_handler),
      )
      .route(
        "/api/sessions",
        get(crate::api::users::sessions_list_handler)
          .delete(crate::api::users::sessions_clear_handler),
      )
      .route(
        "/api/sessions/{id}",
        axum::routing::delete(crate::api::users::session_revoke_handler),
      )
      .route(
        "/api/users",
        get(crate::api::users::users_list_handler).post(crate::api::users::users_create_handler),
      )
      .route(
        "/api/users/{id}/totp",
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
        "/api/me/passkeys/{id}",
        axum::routing::delete(crate::webauthn::passkey_delete_handler),
      )
      .route(
        "/api/users/{id}",
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

    // Network-level fence for the admin surface (APERIO_ADMIN_ALLOWED_IPS).
    // Added after the session layer so it runs first (outermost): a blocked
    // source IP is rejected with 403 before any auth check. Registered before
    // the assets route below so public login assets stay reachable, and it
    // wraps only the dashboard + /api routes, the login page, auth endpoints,
    // health and OIDC routes live on the root router and are never fenced, so
    // APERIO_SERVER_AUTH-protected proxied sites keep working from any address.
    let ip_state = state.clone();
    dash_router = dash_router.layer(axum::middleware::from_fn(
      move |req: axum::extract::Request, next: axum::middleware::Next| {
        let state = ip_state.clone();
        async move {
          let cfg = state.config();
          if !cfg.admin_allowed_ips.is_empty() {
            let peer = req
              .extensions()
              .get::<axum::extract::ConnectInfo<SocketAddr>>()
              .map(|ci| ci.0.ip());
            let allowed = peer.is_some_and(|peer_ip| {
              let client_ip = crate::routing::extract_client_ip(
                req.headers(),
                peer_ip,
                cfg.trust_proxy,
                cfg.real_ip_header.as_deref(),
                &cfg.trusted_proxies,
              );
              crate::routing::ip_in_ranges(client_ip, &cfg.admin_allowed_ips)
            });
            if !allowed {
              return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from(
                  "Admin surface is restricted to allowed source IPs",
                ))
                .unwrap();
            }
          }
          next.run(req).await
        }
      },
    ));

    // Static assets are registered after the session layer on purpose: they
    // are public, because the login page needs them before any session exists.
    dash_router = dash_router.route("/assets/{*path}", get(dashboard_asset_handler));

    app = app.nest("/aperio", dash_router);
  } else {
    // Even with the dashboard disabled the login page (used by
    // APERIO_SERVER_AUTH-protected proxied sites) still needs its assets.
    app = app.nest(
      "/aperio",
      Router::new().route("/assets/{*path}", get(dashboard_asset_handler)),
    );
  }

  // Anything else under `/aperio/` is the admin surface's namespace, and a
  // path that matches nothing in it is a mistake, not traffic. Without this it
  // reaches the proxy: a typo in an API path, or a probe for an endpoint that
  // does not exist, would be served whatever a tunnel client happens to
  // answer, under the URL the docs call the admin surface. The routes
  // registered on the router directly (`/aperio/health`, `/aperio/ws`, the
  // auth endpoints) are literals and still win over this.
  app = app.route(
    "/aperio/{*rest}",
    any(|| async { (StatusCode::NOT_FOUND, "404 Not Found\n") }),
  );

  // `/aperio/` is not the dashboard: nesting matches `/aperio` and
  // `/aperio/<something>`, so the trailing slash falls through to the proxy
  // and a visitor gets whatever a tunnel client answers, a 504, or worse, a
  // stranger's site. It is the same address to everyone typing it, so it
  // redirects to the one that works, query string and all.
  app = app.route(
    "/aperio/",
    any(|uri: axum::http::Uri| async move {
      let target = match uri.query() {
        Some(q) if !q.is_empty() => format!("/aperio?{q}"),
        _ => "/aperio".to_string(),
      };
      axum::response::Redirect::permanent(&target)
    }),
  );

  // Health endpoint is intentionally registered outside the dashboard auth
  // middleware so that external load balancers / monitoring tools can probe
  // server liveness without dashboard credentials.
  app = app.route("/aperio/health", get(health_handler));
  // Split probes for container runtimes: liveness answers "is the process
  // serving" with no body and no locks, readiness answers "should traffic
  // come here", which stops being true the moment a shutdown signal lands.
  app = app.route("/aperio/healthz", get(crate::api::healthz_handler));
  // The OTel bridge's receiving end. Outside the dashboard auth middleware
  // like the tunnel endpoints, because it authenticates with a tunnel token
  // rather than a dashboard session.
  app = app.route(
    "/aperio/otlp/v1/{signal}",
    axum::routing::post(crate::api::otlp::otlp_handler),
  );
  app = app.route("/aperio/readyz", get(crate::api::readyz_handler));
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
  app = app
    .route(
      "/aperio/auth/passkey/start",
      axum::routing::post(crate::webauthn::passkey_login_start_handler),
    )
    .route(
      "/aperio/auth/passkey/discoverable/start",
      axum::routing::post(crate::webauthn::passkey_discoverable_start_handler),
    )
    .route(
      "/aperio/auth/passkey/discoverable/finish",
      axum::routing::post(crate::webauthn::passkey_discoverable_finish_handler),
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
    get(tunnels_declared_handler).post(tunnels_create_handler),
  );
  app = app.route(
    "/aperio/api/tunnels/{id}",
    axum::routing::delete(tunnels_delete_handler),
  );
  app = app.route("/aperio/ws", get(ws_handler));
  app = app.route("/aperio/tcp", get(tcp_ws_handler));
  app = app.route("/aperio/udp", get(udp_ws_handler));
  // Tunnel discovery for --bind-tunnels consumers: same token the client
  // connected with (or master), explicit client id, never a listing.
  app = app.route("/aperio/tunnels", get(tunnels_discovery_handler));
  app = app.route("/aperio/tunnels/{client_id}", get(tunnels_list_handler));
  app = app.route("/aperio/oidc/login", get(oidc_login_handler));
  app = app.route("/aperio/oidc/callback", get(oidc_callback_handler));

  // Prometheus metrics endpoint, registered outside the dashboard session
  // middleware. Access control is handled by APERIO_METRICS_TOKEN if set.
  if metrics_enabled {
    app = app.route("/aperio/metrics", get(metrics_handler));
    info!("Prometheus metrics endpoint enabled at /aperio/metrics");
  }

  // Edge integration. Registered outside the dashboard session middleware on
  // purpose: the edge proxy holds only APERIO_EDGE_TOKEN, and the endpoints
  // must work with the dashboard disabled.
  //
  // The routes exist whether or not the feature is on, and the handlers answer
  // 404 when it is off. Registering them conditionally instead left the paths
  // unmatched, so they fell through to the visitor proxy and came back as a
  // 504 "no client connected": a tunnel error for what is a configuration
  // question, and never the documented 404. Reserving them also keeps a
  // request for them from ever being forwarded to somebody's backend, and lets
  // the token be enabled by config reload without a restart.
  app = app
    .route(
      "/aperio/api/edge/ask",
      get(crate::api::edge::edge_ask_handler),
    )
    .route(
      "/aperio/api/edge/traefik",
      get(crate::api::edge::edge_traefik_handler),
    );
  if state.config().edge_token.is_some() {
    info!(
      "Edge integration enabled at /aperio/api/edge/ask and /aperio/api/edge/traefik{}",
      if state.config().edge_service_url.is_some() {
        ""
      } else {
        " (set APERIO_EDGE_SERVICE_URL for the Traefik document)"
      }
    );
  }

  // The source-IP deny list, outside every route and every other check: a
  // blocked address must not reach a handler, spend a rate-limit bucket,
  // occupy a request slot or open a tunnel. It covers proxied traffic, the
  // dashboard and the tunnel endpoints alike, which is what an operator
  // blocking an address means by it.
  let deny_state = state.clone();
  let app = app.layer(axum::middleware::from_fn(
    move |req: axum::extract::Request, next: axum::middleware::Next| {
      let state = deny_state.clone();
      async move {
        let cfg = state.config();
        if !cfg.denied_ips.is_empty()
          && let Some(peer) = req
            .extensions()
            .get::<axum::extract::ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip())
        {
          // The same address resolution the rest of the server uses, so a
          // deployment behind a trusted proxy blocks the visitor rather than
          // the proxy, and one without trust_proxy cannot be fooled by a
          // forged header into unblocking anybody.
          let client_ip = crate::routing::extract_client_ip(
            req.headers(),
            peer,
            cfg.trust_proxy,
            cfg.real_ip_header.as_deref(),
            &cfg.trusted_proxies,
          );
          if cfg.denied_ips.blocks(client_ip) {
            return Response::builder()
              .status(StatusCode::FORBIDDEN)
              .body(Body::from("Forbidden"))
              .unwrap();
          }
        }
        next.run(req).await
      }
    },
  ));

  // Outermost layer: a panic in any handler (proxy or dashboard) becomes a
  // clean 500 for that one request instead of abruptly dropping the
  // connection. The panic is still logged by the global hook (see
  // `install_panic_logger`); every other in-flight request and the process
  // are unaffected.
  app
    .with_state(state)
    .layer(tower_http::catch_panic::CatchPanicLayer::new())
}

/// Spawns every background loop the server runs: the stats flush, the
/// autoscaling sampler and sweep, the uptime ticker, config hot-reload,
/// alerting, token-expiry warnings, the public expose listeners, retention,
/// backups, and the QoS 1 ack sweeper. Side effects only; the caller keeps
/// the state.
pub(crate) fn spawn_background(state: &Arc<AppState>, host: &str) {
  let state = state.clone();
  // Flush persistent stats periodically and once more on shutdown.
  let stats_flush_state = state.clone();
  crate::supervise::spawn_ticker("stats-flush", Duration::from_secs(30), move || {
    let state = stats_flush_state.clone();
    async move { flush_stats_once(&state).await }
  });

  // Autoscaling loops: the scale-out sampler and the record TTL sweep. Both
  // are no-ops unless the feature is enabled, so a server that never uses it
  // pays nothing.
  if state.config().scaling_enabled {
    let sampler_state = state.clone();
    crate::supervise::spawn_supervised("scaling-sampler", move || {
      crate::scaling::run_scale_out_loop(sampler_state.clone())
    });
    let prune_state = state.clone();
    let prune_ttl = state.config().scaling_record_ttl.as_secs();
    crate::supervise::spawn_supervised("scaling-prune", move || {
      crate::scaling::run_prune_loop(prune_state.clone(), prune_ttl)
    });
    info!("Autoscaling enabled: client `scaling:` declarations are honored");
  }

  // Availability ticker: observe every service entity and accrue elapsed
  // time into the uptime history (APERIO_UPTIME_TICK_SECS, default 10).
  let uptime_state = state.clone();
  let uptime_tick_secs = std::env::var("APERIO_UPTIME_TICK_SECS")
    .ok()
    .and_then(|v| v.parse::<u64>().ok())
    .filter(|v| *v >= 1)
    .unwrap_or(10);
  // The one loop that ticks before it sleeps, which is why it is written out
  // rather than using `spawn_ticker`: uptime accrues from the moment the
  // server is up, and skipping the first interval would lose it.
  crate::supervise::spawn_supervised("uptime", move || {
    let state = uptime_state.clone();
    async move {
      loop {
        uptime_tick_once(&state).await;
        tokio::time::sleep(Duration::from_secs(uptime_tick_secs)).await;
      }
    }
  });
  // Config hot-reload: watch aperio-server.yaml for changes and re-apply the
  // live-editable settings and structured headers/routes without a restart
  // (no `set_var`, so it is safe on the running server). Structural keys
  // (host/port/data_dir, proxy trust, OIDC, `expose` ports) still need a
  // restart. Off when no config file is in use, or when disabled explicitly.
  let hot_reload = std::env::var("APERIO_CONFIG_HOT_RELOAD")
    .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
    .unwrap_or(true);
  if hot_reload && let Some(watch_path) = config_file::watched_path() {
    let reload_state = state.clone();
    info!(
      "Watching {} for configuration changes",
      watch_path.display()
    );
    crate::supervise::spawn_supervised("config-hot-reload", move || {
      let state = reload_state.clone();
      let path = watch_path.clone();
      async move {
        // Re-read on restart rather than carrying a stale mtime across the
        // panic: a change made while the loop was down should be picked up,
        // not skipped because the remembered timestamp is newer.
        let mut last_mtime = std::fs::metadata(&path)
          .ok()
          .and_then(|m| m.modified().ok());
        loop {
          tokio::time::sleep(Duration::from_secs(5)).await;
          last_mtime = hot_reload_tick_once(&state, &path, last_mtime).await;
        }
      }
    });
  }

  // Threshold alerting: the two built-in rules (APERIO_ALERT_*) and any
  // operator-defined `alert_rules:`, evaluated by one background ticker and
  // emitted as webhook/audit events. The ticker runs when either exists,
  // since a file with only `alert_rules:` and no APERIO_ALERT_* would
  // otherwise have armed rules and nothing evaluating them.
  let rules = state.config().alert_rules.clone();
  for rule in rules.rules() {
    if !rule.metric.readable_here() {
      warn!(
        "Alert rule '{}' watches {}, which cannot be read on this platform; it will never fire",
        rule.name,
        rule.metric.as_str()
      );
    }
  }
  let alert_cfg = alerts::AlertConfig::from_env();
  if alert_cfg.is_some() || !rules.is_empty() {
    if !rules.is_empty() {
      info!("Alert rules armed: {}", rules.rules().len());
    }
    alerts::spawn(state.clone(), alert_cfg.unwrap_or_default());
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
    crate::supervise::spawn_supervised("token-expiry-warning", move || {
      let state = warn_state.clone();
      async move {
        // The warned set starts empty again after a restart, which re-arms
        // warnings that were already sent. That is the same thing a server
        // restart does, and a duplicate warning is a far better failure than
        // a silence caused by state nobody can inspect.
        let mut warned: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();
        loop {
          tokio::time::sleep(Duration::from_secs(60)).await;
          let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
          token_expiry_tick_once(&state, expiry_warning_secs, now, &mut warned).await;
        }
      }
    });
  }

  // Experimental public TCP expose ports (aperio-server.yaml `expose:`).
  expose::spawn_listeners(state.clone(), host, expose::from_config_file());

  // Per-data-type retention pruner (APERIO_RETENTION_*): inert when nothing
  // is configured.
  retention::spawn(state.clone());

  // Scheduled physical DB snapshots (APERIO_BACKUP_*): inert unless both an
  // interval and a directory are configured.
  backup::spawn(state.clone());

  // Routine sweeps of the per-IP/per-route rate buckets and expired
  // sessions, off the request path: the sweep used to ride on whichever
  // request drew the five-minute tick, with the lock held.
  let gc_state = state.clone();
  crate::supervise::spawn_ticker("rate-and-session-gc", Duration::from_secs(300), move || {
    let state = gc_state.clone();
    async move { state.gc_tick_once(Instant::now()).await }
  });

  // Resends QoS 1 messages nobody acknowledged, and gives up on the ones
  // that waited out the window.
  crate::tunnel::pubsub::run_ack_sweeper(state);
}

/// One beat of the stats flush loop: writes whatever is dirty.
pub(crate) async fn flush_stats_once(state: &Arc<AppState>) {
  state.persistent_stats.lock().await.save_if_dirty();
  state.uptime.lock().await.save_if_dirty();
  state.activity.lock().await.save_if_dirty();
}

/// One beat of the availability ticker: observe every service entity and
/// accrue the elapsed time into the uptime history.
pub(crate) async fn uptime_tick_once(state: &Arc<AppState>) {
  let live = observe_service_availability(state).await;
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0);
  state.uptime.lock().await.tick(now, live);
}

/// One beat of the hot-reload watcher: compares the file's mtime against the
/// last one seen and re-applies the live-editable settings when it moved.
/// Returns the mtime to compare against next time.
pub(crate) async fn hot_reload_tick_once(
  state: &Arc<AppState>,
  watch_path: &std::path::Path,
  last_mtime: Option<std::time::SystemTime>,
) -> Option<std::time::SystemTime> {
  let mtime = std::fs::metadata(watch_path)
    .ok()
    .and_then(|m| m.modified().ok());
  if mtime == last_mtime {
    return last_mtime;
  }
  match config_file::reload() {
    Ok(_) => {
      let diff = state.reload_from_file().await;
      info!(
        "Reloaded {}: live settings and headers/routes re-applied (structural keys need a restart)",
        watch_path.display()
      );
      let detail = if diff.is_empty() {
        format!("{} (no live-setting changes)", watch_path.display())
      } else {
        format!("{} | {}", watch_path.display(), diff.join(", "))
      };
      state
        .audit("config_reloaded", "system", "system", &detail)
        .await;
    }
    Err(e) => warn!("Config reload of {} failed: {}", watch_path.display(), e),
  }
  mtime
}

/// One beat of the token-expiry warning ticker: warns (once per token per
/// expiry) for every token whose remaining lifetime dropped under the window,
/// and forgets warnings whose expiry passed or moved.
pub(crate) async fn token_expiry_tick_once(
  state: &Arc<AppState>,
  expiry_warning_secs: u64,
  now: u64,
  warned: &mut std::collections::HashSet<(String, u64)>,
) {
  let expiring: Vec<(String, String, u64, Option<String>)> = {
    let store = state.token_store.lock().await;
    store
      .list()
      .iter()
      .filter_map(|t| {
        let exp = t.expires_at?;
        let expires_within = exp > now && exp - now <= expiry_warning_secs;
        (expires_within && !warned.contains(&(t.id.clone(), exp)))
          .then(|| (t.id.clone(), t.name.clone(), exp, t.org_id.clone()))
      })
      .collect()
  };
  for (id, name, exp, org) in expiring {
    warned.insert((id.clone(), exp));
    warn!(
      "Token '{}' expires in {} minutes (at unix {})",
      name,
      (exp - now) / 60,
      exp
    );
    state
      .audit_in(
        "token_expiring",
        "system",
        "system",
        org.clone(),
        &format!("name={} expires_at={}", name, exp),
      )
      .await;
    state
      .emit_event_in(
        "token_expiring",
        serde_json::json!({
          "id": id,
          "name": name,
          "expires_at": exp,
          "seconds_left": exp - now,
        }),
        org,
      )
      .await;
  }
  // Drop warned entries whose expiry has passed or moved (refresh).
  warned.retain(|(_, exp)| *exp > now);
}

/// Binds the listener (plain or SO_REUSEPORT), switches Nagle off per
/// accepted socket, and serves the app until the shutdown signal, exiting
/// the process when the port cannot be bound, exactly as before the split.
async fn serve_until_shutdown(state: Arc<AppState>, app: Router) {
  let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

  let port = std::env::var("PORT")
    .ok()
    .and_then(|p| p.parse::<u16>().ok())
    .unwrap_or(8080);

  // Zero-downtime restarts (APERIO_REUSEPORT): bind with SO_REUSEPORT so a new
  // process can bind the same host:port while the old one is still draining its
  // tunnels. Deploy = start the new process, then SIGTERM the old one; the
  // kernel load-balances new connections across both during the overlap, and
  // clients reconnect to whichever process survives.
  let reuseport = std::env::var("APERIO_REUSEPORT")
    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let listener = match bind_listener(&host, port, reuseport).await {
    Ok(l) => l,
    Err(e) => {
      error!("Failed to bind {}:{}: {}", host, port, e);
      std::process::exit(1);
    }
  };

  info!(
    "Aperio Server v{} listening on {}:{} with connection info tracing enabled{}",
    env!("CARGO_PKG_VERSION"),
    host,
    port,
    if reuseport { " (SO_REUSEPORT)" } else { "" }
  );

  // Nagle off on every accepted connection. These sockets carry messages
  // somebody is waiting for, not a bulk stream, and Nagle holds a small write
  // back until an outstanding acknowledgement arrives, which here is latency
  // added to a request rather than bandwidth saved. `tap_io` is axum 0.8's
  // hook for exactly this; 0.7 owned the accept loop and offered none.
  let listener = listener.tap_io(|stream| {
    if let Err(e) = stream.set_nodelay(true) {
      warn!("could not set TCP_NODELAY on an accepted connection: {e}");
    }
  });

  axum::serve(
    listener,
    app.into_make_service_with_connect_info::<SocketAddr>(),
  )
  .with_graceful_shutdown(shutdown_signal(state.clone()))
  .await
  .unwrap();
}

/// Minimum dashboard role a route requires. User management and server
/// settings can change who controls the server, admin only. Everything
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
    // import replaces them, admin only, even for the GET.
    || path == "/api/export"
    || path == "/api/import"
    // Session management exposes who is signed in from which IP/UA and can
    // end other admins' sessions, admin only, including the list.
    || path == "/api/sessions"
    || path.starts_with("/api/sessions/")
    // Organization management is master-super-admin only (checked in the
    // handlers); require at least Admin at the routing layer.
    || path == "/api/orgs"
    || path.starts_with("/api/orgs/")
    // Programmatic admin keys are powerful cross-org credentials, master
    // super-admin only (also checked in the handlers).
    || path == "/api/admin-keys"
    || path.starts_with("/api/admin-keys/")
  {
    return Role::Admin;
  }
  if matches!(*method, axum::http::Method::GET | axum::http::Method::HEAD) {
    Role::Viewer
  } else {
    Role::Operator
  }
}

/// Ceiling on `shutdown_drain: auto`.
///
/// `auto` sizes the drain from what connected clients announce, and a client
/// is not the operator: a value it invents cannot be allowed to hold the
/// process past what the platform will wait before sending SIGKILL. Thirty
/// seconds is the common orchestrator grace period, which is the number this
/// is actually racing.
const SHUTDOWN_DRAIN_AUTO_CAP: u64 = 30;

/// How long shutdown waits for in-flight proxied requests, and where the
/// number came from.
///
/// Split out so the policy is testable on its own: the loop that waits is
/// timing and signals, and this is the decision.
fn shutdown_drain_budget(
  configured: Option<u64>,
  auto: bool,
  announced: impl IntoIterator<Item = u64>,
) -> Duration {
  if let Some(secs) = configured {
    return Duration::from_secs(secs);
  }
  if !auto {
    return Duration::ZERO;
  }
  // The longest of them, not the average: the drain is over when the slowest
  // client has finished, and an average would cut short exactly the client
  // that needed the time.
  let longest = announced.into_iter().max().unwrap_or(0);
  Duration::from_secs(longest.min(SHUTDOWN_DRAIN_AUTO_CAP))
}

/// Waits for in-flight proxied requests to finish, up to the configured
/// budget.
///
/// Polled rather than signalled: the alternative is a notification on the
/// request bookkeeping's hot path, paid on every request for the benefit of
/// the one moment the process is shutting down.
async fn drain_in_flight(state: &Arc<AppState>) {
  let cfg = state.config();
  let announced: Vec<u64> = {
    let clients = state.clients.read().await;
    clients.values().filter_map(|c| c.drain_secs).collect()
  };
  let budget = shutdown_drain_budget(cfg.shutdown_drain, cfg.shutdown_drain_auto, announced);
  if budget.is_zero() {
    return;
  }
  let deadline = tokio::time::Instant::now() + budget;
  let mut announced_wait = false;
  loop {
    let pending = state.pending_requests.lock().await.len();
    if pending == 0 {
      if announced_wait {
        info!("In-flight requests finished; continuing shutdown");
      }
      return;
    }
    if tokio::time::Instant::now() >= deadline {
      warn!(
        "Shutdown drain of {}s expired with {} request(s) still in flight; ending them",
        budget.as_secs(),
        pending
      );
      return;
    }
    if !announced_wait {
      info!(
        "Waiting up to {}s for {} in-flight request(s) to finish",
        budget.as_secs(),
        pending
      );
      announced_wait = true;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
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
    let clients = state.clients.read().await;
    let notified = clients.len();
    for client in clients.values() {
      // try_send: a client with a full queue must not stall the shutdown.
      let _ = client.tx.try_send(Message::Text(json.clone().into()));
    }
    drop(clients);
    if notified > 0 {
      info!("Notified {} tunnel client(s) of the shutdown", notified);
      // Give the writer tasks a moment to flush the frame out.
      tokio::time::sleep(Duration::from_millis(200)).await;
    }
  }

  // Let what is already in flight finish before the connections carrying it
  // are ended. Behind a load balancer this is the number that decides whether
  // a deploy is invisible or shows up as a handful of 502s: the balancer needs
  // long enough to take this instance out of rotation, and the requests it
  // already sent need long enough to answer.
  drain_in_flight(&state).await;

  // Graceful shutdown only completes once every connection has ended, and
  // long-lived ones never end on their own. End them actively: dashboard SSE
  // streams watch this flag, and each tunnel read loop honors its disconnect
  // notify.
  let _ = state.shutdown.send(true);
  {
    let clients = state.clients.read().await;
    for client in clients.values() {
      client.disconnect.notify_waiters();
    }
  }

  // Last resort: anything still holding a connection open (a proxied
  // WebSocket/TCP/UDP relay, a stalled peer) must not keep the process alive
  // forever. Flush what matters and exit.
  let fallback = state.clone();
  let timeout = state.config().shutdown_timeout;
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_secs(timeout)).await;
    warn!("Graceful shutdown timed out after {timeout}s; forcing exit");
    fallback.persistent_stats.lock().await.save_if_dirty();
    fallback.uptime.lock().await.save_if_dirty();
    fallback.activity.lock().await.save_if_dirty();
    std::process::exit(0);
  });
}

/// In-process composition facade for the integration tests in `tests/`.
///
/// Hidden rather than private: an integration test is its own crate, so the
/// only way to hand it the composed server is a `pub` item, and the only
/// honest way to say "this is not API" is `#[doc(hidden)]` plus this notice.
/// Nothing here is stable, nothing here is for embedding Aperio.
#[doc(hidden)]
pub mod testkit {
  use std::sync::Arc;

  /// The composed server: the state behind it stays opaque.
  pub struct Composed {
    state: Arc<crate::state::AppState>,
    pub router: axum::Router,
  }

  /// Runs the real startup path (environment, stores, settings layering,
  /// router assembly) inside the calling process. `None` = the same refusals
  /// `build_state` logs. Spawns no background loops: the test decides what
  /// runs beside it.
  pub async fn compose() -> Option<Composed> {
    let bundle = crate::build_state().await?;
    let router = crate::build_router(bundle.state.clone(), bundle.metrics_enabled);
    Some(Composed {
      state: bundle.state,
      router,
    })
  }

  impl Composed {
    /// Runs the real serve loop, graceful shutdown included: it returns only
    /// once the shutdown signal has run. The caller owns the process-global
    /// consequences (the signal handlers, and shutdown_signal's ten-second
    /// force-exit fallback), which is why only a single-test integration
    /// binary should call this.
    pub async fn serve_until_shutdown(self) {
      crate::serve_until_shutdown(self.state, self.router).await;
    }

    /// Inserts a minimal connected-client record and returns the receiving
    /// end of its tunnel channel, so a test can observe what the server
    /// writes to clients (the shutdown notice, for one).
    pub async fn insert_probe_client(
      &self,
    ) -> tokio::sync::mpsc::Receiver<axum::extract::ws::Message> {
      let (tx, rx) = tokio::sync::mpsc::channel(16);
      let handle = crate::state::ClientHandle {
        service_custom_name: None,
        tx,
        disconnect: Arc::new(tokio::sync::Notify::new()),
        connected_at: std::time::Instant::now(),
        client_ip: "127.0.0.1".to_string(),
        request_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        declared_path: None,
        assigned_path: None,
        declared_hostname: Some("probe.example.com".to_string()),
        declared_hostnames: vec!["probe.example.com".to_string()],
        assigned_hostnames: Vec::new(),
        random_hostname: None,
        override_path_bind: None,
        override_hostname_binds: Vec::new(),
        capture: true,
        connections: None,
        connections_min: None,
        connections_max: None,
        declared_client_id: None,
        config_notes: Vec::new(),
        metrics_labels: Vec::new(),
        drain_secs: None,
        last_ping_at: Some(std::time::Instant::now()),
        perms: crate::state::ClientPerms::master(),
        max_concurrent: None,
        inflight_limiter: None,
        draining: false,
        admin_enabled: true,
        tcp_enabled: false,
        client_version: None,
        client_protocol: None,
        backend_healthy: true,
        backend_probed: true,
        cpu_percent: None,
        rss_bytes: None,
        rtt_ms: None,
        jitter_ms: None,
        reconnects: None,
        priority: 0,
        reported_instance_id: None,
        instance_group: None,
        subscriptions: Vec::new(),
        bandwidth_bps: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        service_name: None,
        public: false,
        public_denied_warned: false,
        visitor_auth: None,
        visitor_auth_denied_warned: false,
        allowed_ips: Vec::new(),
        allowed_ips_invalid_warned: false,
        scaling_invalid_warned: false,
        tunnels: Vec::new(),
        cache: false,
        cache_ignored_warned: false,
        resilience: false,
        max_request_body: None,
        response_timeout: None,
        webhook_inbox: false,
        denied: None,
        recent_failures: std::collections::VecDeque::new(),
        ejected_until: None,
      };
      self
        .state
        .clients
        .write()
        .await
        .insert("probe-client".to_string(), handle);
      rx
    }

    /// Serves the composed app on an ephemeral loopback port and returns the
    /// address; the serve task runs until the returned handle is aborted.
    pub async fn serve_ephemeral(&self) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let addr = listener.local_addr().unwrap();
      let app = self.router.clone();
      let handle = tokio::spawn(async move {
        axum::serve(
          listener,
          app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
      });
      (addr, handle)
    }

    /// How many live tunnel clients the state currently tracks, so a test
    /// can assert on the state the HTTP surface is serving from.
    pub async fn connected_clients(&self) -> usize {
      self.state.clients.read().await.len()
    }
  }
}

#[cfg(test)]
#[path = "test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

/// Snapshot of every service entity's availability, keyed by service name or
/// stable client id: `up` when at least one connection is heartbeat-healthy,
/// routable, and its backend probe passes; `degraded` when connected but not
/// serving (backend unhealthy, draining, or disabled); absent entities are
/// treated as `down` by the uptime store.
pub(crate) async fn observe_service_availability(
  state: &AppState,
) -> std::collections::HashMap<String, (crate::store::uptime::Availability, Option<String>)> {
  use crate::store::uptime::Availability;
  let down_threshold = state.config().client_down_threshold;
  let clients = state.clients.read().await;
  let mut out: std::collections::HashMap<String, (Availability, Option<String>)> =
    std::collections::HashMap::new();
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
    // Several connections may serve one entity; the best state wins. All
    // connections of one entity share its organization.
    let entry = out
      .entry(key)
      .or_insert((Availability::Down, handle.perms.org_id.clone()));
    let rank = |s: &Availability| match s {
      Availability::Up => 2,
      Availability::Degraded => 1,
      Availability::Down => 0,
    };
    if rank(&status) > rank(&entry.0) {
      entry.0 = status;
    }
    entry.1 = handle.perms.org_id.clone();
  }
  out
}
