//! `aperio-client --bind-tunnels`: local listeners for tunnels declared
//! elsewhere.
//!
//! A normally unexposed service (a database, an internal admin port, an SSH
//! daemon) is declared as a tunnel by the client running next to it. Any
//! client permitted to bind it can turn it into a local listener: every
//! accepted connection is relayed through the server to the declaring client,
//! which dials its local target.
//!
//! Tunnels are addressed by **name**. The server-side connection id is a
//! fresh UUID per connection and the client suffixes its own id per service,
//! so neither is something a configuration file can name and keep naming
//! across a restart. Addressing a peer by client id still works and binds
//! every tunnel that peer declares.
//!
//! What can be bound is discovered rather than assumed: the server lists the
//! tunnels the presented token may reach, so a name that cannot be bound is
//! reported at startup, next to the names that can, instead of failing later
//! as a dropped packet.

use std::collections::HashMap;
use std::time::Duration;
use tracing::{error, info, warn};

use crate::config::{ClientSettings, build_http_url, build_ws_url_with_path};
use crate::protocol::TunnelDecl;
use crate::tcp::bridge_connection;

/// Seconds between discovery retries while nothing is bindable yet.
const DISCOVERY_RETRY_SECS: u64 = 15;

/// Range the derived local port is drawn from when the declared one cannot be
/// reused. Above the privileged range and below the ephemeral range most
/// systems allocate from, so a derived port collides with neither.
const DERIVED_PORT_BASE: u16 = 20000;
const DERIVED_PORT_SPAN: u16 = 10000;

/// One tunnel as the server reports it (`GET /aperio/tunnels`).
#[derive(serde::Deserialize, Clone, Debug)]
pub(crate) struct TunnelView {
  pub(crate) name: String,
  pub(crate) protocol: String,
  pub(crate) target: String,
  #[serde(default)]
  pub(crate) client_id: Option<String>,
  #[serde(default)]
  pub(crate) available: bool,
  #[serde(default)]
  pub(crate) encrypt: bool,
  #[serde(default)]
  pub(crate) idle_timeout: Option<u64>,
  /// Organization that owns it, by name; absent for the master organization
  /// and for a server too old to report it.
  #[serde(default)]
  pub(crate) org: Option<String>,
  /// The token this view was discovered with, so a binding uses the
  /// credential that could actually see it. Client-side only.
  #[serde(skip)]
  pub(crate) discovered_with: Option<String>,
}

/// One resolved binding: a tunnel, where it will listen, and what it needs.
#[derive(Debug)]
struct Binding {
  /// Name the server relays by. Every binding resolves to one, including an
  /// entry keyed by a peer's client id: discovery turns that peer's
  /// declarations into names first, so there is a single address form on the
  /// wire. The server still accepts `?client=&target=` for older binders.
  name: String,
  /// Declaration, for protocol, encryption and the UDP idle timeout.
  decl: TunnelDecl,
  /// What this binding is called in logs.
  label: String,
  address: String,
  port: u16,
  token: String,
  psk: Option<String>,
}

/// Runs bind-tunnels mode until the process is stopped. `cli_id` is the value
/// of `--bind-tunnels`: a tunnel name, a peer's client id, or empty (bind
/// what the `bind-tunnels:` section names, or everything visible when it has
/// nothing to say).
pub(crate) async fn run_bind_tunnels(settings: &ClientSettings, server: &str, cli_id: &str) -> ! {
  crate::tcp::spawn_shutdown_watcher();

  let fallback_token = settings
    .token
    .clone()
    .map(|t| t.trim().to_string())
    .filter(|t| !t.is_empty());

  // Discovery is the startup check: it answers what exists, what a token may
  // bind, and whether a client is there to serve it, in one call. A binding
  // that cannot be resolved is reported now rather than accepting connections
  // that will never be relayed.
  //
  // Every configured credential is asked, because an entry may carry its own
  // `token:` and be the only one that can see its tunnel. That is the older
  // spelling's shape, where the file has no server token at all.
  let mut tokens: Vec<String> = Vec::new();
  for candidate in std::iter::once(fallback_token.clone()).chain(
    settings
      .bind_tunnels
      .values()
      .map(|value| value.entry().token.clone()),
  ) {
    let Some(token) = candidate
      .map(|t| t.trim().to_string())
      .filter(|t| !t.is_empty())
    else {
      continue;
    };
    if !tokens.contains(&token) {
      tokens.push(token);
    }
  }
  if tokens.is_empty() {
    error!(
      "CRITICAL SECURITY ERROR: a token is required (--server-token, APERIO_SERVER_TOKEN, yaml: server.token, or a bind-tunnels entry's token)"
    );
    std::process::exit(1);
  }
  let visible = discover(server, &tokens).await;

  let bindings = match plan(settings, cli_id, &visible, &fallback_token) {
    Ok(bindings) => bindings,
    Err(e) => {
      error!("{}", e);
      std::process::exit(1);
    }
  };
  if bindings.is_empty() {
    error!("Nothing to bind. {}", visible_hint(&visible));
    std::process::exit(1);
  }

  let mut claimed: HashMap<(&str, u16), String> = HashMap::new();
  let mut listeners = 0usize;
  for binding in bindings {
    // A `tcp/udp` tunnel is one name and one local port, opened on both
    // transports. They are separate port spaces, so the same number on each
    // is not a conflict.
    let mut opened = false;
    for transport in ["tcp", "udp"] {
      if !aperio_config::protocol_serves(&binding.decl.protocol, transport) {
        continue;
      }
      let claim = (transport, binding.port);
      if let Some(other) = claimed.get(&claim) {
        error!(
          "Local {} port {} is already used by {}; give {} its own `port:` — not binding",
          transport, binding.port, other, binding.label
        );
        continue;
      }
      if spawn_listener(server, &binding, transport).await {
        claimed.insert(claim, binding.label.clone());
        opened = true;
      }
    }
    if opened {
      listeners += 1;
    }
  }

  if listeners == 0 {
    error!("No tunnel could be bound; exiting.");
    std::process::exit(1);
  }
  info!(
    "{} tunnel listener(s) active. Press Ctrl+C to stop.",
    listeners
  );
  // The shutdown watcher exits the process on SIGINT/SIGTERM.
  std::future::pending().await
}

