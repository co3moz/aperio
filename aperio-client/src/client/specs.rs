//! Turning the layered configuration into the specs the service tasks run.
//!
//! This is where a file stops being text and becomes a decision, so it is also
//! where a bad file has to be refused: every `CRITICAL ERROR` here is a
//! configuration that would otherwise have failed at runtime, on one service,
//! with the cause a reconnect loop away from the symptom.

use tracing::{info, warn};

use crate::config::ClientSettings;
use crate::protocol::ConfigNote;
use crate::service::{self, ServiceSpec};
use crate::*;

/// Resolves a service's visitor gate into the two things the tunnel handshake
/// carries: whether the service declares itself open, and the one
/// `user:password` its `visitor_auth` field can hold.
///
/// The handshake predates the `auth:` grammar, so a policy saying more than
/// those two can express is refused here rather than sent in a shape the
/// server would read as *weaker* than what was written. A gate that quietly
/// becomes a different gate is the failure this whole area exists to avoid,
/// and it is why the check is on this side: the client knows what it meant.
/// (planned_features #105; the wire grows when a method that needs it lands.)
///
/// `method: none` is the deliberate open gate, which the handshake already
/// spells as `public`, so it resolves to that and inherits its token
/// permission rather than becoming a second way to say the same thing.
///
/// A policy the scalar cannot carry is no longer refused here: it travels as
/// the full policy, and whether the server on the other end understands it is
/// a question only the handshake can answer (#111).
pub(crate) fn resolve_visitor_gate(
  label: &str,
  policy: Option<&aperio_config::AuthSetting>,
  public: bool,
) -> Result<(bool, Option<String>), String> {
  let Some(policy) = policy else {
    return Ok((public, None));
  };
  aperio_config::validate_auth_setting(policy).map_err(|why| format!("{label}: {why}"))?;
  if policy
    .methods()
    .iter()
    .all(|m| m.method.trim().eq_ignore_ascii_case("none"))
  {
    return Ok((true, None));
  }
  // Anything richer travels as the full policy, and whether it may travel at
  // all is settled per connection against what the server announced
  // (planned_features #111), not here: this function knows the file, and only
  // the handshake knows the server.
  Ok((public, policy.as_single_credential().map(str::to_string)))
}

