//! Delegable public socket allocations and aggregate resource ceilings.
//! Address grants are exact binds; a wildcard grant does not silently grant
//! every concrete interface (or vice versa).

use crate::expose::{ExposeError, ExposeListener, ExposeProtocol, ExposeSpec};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExposeAllocation {
  #[cfg_attr(feature = "openapi", schema(value_type = String))]
  pub address: IpAddr,
  pub protocol: ExposeProtocol,
  pub first_port: u16,
  pub last_port: u16,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExposePolicy {
  pub org_id: String,
  pub revision: u64,
  pub allocations: Vec<ExposeAllocation>,
  #[serde(default)]
  pub reserved: Vec<ExposeListener>,
  pub max_rules: u32,
  pub max_tcp_connections: u64,
  pub max_udp_sessions: u64,
  pub ingress_bytes_per_second: u64,
  pub egress_bytes_per_second: u64,
}

impl ExposePolicy {
  pub fn validate(&self) -> Vec<ExposeError> {
    let mut errors = Vec::new();
    if self.org_id.is_empty() || self.org_id.trim() != self.org_id || self.org_id == "*" {
      errors.push(ExposeError::new(
        "org_id",
        "a concrete organization id is required",
      ));
    }
    for (i, allocation) in self.allocations.iter().enumerate() {
      if allocation.first_port == 0 || allocation.first_port > allocation.last_port {
        errors.push(ExposeError::new(
          &format!("allocations[{i}]"),
          "invalid port range",
        ));
      }
      if matches!(allocation.address, IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some()) {
        errors.push(ExposeError::new(
          &format!("allocations[{i}].address"),
          "use an IPv4 address instead of mapped IPv6",
        ));
      }
    }
    for (i, port) in self.reserved.iter().enumerate() {
      if port.port == 0 {
        errors.push(ExposeError::new(
          &format!("reserved[{i}].port"),
          "port zero cannot be reserved",
        ));
      }
    }
    errors
  }

  pub fn allows(&self, spec: &ExposeSpec) -> bool {
    spec.org_id == self.org_id
      && self.allocations.iter().any(|a| {
        a.address == spec.listener.address
          && a.protocol == spec.listener.protocol
          && (a.first_port..=a.last_port).contains(&spec.listener.port)
      })
      && !self
        .reserved
        .iter()
        .any(|p| p.conflicts_with(&spec.listener))
  }

  /// Capacity is reserved for enabled rules, including offline targets. This
  /// makes the sum of independently enforced listener budgets an org ceiling.
  pub fn fits<'a>(&self, specs: impl IntoIterator<Item = &'a ExposeSpec>) -> bool {
    let (mut rules, mut tcp, mut udp, mut up, mut down) = (0u64, 0u64, 0u64, 0u64, 0u64);
    for spec in specs {
      if spec.org_id != self.org_id {
        continue;
      }
      rules = rules.saturating_add(1);
      if !self.allows(spec) {
        return false;
      }
      if !spec.enabled {
        continue;
      }
      match spec.listener.protocol {
        ExposeProtocol::Tcp => tcp = tcp.saturating_add(spec.limits.capacity().into()),
        ExposeProtocol::Udp => udp = udp.saturating_add(spec.limits.capacity().into()),
      }
      let bandwidth = spec.limits.bandwidth();
      up = up.saturating_add(bandwidth.0);
      down = down.saturating_add(bandwidth.1);
    }
    rules <= u64::from(self.max_rules)
      && tcp <= self.max_tcp_connections
      && udp <= self.max_udp_sessions
      && up <= self.ingress_bytes_per_second
      && down <= self.egress_bytes_per_second
  }

  /// Intersection for multiple identity-provider groups. Combining roles or
  /// capabilities never widens a restriction contributed by either group.
  pub fn intersection(&self, other: &Self) -> Self {
    let mut allocations = Vec::new();
    for a in &self.allocations {
      for b in &other.allocations {
        let first_port = a.first_port.max(b.first_port);
        let last_port = a.last_port.min(b.last_port);
        if a.address == b.address && a.protocol == b.protocol && first_port <= last_port {
          let range = ExposeAllocation {
            address: a.address,
            protocol: a.protocol,
            first_port,
            last_port,
          };
          if !allocations.contains(&range) {
            allocations.push(range);
          }
        }
      }
    }
    let mut reserved = self.reserved.clone();
    for listener in &other.reserved {
      if !reserved.contains(listener) {
        reserved.push(listener.clone());
      }
    }
    Self {
      org_id: self.org_id.clone(),
      revision: 1,
      allocations,
      reserved,
      max_rules: self.max_rules.min(other.max_rules),
      max_tcp_connections: self.max_tcp_connections.min(other.max_tcp_connections),
      max_udp_sessions: self.max_udp_sessions.min(other.max_udp_sessions),
      ingress_bytes_per_second: self
        .ingress_bytes_per_second
        .min(other.ingress_bytes_per_second),
      egress_bytes_per_second: self
        .egress_bytes_per_second
        .min(other.egress_bytes_per_second),
    }
  }

  /// Used when a policy administrator delegates a narrower allocation.
  pub fn contains(&self, child: &Self) -> bool {
    self.org_id == child.org_id
      && child.max_rules <= self.max_rules
      && child.max_tcp_connections <= self.max_tcp_connections
      && child.max_udp_sessions <= self.max_udp_sessions
      && child.ingress_bytes_per_second <= self.ingress_bytes_per_second
      && child.egress_bytes_per_second <= self.egress_bytes_per_second
      && self.reserved.iter().all(|p| child.reserved.contains(p))
      && child.allocations.iter().all(|c| {
        self.allocations.iter().any(|a| {
          a.address == c.address
            && a.protocol == c.protocol
            && a.first_port <= c.first_port
            && a.last_port >= c.last_port
        })
      })
  }
}

/// Small shared CIDR parser; no DNS or platform-dependent address aliases.
pub fn parse_network(raw: &str) -> Option<(IpAddr, u8)> {
  let (address, prefix) = match raw.split_once('/') {
    Some((ip, prefix)) => (ip.parse::<IpAddr>().ok()?, Some(prefix.parse::<u8>().ok()?)),
    None => (raw.parse::<IpAddr>().ok()?, None),
  };
  let width = if address.is_ipv4() { 32 } else { 128 };
  let prefix = prefix.unwrap_or(width);
  (prefix <= width).then_some((address, prefix))
}

pub fn network_contains(raw: &str, peer: IpAddr) -> bool {
  let Some((network, prefix)) = parse_network(raw) else {
    return false;
  };
  match (network, peer) {
    (IpAddr::V4(a), IpAddr::V4(b)) => {
      prefix == 0 || (u32::from(a) >> (32 - prefix)) == (u32::from(b) >> (32 - prefix))
    }
    (IpAddr::V6(a), IpAddr::V6(b)) => {
      prefix == 0 || (u128::from(a) >> (128 - prefix)) == (u128::from(b) >> (128 - prefix))
    }
    _ => false,
  }
}
