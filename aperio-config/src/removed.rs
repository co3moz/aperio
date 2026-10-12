//! Keys a configuration file may still carry from an earlier release, and
//! what to write instead.
//!
//! There is no `version:` in a configuration and no layer that translates an
//! old spelling into a new one. A file that writes a key from this table gets
//! the hint beside it, naming what replaced the key, instead of the generic
//! "not a setting" a typo gets: the message an operator needs after an
//! upgrade is the one that says where the setting went.
//!
//! Add an entry in the same commit that removes or renames a key. A key that
//! is merely new needs nothing here.

/// One removed key or variable and what to do about it.
pub struct Removed {
  /// The variable as the environment set it.
  pub key: &'static str,
  /// What replaced it, written as the thing to do.
  pub hint: &'static str,
}

/// Where a removed client key was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyScope {
  /// The top level of the file (a dotted path for a key inside a block).
  Top,
  /// A `services:` entry or `defaults:` (a dotted path for a key inside one
  /// of their blocks).
  Service,
}

/// A removed client key and where it was written.
pub struct RemovedKey {
  /// Where it was written.
  pub scope: KeyScope,
  /// The key, dotted when it is inside a block.
  pub key: &'static str,
  /// What replaced it, written as the thing to do.
  pub hint: &'static str,
}

const MOVED_TO_DEFAULTS: &str = "is a service setting; since 0.13.0 a value every service shares is written under `defaults:`, and one service's under its `services:` entry.";
const SINGLE_SERVICE: &str = "describes a single service, which a file has written as a `services:` entry since 0.9.0, keyed by name since 0.13.0 (`services: {web: {target: ..., hostname: ...}}`). The command line and the environment still take one service.";
const ACCESS: &str = "was replaced by `access:` in 0.13.0: `access: public`, a method block such as `{method: basic, users: \"admin:${PASSWORD}\"}`, or a list of them.";
const ACCESS_TOP: &str = "was replaced by `access:` in 0.13.0, written under `defaults:` for every service: `access: public`, or a method block such as `{method: basic, users: \"admin:${PASSWORD}\"}`.";
const BINDS: &str =
  "became `binds:` in 0.13.0, keyed by tunnel name only (`binds: {pg_main: 15432}`).";

