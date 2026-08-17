//! What a service announces about itself once the whole file is known: its
//! share of the bandwidth budget, the tunnels it declares, and the line the
//! log prints for it at startup.

use tracing::{info, warn};

use crate::protocol::ConfigNote;
use crate::service::ServiceSpec;
use crate::*;

/// Settles every service's bandwidth request against the client-wide budget
/// and hands each parallel connection its own share.
///
/// The server shapes each tunnel connection with a token bucket of its own, so
/// N connections all announcing B would let the client be pushed at N*B. On
/// entry `bandwidth_bps` holds what a service asked for (`None` = it asked for
/// nothing); on exit it holds the rate a single connection of that service
/// announces, so the sum over the whole client never exceeds the budget.
///
/// With no top-level budget every service simply keeps what it asked for and
/// the rest stay unlimited. With one:
///
/// - services that named a rate keep it, and whatever is left over is split
///   equally among the services that did not,
/// - if the named rates would leave the unspecified services nothing at all,
///   every named rate is dropped (with a warning) and the budget is split
///   equally, since a service configured to run cannot be given zero,
/// - if every service named a rate and together they overshoot, the rates are
///   scaled down proportionally (with a warning) so the shares keep their
///   relative weight.
///
/// Every difference it introduces is recorded as a `ConfigNote` on the spec, so
/// the dashboard can show the announced rate together with the value the
/// operator actually wrote.
pub(crate) fn allocate_bandwidth(specs: &mut [ServiceSpec], budget_bps: Option<u64>) {
  if specs.is_empty() {
    return;
  }
  // Why a service's rate is not simply what it asked for, filled in by the
  // branch that settled it; the per-connection split appends its own reason.
  let mut settled: Vec<Option<String>> = vec![None; specs.len()];

  if let Some(budget) = budget_bps {
    let asked: u64 = specs.iter().filter_map(|s| s.bandwidth_bps).sum();
    let unspecified = specs.iter().filter(|s| s.bandwidth_bps.is_none()).count();
    if unspecified > 0 && asked >= budget {
      warn!(
        "The per-service bandwidth limits ({} bytes/s) leave nothing of the {} bytes/s budget for the {} service(s) without one; ignoring them and splitting the budget equally",
        asked, budget, unspecified
      );
      let share = budget / specs.len() as u64;
      let reason = format!(
        "the per-service limits left nothing of the {} budget for the {} service(s) without one, so the budget was split equally",
        format_bandwidth(budget),
        unspecified
      );
      for (i, spec) in specs.iter_mut().enumerate() {
        settled[i] = Some(reason.clone());
        spec.bandwidth_bps = Some(share);
      }
    } else if unspecified == 0 && asked > budget {
      warn!(
        "The per-service bandwidth limits add up to {} bytes/s, over the {} bytes/s budget; scaling every limit down proportionally",
        asked, budget
      );
      let reason = format!(
        "the per-service limits added up to {}, over the {} budget, so every limit was scaled down proportionally",
        format_bandwidth(asked),
        format_bandwidth(budget)
      );
      for (i, spec) in specs.iter_mut().enumerate() {
        let want = spec.bandwidth_bps.unwrap_or(0) as u128;
        settled[i] = Some(reason.clone());
        spec.bandwidth_bps = Some((want * budget as u128 / asked as u128) as u64);
      }
    } else if unspecified > 0 {
      let share = (budget - asked) / unspecified as u64;
      let reason = format!(
        "an equal share of what the {} budget leaves the services without a limit of their own",
        format_bandwidth(budget)
      );
      for (i, spec) in specs.iter_mut().enumerate() {
        if spec.bandwidth_bps.is_none() {
          settled[i] = Some(reason.clone());
          spec.bandwidth_bps = Some(share);
        }
      }
    }
  }

  // A service's share is split across its parallel connections, each of which
  // is shaped separately by the server. Never announce 0: the server reads
  // that as unlimited, which is the opposite of what a tiny share means.
  for (i, spec) in specs.iter_mut().enumerate() {
    let per_service = spec.bandwidth_bps;
    if let Some(bps) = per_service {
      spec.bandwidth_bps = Some((bps / spec.connections as u64).max(1));
    }
    let mut reasons: Vec<String> = settled[i].take().into_iter().collect();
    if per_service.is_some() && spec.connections > 1 {
      reasons.push(format!(
        "split across {} parallel connections",
        spec.connections
      ));
    }
    let declared = spec.bandwidth_declared.clone();
    let note = match (declared, spec.bandwidth_bps) {
      // Unparseable: already warned at parse time, reported here as well so
      // it shows up in the dashboard next to the value it failed to become.
      (Some(raw), _) if parse_bandwidth(&raw).is_none() => Some(ConfigNote {
        field: "bandwidth".to_string(),
        declared: raw,
        effective: "unlimited".to_string(),
        reason: "not a valid rate, so it was ignored".to_string(),
      }),
      (declared, Some(effective)) if !reasons.is_empty() => Some(ConfigNote {
        field: "bandwidth".to_string(),
        declared: declared.unwrap_or_default(),
        effective: format_bandwidth(effective),
        reason: reasons.join("; "),
      }),
      _ => None,
    };
    spec.config_notes.extend(note);
  }
}

