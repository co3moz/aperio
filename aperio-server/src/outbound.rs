//! Optional policy over where the server may send *outbound* callbacks:
//! webhook deliveries and autoscaling hooks (planned_features #15).
//!
//! Both features take a URL from a lower-trust party (an Operator creating a
//! webhook, a client declaring `scaling.url`) and then have the server call
//! it, which makes the server a potential blind SSRF probe into its own
//! network. Blocking private addresses outright would break the *normal*
//! deployment, where the receiver lives on the same network, so the policy
//! is opt-in and defaults to today's permissive behaviour:
//!
//! - `outbound.allowlist` (`APERIO_OUTBOUND_ALLOWLIST`): host/CIDR patterns
//!   the server may call. When set, it is the policy: a destination either
//!   matches an entry or is refused, and a matching entry is trusted even if
//!   it is private (the operator named it).
//! - `outbound.block_private` (`APERIO_OUTBOUND_BLOCK_PRIVATE`): with no
//!   allowlist, refuse destinations that resolve to internal addresses
//!   (loopback, RFC 1918, link-local, CGNAT/metadata, unique-local).
//!
//! Enforced when the call is made, not only when the URL is stored, so a
//! policy added later also covers webhooks created before it.

use std::net::IpAddr;

/// One allowlist entry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OutboundPattern {
  /// Exact hostname (case-insensitive, no trailing dot).
  Host(String),
  /// `*.suffix`: any subdomain of `suffix` (but not `suffix` itself).
  Suffix(String),
  /// An IP or CIDR range, matched against IP-literal hosts *and* against
  /// what a hostname resolves to.
  Cidr(IpAddr, u32),
}

/// Parses a comma-separated allowlist. Each entry is a hostname, a
/// `*.suffix` wildcard, an IP, or a CIDR. An unparseable entry is an error:
/// starting with a partial allowlist would silently refuse legitimate
/// destinations (or admit ones the operator meant to exclude).
pub(crate) fn parse_patterns(raw: &str) -> Result<Vec<OutboundPattern>, String> {
  let mut out = Vec::new();
  for entry in raw.split(',') {
    let entry = entry.trim().trim_end_matches('.').to_ascii_lowercase();
    if entry.is_empty() {
      continue;
    }
    if let Some(suffix) = entry.strip_prefix("*.") {
      if suffix.is_empty() || suffix.contains('*') || suffix.contains('/') {
        return Err(format!("invalid wildcard pattern '{entry}'"));
      }
      out.push(OutboundPattern::Suffix(suffix.to_string()));
      continue;
    }
    if entry.contains('*') {
      return Err(format!(
        "invalid pattern '{entry}' (wildcards only as a leading '*.')"
      ));
    }
    // An IP or CIDR entry parses via the shared trusted-proxies grammar;
    // anything else is a literal hostname.
    if entry.parse::<IpAddr>().is_ok() || entry.contains('/') {
      let mut ranges = crate::routing::parse_trusted_proxies(&entry)?;
      out.push(match ranges.pop() {
        Some((base, bits)) => OutboundPattern::Cidr(base, bits),
        None => return Err(format!("invalid entry '{entry}'")),
      });
      continue;
    }
    out.push(OutboundPattern::Host(entry));
  }
  Ok(out)
}

/// The effective outbound policy, snapshotted into the server config at
/// startup. Default: no restriction (today's behaviour).
#[derive(Clone, Debug, Default)]
pub(crate) struct OutboundPolicy {
  pub(crate) allowlist: Vec<OutboundPattern>,
  pub(crate) block_private: bool,
}

impl OutboundPolicy {
  /// Whether any restriction is configured at all: the fast path for the
  /// default deployment skips URL parsing and resolution entirely.
  pub(crate) fn restricted(&self) -> bool {
    !self.allowlist.is_empty() || self.block_private
  }