/// Names the tunnels this token can reach, for an error that tells the reader
/// what to write instead of only what failed.
fn visible_hint(visible: &[TunnelView]) -> String {
  if visible.is_empty() {
    return "This token can bind no tunnel on this server: nothing is declared, or the declaring clients are in another organization and the token lacks allow_bind.".to_string();
  }
  let names: Vec<&str> = visible.iter().map(|v| v.name.as_str()).collect();
  format!("This token can bind: {}.", names.join(", "))
}

/// Turns the configuration into concrete bindings, resolving every key
/// against what the server says is bindable.
fn plan(
  settings: &ClientSettings,
  cli_id: &str,
  visible: &[TunnelView],
  fallback_token: &Option<String>,
) -> Result<Vec<Binding>, String> {
  let entries = &settings.bind_tunnels;
  let mut planned: Vec<Binding> = Vec::new();

  // An explicit `--bind-tunnels <value>` selects one thing; its yaml entry,
  // when there is one, supplies the port and the rest.
  if !cli_id.is_empty() {
    let entry = entries.get(cli_id).map(|v| v.entry()).unwrap_or_default();
    return resolve_key(cli_id, &entry, visible, fallback_token, &mut planned)
      .map(|()| planned)
      .map_err(|e| format!("{e} {}", visible_hint(visible)));
  }

  // No value: the yaml section drives it.
  if entries.is_empty() {
    // Nothing configured either, so bind what this token can reach. This is
    // the "I just want in" case, and it is only reasonable because the server
    // decides what is on the list.
    for view in visible {
      planned.push(binding_for_view(view, &Default::default(), fallback_token)?);
    }
    return Ok(planned);
  }

  let mut failures: Vec<String> = Vec::new();
  for (key, value) in entries {
    let entry = value.entry();
    if let Err(e) = resolve_key(key, &entry, visible, fallback_token, &mut planned) {
      failures.push(e);
    }
  }
  // One unresolvable entry must not take down the bindings that did resolve:
  // this is a break-glass tool, and getting three of four tunnels up during
  // an incident beats getting none.
  for failure in &failures {
    error!("{} {}", failure, visible_hint(visible));
  }
  Ok(planned)
}

