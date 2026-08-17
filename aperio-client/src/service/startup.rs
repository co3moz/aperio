//! What has to be true before a connection is allowed to dial for the first
//! time: it has something to serve, whatever it depends on is up, its startup
//! delay has passed, and the server's announced ceiling leaves room for it.
//!
//! All four gates run once per connection and never again, which is why they
//! are here rather than in the reconnect loop: a reconnect is the same
//! connection coming back, not a new one asking permission.

use super::*;

/// What the gates leave behind for the connection to run on.
pub(crate) struct Prepared {
  /// The connection's own view. A connection carrying several services is
  /// still one socket, one client id and one heartbeat, so one of its services
  /// has to stand for it; what a *request* is about is resolved per request.
  pub(crate) spec: ServiceSpec,
  pub(crate) multiplexed: bool,
  pub(crate) label: String,
  /// The health probes this connection owns, to be aborted when it is done.
  pub(crate) probe_tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// Runs the gates in order. `None` means this connection should not open at
/// all, and the reason has already been logged.
pub(crate) async fn prepare(
  services: &[ServiceRuntime],
  shared: &Shared,
  run_probe: bool,
  connection_index: u32,
  ceiling: &ConnectionCeiling,
) -> Option<Prepared> {
  // The connection's own view. Everything about the socket, the dial, the
  // client id and the heartbeat is the first service's, because a connection
  // carrying several is still one connection and one of its services has to
  // stand for it. What a *request* is about is resolved per request, from the
  // service the server names in the frame.
  // A connection with nothing to serve is not a connection. The *other* half
  // of what this used to check, that the specs and their health states were
  // the same length, is now [`ServiceRuntime`]'s job and cannot be got wrong.
  if services.is_empty() {
    error!("Refusing to open a connection: it was given no services to serve");
    return None;
  }
  let spec = services[0].spec.clone();
  // A connection carrying several services is labelled by how many, not by the
  // first of them: every line about the connection would otherwise read as
  // being about one service, and the other services' own lines already carry
  // their own labels.
  let multiplexed = services.len() > 1;
  let label = if multiplexed {
    format!("{} services", services.len())
  } else {
    spec.label()
  };

  // Lifecycle gates, before anything is dialed. Only the first connection of a
  // pool waits: the others are the same service, and making each of them sit
  // through the same delay would turn a five-second stagger into a
  // five-second-per-connection one.
  //
  // A multiplexed connection waits for every one of its services: they are
  // opening one socket together, so the last one that is ready is when it can
  // open. The waits run in sequence and the delays do not add up in a way that
  // matters, `depends_on` is a shared grace period rather than a per-service
  // one, and `startup_delay` is taken as the longest rather than the sum.
  if connection_index == 1 {
    let depends_on: Vec<String> = {
      let mut all: Vec<String> = services
        .iter()
        .flat_map(|s| s.spec.depends_on.clone())
        .collect();
      // A service of this connection cannot wait for a service of this
      // connection: nothing would ever come up. Dropped rather than refused,
      // because the file is not wrong, it is describing an order that
      // multiplexing has made moot by putting both on one socket.
      all.retain(|d| {
        !services
          .iter()
          .any(|s| s.spec.name.as_deref() == Some(d.as_str()))
      });
      all.sort();
      all.dedup();
      all
    };
    let startup_delay = services
      .iter()
      .map(|s| s.spec.startup_delay)
      .max()
      .unwrap_or(0);
    if !depends_on.is_empty() {
      let missing = await_dependencies(shared, &depends_on).await;
      if !missing.is_empty() {
        warn!(
          "[{}] depends_on: {} did not come up within {}s; starting anyway",
          label,
          missing.join(", "),
          DEPENDS_ON_GRACE.as_secs()
        );
      }
    }
    if startup_delay > 0 {
      info!(
        "[{}] startup_delay: waiting {}s before opening the tunnel",
        label, startup_delay
      );
      tokio::time::sleep(Duration::from_secs(startup_delay)).await;
    }
  }

  // Connections beyond the first wait for the server's announced ceiling
  // before opening a socket. Five seconds is the whole budget: past that the
  // server is either old (no announcement) or slow, and in both cases trying
  // is better than a connection that never happens.
  if connection_index > 1
    && let Some(permitted) = ceiling.learned(Duration::from_secs(5)).await
    && connection_index > permitted
  {
    warn!(
      "[{}] The server permits {} parallel connection(s) for this service; \
     connection {} stands down. Raise max_connections_per_service on the server \
     (or the token's max_connections) to use more.",
      label, permitted, connection_index
    );
    return None;
  }
  // Backend health is per service and shared across a service's parallel
  // connections (created once by the supervisor, one per spec). This connection
  // reports every one of them in its heartbeat and, when it owns the probes,
  // drives the probe/gate that updates each.
  let probe_tasks: Vec<tokio::task::JoinHandle<()>> = if run_probe {
    services
      .iter()
      .flat_map(|s| {
        [
          spawn_health_probe(&s.spec, &s.health),
          spawn_backend_wait(&s.spec, &s.health),
        ]
      })
      .flatten()
      .collect()
  } else {
    Vec::new()
  };
  Some(Prepared {
    spec,
    multiplexed,
    label,
    probe_tasks,
  })
}
