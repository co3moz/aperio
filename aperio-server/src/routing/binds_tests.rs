//! What a bind normalizes to and what it refuses: path binds at segment
//! boundaries, hostname binds, the random-subdomain pattern and its
//! placeholder, and traversal detected in both literal and encoded form.

use super::*;
use crate::routing::{
  extract_request_host, normalize_hostname_bind, normalize_path_bind,
  normalize_random_subdomain_pattern, path_matches_bind, random_subdomain_hostname,
};
use axum::http::{HeaderMap, HeaderValue};

// --- normalize_path_bind ----------------------------------------------------

#[test]
fn path_bind_normalizes_and_rejects() {
  assert_eq!(normalize_path_bind("/api"), Some("/api".to_string()));
  // Leading slash added, trailing slashes stripped.
  assert_eq!(normalize_path_bind("api/"), Some("/api".to_string()));
  assert_eq!(
    normalize_path_bind("/api/v1//"),
    Some("/api/v1".to_string())
  );
  // Root / empty binds are "no bind".
  assert_eq!(normalize_path_bind(""), None);
  assert_eq!(normalize_path_bind("/"), None);
  assert_eq!(normalize_path_bind("   "), None);
  // Traversal and unsafe characters rejected.
  assert_eq!(normalize_path_bind("/api/../etc"), None);
  assert_eq!(normalize_path_bind("/api/./x"), None);
  assert_eq!(normalize_path_bind("/api/a b"), None);
  assert_eq!(normalize_path_bind("/api/%2e"), None);
  // Allowed URL-safe characters pass.
  assert_eq!(
    normalize_path_bind("/a-b_c.d~e"),
    Some("/a-b_c.d~e".to_string())
  );
  // Over the length limit.
  let long = format!("/{}", "a".repeat(300));
  assert_eq!(normalize_path_bind(&long), None);
}

#[test]
fn path_matches_bind_respects_segment_boundary() {
  assert!(path_matches_bind("/api", "/api"));
  assert!(path_matches_bind("/api/users", "/api"));
  // Not a prefix on a segment boundary.
  assert!(!path_matches_bind("/apixyz", "/api"));
  assert!(!path_matches_bind("/ap", "/api"));
  assert!(!path_matches_bind("/", "/api"));
}

#[test]
fn request_path_traversal_detected_literal_and_encoded() {
  // Clean paths are not traversal.
  assert!(!request_path_has_traversal("/public"));
  assert!(!request_path_has_traversal("/public/page"));
  assert!(!request_path_has_traversal("/a.b/c-d/e_f")); // dots inside a segment are fine

  // Literal traversal.
  assert!(request_path_has_traversal("/public/../admin"));
  assert!(request_path_has_traversal("/public/./x"));
  assert!(request_path_has_traversal("/.."));

  // Single-percent-encoded traversal (a backend decodes once before resolving).
  assert!(request_path_has_traversal("/public/%2e%2e/admin"));
  assert!(request_path_has_traversal("/public/..%2fadmin"));
  assert!(request_path_has_traversal("/public/%2e%2e%2fadmin"));

  // Backslash separator variant.
  assert!(request_path_has_traversal("/public\\..\\admin"));

  // Encoded traversal at the very end of the path (the `%XX` bounds check
  // must still decode a sequence whose last byte is the final byte).
  assert!(request_path_has_traversal("/public/%2e%2e"));
  assert!(request_path_has_traversal("/public/.%2e"));
}

// --- hostname / subdomain normalization -------------------------------------

#[test]
fn hostname_bind_normalizes_and_rejects() {
  assert_eq!(
    normalize_hostname_bind("Example.COM"),
    Some("example.com".to_string())
  );
  // Trailing dot stripped.
  assert_eq!(
    normalize_hostname_bind("example.com."),
    Some("example.com".to_string())
  );
  // Port suffix stripped.
  assert_eq!(
    normalize_hostname_bind("example.com:8080"),
    Some("example.com".to_string())
  );
  assert_eq!(
    normalize_hostname_bind("host:443"),
    Some("host".to_string())
  );
  assert_eq!(normalize_hostname_bind(""), None);
  assert_eq!(normalize_hostname_bind("bad_host"), None); // underscore invalid
  assert_eq!(normalize_hostname_bind("a..b"), None); // empty label
  assert_eq!(normalize_hostname_bind(&"a".repeat(300)), None); // too long
}

