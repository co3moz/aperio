//! Who the visitor is, seen through whatever is in front of the server.
//!
//! A forwarding header is only as good as the hop that set it, so none of them
//! are read unless the peer is a configured trusted proxy, and the chain is
//! walked from the right rather than trusting its leftmost entry.

use axum::http::HeaderMap;
use std::net::IpAddr;

use super::*;

/// Resolves the real client IP, honoring forwarding headers only when
/// `trust_proxy` is enabled (i.e. the server runs behind a trusted reverse
/// proxy). Otherwise the direct socket address is used, since clients could
/// otherwise spoof these headers to bypass rate limiting.
///
/// Two modes (both only under `trust_proxy`):
///
/// **Trusted-proxy chain** (`trusted_proxies` non-empty, the recommended,
/// CDN-agnostic model, same as nginx `real_ip` / Express `trust proxy`): the
/// `X-Forwarded-For` chain plus the direct socket peer is walked from the
/// nearest hop backwards, skipping addresses inside the trusted ranges; the
/// first untrusted address is the client. Headers are ignored entirely when
/// the direct peer itself is not trusted, so a visitor connecting around the
/// proxy cannot spoof anything. Works for any chain (Cloudflare, Fastly,
/// Akamai, an LB + CDN combo, …) as long as every hop appends to XFF.
/// A configured `real_ip_header` still wins over the walk (for proxies that
/// rewrite XFF instead of appending), but only when the direct peer is
/// trusted.
///
/// **Legacy header mode** (`trusted_proxies` empty):
/// 1. A configured `real_ip_header` (APERIO_REAL_IP_HEADER, or
///    `CF-Connecting-IP` via the APERIO_TRUST_CF_HEADER opt-in). Never
///    consulted automatically: any visitor can send such a header, and an
///    intermediate proxy that was never told about it will pass it through
///    untouched, trusting it implicitly would let clients spoof their IP for
///    rate limiting, audit logs, and token IP allowlists. When the configured
///    header is Cloudflare's and `X-Forwarded-For` starts with a *different*
///    address, an intermediate proxy has rewritten XFF,
///    [`warn_if_xff_rewritten`] flags that misconfiguration once so the
///    operator can fix the chain.
/// 2. The first `X-Forwarded-For` entry.
/// 3. `X-Real-IP`.
pub(crate) fn extract_client_ip(
  headers: &HeaderMap,
  fallback: IpAddr,
  trust_proxy: bool,
  real_ip_header: Option<&str>,
  trusted_proxies: &[(IpAddr, u32)],
) -> IpAddr {
  if !trust_proxy {
    return fallback;
  }
  if !trusted_proxies.is_empty() {
    return extract_via_trusted_chain(headers, fallback, real_ip_header, trusted_proxies);
  }
  if let Some(parsed) = real_ip_header_value(headers, real_ip_header) {
    return parsed;
  }
  if let Some(xff) = headers.get("x-forwarded-for")
    && let Ok(xff_str) = xff.to_str()
    && let Some(first) = xff_str.split(',').next()
    && let Ok(parsed) = first.trim().parse::<IpAddr>()
  {
    return parsed;
  }
  if let Some(real_ip) = headers.get("x-real-ip")
    && let Ok(real_str) = real_ip.to_str()
    && let Ok(parsed) = real_str.trim().parse::<IpAddr>()
  {
    return parsed;
  }
  fallback
}

/// Reads and parses the configured real-IP header, emitting the Cloudflare
/// rewritten-XFF diagnostic when applicable.
fn real_ip_header_value(headers: &HeaderMap, real_ip_header: Option<&str>) -> Option<IpAddr> {
  let name = real_ip_header?;
  let parsed = headers
    .get(name)?
    .to_str()
    .ok()?
    .trim()
    .parse::<IpAddr>()
    .ok()?;
  if name.eq_ignore_ascii_case("cf-connecting-ip") {
    warn_if_xff_rewritten(headers, parsed);
  }
  Some(parsed)
}

/// True when `ip` falls inside any of the given IP/CIDR ranges.
pub(crate) fn ip_in_ranges(ip: IpAddr, ranges: &[(IpAddr, u32)]) -> bool {
  ranges
    .iter()
    .any(|(base, bits)| crate::auth::cidr_contains(*base, *bits, ip))
}

/// True when `ip` falls inside any of the trusted proxy ranges.
fn is_trusted_proxy(ip: IpAddr, trusted: &[(IpAddr, u32)]) -> bool {
  ip_in_ranges(ip, trusted)
}

