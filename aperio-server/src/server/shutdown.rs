//! Serving until told to stop, and stopping properly: the listener, the role
//! gate on the dashboard's own routes, the drain that lets in-flight requests
//! finish, and the signals that start it.

use crate::protocol::TunnelMessage;
use crate::state::AppState;
use crate::*;
use axum::serve::ListenerExt;
use axum::{Router, extract::ws::Message};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

/// Binds the listener (plain or SO_REUSEPORT), switches Nagle off per
/// accepted socket, and serves the app until the shutdown signal, exiting
/// the process when the port cannot be bound, exactly as before the split.
pub(crate) async fn serve_until_shutdown(state: Arc<AppState>, app: Router) {
  let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

  let port = std::env::var("PORT")
    .ok()
    .and_then(|p| p.parse::<u16>().ok())
    .unwrap_or(8080);

  // Zero-downtime restarts (APERIO_REUSEPORT): bind with SO_REUSEPORT so a new
  // process can bind the same host:port while the old one is still draining its
  // tunnels. Deploy = start the new process, then SIGTERM the old one; the
  // kernel load-balances new connections across both during the overlap, and
  // clients reconnect to whichever process survives.
  let reuseport = std::env::var("APERIO_REUSEPORT")
    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let listener = match bind_listener(&host, port, reuseport).await {
    Ok(l) => l,
    Err(e) => {
      error!("Failed to bind {}:{}: {}", host, port, e);
      std::process::exit(1);
    }
  };

  info!(
    "Aperio Server v{} listening on {}:{} with connection info tracing enabled{}",
    env!("CARGO_PKG_VERSION"),
    host,
    port,
    if reuseport { " (SO_REUSEPORT)" } else { "" }
  );

  // Nagle off on every accepted connection. These sockets carry messages
  // somebody is waiting for, not a bulk stream, and Nagle holds a small write
  // back until an outstanding acknowledgement arrives, which here is latency
  // added to a request rather than bandwidth saved. `tap_io` is axum 0.8's
  // hook for exactly this; 0.7 owned the accept loop and offered none.
  let listener = listener.tap_io(|stream| {
    if let Err(e) = stream.set_nodelay(true) {
      warn!("could not set TCP_NODELAY on an accepted connection: {e}");
    }
  });

  axum::serve(
    listener,
    app.into_make_service_with_connect_info::<SocketAddr>(),
  )
  .with_graceful_shutdown(shutdown_signal(state.clone()))
  .await
  .unwrap();
}

/// Minimum dashboard role a route requires. User management and server
/// settings can change who controls the server, admin only. Everything
/// else: reads for viewers, mutations for operators.
pub(crate) fn required_role(path: &str, method: &axum::http::Method) -> crate::store::users::Role {
  use crate::store::users::Role;
  // Self-service routes (own TOTP enrollment): any signed-in role.
  if path.starts_with("/api/me/") {
    return Role::Viewer;
  }
  if path.starts_with("/api/users")
    || path == "/api/settings"
    // The dump contains token/password hashes and TOTP secrets, and an
    // import replaces them, admin only, even for the GET.
    || path == "/api/export"
    || path == "/api/import"
    // Session management exposes who is signed in from which IP/UA and can
    // end other admins' sessions, admin only, including the list.
    || path == "/api/sessions"
    || path.starts_with("/api/sessions/")
    // Organization management is master-super-admin only (checked in the
    // handlers); require at least Admin at the routing layer.
    || path == "/api/orgs"
    || path.starts_with("/api/orgs/")
    // Programmatic admin keys are powerful cross-org credentials, master
    // super-admin only (also checked in the handlers).
    || path == "/api/admin-keys"
    || path.starts_with("/api/admin-keys/")
  {
    return Role::Admin;
  }
  if matches!(*method, axum::http::Method::GET | axum::http::Method::HEAD) {
    Role::Viewer
  } else {
    Role::Operator
  }
}

/// Ceiling on `shutdown_drain: auto`.
///
/// `auto` sizes the drain from what connected clients announce, and a client
/// is not the operator: a value it invents cannot be allowed to hold the
/// process past what the platform will wait before sending SIGKILL. Thirty
/// seconds is the common orchestrator grace period, which is the number this
/// is actually racing.
pub(crate) const SHUTDOWN_DRAIN_AUTO_CAP: u64 = 30;

