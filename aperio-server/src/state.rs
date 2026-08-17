//! Server state: what a connection is, what it is serving, and everything the
//! request path reads while deciding where a request goes.
//!
//! Split by what each piece answers about, rather than by type, because the
//! file was 3840 lines and the seams were already there in the reading:
//!
//! - [`client`] is the model of a connected client, its services, and what its
//!   token permits. The `#46` per-service work lives against these types.
//! - [`stream`] is everything with a body moving through it: the timelines, the
//!   flow-control pump, the stream handles and the pending-request map.
//! - [`latency`], [`capture`] and [`activity`] are the observability side, kept
//!   apart because they are read by the dashboard and written by the request
//!   path and share nothing else.
//! - [`limits`] is what a request is charged against, and [`admission`] is the
//!   half of `AppState` a request asks on the way in: quotas, the organization
//!   fence, rate budgets and the maintenance flag.
//!
//! [`AppState`] itself stays here: it is the thing that owns one of each, and a
//! reader looking for "what does the server have" should find it in the file
//! named after the module.
//!
//! Every type is re-exported, so `crate::state::Thing` still resolves wherever
//! it did before the split. That is deliberate: this was a move, and a move
//! that renamed three hundred call sites would be a different change wearing
//! the same commit message.

pub(crate) mod activity;
pub(crate) mod admission;
pub(crate) mod capture;
pub(crate) mod client;
pub(crate) mod latency;
pub(crate) mod limits;
pub(crate) mod stream;

pub(crate) use activity::*;
pub(crate) use capture::*;
pub(crate) use client::*;
pub(crate) use latency::*;
pub(crate) use limits::*;
pub(crate) use stream::*;

use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use tokio::sync::{Mutex, broadcast, mpsc, watch};

use crate::oidc;
use crate::store::audit::AuditLog;
use crate::store::stats::{self, StatsStore};
use crate::store::tokens::TokenStore;
use crate::store::webhooks::{self, WebhookStore};

use crate::settings::{ServerConfig, SettingsOverrides};

/// In-memory server-wide traffic statistics.
#[derive(Serialize, Clone)]
pub(crate) struct ServerStats {
  /// Total count of incoming proxied requests.
  pub(crate) total_requests: u64,
  /// Count of successful request forwards.
  pub(crate) successful_requests: u64,
  /// Count of failed request forwards.
  pub(crate) failed_requests: u64,
  /// Total bytes of payloads transferred through the server.
  pub(crate) total_bytes_transferred: u64,
}

/// Details of an active tunnel client connection.
#[derive(Serialize, Clone, utoipa::ToSchema)]
pub(crate) struct ClientDetail {
  /// Unique client UUID. Shared by every service the connection carries, so
  /// it identifies a row only together with `service_index`.
  pub(crate) id: String,
  /// Which of the connection's services this row is. `0` for a connection
  /// carrying one, which is every client that predates protocol v8, so a
  /// reader that ignores the field sees exactly what it saw before.
  pub(crate) service_index: usize,
  /// Remote socket IP address of the client connection.
  pub(crate) ip: String,
  /// Number of seconds elapsed since connection establishment.
  pub(crate) connected_for_seconds: u64,
  /// Total request count processed by this client connection.
  pub(crate) request_count: u64,
  /// Path bind in effect (declared by the client or granted by its token).
  pub(crate) path_bind: Option<String>,
  /// Hostnames in effect (declared, token-granted, and random-subdomain).
  pub(crate) hostname_binds: Vec<String>,
  /// The subset of `hostname_binds` the client asked for itself, in the order
  /// it declared them. The dashboard shows the first of these as the client's
  /// primary hostname, since a name the operator chose identifies the service
  /// better than one the server handed out.
  pub(crate) declared_hostnames: Vec<String>,
  /// The random subdomain the server assigned this client, if any. Listed
  /// apart from the declared names because it is the server's name, not the
  /// client's, and it changes whenever the subdomain pattern does.
  pub(crate) random_hostname: Option<String>,
  /// Name of the dynamic token this client authenticated with (None = master).
  pub(crate) token_name: Option<String>,
  /// Organization this client belongs to, from its token (None = master).
  pub(crate) org_id: Option<String>,
  /// Temporary server-side path bind override (dashboard overrule).
  pub(crate) override_path_bind: Option<String>,
  /// Temporary server-side hostname binds (dashboard overrule). Empty = none;
  /// otherwise this list is what the connection is routed on, in place of
  /// every declared and assigned name.
  pub(crate) override_hostname_binds: Vec<String>,
  /// Seconds elapsed since the last heartbeat Ping was received.
  pub(crate) last_ping_seconds_ago: Option<u64>,
  /// Concurrency limit announced by the client (None = unlimited).
  pub(crate) max_concurrent: Option<u32>,
  /// Client build version announced via Ping (None until the first Ping).
  pub(crate) version: Option<String>,
  /// Service name announced via Ping (multi-service clients).
  pub(crate) service: Option<String>,
  /// What that service is called on screen, when the client's file said so.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) service_custom_name: Option<String>,
  /// True when this client serves its traffic without the visitor auth gate.
  pub(crate) public: bool,
  /// True when this client gates its service behind a client-set visitor
  /// login (the credentials themselves are never exposed to the dashboard).
  pub(crate) visitor_auth: bool,
  /// Visitor IPs/CIDRs allowed to reach this client's service (empty = everyone).
  pub(crate) allowed_ips: Vec<String>,
  /// Tunnel protocol version announced via Ping.
  pub(crate) protocol: Option<u32>,
  /// True when the announced protocol version differs from the server's.
  pub(crate) protocol_mismatch: bool,
  /// Latest backend health verdict reported by the client's own probe.
  pub(crate) backend_healthy: bool,
  /// False only while a configured health check has not completed its first
  /// probe (dashboard shows "checking" instead of "backend down").
  pub(crate) backend_probed: bool,
  /// What the client reports about itself (planned_features #37): CPU as a
  /// percentage of one core and resident memory of the client process, then
  /// round-trip time, jitter and reconnects of this tunnel connection. All
  /// `None` from a client that does not report them, and the process figures
  /// are `None` where they cannot be read without guessing.
  pub(crate) cpu_percent: Option<f64>,
  pub(crate) rss_bytes: Option<u64>,
  pub(crate) rtt_ms: Option<u64>,
  pub(crate) jitter_ms: Option<u64>,
  pub(crate) reconnects: Option<u32>,
  /// Announced load-balancing priority tier (0 = primary, higher = standby).
  pub(crate) priority: u32,
  /// Announced downstream link capacity in bytes/second (None = unlimited).
  pub(crate) bandwidth_bps: Option<u64>,
  /// False when the client missed its heartbeat window and is out of the pool.
  pub(crate) healthy: bool,
  /// True while the client is gracefully draining before shutdown.
  pub(crate) draining: bool,
  /// True while the client is passively ejected from routing after crossing the
  /// outlier failure threshold (5xx / timeout / connection loss). The tunnel
  /// connection stays up; it is re-admitted automatically when the window ends.
  pub(crate) ejected: bool,
  /// Dashboard kill switch state (false = excluded from routing).
  pub(crate) enabled: bool,
  /// True when the service opted into caching (`cache: true`) but the server's
  /// response cache is disabled (APERIO_CACHE off), so the opt-in does nothing.
  pub(crate) cache_ignored: bool,
  /// False when this service asked not to be recorded for the request
  /// inspector (`capture: false`).
  pub(crate) capture: bool,
  /// True when the service is willing to be recorded but the server has the
  /// inspector off altogether, so nothing about it can be inspected however
  /// the client is configured.
  pub(crate) capture_disabled_by_server: bool,
  /// Client-process instance id self-reported via Ping (`--client-id`).
  pub(crate) instance_id: Option<String>,
  /// True when another live connection reports the same instance id, a
  /// misconfiguration warning surfaced in the dashboard (`--bind-tunnels`
  /// and failover `wait` lookups become ambiguous).
  pub(crate) instance_id_shared: bool,
  /// Process-wide instance group id (the client's raw `client_id` base, shared
  /// by every service and every parallel connection of one client process).
  /// `None` for clients that predate the `x-aperio-instance` handshake header.
  /// The dashboard groups connections by this so a multi-connection client
  /// shows as one entity.
  pub(crate) instance_group: Option<String>,
}