/// Validates the `tunnels:` list: only TCP is supported for now, targets
/// must be `host:port`, and duplicates are rejected. Returns the normalized
/// declarations.
pub(crate) fn validate_tunnels(
  raw: &[crate::protocol::TunnelDecl],
) -> Result<Vec<crate::protocol::TunnelDecl>, String> {
  let mut seen = std::collections::HashSet::new();
  let mut names = std::collections::HashSet::new();
  let mut out = Vec::with_capacity(raw.len());
  for decl in raw {
    let target = decl.target.trim().to_string();
    // `udp/tcp` is normalized to the one spelling everything else compares
    // against, so a file may write it either way round.
    let protocol = match decl.protocol.trim().to_ascii_lowercase().as_str() {
      "udp/tcp" => aperio_config::PROTOCOL_BOTH.to_string(),
      other => other.to_string(),
    };
    if !matches!(
      protocol.as_str(),
      "tcp" | "udp" | aperio_config::PROTOCOL_BOTH
    ) {
      return Err(format!(
        "CRITICAL ERROR: tunnel '{}' declares protocol '{}'; use tcp, udp, or tcp/udp for a service that is both",
        target, decl.protocol
      ));
    }
    let port_ok = target
      .rsplit_once(':')
      .and_then(|(host, port)| {
        let port = port.parse::<u16>().ok().filter(|p| *p > 0)?;
        if host.is_empty() { None } else { Some(port) }
      })
      .is_some();
    if !port_ok {
      return Err(format!(
        "CRITICAL ERROR: tunnel target '{}' is not a host:port address",
        decl.target
      ));
    }
    if !seen.insert((target.clone(), protocol.clone())) {
      return Err(format!(
        "CRITICAL ERROR: tunnel target '{}' ({}) is declared more than once",
        target, protocol
      ));
    }
    if decl.encrypt && protocol != "tcp" {
      return Err(format!(
        "CRITICAL ERROR: tunnel '{}' sets encrypt: true, which is only supported for tcp tunnels (a tcp/udp tunnel would leave its udp half in the clear)",
        target
      ));
    }
    if decl.psk.is_some() && !decl.encrypt {
      return Err(format!(
        "CRITICAL ERROR: tunnel '{}' sets a psk without encrypt: true",
        target
      ));
    }
    if let Some(secs) = decl.idle_timeout {
      // Applies to the datagram half, so a combined tunnel may set it.
      if !aperio_config::protocol_serves(&protocol, "udp") {
        return Err(format!(
          "CRITICAL ERROR: tunnel '{}' sets idle_timeout, which is only supported for udp tunnels",
          target
        ));
      }
      if secs == 0 {
        return Err(format!(
          "CRITICAL ERROR: tunnel '{}' sets idle_timeout: 0; it must be at least 1 second",
          target
        ));
      }
    }
    if decl.expose.is_some() {
      // A public port relays TCP; a combined tunnel qualifies for its tcp half.
      if !aperio_config::protocol_serves(&protocol, "tcp") {
        return Err(format!(
          "CRITICAL ERROR: tunnel '{}' sets expose, which is only supported for tcp tunnels",
          target
        ));
      }
      if decl.encrypt {
        return Err(format!(
          "CRITICAL ERROR: tunnel '{}' sets expose together with encrypt: true; a public port cannot run the client-side encryption handshake",
          target
        ));
      }
    }
    // The name is the handle a binder and an `expose:` entry address, so it is
    // settled here and announced, rather than being re-derived by whoever
    // needs it. An explicit name is validated; a derived one cannot fail.
    if let Some(name) = decl.name.as_deref() {
      aperio_config::validate_tunnel_name(name).map_err(|e| format!("CRITICAL ERROR: {e}"))?;
    }
    let normalized = crate::protocol::TunnelDecl {
      name: decl.name.as_ref().map(|n| n.trim().to_string()),
      custom_name: decl
        .custom_name
        .as_ref()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty()),
      target,
      protocol,
      encrypt: decl.encrypt,
      psk: decl.psk.clone(),
      proxy_protocol: decl.proxy_protocol,
      idle_timeout: decl.idle_timeout,
      expose: decl.expose.clone(),
    };
    let name = aperio_config::tunnel_name(&normalized);
    if !names.insert(name.clone()) {
      return Err(format!(
        "CRITICAL ERROR: two tunnels resolve to the name '{name}'; give one of them a distinct `name:`"
      ));
    }
    out.push(normalized);
  }
  Ok(out)
}

