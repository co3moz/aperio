//! `aperio-client --bind-tunnels`: local listeners for the tunnels a peer
//! client declared in its `tunnels:` list.
//!
//! This is an emergency fallback path, not a load-bearing proxy: a normally
//! unexposed service (say a database) declared as a tunnel by a running
//! client can be reached by starting another client with the SAME token and
//! that client's id. Each declared tunnel becomes a local 127.0.0.1
//! listener (port = the declared target's port, overridable per target);
//! every accepted connection is relayed through the server to the declaring
//! client, which dials its local target.

use std::collections::HashMap;
use std::time::Duration;
use tracing::{error, info, warn};

use crate::config::{ClientSettings, build_http_url, build_ws_url_with_path};
use crate::protocol::TunnelDecl;
use crate::tcp::bridge_connection;

/// How to reach (and locally map) the tunnels of one peer client.
struct BindSpec {
  client_id: String,
  token: String,
  /// Declared target → local port override.
  overrides: HashMap<String, u16>,
}

/// Seconds between discovery retries for peers that are not connected yet.
const DISCOVERY_RETRY_SECS: u64 = 15;

/// Runs bind-tunnels mode until the process is stopped. `cli_id` is the
/// value of `--bind-tunnels` (empty = bind every entry of the local
/// `bind-tunnels:` yaml section).
pub(crate) async fn run_bind_tunnels(settings: &ClientSettings, server: &str, cli_id: &str) -> ! {
  let specs = build_bind_specs(settings, cli_id).unwrap_or_else(|e| {
    error!("{}", e);
    std::process::exit(1);
  });

  info!(
    "Bind-tunnels mode: {} peer client(s) configured on {}",
    specs.len(),
    server
  );

  // Discover each peer's declared tunnels. A peer that is not connected yet
  // is retried in the background so the binder can be started first.
  let (ready_tx, mut ready_rx) = tokio::sync::mpsc::channel::<(BindSpec, Vec<TunnelDecl>)>(16);
  for spec in specs {
    let server = server.to_string();
    let ready_tx = ready_tx.clone();
    tokio::spawn(async move {
      let tunnels = discover_with_retry(&server, &spec).await;
      let _ = ready_tx.send((spec, tunnels)).await;
    });
  }
  drop(ready_tx);

  // Local ports already claimed, for cross-client conflict detection:
  // port → (client id, declared target).
  let mut claimed: HashMap<u16, (String, String)> = HashMap::new();
  let mut listeners = 0usize;

  while let Some((spec, tunnels)) = ready_rx.recv().await {
    if tunnels.is_empty() {
      warn!(
        "Peer client {} declares no tunnels; nothing to bind",
        spec.client_id
      );
      continue;
    }
    let server = server.to_string();
    for decl in tunnels {
      let Some(port) = local_port_for(&spec, &decl) else {
        error!(
          "Cannot derive a local port for tunnel {} of client {}; add an override rule",
          decl.target, spec.client_id
        );
        continue;
      };
      if let Some((other_client, other_target)) = claimed.get(&port) {
        error!(
          "Local port {} conflicts: client {} target {} and client {} target {} — define an override rule (bind-tunnels: {}: override: '{}': <port>); not binding",
          port,
          other_client,
          other_target,
          spec.client_id,
          decl.target,
          spec.client_id,
          decl.target
        );
        continue;
      }
      let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
          error!(
            "Failed to bind 127.0.0.1:{} for tunnel {} of client {}: {} — not binding",
            port, decl.target, spec.client_id, e
          );
          continue;
        }
      };
      claimed.insert(port, (spec.client_id.clone(), decl.target.clone()));
      listeners += 1;
      info!(
        "Tunnel bound: 127.0.0.1:{} -> client {} -> {} ({})",
        port, spec.client_id, decl.target, decl.protocol
      );

      let ws_url = match tunnel_ws_url(&server, &spec.client_id, &decl.target) {
        Ok(u) => u,
        Err(e) => {
          error!("Failed to build tunnel URL: {}", e);
          continue;
        }
      };
      let token = spec.token.clone();
      tokio::spawn(async move {
        loop {
          match listener.accept().await {
            Ok((sock, peer)) => {
              info!("Tunnel connection from {} -> {}", peer, ws_url);
              let (ws_url, token) = (ws_url.clone(), token.clone());
              tokio::spawn(async move {
                bridge_connection(sock, &ws_url, &token).await;
              });
            }
            Err(e) => {
              error!("Tunnel accept error: {}", e);
              tokio::time::sleep(Duration::from_secs(1)).await;
            }
          }
        }
      });
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
  let _ = tokio::signal::ctrl_c().await;
  info!("Shutting down bind-tunnels mode.");
  std::process::exit(0);
}

