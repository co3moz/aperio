use axum::{extract::ws::Message, http::HeaderMap};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Semaphore, mpsc};
use tracing::warn;

use crate::settings::LbStrategy;
use crate::state::{AppState, ClientHandle, RouteGroupKey};

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

/// Selects the pool of candidate client IDs for a request, honoring hostname
/// binds first, then path binds within the hostname group. Returns the pool
/// together with the round-robin group key.
///
/// Hostname stage:
/// - Clients whose effective hostname bind equals the request host win.
/// - Otherwise, when `require_hostname_bind` is off, clients without any
///   hostname bind act as the fallback pool. When the flag is on, clients
///   without a hostname bind never receive traffic.
///
/// Path stage (within the hostname pool): longest matching path bind wins;
/// clients without a path bind are the fallback.
pub(crate) fn select_client_pool(
  clients: &HashMap<String, ClientHandle>,
  uri_path: &str,
  request_host: Option<&str>,
  require_hostname_bind: bool,
  down_threshold: Duration,
) -> Option<(Vec<String>, RouteGroupKey)> {
  // --- Eligibility stage: unhealthy, draining, or admin-disabled clients
  // never receive new traffic (in-flight requests still complete) ---
  let eligible: Vec<(&String, &ClientHandle)> = clients
    .iter()
    .filter(|(_, c)| {
      c.is_healthy(down_threshold) && c.backend_healthy && !c.draining && c.admin_enabled
    })
    .collect();

  // --- Hostname stage ---
  let host_matched: Vec<(&String, &ClientHandle)> = match request_host {
    Some(host) => eligible
      .iter()
      .filter(|(_, c)| c.matches_host(host))
      .cloned()
      .collect(),
    None => Vec::new(),
  };

  let (host_pool, host_key): (Vec<(&String, &ClientHandle)>, Option<String>) =
    if !host_matched.is_empty() {
      (host_matched, request_host.map(|h| h.to_string()))
    } else if require_hostname_bind {
      // Strict mode: unbound clients are never eligible.
      return None;
    } else {
      let unbound: Vec<(&String, &ClientHandle)> = eligible
        .iter()
        .filter(|(_, c)| !c.has_hostname_bind())
        .cloned()
        .collect();
      (unbound, None)
    };

  if host_pool.is_empty() {
    return None;
  }

  // --- Path stage ---
  let path_matched: Vec<(&String, &String)> = host_pool
    .iter()
    .filter_map(|(id, c)| {
      c.effective_path_bind()
        .filter(|bind| path_matches_bind(uri_path, bind))
        .map(|bind| (*id, bind))
    })
    .collect();

  let (pool, path_key): (Vec<String>, Option<String>) = if !path_matched.is_empty() {
    // Longest matching bind wins; only clients with that exact bind pool together.
    let longest = path_matched
      .iter()
      .map(|(_, b)| (*b).clone())
      .max_by_key(|b| b.len())
      .unwrap();
    let ids = path_matched
      .iter()
      .filter(|(_, b)| **b == longest)
      .map(|(id, _)| (*id).clone())
      .collect();
    (ids, Some(longest))
  } else {
    let ids: Vec<String> = host_pool
      .iter()
      .filter(|(_, c)| c.effective_path_bind().is_none())
      .map(|(id, _)| (*id).clone())
      .collect();
    (ids, None)
  };

  if pool.is_empty() {
    None
  } else {
    Some((pool, (host_key, path_key)))
  }
}

/// Applies the configured load-balancing strategy to a routed pool.
/// `RoundRobin` keeps the whole pool (the caller's per-group counter rotates
/// through it); `PrimaryStandby` narrows it to the clients sharing the lowest
/// announced priority, so standbys only receive traffic once every
/// more-primary client has dropped out of the pool.
pub(crate) fn apply_lb_strategy(
  pool: Vec<String>,
  clients: &HashMap<String, ClientHandle>,
  strategy: LbStrategy,
) -> Vec<String> {
  match strategy {
    // Sticky affinity is resolved later in pick_proxy_client; the pool
    // itself is built exactly like round-robin.
    LbStrategy::RoundRobin | LbStrategy::Sticky => pool,
    LbStrategy::PrimaryStandby => {
      let min_priority = pool
        .iter()
        .filter_map(|id| clients.get(id))
        .map(|c| c.priority)
        .min()
        .unwrap_or(0);
      pool
        .into_iter()
        .filter(|id| clients.get(id).is_some_and(|c| c.priority == min_priority))
        .collect()
    }
  }
}

/// A dispatch target chosen from the routed pool.
pub(crate) struct SelectedClient {
  pub(crate) id: String,
  pub(crate) tx: mpsc::Sender<Message>,
  pub(crate) request_count: Arc<AtomicU64>,
  pub(crate) inflight_limiter: Option<Arc<Semaphore>>,
  pub(crate) token_name: Option<String>,
  /// Record ID of the dynamic token (None = master); limits key on this.
  pub(crate) token_id: Option<String>,
  /// Client-process instance ID (from Ping); used by failover `wait` mode.
  pub(crate) instance_id: Option<String>,
  /// Tunnel protocol version the client announced (None until known).
  pub(crate) protocol: Option<u32>,
  /// The client opted into the server-side response cache (Ping `cache`).
  pub(crate) cache: bool,
}