/// Client keys (`aperio.yaml`) that no longer exist, and what replaced them.
pub const REMOVED_CLIENT_KEYS: &[RemovedKey] = &[
  RemovedKey {
    scope: KeyScope::Top,
    key: "version",
    hint: "was removed in 0.13.0: a file no longer declares the release it was written for, since a client pairs only with a server of its own minor release. Delete the key.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "token",
    hint: "moved to `server.token` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "target",
    hint: SINGLE_SERVICE,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "serve",
    hint: SINGLE_SERVICE,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "tcp_target",
    hint: SINGLE_SERVICE,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "hostname",
    hint: SINGLE_SERVICE,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "path",
    hint: SINGLE_SERVICE,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "custom_name",
    hint: SINGLE_SERVICE,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "target_health",
    hint: SINGLE_SERVICE,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "public",
    hint: ACCESS_TOP,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "auth",
    hint: ACCESS_TOP,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "capture",
    hint: "was renamed `inspector` in 0.13.0, written under `defaults:` for every service.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "resilience",
    hint: "became `cache.offline` in 0.13.0, written under `defaults:` for every service.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "max_concurrent",
    hint: "became `limits.concurrent_requests` in 0.13.0, written under `defaults:` for every service.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "adaptive_concurrency",
    hint: "became `limits.adaptive_concurrency` in 0.13.0, written under `defaults:` for every service.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "max_request_body",
    hint: "became `limits.request_body` in 0.13.0, written under `defaults:` for every service.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "max_response_body",
    hint: "became `limits.response_body` in 0.13.0, written under `defaults:` for every service.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "max_redirects",
    hint: "became `limits.redirects` in 0.13.0, written under `defaults:` for every service.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "wait_for_backend",
    hint: "became `health.wait_for_backend` in 0.13.0, written under `defaults:` for every service.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "health_interval",
    hint: "became `health.interval` in 0.13.0, written under `defaults:` for every service.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "health_timeout",
    hint: "became `health.timeout` in 0.13.0, written under `defaults:` for every service.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "health_threshold",
    hint: "became `health.threshold` in 0.13.0, written under `defaults:` for every service.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "depends_on",
    hint: "is written on the `services:` entry that waits since 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "timeout",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "connect_timeout",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "response_timeout",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "startup_delay",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "retry",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "circuit_breaker",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "health",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "headers",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "security_headers",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "allowed_ips",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "denied",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "cache",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "webhook_inbox",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "connections",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "multiplex",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "metrics_labels",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "pass_hostname",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "trim_bind",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "priority",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "server_side",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "min_tls_version",
    hint: MOVED_TO_DEFAULTS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "ip_family",
    hint: "moved to `server.ip_family` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "egress_proxy",
    hint: "moved to `server.proxy` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "tls_min_version",
    hint: "moved to `server.min_tls_version` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "tls_cipher_suites",
    hint: "moved to `server.tls_cipher_suites` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "messages_listen",
    hint: "moved to `messages.listen` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "messages_mqtt_listen",
    hint: "moved to `messages.mqtt_listen` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "bind-tunnels",
    hint: BINDS,
  },
  RemovedKey {
    scope: KeyScope::Top,
    key: "bind_tunnels",
    hint: BINDS,
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "name",
    hint: "was removed in 0.13.0: a service's name is the key it is written under (`services: {web: {...}}`).",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "public",
    hint: ACCESS,
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "auth",
    hint: ACCESS,
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "capture",
    hint: "was renamed `inspector` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "resilience",
    hint: "became `cache.offline` in 0.13.0 (`cache: {enabled: true, offline: true}`).",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "target_health",
    hint: "became `health.endpoint` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "wait_for_backend",
    hint: "became `health.wait_for_backend` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "health_interval",
    hint: "became `health.interval` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "health_timeout",
    hint: "became `health.timeout` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "health_threshold",
    hint: "became `health.threshold` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "max_concurrent",
    hint: "became `limits.concurrent_requests` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "adaptive_concurrency",
    hint: "became `limits.adaptive_concurrency` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "max_request_body",
    hint: "became `limits.request_body` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "max_response_body",
    hint: "became `limits.response_body` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "max_redirects",
    hint: "became `limits.redirects` in 0.13.0.",
  },
  RemovedKey {
    scope: KeyScope::Service,
    key: "retry.backoff",
    hint: "became `retry.delay` in 0.13.0, with a unit: it was milliseconds, so `backoff: 100` is `delay: 100ms`.",
  },
];

/// The hint for a removed client key, if `key` is one in `scope`.
pub fn client_hint(scope: KeyScope, key: &str) -> Option<&'static str> {
  REMOVED_CLIENT_KEYS
    .iter()
    .find(|r| r.scope == scope && r.key == key)
    .map(|r| r.hint)
}

/// A key that still exists and changed shape: the old shape gets a sentence
/// saying what the new one is, rather than a type error.
pub fn shape_hint(key: &str, value: &serde_yaml::Value) -> Option<&'static str> {
  match (key, value) {
    ("server", serde_yaml::Value::String(_)) => {
      Some("is a block since 0.13.0: `server: {url: https://..., token: ...}`.")
    }
    ("cache", serde_yaml::Value::Bool(_)) => Some(
      "is a block since 0.13.0: `cache: {enabled: true}`, and what `resilience` did is `cache.offline`.",
    ),
    ("tunnels" | "services", serde_yaml::Value::Sequence(_)) => Some(
      "is a map keyed by name since 0.13.0, not a list: write `<name>: {target: ...}` for each entry.",
    ),
    _ => None,
  }
}