/// Enhanced metrics stats combined with active client details.
#[derive(Serialize, Clone, utoipa::ToSchema)]
pub(crate) struct EnhancedServerStats {
  /// Total incoming request count.
  pub(crate) total_requests: u64,
  /// Successful requests count.
  pub(crate) successful_requests: u64,
  /// Failed requests count.
  pub(crate) failed_requests: u64,
  /// Total bytes transferred.
  pub(crate) total_bytes_transferred: u64,
  /// Current count of connected tunnel clients.
  pub(crate) connected_clients_count: usize,
  /// Uptime in seconds.
  pub(crate) uptime_seconds: u64,
  /// Request count waiting in the reconnection buffer.
  pub(crate) pending_requests_count: usize,
  /// List of client connection details.
  pub(crate) active_clients: Vec<ClientDetail>,
  /// Restart-surviving counters and period buckets.
  pub(crate) persistent: stats::PersistentStats,
  /// All-time average response time in milliseconds.
  pub(crate) avg_response_ms: f64,
  /// Today.s traffic bucket.
  pub(crate) today: stats::PeriodStats,
}

/// One server event on its way to the dashboard's notification bell.
///
/// The same events that feed webhooks and the `$aperio/` topics, carried on a
/// broadcast channel so an open dashboard hears about a client dropping, a
/// token about to expire or an alert firing without polling a table for it.
/// `org` is the organization the event belongs to (`None` = master) and is
/// what the SSE handler filters on, so an event never leaves its own org.
#[derive(Serialize, Clone, utoipa::ToSchema)]
pub(crate) struct ServerEvent {
  /// Event name, as webhooks receive it (`client_disconnected`, ...).
  pub(crate) event: String,
  /// When it happened, RFC 3339 in the server's local zone, the same format
  /// the webhook payload and the audit log use.
  pub(crate) timestamp: String,
  /// The event's own fields, verbatim from the webhook payload.
  pub(crate) data: serde_json::Value,
  /// Owning organization; `None` is master. Not serialized to the dashboard,
  /// which only ever receives the events of its own org anyway.
  #[serde(skip)]
  #[schema(ignore)]
  pub(crate) org: Option<String>,
}