/// Returns the pool member matching an affinity value — either a client's
/// self-reported instance ID (survives reconnects) or its connection ID.
pub(crate) fn find_affinity_match(
  pool: &[String],
  clients: &HashMap<String, ClientHandle>,
  affinity: &str,
) -> Option<String> {
  pool
    .iter()
    .find(|id| {
      clients.get(*id).is_some_and(|c| {
        c.reported_instance_id.as_deref() == Some(affinity) || id.as_str() == affinity
      })
    })
    .cloned()
}

/// Picks a client for a request with the full routing pipeline (eligibility →
/// hostname → path → strategy → round-robin). When `require_instance` is
/// given, only clients that reported that instance ID qualify (failover
/// `wait` mode waiting for a specific client process to return). With the
/// sticky strategy, a matching `affinity` cookie value pins the choice to
/// the client that served this visitor before.
pub(crate) async fn pick_proxy_client(
  state: &AppState,
  uri_path: &str,
  request_host: Option<&str>,
  require_instance: Option<&str>,
  affinity: Option<&str>,
) -> Option<SelectedClient> {
  let clients = state.clients.lock().await;
  let (pool, group_key) = select_client_pool(
    &clients,
    uri_path,
    request_host,
    state.config().require_hostname_bind,
    state.config().client_down_threshold,
  )?;
  let mut pool = apply_lb_strategy(pool, &clients, state.config().lb_strategy);
  if let Some(instance) = require_instance {
    pool.retain(|id| {
      clients
        .get(id)
        .is_some_and(|c| c.reported_instance_id.as_deref() == Some(instance))
    });
  }
  if pool.is_empty() {
    return None;
  }

  // Sticky affinity: honor the visitor's cookie when that client is still in
  // the pool; otherwise fall back to rotation (and the response sets a fresh
  // cookie for the newly chosen client).
  let chosen_id = if state.config().lb_strategy == LbStrategy::Sticky
    && let Some(previous) = affinity.and_then(|a| find_affinity_match(&pool, &clients, a))
  {
    previous
  } else {
    let mut rr_map = state.path_rr.lock().await;
    let idx = rr_map.entry(group_key).or_insert(0);
    let chosen = pool[*idx % pool.len()].clone();
    *idx = (*idx + 1) % pool.len();
    chosen
  };

  clients.get(&chosen_id).map(|c| SelectedClient {
    id: chosen_id.clone(),
    tx: c.tx.clone(),
    request_count: c.request_count.clone(),
    inflight_limiter: c.inflight_limiter.clone(),
    token_name: c.perms.token_name.clone(),
    token_id: c.perms.token_id.clone(),
    instance_id: c.reported_instance_id.clone(),
    protocol: c.client_protocol,
    cache: c.cache,
  })
}

/// True when the route for this host/path is served exclusively by clients
/// that declared themselves public (with a token permitting it): the visitor
/// auth gate is skipped. An empty or mixed pool keeps the gate — a request
/// must never leak past auth because one pool member happens to be public.
pub(crate) async fn route_is_public(
  state: &AppState,
  uri_path: &str,
  request_host: Option<&str>,
) -> bool {
  // A traversal segment can widen the matched scope (`/public/../admin`
  // matches a `/public` path bind) and a backend that resolves `..` would then
  // serve the sibling path without the gate — never treat such a path as
  // public; it falls back to the normal login gate.
  if request_path_has_traversal(uri_path) {
    return false;
  }
  let clients = state.clients.lock().await;
  let Some((pool, _)) = select_client_pool(
    &clients,
    uri_path,
    request_host,
    state.config().require_hostname_bind,
    state.config().client_down_threshold,
  ) else {
    return false;
  };
  !pool.is_empty()
    && pool
      .iter()
      .all(|id| clients.get(id).is_some_and(|c| c.public))
}

/// True when visitor `ip` passes every IP allowlist declared by the clients
/// serving this route. A client without a list imposes nothing; when several
/// pool members declare lists, the visitor must pass all of them — a request
/// can never dodge a restriction because another pool member left it open.
/// An empty pool imposes nothing (the request fails with 504 downstream).
pub(crate) async fn route_ip_allowed(
  state: &AppState,
  uri_path: &str,
  request_host: Option<&str>,
  ip: std::net::IpAddr,
) -> bool {
  let clients = state.clients.lock().await;
  let Some((pool, _)) = select_client_pool(
    &clients,
    uri_path,
    request_host,
    state.config().require_hostname_bind,
    state.config().client_down_threshold,
  ) else {
    return true;
  };
  pool.iter().all(|id| {
    clients
      .get(id)
      .is_none_or(|c| c.allowed_ips.is_empty() || crate::auth::ip_allowed(ip, &c.allowed_ips))
  })
}