/// Validates the resolved settings and builds the runnable service specs.
///
/// Single-service mode uses the top-level `target`; a non-empty `services:`
/// list in the local config file expands to one spec per entry, with unset
/// per-entry knobs falling back to the top-level resolved values. A CLI
/// positional target always wins and forces single-service mode. Returns an
/// error message (used verbatim in logs) when a required value is missing or
/// invalid.
pub(crate) fn build_specs(
  settings: &ClientSettings,
  client_id_base: &str,
  cli_target_given: bool,
) -> Result<Vec<ServiceSpec>, String> {
  let token = settings
    .token
    .clone()
    .filter(|t| !t.trim().is_empty())
    .ok_or(
      "CRITICAL SECURITY ERROR: a tunnel token is required (--server-token, APERIO_SERVER_TOKEN, or yaml: server.token)!",
    )?;
  let server_addr = settings.server.clone().ok_or(
    "CRITICAL ERROR: the server URL is required (--server-url, APERIO_SERVER_URL, or yaml: server.url)!",
  )?;
  let ws_url =
    build_ws_url(&server_addr).map_err(|e| format!("Failed to build WebSocket URL: {}", e))?;

  // Additional server URLs for cross-server failover (APERIO_SERVER_URLS,
  // comma-separated). The primary (server_addr) is always the first candidate;
  // the reconnect loop rotates to the next after a failed connection.
  let mut ws_urls = vec![ws_url.clone()];
  for extra in &settings.server_urls {
    match build_ws_url(extra) {
      Ok(u) if !ws_urls.contains(&u) => ws_urls.push(u),
      Ok(_) => {}
      Err(e) => tracing::warn!(
        "Ignoring invalid entry '{}' in server.urls / APERIO_SERVER_URLS: {}",
        extra,
        e
      ),
    }
  }
  if ws_urls.len() > 1 {
    info!(
      "Cross-server failover across {} servers configured",
      ws_urls.len()
    );
  }

  let parse_bw = |raw: Option<&str>| {
    raw.and_then(|raw| {
      let parsed = parse_bandwidth(raw);
      if parsed.is_none() {
        warn!("Invalid bandwidth value '{}'; ignoring", raw);
      }
      parsed
    })
  };
  // The top-level value is a budget for the whole client process, not a
  // per-service default: `allocate_bandwidth` divides it up once every spec
  // is built.
  let budget_bps = parse_bw(settings.bandwidth.as_deref());

  let tunnels = validate_tunnels(&settings.tunnels)?;

  // Visitor allowlists fail at startup, not silently on the server.
  let validate_ips = |ips: &[String], what: &str| -> Result<(), String> {
    for entry in ips {
      if !crate::config::valid_ip_entry(entry) {
        return Err(format!(
          "CRITICAL ERROR: {} has an invalid allowed_ips entry '{}'; expected an IP, a CIDR range, or '*'",
          what, entry
        ));
      }
    }
    Ok(())
  };
  validate_ips(&settings.allowed_ips, "the client configuration")?;
  for entry in &settings.services {
    if let Some(ips) = &entry.allowed_ips {
      validate_ips(
        ips,
        &format!(
          "service '{}'",
          entry.name.clone().unwrap_or_else(|| "?".into())
        ),
      )?;
    }
  }

  // Unix socket targets: must carry a path, and only exist on Unix platforms.
  let validate_unix_target = |target: &str, what: &str| -> Result<(), String> {
    if !crate::proxy::unix::is_unix_target(target) {
      return Ok(());
    }
    if cfg!(not(unix)) {
      return Err(format!(
        "CRITICAL ERROR: {} uses a unix:// target, which is not supported on this platform",
        what
      ));
    }
    if crate::proxy::unix::unix_socket_path(target).is_none() {
      return Err(format!(
        "CRITICAL ERROR: {} has a unix:// target without a socket path (expected e.g. unix:///var/run/app.sock)",
        what
      ));
    }
    Ok(())
  };
  if let Some(t) = &settings.target {
    validate_unix_target(t, "the client configuration")?;
  }
  for entry in &settings.services {
    if let Some(t) = &entry.target {
      validate_unix_target(
        t,
        &format!(
          "service '{}'",
          entry.name.clone().unwrap_or_else(|| "?".into())
        ),
      )?;
    }
  }

  // Denied-visitor redirects must be absolute http(s) URLs, anything else
  // would silently degrade to stealth on the server.
  let validate_denied = |denied: Option<&String>, what: &str| -> Result<(), String> {
    if let Some(url) = denied {
      let ok =
        (url.starts_with("http://") || url.starts_with("https://")) && url::Url::parse(url).is_ok();
      if !ok {
        return Err(format!(
          "CRITICAL ERROR: {} has an invalid denied: value '{}'; expected an absolute http(s) URL",
          what, url
        ));
      }
    }
    Ok(())
  };
  validate_denied(settings.denied.as_ref(), "the client configuration")?;
  for entry in &settings.services {
    validate_denied(
      entry.denied.as_ref(),
      &format!(
        "service '{}'",
        entry.name.clone().unwrap_or_else(|| "?".into())
      ),
    )?;
  }

  // Parallel connections per service: bounded so a typo cannot exhaust the
  // server's tunnel slots (it also has its own max_tunnels guard). Defaults to
  // 1; set `connections: N` (or APERIO_CONNECTIONS) to run N parallel
  // connections so a single dropped one (e.g. a CDN recycling a long-lived
  // WebSocket) is covered by a sibling with no visitor-facing gap.
  // A sanity bound, not a policy. The policy is the server's
  // `max_connections_per_service` (lowered further by a token's own
  // `max_connections`), announced on the handshake and applied per connection;
  // this only stops `connections: 100000` from spawning a hundred thousand
  // tasks before the first one has connected to find out.
  const CONNECTIONS_SANITY_BOUND: u32 = 256;
  // Returns (floor, ceiling): equal for a fixed count, a range for an elastic
  // pool.
  let clamp_connections = |raw: Option<&aperio_config::Connections>, what: &str| -> (u32, u32) {
    let (min, max) = match raw {
      None => (1, 1),
      Some(c) => (c.min(), c.max()),
    };
    if max > CONNECTIONS_SANITY_BOUND {
      warn!(
        "{} requests {} connections; clamping to {}. The server decides the real \
         ceiling and announces it on connect.",
        what, max, CONNECTIONS_SANITY_BOUND
      );
      return (min.min(CONNECTIONS_SANITY_BOUND), CONNECTIONS_SANITY_BOUND);
    }
    (min, max)
  };
  // A clamped count is announced so the dashboard shows what the config asked
  // for next to what the client is really running.
  let connections_note =
    |raw: Option<&aperio_config::Connections>, effective: u32| -> Vec<ConfigNote> {
      let asked = raw.map(|c| c.max()).unwrap_or(1).max(1);
      (asked != effective)
        .then(|| ConfigNote {
          field: "connections".to_string(),
          declared: asked.to_string(),
          effective: effective.to_string(),
          reason: format!("clamped to {CONNECTIONS_SANITY_BOUND} parallel connections"),
        })
        .into_iter()
        .collect()
    };

  if !settings.services.is_empty() && !cli_target_given {
    // The services: list wins, and the keys that describe a single service at
    // the top level are read by nothing. Saying so is the point: they were
    // written to have an effect, they have none, and until now the client
    // dropped them without a word.
    let shadowed: Vec<&str> = [
      ("target", settings.target.is_some()),
      ("serve", settings.serve.is_some()),
      ("hostname", !settings.hostnames.is_empty()),
      ("path", settings.path.is_some()),
      ("tcp_target", settings.tcp_target.is_some()),
      ("target_health", settings.target_health.is_some()),
    ]
    .into_iter()
    .filter(|(_, set)| *set)
    .map(|(key, _)| key)
    .collect();
    if !shadowed.is_empty() {
      warn!(
        "Ignoring the top-level `{}`: a services: list is present and it is what runs. Move them into the entry they belong to.",
        shadowed.join("`, `")
      );
    }
  }

  if settings.services.is_empty() || cli_target_given {
    if cli_target_given && !settings.services.is_empty() {
      warn!(
        "A positional target was given on the command line; ignoring the {} entry/entries of the services: list",
        settings.services.len()
      );
    }
    // A client may run with nothing exposed at all. The connection then
    // serves no HTTP target and exists purely to carry something else: the
    // `tunnels:` a peer binds in an emergency, or the messages this client
    // publishes and subscribes to. Refusing to start would mean a
    // publish-only client had to invent a service it does not have.
    let carries_messages = !settings.subscribe.is_empty()
      || settings.messages_listen.is_some()
      || settings.messages_mqtt_listen.is_some();
    let target = match settings.target.clone() {
      Some(t) => t,
      None if !tunnels.is_empty() || carries_messages => String::new(),
      None => {
        return Err(
          "CRITICAL ERROR: there is nothing for this client to do (give a positional target, APERIO_TARGET, or a services:/tunnels:/subscribe: list)!".to_string(),
        );
      }
    };
    let (connections_min, connections) =
      clamp_connections(settings.connections.as_ref(), "the service");
    let (top_public, top_visitor_auth) =
      resolve_visitor_gate("auth", settings.visitor_auth.as_ref(), settings.public)?;
    let mut specs = vec![ServiceSpec {
      name: None,
      custom_name: settings.custom_name.clone(),
      client_id: client_id_base.to_string(),
      token,
      instance_group: client_id_base.to_string(),
      server_addr,
      ws_url,
      ws_urls: ws_urls.clone(),
      // Not offered on the single-service shape: `server_side:` is a
      // `services:` key, and the top-level spellings are the deprecated form
      // this project is retiring, so a new feature does not grow one.
      server_side_target: None,
      target,
      hostnames: settings.hostnames.clone(),
      path: settings.path.clone(),
      trim_bind: if settings.path.is_some() {
        settings.trim_bind.unwrap_or(true)
      } else {
        false
      },
      pass_hostname: settings.pass_hostname,
      max_response_body: settings.max_response_body,
      reload_drain_secs: settings.reload_drain_secs,
      retry_attempts: settings.retry_attempts,
      retry_backoff_ms: settings.retry_backoff_ms,
      retry_all_methods: settings.retry_all_methods,
      breaker_failures: settings.breaker_failures,
      breaker_open_for_secs: settings.breaker_open_for_secs,
      max_request_body: settings.max_request_body,
      response_timeout: settings.response_timeout,
      timeout_secs: settings.timeout_secs,
      max_concurrent: settings.max_concurrent,
      adaptive_concurrency: settings.adaptive_concurrency,
      connections,
      connections_min,
      // A single service has nobody to share a connection with, so the flag
      // has nothing to do here whichever way it is set.
      multiplex: false,
      multiplex_group: None,
      metrics_labels: settings.metrics_labels.clone(),
      startup_delay: settings.startup_delay.unwrap_or(0),
      // A single service has nothing in the same file to depend on.
      depends_on: Vec::new(),
      connect_timeout: settings.connect_timeout,
      min_tls_version: settings.min_tls_version.clone(),
      pool_load: std::sync::Arc::new(service::PoolLoad::default()),
      priority: settings.priority,
      bandwidth_bps: budget_bps,
      bandwidth_declared: settings.bandwidth.clone(),
      config_notes: connections_note(settings.connections.as_ref(), connections),
      max_message_size: settings.max_message_size,
      max_redirects: settings.max_redirects,
      tcp_target: settings.tcp_target.clone(),
      target_health: settings.target_health.clone(),
      wait_for_backend: settings.wait_for_backend,
      health_interval: settings.health_interval,
      health_timeout: settings.health_timeout,
      health_threshold: settings.health_threshold,
      public: top_public,
      visitor_auth: top_visitor_auth,
      visitor_auth_policy: settings.visitor_auth.clone(),
      allowed_ips: settings.allowed_ips.clone(),
      resilience: settings.resilience,
      capture: settings.capture,
      webhook_inbox: settings.webhook_inbox,
      denied: settings.denied.clone(),
      scaling: settings.scaling.clone(),
      tunnels,
      headers: crate::config::merge_security_headers(
        settings.headers.clone(),
        settings.security_headers.as_ref(),
      ),
      cache: settings.cache,
    }];
    validate_tls_floors(&specs)?;
    allocate_bandwidth(&mut specs, budget_bps);
    return Ok(specs);
  }

  // Multi-service mode: one spec (and one tunnel connection) per entry.
  // Binds (hostname/path/tcp_target/target_health) are strictly per entry;
  // tuning knobs fall back to the top-level resolved values.
  let mut specs: Vec<ServiceSpec> = settings
    .services
    .iter()
    .enumerate()
    .map(|(i, entry)| {
      let describe = || {
        entry
          .name
          .clone()
          .unwrap_or_else(|| format!("services[{}]", i))
      };
      // A service name is an identifier the dashboard, the logs and a future
      // address form all share, so it is settled here rather than wherever it
      // is first displayed. `custom_name:` is where anything human goes.
      if let Some(name) = entry.name.as_deref() {
        aperio_config::validate_name("service", name)
          .map_err(|e| format!("CRITICAL ERROR: {e}"))?;
      }
      let target = entry
        .target
        .clone()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| {
          format!(
            "CRITICAL ERROR: service '{}' has no target (set target: or serve:)!",
            describe()
          )
        })?;
      // `server_side:` moves the last hop to the server, so `serve:` stops
      // being reachable: the files are on this machine. Refused here rather
      // than at the server, where the reason would arrive as a log line on
      // somebody else's machine.
      if entry.server_side.unwrap_or(false) && entry.serve.is_some() {
        return Err(format!(
          "CRITICAL ERROR: service '{}' sets both server_side: and serve:; the files are on \
           this machine, and a server reaching the target itself cannot serve them!",
          describe()
        ));
      }
      let path = entry.path.clone();
      let declared_connections = entry.connections.as_ref().or(settings.connections.as_ref());
      let (connections_min, connections) =
        clamp_connections(declared_connections, &format!("service '{}'", describe()));
      let (entry_public, entry_visitor_auth) = resolve_visitor_gate(
        &format!("auth for service '{}'", describe()),
        entry.auth.as_ref().or(settings.visitor_auth.as_ref()),
        entry.public.unwrap_or(settings.public),
      )?;
      Ok(ServiceSpec {
        name: entry.name.clone(),
        custom_name: entry
          .custom_name
          .clone()
          .map(|n| n.trim().to_string())
          .filter(|n| !n.is_empty()),
        client_id: format!("{}-{}", client_id_base, i),
        token: token.clone(),
        instance_group: client_id_base.to_string(),
        server_addr: server_addr.clone(),
        ws_url: ws_url.clone(),
        ws_urls: ws_urls.clone(),
        // The address travels with the ask, so the server is told where to go
        // only by a service that asked it to go there.
        server_side_target: entry.server_side.unwrap_or(false).then(|| target.clone()),
        target,
        hostnames: entry
          .hostname
          .clone()
          .map(|h| {
            h.into_vec()
              .into_iter()
              .map(|s| s.trim().to_ascii_lowercase())
              .filter(|s| !s.is_empty())
              .collect::<Vec<_>>()
          })
          .filter(|v| !v.is_empty())
          .unwrap_or_default(),
        trim_bind: if path.is_some() {
          entry.trim_bind.or(settings.trim_bind).unwrap_or(true)
        } else {
          false
        },
        path,
        pass_hostname: entry.pass_hostname.unwrap_or(settings.pass_hostname),
        max_response_body: entry
          .max_response_body
          .unwrap_or(settings.max_response_body),
        reload_drain_secs: settings.reload_drain_secs,
        retry_attempts: entry
          .retry
          .as_ref()
          .and_then(|r| r.attempts)
          .map(|n| n.clamp(1, 10))
          .unwrap_or(settings.retry_attempts),
        retry_backoff_ms: entry
          .retry
          .as_ref()
          .and_then(|r| r.backoff)
          .unwrap_or(settings.retry_backoff_ms),
        retry_all_methods: entry
          .retry
          .as_ref()
          .and_then(|r| r.all_methods)
          .unwrap_or(settings.retry_all_methods),
        breaker_failures: entry
          .circuit_breaker
          .as_ref()
          .and_then(|b| b.failures)
          .unwrap_or(settings.breaker_failures),
        breaker_open_for_secs: entry
          .circuit_breaker
          .as_ref()
          .and_then(|b| b.open_for)
          .map(|s| s.max(1))
          .unwrap_or(settings.breaker_open_for_secs),
        max_request_body: entry.max_request_body.or(settings.max_request_body),
        response_timeout: entry.response_timeout.or(settings.response_timeout),
        timeout_secs: entry.timeout.unwrap_or(settings.timeout_secs),
        max_concurrent: entry
          .max_concurrent
          .or(settings.max_concurrent)
          .filter(|n| *n > 0),
        adaptive_concurrency: entry
          .adaptive_concurrency
          .unwrap_or(settings.adaptive_concurrency),
        connections,
        connections_min,
        multiplex: entry.multiplex.unwrap_or(settings.multiplex),
        // Settled below, once every entry has been built: whether this service
        // shares a connection depends on what the others asked for.
        multiplex_group: None,
        metrics_labels: entry
          .metrics_labels
          .clone()
          .unwrap_or_else(|| settings.metrics_labels.clone()),
        startup_delay: entry.startup_delay.or(settings.startup_delay).unwrap_or(0),
        // A file-wide `depends_on:` is the default for entries that name none
        // of their own, and it reads as "everything else waits for these", so
        // a service the list itself names is not one of the things that wait.
        // Dropping only the self-reference is not enough: `depends_on: [a, b]`
        // over services `a` and `b` would leave each waiting for the other,
        // and a cycle refuses to start.
        depends_on: match entry.depends_on.clone() {
          Some(own) => own,
          // An entry with no name does not inherit it either, and that is not
          // a detail: `validate_depends_on` refuses a nameless spec that
          // carries a list, so handing the default to one would turn a file
          // that started yesterday into a client that refuses to start today.
          // A nameless service is also nothing another can wait *for*, so
          // there is no meaning being lost.
          None => match entry.name.as_deref() {
            None => Vec::new(),
            Some(name) => {
              let file_wide = settings.depends_on.clone().unwrap_or_default();
              if file_wide.iter().any(|d| d == name) {
                Vec::new()
              } else {
                file_wide
              }
            }
          },
        },
        connect_timeout: entry.connect_timeout.or(settings.connect_timeout),
        min_tls_version: entry
          .min_tls_version
          .clone()
          .or_else(|| settings.min_tls_version.clone()),
        pool_load: std::sync::Arc::new(service::PoolLoad::default()),
        priority: entry.priority.unwrap_or(settings.priority),
        // Only what this entry asked for: the top-level value is the budget
        // these requests are settled against, not a fallback default.
        bandwidth_bps: parse_bw(entry.bandwidth.as_deref()),
        bandwidth_declared: entry.bandwidth.clone(),
        config_notes: connections_note(declared_connections, connections),
        max_message_size: settings.max_message_size,
        max_redirects: entry.max_redirects.unwrap_or(settings.max_redirects),
        tcp_target: entry
          .tcp_target
          .clone()
          .map(|s| s.trim().to_string())
          .filter(|s| !s.is_empty()),
        target_health: entry
          .target_health
          .clone()
          .map(|s| s.trim().to_string())
          .filter(|s| !s.is_empty()),
        wait_for_backend: entry.wait_for_backend.unwrap_or(settings.wait_for_backend),
        health_interval: entry
          .health_interval
          .unwrap_or(settings.health_interval)
          .max(1),
        health_timeout: entry
          .health_timeout
          .unwrap_or(settings.health_timeout)
          .max(1),
        health_threshold: entry
          .health_threshold
          .unwrap_or(settings.health_threshold)
          .max(1),
        public: entry_public,
        visitor_auth: entry_visitor_auth,
        visitor_auth_policy: entry.auth.clone().or_else(|| settings.visitor_auth.clone()),
        allowed_ips: entry
          .allowed_ips
          .clone()
          .unwrap_or_else(|| settings.allowed_ips.clone()),
        resilience: entry.resilience.unwrap_or(settings.resilience),
        capture: entry.capture.unwrap_or(settings.capture),
        webhook_inbox: entry.webhook_inbox.unwrap_or(settings.webhook_inbox),
        scaling: settings.scaling.clone(),
        denied: entry
          .denied
          .clone()
          .map(|s| s.trim().to_string())
          .filter(|s| !s.is_empty())
          .or_else(|| settings.denied.clone()),
        tunnels: tunnels.clone(),
        headers: crate::config::merge_security_headers(
          entry.headers.clone().or_else(|| settings.headers.clone()),
          entry
            .security_headers
            .as_ref()
            .or(settings.security_headers.as_ref()),
        ),
        cache: entry.cache.unwrap_or(settings.cache),
      })
    })
    .collect::<Result<_, String>>()?;
  validate_depends_on(&specs)?;
  validate_tls_floors(&specs)?;
  group_multiplexed(&mut specs)?;
  allocate_bandwidth(&mut specs, budget_bps);
  settle_multiplexed_bandwidth(&mut specs);
  Ok(specs)
}