/// Structure representing a logged HTTP transaction.
#[derive(Serialize, Clone, utoipa::ToSchema)]
pub(crate) struct RequestLog {
  /// Request UUID.
  pub(crate) id: String,
  /// Timestamp formatted as string.
  pub(crate) timestamp: String,
  /// HTTP method (GET, POST, etc.).
  pub(crate) method: String,
  /// Request URI path.
  pub(crate) uri: String,
  /// Status code returned.
  pub(crate) status: Option<u16>,
  /// Duration of processing in milliseconds.
  pub(crate) duration_ms: u128,
  /// Reason string if request failed.
  pub(crate) error: Option<String>,
  /// Request hostname (None for failures resolved before routing). Also the
  /// selector the right-to-erasure purge matches log entries on.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) host: Option<String>,
  /// Organization of the client that served the request (None = master, or a
  /// server-level failure with no client). The dashboard traffic log and live
  /// stream are filtered to the caller's effective org on this field.
  #[serde(skip)]
  pub(crate) org_id: Option<String>,
}

/// Connection liveness state, kept under a single lock for consistent snapshots.
pub(crate) struct ConnectionState {
  pub(crate) connected: bool,
  pub(crate) last_disconnect: Option<Instant>,
}

/// One maintenance flag: who owns it, why it is up, and when it ends.
///
/// It started as just the owning organization. The rest is what someone
/// asking "why is this site 503ing" needs and had to find in a chat log: a
/// reason, and an expiry, because the flag that causes an outage is the one
/// switched on for twenty minutes of work and left up.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct MaintenanceFlag {
  /// The organization that enabled it (`None` = master).
  pub(crate) org: Option<String>,
  /// Free text shown on the 503 page and in the dashboard. Empty = none.
  pub(crate) reason: Option<String>,
  /// Unix seconds after which the flag stops applying and is swept away.
  /// `None` = until someone turns it off.
  pub(crate) until: Option<u64>,
  /// Unix seconds when it was set, and who set it.
  pub(crate) since: u64,
  pub(crate) actor: String,
}

impl MaintenanceFlag {
  /// True once `until` has passed. An expired flag serves no request, and is
  /// removed on the next write rather than on a timer: a sweep that has to be
  /// scheduled is a sweep that can stop running.
  pub(crate) fn expired(&self, now: u64) -> bool {
    self.until.is_some_and(|end| now >= end)
  }

  /// Seconds until it lifts, for `Retry-After`. `None` when open-ended, where
  /// the response keeps its fixed fallback rather than promising a time.
  pub(crate) fn retry_after(&self, now: u64) -> Option<u64> {
    self.until.map(|end| end.saturating_sub(now).max(1))
  }
}

/// Core shared state of the Aperio server, accessed concurrently by multiple handlers.
/// One served (or refused) request's bookkeeping, handed to the telemetry
/// collector task instead of being written under four global locks on the
/// request's own back.
pub(crate) struct TelemetryEvent {
  /// The dashboard log entry; its fields (host, uri, org) also feed the
  /// per-endpoint and per-route records.
  pub(crate) log: RequestLog,
  pub(crate) status: u16,
  pub(crate) duration_ms: u64,
  /// True for a served request (feeds the endpoint/trend records); false for
  /// a refusal, which only the activity ring counts, as a failure.
  pub(crate) success: bool,
  pub(crate) now_secs: u64,
}

