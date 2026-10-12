//! The server's settings: one typed document, kept in the server's database.
//!
//! The server is configured at runtime, from the dashboard or the API, and
//! this is the shape that is stored, validated, exported and applied. Only
//! what is needed before the database can be opened stays outside it, in the
//! environment: the master token, the listen address, the data directory, the
//! log level, the backup key and `reuseport` (see [`BOOT_ENV`]).
//!
//! Every section and every field is optional in a document: whatever is left
//! out keeps its default, so a document only has to say what it changes, and
//! [`ServerSettings::minimal`] writes one back that way. Every struct refuses
//! unknown keys, so a typo is an error naming the key rather than a setting
//! that silently does nothing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Site;

/// The environment variables the server still reads, all of them needed
/// before its database exists. Everything else is a [`ServerSettings`] field.
pub const BOOT_ENV: &[(&str, &str)] = &[
  (
    "APERIO_SERVER_TOKEN",
    "The master token: authenticates tunnel clients, and signs in as the built-in `aperio` admin. Required.",
  ),
  ("HOST", "Address to listen on. Default `0.0.0.0`."),
  ("PORT", "Port to listen on. Default `8080`."),
  (
    "APERIO_DATA_DIR",
    "Directory holding the database, the audit log and the backups' staging. Default `./data`.",
  ),
  (
    "LOG_LEVEL",
    "Log verbosity: error, warn, info, debug or trace. Default `info`.",
  ),
  (
    "APERIO_REUSEPORT",
    "`1` binds the listener with SO_REUSEPORT, so a new process can take over the port for a zero-downtime restart.",
  ),
  (
    "APERIO_BACKUP_KEY",
    "Encryption key for backup snapshots (64 hex characters or base64 of 32 bytes). Outside the database because it protects the database's copies.",
  ),
  (
    "APERIO_BACKUP_KEY_FILE",
    "File holding that key, which is what a secret manager mounts.",
  ),
  (
    "APERIO_INIT_FILE",
    "A settings document (yaml or json) applied to an empty database on the first start, the way a container is set up without a click.",
  ),
];

/// Everything the server is configured with at runtime.
#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSettings {
  /// Ceilings that protect the server: body size, concurrency, tunnels.
  pub limits: Limits,
  /// How long the server waits on a client.
  pub gateway: Gateway,
  /// How a request picks among the clients serving its route.
  pub routing: Routing,
  /// What happens to a request whose client fails or disappears mid-flight.
  pub failover: Failover,
  /// Taking a client out of routing when it keeps failing, while its health
  /// probe still reports green.
  pub outlier: Outlier,
  /// Who may reach a proxied route when nothing more specific says.
  pub access: Access,
  /// The reverse proxy or CDN in front of this server, and whose
  /// `X-Forwarded-For` to believe. Security-relevant.
  pub proxy: Proxy,
  /// Fences around the server itself. Security-relevant.
  pub security: Security,
  /// Where the server may send callbacks it makes on its own initiative
  /// (webhooks, autoscaling hooks, JWKS, OIDC, OTLP). Security-relevant.
  pub outbound: Outbound,
  /// The built-in dashboard and its sign-in.
  pub dashboard: Dashboard,
  /// Single sign-on for the dashboard through an OpenID Connect provider.
  pub oidc: Oidc,
  /// The server-side GET response cache for services that opt in.
  pub cache: Cache,
  /// Per-stream flow control for streamed responses, WebSockets and TCP.
  pub stream: Stream,
  /// The tunnel connections themselves.
  pub tunnel: Tunnel,
  /// The dashboard's request inspector.
  pub inspector: Inspector,
  /// The per-request access log.
  pub logging: Logging,
  /// The request id sent to the backend and echoed to the visitor.
  pub request_id: RequestId,
  /// Headers telling the backend who served and who visited.
  pub identity_headers: IdentityHeaders,
  /// The Prometheus endpoint at `/aperio/metrics`.
  pub metrics: Metrics,
  /// OpenTelemetry trace export, and the bridge for clients' exports.
  pub otel: Otel,
  /// Rotation of the tamper-evident audit log.
  pub audit: Audit,
  /// How long recorded data is kept.
  pub retention: Retention,
  /// Scheduled snapshots of the database.
  pub backup: Backup,
  /// Built-in and operator-defined alerts.
  pub alerts: Alerts,
  /// Webhook delivery.
  pub webhooks: Webhooks,
  /// Autoscaling hooks clients may declare.
  pub scaling: Scaling,
  /// Publishing the served hostnames to a dynamic edge proxy.
  pub edge: Edge,
  /// What a shutdown waits for.
  pub shutdown: Shutdown,
  /// The availability history behind the dashboard's Uptime panel.
  pub uptime: Uptime,
  /// What the server does for a hostname, or a path under one, by itself:
  /// answers without a client, policy on proxied traffic, and what a
  /// hostname says when nothing serves it. Each setting comes from the most
  /// specific site that writes it; `hostname: "*"` is every hostname.
  #[schemars(extend("examples" = [[
    {"hostname": "*", "headers": {"response": {"remove": ["x-powered-by"]}}, "pages": {"unavailable": "<h1>Nothing is serving this right now</h1>"}},
    {"hostname": "old.example.com", "redirect": {"url": "https://new.example.com", "permanent": true}},
    {"hostname": "app.example.com", "path": "/login", "rate_limit": {"rps": 5, "burst": 10}},
    {"hostname": "app.example.com", "fallback": {"url": "https://status.example.com"}, "maintenance": [{"from": "02:00", "to": "04:00", "days": ["sun"]}]}
  ]]))]
  pub sites: Vec<Site>,
}