/// True when `creds` is a well-formed visitor login (`user:password` with both
/// parts non-empty). The password may itself contain `:` (split on the first).
pub(crate) fn valid_visitor_creds(creds: &str) -> bool {
  match creds.split_once(':') {
    Some((user, pass)) => !user.is_empty() && !pass.is_empty(),
    None => false,
  }
}

/// Resolves the client-declared visitor credentials for a route, if any.
///
/// Returns `Some("user:password")` only when the serving pool is non-empty and
/// *every* client in it declares the *same* override — mirroring the "all
/// members must agree" rule of [`route_is_public`], so a request can never be
/// gated by (or leak past) an override that only some pool members set. Returns
/// `None` (use the server's own gate) when the pool is empty, mixed, declares
/// differing credentials, or the server set `APERIO_IGNORE_CLIENT_AUTH`.
pub(crate) async fn route_visitor_auth(
  state: &AppState,
  uri_path: &str,
  request_host: Option<&str>,
) -> Option<String> {
  if state.config().ignore_client_auth {
    return None;
  }
  // Mirror `route_is_public`: a traversal path must not select (or unlock) a
  // client's per-service credentials for a scope it could escape from.
  if request_path_has_traversal(uri_path) {
    return None;
  }
  let clients = state.clients.lock().await;
  let (pool, _) = select_client_pool(
    &clients,
    uri_path,
    request_host,
    state.config().require_hostname_bind,
    state.config().client_down_threshold,
  )?;
  if pool.is_empty() {
    return None;
  }
  let mut creds: Option<&str> = None;
  for id in &pool {
    match clients.get(id).and_then(|c| c.visitor_auth.as_deref()) {
      Some(c) => match creds {
        None => creds = Some(c),
        Some(existing) if existing == c => {}
        // Differing overrides in the same pool: ambiguous, fall back.
        Some(_) => return None,
      },
      // A pool member without an override: not unanimous, fall back.
      None => return None,
    }
  }
  creds.map(str::to_string)
}

/// True when any connected client that could serve this host declares a
/// per-service visitor password. Used for traversal paths, where the matched
/// path scope cannot be trusted: the gate must assume the strictest override
/// present on the host instead of resolving one per path bind.
pub(crate) async fn host_has_visitor_auth(state: &AppState, request_host: Option<&str>) -> bool {
  if state.config().ignore_client_auth {
    return false;
  }
  let clients = state.clients.lock().await;
  clients.values().any(|c| {
    c.visitor_auth.is_some()
      && match request_host {
        Some(h) => c.matches_host(h) || !c.has_hostname_bind(),
        None => !c.has_hostname_bind(),
      }
  })
}

/// Polls the routing pool until a candidate appears or the deadline passes.
pub(crate) async fn wait_for_candidate(
  state: &AppState,
  uri_path: &str,
  request_host: Option<&str>,
  require_instance: Option<&str>,
  deadline: tokio::time::Instant,
) -> Option<SelectedClient> {
  loop {
    if let Some(client) =
      pick_proxy_client(state, uri_path, request_host, require_instance, None).await
    {
      return Some(client);
    }
    if tokio::time::Instant::now() >= deadline {
      return None;
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
  }
}

/// True when in-flight failover may re-dispatch this method: idempotent
/// methods (RFC 9110) are safe to send twice, while POST/PATCH may execute
/// twice on the backend and require the APERIO_FAILOVER_ALL_METHODS opt-in.
pub(crate) fn method_retryable(method: &str, all_methods: bool) -> bool {
  all_methods
    || matches!(
      method,
      "GET" | "HEAD" | "OPTIONS" | "PUT" | "DELETE" | "TRACE"
    )
}

/// Resolves the real client IP, honoring forwarding headers only when
/// `trust_proxy` is enabled (i.e. the server runs behind a trusted reverse
/// proxy). Otherwise the direct socket address is used, since clients could
/// otherwise spoof these headers to bypass rate limiting.
///
/// Two modes (both only under `trust_proxy`):
///
/// **Trusted-proxy chain** (`trusted_proxies` non-empty — the recommended,
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
///    untouched — trusting it implicitly would let clients spoof their IP for
///    rate limiting, audit logs, and token IP allowlists. When the configured
///    header is Cloudflare's and `X-Forwarded-For` starts with a *different*
///    address, an intermediate proxy has rewritten XFF —
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

/// True when `ip` falls inside any of the trusted proxy ranges.
fn is_trusted_proxy(ip: IpAddr, trusted: &[(IpAddr, u32)]) -> bool {
  trusted
    .iter()
    .any(|(base, bits)| crate::auth::cidr_contains(*base, *bits, ip))
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
  // XFF) wins over the walk — but only from a trusted peer, checked above.
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
       with the real client — an intermediate proxy is rewriting X-Forwarded-For to its own peer. \
       Using the Cloudflare header for the client IP; check your reverse-proxy chain so forwarded \
       headers are trusted and preserved (e.g. Traefik forwardedHeaders / nginx real_ip), or set \
       APERIO_REAL_IP_HEADER explicitly."
    );
  }
}

#[cfg(test)]
#[path = "routing_tests.rs"]
mod tests;