pub(crate) struct AppState {
  /// Every live tunnel connection. An RwLock rather than a Mutex: routing,
  /// stats, pubsub fan-out and the maps read it on every request, and only
  /// connect/disconnect and the heartbeat's announcement processing mutate
  /// it. Under a Mutex those readers serialized on one queue with each other
  /// and with every heartbeat.
  pub(crate) clients: tokio::sync::RwLock<HashMap<String, ClientHandle>>,
  /// Client-to-client dependencies observed on the tunnel endpoints: who is
  /// dialing whose tunnel (planned_features #56). Behind its own Mutex rather
  /// than folded into `clients`: a consumer is not a registered client, and
  /// this is written on connect/disconnect and read only when somebody opens
  /// the topology view.
  pub(crate) consumers: tokio::sync::Mutex<crate::consumers::Consumers>,
  /// Queue into the telemetry collector task, which owns the writes to
  /// `endpoint_stats`/`route_trends`/`activity`/`recent_logs`. The request
  /// path try_sends one event; a full or absent queue falls back to writing
  /// inline, so nothing is ever lost, only contended for the old way.
  pub(crate) telemetry_tx: tokio::sync::mpsc::Sender<TelemetryEvent>,
  /// QoS 1 messages handed to a client process and not yet acknowledged,
  /// keyed by that process *and its organization*. Bounded in count and in
  /// age: it covers a connection that died between the write and the
  /// acknowledgement, not a subscriber that is away.
  ///
  /// The organization is part of the key because the process half of it is
  /// the `x-aperio-instance` header, which the client chooses. Keyed on the
  /// process alone, two tenants announcing the same instance id would share
  /// one queue, and a redelivery would go to whichever of them the client map
  /// happened to yield first.
  pub(crate) pending_messages:
    Mutex<HashMap<crate::tunnel::pubsub::PendingKey, Vec<crate::tunnel::pubsub::Pending>>>,
  /// Verdicts remembered for the `forward` visitor-auth method, keyed on the
  /// credential that produced them. Only admissions are kept, so a visitor
  /// who has just been let in is not turned away for the rest of the window.
  pub(crate) forward_auth_cache: Mutex<HashMap<String, crate::forward_auth::CachedVerdict>>,
  /// Public keys fetched for the `jwt` visitor-auth method, by JWKS URL.
  ///
  /// Cached because verifying a token must not be a request to somebody
  /// else's server, and re-fetched when a token names a key id that is not
  /// here, which is what a rotation looks like from this side.
  pub(crate) jwks_cache: Mutex<HashMap<String, crate::jwt::CachedJwks>>,
  /// Counters for the messaging path, rendered by the metrics endpoint.
  pub(crate) message_metrics: crate::tunnel::pubsub::MessageMetrics,
  pub(crate) client_connected: watch::Sender<bool>,
  /// Persisted autoscaling records, armed per bind.
  pub(crate) scaling_store: Mutex<crate::store::scaling::ScalingStore>,
  /// In-memory per-bind scaling state (single flight, cooldown, breaker).
  pub(crate) scaling_runtime: Mutex<crate::scaling::ScalingRuntime>,
  /// Caps how many capacity calls may be in flight across every bind at once.
  pub(crate) scaling_calls: Arc<tokio::sync::Semaphore>,
  pub(crate) connection_state: Mutex<ConnectionState>,
  pub(crate) server_start_time: Instant,
  pub(crate) pending_requests: Mutex<HashMap<String, PendingRequest>>,
  pub(crate) stats: Mutex<ServerStats>,
  pub(crate) recent_logs: Mutex<VecDeque<RequestLog>>,
  /// Live traffic fan-out: each proxied request's `RequestLog` is broadcast to
  /// any connected dashboard SSE subscribers (`/aperio/api/stream`). Dropped
  /// when there are no subscribers.
  pub(crate) traffic_tx: broadcast::Sender<RequestLog>,
  /// Live server-event fan-out: every event that reaches a webhook or the
  /// `$aperio/` bus is also broadcast here, for the dashboard's notification
  /// bell (`/aperio/api/stream`, `notification` events). Dropped when there
  /// are no subscribers, exactly like [`AppState::traffic_tx`].
  pub(crate) events_tx: broadcast::Sender<ServerEvent>,
  /// Live server configuration. Dashboard-editable settings swap in a new
  /// `Arc<ServerConfig>`; every access takes a cheap read-lock snapshot via
  /// [`AppState::config`].
  pub(crate) config_store: std::sync::RwLock<Arc<ServerConfig>>,
  /// Configuration as derived from environment variables only, used as the
  /// base that persisted overrides apply on top of (and for "reset").
  pub(crate) config_env_defaults: Arc<ServerConfig>,
  /// Currently persisted dashboard overrides (subset of settings).
  pub(crate) settings_overrides: Mutex<SettingsOverrides>,
  /// Path of the persisted overrides file (`<data_dir>/settings.json`).
  pub(crate) settings_path: std::path::PathBuf,
  /// True when the admin dashboard is served (APERIO_DASHBOARD != 0); the
  /// first-run helper redirect to /aperio only makes sense when it is.
  pub(crate) dashboard_enabled: bool,
  /// Flipped to true once a shutdown signal arrives; long-lived streams
  /// (dashboard SSE) watch it and end so graceful shutdown can complete.
  pub(crate) shutdown: watch::Sender<bool>,
  /// Currently in-flight proxied requests, checked against the (live,
  /// dashboard-editable) max_concurrent_requests limit. A plain counter
  /// instead of a semaphore so the limit can change at runtime.
  pub(crate) active_proxied_requests: Arc<AtomicUsize>,
  /// Currently-live proxied public WebSockets, checked against
  /// `max_ws_connections`. WebSockets are long-lived, so they get their own
  /// counter separate from the (short-lived) HTTP request slots above.
  pub(crate) active_ws_connections: Arc<AtomicUsize>,
  pub(crate) path_rr: Mutex<HashMap<RouteGroupKey, usize>>,
  /// Dashboard sessions, persisted in SQLite so restarts don't sign
  /// everyone out.
  pub(crate) sessions: Mutex<crate::store::sessions::SessionStore>,
  pub(crate) rate_limiter: Mutex<HashMap<IpAddr, RateLimitState>>,
  /// Escalating per-IP failed-login lockout (brute-force protection).
  pub(crate) login_lockout: Mutex<crate::auth::LockoutTracker>,
  /// Per-token request rate buckets (key = dynamic token record id),
  /// enforcing the token's optional `max_rps`.
  pub(crate) token_rate: Mutex<HashMap<String, RateLimitState>>,
  /// Per-token daily byte usage: token id → (day key, bytes). In-memory
  /// only, a restart resets the current day's usage.
  pub(crate) token_daily_bytes: Mutex<HashMap<String, (String, u64)>>,
  /// Source IPs a dynamic token has connected from (token id → set of IPs).
  /// In-memory only; drives the `token_new_ip` alert when a token connects
  /// from an address it has not been seen from before this run.
  pub(crate) token_seen_ips: Mutex<HashMap<String, HashSet<IpAddr>>>,
  /// Per-route request-rate buckets (key = matched `rate_limits:` rule),
  /// enforcing the section's aggregate rps/burst per host+path. GC'd on size.
  pub(crate) route_rate: Mutex<HashMap<String, RateLimitState>>,
  pub(crate) active_tunnel_count: AtomicUsize,
  /// Active WebSocket proxy streams: stream_id → sender to relay tunnel WsData to public WS.
  pub(crate) ws_streams: Mutex<HashMap<String, WsStreamHandle>>,
  /// Pending WebSocket upgrade responses: upgrade_id → oneshot to resolve when client responds.
  pub(crate) pending_upgrades: Mutex<HashMap<String, PendingRequest>>,
  /// Persistent store of dashboard-created dynamic API tokens.
  pub(crate) token_store: Mutex<TokenStore>,
  /// Persistent store of programmatic admin API keys (Bearer auth for the
  /// dashboard API; scoped by role + organization).
  pub(crate) admin_key_store: Mutex<crate::store::admin_keys::AdminKeyStore>,
  /// Persistent inbound-webhook inbox (`webhook_inbox: true` services).
  pub(crate) inbox_store: Mutex<crate::store::inbox::InboxStore>,
  /// Dashboard users (role-based access; separate from tunnel tokens).
  pub(crate) users: Mutex<crate::store::users::UserStore>,
  /// In-flight streamed response bodies: request_id → chunk sender.
  pub(crate) response_streams: Mutex<HashMap<String, ResponseStreamHandle>>,
  /// Recently captured HTTP transactions for the dashboard inspector.
  pub(crate) captured_requests: Mutex<VecDeque<CapturedRequest>>,
  /// Persistent audit log of administrative/security events.
  pub(crate) audit: Mutex<AuditLog>,
  /// Restart-surviving traffic statistics (all-time + period buckets).
  pub(crate) persistent_stats: Mutex<StatsStore>,
  /// Persistent webhook definitions for the event system.
  pub(crate) webhook_store: Mutex<WebhookStore>,
  /// Child organizations (multi-tenancy); master is implicit (org_id None).
  pub(crate) org_store: Mutex<crate::store::orgs::OrgStore>,
  /// Persistent log of webhook delivery outcomes (shared with the delivery
  /// tasks, which record after their retries finish).
  pub(crate) webhook_deliveries: std::sync::Arc<Mutex<webhooks::DeliveryLog>>,
  /// WebAuthn verifier for passkey sign-in (None until
  /// APERIO_WEBAUTHN_ORIGIN is configured).
  pub(crate) webauthn: Option<webauthn_rs::Webauthn>,
  /// In-flight WebAuthn registration/authentication ceremonies.
  pub(crate) webauthn_ceremonies: Mutex<crate::webauthn::WebauthnCeremonies>,
  /// Per-service availability history (uptime/SLA reporting).
  pub(crate) uptime: Mutex<crate::store::uptime::UptimeStore>,
  /// OIDC SSO runtime config (None = feature disabled).
  pub(crate) oidc: Option<oidc::OidcRuntime>,
  /// Per-organization OIDC runtimes, built lazily from each org's stored
  /// config and cached by org id (invalidated when the org's OIDC is updated).
  pub(crate) org_oidc: Mutex<HashMap<String, oidc::OidcRuntime>>,
  /// Pending OIDC login flows: state token → (original redirect, bound org id
  /// for a per-org login, the callback URL sent to the provider, expiry).
  pub(crate) oidc_states: Mutex<HashMap<String, OidcStateEntry>>,
  /// Active experimental TCP tunnel streams: stream_id → consumer sender.
  pub(crate) tcp_streams: Mutex<HashMap<String, TcpStreamHandle>>,
  /// Active UDP relay streams (declared `protocol: udp` tunnels):
  /// stream_id → consumer sender. Same handle shape as TCP; the payloads are
  /// whole datagrams instead of stream bytes.
  pub(crate) udp_streams: Mutex<HashMap<String, UdpStreamHandle>>,
  /// Server-side GET response cache (APERIO_CACHE; see the cache module).
  pub(crate) response_cache: Mutex<crate::cache::ResponseCache>,
  /// Cacheable GET misses currently being fetched, keyed like the response
  /// cache (`host|uri`). Concurrent identical misses subscribe to the
  /// leader's watch channel and re-check the cache when it completes
  /// (single-flight coalescing). Sync mutex: only held for map ops.
  pub(crate) cache_inflight:
    std::sync::Mutex<std::collections::HashMap<String, tokio::sync::watch::Receiver<bool>>>,
  /// Rolling per-stage latency statistics per route (in-memory).
  pub(crate) stage_stats: Mutex<StageStats>,
  /// Rolling per-endpoint latency windows (slowest-endpoints report).
  pub(crate) endpoint_stats: Mutex<EndpointStats>,
  /// Rolling per-route minute-bucketed status trends (dashboard sparklines).
  pub(crate) route_trends: Mutex<RouteTrends>,
  /// Request volume in five-second slices over the last quarter hour, per
  /// organization: the long view of the dashboard's live activity chart, kept
  /// here so it survives a reload and is the same for everyone looking.
  pub(crate) activity: Mutex<Activity>,
  /// Hostnames and patterns currently in maintenance mode (`*` = every
  /// hostname, `*.example.com` = every subdomain of it), mapped to what is
  /// known about each flag. Requests to them get a 503 page even while
  /// clients are connected. In-memory only, like bind overrides: cleared by a
  /// server restart.
  pub(crate) maintenance: Mutex<std::collections::HashMap<String, MaintenanceFlag>>,
  /// Structured access log file (APERIO_ACCESS_LOG): one JSON line per
  /// proxied request, ready for Loki/ClickHouse ingestion. The same data is
  /// always emitted as structured `aperio_access` tracing events on stdout.
  pub(crate) access_log: Option<tokio::sync::mpsc::Sender<crate::access_log::AccessLogCmd>>,
  /// Request duration histogram exposed on `/aperio/metrics`.
  pub(crate) duration_histogram: DurationHistogram,
  /// Refusals by limit, for `aperio_rate_limited_total`. A load test asks
  /// "which limit is firing", and that is a counter's question, not a
  /// header's.
  pub(crate) limit_counters: crate::limits::LimitCounters,
  /// Streamed responses currently open, per visitor address
  /// (planned_features #20). A `std` mutex rather than a `tokio` one: every
  /// operation is a hash lookup and an integer, and the guard has to be
  /// releasable from `Drop`, which cannot await.
  pub(crate) stream_counts: Arc<std::sync::Mutex<HashMap<std::net::IpAddr, u32>>>,
}

