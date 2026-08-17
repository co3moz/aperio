//! What a service binds on, and whether a request matches it: path binds and
//! their segment boundaries, hostname binds, the random-subdomain pattern, and
//! the traversal check that decides a path cannot be trusted to mean what it
//! looks like.

use axum::http::HeaderMap;

use super::*;

/// Normalizes a path bind by ensuring it starts with `/` and stripping any
/// trailing slashes. Returns `None` for the empty/root bind or for values
/// that fail validation (too long, path traversal, or unsafe characters).
pub(crate) fn normalize_path_bind(bind: &str) -> Option<String> {
  const MAX_PATH_BIND_LEN: usize = 256;

  let trimmed = bind.trim().trim_end_matches('/');
  if trimmed.is_empty() || trimmed == "/" {
    return None;
  }
  if trimmed.len() > MAX_PATH_BIND_LEN {
    warn!(
      "Rejected path_bind exceeding maximum length ({} > {})",
      trimmed.len(),
      MAX_PATH_BIND_LEN
    );
    return None;
  }
  // Reject path traversal segments and require URL-safe path characters only.
  for segment in trimmed.split('/') {
    if segment.is_empty() {
      continue;
    }
    if segment == ".." || segment == "." {
      warn!("Rejected path_bind containing traversal segment: {}", bind);
      return None;
    }
    if !segment
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'))
    {
      warn!("Rejected path_bind with unsafe characters: {}", bind);
      return None;
    }
  }
  let with_slash = if trimmed.starts_with('/') {
    trimmed.to_string()
  } else {
    format!("/{}", trimmed)
  };
  Some(with_slash)
}

/// Checks whether `uri_path` matches a path `bind` on a segment boundary,
/// preventing `/apixyz` from matching a bind of `/api`.
pub(crate) fn path_matches_bind(uri_path: &str, bind: &str) -> bool {
  uri_path == bind || uri_path.starts_with(&format!("{}/", bind))
}

/// Decodes single-level percent-encoding in a path (`%2e` → `.`, `%2f` → `/`),
/// mirroring the one decode a backend performs before resolving the path.
/// Undecodable/invalid `%XX` sequences are left as-is.
fn percent_decode_once(s: &str) -> String {
  let bytes = s.as_bytes();
  let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'%' && i + 2 < bytes.len() {
      let hi = (bytes[i + 1] as char).to_digit(16);
      let lo = (bytes[i + 2] as char).to_digit(16);
      if let (Some(h), Some(l)) = (hi, lo) {
        out.push((h * 16 + l) as u8);
        i += 3;
        continue;
      }
    }
    out.push(bytes[i]);
    i += 1;
  }
  String::from_utf8_lossy(&out).into_owned()
}

/// True when a request path contains a `.`/`..` traversal segment, either
/// literally or single-percent-encoded (`%2e%2e`, `..%2f`, `%2e%2e/`). Path
/// binds themselves forbid traversal ([`normalize_path_bind`]), but the
/// *request* path is never normalized by hyper/axum, so a scope check that
/// trusts it (share links, path-bind routing) could otherwise be widened with
/// `..` (`/public/../admin` starts with `/public/`). Both `/` and `\` are
/// treated as separators.
pub(crate) fn request_path_has_traversal(path: &str) -> bool {
  let decoded = percent_decode_once(path);
  [path, decoded.as_str()].iter().any(|candidate| {
    candidate
      .split(['/', '\\'])
      .any(|seg| seg == ".." || seg == ".")
  })
}