/// How long shutdown waits for in-flight proxied requests, and where the
/// number came from.
///
/// Split out so the policy is testable on its own: the loop that waits is
/// timing and signals, and this is the decision.
pub(crate) fn shutdown_drain_budget(
  configured: Option<u64>,
  auto: bool,
  announced: impl IntoIterator<Item = u64>,
) -> Duration {
  if let Some(secs) = configured {
    return Duration::from_secs(secs);
  }
  if !auto {
    return Duration::ZERO;
  }
  // The longest of them, not the average: the drain is over when the slowest
  // client has finished, and an average would cut short exactly the client
  // that needed the time.
  let longest = announced.into_iter().max().unwrap_or(0);
  Duration::from_secs(longest.min(SHUTDOWN_DRAIN_AUTO_CAP))
}

/// Waits for in-flight proxied requests to finish, up to the configured
/// budget.
///
/// Polled rather than signalled: the alternative is a notification on the
/// request bookkeeping's hot path, paid on every request for the benefit of
/// the one moment the process is shutting down.
async fn drain_in_flight(state: &Arc<AppState>) {
  let cfg = state.config();
  let announced: Vec<u64> = {
    let clients = state.clients.read().await;
    clients.values().filter_map(|c| c.drain_secs).collect()
  };
  let budget = shutdown_drain_budget(cfg.shutdown_drain, cfg.shutdown_drain_auto, announced);
  if budget.is_zero() {
    return;
  }
  let deadline = tokio::time::Instant::now() + budget;
  let mut announced_wait = false;
  loop {
    let pending = state.pending_requests.lock().await.len();
    if pending == 0 {
      if announced_wait {
        info!("In-flight requests finished; continuing shutdown");
      }
      return;
    }
    if tokio::time::Instant::now() >= deadline {
      warn!(
        "Shutdown drain of {}s expired with {} request(s) still in flight; ending them",
        budget.as_secs(),
        pending
      );
      return;
    }
    if !announced_wait {
      info!(
        "Waiting up to {}s for {} in-flight request(s) to finish",
        budget.as_secs(),
        pending
      );
      announced_wait = true;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

/// Graceful shutdown listener for receiving SIGINT or SIGTERM signals.
/// Before handing control back to axum (which drops the tunnel sockets), a
/// `ServerShutdown` message is broadcast to every connected client so they
/// reconnect aggressively instead of waiting out their normal backoff.
async fn shutdown_signal(state: Arc<AppState>) {
  let ctrl_c = async {
    tokio::signal::ctrl_c()
      .await
      .expect("Failed to install Ctrl+C handler");
  };

  #[cfg(unix)]
  let terminate = async {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
      .expect("Failed to install SIGTERM handler")
      .recv()
      .await;
  };

  #[cfg(not(unix))]
  let terminate = std::future::pending::<()>();

  tokio::select! {
      _ = ctrl_c => {},
      _ = terminate => {},
  }

  info!("Shutdown signal received, closing Aperio Server connections...");

  if let Ok(json) = serde_json::to_string(&TunnelMessage::ServerShutdown {}) {
    let clients = state.clients.read().await;
    let notified = clients.len();
    for client in clients.values() {
      // try_send: a client with a full queue must not stall the shutdown.
      let _ = client.tx.try_send(Message::Text(json.clone().into()));
    }
    drop(clients);
    if notified > 0 {
      info!("Notified {} tunnel client(s) of the shutdown", notified);
      // Give the writer tasks a moment to flush the frame out.
      tokio::time::sleep(Duration::from_millis(200)).await;
    }
  }

  // Let what is already in flight finish before the connections carrying it
  // are ended. Behind a load balancer this is the number that decides whether
  // a deploy is invisible or shows up as a handful of 502s: the balancer needs
  // long enough to take this instance out of rotation, and the requests it
  // already sent need long enough to answer.
  drain_in_flight(&state).await;

  // Graceful shutdown only completes once every connection has ended, and
  // long-lived ones never end on their own. End them actively: dashboard SSE
  // streams watch this flag, and each tunnel read loop honors its disconnect
  // notify.
  let _ = state.shutdown.send(true);
  {
    let clients = state.clients.read().await;
    for client in clients.values() {
      client.disconnect.notify_waiters();
    }
  }

  // Last resort: anything still holding a connection open (a proxied
  // WebSocket/TCP/UDP relay, a stalled peer) must not keep the process alive
  // forever. Flush what matters and exit.
  let fallback = state.clone();
  let timeout = state.config().shutdown_timeout;
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_secs(timeout)).await;
    warn!("Graceful shutdown timed out after {timeout}s; forcing exit");
    fallback.persistent_stats.lock().await.save_if_dirty();
    fallback.uptime.lock().await.save_if_dirty();
    fallback.activity.lock().await.save_if_dirty();
    std::process::exit(0);
  });
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;