/// Client environment variables that no longer exist, and what replaced
/// them. The client refuses to start while one is set: a variable that is
/// silently ignored is a setting an operator believes is in force.
pub const REMOVED_CLIENT_ENV: &[Removed] = &[
  Removed {
    key: "APERIO_PUBLIC",
    hint: "was replaced by APERIO_ACCESS=public in 0.13.0.",
  },
  Removed {
    key: "APERIO_VISITOR_AUTH",
    hint: "was replaced by APERIO_ACCESS in 0.13.0, which takes `public` or a method block as yaml: APERIO_ACCESS='{method: basic, users: \"user:password\"}'.",
  },
  Removed {
    key: "APERIO_TARGET_HEALTH",
    hint: "was renamed APERIO_HEALTH_ENDPOINT in 0.13.0.",
  },
  Removed {
    key: "APERIO_WAIT_FOR_BACKEND",
    hint: "was renamed APERIO_HEALTH_WAIT_FOR_BACKEND in 0.13.0.",
  },
  Removed {
    key: "APERIO_CAPTURE",
    hint: "was renamed APERIO_INSPECTOR in 0.13.0.",
  },
  Removed {
    key: "APERIO_RESILIENCE",
    hint: "was renamed APERIO_CACHE_OFFLINE in 0.13.0.",
  },
  Removed {
    key: "APERIO_CACHE",
    hint: "was renamed APERIO_CACHE_ENABLED in 0.13.0.",
  },
  Removed {
    key: "APERIO_API_KEY",
    hint: "was renamed APERIO_SERVER_API_KEY in 0.13.0.",
  },
  Removed {
    key: "APERIO_EGRESS_PROXY",
    hint: "was renamed APERIO_SERVER_PROXY in 0.13.0.",
  },
  Removed {
    key: "APERIO_IP_FAMILY",
    hint: "was renamed APERIO_SERVER_IP_FAMILY in 0.13.0.",
  },
  Removed {
    key: "APERIO_TLS_MIN_VERSION",
    hint: "was renamed APERIO_SERVER_MIN_TLS_VERSION in 0.13.0.",
  },
  Removed {
    key: "APERIO_TLS_CIPHER_SUITES",
    hint: "was renamed APERIO_SERVER_TLS_CIPHER_SUITES in 0.13.0.",
  },
  Removed {
    key: "APERIO_MAX_CONCURRENT",
    hint: "was renamed APERIO_LIMITS_CONCURRENT_REQUESTS in 0.13.0.",
  },
  Removed {
    key: "APERIO_ADAPTIVE_CONCURRENCY",
    hint: "was renamed APERIO_LIMITS_ADAPTIVE_CONCURRENCY in 0.13.0.",
  },
  Removed {
    key: "APERIO_MAX_REQUEST_BODY",
    hint: "was renamed APERIO_LIMITS_REQUEST_BODY in 0.13.0.",
  },
  Removed {
    key: "APERIO_MAX_RESPONSE_BODY",
    hint: "was renamed APERIO_LIMITS_RESPONSE_BODY in 0.13.0.",
  },
  Removed {
    key: "APERIO_MAX_REDIRECTS",
    hint: "was renamed APERIO_LIMITS_REDIRECTS in 0.13.0.",
  },
  Removed {
    key: "APERIO_RETRY_BACKOFF",
    hint: "was replaced by APERIO_RETRY_DELAY in 0.13.0, with a unit: it was milliseconds, so 100 is 100ms.",
  },
  Removed {
    key: "APERIO_BREAKER_FAILURES",
    hint: "was renamed APERIO_CIRCUIT_BREAKER_FAILURES in 0.13.0.",
  },
  Removed {
    key: "APERIO_BREAKER_OPEN_FOR",
    hint: "was renamed APERIO_CIRCUIT_BREAKER_OPEN_FOR in 0.13.0.",
  },
  Removed {
    key: "APERIO_DEPENDS_ON",
    hint: "was removed in 0.13.0: a single service has nothing to wait for; write `depends_on:` on a `services:` entry.",
  },
];

