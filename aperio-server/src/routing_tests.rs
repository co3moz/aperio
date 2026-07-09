use super::*;
use crate::state::ClientPerms;
use axum::http::HeaderMap;
use std::sync::atomic::AtomicU64;

// --- Fixtures ---------------------------------------------------------------

/// A minimally-populated, healthy, master-token client with no binds. Tests
/// mutate the fields they care about.
fn base_handle() -> ClientHandle {
  // The routing functions under test never send on this channel, so the
  // receiver can be dropped immediately.
  let (tx, _rx) = mpsc::channel::<Message>(1);
  ClientHandle {
    tx,
    connected_at: std::time::Instant::now(),
    client_ip: "127.0.0.1".to_string(),
    request_count: Arc::new(AtomicU64::new(0)),
    declared_path: None,
    assigned_path: None,
    declared_hostname: None,
    assigned_hostnames: Vec::new(),
    random_hostname: None,
    override_path_bind: None,
    override_hostname_bind: None,
    last_ping_at: None,
    perms: ClientPerms::master(),
    max_concurrent: None,
    inflight_limiter: None,
    draining: false,
    admin_enabled: true,
    tcp_enabled: false,
    client_version: None,
    client_protocol: None,
    backend_healthy: true,
    priority: 0,
    reported_instance_id: None,
    bandwidth_bps: Arc::new(AtomicU64::new(0)),
    service_name: None,
    public: false,
    public_denied_warned: false,
    visitor_auth: None,
    visitor_auth_denied_warned: false,
    tunnels: Vec::new(),
  }
}

fn pool_of(clients: Vec<(&str, ClientHandle)>) -> HashMap<String, ClientHandle> {
  clients
    .into_iter()
    .map(|(id, h)| (id.to_string(), h))
    .collect()
}

/// Generous threshold: every fresh fixture counts as healthy.
const HEALTHY: Duration = Duration::from_secs(3600);

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

// --- method_retryable -------------------------------------------------------

#[test]
fn method_retryable_rules() {
  for m in ["GET", "HEAD", "OPTIONS", "PUT", "DELETE", "TRACE"] {
    assert!(method_retryable(m, false), "{m} should be retryable");
  }
  // Non-idempotent methods only when the opt-in is set.
  assert!(!method_retryable("POST", false));
  assert!(!method_retryable("PATCH", false));
  assert!(method_retryable("POST", true));
  assert!(method_retryable("PATCH", true));
}

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
    extract_client_ip(&h, ip("1.1.1.1"), false, None),
    ip("1.1.1.1")
  );
}

#[test]
fn client_ip_honors_headers_with_trust() {
  let mut h = HeaderMap::new();
  h.insert("x-forwarded-for", "9.9.9.9, 8.8.8.8".parse().unwrap());
  // First XFF entry wins.
  assert_eq!(
    extract_client_ip(&h, ip("1.1.1.1"), true, None),
    ip("9.9.9.9")
  );

  // x-real-ip is used when XFF is absent.
  let mut r = HeaderMap::new();
  r.insert("x-real-ip", "7.7.7.7".parse().unwrap());
  assert_eq!(
    extract_client_ip(&r, ip("1.1.1.1"), true, None),
    ip("7.7.7.7")
  );
}

#[test]
fn client_ip_custom_header_takes_precedence() {
  let mut h = HeaderMap::new();
  h.insert("cf-connecting-ip", "5.5.5.5".parse().unwrap());
  h.insert("x-forwarded-for", "9.9.9.9".parse().unwrap());
  assert_eq!(
    extract_client_ip(&h, ip("1.1.1.1"), true, Some("cf-connecting-ip")),
    ip("5.5.5.5")
  );
  // A malformed custom header value falls through to XFF.
  let mut bad = HeaderMap::new();
  bad.insert("cf-connecting-ip", "not-an-ip".parse().unwrap());
  bad.insert("x-forwarded-for", "9.9.9.9".parse().unwrap());
  assert_eq!(
    extract_client_ip(&bad, ip("1.1.1.1"), true, Some("cf-connecting-ip")),
    ip("9.9.9.9")
  );
}