/// Resolves the configured peers: an explicit `--bind-tunnels <id>` selects
/// one (its yaml entry supplies token/overrides when present, the layered
/// token otherwise); without a value every `bind-tunnels:` yaml entry runs.
fn build_bind_specs(settings: &ClientSettings, cli_id: &str) -> Result<Vec<BindSpec>, String> {
  let trimmed_entry = |overrides: &HashMap<String, u16>| -> HashMap<String, u16> {
    overrides
      .iter()
      .map(|(k, v)| (k.trim().to_string(), *v))
      .collect()
  };

  if !cli_id.is_empty() {
    let entry = settings.bind_tunnels.get(cli_id);
    let token = entry
      .and_then(|e| e.token.clone())
      .or_else(|| settings.token.clone())
      .filter(|t| !t.trim().is_empty())
      .ok_or(
        "CRITICAL SECURITY ERROR: a tunnel token is required (--server-token, APERIO_SERVER_TOKEN, or the bind-tunnels entry's token) — it must be the SAME token the peer client connected with!",
      )?;
    return Ok(vec![BindSpec {
      client_id: cli_id.to_string(),
      token,
      overrides: entry
        .map(|e| trimmed_entry(&e.overrides))
        .unwrap_or_default(),
    }]);
  }

  if settings.bind_tunnels.is_empty() {
    return Err(
      "CRITICAL ERROR: --bind-tunnels without a client id needs a bind-tunnels: section in aperio.yaml".to_string(),
    );
  }
  settings
    .bind_tunnels
    .iter()
    .map(|(id, entry)| {
      let token = entry
        .token
        .clone()
        .or_else(|| settings.token.clone())
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| {
          format!(
            "CRITICAL SECURITY ERROR: bind-tunnels entry '{}' has no token and no layered token is configured",
            id
          )
        })?;
      Ok(BindSpec {
        client_id: id.trim().to_string(),
        token,
        overrides: trimmed_entry(&entry.overrides),
      })
    })
    .collect()
}

/// Local listener port for one declared tunnel: the override for that target
/// wins, otherwise the port of the declared target itself.
fn local_port_for(spec: &BindSpec, decl: &TunnelDecl) -> Option<u16> {
  if let Some(p) = spec.overrides.get(decl.target.trim()) {
    return Some(*p);
  }
  decl
    .target
    .rsplit_once(':')
    .and_then(|(_, port)| port.parse::<u16>().ok())
}

/// WebSocket URL selecting a specific peer client and declared target on the
/// server's `/aperio/tcp` endpoint.
fn tunnel_ws_url(server: &str, client_id: &str, target: &str) -> Result<String, String> {
  let base = build_ws_url_with_path(server, "/aperio/tcp")?;
  let mut parsed = url::Url::parse(&base).map_err(|e| e.to_string())?;
  parsed
    .query_pairs_mut()
    .append_pair("client", client_id)
    .append_pair("target", target);
  Ok(parsed.to_string())
}

/// Fetches the peer's declared tunnels from the server, retrying while the
/// peer is not connected (yet). Fatal on authentication errors — retrying
/// cannot fix a wrong token.
async fn discover_with_retry(server: &str, spec: &BindSpec) -> Vec<TunnelDecl> {
  let url = match build_http_url(server, &format!("/aperio/tunnels/{}", spec.client_id)) {
    Ok(u) => u,
    Err(e) => {
      error!("Failed to build discovery URL: {}", e);
      std::process::exit(1);
    }
  };
  let http = reqwest::Client::builder()
    .timeout(Duration::from_secs(10))
    .build()
    .unwrap_or_default();

  loop {
    match http.get(&url).bearer_auth(&spec.token).send().await {
      Ok(resp) if resp.status().is_success() => match resp.json::<Vec<TunnelDecl>>().await {
        Ok(tunnels) => {
          info!(
            "Client {} declares {} tunnel(s)",
            spec.client_id,
            tunnels.len()
          );
          return tunnels;
        }
        Err(e) => error!("Failed to parse tunnel list for {}: {}", spec.client_id, e),
      },
      Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
        error!(
          "Server rejected the token for client {} (401). --bind-tunnels requires the SAME token that client connected with.",
          spec.client_id
        );
        std::process::exit(1);
      }
      Ok(resp) if resp.status() == reqwest::StatusCode::FORBIDDEN => {
        error!(
          "Token mismatch for client {} (403): --bind-tunnels requires the SAME token that client connected with.",
          spec.client_id
        );
        std::process::exit(1);
      }
      Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
        warn!(
          "Client {} is not connected (yet); retrying in {}s",
          spec.client_id, DISCOVERY_RETRY_SECS
        );
      }
      Ok(resp) => warn!(
        "Tunnel discovery for {} returned HTTP {}; retrying in {}s",
        spec.client_id,
        resp.status(),
        DISCOVERY_RETRY_SECS
      ),
      Err(e) => warn!(
        "Tunnel discovery for {} failed: {}; retrying in {}s",
        spec.client_id, e, DISCOVERY_RETRY_SECS
      ),
    }
    tokio::time::sleep(Duration::from_secs(DISCOVERY_RETRY_SECS)).await;
  }
}