  /// Checks one destination URL against the policy. `Ok(())` when the
  /// policy is empty, when the host matches an allowlist entry, or when
  /// (allowlist-less) every resolved address is public.
  pub(crate) async fn check(&self, url_str: &str) -> Result<(), String> {
    if !self.restricted() {
      return Ok(());
    }
    let url = url::Url::parse(url_str).map_err(|e| format!("invalid url: {e}"))?;
    let Some(host) = url.host_str() else {
      return Err("no host in the URL".to_string());
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    // Bracketed IPv6 literals come back as `[::1]` from host_str.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let literal_ip = bare.parse::<IpAddr>().ok();

    if !self.allowlist.is_empty() {
      let port = url.port_or_known_default().unwrap_or(443);
      // A hostname is matched by name first; its resolved addresses are also
      // given to the CIDR entries, so an all-IP allowlist still covers named
      // destinations.
      for pattern in &self.allowlist {
        match pattern {
          OutboundPattern::Host(h) if *h == host => return Ok(()),
          OutboundPattern::Suffix(s) => {
            if host.len() > s.len() + 1 && host.ends_with(s.as_str()) {
              let boundary = host.as_bytes()[host.len() - s.len() - 1];
              if boundary == b'.' {
                return Ok(());
              }
            }
          }
          OutboundPattern::Cidr(base, bits) => {
            if let Some(ip) = literal_ip
              && crate::auth::cidr_contains(*base, *bits, ip)
            {
              return Ok(());
            }
          }
          _ => {}
        }
      }
      let has_cidrs = self
        .allowlist
        .iter()
        .any(|p| matches!(p, OutboundPattern::Cidr(..)));
      if literal_ip.is_none() && has_cidrs {
        let addrs = tokio::net::lookup_host((bare, port))
          .await
          .map_err(|e| format!("cannot resolve {host}: {e}"))?;
        for addr in addrs {
          for pattern in &self.allowlist {
            if let OutboundPattern::Cidr(base, bits) = pattern
              && crate::auth::cidr_contains(*base, *bits, addr.ip())
            {
              return Ok(());
            }
          }
        }
      }
      return Err(format!(
        "destination {host} is not on the outbound allowlist (APERIO_OUTBOUND_ALLOWLIST)"
      ));
    }

    // Allowlist-less mode: only the private-address gate.
    if let Some(ip) = literal_ip {
      if is_internal(ip) {
        return Err(format!(
          "destination {host} is an internal address (refused by APERIO_OUTBOUND_BLOCK_PRIVATE)"
        ));
      }
      return Ok(());
    }
    let port = url.port_or_known_default().unwrap_or(443);
    // Resolve and check every address the name maps to: a hostname resolving
    // to 127.0.0.1 or 169.254.169.254 is the classic bypass.
    let addrs = tokio::net::lookup_host((bare, port))
      .await
      .map_err(|e| format!("cannot resolve {host}: {e}"))?;
    let mut any = false;
    for addr in addrs {
      any = true;
      if is_internal(addr.ip()) {
        return Err(format!(
          "{host} resolves to the internal address {} (refused by APERIO_OUTBOUND_BLOCK_PRIVATE)",
          addr.ip()
        ));
      }
    }
    if !any {
      return Err(format!("{host} resolves to no address"));
    }
    Ok(())
  }
}

/// True for addresses that live inside the deployment rather than on the
/// public internet: loopback, RFC 1918, link-local (including the cloud
/// metadata services), CGNAT, unique-local, unspecified, and IPv4-mapped
/// forms of all of those.
pub(crate) fn is_internal(ip: IpAddr) -> bool {
  match ip {
    IpAddr::V4(v4) => {
      v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_documentation()
        || v4.is_unspecified()
        // 100.64.0.0/10, carrier-grade NAT and the usual cloud metadata mesh.
        || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
    }
    IpAddr::V6(v6) => {
      v6.is_loopback()
        || v6.is_unspecified()
        // Unique local (fc00::/7) and link local (fe80::/10).
        || (v6.segments()[0] & 0xfe00) == 0xfc00
        || (v6.segments()[0] & 0xffc0) == 0xfe80
        // IPv4-mapped addresses must be judged by the address they carry.
        || v6.to_ipv4_mapped().is_some_and(|v4| is_internal(IpAddr::V4(v4)))
    }
  }
}

#[cfg(test)]
#[path = "outbound_tests.rs"]
mod tests;