/// Makes a multiplexed group's announced bandwidth mean what the server will
/// actually do with it.
///
/// The server shapes the *socket*: a connection has one writer and one token
/// bucket, and every service on it announces into that same cell, last one
/// winning. `allocate_bandwidth` above has just divided the budget into a share
/// per service, which is exactly right when each service has a connection of
/// its own and exactly wrong when they share one. Four services splitting an
/// 8mbit budget announce 2mbit each, the cell ends up holding 2mbit, and a link
/// the operator sized at 8 is paced at 2. At forty services it is a fortieth.
///
/// So the group announces one number, the same on every member, and it is the
/// sum: that is the capacity the operator allocated to these services, and the
/// socket is where it is enforced. Each member reporting the connection's cap
/// is also the truthful thing to show, since on a shared socket there is no
/// per-service cap to show instead.
///
/// **A member with no cap uncaps the group**, and that is not a shortcut. The
/// server reads an absent value as zero and zero as unlimited, so such a member
/// wipes the cell whatever the others declared: the cap was already not being
/// enforced, silently, and the only question is whether the client says so.
/// Capping the socket at the sum of the declared ones instead would throttle a
/// service nobody limited, which is a constraint the file does not contain.
pub(crate) fn settle_multiplexed_bandwidth(specs: &mut [ServiceSpec]) {
  let groups: Vec<usize> = {
    let mut seen: Vec<usize> = specs.iter().filter_map(|s| s.multiplex_group).collect();
    seen.sort_unstable();
    seen.dedup();
    seen
  };
  for group in groups {
    let members: Vec<usize> = (0..specs.len())
      .filter(|&i| specs[i].multiplex_group == Some(group))
      .collect();
    let declared: Vec<Option<u64>> = members.iter().map(|&i| specs[i].bandwidth_bps).collect();
    if declared.iter().all(Option::is_none) {
      continue;
    }
    let uncapped: Vec<String> = members
      .iter()
      .filter(|&&i| specs[i].bandwidth_bps.is_none())
      .map(|&i| specs[i].label())
      .collect();
    let total: Option<u64> = if uncapped.is_empty() {
      // Saturating: these come from the config, and a sum of 256 of them that
      // wrapped would announce a *small* cap for a link the operator asked to
      // leave fast, which is the wrong direction to be wrong in. In a debug
      // build it would panic outright.
      Some(
        declared
          .iter()
          .flatten()
          .fold(0u64, |acc, n| acc.saturating_add(*n)),
      )
    } else {
      warn!(
        "bandwidth: {} share(s) a connection with {} which declare(s) no limit, so nothing on it is paced. The server shapes the socket, not the service, and a service without a limit lifts it for the whole connection. Give every service in the group a bandwidth:, or take one out with multiplex: false.",
        members
          .iter()
          .filter(|&&i| specs[i].bandwidth_bps.is_some())
          .map(|&i| specs[i].label())
          .collect::<Vec<_>>()
          .join(", "),
        uncapped.join(", ")
      );
      None
    };
    for &i in &members {
      let before = specs[i].bandwidth_bps;
      specs[i].bandwidth_bps = total;
      if before == total {
        continue;
      }
      // Replaces the note `allocate_bandwidth` just wrote: it explains a split
      // that no longer describes what is announced.
      specs[i].config_notes.retain(|n| n.field != "bandwidth");
      specs[i].config_notes.push(ConfigNote {
        field: "bandwidth".to_string(),
        declared: before
          .map(format_bandwidth)
          .unwrap_or_else(|| "unlimited".to_string()),
        effective: total
          .map(format_bandwidth)
          .unwrap_or_else(|| "unlimited".to_string()),
        reason: match total {
          Some(_) => "multiplexed services share one shaped connection, so the group's limits are announced as one".to_string(),
          None => "a service sharing this connection declares no limit, and the server shapes the connection rather than the service".to_string(),
        },
      });
    }
  }
}

