//! Server-wide source-IP deny list (the `denied_ips:` key of
//! `aperio-server.yaml`, or `APERIO_DENIED_IPS`).
//!
//! The inverse of the allowlists: `allowed_ips` on a token or a service says
//! who may in, and `admin_allowed_ips` fences the admin surface, but until
//! this there was no way to say "this address never gets in". Blocking one
//! scanner meant either turning on an allowlist, which locks out everyone the
//! operator did not think to name, or reaching for the fronting proxy.
//!
//! The list is checked at the outermost layer, so it covers everything the
//! server answers: proxied traffic, the dashboard and its API, the tunnel
//! endpoints. It is deliberately blunt, which is the point of a deny list,
//! and it is the first thing a request meets, so a blocked address cannot
//! spend a rate-limit bucket, occupy a request slot, or reach a backend.
//!
//! The answer is `403` rather than a stealth `404`. A per-service
//! `allowed_ips` rejection stays quiet because it is a routing decision about
//! one service among many; this is an operator's explicit, server-wide block,
//! and saying so is what makes it debuggable when the operator has blocked
//! themselves, which is the common accident.

use std::net::IpAddr;

/// Compiled deny list carried in the server configuration.
#[derive(Default, Clone)]
pub(crate) struct DenyList {
  /// Normalized (address, prefix length) ranges; empty = feature off.
  ranges: Vec<(IpAddr, u32)>,
}

impl DenyList {
  /// True when nothing is denied (the fast path, checked before anything
  /// else so an unconfigured server pays one comparison per request).
  pub(crate) fn is_empty(&self) -> bool {
    self.ranges.is_empty()
  }

  /// True when this address falls inside a denied range.
  pub(crate) fn blocks(&self, ip: IpAddr) -> bool {
    !self.ranges.is_empty() && crate::routing::ip_in_ranges(ip, &self.ranges)
  }

  /// Number of configured ranges, for the startup log.
  pub(crate) fn len(&self) -> usize {
    self.ranges.len()
  }

  /// Compiles a comma-separated or list-shaped source. An invalid entry
  /// disables nothing and drops nothing silently: the caller decides, and
  /// both callers report it.
  pub(crate) fn parse(raw: &str) -> Result<Self, String> {
    crate::routing::parse_trusted_proxies(raw).map(|ranges| DenyList { ranges })
  }
}

/// Reads `denied_ips:` from the live config document, falling back to
/// `APERIO_DENIED_IPS`. Reading the document rather than the materialized
/// environment variable is what makes the list hot-reloadable: blocking an
/// address in the middle of an incident should not need a restart.
///
/// An invalid entry leaves the previous list in place rather than applying a
/// partial one, and says so: silently dropping one range from a block list is
/// the failure mode where somebody stays reachable while the operator
/// believes otherwise.
pub(crate) fn from_config() -> DenyList {
  let raw = match crate::config_file::structured("denied_ips") {
    Some(value) => match value {
      serde_yaml::Value::Sequence(items) => {
        let parts: Vec<String> = items
          .iter()
          .filter_map(|v| match v {
            serde_yaml::Value::String(s) => Some(s.clone()),
            other => other.as_i64().map(|n| n.to_string()),
          })
          .collect();
        parts.join(",")
      }
      serde_yaml::Value::String(s) => s,
      _ => {
        tracing::error!(
          "`denied_ips:` in aperio-server.yaml must be a list of IPs or CIDRs; the deny list is unchanged"
        );
        return DenyList::default();
      }
    },
    None => std::env::var("APERIO_DENIED_IPS").unwrap_or_default(),
  };
  if raw.trim().is_empty() {
    return DenyList::default();
  }
  match DenyList::parse(&raw) {
    Ok(list) => list,
    Err(e) => {
      tracing::error!("`denied_ips` is invalid ({e}); no addresses are being blocked");
      DenyList::default()
    }
  }
}

#[cfg(test)]
#[path = "deny_list_tests.rs"]
mod tests;