#[path = "server_settings/sections.rs"]
mod sections;
pub use sections::*;

// --- Which settings are special --------------------------------------------

/// Settings whose change needs `*` Admin, a recent sign-in, and is audited
/// on its own: each one decides who reaches what, or where the server may be
/// steered. A path names a field or a whole section.
pub const SECURITY_PATHS: &[&str] = &[
  "access.default",
  "access.gate",
  "access.ignore_client_auth",
  "routing.require_hostname_bind",
  "proxy",
  "security.admin_allowed_ips",
  "security.secure_cookies",
  "security.token_pinning",
  "security.login_lockout",
  "security.server_side_targets",
  "security.jwks",
  "outbound",
  "oidc",
  "dashboard.hostname",
  "dashboard.fenced_login",
  "dashboard.webauthn",
  "inspector.redact",
  "otel.endpoint",
  "otel.headers",
  "metrics.token",
  "edge.token",
  "edge.include_offline",
  "scaling.allow_http",
  "scaling.allow_private",
];

/// Settings read once, when the server starts: a change is stored at once
/// and takes effect on the next start.
pub const RESTART_PATHS: &[&str] = &[
  "dashboard.enabled",
  "dashboard.webauthn",
  "oidc",
  "metrics.enabled",
  "otel",
  "logging.access_log",
  "outbound.proxy",
  "outbound.no_proxy",
  "edge.token",
  "backup",
];

/// The sections a settings document (an export, `APERIO_INIT_FILE`, or
/// `aperio-server apply`) may carry besides `settings`. The server's import
/// reads exactly these; its tests hold the two together.
pub const DOCUMENT_SECTIONS: &[&str] = &[
  "format_version",
  "tokens",
  "webhooks",
  "users",
  "organizations",
  "scaling",
  "exposes",
  "statistics",
  "uptime",
  "activity",
  "inbox",
  "admin_keys",
];

/// Settings that hold a credential. Reading the settings over the API shows
/// [`MASKED`] in their place, and writing [`MASKED`] back keeps the value
/// already stored, so the dashboard can show and save the whole document
/// without ever holding a secret.
pub const SECRET_PATHS: &[&str] = &[
  "oidc.client_secret",
  "metrics.token",
  "edge.token",
  "otel.headers",
  "access.gate",
];

/// Inside a visitor gate, the fields that carry a credential. The rest of a
/// gate, its method, issuer or URL, stays readable.
const GATE_SECRET_FIELDS: &[&str] = &["users", "secret", "hmac_secret"];

/// What a stored credential reads as.
pub const MASKED: &str = "********";

impl ServerSettings {
  /// Parses a settings document, yaml or json.
  pub fn from_yaml(raw: &str) -> Result<ServerSettings, String> {
    serde_yaml::from_str::<Option<ServerSettings>>(raw)
      .map(Option::unwrap_or_default)
      .map_err(|e| crate::removed::with_settings_hint(e.to_string()))
  }

  /// The document as a tree, every field present.
  pub fn to_value(&self) -> serde_json::Value {
    serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
  }

  /// The document with every value equal to its default left out, which is
  /// what an export writes and what a person wants to read: only what this
  /// server changes.
  pub fn minimal(&self) -> serde_json::Value {
    let mut value = self.to_value();
    prune_defaults(&mut value, &ServerSettings::default().to_value());
    match value {
      serde_json::Value::Null => serde_json::json!({}),
      v => v,
    }
  }

  /// The document as a tree with every credential ([`SECRET_PATHS`])
  /// replaced by [`MASKED`]. An unset one stays unset, so a reader can still
  /// tell the two apart.
  pub fn masked(&self) -> serde_json::Value {
    let mut value = self.to_value();
    for path in SECRET_PATHS {
      if let Some(node) = value.pointer_mut(&pointer(path)) {
        if *path == "access.gate" {
          mask_gate(node);
        } else {
          mask_all(node);
        }
      }
    }
    value
  }

