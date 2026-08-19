//! How a request finds a service: bind normalization and the refusals that go with
//! it, path matching at segment boundaries, traversal detection in both literal and
//! encoded form, hostname extraction, and client-IP trust.

use super::*;
use crate::state::ClientPerms;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

// --- Fixtures ---------------------------------------------------------------

/// A minimally-populated, healthy, master-token client with no binds. Tests
/// mutate the fields they care about.
pub(super) fn base_handle() -> ClientHandle {
  // The routing functions under test never send on this channel, so the
  // receiver can be dropped immediately.
  let (tx, _rx) = mpsc::channel::<Message>(1);
  ClientHandle {
    declared_name: None,
    tx,
    disconnect: std::sync::Arc::new(tokio::sync::Notify::new()),
    connected_at: std::time::Instant::now(),
    client_ip: "127.0.0.1".to_string(),
    declared_client_id: None,
    drain_secs: None,
    last_ping_at: None,
    perms: ClientPerms::master(),
    draining: false,
    client_version: None,
    client_protocol: None,
    cpu_percent: None,
    rss_bytes: None,
    rtt_ms: None,
    jitter_ms: None,
    reconnects: None,
    reported_instance_id: None,
    instance_group: None,
    subscriptions: Vec::new(),
    services: vec![crate::state::ServiceState {
      server_side_refused: None,
      server_side_target: None,
      service_custom_name: None,
      request_count: Arc::new(AtomicU64::new(0)),
      declared_path: None,
      assigned_path: None,
      declared_hostname: None,
      declared_hostnames: Vec::new(),
      assigned_hostnames: Vec::new(),
      random_hostname: None,
      override_path_bind: None,
      override_hostname_binds: Vec::new(),
      capture: true,
      connections: None,
      connections_min: None,
      connections_max: None,
      config_notes: Vec::new(),
      metrics_labels: Vec::new(),
      max_concurrent: None,
      max_concurrent_ceiling: None,
      inflight_limiter: None,
      admin_enabled: true,
      tcp_enabled: false,
      backend_healthy: true,
      backend_probed: true,
      priority: 0,
      bandwidth_bps: Arc::new(AtomicU64::new(0)),
      service_name: None,
      public: false,
      public_denied_warned: false,
      visitor_auth: None,
      visitor_auth_policy: None,
      visitor_auth_denied_warned: false,
      ungated_warned: false,
      allowed_ips: Vec::new(),
      allowed_ips_invalid_warned: false,
      scaling_invalid_warned: false,
      tunnels: Vec::new(),
      cache: false,
      cache_ignored_warned: false,
      resilience: false,
      max_request_body: None,
      response_timeout: None,
      webhook_inbox: false,
      denied: None,
      recent_failures: VecDeque::new(),
      ejected_until: None,
    }],
  }
}

pub(super) fn pool_of(clients: Vec<(&str, ClientHandle)>) -> HashMap<String, ClientHandle> {
  clients
    .into_iter()
    .map(|(id, h)| (id.to_string(), h))
    .collect()
}

/// Generous threshold: every fresh fixture counts as healthy.
pub(super) const HEALTHY: Duration = Duration::from_secs(3600);
