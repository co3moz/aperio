//! Public listener contracts shared by config validation and management.
//!
//! A stored resource names an organization id, never a mutable display name.

use std::net::IpAddr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Explicit actions; tenant roles alone never grant public listener control.
/// Every non-read action implies read, but never another mutation or delegation.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(
  Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ExposeAction {
  Read,
  Create,
  Update,
  Enable,
  Disable,
  Delete,
  Disconnect,
  Delegate,
}

impl ExposeAction {
  pub const ALL: [Self; 8] = [
    Self::Read,
    Self::Create,
    Self::Update,
    Self::Enable,
    Self::Disable,
    Self::Delete,
    Self::Disconnect,
    Self::Delegate,
  ];

  pub fn as_str(self) -> &'static str {
    match self {
      Self::Read => "read",
      Self::Create => "create",
      Self::Update => "update",
      Self::Enable => "enable",
      Self::Disable => "disable",
      Self::Delete => "delete",
      Self::Disconnect => "disconnect",
      Self::Delegate => "delegate",
    }
  }
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Hash, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ExposeProtocol {
  Tcp,
  Udp,
}

impl ExposeProtocol {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Tcp => "tcp",
      Self::Udp => "udp",
    }
  }
}

/// A validation failure safe to return to a caller: no claim secrets.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct ExposeError {
  pub field: String,
  pub message: String,
}

impl ExposeError {
  pub fn new(field: &str, message: impl Into<String>) -> Self {
    Self {
      field: field.into(),
      message: message.into(),
    }
  }
}

impl std::fmt::Display for ExposeError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}: {}", self.field, self.message)
  }
}

/// The qualified `<org>@<name>` spelling tunnel discovery and binds use.
pub fn split_qualified(raw: &str) -> (Option<&str>, &str) {
  match raw.trim().split_once('@') {
    Some((org, name)) => (Some(org.trim()), name.trim()),
    None => (None, raw.trim()),
  }
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExposeSource {
  /// Created and managed through the API or the dashboard, the only source
  /// there is since the server stopped reading a configuration file.
  Api,
}

/// IPv6 listeners are explicitly v6-only. This removes platform-dependent
/// dual-stack overlap; IPv4-mapped IPv6 addresses are rejected by validation.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExposeListener {
  #[cfg_attr(feature = "openapi", schema(value_type = String))]
  pub address: IpAddr,
  pub port: u16,
  pub protocol: ExposeProtocol,
}

impl ExposeListener {
  pub fn conflicts_with(&self, other: &Self) -> bool {
    if self.port != other.port || self.protocol != other.protocol {
      return false;
    }
    match (self.address, other.address) {
      (IpAddr::V4(a), IpAddr::V4(b)) => a == b || a.is_unspecified() || b.is_unspecified(),
      (IpAddr::V6(a), IpAddr::V6(b)) => a == b || a.is_unspecified() || b.is_unspecified(),
      _ => false,
    }
  }
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(tag = "protocol", rename_all = "lowercase", deny_unknown_fields)]
pub enum ExposeLimits {
  Tcp {
    max_connections: u32,
    open_timeout_secs: u32,
    drain_timeout_secs: u32,
    #[serde(default = "default_bytes_per_second")]
    ingress_bytes_per_second: u64,
    #[serde(default = "default_bytes_per_second")]
    egress_bytes_per_second: u64,
  },
  Udp {
    max_sessions: u32,
    max_sessions_per_ip: u32,
    idle_timeout_secs: u32,
    max_datagram_bytes: u32,
    queue_packets: u32,
    queue_bytes: u32,
    new_sessions_per_second: u32,
    ingress_bytes_per_second: u64,
    egress_bytes_per_second: u64,
  },
}

/// Desired state of a managed public listener. Runtime state is kept separate
/// so persisting a resource cannot falsely claim that a socket is listening.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExposeSpec {
  /// Stable organization id, or `master`. Never `*` or a display name.
  pub org_id: String,
  pub tunnel: String,
  pub listener: ExposeListener,
  pub enabled: bool,
  pub limits: ExposeLimits,
  /// Actual socket peers allowed to connect; empty permits every source.
  #[serde(default)]
  pub allowed_ips: Vec<String>,
  /// Visitor-facing DNS name or IP. Does not alter bind, DNS or firewall configuration.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub advertised_host: Option<String>,
}

pub fn default_bytes_per_second() -> u64 {
  16 * 1024 * 1024
}

impl ExposeLimits {
  pub fn defaults(protocol: ExposeProtocol) -> Self {
    match protocol {
      ExposeProtocol::Tcp => Self::Tcp {
        max_connections: 256,
        open_timeout_secs: 10,
        drain_timeout_secs: 30,
        ingress_bytes_per_second: default_bytes_per_second(),
        egress_bytes_per_second: default_bytes_per_second(),
      },
      ExposeProtocol::Udp => Self::Udp {
        max_sessions: 1024,
        max_sessions_per_ip: 32,
        idle_timeout_secs: 60,
        max_datagram_bytes: 65507,
        queue_packets: 64,
        queue_bytes: 262144,
        new_sessions_per_second: 64,
        ingress_bytes_per_second: default_bytes_per_second(),
        egress_bytes_per_second: default_bytes_per_second(),
      },
    }
  }