/// Logs the effective configuration of a service at startup.
pub(crate) fn log_spec(spec: &ServiceSpec) {
  match spec.name {
    Some(ref name) => info!("Service '{}' configured:", name),
    None => info!("Configuration loaded:"),
  }
  info!("- Client ID: {}", spec.client_id);
  if spec.target.is_empty() {
    if spec.tunnels.is_empty() {
      info!("- Target: (none, this connection carries messages)");
    } else {
      info!("- Target: (none, tunnels only)");
    }
  } else {
    info!("- Target: {}", spec.target);
  }
  info!("- Pass Hostname: {}", spec.pass_hostname);
  if let Some(ref bind) = spec.path {
    info!("- Path Bind: {}", bind);
    info!("- Trim Bind: {}", spec.trim_bind);
  }
  match spec.hostnames.as_slice() {
    [] => {}
    [one] => info!("- Hostname Bind: {}", one),
    many => info!("- Hostname Binds: {}", many.join(", ")),
  }
  if let Some(n) = spec.max_concurrent {
    info!("- Max Concurrent Requests: {}", n);
  }
  if spec.priority > 0 {
    info!(
      "- Load Balancing Priority: {} (standby tier)",
      spec.priority
    );
  }
  if let Some(bw) = spec.bandwidth_bps {
    if spec.connections > 1 {
      info!(
        "- Announced Bandwidth: {} bytes/s per connection ({} bytes/s across {} connections)",
        bw,
        bw * spec.connections as u64,
        spec.connections
      );
    } else {
      info!("- Announced Bandwidth: {} bytes/s", bw);
    }
  }
  if spec.connections > 1 {
    info!(
      "- Connections: {} parallel tunnel connections (ids {}, {}-c2, ...)",
      spec.connections, spec.client_id, spec.client_id
    );
  }
  if let Some(ref t) = spec.tcp_target {
    info!("- TCP Target: {}", t);
  }
  if spec.public {
    info!("- Public: visitor auth gate skipped for this service (token permitting)");
  }
  if spec.visitor_auth.is_some() {
    info!("- Visitor auth: this service is gated behind a client-set login (token permitting)");
  }
  for t in &spec.tunnels {
    info!(
      "- Tunnel: {} ({}), bindable by a peer client with this token and client id",
      t.target, t.protocol
    );
  }
  info!("- Server URL: {}", spec.server_addr);
  info!("- WebSocket URL: {}", spec.ws_url);
  if spec.ws_urls.len() > 1 {
    info!("- Failover servers: {}", spec.ws_urls.len());
  }
}

#[cfg(test)]
#[path = "bandwidth_tests.rs"]
mod tests;