#[test]
fn random_subdomain_pattern_canonicalizes() {
  assert_eq!(
    normalize_random_subdomain_pattern("example.com"),
    Some("*.example.com".to_string())
  );
  assert_eq!(
    normalize_random_subdomain_pattern("*.example.com"),
    Some("*.example.com".to_string())
  );
  assert_eq!(
    normalize_random_subdomain_pattern("*-test.example.com"),
    Some("*-test.example.com".to_string())
  );
  // Empty, multiple wildcards, or wildcard outside the leftmost label.
  assert_eq!(normalize_random_subdomain_pattern(""), None);
  assert_eq!(normalize_random_subdomain_pattern("*.*.example.com"), None);
  assert_eq!(
    normalize_random_subdomain_pattern("foo.*.example.com"),
    None
  );
}

#[test]
fn random_subdomain_hostname_fills_placeholder() {
  let host = random_subdomain_hostname("*.example.com");
  assert!(host.ends_with(".example.com"));
  assert!(!host.contains('*'));
  // The generated label is non-empty.
  let label = host.strip_suffix(".example.com").unwrap();
  assert!(!label.is_empty());

  let suffixed = random_subdomain_hostname("*-test.example.com");
  assert!(suffixed.ends_with("-test.example.com"));
  assert!(!suffixed.contains('*'));
}

// --- extract_request_host ---------------------------------------------------

#[test]
fn extract_request_host_variants() {
  let mut h = HeaderMap::new();
  assert_eq!(extract_request_host(&h), None);

  h.insert("host", "Example.com:8080".parse().unwrap());
  assert_eq!(extract_request_host(&h), Some("example.com".to_string()));

  let mut v6 = HeaderMap::new();
  v6.insert("host", "[::1]:8080".parse().unwrap());
  assert_eq!(extract_request_host(&v6), Some("::1".to_string()));
}

#[test]
fn test_host_matches_random_pattern() {
  use super::host_matches_random_pattern;
  // Plain wildcard pattern.
  assert!(host_matches_random_pattern(
    "a1b2c3d4e5.example.com",
    "*.example.com"
  ));
  assert!(host_matches_random_pattern(
    "A1B2C3.example.com:8080",
    "*.example.com"
  ));
  // The parent domain itself is not a preview host.
  assert!(!host_matches_random_pattern("example.com", "*.example.com"));
  // Other domains and deeper subdomains do not match.
  assert!(!host_matches_random_pattern("a.other.com", "*.example.com"));
  assert!(!host_matches_random_pattern(
    "a.b.example.com",
    "*.example.com"
  ));
  // Prefix/suffix patterns: the random part must be non-empty.
  assert!(host_matches_random_pattern(
    "abc-test.example.com",
    "*-test.example.com"
  ));
  assert!(!host_matches_random_pattern(
    "-test.example.com",
    "*-test.example.com"
  ));
  assert!(!host_matches_random_pattern(
    "app.example.com",
    "*-test.example.com"
  ));
}