/// Trusted-proxy chain resolution: walk `[XFF entries…, direct peer]` from the
/// nearest hop backwards and return the first address that is not a trusted
/// proxy. All-trusted chains fall back to the leftmost XFF entry (the chain
/// origin); an untrusted direct peer short-circuits to itself (its headers
/// cannot be trusted at all).
fn extract_via_trusted_chain(
  headers: &HeaderMap,
  peer: IpAddr,
  real_ip_header: Option<&str>,
  trusted: &[(IpAddr, u32)],
) -> IpAddr {
  if !is_trusted_proxy(peer, trusted) {
    return peer;
  }
  // A configured header (e.g. CF-Connecting-IP when the inner proxy rewrites
  // XFF) wins over the walk, but only from a trusted peer, checked above.
  if let Some(parsed) = real_ip_header_value(headers, real_ip_header) {
    return parsed;
  }
  let chain: Vec<IpAddr> = headers
    .get("x-forwarded-for")
    .and_then(|v| v.to_str().ok())
    .map(|s| {
      s.split(',')
        .filter_map(|e| e.trim().parse::<IpAddr>().ok())
        .collect()
    })
    .unwrap_or_default();
  for ip in chain.iter().rev() {
    if !is_trusted_proxy(*ip, trusted) {
      return *ip;
    }
  }
  chain.first().copied().unwrap_or(peer)
}

/// Parses the APERIO_TRUSTED_PROXIES value: a comma-separated list of IPs
/// (`203.0.113.7`) and CIDR ranges (`10.0.0.0/8`). Invalid entries are
/// rejected (returned in the error) rather than silently dropped, so a typo
/// cannot silently shrink the trusted set.
pub(crate) fn parse_trusted_proxies(raw: &str) -> Result<Vec<(IpAddr, u32)>, String> {
  let mut out = Vec::new();
  for entry in raw.split(',') {
    let entry = entry.trim();
    if entry.is_empty() {
      continue;
    }
    let parsed = match entry.split_once('/') {
      Some((base, prefix)) => base.parse::<IpAddr>().ok().zip(prefix.parse::<u32>().ok()),
      None => entry
        .parse::<IpAddr>()
        .ok()
        .map(|ip| (ip, if ip.is_ipv4() { 32 } else { 128 })),
    };
    match parsed {
      Some((ip, bits)) if bits <= if ip.is_ipv4() { 32 } else { 128 } => out.push((ip, bits)),
      _ => return Err(format!("invalid trusted proxy entry: '{entry}'")),
    }
  }
  Ok(out)
}

/// Set once we have warned about a rewritten X-Forwarded-For, so the
/// diagnostic below does not repeat on every subsequent request.
static XFF_MISMATCH_WARNED: AtomicBool = AtomicBool::new(false);

/// True when Cloudflare's `CF-Connecting-IP` is present but `X-Forwarded-For`
/// starts with a *different* address. A correctly configured chain keeps the
/// real client at the front of XFF (`<visitor>, <cf-edge>`), so a mismatch
/// means an intermediate proxy replaced XFF with its own peer. Absent or
/// unparsable XFF is not a mismatch (nothing to compare against).
fn cloudflare_xff_rewritten(headers: &HeaderMap, cf_ip: IpAddr) -> bool {
  headers
    .get("x-forwarded-for")
    .and_then(|v| v.to_str().ok())
    .and_then(|s| s.split(',').next())
    .and_then(|s| s.trim().parse::<IpAddr>().ok())
    .is_some_and(|first| first != cf_ip)
}

/// Emits a one-time warning when the configured `CF-Connecting-IP` real-IP
/// header is used but an intermediate proxy (e.g. Traefik that does not trust
/// Cloudflare's forwarded headers) rewrote `X-Forwarded-For`. The Cloudflare
/// header is still used for the real client IP; the warning points the
/// operator at the actual fix.
fn warn_if_xff_rewritten(headers: &HeaderMap, cf_ip: IpAddr) {
  if XFF_MISMATCH_WARNED.load(Ordering::Relaxed) || !cloudflare_xff_rewritten(headers, cf_ip) {
    return;
  }
  if !XFF_MISMATCH_WARNED.swap(true, Ordering::Relaxed) {
    let xff = headers
      .get("x-forwarded-for")
      .and_then(|v| v.to_str().ok())
      .unwrap_or("");
    warn!(
      "Behind Cloudflare (CF-Connecting-IP={cf_ip}) but X-Forwarded-For ({xff}) does not start \
       with the real client, an intermediate proxy is rewriting X-Forwarded-For to its own peer. \
       Using the Cloudflare header for the client IP; check your reverse-proxy chain so forwarded \
       headers are trusted and preserved (e.g. Traefik forwardedHeaders / nginx real_ip), or set \
       APERIO_REAL_IP_HEADER explicitly."
    );
  }
}

#[cfg(test)]
#[path = "client_ip_tests.rs"]
mod tests;