  /// The dotted paths (`section.field`, or `section` for a list section)
  /// whose values differ between `self` and `other`.
  pub fn changed_paths(&self, other: &ServerSettings) -> Vec<String> {
    let mut out = Vec::new();
    diff_paths(&self.to_value(), &other.to_value(), String::new(), &mut out);
    out
  }
}

/// Puts the stored credential back wherever `incoming`, a document read from
/// [`ServerSettings::masked`] and edited, still carries [`MASKED`]. A mask
/// with nothing stored behind it is refused rather than stored as the secret.
pub fn unmask(incoming: &mut serde_json::Value, current: &ServerSettings) -> Result<(), String> {
  let current = current.to_value();
  for path in SECRET_PATHS {
    let pointer = pointer(path);
    if let Some(node) = incoming.pointer_mut(&pointer) {
      restore(node, current.pointer(&pointer), path)?;
    }
  }
  Ok(())
}

fn pointer(path: &str) -> String {
  format!("/{}", path.replace('.', "/"))
}

fn mask_all(node: &mut serde_json::Value) {
  match node {
    serde_json::Value::String(s) => *s = MASKED.to_string(),
    serde_json::Value::Array(items) => items.iter_mut().for_each(mask_all),
    serde_json::Value::Object(map) => map.values_mut().for_each(mask_all),
    _ => {}
  }
}

fn mask_gate(node: &mut serde_json::Value) {
  match node {
    // `gate: "user:password"`, the scalar spelling.
    serde_json::Value::String(s) => *s = MASKED.to_string(),
    serde_json::Value::Array(methods) => methods.iter_mut().for_each(mask_gate),
    serde_json::Value::Object(method) => {
      for field in GATE_SECRET_FIELDS {
        if let Some(value) = method.get_mut(*field) {
          mask_all(value);
        }
      }
    }
    _ => {}
  }
}

fn restore(
  node: &mut serde_json::Value,
  stored: Option<&serde_json::Value>,
  path: &str,
) -> Result<(), String> {
  match node {
    serde_json::Value::String(s) if s == MASKED => match stored {
      Some(value) if !value.is_null() => {
        *node = value.clone();
        Ok(())
      }
      _ => Err(format!(
        "{path}: `{MASKED}` stands for a stored credential, and none is stored there"
      )),
    },
    serde_json::Value::Array(items) => {
      for (i, item) in items.iter_mut().enumerate() {
        restore(item, stored.and_then(|s| s.get(i)), path)?;
      }
      Ok(())
    }
    serde_json::Value::Object(map) => {
      for (key, value) in map.iter_mut() {
        restore(value, stored.and_then(|s| s.get(key.as_str())), path)?;
      }
      Ok(())
    }
    _ => Ok(()),
  }
}

/// Whether `path` falls under one of `prefixes` (a prefix names a field or a
/// whole section).
pub fn path_in(path: &str, prefixes: &[&str]) -> bool {
  prefixes
    .iter()
    .any(|p| path == *p || path.starts_with(&format!("{p}.")))
}

fn prune_defaults(value: &mut serde_json::Value, default: &serde_json::Value) {
  if let (serde_json::Value::Object(map), serde_json::Value::Object(defaults)) =
    (&mut *value, default)
  {
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
      let Some(d) = defaults.get(&key) else {
        continue;
      };
      let entry = map.get_mut(&key).expect("key just listed");
      if entry == d {
        map.remove(&key);
        continue;
      }
      prune_defaults(entry, d);
      if entry.as_object().is_some_and(|m| m.is_empty()) {
        map.remove(&key);
      }
    }
  }
}

fn diff_paths(a: &serde_json::Value, b: &serde_json::Value, prefix: String, out: &mut Vec<String>) {
  match (a, b) {
    (serde_json::Value::Object(ma), serde_json::Value::Object(mb)) => {
      let mut keys: Vec<&String> = ma.keys().chain(mb.keys()).collect();
      keys.sort();
      keys.dedup();
      for key in keys {
        let path = if prefix.is_empty() {
          key.clone()
        } else {
          format!("{prefix}.{key}")
        };
        let null = serde_json::Value::Null;
        diff_paths(
          ma.get(key).unwrap_or(&null),
          mb.get(key).unwrap_or(&null),
          path,
          out,
        );
      }
    }
    _ if a != b => out.push(prefix),
    _ => {}
  }
}

#[cfg(test)]
#[path = "server_settings_tests.rs"]
mod tests;