  pub fn capacity(&self) -> u32 {
    match self {
      Self::Tcp {
        max_connections, ..
      } => *max_connections,
      Self::Udp { max_sessions, .. } => *max_sessions,
    }
  }

  pub fn bandwidth(&self) -> (u64, u64) {
    match self {
      Self::Tcp {
        ingress_bytes_per_second,
        egress_bytes_per_second,
        ..
      }
      | Self::Udp {
        ingress_bytes_per_second,
        egress_bytes_per_second,
        ..
      } => (*ingress_bytes_per_second, *egress_bytes_per_second),
    }
  }
}

impl ExposeSpec {
  /// Store lookups (org existence, target encryption and serving capability)
  /// are deliberately separate from structural validation, permitting offline
  /// named targets without guessing their runtime state.
  pub fn validate(&self) -> Vec<ExposeError> {
    let mut errors = Vec::new();
    if let Some(host) = &self.advertised_host {
      let valid = match host.parse::<IpAddr>() {
        Ok(ip) => !ip.is_unspecified() && !ip.is_multicast(),
        Err(_) => {
          host.len() <= 253
            && host.split('.').all(|label| {
              !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                  .bytes()
                  .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        }
      };
      if !valid {
        errors.push(ExposeError::new(
          "advertised_host",
          "expected a visitor DNS name or non-wildcard IP, without scheme or port",
        ));
      }
    }
    for (index, range) in self.allowed_ips.iter().enumerate() {
      if crate::expose_policy::parse_network(range).is_none() {
        errors.push(ExposeError::new(
          &format!("allowed_ips[{index}]"),
          "expected an IP address or CIDR network",
        ));
      }
    }
    if self.org_id.trim().is_empty() || self.org_id.trim() != self.org_id || self.org_id == "*" {
      errors.push(ExposeError::new(
        "org_id",
        "a concrete organization id is required",
      ));
    }
    if let Err(e) = crate::validate_tunnel_name(&self.tunnel) {
      errors.push(ExposeError::new("tunnel", e));
    }
    if self.listener.port == 0 {
      errors.push(ExposeError::new(
        "listener.port",
        "port must be between 1 and 65535",
      ));
    }
    if matches!(self.listener.address, IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some()) {
      errors.push(ExposeError::new(
        "listener.address",
        "use an IPv4 address instead of mapped IPv6",
      ));
    }
    let mut positive = |field: &str, value: u64| {
      if value == 0 {
        errors.push(ExposeError::new(field, "must be positive"));
      }
    };
    match self.limits {
      ExposeLimits::Tcp {
        max_connections,
        open_timeout_secs,
        ingress_bytes_per_second,
        egress_bytes_per_second,
        ..
      } => {
        positive("limits.max_connections", max_connections.into());
        positive("limits.open_timeout_secs", open_timeout_secs.into());
        positive("limits.ingress_bytes_per_second", ingress_bytes_per_second);
        positive("limits.egress_bytes_per_second", egress_bytes_per_second);
        if self.listener.protocol != ExposeProtocol::Tcp {
          errors.push(ExposeError::new(
            "limits.protocol",
            "limits must match listener protocol",
          ));
        }
      }
      ExposeLimits::Udp {
        max_sessions,
        max_sessions_per_ip,
        idle_timeout_secs,
        max_datagram_bytes,
        queue_packets,
        queue_bytes,
        new_sessions_per_second,
        ingress_bytes_per_second,
        egress_bytes_per_second,
      } => {
        for (field, value) in [
          ("max_sessions", max_sessions),
          ("max_sessions_per_ip", max_sessions_per_ip),
          ("idle_timeout_secs", idle_timeout_secs),
          ("max_datagram_bytes", max_datagram_bytes),
          ("queue_packets", queue_packets),
          ("queue_bytes", queue_bytes),
          ("new_sessions_per_second", new_sessions_per_second),
        ] {
          positive(&format!("limits.{field}"), value.into());
        }
        positive("limits.ingress_bytes_per_second", ingress_bytes_per_second);
        positive("limits.egress_bytes_per_second", egress_bytes_per_second);
        if max_datagram_bytes > 65_507 {
          errors.push(ExposeError::new(
            "limits.max_datagram_bytes",
            "must not exceed 65507 bytes",
          ));
        }
        if max_sessions_per_ip > max_sessions {
          errors.push(ExposeError::new(
            "limits.max_sessions_per_ip",
            "must not exceed max_sessions",
          ));
        }
        if queue_bytes < max_datagram_bytes {
          errors.push(ExposeError::new(
            "limits.queue_bytes",
            "must hold at least one maximum-sized datagram",
          ));
        }
        if self.listener.protocol != ExposeProtocol::Udp {
          errors.push(ExposeError::new(
            "limits.protocol",
            "limits must match listener protocol",
          ));
        }
      }
    }
    errors
  }
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExposeResource {
  pub id: String,
  pub revision: u64,
  pub source: ExposeSource,
  pub spec: ExposeSpec,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExposeListenerState {
  Disabled,
  Binding,
  Listening,
  Draining,
  Suspended,
  Failed,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExposeTargetState {
  Waiting,
  Ready,
  Unavailable,
  Incompatible,
}

#[cfg(test)]
#[path = "expose_tests.rs"]
mod tests;
