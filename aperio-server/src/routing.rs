use axum::{extract::ws::Message, http::HeaderMap};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
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
  /// Client-process instance ID (from Ping); used by failover `wait` mode.
  pub(crate) instance_id: Option<String>,
  /// Tunnel protocol version the client announced (None until known).
  pub(crate) protocol: Option<u32>,
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
    instance_id: c.reported_instance_id.clone(),
    protocol: c.client_protocol,
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
/// `real_ip_header` (APERIO_REAL_IP_HEADER, e.g. `CF-Connecting-IP`) takes
/// precedence over X-Forwarded-For: intermediate proxies such as Traefik
/// often reset XFF to their immediate peer (a CDN edge), while the CDN's own
/// header still carries the true visitor address.
pub(crate) fn extract_client_ip(
  headers: &HeaderMap,
  fallback: IpAddr,
  trust_proxy: bool,
  real_ip_header: Option<&str>,
) -> IpAddr {
  if trust_proxy {
    if let Some(name) = real_ip_header
      && let Some(value) = headers.get(name)
      && let Ok(value_str) = value.to_str()
      && let Ok(parsed) = value_str.trim().parse::<IpAddr>()
    {
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
  }
  fallback
}