/// What one call costs against the per-IP bucket (planned_features #64).
///
impl AppState {
  /// Whether `client_id` announced a protocol version with per-stream flow
  /// control (v3+), i.e. it honors `StreamPause`/`StreamResume`. Read when a
  /// stream starts; a client is only routable once its first Ping announced
  /// the version, so the answer is stable by then.
  /// The tunnel protocol version this connection announced (via Ping), or 1
  /// for a client that has not announced yet. Read once per relay stream:
  /// the announcement precedes routability, so the answer is stable by the
  /// time any stream targets the connection.
  pub(crate) async fn client_protocol(&self, client_id: &str) -> u32 {
    self
      .clients
      .read()
      .await
      .get(client_id)
      .and_then(|h| h.client_protocol)
      .unwrap_or(1)
  }

  pub(crate) async fn client_supports_pause(&self, client_id: &str) -> bool {
    self
      .clients
      .read()
      .await
      .get(client_id)
      .and_then(|h| h.client_protocol)
      .unwrap_or(0)
      >= 3
  }

  /// The flow-control watermarks new streams start with: the live config's
  /// values, repaired into a consistent trio. Snapshotted per stream, so a
  /// settings change applies to streams started after it.
  pub(crate) fn stream_limits(&self) -> StreamLimits {
    let c = self.config();
    StreamLimits::sanitized(
      c.stream_pause_bytes,
      c.stream_resume_bytes,
      c.stream_backlog_limit,
      c.stream_min_throughput,
    )
  }

