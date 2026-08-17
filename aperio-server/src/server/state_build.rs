//! Resolving every setting into the one `AppState` the rest of the server
//! reads.
//!
//! One function, deliberately whole. It is a single pass over roughly two
//! hundred settings, and most of them are read once, validated against a
//! neighbour, and dropped into one struct literal at the end. Split into stages
//! it becomes a set of half-built structs handed between them, which is more
//! moving parts than the thing it would be organising.

use crate::settings::{SettingsOverrides, apply_settings_overrides, override_keys};
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
use tracing::{error, info, warn};

/// Everything the server resolves before it can exist: the environment (with
/// the yaml file already folded in by `config_file::load`), the persisted
/// stores, the settings-override layering, and the assembled `AppState`.
///
/// `None` means "refuse to start", and the reason has already been logged:
/// an invalid trusted-proxy list, admin allowlist, or outbound allowlist.
/// Split out of `async_main` (planned_features #21) so startup can be
/// exercised in-process instead of only as a spawned server.
// Split at the one seam this function has: `ServerConfig` absorbs the eighty
// settings, so only eight named values cross from [`config`] into the assembly
// below. Splitting anywhere *inside* the settings pass would mean handing
// partial configs between the pieces, which is what kept this file whole until
// the seam was found.
pub(crate) mod config;

pub(crate) async fn build_state() -> Option<StartupBundle> {
  let config::Resolved {
    config,
    access_log,
    admin_key_store,
    data_dir,
    inbox_store,
    metrics_enabled,
    require_hostname_bind,
    token_store,
  } = config::resolve()?;
  let config = config;

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

  // OIDC SSO configuration (optional). The issuer is a configured URL the
  // server fetches from, so it goes through the outbound fence like every
  // other one, which is also why this runs after the config is resolved.
  let oidc_runtime = oidc::load_from_env(&config.outbound_policy).await;

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
    jwks_cache: Mutex::new(HashMap::new()),
    forward_auth_cache: Mutex::new(HashMap::new()),
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
