//! A site's `canary`: which visitors see the new version of a service.

/// Splitting a route's traffic between two versions of a service
/// (planned_features #51).
///
/// Weighted routing and a header-based canary are the same mechanism seen from
/// two angles, which is why they are one block: `weight` is who gets the new
/// version by default, `header` is how somebody opts in regardless.
///
/// ## The split is per visitor, not per request
///
/// Decided by hashing the visitor's address rather than by counting requests.
/// A per-request coin flip would send one page load's twenty assets to both
/// versions, which is not a canary, it is a mixture, and the first thing it
/// breaks is the thing being tested. Hashing means a visitor stays where they
/// landed for as long as their address holds.
///
/// The cost is that the split is only as even as the addresses are spread. At
/// low traffic, or behind one large NAT, twenty percent may not look like
/// twenty percent. That is the right trade for a canary, where consistency
/// per visitor matters more than precision in aggregate.
#[derive(Clone, Debug, Default)]
pub(crate) struct CanaryRule {
  /// The service name traffic is sent to, as a client announces it.
  pub(crate) service: String,
  /// Percentage of visitors sent there without asking. `0` = nobody, which
  /// with a `header` set is the "opt-in only" shape.
  pub(crate) weight: u8,
  /// Request header that sends this visitor there whatever the weight says.
  pub(crate) header: Option<String>,
  /// Value that header must carry. Unset = any non-empty value.
  pub(crate) value: Option<String>,
}

/// Which side of a canary split a request belongs on.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub(crate) enum Side {
  /// Serve from the canary service only.
  Canary,
  /// Serve from anything except the canary service.
  Stable,
}

impl CanaryRule {
  /// Which side this request lands on.
  ///
  /// The header wins outright when it matches: it is somebody asking, and a
  /// weight is only what happens to those who did not.
  pub(crate) fn side_for(
    &self,
    header_value: Option<&str>,
    visitor: Option<std::net::IpAddr>,
  ) -> Side {
    if let Some(name) = &self.header
      && !name.is_empty()
      && let Some(sent) = header_value
    {
      let matches = match &self.value {
        Some(expected) => sent.eq_ignore_ascii_case(expected),
        None => !sent.trim().is_empty(),
      };
      if matches {
        return Side::Canary;
      }
    }
    if self.weight == 0 {
      return Side::Stable;
    }
    if self.weight >= 100 {
      return Side::Canary;
    }
    // No address to be consistent for: a visitor whose address cannot be
    // read gets the stable side rather than a coin flip, because an
    // inconsistent canary is worse than a small one.
    let Some(ip) = visitor else {
      return Side::Stable;
    };
    if bucket_of(ip) < self.weight {
      Side::Canary
    } else {
      Side::Stable
    }
  }
}

/// A visitor's stable position in 0..100.
///
/// FNV-1a rather than the default hasher: `DefaultHasher` is randomly seeded
/// per process, so two servers behind a load balancer would disagree about
/// which visitors are in the canary, and the same visitor would move on every
/// restart. A canary has to be the same answer everywhere, every time.
fn bucket_of(ip: std::net::IpAddr) -> u8 {
  let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
  let octets = match ip {
    std::net::IpAddr::V4(v4) => v4.octets().to_vec(),
    std::net::IpAddr::V6(v6) => v6.octets().to_vec(),
  };
  for byte in octets {
    hash ^= byte as u64;
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
  }
  (hash % 100) as u8
}

#[cfg(test)]
#[path = "canary_tests.rs"]
mod tests;