/// Normalizes a hostname bind: lowercases, trims whitespace, strips a
/// trailing dot and an optional port suffix. Returns `None` for empty values
/// or values containing characters outside the DNS-safe set.
/// Normalizes a random-subdomain pattern into canonical form: a hostname
/// whose leftmost label contains exactly one `*` placeholder.
///
/// - `example.com`        → `*.example.com`
/// - `*.example.com`      → `*.example.com`
/// - `*-test.example.com` → `*-test.example.com` (same-level suffix, so one
///   wildcard TLS certificate covers `<random>-test.example.com`)
pub(crate) fn normalize_random_subdomain_pattern(raw: &str) -> Option<String> {
  let trimmed = raw.trim().trim_matches('.').to_ascii_lowercase();
  if trimmed.is_empty() {
    return None;
  }
  let pattern = if trimmed.contains('*') {
    trimmed
  } else {
    format!("*.{}", trimmed)
  };
  // Exactly one `*`, and only in the leftmost label.
  if pattern.matches('*').count() != 1 {
    return None;
  }
  let (head, tail) = pattern.split_once('.')?;
  if !head.contains('*') || tail.contains('*') {
    return None;
  }
  // The pattern must yield a valid hostname once the placeholder is filled.
  normalize_hostname_bind(&pattern.replacen('*', "abc123", 1))?;
  Some(pattern)
}

/// True when `host` could have been produced by the canonical
/// random-subdomain pattern: every label after the first matches exactly,
/// and the leftmost label fits the pattern's prefix/suffix around the `*`
/// (with a non-empty random part). Used to recognize preview hosts for
/// noindex marking (APERIO_PREVIEW_NOINDEX).
pub(crate) fn host_matches_random_pattern(host: &str, pattern: &str) -> bool {
  let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
  let (Some((host_label, host_rest)), Some((pat_label, pat_rest))) =
    (host.split_once('.'), pattern.split_once('.'))
  else {
    return false;
  };
  if host_rest != pat_rest {
    return false;
  }
  let Some((prefix, suffix)) = pat_label.split_once('*') else {
    return false;
  };
  host_label.len() > prefix.len() + suffix.len()
    && host_label.starts_with(prefix)
    && host_label.ends_with(suffix)
}

/// Produces a concrete random hostname from a canonical subdomain pattern
/// (the `*` placeholder is replaced with a random label).
pub(crate) fn random_subdomain_hostname(pattern: &str) -> String {
  let label: String = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
  pattern.replacen('*', &label, 1)
}

/// Deterministic variant of [`random_subdomain_hostname`]: the label is derived
/// from `seed` (a stable per-instance + declared-bind key) so every parallel
/// connection of the same client process independently produces the *same*
/// random hostname, no coordination, no race, instead of each minting a
/// distinct name.
pub(crate) fn random_subdomain_hostname_seeded(pattern: &str, seed: &str) -> String {
  use sha2::{Digest, Sha256};
  let digest = Sha256::digest(seed.as_bytes());
  // 5 bytes -> 10 hex chars, matching the random label's length.
  let label: String = digest.iter().take(5).map(|b| format!("{b:02x}")).collect();
  pattern.replacen('*', &label, 1)
}

pub(crate) fn normalize_hostname_bind(host: &str) -> Option<String> {
  const MAX_HOSTNAME_LEN: usize = 253;

  let trimmed = host.trim().trim_end_matches('.').to_ascii_lowercase();
  // Strip a port suffix (not applicable to bracketed IPv6 literals).
  let without_port = match trimmed.split_once(':') {
    Some((h, port)) if !h.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h.to_string(),
    _ => trimmed,
  };
  if without_port.is_empty() || without_port.len() > MAX_HOSTNAME_LEN {
    return None;
  }
  let valid = without_port
    .split('.')
    .all(|label| !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
  if !valid {
    warn!("Rejected hostname_bind with invalid format: {}", host);
    return None;
  }
  Some(without_port)
}

/// Extracts the request hostname from the `Host` header (lowercased, port
/// stripped). Returns `None` when the header is absent or malformed.
pub(crate) fn extract_request_host(headers: &HeaderMap) -> Option<String> {
  let raw = headers.get("host")?.to_str().ok()?;
  let trimmed = raw.trim().to_ascii_lowercase();
  // Bracketed IPv6 literal: [::1]:8080 → ::1 is not a valid hostname bind
  // anyway, but strip the port consistently.
  let host = if let Some(stripped) = trimmed.strip_prefix('[') {
    stripped.split(']').next().unwrap_or("").to_string()
  } else {
    trimmed.split(':').next().unwrap_or("").to_string()
  };
  if host.is_empty() { None } else { Some(host) }
}

#[cfg(test)]
#[path = "binds_tests.rs"]
mod tests;
