//! Resolving every setting into the one `AppState` the rest of the server
//! reads.
//!
//! One function, deliberately whole. It is a single pass over roughly two
//! hundred settings, and most of them are read once, validated against a
//! neighbour, and dropped into one struct literal at the end. Split into stages
//! it becomes a set of half-built structs handed between them, which is more
//! moving parts than the thing it would be organising.

use crate::state::{
  AppState, CAPTURE_MAX_ENTRIES, ConnectionState, DurationHistogram, ServerStats,
};
use crate::store::audit::AuditLog;
use crate::store::stats::StatsStore;
use crate::store::webhooks::WebhookStore;
use crate::*;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, watch};
use tracing::{error, info};

/// Everything the server resolves before it can exist: the boot environment,
/// the settings document from the database (or the init file on a first
/// start), the persisted stores, and the assembled `AppState`.
///
/// `None` means "refuse to start", and the reason has already been logged.
// Split at the one seam this function has: `ServerConfig` absorbs the
// settings, so only the opened stores cross from [`config`] into the
// assembly below.
pub(crate) mod config;

pub(crate) async fn build_state(
  boot: crate::boot::Boot,
  settings: aperio_config::server_settings::ServerSettings,
  init: Option<crate::api::export::ImportDump>,
) -> Option<StartupBundle> {
  let data_dir = boot.data_dir.clone();
  crate::live::set(settings.clone());
  let config::Resolved {
    config,
    access_log,
    admin_key_store,
    inbox_store,
    token_store,
  } = match config::resolve(&settings, &boot.token, &data_dir) {
    Ok(resolved) => resolved,
    Err(e) => {
      error!(
        "Refusing to start: the stored settings are not usable ({e}). Fix them with `aperio-server settings reset <path>`."
      );
      return None;
    }
  };
  crate::outbound::set_egress(config.outbound_policy.egress.clone());
  let metrics_enabled = settings.metrics.enabled;
  let require_hostname_bind = config.require_hostname_bind;

  if require_hostname_bind {
    info!(
      "Hostname bind requirement is ENABLED: clients without a hostname bind will not receive traffic."
    );
  }

  // OIDC SSO configuration (optional). The issuer is a configured URL the
  // server fetches from, so it goes through the outbound fence like every
  // other one, which is also why this runs after the config is resolved.
  let oidc_runtime = oidc::load(&settings.oidc, &config.outbound_policy).await;

  // Copied out before config moves into the state (values needed by the
  // live structures below).
  let lockout_threshold = config.login_lockout_threshold;
  let lockout_secs = config.login_lockout_secs;
  let audit_max_size = config.audit_max_size;
  let audit_max_files = config.audit_max_files;

  let dashboard_enabled = settings.dashboard.enabled;

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
  // Store changes for the dashboard streams' topics. An import touches
  // every store at once, and a stream that lags simply refreshes every
  // change-driven topic it holds, so a modest buffer is enough.
  let (changes_tx, _) = tokio::sync::broadcast::channel(256);

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
    jwks_cache: Mutex::new(HashMap::new()),
    forward_auth_cache: Mutex::new(HashMap::new()),
    message_metrics: Default::default(),
    client_connected: client_connected_tx,
    dashboard_enabled,
    panel_hostnames: std::sync::RwLock::new(Vec::new()),
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
    changes_tx,
    config_store: std::sync::RwLock::new(Arc::new(config)),
    settings_store: Mutex::new(crate::store::settings::SettingsStore::load(&data_dir)),
    started_with: Arc::new(settings.clone()),
    data_dir: data_dir.clone(),
    exposes: Mutex::new(crate::expose_manager::ExposeManager::load(&data_dir)),
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
    pending_auth_asks: Mutex::new(HashMap::new()),
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

  // A first start configured from the init file applies the rest of the
  // document (tokens, users, organizations...) now that the stores exist.
  if let Some(init) = init
    && let Err(e) = crate::api::export::apply_document(&state, init).await
  {
    error!("Refusing to start: APERIO_INIT_FILE could not be applied ({e})");
    return None;
  }

  // The accounts the stores read wider than a single organization, once: a
  // master Admin from before per-organization grants existed is exactly as
  // able as it was, and this is where the operator is told to look at it.
  // In the audit log, because the person who created that account is not
  // the one reading the startup log.
  let widened_users = state.users.lock().await.take_widened();
  for name in widened_users {
    state
      .audit(
        "grants_widened_on_upgrade",
        "system",
        "system",
        &format!(
          "user={} grants=*:admin; an Admin of the master organization could already reach every \
           organization, narrow it from the Users page if it should not",
          name
        ),
      )
      .await;
  }
  let widened_keys = state.admin_key_store.lock().await.take_widened();
  for name in widened_keys {
    state
      .audit(
        "grants_widened_on_upgrade",
        "system",
        "system",
        &format!(
          "admin_key={} org=*; an Admin key of the master organization could already reach every \
           organization, revoke and re-create it narrower if it should not",
          name
        ),
      )
      .await;
  }

  // The panel set is a cache of the config and the org store; filled once
  // here, and again by every change to either.
  state.refresh_panel_hostnames().await;

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
