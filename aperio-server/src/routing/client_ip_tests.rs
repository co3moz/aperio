//! Who the visitor is through a proxy, and mostly who they are *not*: a
//! forwarding header is ignored without a trusted peer, the chain is walked
//! from the right, and a rewritten `X-Forwarded-For` is noticed rather than
//! believed.

use super::*;
use crate::routing::extract_client_ip;
use axum::http::{HeaderMap, HeaderValue};
use std::net::{IpAddr, Ipv4Addr};

// --- extract_client_ip ------------------------------------------------------

fn ip(s: &str) -> IpAddr {
  s.parse().unwrap()
}

#[test]
fn client_ip_ignores_headers_without_trust() {
  let mut h = HeaderMap::new();
  h.insert("x-forwarded-for", "9.9.9.9".parse().unwrap());
  // trust_proxy = false → always the socket fallback.
  assert_eq!(
    extract_client_ip(&h, ip("1.1.1.1"), false, None, &[]),
    ip("1.1.1.1")
  );
}

#[test]
fn client_ip_honors_headers_with_trust() {
  let mut h = HeaderMap::new();
  h.insert("x-forwarded-for", "9.9.9.9, 8.8.8.8".parse().unwrap());
  // First XFF entry wins.
  assert_eq!(
    extract_client_ip(&h, ip("1.1.1.1"), true, None, &[]),
    ip("9.9.9.9")
  );

  // x-real-ip is used when XFF is absent.
  let mut r = HeaderMap::new();
  r.insert("x-real-ip", "7.7.7.7".parse().unwrap());
  assert_eq!(
    extract_client_ip(&r, ip("1.1.1.1"), true, None, &[]),
    ip("7.7.7.7")
  );
}

#[test]
fn client_ip_custom_header_takes_precedence() {
  let mut h = HeaderMap::new();
  h.insert("cf-connecting-ip", "5.5.5.5".parse().unwrap());
  h.insert("x-forwarded-for", "9.9.9.9".parse().unwrap());
  assert_eq!(
    extract_client_ip(&h, ip("1.1.1.1"), true, Some("cf-connecting-ip"), &[]),
    ip("5.5.5.5")
  );
  // A malformed custom header value falls through to XFF.
  let mut bad = HeaderMap::new();
  bad.insert("cf-connecting-ip", "not-an-ip".parse().unwrap());
  bad.insert("x-forwarded-for", "9.9.9.9".parse().unwrap());
  assert_eq!(
    extract_client_ip(&bad, ip("1.1.1.1"), true, Some("cf-connecting-ip"), &[]),
    ip("9.9.9.9")
  );
}

#[test]
fn client_ip_cloudflare_only_when_configured() {
  // Cloudflare → Traefik: Traefik rewrote XFF down to the Cloudflare edge, but
  // CF-Connecting-IP still carries the true visitor. It is honored when
  // configured as the real-IP header (APERIO_TRUST_CF_HEADER resolves to this).
  let mut h = HeaderMap::new();
  h.insert("cf-connecting-ip", "203.0.113.18".parse().unwrap());
  h.insert("x-forwarded-for", "162.158.19.179".parse().unwrap());
  assert_eq!(
    extract_client_ip(&h, ip("1.1.1.1"), true, Some("cf-connecting-ip"), &[]),
    ip("203.0.113.18")
  );

  // Without that configuration the header is NOT trusted: any visitor can send
  // CF-Connecting-IP, and a non-Cloudflare proxy passes it through untouched,
  // trusting it implicitly would let clients spoof rate limiting, audit logs,
  // and token IP allowlists. XFF (set by the trusted proxy) wins instead.
  assert_eq!(
    extract_client_ip(&h, ip("1.1.1.1"), true, None, &[]),
    ip("162.158.19.179")
  );

  // Without trust_proxy the header is ignored entirely.
  assert_eq!(
    extract_client_ip(&h, ip("1.1.1.1"), false, Some("cf-connecting-ip"), &[]),
    ip("1.1.1.1")
  );
}

#[test]
fn cloudflare_xff_rewritten_detects_intermediate_proxy() {
  let cf = ip("203.0.113.18");
  // XFF was rewritten to the Cloudflare edge only → mismatch worth flagging.
  let mut rewritten = HeaderMap::new();
  rewritten.insert("x-forwarded-for", "162.158.19.179".parse().unwrap());
  assert!(cloudflare_xff_rewritten(&rewritten, cf));
  // XFF keeps the real client first (visitor, cf-edge) → consistent chain.
  let mut ok = HeaderMap::new();
  ok.insert(
    "x-forwarded-for",
    "203.0.113.18, 162.158.19.179".parse().unwrap(),
  );
  assert!(!cloudflare_xff_rewritten(&ok, cf));
  // No X-Forwarded-For at all → nothing to compare, not a mismatch.
  assert!(!cloudflare_xff_rewritten(&HeaderMap::new(), cf));
}

