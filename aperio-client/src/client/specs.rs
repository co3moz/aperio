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
  aperio_config::validate_client_declared_auth(policy).map_err(|why| format!("{label}: {why}"))?;
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
/// One spec per service, each from its own entry with `defaults:` already
/// laid under it, so there is nothing left to fall back to here. The one
/// service the command line or the environment describes runs under the
/// client's own id; a file's services suffix it with their position. Returns
/// an error message (used verbatim in logs) when a required value is missing
/// or invalid.
pub(crate) fn build_specs(
  settings: &ClientSettings,
  client_id_base: &str,
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

  // Additional server URLs for cross-server failover. The primary is always
  // the first candidate; the reconnect loop rotates to the next after a
  // failed connection.
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

  // A client may run with nothing exposed at all. The connection then serves
  // no HTTP target and exists purely to carry something else: the `tunnels:`
  // a peer binds in an emergency, or the messages this client publishes and
  // subscribes to. Refusing to start would mean a publish-only client had to
  // invent a service it does not have.
  let carrier;
  let services: &[crate::config::Service] = if settings.services.is_empty() {
    let carries_messages = !settings.subscribe.is_empty()
      || settings.messages_listen.is_some()
      || settings.messages_mqtt_listen.is_some();
    if tunnels.is_empty() && !carries_messages {
      return Err(
        "CRITICAL ERROR: there is nothing for this client to do (give a positional target, APERIO_TARGET, or a services:/tunnels:/subscribe: section)!".to_string(),
      );
    }
    carrier = [crate::config::Service {
      name: None,
      entry: settings.defaults.clone(),
    }];
    &carrier
  } else {
    &settings.services
  };
  let single = services.len() == 1 && services[0].name.is_none();

  // Parallel connections per service: bounded so a typo cannot exhaust the
  // server's tunnel slots. A sanity bound, not a policy: the policy is the
  // server's `limits.connections_per_service` (lowered further by a token's
  // own `max_connections`), announced on the handshake and applied per
  // connection; this only stops `connections: 100000` from spawning a hundred
  // thousand tasks before the first one has connected to find out.
  const CONNECTIONS_SANITY_BOUND: u32 = 256;

  let mut specs: Vec<ServiceSpec> = Vec::with_capacity(services.len());
  for (i, service) in services.iter().enumerate() {
    let entry = &service.entry;
    let p = &entry.policy;
    let label = match &service.name {
      Some(name) => format!("service '{name}'"),
      None => "the service".to_string(),
    };
    // A service name is an identifier the dashboard, the logs and a future
    // address form all share, so it is settled here rather than wherever it
    // is first displayed. `custom_name:` is where anything human goes.
    if let Some(name) = service.name.as_deref() {
      aperio_config::validate_name("service", name).map_err(|e| format!("CRITICAL ERROR: {e}"))?;
    }
    let tcp_target = entry
      .tcp_target
      .clone()
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty());
    let target = entry
      .target
      .as_deref()
      .map(normalize_target)
      .filter(|t| !t.is_empty());
    let target = match target {
      Some(t) => t,
      // A raw TCP service, or the carrier, has no HTTP target.
      None if tcp_target.is_some() || service.name.is_none() && settings.services.is_empty() => {
        String::new()
      }
      None => {
        return Err(format!(
          "CRITICAL ERROR: {label} has nothing to expose (set target, serve or tcp_target)!"
        ));
      }
    };
    // Unix socket targets must carry a path, and only exist on Unix.
    if crate::proxy::unix::is_unix_target(&target) {
      if cfg!(not(unix)) {
        return Err(format!(
          "CRITICAL ERROR: {label} uses a unix:// target, which is not supported on this platform"
        ));
      }
      if crate::proxy::unix::unix_socket_path(&target).is_none() {
        return Err(format!(
          "CRITICAL ERROR: {label} has a unix:// target without a socket path (expected e.g. unix:///var/run/app.sock)"
        ));
      }
    }
    // `server_side:` moves the last hop to the server, so `serve:` stops
    // being reachable: the files are on this machine. Refused here rather
    // than at the server, where the reason would arrive as a log line on
    // somebody else's machine.
    let server_side = p.server_side.unwrap_or(false);
    if server_side && entry.serve.is_some() {
      return Err(format!(
        "CRITICAL ERROR: {label} sets both server_side: and serve:; the files are on this \
         machine, and a server reaching the target itself cannot serve them!"
      ));
    }
    // Visitor allowlists fail at startup, not silently on the server.
    let allowed_ips = p.allowed_ips.clone().unwrap_or_default();
    for ip in &allowed_ips {
      if !crate::config::valid_ip_entry(ip) {
        return Err(format!(
          "CRITICAL ERROR: {label} has an invalid allowed_ips entry '{ip}'; expected an IP, a CIDR range, or '*'"
        ));
      }
    }
    // Denied-visitor redirects must be absolute http(s) URLs, anything else
    // would silently degrade to stealth on the server.
    let denied = p
      .denied
      .clone()
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty());
    if let Some(url) = &denied {
      let ok =
        (url.starts_with("http://") || url.starts_with("https://")) && url::Url::parse(url).is_ok();
      if !ok {
        return Err(format!(
          "CRITICAL ERROR: {label} has an invalid denied: value '{url}'; expected an absolute http(s) URL"
        ));
      }
    }

    let declared = p.connections.as_ref();
    let (connections_min, connections) = match declared {
      None => (1, 1),
      Some(c) if c.max() > CONNECTIONS_SANITY_BOUND => {
        warn!(
          "{} requests {} connections; clamping to {}. The server decides the real \
           ceiling and announces it on connect.",
          label,
          c.max(),
          CONNECTIONS_SANITY_BOUND
        );
        (
          c.min().min(CONNECTIONS_SANITY_BOUND),
          CONNECTIONS_SANITY_BOUND,
        )
      }
      Some(c) => (c.min(), c.max()),
    };
    // A clamped count is announced so the dashboard shows what the config
    // asked for next to what the client is really running.
    let asked = declared.map(|c| c.max()).unwrap_or(1).max(1);
    let config_notes: Vec<ConfigNote> = (asked != connections)
      .then(|| ConfigNote {
        field: "connections".to_string(),
        declared: asked.to_string(),
        effective: connections.to_string(),
        reason: format!("clamped to {CONNECTIONS_SANITY_BOUND} parallel connections"),
      })
      .into_iter()
      .collect();

    let access = p.access.as_ref().map(aperio_config::Access::auth_setting);
    let (public, visitor_auth) =
      resolve_visitor_gate(&format!("access for {label}"), access.as_ref(), false)?;
    let health = p.health.clone().unwrap_or_default();
    let retry = p.retry.clone().unwrap_or_default();
    let breaker = p.circuit_breaker.clone().unwrap_or_default();
    let limits = p.limits.clone().unwrap_or_default();
    let cache = p.cache.clone().unwrap_or_default();
    let path = entry
      .path
      .clone()
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty());
    let secs = |span: Option<aperio_config::units::Span>| span.map(|s| s.as_secs());

    specs.push(ServiceSpec {
      name: service.name.clone(),
      custom_name: entry
        .custom_name
        .clone()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty()),
      client_id: if single {
        client_id_base.to_string()
      } else {
        format!("{}-{}", client_id_base, i)
      },
      token: token.clone(),
      instance_group: client_id_base.to_string(),
      server_addr: server_addr.clone(),
      ws_url: ws_url.clone(),
      ws_urls: ws_urls.clone(),
      client_name: settings.name.clone(),
      // The address travels with the ask, so the server is told where to go
      // only by a service that asked it to go there.
      server_side_target: server_side.then(|| target.clone()),
      allow_server_forward: settings.allow_server_forward,
      target,
      hostnames: entry
        .hostname
        .clone()
        .map(aperio_config::Hostnames::into_vec)
        .unwrap_or_default(),
      trim_bind: path.is_some() && p.trim_bind.unwrap_or(true),
      path,
      pass_hostname: p.pass_hostname.unwrap_or(false),
      max_response_body: limits
        .response_body
        .map(|b| b.0 as usize)
        .unwrap_or(50 * 1024 * 1024),
      reload_drain_secs: settings.reload_drain_secs,
      retry_attempts: retry.attempts.unwrap_or(1).clamp(1, 10),
      retry_backoff_ms: retry.delay.map(|d| d.0.as_millis() as u64).unwrap_or(100),
      retry_all_methods: retry.all_methods.unwrap_or(false),
      breaker_failures: breaker.failures.unwrap_or(0),
      breaker_open_for_secs: secs(breaker.open_for).unwrap_or(30).max(1),
      max_request_body: limits.request_body.map(|b| b.0),
      response_timeout: secs(p.response_timeout).filter(|s| *s > 0),
      timeout_secs: secs(p.timeout).unwrap_or(30),
      max_concurrent: limits.concurrent_requests.filter(|n| *n > 0),
      adaptive_concurrency: limits.adaptive_concurrency.unwrap_or(false),
      connections,
      connections_min,
      // A lone service has nobody to share a connection with, so the flag has
      // nothing to do there whichever way it is set.
      multiplex: !single && p.multiplex.unwrap_or(false),
      // Settled below, once every entry has been built: whether this service
      // shares a connection depends on what the others asked for.
      multiplex_group: None,
      metrics_labels: p.metrics_labels.clone().unwrap_or_default(),
      startup_delay: secs(p.startup_delay).unwrap_or(0),
      depends_on: entry.depends_on.clone().unwrap_or_default(),
      connect_timeout: secs(p.connect_timeout).filter(|s| *s > 0),
      min_tls_version: p.min_tls_version.clone(),
      pool_load: std::sync::Arc::new(service::PoolLoad::default()),
      priority: p.priority.unwrap_or(0),
      // Only what this entry asked for: the top-level value is the budget
      // these requests are settled against, not a fallback default.
      bandwidth_bps: parse_bw(entry.bandwidth.as_deref()),
      bandwidth_declared: entry.bandwidth.clone(),
      config_notes,
      max_message_size: settings.max_message_size,
      max_redirects: limits.redirects.unwrap_or(5),
      tcp_target,
      target_health: health
        .endpoint
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()),
      wait_for_backend: health.wait_for_backend.unwrap_or(false),
      health_interval: secs(health.interval).unwrap_or(10).max(1),
      health_timeout: secs(health.timeout).unwrap_or(5).max(1),
      health_threshold: health.threshold.unwrap_or(2).max(1),
      public,
      visitor_auth,
      visitor_auth_policy: access,
      allowed_ips,
      resilience: cache.offline.unwrap_or(false),
      capture: p.inspector.unwrap_or(true),
      webhook_inbox: p.webhook_inbox.unwrap_or(false),
      denied,
      scaling: settings.scaling.clone(),
      tunnels: tunnels.clone(),
      headers: crate::config::merge_security_headers(
        p.headers.clone(),
        p.security_headers.as_ref(),
      ),
      cache: cache.enabled.unwrap_or(false),
    });
  }
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