  /// Rebuilds the live config from the layers (env defaults ->
  /// `aperio-server.yaml` live settings -> dashboard overrides) with the
  /// current structured `headers`/`routes`, and applies it. Called on file
  /// hot-reload. Structural keys (host/port/data_dir, proxy trust, OIDC,
  /// `expose` ports) are not re-applied live and need a restart.
  /// Returns the list of live-setting keys that changed (`key: old→new`, with
  /// secrets masked) so the caller can record it in the `config_reloaded`
  /// audit entry.
  pub(crate) async fn reload_from_file(self: &Arc<Self>) -> Vec<String> {
    let file_layer = crate::settings::file_overrides();
    let dashboard = self.settings_overrides.lock().await.clone();
    let base = crate::settings::apply_settings_overrides(&self.config_env_defaults, &file_layer);
    let mut effective = crate::settings::apply_settings_overrides(&base, &dashboard);
    effective.header_rules = crate::headers::from_config_file();
    effective.static_routes = crate::static_routes::from_config_file();
    effective.error_pages = crate::error_pages::from_config_file();
    effective.route_limits = crate::route_limits::from_config_file();
    effective.fallbacks = crate::fallbacks::from_config_file();
    effective.waf = crate::waf::from_config_file();
    effective.maintenance_windows = crate::maintenance_windows::from_config_file();
    effective.alert_rules = crate::alert_rules::from_config_file();
    effective.denied_ips = crate::deny_list::from_config();
    let old = self.config();
    let diff = crate::settings::config_reload_diff(&old, &effective);
    crate::api::settings::swap_config(self, effective).await;
    diff
  }

  /// Snapshot of the live configuration (cheap Arc clone).
  pub(crate) fn config(&self) -> Arc<ServerConfig> {
    // Recover from a poisoned lock rather than panicking: config() is on
    // essentially every proxied request, so a single panic under the write
    // lock must not turn into a total outage, the stored config is a valid
    // Arc regardless of who poisoned the lock.
    self
      .config_store
      .read()
      .unwrap_or_else(|e| e.into_inner())
      .clone()
  }