/// Server environment variables from before the settings moved into the
/// database, and where each one went. The server refuses to start while one
/// is set, naming the setting to use instead: a variable that is silently
/// ignored is a fence an operator believes is up.
pub const REMOVED_SERVER_ENV: &[Removed] = &[
  Removed {
    key: "APERIO_GATEWAY_TIMEOUT",
    hint: "moved into the settings as `gateway.timeout` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_GATEWAY_RESPONSE_TIMEOUT",
    hint: "moved into the settings as `gateway.response_timeout` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_CLIENT_DOWN_THRESHOLD",
    hint: "moved into the settings as `gateway.client_down_threshold` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_MAX_BODY_SIZE",
    hint: "moved into the settings as `limits.request_body` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_MAX_CONCURRENT_REQUESTS",
    hint: "moved into the settings as `limits.concurrent_requests` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_MAX_WS_CONNECTIONS",
    hint: "moved into the settings as `limits.websockets` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_MAX_TUNNELS",
    hint: "moved into the settings as `limits.tunnels` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_MAX_CONNECTIONS_PER_SERVICE",
    hint: "moved into the settings as `limits.connections_per_service` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_MAX_STREAMS_PER_IP",
    hint: "moved into the settings as `limits.streams_per_ip` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_IP_LIMIT_MAX",
    hint: "moved into the settings as `limits.ip_rate.burst` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_IP_LIMIT_REFILL",
    hint: "moved into the settings as `limits.ip_rate.per_second` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_LB_STRATEGY",
    hint: "moved into the settings as `routing.lb_strategy` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_REQUIRE_HOSTNAME_BIND",
    hint: "moved into the settings as `routing.require_hostname_bind` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_RANDOM_SUBDOMAIN",
    hint: "moved into the settings as `routing.random_subdomain` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_PREVIEW_NOINDEX",
    hint: "moved into the settings as `routing.preview_noindex` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_ALTERNATE_SERVERS",
    hint: "moved into the settings as `routing.alternate_servers` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_FAILOVER",
    hint: "moved into the settings as `failover.mode` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_FAILOVER_MAX_JUMPS",
    hint: "moved into the settings as `failover.max_jumps` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_FAILOVER_WINDOW",
    hint: "moved into the settings as `failover.window` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_FAILOVER_ALL_METHODS",
    hint: "moved into the settings as `failover.all_methods` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_RETRY_ON_5XX",
    hint: "moved into the settings as `failover.retry_on_5xx` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_RETRY_STATUSES",
    hint: "moved into the settings as `failover.retry_statuses` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OUTLIER_EJECTION",
    hint: "moved into the settings as `outlier.enabled` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OUTLIER_MAX_FAILURES",
    hint: "moved into the settings as `outlier.max_failures` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OUTLIER_WINDOW",
    hint: "moved into the settings as `outlier.window` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OUTLIER_EJECT_SECS",
    hint: "moved into the settings as `outlier.eject_for` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_DEFAULT_ACCESS",
    hint: "moved into the settings as `access.default` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_SERVER_AUTH",
    hint: "moved into the settings as `access.gate` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_IGNORE_CLIENT_AUTH",
    hint: "moved into the settings as `access.ignore_client_auth` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_TRUST_PROXY",
    hint: "moved into the settings as `proxy.trust` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_TRUSTED_PROXIES",
    hint: "moved into the settings as `proxy.trusted` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_REAL_IP_HEADER",
    hint: "moved into the settings as `proxy.real_ip_header` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_TRUST_CF_HEADER",
    hint: "moved into the settings as `proxy.real_ip_header` (write CF-Connecting-IP) in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_ADMIN_ALLOWED_IPS",
    hint: "moved into the settings as `security.admin_allowed_ips` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_DENIED_IPS",
    hint: "moved into the settings as `security.denied_ips` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_SECURE_COOKIES",
    hint: "moved into the settings as `security.secure_cookies` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_TOKEN_PINNING",
    hint: "moved into the settings as `security.token_pinning` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_TOKEN_EXPIRY_WARNING",
    hint: "moved into the settings as `security.token_expiry_warning` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_SERVER_SIDE_TARGETS",
    hint: "moved into the settings as `security.server_side_targets` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_LOGIN_LOCKOUT_THRESHOLD",
    hint: "moved into the settings as `security.login_lockout.threshold` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_LOGIN_LOCKOUT_SECS",
    hint: "moved into the settings as `security.login_lockout.duration` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_JWKS_ALLOW_HTTP",
    hint: "moved into the settings as `security.jwks.allow_http` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_JWKS_ALLOW_PRIVATE",
    hint: "moved into the settings as `security.jwks.allow_private` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OUTBOUND_ALLOWLIST",
    hint: "moved into the settings as `outbound.allowlist` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OUTBOUND_BLOCK_PRIVATE",
    hint: "moved into the settings as `outbound.block_private` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OUTBOUND_PROXY",
    hint: "moved into the settings as `outbound.proxy` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OUTBOUND_NO_PROXY",
    hint: "moved into the settings as `outbound.no_proxy` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_DASHBOARD",
    hint: "moved into the settings as `dashboard.enabled` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_DASHBOARD_HOSTNAME",
    hint: "moved into the settings as `dashboard.hostname` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_DASHBOARD_FENCED_LOGIN",
    hint: "moved into the settings as `dashboard.fenced_login` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_UI_LANGUAGE",
    hint: "moved into the settings as `dashboard.ui_language` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_WEBAUTHN_ORIGIN",
    hint: "moved into the settings as `dashboard.webauthn.origin` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_WEBAUTHN_RP_ID",
    hint: "moved into the settings as `dashboard.webauthn.rp_id` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OIDC_ISSUER",
    hint: "moved into the settings as `oidc.issuer` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OIDC_CLIENT_ID",
    hint: "moved into the settings as `oidc.client_id` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OIDC_CLIENT_SECRET",
    hint: "moved into the settings as `oidc.client_secret` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OIDC_ALLOWED_EMAILS",
    hint: "moved into the settings as `oidc.allowed_emails` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OIDC_SCOPES",
    hint: "moved into the settings as `oidc.scopes` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OIDC_REDIRECT_URL",
    hint: "moved into the settings as `oidc.redirect_url` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OIDC_DEFAULT_GRANTS",
    hint: "moved into the settings as `oidc.default_grants` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OIDC_GROUPS_CLAIM",
    hint: "moved into the settings as `oidc.groups_claim` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OIDC_GROUP_GRANTS",
    hint: "moved into the settings as `oidc.group_grants` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_CACHE",
    hint: "moved into the settings as `cache.enabled` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_CACHE_MAX_BYTES",
    hint: "moved into the settings as `cache.size` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_CACHE_MAX_STALE",
    hint: "moved into the settings as `cache.max_stale` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_CACHE_NEGATIVE_TTL",
    hint: "moved into the settings as `cache.negative_ttl` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_STREAM_PAUSE_BYTES",
    hint: "moved into the settings as `stream.pause_at` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_STREAM_RESUME_BYTES",
    hint: "moved into the settings as `stream.resume_at` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_STREAM_BACKLOG_LIMIT",
    hint: "moved into the settings as `stream.backlog_limit` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_STREAM_MIN_THROUGHPUT",
    hint: "moved into the settings as `stream.min_throughput` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_TUNNEL_COMPRESSION",
    hint: "moved into the settings as `tunnel.compression` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_INSPECTOR",
    hint: "moved into the settings as `inspector.enabled` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_INSPECTOR_REDACT",
    hint: "moved into the settings as `inspector.redact` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_ACCESS_EVENTS",
    hint: "moved into the settings as `logging.access_events` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_ACCESS_LOG",
    hint: "moved into the settings as `logging.access_log` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_ACCESS_LOG_SAMPLE_RATE",
    hint: "moved into the settings as `logging.sample_rate` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_REQUEST_ID",
    hint: "moved into the settings as `request_id.enabled` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_REQUEST_ID_HEADER",
    hint: "moved into the settings as `request_id.header` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_REQUEST_ID_TRUST_INBOUND",
    hint: "moved into the settings as `request_id.trust_inbound` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_IDENTITY_HEADERS",
    hint: "moved into the settings as `identity_headers.serving` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_VISITOR_IDENTITY_HEADERS",
    hint: "moved into the settings as `identity_headers.visitor` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_METRICS",
    hint: "moved into the settings as `metrics.enabled` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_METRICS_TOKEN",
    hint: "moved into the settings as `metrics.token` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OTEL",
    hint: "moved into the settings as `otel.enabled` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OTEL_ENDPOINT",
    hint: "moved into the settings as `otel.endpoint` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OTEL_PROTOCOL",
    hint: "moved into the settings as `otel.protocol` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OTEL_SERVICE_NAME",
    hint: "moved into the settings as `otel.service_name` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OTEL_SAMPLE_RATE",
    hint: "moved into the settings as `otel.sample_rate` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OTEL_HEADERS",
    hint: "moved into the settings as `otel.headers` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_OTEL_BRIDGE",
    hint: "moved into the settings as `otel.bridge` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_AUDIT_MAX_SIZE",
    hint: "moved into the settings as `audit.max_size` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_AUDIT_MAX_FILES",
    hint: "moved into the settings as `audit.max_files` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_RETENTION_CAPTURES",
    hint: "moved into the settings as `retention.captures` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_RETENTION_ACCESS_LOG",
    hint: "moved into the settings as `retention.access_log` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_RETENTION_AUDIT",
    hint: "moved into the settings as `retention.audit` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_RETENTION_STATS",
    hint: "moved into the settings as `retention.stats` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_DB_MAX_BYTES",
    hint: "moved into the settings as `retention.db_max_size` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_BACKUP_INTERVAL",
    hint: "moved into the settings as `backup.interval` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_BACKUP_DIR",
    hint: "moved into the settings as `backup.dir` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_BACKUP_KEEP",
    hint: "moved into the settings as `backup.keep` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_ALERT_ERROR_RATE",
    hint: "moved into the settings as `alerts.error_rate` (a fraction now: 5 is 0.05) in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_ALERT_WINDOW",
    hint: "moved into the settings as `alerts.window` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_ALERT_MIN_REQUESTS",
    hint: "moved into the settings as `alerts.min_requests` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_ALERT_CLIENT_DOWN",
    hint: "moved into the settings as `alerts.client_down` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_WEBHOOK_RETRY_SCHEDULE",
    hint: "moved into the settings as `webhooks.retry_schedule` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_SCALING",
    hint: "moved into the settings as `scaling.enabled` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_SCALING_ALLOW_HTTP",
    hint: "moved into the settings as `scaling.allow_http` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_SCALING_ALLOW_PRIVATE",
    hint: "moved into the settings as `scaling.allow_private` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_SCALING_RECORD_TTL",
    hint: "moved into the settings as `scaling.record_ttl` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_EDGE_TOKEN",
    hint: "moved into the settings as `edge.token` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_EDGE_SERVICE_URL",
    hint: "moved into the settings as `edge.service_url` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_EDGE_ENTRYPOINTS",
    hint: "moved into the settings as `edge.entrypoints` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_EDGE_CERT_RESOLVER",
    hint: "moved into the settings as `edge.cert_resolver` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_EDGE_INCLUDE_OFFLINE",
    hint: "moved into the settings as `edge.include_offline` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_SHUTDOWN_DRAIN",
    hint: "moved into the settings as `shutdown.drain` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_SHUTDOWN_TIMEOUT",
    hint: "moved into the settings as `shutdown.timeout` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_UPTIME_TICK_SECS",
    hint: "moved into the settings as `uptime.tick` in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_504_PAGE",
    hint: "moved into the settings as `pages.unavailable` (the HTML itself) in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_503_PAGE",
    hint: "moved into the settings as `pages.maintenance` (the HTML itself) in 0.13.0. Set it from the dashboard (Settings), with `PUT /aperio/api/settings`, or in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_SERVER_CONFIG",
    hint: "removed in 0.13.0: aperio-server.yaml is gone: settings live in the database. Apply a document once with `aperio-server apply <file>`, or name it in APERIO_INIT_FILE for a first start.",
  },
  Removed {
    key: "APERIO_CONFIG_HOT_RELOAD",
    hint: "removed in 0.13.0: there is no configuration file to watch; a settings change applies when it is made.",
  },
  Removed {
    key: "APERIO_DASHBOARD_AUTH",
    hint: "removed in 0.6.0: the separate dashboard password was removed; sign in as `aperio` with the master token, or create a dashboard user.",
  },
];