/// Resolves one `bind-tunnels:` key: a tunnel name, or a peer's client id
/// (which binds every tunnel that peer declares).
fn resolve_key(
  key: &str,
  entry: &aperio_config::BindTunnelEntry,
  visible: &[TunnelView],
  fallback_token: &Option<String>,
  out: &mut Vec<Binding>,
) -> Result<(), String> {
  let key = key.trim();
  // `<org>@<name>` addresses one organization's tunnel. A name is unique
  // inside an organization and nowhere else, so a binder that can see two of
  // them needs a way to say which; a bare name still works and matches the
  // one tunnel of that name it can see.
  if let Some((org, name)) = key.split_once('@') {
    let (org, name) = (org.trim(), name.trim());
    let found = visible.iter().find(|v| {
      v.name == name
        && match &v.org {
          Some(owner) => owner.eq_ignore_ascii_case(org),
          None => org.eq_ignore_ascii_case("master"),
        }
    });
    return match found {
      Some(view) => {
        out.push(binding_for_view(view, entry, fallback_token)?);
        Ok(())
      }
      None => Err(format!(
        "`{key}` is not a tunnel this token may bind: no `{name}` in the organization `{org}`."
      )),
    };
  }
  if let Some(view) = visible.iter().find(|v| v.name == key) {
    out.push(binding_for_view(view, entry, fallback_token)?);
    return Ok(());
  }
  // Not a name this token can bind. A client id binds everything that peer
  // declares, which is the older spelling.
  //
  // Discovery reports the process's own `client_id`, but a configuration
  // written before names existed may name one of its *connections*
  // (`<uuid>-0`, `<uuid>-c2`), since that is what the server used to answer
  // to. Either selects the process: the id is a full UUID, so a key that
  // extends it with a suffix cannot be some other client.
  let peer: Vec<&TunnelView> = visible
    .iter()
    .filter(|v| {
      v.client_id.as_deref().is_some_and(|id| {
        id == key
          || key
            .strip_prefix(id)
            .is_some_and(|rest| rest.starts_with('-'))
      })
    })
    .collect();
  if peer.is_empty() {
    return Err(format!(
      "`{key}` is neither a tunnel this token may bind nor a connected client's id."
    ));
  }
  for view in peer {
    let mut per_tunnel = entry.clone();
    // The old spelling maps a declared target to a local port; a named entry
    // uses `port:`, which cannot apply to a whole peer.
    per_tunnel.port = entry.overrides.get(view.target.trim()).copied();
    out.push(binding_for_view(view, &per_tunnel, fallback_token)?);
  }
  Ok(())
}

/// Builds the binding for one discovered tunnel.
fn binding_for_view(
  view: &TunnelView,
  entry: &aperio_config::BindTunnelEntry,
  fallback_token: &Option<String>,
) -> Result<Binding, String> {
  let token = entry
    .token
    .clone()
    .or_else(|| view.discovered_with.clone())
    .or_else(|| fallback_token.clone())
    .map(|t| t.trim().to_string())
    .filter(|t| !t.is_empty())
    .ok_or_else(|| format!("tunnel `{}` has no token to bind with", view.name))?;
  let address = entry
    .address
    .clone()
    .map(|a| a.trim().to_string())
    .filter(|a| !a.is_empty())
    .unwrap_or_else(|| "127.0.0.1".to_string());
  if address != "127.0.0.1" && address != "::1" && address != "localhost" {
    warn!(
      "Tunnel {} listens on {}, not loopback: a service deliberately kept off the network is now reachable from it",
      view.name, address
    );
  }
  if !view.available {
    warn!(
      "Tunnel {} is declared but no client can serve it right now; binding anyway, connections will fail until one is back",
      view.name
    );
  }
  Ok(Binding {
    name: view.name.clone(),
    decl: TunnelDecl {
      name: Some(view.name.clone()),
      target: view.target.clone(),
      protocol: view.protocol.clone(),
      encrypt: view.encrypt,
      psk: None,
      idle_timeout: view.idle_timeout,
      expose: None,
    },
    port: local_port(view, entry.port),
    label: format!("tunnel {}", view.name),
    address,
    token,
    psk: entry.psk.clone(),
  })
}

/// Local port for a tunnel: what the configuration asked for, else the
/// declared target's port when that is usable, else a port derived from the
/// name so it is the same on every run.
///
/// The declared port stays the default because binders in the field depend on
/// it; only the cases that used to fail outright (a privileged port, or none
/// to parse) fall through to a derived one.
fn local_port(view: &TunnelView, configured: Option<u16>) -> u16 {
  if let Some(port) = configured.filter(|p| *p > 0) {
    return port;
  }
  let declared = view
    .target
    .trim()
    .rsplit_once(':')
    .and_then(|(_, port)| port.parse::<u16>().ok())
    .filter(|p| *p >= 1024);
  declared.unwrap_or_else(|| derived_port(&view.name))
}

/// A stable local port for a tunnel name (FNV-1a, folded into the range).
fn derived_port(name: &str) -> u16 {
  let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
  for byte in name.as_bytes() {
    hash ^= *byte as u64;
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
  }
  DERIVED_PORT_BASE + (hash % DERIVED_PORT_SPAN as u64) as u16
}