#[test]
fn client_ip_cloudflare_auto_detected() {
  // Cloudflare → Traefik: Traefik rewrote XFF down to the Cloudflare edge, but
  // CF-Connecting-IP still carries the true visitor. It is preferred with no
  // real_ip_header configured.
  let mut h = HeaderMap::new();
  h.insert("cf-connecting-ip", "203.0.113.18".parse().unwrap());
  h.insert("x-forwarded-for", "162.158.19.179".parse().unwrap());
  assert_eq!(
    extract_client_ip(&h, ip("1.1.1.1"), true, None),
    ip("203.0.113.18")
  );

  // An explicit real_ip_header still wins over the automatic Cloudflare header.
  let mut both = HeaderMap::new();
  both.insert("cf-connecting-ip", "203.0.113.18".parse().unwrap());
  both.insert("true-client-ip", "7.7.7.7".parse().unwrap());
  assert_eq!(
    extract_client_ip(&both, ip("1.1.1.1"), true, Some("true-client-ip")),
    ip("7.7.7.7")
  );

  // Without trust_proxy the header is ignored entirely.
  assert_eq!(
    extract_client_ip(&h, ip("1.1.1.1"), false, None),
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

// --- select_client_pool -----------------------------------------------------

#[test]
fn pool_prefers_host_matched_clients() {
  let mut bound = base_handle();
  bound.assigned_hostnames = vec!["a.example.com".to_string()];
  let unbound = base_handle();

  let clients = pool_of(vec![("bound", bound), ("unbound", unbound)]);
  let (pool, (host_key, path_key)) =
    select_client_pool(&clients, "/", Some("a.example.com"), false, HEALTHY).unwrap();
  assert_eq!(pool, vec!["bound".to_string()]);
  assert_eq!(host_key, Some("a.example.com".to_string()));
  assert_eq!(path_key, None);
}

#[test]
fn pool_falls_back_to_unbound_when_not_strict() {
  let mut bound = base_handle();
  bound.assigned_hostnames = vec!["a.example.com".to_string()];
  let unbound = base_handle();

  let clients = pool_of(vec![("bound", bound), ("unbound", unbound)]);
  // Request host matches nobody → unbound pool answers when not strict.
  let (pool, (host_key, _)) =
    select_client_pool(&clients, "/", Some("other.example.com"), false, HEALTHY).unwrap();
  assert_eq!(pool, vec!["unbound".to_string()]);
  assert_eq!(host_key, None);
}

#[test]
fn pool_strict_mode_rejects_unbound() {
  let unbound = base_handle();
  let clients = pool_of(vec![("unbound", unbound)]);
  // require_hostname_bind = true and no host match → no route.
  assert!(select_client_pool(&clients, "/", Some("x.example.com"), true, HEALTHY).is_none());
}

#[test]
fn pool_longest_path_bind_wins() {
  let mut api = base_handle();
  api.declared_path = Some("/api".to_string());
  let mut apiv1 = base_handle();
  apiv1.declared_path = Some("/api/v1".to_string());
  let unbound = base_handle();

  let clients = pool_of(vec![("api", api), ("apiv1", apiv1), ("unbound", unbound)]);
  let (pool, (_, path_key)) =
    select_client_pool(&clients, "/api/v1/users", None, false, HEALTHY).unwrap();
  assert_eq!(pool, vec!["apiv1".to_string()]);
  assert_eq!(path_key, Some("/api/v1".to_string()));
}

#[test]
fn pool_ineligible_clients_excluded() {
  let mut draining = base_handle();
  draining.draining = true;
  let mut disabled = base_handle();
  disabled.admin_enabled = false;
  let mut unhealthy_backend = base_handle();
  unhealthy_backend.backend_healthy = false;

  let clients = pool_of(vec![
    ("draining", draining),
    ("disabled", disabled),
    ("unhealthy", unhealthy_backend),
  ]);
  assert!(select_client_pool(&clients, "/", None, false, HEALTHY).is_none());
}

#[test]
fn pool_excludes_stale_clients() {
  let healthy = base_handle();
  let clients = pool_of(vec![("healthy", healthy)]);
  // A zero threshold makes even a just-connected client stale.
  assert!(select_client_pool(&clients, "/", None, false, Duration::ZERO).is_none());
}

// --- apply_lb_strategy ------------------------------------------------------

#[test]
fn lb_round_robin_and_sticky_keep_pool() {
  let clients = pool_of(vec![("a", base_handle()), ("b", base_handle())]);
  let pool = vec!["a".to_string(), "b".to_string()];
  assert_eq!(
    apply_lb_strategy(pool.clone(), &clients, LbStrategy::RoundRobin),
    pool
  );
  assert_eq!(
    apply_lb_strategy(pool.clone(), &clients, LbStrategy::Sticky),
    pool
  );
}

#[test]
fn lb_primary_standby_keeps_lowest_priority() {
  let mut primary = base_handle();
  primary.priority = 0;
  let mut standby = base_handle();
  standby.priority = 5;
  let clients = pool_of(vec![("primary", primary), ("standby", standby)]);

  let pool = vec!["primary".to_string(), "standby".to_string()];
  let narrowed = apply_lb_strategy(pool, &clients, LbStrategy::PrimaryStandby);
  assert_eq!(narrowed, vec!["primary".to_string()]);
}

// --- find_affinity_match ----------------------------------------------------

#[test]
fn affinity_matches_instance_id_then_connection_id() {
  let mut with_instance = base_handle();
  with_instance.reported_instance_id = Some("inst-1".to_string());
  let plain = base_handle();

  let clients = pool_of(vec![("conn-a", with_instance), ("conn-b", plain)]);
  let pool = vec!["conn-a".to_string(), "conn-b".to_string()];

  // Reported instance id wins.
  assert_eq!(
    find_affinity_match(&pool, &clients, "inst-1"),
    Some("conn-a".to_string())
  );
  // Falls back to the connection id.
  assert_eq!(
    find_affinity_match(&pool, &clients, "conn-b"),
    Some("conn-b".to_string())
  );
  // Unknown affinity value.
  assert_eq!(find_affinity_match(&pool, &clients, "nope"), None);
}

// --- ClientHandle routing accessors -----------------------------------------

#[test]
fn effective_path_bind_precedence() {
  let mut h = base_handle();
  h.assigned_path = Some("/granted".to_string());
  assert_eq!(h.effective_path_bind(), Some(&"/granted".to_string()));

  // Declared wins over assigned.
  h.declared_path = Some("/declared".to_string());
  assert_eq!(h.effective_path_bind(), Some(&"/declared".to_string()));

  // Dashboard override wins over everything.
  h.override_path_bind = Some("/override".to_string());
  assert_eq!(h.effective_path_bind(), Some(&"/override".to_string()));
}

#[test]
fn matches_host_uses_override_then_union() {
  let mut h = base_handle();
  h.assigned_hostnames = vec!["a.example.com".to_string()];
  h.declared_hostname = Some("b.example.com".to_string());
  assert!(h.has_hostname_bind());
  assert!(h.matches_host("a.example.com"));
  assert!(h.matches_host("b.example.com"));
  assert!(!h.matches_host("c.example.com"));

  // An override replaces the whole set.
  h.override_hostname_bind = Some("c.example.com".to_string());
  assert!(h.matches_host("c.example.com"));
  assert!(!h.matches_host("a.example.com"));
}

#[test]
fn is_healthy_threshold() {
  let h = base_handle();
  // Just connected: healthy under any positive threshold.
  assert!(h.is_healthy(Duration::from_secs(60)));
  // A zero threshold is never satisfied (elapsed is never < 0).
  assert!(!h.is_healthy(Duration::ZERO));
}

// --- valid_visitor_creds ----------------------------------------------------

#[test]
fn visitor_creds_require_user_and_password() {
  assert!(valid_visitor_creds("user:password"));
  assert!(valid_visitor_creds("u:p"));
  // The password may itself contain ':' (only the first is the separator).
  assert!(valid_visitor_creds("user:pa:ss"));
  // Missing separator or an empty half is rejected.
  assert!(!valid_visitor_creds("userpassword"));
  assert!(!valid_visitor_creds(":password"));
  assert!(!valid_visitor_creds("user:"));
  assert!(!valid_visitor_creds(""));
  assert!(!valid_visitor_creds(":"));
}