/// The hint for a removed server environment variable, if `name` is one.
pub fn server_env_hint(name: &str) -> Option<&'static str> {
  REMOVED_SERVER_ENV
    .iter()
    .find(|r| r.key == name)
    .map(|r| r.hint)
}

/// Sections of the server's settings that moved, and where to. A document
/// written for the old shape is refused naming the key; this is what says
/// where it went, so the refusal is the fix rather than a search.
pub const REMOVED_SETTINGS: &[Removed] = &[
  Removed {
    key: "routes",
    hint: "moved into `sites` in 0.13.0: an entry with a `hostname` (`*` for every one) and an optional `path`, with `redirect: {url: ...}` or `respond:` as its answer, or `timeout`, `headers`, `rate_limit`, `canary` as its policy.",
  },
  Removed {
    key: "rate_limits",
    hint: "moved into `sites` in 0.13.0: a site's `rate_limit: {rps, burst, methods}`.",
  },
  Removed {
    key: "waf",
    hint: "moved into `sites` in 0.13.0: a site's `waf:` list, on `hostname: \"*\"` for every hostname.",
  },
  Removed {
    key: "fallbacks",
    hint: "moved into `sites` in 0.13.0: a site's `fallback: {url, permanent, preserve_path}`.",
  },
  Removed {
    key: "error_pages",
    hint: "moved into `sites` in 0.13.0: a site's `pages: {unavailable, maintenance}`, the HTML itself rather than a file path.",
  },
  Removed {
    key: "maintenance_windows",
    hint: "moved into `sites` in 0.13.0: a site's `maintenance:` list, without the `hostname`, which the site gives.",
  },
  Removed {
    key: "headers",
    hint: "moved into `sites` in 0.13.0: the `headers` of the `hostname: \"*\"` site.",
  },
  Removed {
    key: "pages",
    hint: "moved into `sites` in 0.13.0: the `pages` of the `hostname: \"*\"` site.",
  },
];

/// `error`, a refusal of a settings document, with where the key it names
/// went when it is one of [`REMOVED_SETTINGS`].
pub fn with_settings_hint(error: String) -> String {
  let Some(rest) = error.split("unknown field `").nth(1) else {
    return error;
  };
  let key = rest.split('`').next().unwrap_or_default();
  match REMOVED_SETTINGS.iter().find(|r| r.key == key) {
    Some(r) => format!("{error}. `{key}` {}", r.hint),
    None => error,
  }
}

#[cfg(test)]
#[path = "removed_tests.rs"]
mod tests;