#[test]
fn test_random_subdomain_hostname_seeded_is_deterministic_and_distinct() {
  let pattern = "*-aperio.example.com";
  // Same seed -> same hostname (so parallel connections of one process share it).
  let a = random_subdomain_hostname_seeded(pattern, "grp1\0app.example.com");
  let b = random_subdomain_hostname_seeded(pattern, "grp1\0app.example.com");
  assert_eq!(a, b);
  // Different seed (other instance, or other declared bind) -> different label.
  let c = random_subdomain_hostname_seeded(pattern, "grp2\0app.example.com");
  let d = random_subdomain_hostname_seeded(pattern, "grp1\0other.example.com");
  assert_ne!(a, c);
  assert_ne!(a, d);
  // Label shape matches the random variant: a 10-char label in place of `*`.
  assert!(a.ends_with("-aperio.example.com"));
  let label = a.strip_suffix("-aperio.example.com").unwrap();
  assert_eq!(label.len(), 10);
  assert!(label.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
pub(crate) fn test_path_matches_bind_segment_boundary() {
  // Exact match
  assert!(path_matches_bind("/api", "/api"));
  // Segment boundary: trailing slash should match
  assert!(path_matches_bind("/api/users", "/api"));
  // Non-boundary prefix must NOT match (the original bug)
  assert!(!path_matches_bind("/apixyz", "/api"));
  assert!(!path_matches_bind("/api-v2", "/api"));
  // Empty bind semantics
  assert!(!path_matches_bind("/", "/api"));
}

#[test]
pub(crate) fn test_normalize_path_bind() {
  // Empty / root → None
  assert_eq!(normalize_path_bind(""), None);
  assert_eq!(normalize_path_bind("/"), None);
  assert_eq!(normalize_path_bind("   "), None);
  // Adds leading slash
  assert_eq!(normalize_path_bind("api"), Some("/api".to_string()));
  // Strips trailing slashes
  assert_eq!(normalize_path_bind("/api/"), Some("/api".to_string()));
  assert_eq!(normalize_path_bind("/api///"), Some("/api".to_string()));
  // Nested paths preserved
  assert_eq!(normalize_path_bind("/api/v2"), Some("/api/v2".to_string()));
  // Path traversal rejected
  assert_eq!(normalize_path_bind("/api/../etc"), None);
  assert_eq!(normalize_path_bind("/.."), None);
  assert_eq!(normalize_path_bind("/./api"), None);
  // Unsafe characters rejected
  assert_eq!(normalize_path_bind("/api;rm -rf"), None);
  assert_eq!(normalize_path_bind("/api?x=1"), None);
  // Allowed special characters
  assert_eq!(
    normalize_path_bind("/api_v2.1"),
    Some("/api_v2.1".to_string())
  );
  assert_eq!(normalize_path_bind("/a-b~c"), Some("/a-b~c".to_string()));
}

#[test]
pub(crate) fn test_normalize_random_subdomain_pattern() {
  // Bare domain gets the implicit leading wildcard label.
  assert_eq!(
    normalize_random_subdomain_pattern("example.com").as_deref(),
    Some("*.example.com")
  );
  // Canonical form is accepted as-is.
  assert_eq!(
    normalize_random_subdomain_pattern("*.example.com").as_deref(),
    Some("*.example.com")
  );
  // Same-level suffix pattern is preserved, not turned into *.-test....
  assert_eq!(
    normalize_random_subdomain_pattern("*-test.example.com").as_deref(),
    Some("*-test.example.com")
  );
  assert_eq!(
    normalize_random_subdomain_pattern("  *.Example.COM.  ").as_deref(),
    Some("*.example.com")
  );
  // Invalid: wildcard outside the leftmost label, multiple wildcards,
  // no domain part, empty.
  assert_eq!(
    normalize_random_subdomain_pattern("test.*.example.com"),
    None
  );
  assert_eq!(normalize_random_subdomain_pattern("*.*.example.com"), None);
  assert_eq!(normalize_random_subdomain_pattern("*"), None);
  assert_eq!(normalize_random_subdomain_pattern(""), None);

  // Generation replaces the placeholder in place.
  let host = random_subdomain_hostname("*-pi.example.com");
  assert!(host.ends_with("-pi.example.com"), "got {host}");
  assert!(!host.contains('*'));
  let host = random_subdomain_hostname("*.example.com");
  assert!(host.ends_with(".example.com") && !host.contains('*'));
}

#[test]
pub(crate) fn test_normalize_hostname_bind() {
  assert_eq!(
    normalize_hostname_bind("a.example.com"),
    Some("a.example.com".to_string())
  );
  // Case-insensitive
  assert_eq!(
    normalize_hostname_bind("A.Example.COM"),
    Some("a.example.com".to_string())
  );
  // Port stripped
  assert_eq!(
    normalize_hostname_bind("a.example.com:8080"),
    Some("a.example.com".to_string())
  );
  // Trailing dot stripped
  assert_eq!(
    normalize_hostname_bind("a.example.com."),
    Some("a.example.com".to_string())
  );
  // Invalid values rejected
  assert_eq!(normalize_hostname_bind(""), None);
  assert_eq!(normalize_hostname_bind("   "), None);
  assert_eq!(normalize_hostname_bind("exa mple.com"), None);
  assert_eq!(normalize_hostname_bind("example..com"), None);
  assert_eq!(normalize_hostname_bind("exa_mple.com"), None);
  assert_eq!(normalize_hostname_bind(&"a".repeat(300)), None);
}

#[test]
pub(crate) fn test_extract_request_host() {
  let mut headers = HeaderMap::new();
  assert_eq!(extract_request_host(&headers), None);

  headers.insert("host", HeaderValue::from_static("A.Example.com:443"));
  assert_eq!(
    extract_request_host(&headers),
    Some("a.example.com".to_string())
  );

  headers.insert("host", HeaderValue::from_static("[::1]:8080"));
  assert_eq!(extract_request_host(&headers), Some("::1".to_string()));
}