/// Settles which services actually share a connection, and what that costs
/// them.
///
/// Done here, over the whole list, because it is not a per-entry question. A
/// service asks to be multiplexed on its own, but sharing needs somebody to
/// share with, and the somebody has to be reachable on the same socket: a
/// connection carries one server URL and one token, so services that disagree
/// about either cannot be on it however they are configured. Today that pair is
/// the same for every entry, since `server:` and `token:` are file-wide, so
/// this always finds a single group. It is keyed on the pair anyway because
/// this is the line that would otherwise become silently wrong the day an entry
/// may name its own server, and putting two servers' services on one socket is
/// not a failure that announces itself.
///
/// A group of one is left ungrouped, which is the same connection it would have
/// had anyway.
pub(crate) fn group_multiplexed(specs: &mut [ServiceSpec]) -> Result<(), String> {
  // Insertion-ordered, so the group ids follow the file rather than a hash.
  let mut keys: Vec<(String, String)> = Vec::new();
  let mut members: Vec<Vec<usize>> = Vec::new();
  for (i, spec) in specs.iter().enumerate() {
    if !spec.multiplex {
      continue;
    }
    let key = (spec.token.clone(), spec.ws_url.clone());
    match keys.iter().position(|k| *k == key) {
      Some(g) => members[g].push(i),
      None => {
        keys.push(key);
        members.push(vec![i]);
      }
    }
  }
  let mut group = 0usize;
  for (key, indexes) in keys.iter().zip(members) {
    if indexes.len() < 2 {
      // Asked for, and nobody to share with. Said out loud rather than
      // silently ignored: `multiplex: true` on one service reads as a setting
      // that took effect, and the operator who wrote it is usually one entry
      // away from meaning it.
      if let Some(i) = indexes.first() {
        info!(
          "[{}] multiplex: no other service shares this server and token, so this one keeps its own connection",
          specs[*i].label()
        );
      }
      continue;
    }
    // Refused here, where the message can name the file, rather than at the
    // server, which answers a list this long by dropping the connection: the
    // operator would see a client that connects and disconnects with the
    // reason in somebody else's log.
    if indexes.len() > service::MAX_MULTIPLEXED_SERVICES {
      return Err(format!(
        "CRITICAL ERROR: {} services ask to share one connection (multiplex: true) and a server accepts at most {}! Split them across connections with multiplex: false, or give some of them a server of their own.",
        indexes.len(),
        service::MAX_MULTIPLEXED_SERVICES
      ));
    }
    for i in &indexes {
      // A name is what the server files this service's routing, ejection and
      // statistics under, and what addresses it in the dashboard. On a
      // connection of its own a service can do without one, because the
      // connection is the address; sharing one, two unnamed services are told
      // apart only by their position in a list, which is not something a
      // config file promises to keep.
      if specs[*i].name.is_none() {
        return Err(format!(
          "CRITICAL ERROR: every multiplexed service needs a name: (the service at {} shares a connection with {} other(s) and has none)!",
          specs[*i].target,
          indexes.len() - 1
        ));
      }
    }
    for i in indexes {
      specs[i].multiplex_group = Some(group);
      // One connection is what multiplexing means, so a pool is not something
      // this service can also have. Reported rather than dropped: the
      // dashboard's config view is where a value that did not survive its
      // config is supposed to show up, and `connections:` is exactly the key
      // an operator would otherwise believe was in force.
      if specs[i].connections != 1 || specs[i].connections_min != 1 {
        let declared = if specs[i].connections_min < specs[i].connections {
          format!("{}-{}", specs[i].connections_min, specs[i].connections)
        } else {
          specs[i].connections.to_string()
        };
        specs[i].config_notes.retain(|n| n.field != "connections");
        specs[i].config_notes.push(ConfigNote {
          field: "connections".to_string(),
          declared,
          effective: "1".to_string(),
          reason: "multiplexed services share one connection".to_string(),
        });
        specs[i].connections = 1;
        specs[i].connections_min = 1;
      }
    }
    info!(
      "multiplex: {} services share one connection to {}",
      specs
        .iter()
        .filter(|s| s.multiplex_group == Some(group))
        .count(),
      key.1
    );
    group += 1;
  }
  Ok(())
}