  /// Claims a slot under `max_concurrent_requests`, or None when the server
  /// is at capacity. The limit is read live, so dashboard edits apply to the
  /// very next request.
  pub(crate) fn try_acquire_request_slot(&self) -> Option<RequestSlot> {
    let limit = self.config().max_concurrent_requests;
    self
      .active_proxied_requests
      .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
        if cur < limit { Some(cur + 1) } else { None }
      })
      .ok()
      .map(|_| RequestSlot(self.active_proxied_requests.clone()))
  }

  /// Claims a slot under `max_streams_per_ip` for one visitor address, or
  /// `None` when that address already holds its share.
  ///
  /// `Some` with no accounting when the limit is off, which is the default:
  /// the ceiling protects against one host holding many slow streams, and the
  /// number that does so without cutting off a legitimate visitor depends
  /// entirely on the deployment. A NAT or a CGNAT puts many real people behind
  /// one address, so a default here would be a guess with a queue of users
  /// behind it.
  pub(crate) fn try_acquire_stream_slot(&self, ip: std::net::IpAddr) -> Option<StreamSlot> {
    let limit = self.config().max_streams_per_ip;
    if limit == 0 {
      return None;
    }
    let mut counts = self.stream_counts.lock().unwrap_or_else(|e| e.into_inner());
    let entry = counts.entry(ip).or_insert(0);
    if *entry >= limit {
      return None;
    }
    *entry += 1;
    Some(StreamSlot {
      ip,
      counts: self.stream_counts.clone(),
    })
  }

  /// Claims a live-WebSocket slot under `max_ws_connections`, or None at
  /// capacity. Held (via the returned [`WsSlot`]) for the whole life of the
  /// proxied WebSocket, so long-lived connections can't pile up unbounded.
  pub(crate) fn try_acquire_ws_slot(&self) -> Option<WsSlot> {
    let limit = self.config().max_ws_connections;
    self
      .active_ws_connections
      .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
        if cur < limit { Some(cur + 1) } else { None }
      })
      .ok()
      .map(|_| WsSlot(self.active_ws_connections.clone()))
  }

  /// Records a server-global (master-organization) audit event: config
  /// reloads, export/import, failed logins, and other events not tied to a
  /// child organization. Org-scoped actions use [`audit_in`] or the
  /// [`audit_session`] convenience instead.
  pub(crate) async fn audit(&self, event: &str, actor: &str, actor_ip: &str, details: &str) {
    self
      .audit
      .lock()
      .await
      .record(event, actor, actor_ip, None, details);
  }

  /// Records an audit event scoped to a specific organization (`None` = the
  /// implicit master org). Use when the event belongs to a child org, e.g. a
  /// client of that org connecting, or a token of that org being created.
  pub(crate) async fn audit_in(
    &self,
    event: &str,
    actor: &str,
    actor_ip: &str,
    org: Option<String>,
    details: &str,
  ) {
    self
      .audit
      .lock()
      .await
      .record(event, actor, actor_ip, org, details);
  }

  /// Records an audit event for a dashboard action, resolving both the acting
  /// user and the caller's effective organization from the request. This is the
  /// common path for session-driven admin actions, so the event is filed under
  /// whichever org the caller is currently acting in.
  pub(crate) async fn audit_session(
    &self,
    event: &str,
    headers: &axum::http::HeaderMap,
    actor_ip: &str,
    details: &str,
  ) {
    let actor = self.session_actor(headers).await;
    let org = crate::auth::effective_org(self, headers).await;
    self
      .audit
      .lock()
      .await
      .record(event, &actor, actor_ip, org, details);
  }

  /// Resolves the acting dashboard user for an audit record from the request:
  /// the signed-in username, "aperio" for the built-in admin (master token /
  /// dashboard password / OIDC), or "-" when there is no valid session.
  pub(crate) async fn session_actor(&self, headers: &axum::http::HeaderMap) -> String {
    match crate::auth::dashboard_role(self, headers).await {
      Some(_) => {
        if let Some(user) = crate::auth::dashboard_username(self, headers).await {
          user
        } else if let Some((_, _, name)) = crate::auth::admin_key_identity(self, headers).await {
          // Programmatic admin key: attribute the action to the key by name.
          format!("key:{name}")
        } else {
          "aperio".to_string()
        }
      }
      None => "-".to_string(),
    }
  }

  /// Delivers a server-global (master-organization) event to its subscribed
  /// webhooks. Org-scoped events use [`emit_event_in`].
  pub(crate) async fn emit_event(&self, event: &str, data: serde_json::Value) {
    self.emit_event_in(event, data, None).await;
  }

  /// Delivers an event to the webhooks of one organization (`None` = master):
  /// a webhook only ever fires for events in its own org, so a child org's
  /// webhook never learns about master's or another org's activity.
  pub(crate) async fn emit_event_in(
    &self,
    event: &str,
    data: serde_json::Value,
    org: Option<String>,
  ) {
    let subs: Vec<_> = self
      .webhook_store
      .lock()
      .await
      .subscribers(event)
      .into_iter()
      .filter(|w| w.org_id == org)
      .collect();
    webhooks::dispatch(
      subs,
      event,
      data.clone(),
      self.webhook_deliveries.clone(),
      self.config().outbound_policy.clone(),
    );
    self.broadcast_event(event, &data, &org);
    self.publish_event_topic(event, data, org).await;
  }

  /// Fans an event out to the dashboards of its own organization.
  ///
  /// `send` fails only when nobody is subscribed, which is the normal state of
  /// a server with no dashboard open, so the result is deliberately dropped.
  fn broadcast_event(&self, event: &str, data: &serde_json::Value, org: &Option<String>) {
    let _ = self.events_tx.send(ServerEvent {
      event: event.to_string(),
      timestamp: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
      data: data.clone(),
      org: org.clone(),
    });
  }

  /// Mirrors a server event onto its `$aperio/` topic, so a client can react
  /// to infrastructure the same way it reacts to anything else.
  ///
  /// The events already existed and already fed webhooks; putting them on
  /// topics is what lets a client hear about a peer connecting without
  /// standing up an HTTP receiver for it. `client_draining` becomes
  /// `$aperio/client/draining`: the underscore separates a level, the way a
  /// topic reads.
  ///
  /// Nothing is spent when nobody is listening: `publish` walks the client map
  /// and finds no subscriber, and a bare `#` deliberately does not match this
  /// namespace, so a client debugging with a wildcard is not enrolled in it.
  async fn publish_event_topic(&self, event: &str, data: serde_json::Value, org: Option<String>) {
    let topic = format!(
      "{}{}",
      aperio_config::RESERVED_TOPIC_PREFIX,
      event.replace('_', "/")
    );
    let payload = serde_json::to_vec(&data).unwrap_or_default();
    let _ = crate::tunnel::pubsub::publish(
      self,
      org.as_deref(),
      &topic,
      &payload,
      crate::tunnel::pubsub::Publisher::Server,
      // Server events are a stream of what is happening now. A client that
      // was not connected did not miss anything it can act on.
      0,
    )
    .await;
  }

  /// Force-disconnects every live tunnel connection authenticated with the
  /// given dynamic token: their read loops end and they leave the routing pool
  /// immediately, instead of serving until they next reconnect (when the
  /// revoked token would be rejected anyway). Returns how many were dropped.
  pub(crate) async fn disconnect_token_clients(&self, token_id: &str) -> usize {
    let mut dropped = 0usize;
    {
      let clients = self.clients.read().await;
      for handle in clients.values() {
        if handle.perms.token_id.as_deref() == Some(token_id) {
          handle.disconnect.notify_one();
          dropped += 1;
        }
      }
    }
    // The token is being revoked/deleted; drop its source-IP tracking so the
    // in-memory `token_seen_ips` map does not accumulate entries for tokens
    // that no longer exist.
    self.token_seen_ips.lock().await.remove(token_id);
    dropped
  }

  /// Applies a changed organization hostname allowlist to that org's live
  /// tunnel connections: each one's cached copy is refreshed, and any
  /// connection now serving a hostname outside the fence is dropped. Returns
  /// how many were dropped.
  ///
  /// The allowlist is copied into `ClientPerms` at connect time so that later
  /// bind checks stay a pure in-memory comparison. Without this, tightening
  /// the fence only took effect the next time each client happened to
  /// reconnect, while the endpoint's own documentation promised it applied at
  /// once, so a hostname an operator had just revoked kept being served,
  /// potentially for as long as the client stayed up.
  pub(crate) async fn apply_org_hostnames(&self, org_id: &str, hostnames: &[String]) -> usize {
    let mut dropped = 0usize;
    let mut clients = self.clients.write().await;
    for handle in clients.values_mut() {
      if handle.perms.org_id.as_deref() != Some(org_id) {
        continue;
      }
      handle.perms.org_hostnames = hostnames.to_vec();
      // Every service on the connection, not the first one's names. The fence
      // is a tenant boundary and the question it asks is "is this connection
      // serving anything outside the allowlist", so a name reached the first
      // way it can be reached is a name that has to be checked: a multiplexed
      // connection whose second service held the revoked hostname would
      // otherwise pass the check and keep serving it. Same reasoning, and the
      // same hole, as `effective_hostnames`.
      let serving: Vec<&String> = handle
        .services
        .iter()
        .flat_map(|s| {
          s.assigned_hostnames
            .iter()
            .chain(s.declared_hostnames.iter())
        })
        .collect();
      if serving
        .iter()
        .any(|h| !crate::store::orgs::hostname_in_org_allowlist(h, hostnames))
      {
        handle.disconnect.notify_one();
        dropped += 1;
      }
    }
    dropped
  }

  /// Applies a changed `topics` grant to that token's live tunnel
  /// connections: each one's cached copy is refreshed, and every subscription
  /// the new grant no longer covers is withdrawn and reported to the client.
  /// Returns how many subscriptions were withdrawn.
  ///
  /// A subscription is the one messaging capability the server goes on
  /// holding *for* a client between requests. A bind is checked when it is
  /// declared and a publish when it is made, so narrowing either is felt at
  /// the next attempt; a subscription, once admitted, keeps delivering on its
  /// own. `ClientPerms` is a snapshot taken at connect, so without this,
  /// taking a topic away only took effect the next time the client happened
  /// to reconnect, and messages an operator had just revoked kept arriving
  /// for as long as the process stayed up, which for a tunnel client is
  /// measured in weeks.
  ///
  /// The connection is not dropped, unlike a revoked token or a hostname that
  /// left its organization's fence. Nothing about it is wrong: it is serving
  /// its routes under a grant that is still valid, and only one thing
  /// changed. That one thing is reported as a `SubscribeRefused`, the frame
  /// the client already logs by name for a filter it never got, so an
  /// operator reading the client's output sees the withdrawal rather than
  /// wondering why a topic went quiet.
  pub(crate) async fn apply_token_topics(&self, token_id: &str, topics: &[String]) -> usize {
    type Withdrawal = (mpsc::Sender<axum::extract::ws::Message>, Vec<String>);
    let withdrawn: Vec<Withdrawal> = {
      let mut clients = self.clients.write().await;
      let mut out = Vec::new();
      for handle in clients.values_mut() {
        if handle.perms.token_id.as_deref() != Some(token_id) {
          continue;
        }
        handle.perms.topics = topics.to_vec();
        let held = std::mem::take(&mut handle.subscriptions);
        let (kept, gone): (Vec<String>, Vec<String>) = held
          .into_iter()
          .partition(|filter| crate::tunnel::pubsub::may_use_topic(&handle.perms, filter));
        handle.subscriptions = kept;
        if !gone.is_empty() {
          out.push((handle.tx.clone(), gone));
        }
      }
      out
    };

    // Told outside the lock, as every other fan-out here is: the client map is
    // on the path of every request, and a write lock is not the place to be
    // walking channels.
    let mut count = 0usize;
    for (tx, filters) in withdrawn {
      for filter in filters {
        count += 1;
        let frame = crate::protocol::TunnelMessage::SubscribeRefused {
          topic: filter,
          reason: "the token's topics no longer cover this filter".to_string(),
        };
        if let Ok(text) = serde_json::to_string(&frame) {
          let _ = tx.try_send(axum::extract::ws::Message::Text(text.into()));
        }
      }
    }
    count
  }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