#[test]
fn client_ip_trusted_chain_walk() {
  // Trusted set: the local LB and the CDN egress range.
  let trusted = parse_trusted_proxies("10.0.0.0/8, 162.158.0.0/16").unwrap();

  // visitor → CDN (162.158.19.179) → LB (10.0.0.2) → aperio.
  // XFF: "visitor, cdn"; direct peer: the LB. Walking backwards past trusted
  // hops lands on the visitor.
  let mut h = HeaderMap::new();
  h.insert(
    "x-forwarded-for",
    "203.0.113.18, 162.158.19.179".parse().unwrap(),
  );
  assert_eq!(
    extract_client_ip(&h, ip("10.0.0.2"), true, None, &trusted),
    ip("203.0.113.18")
  );

  // A spoofed XFF prefix cannot push the resolution past the first untrusted
  // hop: the walk stops at the visitor's own (untrusted) address.
  let mut spoof = HeaderMap::new();
  spoof.insert(
    "x-forwarded-for",
    "1.2.3.4, 203.0.113.18, 162.158.19.179".parse().unwrap(),
  );
  assert_eq!(
    extract_client_ip(&spoof, ip("10.0.0.2"), true, None, &trusted),
    ip("203.0.113.18")
  );

  // Direct connection from an untrusted peer: headers are ignored entirely.
  assert_eq!(
    extract_client_ip(&spoof, ip("198.51.100.9"), true, None, &trusted),
    ip("198.51.100.9")
  );

  // Every hop trusted (an internal health check): leftmost XFF entry wins.
  let mut internal = HeaderMap::new();
  internal.insert("x-forwarded-for", "10.0.0.7, 10.0.0.8".parse().unwrap());
  assert_eq!(
    extract_client_ip(&internal, ip("10.0.0.2"), true, None, &trusted),
    ip("10.0.0.7")
  );

  // No XFF at all from a trusted peer: the peer itself is reported.
  assert_eq!(
    extract_client_ip(&HeaderMap::new(), ip("10.0.0.2"), true, None, &trusted),
    ip("10.0.0.2")
  );

  // A configured real-IP header wins over the walk, but only when the direct
  // peer is trusted.
  let mut cf = HeaderMap::new();
  cf.insert("true-client-ip", "203.0.113.99".parse().unwrap());
  cf.insert("x-forwarded-for", "162.158.19.179".parse().unwrap());
  assert_eq!(
    extract_client_ip(&cf, ip("10.0.0.2"), true, Some("true-client-ip"), &trusted),
    ip("203.0.113.99")
  );
  assert_eq!(
    extract_client_ip(
      &cf,
      ip("198.51.100.9"),
      true,
      Some("true-client-ip"),
      &trusted
    ),
    ip("198.51.100.9")
  );
}

#[test]
fn trusted_proxies_parse_and_reject() {
  let list = parse_trusted_proxies("10.0.0.0/8, 203.0.113.7, 2400:cb00::/32").unwrap();
  assert_eq!(list.len(), 3);
  assert_eq!(list[1], (ip("203.0.113.7"), 32));

  // Invalid entries are an error, not silently dropped.
  assert!(parse_trusted_proxies("10.0.0.0/8, not-an-ip").is_err());
  assert!(parse_trusted_proxies("10.0.0.0/40").is_err());
  // Empty segments are tolerated (trailing comma).
  assert_eq!(parse_trusted_proxies("10.0.0.0/8,").unwrap().len(), 1);
}

#[test]
pub(crate) fn test_extract_client_ip_trusted() {
  let direct = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));

  // No headers → fallback to socket address
  let headers = HeaderMap::new();
  assert_eq!(extract_client_ip(&headers, direct, true, None, &[]), direct);

  // X-Forwarded-For with single IP
  let mut headers = HeaderMap::new();
  headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.10"));
  assert_eq!(
    extract_client_ip(&headers, direct, true, None, &[]),
    "198.51.100.10".parse::<IpAddr>().unwrap()
  );

  // X-Forwarded-For with chained proxies → leftmost (original client)
  let mut headers = HeaderMap::new();
  headers.insert(
    "x-forwarded-for",
    HeaderValue::from_static("198.51.100.10, 10.0.0.1, 10.0.0.2"),
  );
  assert_eq!(
    extract_client_ip(&headers, direct, true, None, &[]),
    "198.51.100.10".parse::<IpAddr>().unwrap()
  );

  // X-Real-IP fallback when X-Forwarded-For absent
  let mut headers = HeaderMap::new();
  headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.20"));
  assert_eq!(
    extract_client_ip(&headers, direct, true, None, &[]),
    "198.51.100.20".parse::<IpAddr>().unwrap()
  );

  // Malformed X-Forwarded-For → fallback
  let mut headers = HeaderMap::new();
  headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
  assert_eq!(extract_client_ip(&headers, direct, true, None, &[]), direct);

  // A configured real-IP header (e.g. CF-Connecting-IP) wins over
  // X-Forwarded-For, which chained proxies often reset to the CDN edge.
  let mut headers = HeaderMap::new();
  headers.insert(
    "x-forwarded-for",
    HeaderValue::from_static("162.158.14.210"),
  );
  headers.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.18"));
  assert_eq!(
    extract_client_ip(&headers, direct, true, Some("cf-connecting-ip"), &[]),
    "203.0.113.18".parse::<IpAddr>().unwrap()
  );
  // ...but only when trust_proxy is on.
  assert_eq!(
    extract_client_ip(&headers, direct, false, Some("cf-connecting-ip"), &[]),
    direct
  );
}

#[test]
pub(crate) fn test_extract_client_ip_untrusted_ignores_headers() {
  let direct = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));

  // When trust_proxy is false, spoofed X-Forwarded-For must be ignored.
  let mut headers = HeaderMap::new();
  headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.10"));
  assert_eq!(
    extract_client_ip(&headers, direct, false, None, &[]),
    direct
  );

  // Spoofed X-Real-IP must also be ignored.
  let mut headers = HeaderMap::new();
  headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.20"));
  assert_eq!(
    extract_client_ip(&headers, direct, false, None, &[]),
    direct
  );

  // No headers → fallback to socket address
  let headers = HeaderMap::new();
  assert_eq!(
    extract_client_ip(&headers, direct, false, None, &[]),
    direct
  );
}