/// Rejects a `min_tls_version` this build cannot honour, here rather than
/// where it is used.
///
/// It used to be parsed inside the running service task, which had no way to
/// refuse a configuration and so called `process::exit(1)`: a hot-reload that
/// introduced one typo took the whole client down, including every other
/// service in the file, when the contract is that a bad reload is warned
/// about and the previous configuration kept. `--check-config` could not see
/// it either, because nothing on the validation path ever parsed the value.
pub(crate) fn validate_tls_floors(specs: &[ServiceSpec]) -> Result<(), String> {
  for spec in specs {
    if let Err(e) = crate::proxy::http::tls_floor(spec.min_tls_version.as_deref()) {
      return Err(match spec.name.as_deref() {
        Some(name) => format!("CRITICAL ERROR: service '{name}': {e}"),
        None => format!("CRITICAL ERROR: {e}"),
      });
    }
  }
  Ok(())
}

/// Rejects a `depends_on` that cannot be satisfied.
///
/// All three of these end the same way at runtime, everybody waiting out the
/// grace period and then starting anyway, so the failure is invisible unless
/// it is caught here. A name that is not in the file is almost always a typo;
/// a cycle is always a mistake, since no member of it can ever come up first.
pub(crate) fn validate_depends_on(specs: &[ServiceSpec]) -> Result<(), String> {
  use std::collections::{HashMap, HashSet};
  let names: HashSet<&str> = specs.iter().filter_map(|s| s.name.as_deref()).collect();
  let mut deps: HashMap<&str, &[String]> = HashMap::new();
  for spec in specs {
    let Some(name) = spec.name.as_deref() else {
      if !spec.depends_on.is_empty() {
        return Err(
          "CRITICAL ERROR: depends_on needs services with names; the entry that declares it has none"
            .to_string(),
        );
      }
      continue;
    };
    for dep in &spec.depends_on {
      if dep == name {
        return Err(format!(
          "CRITICAL ERROR: service '{name}' depends_on itself"
        ));
      }
      if !names.contains(dep.as_str()) {
        return Err(format!(
          "CRITICAL ERROR: service '{name}' depends_on '{dep}', which is not a service in this configuration"
        ));
      }
    }
    deps.insert(name, &spec.depends_on);
  }
  // Depth-first walk; `visiting` is the current chain, so re-entering it is a
  // cycle rather than merely a diamond.
  fn walk<'a>(
    node: &'a str,
    deps: &HashMap<&'a str, &'a [String]>,
    visiting: &mut Vec<&'a str>,
    done: &mut HashSet<&'a str>,
  ) -> Result<(), String> {
    if done.contains(node) {
      return Ok(());
    }
    if let Some(at) = visiting.iter().position(|n| *n == node) {
      let mut chain: Vec<&str> = visiting[at..].to_vec();
      chain.push(node);
      return Err(format!(
        "CRITICAL ERROR: depends_on forms a cycle ({}); no service in it can ever start first",
        chain.join(" -> ")
      ));
    }
    visiting.push(node);
    for dep in deps.get(node).copied().unwrap_or(&[]) {
      walk(dep, deps, visiting, done)?;
    }
    visiting.pop();
    done.insert(node);
    Ok(())
  }
  let mut done = HashSet::new();
  for name in deps.keys() {
    walk(name, &deps, &mut Vec::new(), &mut done)?;
  }
  Ok(())
}

#[cfg(test)]
#[path = "specs_tests.rs"]
pub(crate) mod tests;
