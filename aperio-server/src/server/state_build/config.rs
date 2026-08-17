//! Reading every setting the server has, and folding them into one
//! [`ServerConfig`].
//!
//! One function on purpose, and this is the half where that is genuinely
//! forced: eighty-odd settings are each read from the environment, validated
//! against a neighbour, and written once into the struct literal at the end.
//! Split *within* this pass and the pieces have to hand each other partial
//! configs, which is more moving parts than it organises.
//!
//! What the pass does not have to carry is everything downstream of it. Only
//! the eight values in [`Resolved`] cross into [`super`], because
//! `ServerConfig` absorbs the rest, and that is the seam this file exists on.

use crate::routing::normalize_random_subdomain_pattern;
use crate::settings::{
  FailoverMode, LbStrategy, ServerConfig, parse_failover_mode, parse_lb_strategy,
};
use crate::store::tokens::TokenStore;
use crate::*;
use std::time::Duration;
use tracing::{error, info, warn};

/// What [`resolve`] hands to the caller: the config it built, plus the few
/// things the assembly step also needs and would otherwise re-derive.
pub(crate) struct Resolved {
  pub(crate) config: ServerConfig,
  pub(crate) access_log: Option<tokio::sync::mpsc::Sender<crate::access_log::AccessLogCmd>>,
  pub(crate) admin_key_store: crate::store::admin_keys::AdminKeyStore,
  pub(crate) data_dir: String,
  pub(crate) inbox_store: crate::store::inbox::InboxStore,
  pub(crate) metrics_enabled: bool,
  pub(crate) require_hostname_bind: bool,
  pub(crate) token_store: TokenStore,
}

