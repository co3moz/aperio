//! Address allowlists: what an entry may be written as, whether an address
//! falls inside one, and the constant-time comparison every credential check
//! goes through.

use std::net::IpAddr;

use super::*;

/// Checks whether `ip` matches an allowlist of plain IPs and CIDR ranges.
/// An empty list, `*`, `0.0.0.0/0` or `::/0` allow any address.
pub(crate) fn ip_allowed(ip: IpAddr, allowed: &[String]) -> bool {
  if allowed.is_empty() {
    return true;
  }
  allowed.iter().any(|entry| {
    let entry = entry.trim();
    if entry == "*" || entry == "0.0.0.0/0" || entry == "::/0" || entry == "0.0.0.0" {
      return true;
    }
    match entry.split_once('/') {
      Some((base, prefix)) => {
        let (Ok(base_ip), Ok(bits)) = (base.parse::<IpAddr>(), prefix.parse::<u32>()) else {
          return false;
        };
        cidr_contains(base_ip, bits, ip)
      }
      None => entry
        .parse::<IpAddr>()
        .is_ok_and(|allowed_ip| allowed_ip == ip),
    }
  })
}

/// True when `ip` falls inside the CIDR `base/bits` (families must match).
pub(crate) fn cidr_contains(base: IpAddr, bits: u32, ip: IpAddr) -> bool {
  match (base, ip) {
    (IpAddr::V4(b), IpAddr::V4(i)) => {
      if bits > 32 {
        return false;
      }
      if bits == 0 {
        return true;
      }
      let mask = u32::MAX << (32 - bits);
      (u32::from(b) & mask) == (u32::from(i) & mask)
    }
    (IpAddr::V6(b), IpAddr::V6(i)) => {
      if bits > 128 {
        return false;
      }
      if bits == 0 {
        return true;
      }
      let mask = u128::MAX << (128 - bits);
      (u128::from(b) & mask) == (u128::from(i) & mask)
    }
    _ => false,
  }
}

/// Validates an allowlist entry (plain IP or CIDR, or a wildcard form).
pub(crate) fn valid_ip_entry(entry: &str) -> bool {
  let entry = entry.trim();
  if entry == "*" {
    return true;
  }
  match entry.split_once('/') {
    Some((base, prefix)) => {
      let Ok(base_ip) = base.parse::<IpAddr>() else {
        return false;
      };
      match prefix.parse::<u32>() {
        Ok(bits) => match base_ip {
          IpAddr::V4(_) => bits <= 32,
          IpAddr::V6(_) => bits <= 128,
        },
        Err(_) => false,
      }
    }
    None => entry.parse::<IpAddr>().is_ok(),
  }
}

/// Constant-time string comparison to mitigate timing attacks on secrets.
/// Hashes both inputs with SHA-256 first so that length differences do not
/// leak through the comparison timing, then compares the digests using
/// `subtle::ConstantTimeEq`.
pub(crate) fn constant_time_eq_str(a: &str, b: &str) -> bool {
  use subtle::ConstantTimeEq;
  let mut ha = Sha256::default();
  ha.update(a.as_bytes());
  let mut hb = Sha256::default();
  hb.update(b.as_bytes());
  let da = ha.finalize();
  let db = hb.finalize();
  da.ct_eq(&db).into()
}

#[cfg(test)]
#[path = "ip_tests.rs"]
mod tests;