/// Opens one local listener for a binding, on `transport` (`tcp` or `udp`).
/// Returns false when it could not be opened, which is reported but never
/// fatal for the other bindings.
async fn spawn_listener(server: &str, binding: &Binding, transport: &str) -> bool {
  let path = if transport == "udp" {
    "/aperio/udp"
  } else {
    "/aperio/tcp"
  };
  let ws_url = match tunnel_ws_url(server, path, &binding.name) {
    Ok(u) => u,
    Err(e) => {
      error!(
        "Failed to build the tunnel URL for {}: {}",
        binding.label, e
      );
      return false;
    }
  };

  if transport == "udp" {
    let idle_timeout = crate::udp::effective_idle_timeout(binding.decl.idle_timeout);
    info!(
      "Tunnel bound: {}:{} -> {} -> {} (udp)",
      binding.address, binding.port, binding.label, binding.decl.target
    );
    let (address, port, token) = (binding.address.clone(), binding.port, binding.token.clone());
    tokio::spawn(async move {
      crate::udp::run_udp_bind(address, port, ws_url, token, idle_timeout).await;
    });
    return true;
  }

  if binding.decl.encrypt {
    info!(
      "{} is end-to-end encrypted{}",
      binding.label,
      if binding.psk.is_some() {
        " (with PSK)"
      } else {
        " (no PSK — an actively hostile server could MITM; configure a psk on both sides)"
      }
    );
  } else if binding.psk.is_some() {
    warn!(
      "A psk is configured for {} but it is not declared encrypted; the psk is unused",
      binding.label
    );
  }

  let listener = match tokio::net::TcpListener::bind((binding.address.as_str(), binding.port)).await
  {
    Ok(l) => l,
    Err(e) => {
      error!(
        "Failed to bind {}:{} for {}: {} — not binding",
        binding.address, binding.port, binding.label, e
      );
      return false;
    }
  };
  info!(
    "Tunnel bound: {}:{} -> {} -> {} (tcp)",
    binding.address, binding.port, binding.label, binding.decl.target
  );

  let (token, encrypt, psk) = (
    binding.token.clone(),
    binding.decl.encrypt,
    binding.psk.clone(),
  );
  tokio::spawn(async move {
    loop {
      match listener.accept().await {
        Ok((sock, peer)) => {
          info!("Tunnel connection from {} -> {}", peer, ws_url);
          let (ws_url, token, psk) = (ws_url.clone(), token.clone(), psk.clone());
          tokio::spawn(async move {
            bridge_connection(sock, &ws_url, &token, encrypt, psk).await;
          });
        }
        Err(e) => {
          error!("Tunnel accept error: {}", e);
          tokio::time::sleep(Duration::from_secs(1)).await;
        }
      }
    }
  });
  true
}

/// WebSocket URL for one binding's stream endpoint.
fn tunnel_ws_url(server: &str, path: &str, name: &str) -> Result<String, String> {
  let base = build_ws_url_with_path(server, path)?;
  let mut parsed = url::Url::parse(&base).map_err(|e| e.to_string())?;
  parsed.query_pairs_mut().append_pair("tunnel", name);
  Ok(parsed.to_string())
}

/// Lists everything the configured credentials may bind, retrying while the
/// answer is empty: a binder may legitimately start before the clients it
/// binds, so nothing visible yet is a wait rather than a failure.
async fn discover(server: &str, tokens: &[String]) -> Vec<TunnelView> {
  let url = match build_http_url(server, "/aperio/tunnels") {
    Ok(u) => u,
    Err(e) => {
      error!("Failed to build the discovery URL: {}", e);
      std::process::exit(1);
    }
  };
  let http = reqwest::Client::builder()
    .timeout(Duration::from_secs(10))
    .build()
    .unwrap_or_default();

  loop {
    let mut seen: Vec<TunnelView> = Vec::new();
    for token in tokens {
      for mut view in list_for(&http, &url, token).await {
        if seen.iter().any(|v| v.name == view.name) {
          continue;
        }
        view.discovered_with = Some(token.clone());
        seen.push(view);
      }
    }
    if !seen.is_empty() {
      info!("{} tunnel(s) bindable with this configuration", seen.len());
      return seen;
    }
    warn!(
      "No tunnel is bindable yet; retrying in {}s",
      DISCOVERY_RETRY_SECS
    );
    tokio::time::sleep(Duration::from_secs(DISCOVERY_RETRY_SECS)).await;
  }
}

/// One discovery call. An empty answer and a failed one are both "nothing
/// from this token"; the caller decides whether to wait.
async fn list_for(http: &reqwest::Client, url: &str, token: &str) -> Vec<TunnelView> {
  match http.get(url).bearer_auth(token).send().await {
    Ok(resp) if resp.status().is_success() => match resp.json::<Vec<TunnelView>>().await {
      Ok(tunnels) => tunnels,
      Err(e) => {
        error!("Failed to parse the tunnel list: {}", e);
        Vec::new()
      }
    },
    Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
      error!("The server rejected a configured token (401).");
      Vec::new()
    }
    Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
      error!(
        "This server does not support tunnel discovery (404); it predates named tunnels. Upgrade it, or bind against the older server with an older client."
      );
      std::process::exit(1);
    }
    Ok(resp) => {
      warn!("Tunnel discovery returned HTTP {}", resp.status());
      Vec::new()
    }
    Err(e) => {
      warn!("Tunnel discovery failed: {}", e);
      Vec::new()
    }
  }
}

#[cfg(test)]
#[path = "bind_tunnels_tests.rs"]
mod tests;