/// Resolves the whole environment into a [`ServerConfig`]. `None` on any
/// setting the server refuses to start under, each of which logs its own
/// reason first.
pub(crate) fn resolve() -> Option<Resolved> {
  // Enforce APERIO_SERVER_TOKEN environment variable
  let token = std::env::var("APERIO_SERVER_TOKEN").unwrap_or_else(|_| {
    error!("CRITICAL SECURITY ERROR: APERIO_SERVER_TOKEN environment variable must be set!");
    std::process::exit(1);
  });
  if token.trim().is_empty() {
    error!("CRITICAL SECURITY ERROR: APERIO_SERVER_TOKEN cannot be empty!");
    std::process::exit(1);
  }

  let gateway_timeout_secs = std::env::var("APERIO_GATEWAY_TIMEOUT")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .unwrap_or(10);

  let gateway_response_timeout_secs = std::env::var("APERIO_GATEWAY_RESPONSE_TIMEOUT")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .unwrap_or(30);

  // Limit on max request body size (default: 10MB)
  let max_body_size = std::env::var("APERIO_MAX_BODY_SIZE")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(10 * 1024 * 1024);

  // Concurrency limit on tunnel requests (default: 100 concurrent)
  let max_concurrent_requests = std::env::var("APERIO_MAX_CONCURRENT_REQUESTS")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(100);

  // Max concurrently-live proxied public WebSockets. WebSockets are long-lived,
  // so they get their own ceiling separate from the (short-lived) HTTP request
  // limit above; the default is generous enough to never touch normal use while
  // still capping a pathological pile-up. 0 is treated as "no cap".
  let max_ws_connections = std::env::var("APERIO_MAX_WS_CONNECTIONS")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .map(|v| if v == 0 { usize::MAX } else { v })
    .unwrap_or(10_000);

  // Max connected tunnel clients limit (default: 10 active clients)
  let max_tunnels = std::env::var("APERIO_MAX_TUNNELS")
    .ok()
    .and_then(|val| val.parse::<usize>().ok())
    .unwrap_or(10);

  // Parallel connections one client may open for a single service. 16 is what
  // the client used to clamp to on its own, so an unset server keeps exactly
  // the behaviour that was there before this became the server's decision.
  // Both default on: they are what makes the dashboard useful, and a server
  // that is not saturated should not have to know they exist. Same spelling
  // as the other on-by-default flags: `0`/`false` turns one off.
  let opt_out = |key: &str| {
    std::env::var(key)
      .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
      .unwrap_or(true)
  };
  let inspector = opt_out("APERIO_INSPECTOR");
  let access_events = opt_out("APERIO_ACCESS_EVENTS");

  let max_connections_per_service = std::env::var("APERIO_MAX_CONNECTIONS_PER_SERVICE")
    .ok()
    .and_then(|val| val.parse::<u32>().ok())
    .filter(|v| *v > 0)
    .unwrap_or(16);

  // Max IP token bucket capacity burst (default: 100 requests)
  // Only a finite, strictly positive bucket size is meaningful: 0, a negative,
  // NaN or infinity would silently wedge the limiter (never/always throttling),
  // so reject those and fall back to the default, mirroring the dashboard
  // settings validation (`v > 0.0`).
  let ip_limit_max = std::env::var("APERIO_IP_LIMIT_MAX")
    .ok()
    .and_then(|val| val.parse::<f64>().ok())
    .filter(|v| v.is_finite() && *v > 0.0)
    .unwrap_or(100.0);

  // IP token bucket refill rate per second (default: 5.0 requests/sec, which is 300 req/min)
  let ip_limit_refill = std::env::var("APERIO_IP_LIMIT_REFILL")
    .ok()
    .and_then(|val| val.parse::<f64>().ok())
    .filter(|v| v.is_finite() && *v > 0.0)
    .unwrap_or(5.0);

  // The server's default visitor gate. Two spellings reach here: the scalar
  // `user:password` that the environment variable, the dashboard field and
  // every file written before the grammar carry, and the `auth:` block or
  // list a file may write instead (planned_features #105). The block wins
  // where it is present, and `auth_credentials` keeps holding whatever of it
  // the scalar surfaces can still show.
  let visitor_auth_block = visitor_auth::block_from_config_file();
  let visitor_auth = match visitor_auth_block {
    Some(ref block) => visitor_auth::Policy::compile(block),
    None => std::env::var("APERIO_SERVER_AUTH")
      .map(|creds| visitor_auth::Policy::from_credentials(&creds))
      .unwrap_or_default(),
  };

  // Trust proxy headers (X-Forwarded-For / X-Real-IP) for client IP resolution.
  // Only enable when running behind a trusted reverse proxy that overwrites
  // these headers; otherwise clients can spoof them to bypass rate limiting.
  let trust_proxy = std::env::var("APERIO_TRUST_PROXY")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);

  // When enabled, the server ignores any client-declared visitor password
  // override and keeps full control of the visitor gate with its own settings.
  let ignore_client_auth = std::env::var("APERIO_IGNORE_CLIENT_AUTH")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  if ignore_client_auth {
    info!(
      "APERIO_IGNORE_CLIENT_AUTH is set: client-declared visitor password overrides are ignored"
    );
  }

  // Optional real-IP header consulted before X-Forwarded-For (only with
  // trust_proxy). Needed behind CDN → proxy chains where the proxy resets
  // XFF to the CDN edge address, e.g. APERIO_REAL_IP_HEADER=CF-Connecting-IP.
  // APERIO_TRUST_CF_HEADER=1 is shorthand for the common Cloudflare chain: it
  // resolves to APERIO_REAL_IP_HEADER=CF-Connecting-IP (an explicit
  // APERIO_REAL_IP_HEADER still wins). Deliberately opt-in, any visitor can
  // send that header, so trusting it automatically would let clients spoof
  // their IP for rate limiting, audit logs, and token IP allowlists on
  // deployments that are not actually behind Cloudflare.
  let trust_cf_header = std::env::var("APERIO_TRUST_CF_HEADER")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let real_ip_header = std::env::var("APERIO_REAL_IP_HEADER")
    .ok()
    .map(|v| v.trim().to_ascii_lowercase())
    .filter(|v| !v.is_empty())
    .or_else(|| trust_cf_header.then(|| "cf-connecting-ip".to_string()));
  // Trusted proxy/CDN egress ranges (comma-separated IPs/CIDRs). When set,
  // the client IP is resolved by walking the X-Forwarded-For chain from the
  // nearest hop backwards past trusted addresses, the CDN-agnostic model
  // that works for any proxy chain. Implies trust_proxy.
  let trusted_proxies = match std::env::var("APERIO_TRUSTED_PROXIES") {
    Ok(raw) => match crate::routing::parse_trusted_proxies(&raw) {
      Ok(list) => list,
      Err(e) => {
        error!(
          "APERIO_TRUSTED_PROXIES is invalid ({e}); refusing to start with a partial trusted set"
        );
        return None;
      }
    },
    Err(_) => Vec::new(),
  };
  // Source IPs/CIDRs allowed to reach the authenticated admin surface
  // (`/aperio` dashboard + `/aperio/api/*`). Empty = no network restriction.
  let admin_allowed_ips = match std::env::var("APERIO_ADMIN_ALLOWED_IPS") {
    Ok(raw) => match crate::routing::parse_trusted_proxies(&raw) {
      Ok(list) => list,
      Err(e) => {
        error!(
          "APERIO_ADMIN_ALLOWED_IPS is invalid ({e}); refusing to start with a partial allowlist"
        );
        return None;
      }
    },
    Err(_) => Vec::new(),
  };
  if !admin_allowed_ips.is_empty() {
    info!(
      "Admin surface IP allowlist active ({} entries): only matching client IPs may reach the dashboard and its API",
      admin_allowed_ips.len()
    );
  }
  // The deny list is read from the live config document (so it hot-reloads),
  // falling back to the environment. A malformed entry refuses the start
  // rather than applying a partial block list: an operator who wrote a deny
  // list believes those addresses cannot reach the server.
  let denied_ips_config = match std::env::var("APERIO_DENIED_IPS") {
    Ok(raw) if crate::config_file::structured("denied_ips").is_none() && !raw.trim().is_empty() => {
      match crate::deny_list::DenyList::parse(&raw) {
        Ok(list) => list,
        Err(e) => {
          error!("APERIO_DENIED_IPS is invalid ({e}); refusing to start with a partial deny list");
          return None;
        }
      }
    }
    _ => crate::deny_list::from_config(),
  };
  if !denied_ips_config.is_empty() {
    info!(
      "Source IP deny list active ({} entries): matching addresses are refused before anything else",
      denied_ips_config.len()
    );
  }
  let trust_proxy = trust_proxy || !trusted_proxies.is_empty();
  if !trusted_proxies.is_empty() {
    info!(
      "Trusted proxy ranges configured ({} entries): client IPs resolve via the X-Forwarded-For chain walk",
      trusted_proxies.len()
    );
  }
  if let Some(ref h) = real_ip_header {
    if trust_proxy {
      info!("Real client IP is read from the '{}' header", h);
    } else {
      warn!(
        "APERIO_REAL_IP_HEADER / APERIO_TRUST_CF_HEADER is set but APERIO_TRUST_PROXY is off; the header is ignored"
      );
    }
  }

  // When true, session cookies include the `Secure` flag (HTTPS-only).
  // Defaults to `trust_proxy` since a TLS-terminating reverse proxy implies HTTPS.
  let secure_cookies = std::env::var("APERIO_SECURE_COOKIES")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(trust_proxy);

  // When enabled, clients that did not declare a hostname bind (and were not
  // given one via dashboard overrule) are excluded from load balancing.
  let require_hostname_bind = std::env::var("APERIO_REQUIRE_HOSTNAME_BIND")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);

  // What a route nobody gated means. An unreadable value refuses the start
  // rather than falling back: this decides whether a route is open, and
  // guessing at it is the one thing a posture setting must never do.
  let default_access = match std::env::var("APERIO_DEFAULT_ACCESS") {
    Ok(raw) => match settings::parse_default_access(&raw) {
      Some(v) => v,
      None => {
        error!("APERIO_DEFAULT_ACCESS: `{raw}` is not `allow` or `deny`");
        std::process::exit(1);
      }
    },
    // Unset is the posture, and since 0.10.0 the posture is closed: a route
    // is reachable because something said so, rather than because nothing
    // said otherwise. `allow` restores what every server did before.
    Err(_) => settings::DefaultAccess::default(),
  };
  if default_access == settings::DefaultAccess::Deny {
    info!(
      "Closed by default: a proxied route is served only where an `auth:` policy admits the visitor, or where it is declared open (`method: none` / `public: true`)"
    );
  }

  // Prometheus metrics endpoint (default: disabled). Auth is always required:
  // either APERIO_METRICS_TOKEN, or a random token generated once and
  // persisted in the data directory (a truly public metrics endpoint brings
  // no benefit and leaks operational details).
  let metrics_enabled = std::env::var("APERIO_METRICS")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let metrics_token = std::env::var("APERIO_METRICS_TOKEN")
    .ok()
    .filter(|t| !t.trim().is_empty());

  // Autoscaling (default: disabled). A client's `scaling:` block is ignored
  // entirely unless the operator turns the feature on.
  let scaling_enabled = std::env::var("APERIO_SCALING")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let scaling_allow_http = std::env::var("APERIO_SCALING_ALLOW_HTTP")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let scaling_allow_private = std::env::var("APERIO_SCALING_ALLOW_PRIVATE")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let scaling_record_ttl = Duration::from_secs(
    std::env::var("APERIO_SCALING_RECORD_TTL")
      .ok()
      .and_then(|v| v.parse::<u64>().ok())
      .unwrap_or(30 * 24 * 3600),
  );

  // Edge integration (default: disabled). The token is the on/off switch:
  // without it the `/aperio/api/edge/*` routes are not registered at all.
  let edge_token = std::env::var("APERIO_EDGE_TOKEN")
    .ok()
    .filter(|t| !t.trim().is_empty());
  let edge_service_url = std::env::var("APERIO_EDGE_SERVICE_URL")
    .ok()
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty());
  let edge_entrypoints: Vec<String> = std::env::var("APERIO_EDGE_ENTRYPOINTS")
    .unwrap_or_default()
    .split(',')
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty())
    .collect();
  let edge_cert_resolver = std::env::var("APERIO_EDGE_CERT_RESOLVER")
    .ok()
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty());
  let edge_include_offline = std::env::var("APERIO_EDGE_INCLUDE_OFFLINE")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);

  // Server-side GET response cache (default: disabled). Only effective for
  // clients that announce `cache: true`, and strictly Cache-Control-driven.
  let cache_enabled = std::env::var("APERIO_CACHE")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  // Mark random-subdomain (preview) services as non-indexable.
  let preview_noindex = std::env::var("APERIO_PREVIEW_NOINDEX")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let cache_max_bytes = std::env::var("APERIO_CACHE_MAX_BYTES")
    .ok()
    .and_then(|v| v.trim().parse::<u64>().ok())
    .filter(|v| *v > 0)
    .unwrap_or(64 * 1024 * 1024);
  // Serve-stale window for resilient services (#69 semantics): how long an
  // expired cached response may still answer visitors during an outage.
  let cache_max_stale = std::env::var("APERIO_CACHE_MAX_STALE")
    .ok()
    .and_then(|v| v.trim().parse::<u64>().ok())
    .unwrap_or(3600);
  if cache_enabled {
    info!(
      "Response cache is enabled ({} byte budget) for services that opt in with cache: true",
      cache_max_bytes
    );
  }

  // Optional outbound-callback policy (webhooks, autoscaling hooks): an
  // allowlist of host/CIDR patterns and/or a block on private destinations.
  // Empty/off keeps today's permissive behaviour. An invalid entry refuses
  // startup rather than applying a partial allowlist.
  let outbound_policy = {
    let allowlist = match std::env::var("APERIO_OUTBOUND_ALLOWLIST") {
      Ok(raw) => match crate::outbound::parse_patterns(&raw) {
        Ok(list) => list,
        Err(e) => {
          error!(
            "APERIO_OUTBOUND_ALLOWLIST is invalid ({e}); refusing to start with a partial allowlist"
          );
          return None;
        }
      },
      Err(_) => Vec::new(),
    };
    let block_private = std::env::var("APERIO_OUTBOUND_BLOCK_PRIVATE")
      .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
      .unwrap_or(false);
    // Where these calls leave from, which the policy has to know because it
    // changes what the policy can decide. Configured rather than inherited:
    // the environment's `HTTP_PROXY` is no longer read by anything here, so a
    // deployment that proxies its callbacks says so.
    let egress = {
      let proxy = match std::env::var("APERIO_OUTBOUND_PROXY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
      {
        Some(raw) => match aperio_config::egress::EgressProxy::parse(&raw) {
          Ok(proxy) => Some(proxy),
          Err(e) => {
            // Refused rather than ignored, for the same reason a partial
            // allowlist is: a server told to go through a proxy is on a
            // network where going direct does not work, and silently
            // dropping the value produces a failure whose cause is a typo.
            error!("APERIO_OUTBOUND_PROXY is invalid ({e}); refusing to start");
            return None;
          }
        },
        None => None,
      };
      let bypass = aperio_config::egress::EgressBypass::parse(
        &std::env::var("APERIO_OUTBOUND_NO_PROXY").unwrap_or_default(),
      );
      crate::outbound::Egress { proxy, bypass }
    };
    // The machine that was relying on the old behaviour. These used to be read
    // by the HTTP client and are not read by anything now, so a route that
    // worked yesterday would otherwise disappear without a word.
    if egress.proxy.is_none() {
      let stale = crate::outbound::proxy_env_vars();
      if !stale.is_empty() {
        warn!(
          "{} set in the environment, and no longer used: outbound callbacks now go where \
           APERIO_OUTBOUND_PROXY says, and direct when it says nothing. Set it to the same value \
           to keep the route you had.",
          stale.join(", ")
        );
      }
    }
    if let Some(ref proxy) = egress.proxy {
      info!(
        "Outbound callbacks go through the proxy {}{}{}",
        proxy.redacted(),
        if proxy.has_credentials() {
          " (with a credential)"
        } else {
          ""
        },
        if egress.bypass.is_empty() {
          String::new()
        } else {
          format!(", except {} destination(s)", egress.bypass.len())
        }
      );
    }
    let policy = crate::outbound::OutboundPolicy {
      allowlist,
      block_private,
      egress: egress.clone(),
    };
    // Set from the same value the policy holds, in this one place, so the
    // copy that reasons about a destination and the copy that dials it cannot
    // drift apart.
    crate::outbound::set_egress(egress);
    if policy.restricted() {
      info!(
        "Outbound callback policy active: {} allowlist entr{}, block_private={}",
        policy.allowlist.len(),
        if policy.allowlist.len() == 1 {
          "y"
        } else {
          "ies"
        },
        policy.block_private
      );
      // What a proxy takes away, said once and plainly. The policy decides by
      // resolving a destination here and looking at the addresses; through a
      // proxy the name is resolved elsewhere, so that half judges nothing and
      // is not run. An operator who set this believes something is gated, and
      // the honest thing is to name the part that is not.
      if policy.egress.proxy.is_some() {
        let cidrs = policy
          .allowlist
          .iter()
          .filter(|p| matches!(p, crate::outbound::OutboundPattern::Cidr(..)))
          .count();
        warn!(
          "Through the proxy this policy covers what the URL says as text: a literal address, \
           and hostname or *.suffix allowlist entries. It cannot cover a hostname's resolved \
           addresses, because the proxy resolves the name on its own network: \
           block_private={} applies to literal addresses only, and {cidrs} CIDR allowlist \
           entr{} cannot admit a named destination. Destinations on APERIO_OUTBOUND_NO_PROXY are \
           dialed by this server and keep the whole policy.",
          policy.block_private,
          if cidrs == 1 { "y" } else { "ies" }
        );
      }
    }
    policy
  };

  // Per-stream flow-control watermarks (protocol v3). Invalid combinations
  // are repaired by StreamLimits::sanitized with a warning.
  let stream_pause_bytes = std::env::var("APERIO_STREAM_PAUSE_BYTES")
    .ok()
    .and_then(|v| v.trim().parse::<usize>().ok())
    .filter(|v| *v > 0)
    .unwrap_or(crate::state::STREAM_PAUSE_BYTES);
  let stream_resume_bytes = std::env::var("APERIO_STREAM_RESUME_BYTES")
    .ok()
    .and_then(|v| v.trim().parse::<usize>().ok())
    .filter(|v| *v > 0)
    .unwrap_or(crate::state::STREAM_RESUME_BYTES);
  let stream_min_throughput = std::env::var("APERIO_STREAM_MIN_THROUGHPUT")
    .ok()
    .and_then(|v| v.trim().parse::<u64>().ok())
    .unwrap_or(0);
  let stream_backlog_limit = std::env::var("APERIO_STREAM_BACKLOG_LIMIT")
    .ok()
    .and_then(|v| v.trim().parse::<usize>().ok())
    .filter(|v| *v > 0)
    .unwrap_or(crate::state::STREAM_BACKLOG_LIMIT);

  // Tunnel frame compression (zlib). Offered to clients on connect; enabled
  // per connection once the client acknowledges support.
  let tunnel_compression = std::env::var("APERIO_TUNNEL_COMPRESSION")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  if tunnel_compression {
    info!("Tunnel compression is enabled (zlib per-message)");
  }

  // Optional custom 504 error page (e.g. APERIO_504_PAGE=/app/error_504.html).
  // Loaded once at startup; on read failure the default plain-text 504 is kept.
  let custom_504_page =
    std::env::var("APERIO_504_PAGE")
      .ok()
      .and_then(|path| match std::fs::read_to_string(&path) {
        Ok(html) => {
          info!("Custom 504 page loaded from {}", path);
          Some(html)
        }
        Err(e) => {
          error!(
            "Failed to read APERIO_504_PAGE {}: {}, using default 504 text",
            path, e
          );
          None
        }
      });

  // Structured access log: APERIO_ACCESS_LOG=<path> appends one JSON line
  // per proxied request to the file (in addition to the structured
  // aperio_access tracing events that always go to stdout).
  let access_log_configured = std::env::var("APERIO_ACCESS_LOG")
    .ok()
    .map(|p| p.trim().to_string())
    .filter(|p| !p.is_empty());
  let access_log = access_log_configured.as_ref().and_then(|path| {
    match std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(path)
    {
      Ok(file) => {
        info!("Structured access log enabled: {}", path);
        // One writer task owns the file from here on: the request path
        // queues lines instead of taking a process-wide mutex around a
        // synchronous write.
        Some(crate::access_log::spawn_writer(path.clone(), file))
      }
      Err(e) => {
        error!(
          "Failed to open APERIO_ACCESS_LOG {}: {}, access log file disabled",
          path, e
        );
        None
      }
    }
  });

  // Optional custom maintenance page (APERIO_503_PAGE=/app/maintenance.html).
  let custom_503_page =
    std::env::var("APERIO_503_PAGE")
      .ok()
      .and_then(|path| match std::fs::read_to_string(&path) {
        Ok(html) => {
          info!("Custom 503 maintenance page loaded from {}", path);
          Some(html)
        }
        Err(e) => {
          error!(
            "Failed to read APERIO_503_PAGE {}: {}, using default 503 text",
            path, e
          );
          None
        }
      });

  // Load-balancing strategy applied after routing narrows the pool.
  let lb_strategy_raw = std::env::var("APERIO_LB_STRATEGY").unwrap_or_default();
  let lb_strategy = parse_lb_strategy(&lb_strategy_raw).unwrap_or_else(|| {
    warn!(
      "Unknown APERIO_LB_STRATEGY '{}' (expected 'round-robin', 'primary-standby' or 'sticky'); using round-robin",
      lb_strategy_raw
    );
    LbStrategy::RoundRobin
  });
  if lb_strategy != LbStrategy::RoundRobin {
    info!("Load balancing strategy: {:?}", lb_strategy);
  }

  // In-flight failover: what to do when a client dies mid-request.
  let failover_raw = std::env::var("APERIO_FAILOVER").unwrap_or_default();
  let failover_mode = parse_failover_mode(&failover_raw).unwrap_or_else(|| {
    warn!(
      "Unknown APERIO_FAILOVER '{}' (expected 'fail', 'retry', 'wait' or 'retry-wait'); using fail",
      failover_raw
    );
    FailoverMode::Fail
  });
  let failover_max_jumps = std::env::var("APERIO_FAILOVER_MAX_JUMPS")
    .ok()
    .and_then(|val| val.parse::<u32>().ok())
    .unwrap_or(2);
  let failover_window = Duration::from_secs(
    std::env::var("APERIO_FAILOVER_WINDOW")
      .ok()
      .and_then(|val| val.parse::<u64>().ok())
      .unwrap_or(15),
  );
  let failover_all_methods = std::env::var("APERIO_FAILOVER_ALL_METHODS")
    .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  if failover_mode != FailoverMode::Fail {
    info!(
      "In-flight failover enabled: {:?} (max {} jumps, {}s window{})",
      failover_mode,
      failover_max_jumps,
      failover_window.as_secs(),
      if failover_all_methods {
        ", all methods"
      } else {
        ", idempotent methods only"
      }
    );
  }

  // Heartbeat-based health: clients whose last Ping is older than this many
  // seconds are treated as down and excluded from load balancing.
  let client_down_threshold_secs = std::env::var("APERIO_CLIENT_DOWN_THRESHOLD")
    .ok()
    .and_then(|val| val.parse::<u64>().ok())
    .filter(|n| *n > 0)
    .unwrap_or(15);

  // Random subdomain assignment: APERIO_RANDOM_SUBDOMAIN="*.example.com"
  // gives every connecting client a random hostname under that suffix.
  let random_subdomain_suffix = std::env::var("APERIO_RANDOM_SUBDOMAIN")
    .ok()
    .and_then(|val| {
      match normalize_random_subdomain_pattern(&val) {
        Some(s) => Some(s),
        None => {
          error!(
            "Invalid APERIO_RANDOM_SUBDOMAIN value '{}' (expected e.g. \"example.com\", \"*.example.com\", or \"*-test.example.com\"); ignoring",
            val
          );
          None
        }
      }
    });
  if let Some(ref pattern) = random_subdomain_suffix {
    info!(
      "Random subdomain assignment enabled: every client gets {} (* = random label)",
      pattern
    );
  }

  // Data directory for persisted state (dynamic tokens, etc.). In Docker,
  // mount a volume here (e.g. ./data:/app/data) so tokens survive restarts.
  let data_dir = std::env::var("APERIO_DATA_DIR").unwrap_or_else(|_| "./data".to_string());
  let token_store = TokenStore::load(&data_dir);
  let admin_key_store = crate::store::admin_keys::AdminKeyStore::load(&data_dir);
  let inbox_store = crate::store::inbox::InboxStore::load(&data_dir);

  // Resolve the effective metrics token: env var wins; otherwise generate a
  // random token once and persist it so every restart uses the same value.
  let metrics_token = if metrics_enabled && metrics_token.is_none() {
    let path = std::path::Path::new(&data_dir).join("metrics_token");
    let persisted = std::fs::read_to_string(&path)
      .ok()
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty());
    match persisted {
      Some(tok) => {
        warn!(
          "APERIO_METRICS_TOKEN not set; using the persisted random metrics token from {:?}. \
           Scrape with /aperio/metrics?token=<token> or an Authorization: Bearer header.",
          path
        );
        Some(tok)
      }
      None => {
        let tok = format!("mtr_{}", uuid::Uuid::new_v4().simple());
        if let Err(e) = std::fs::write(&path, &tok) {
          error!(
            "Failed to persist generated metrics token to {:?}: {}",
            path, e
          );
        }
        warn!(
          "APERIO_METRICS_TOKEN not set; generated a random metrics token: {} (persisted in {:?}). \
           Scrape with /aperio/metrics?token=<token>. This value is logged only on first generation.",
          tok, path
        );
        Some(tok)
      }
    }
  } else {
    metrics_token
  };

  let config = ServerConfig {
    token: token.clone(),
    gateway_timeout: Duration::from_secs(gateway_timeout_secs),
    gateway_response_timeout: Duration::from_secs(gateway_response_timeout_secs),
    max_body_size,
    max_tunnels,
    max_connections_per_service,
    inspector,
    access_events,
    ip_limit_max,
    ip_limit_refill,
    visitor_auth_block,
    visitor_auth,
    trust_proxy,
    ignore_client_auth,
    real_ip_header,
    trusted_proxies,
    admin_allowed_ips,
    secure_cookies,
    outbound_policy,
    require_hostname_bind,
    default_access,
    metrics_token,
    scaling_enabled,
    scaling_allow_http,
    scaling_allow_private,
    scaling_record_ttl,
    edge_token,
    edge_service_url,
    edge_entrypoints,
    edge_cert_resolver,
    edge_include_offline,
    random_subdomain_suffix,
    client_down_threshold: Duration::from_secs(client_down_threshold_secs),
    tunnel_compression,
    custom_504_page,
    custom_503_page,
    lb_strategy,
    failover_mode,
    failover_max_jumps,
    failover_window,
    failover_all_methods,
    retry_on_5xx: std::env::var("APERIO_RETRY_ON_5XX")
      .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
      .unwrap_or(false),
    retry_statuses: std::env::var("APERIO_RETRY_STATUSES")
      .ok()
      .map(|raw| {
        raw
          .split(',')
          .filter_map(|s| s.trim().parse::<u16>().ok())
          .collect()
      })
      .unwrap_or_default(),
    outlier_ejection: std::env::var("APERIO_OUTLIER_EJECTION")
      .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
      .unwrap_or(false),
    outlier_max_failures: std::env::var("APERIO_OUTLIER_MAX_FAILURES")
      .ok()
      .and_then(|v| v.trim().parse::<u32>().ok())
      .filter(|n| *n > 0)
      .unwrap_or(5),
    outlier_window: Duration::from_secs(
      std::env::var("APERIO_OUTLIER_WINDOW")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(30),
    ),
    outlier_eject: Duration::from_secs(
      std::env::var("APERIO_OUTLIER_EJECT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(30),
    ),
    cache_enabled,
    cache_max_bytes,
    cache_max_stale,
    stream_min_throughput,
    stream_pause_bytes,
    stream_resume_bytes,
    stream_backlog_limit,
    max_concurrent_requests,
    max_ws_connections,
    login_lockout_threshold: std::env::var("APERIO_LOGIN_LOCKOUT_THRESHOLD")
      .ok()
      .and_then(|v| v.parse::<u32>().ok())
      .unwrap_or(5),
    login_lockout_secs: std::env::var("APERIO_LOGIN_LOCKOUT_SECS")
      .ok()
      .and_then(|v| v.parse::<u64>().ok())
      .unwrap_or(60),
    // Audit log rotation: the active audit.jsonl is rotated once it exceeds
    // this size in bytes (0 = never rotate), keeping the configured number
    // of older generations (audit.jsonl.1 ..).
    audit_max_size: std::env::var("APERIO_AUDIT_MAX_SIZE")
      .ok()
      .and_then(|v| v.parse::<u64>().ok())
      .unwrap_or(10 * 1024 * 1024),
    audit_max_files: std::env::var("APERIO_AUDIT_MAX_FILES")
      .ok()
      .and_then(|v| v.parse::<usize>().ok())
      .unwrap_or(3),
    ui_language: std::env::var("APERIO_UI_LANGUAGE")
      .ok()
      .map(|v| v.trim().to_ascii_lowercase())
      .filter(|v| crate::settings::UI_LANGUAGES.contains(&v.as_str()))
      .unwrap_or_else(|| "en".to_string()),
    header_rules: headers::from_config_file(),
    static_routes: static_routes::from_config_file(),
    error_pages: error_pages::from_config_file(),
    route_limits: route_limits::from_config_file(),
    waf: waf::from_config_file(),
    alert_rules: alert_rules::from_config_file(),
    maintenance_windows: maintenance_windows::from_config_file(),
    denied_ips: denied_ips_config,
    alternate_servers: crate::tunnel::ws::parse_alternates(
      &std::env::var("APERIO_ALTERNATE_SERVERS").unwrap_or_default(),
    ),
    max_streams_per_ip: std::env::var("APERIO_MAX_STREAMS_PER_IP")
      .ok()
      .and_then(|v| v.trim().parse::<u32>().ok())
      .unwrap_or(0),
    otel_bridge: std::env::var("APERIO_OTEL_BRIDGE")
      .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
      .unwrap_or(false),
    shutdown_drain: {
      let raw = std::env::var("APERIO_SHUTDOWN_DRAIN").unwrap_or_default();
      raw.trim().parse::<u64>().ok()
    },
    shutdown_drain_auto: std::env::var("APERIO_SHUTDOWN_DRAIN")
      .map(|v| v.trim().eq_ignore_ascii_case("auto"))
      .unwrap_or(false),
    shutdown_timeout: std::env::var("APERIO_SHUTDOWN_TIMEOUT")
      .ok()
      .and_then(|v| v.trim().parse::<u64>().ok())
      .filter(|v| *v > 0)
      .unwrap_or(10),
    access_log_sample_rate: std::env::var("APERIO_ACCESS_LOG_SAMPLE_RATE")
      .ok()
      .and_then(|v| v.trim().parse::<f64>().ok())
      .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
      .unwrap_or(1.0),
    identity_headers: std::env::var("APERIO_IDENTITY_HEADERS")
      .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
      .unwrap_or(false),
    visitor_identity_headers: std::env::var("APERIO_VISITOR_IDENTITY_HEADERS")
      .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
      .unwrap_or(false),
    request_id_enabled: std::env::var("APERIO_REQUEST_ID")
      .map(|v| !matches!(v.trim(), "0" | "false" | "no"))
      .unwrap_or(true),
    request_id_header: std::env::var("APERIO_REQUEST_ID_HEADER")
      .ok()
      .map(|v| v.trim().to_ascii_lowercase())
      .filter(|v| !v.is_empty() && v.parse::<axum::http::HeaderName>().is_ok())
      .unwrap_or_else(|| "x-request-id".to_string()),
    request_id_trust_inbound: std::env::var("APERIO_REQUEST_ID_TRUST_INBOUND")
      .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
      .unwrap_or(false),
    fallbacks: fallbacks::from_config_file(),
    token_pinning: std::env::var("APERIO_TOKEN_PINNING")
      .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
      .unwrap_or(false),
    preview_noindex,
  };

  Some(Resolved {
    config,
    access_log,
    admin_key_store,
    data_dir,
    inbox_store,
    metrics_enabled,
    require_hostname_bind,
    token_store,
  })
}
