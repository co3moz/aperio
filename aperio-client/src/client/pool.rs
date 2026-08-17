//! Turning a resolved service into running connections: one task per
//! connection for a fixed `connections:`, and a supervisor that opens and
//! retires them for an elastic range.

use std::time::Duration;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::service::{self, ServiceSpec, Shared, run_service};

/// Spawns one task per service connection, each with its own cancel channel.
/// A service with `connections: N` runs as N parallel tunnel connections; the
/// first keeps the service's client id, extras derive `<id>-c2`, `<id>-c3`, …
/// so every connection has a distinct instance id (no shared-id ambiguity for
/// failover or `--bind-tunnels` lookups).
///
/// A multiplexed group is the other direction: its services are one connection
/// between them, spawned once at the first member and skipped at the rest.
pub(crate) fn spawn_services(
  specs: &[ServiceSpec],
  shared: &Shared,
) -> Vec<(watch::Sender<bool>, tokio::task::JoinHandle<()>)> {
  let mut out = Vec::new();
  let mut started: Vec<usize> = Vec::new();
  for spec in specs {
    // One ceiling per connection: the first connection learns what the server
    // permits and the rest size themselves from it instead of each finding
    // out by being closed. A multiplexed group opens one connection, so it
    // has nothing to size, but the parameter is the same shape.
    let ceiling = service::ConnectionCeiling::new();
    if let Some(group) = spec.multiplex_group {
      if started.contains(&group) {
        continue;
      }
      started.push(group);
      let members: Vec<ServiceSpec> = specs
        .iter()
        .filter(|s| s.multiplex_group == Some(group))
        .cloned()
        .collect();
      // Still one health state per service, which is the point: the list is
      // what the heartbeat reports, and a service ejected for its own backend
      // must not take the others off the connection with it.
      let healths: Vec<service::BackendHealth> = members
        .iter()
        .map(service::BackendHealth::for_spec)
        .collect();
      out.push(spawn_connection(&members, shared, &healths, &ceiling, 1));
      continue;
    }
    // One shared backend-health state per service: the backend is probed
    // once (by the first connection), not once per parallel connection.
    let health = service::BackendHealth::for_spec(spec);
    // An elastic pool runs as a single supervisor task that owns its own
    // connections. That keeps the caller's contract intact, it still holds
    // one cancel channel and one handle per entry, and it puts the decision
    // to open or retire a connection next to the state it is made from.
    if spec.connections_min < spec.connections {
      let (cancel_tx, cancel_rx) = watch::channel(false);
      let handle = tokio::spawn(run_elastic_pool(
        spec.clone(),
        shared.clone(),
        cancel_rx,
        health,
        ceiling,
      ));
      out.push((cancel_tx, handle));
      continue;
    }
    let group = [spec.clone()];
    let healths = [health];
    out.extend(
      (1..=spec.connections).map(|conn| spawn_connection(&group, shared, &healths, &ceiling, conn)),
    );
  }
  out
}

/// Starts connection number `conn` of a service, or the single connection of a
/// multiplexed group.
///
/// `group` is one service in the ordinary shape and several under `multiplex:
/// true`; `healths` is that list's backend-health state, index for index.
///
/// The two are paired into [`service::ServiceRuntime`] here, which is the last
/// point at which they are two lists. Everything downstream takes the paired
/// form, so a caller cannot hand the connection a health state belonging to a
/// different service than the spec beside it.
pub(crate) fn spawn_connection(
  group: &[ServiceSpec],
  shared: &Shared,
  healths: &[service::BackendHealth],
  ceiling: &service::ConnectionCeiling,
  conn: u32,
) -> (watch::Sender<bool>, tokio::task::JoinHandle<()>) {
  let mut group = group.to_vec();
  if conn > 1 {
    // The connection's id is the first service's, so that is the one the
    // per-connection suffix goes on. A multiplexed group is always connection
    // 1 and never reaches this.
    group[0].client_id = format!("{}-c{}", group[0].client_id, conn);
  }
  let services: Vec<service::ServiceRuntime> = group
    .into_iter()
    .zip(healths.iter().cloned())
    .map(|(spec, health)| service::ServiceRuntime::new(spec, health))
    .collect();
  let (cancel_tx, cancel_rx) = watch::channel(false);
  let handle = tokio::spawn(run_service(
    services,
    shared.clone(),
    cancel_rx,
    conn == 1,
    conn,
    ceiling.clone(),
  ));
  (cancel_tx, handle)
}

/// The number to give a pool's next connection: the lowest one not in use.
///
/// Not `len + 1`. Entries do not only leave a pool from the end, a connection
/// past the server's announced ceiling stands down by itself, so after one of
/// those the length and the highest number in use are different things and
/// counting from the length hands out a number a live connection is already
/// answering to. Two clients with one id is exactly the ambiguity the
/// per-connection suffix exists to prevent.
pub(crate) fn next_connection_number(taken: impl IntoIterator<Item = u32>) -> u32 {
  let taken: Vec<u32> = taken.into_iter().collect();
  (1..).find(|n| !taken.contains(n)).unwrap_or(1)
}

/// How often the elastic pool looks at its load.
const POOL_TICK: Duration = Duration::from_secs(2);
/// Requests in flight per connection above which the pool opens another one.
///
/// A tunnel connection multiplexes requests, so this is not a hard capacity,
/// it is the point at which a connection's frames start queueing behind each
/// other rather than going out as they arrive.
const POOL_GROW_PER_CONNECTION: usize = 8;
/// Load per connection below which the pool gives one back.
///
/// Deliberately well under the growth figure: the gap is the hysteresis that
/// stops a service sitting between the two thresholds from opening and closing
/// a connection every few seconds, which costs both ends more than the
/// connection ever saved.
const POOL_SHRINK_PER_CONNECTION: usize = 2;
/// Quiet period after growing before the pool may grow again. One connection
/// at a time, with a pause to see whether it helped.
const POOL_GROW_COOLDOWN: Duration = Duration::from_secs(10);
/// Quiet period before the pool gives a connection back. Much longer than the
/// growth cooldown on purpose: being one connection too many costs a little
/// memory, being one too few costs latency on live traffic, so the pool is
/// eager to grow and reluctant to shrink.
const POOL_SHRINK_COOLDOWN: Duration = Duration::from_secs(120);

/// Runs a service whose `connections:` is a range, opening `min` connections
/// and growing towards `max` while the pool is busy.
///
/// Growth is driven by requests in flight rather than by a request *rate*: a
/// thousand requests a second that all answer in a millisecond need one
/// connection, and ten slow uploads need room to run in parallel. In flight is
/// the quantity that tells those apart.
pub(crate) async fn run_elastic_pool(
  spec: ServiceSpec,
  shared: Shared,
  mut cancel_rx: watch::Receiver<bool>,
  health: service::BackendHealth,
  ceiling: service::ConnectionCeiling,
) {
  // The connection number is carried alongside the handle rather than implied
  // by the position, because entries do not only leave from the end: a
  // connection past the server's ceiling stands down on its own, and deriving
  // the next number from the length would then hand out a number a live
  // connection is already using.
  let mut pool: Vec<(u32, watch::Sender<bool>, tokio::task::JoinHandle<()>)> = Vec::new();
  for conn in 1..=spec.connections_min {
    let (cancel_tx, handle) = spawn_connection(
      std::slice::from_ref(&spec),
      &shared,
      std::slice::from_ref(&health),
      &ceiling,
      conn,
    );
    pool.push((conn, cancel_tx, handle));
  }
  spec.pool_load.set_open(spec.connections_min);
  info!(
    "[{}] Elastic pool: {} connection(s) open, growing to {} under load",
    spec.client_id, spec.connections_min, spec.connections
  );
  let mut grew_at = tokio::time::Instant::now();
  let mut shrank_at = tokio::time::Instant::now();
  let mut ticker = tokio::time::interval(POOL_TICK);
  ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
  loop {
    tokio::select! {
      changed = cancel_rx.changed() => {
        if changed.is_err() || *cancel_rx.borrow() {
          break;
        }
      }
      _ = ticker.tick() => {
        // A connection whose task has ended is not open, whatever the pool
        // spawned. The server announces a per-service ceiling and a
        // connection above it stands down by returning, so a pool told to
        // start more than the server allows was counting connections that had
        // never opened: the dashboard and the Ping reported them, and the
        // growth arithmetic divided by them.
        let before = pool.len();
        pool.retain(|(_, _, handle)| !handle.is_finished());
        if pool.len() != before {
          warn!(
            "[{}] {} connection(s) of this pool are not running (the server's \
             ceiling, or a connection that gave up); the pool is {} deep",
            spec.client_id,
            before - pool.len(),
            pool.len()
          );
          spec.pool_load.set_open(pool.len() as u32);
        }
        let peak = spec.pool_load.take_peak();
        let open = pool.len() as u32;
        let now = tokio::time::Instant::now();
        // The server's announced ceiling wins over the file: asking for a
        // connection it will refuse just burns a handshake.
        let permitted = ceiling.permitted().unwrap_or(spec.connections).min(spec.connections);
        if open < permitted
          && peak >= open as usize * POOL_GROW_PER_CONNECTION
          && now.duration_since(grew_at) >= POOL_GROW_COOLDOWN
        {
          let conn = next_connection_number(pool.iter().map(|(c, _, _)| *c));
          info!(
            "[{}] {} request(s) in flight over {} connection(s); opening connection {}",
            spec.client_id, peak, open, conn
          );
          let (cancel_tx, handle) = spawn_connection(std::slice::from_ref(&spec), &shared, std::slice::from_ref(&health), &ceiling, conn);
          pool.push((conn, cancel_tx, handle));
          spec.pool_load.set_open(pool.len() as u32);
          grew_at = now;
          shrank_at = now;
          continue;
        }
        if open > spec.connections_min
          && peak <= (open as usize - 1) * POOL_SHRINK_PER_CONNECTION
          && now.duration_since(shrank_at) >= POOL_SHRINK_COOLDOWN
        {
          if let Some((conn, cancel_tx, handle)) = pool.pop() {
            info!(
              "[{}] Load dropped to {} request(s) in flight over {} connection(s); \
               retiring connection {} (pool floor is {})",
              spec.client_id, peak, open, conn, spec.connections_min
            );
            let _ = cancel_tx.send(true);
            // Awaited rather than detached: the retired connection's client id
            // is `<id>-c<open>`, and the pool hands that same number out again
            // the next time it grows. Letting a draining connection overlap
            // with its replacement would put two clients with one id in front
            // of the server, which is exactly the ambiguity the per-connection
            // suffix exists to prevent.
            let _ = handle.await;
            spec.pool_load.set_open(pool.len() as u32);
          }
          shrank_at = tokio::time::Instant::now();
          grew_at = shrank_at;
        }
      }
    }
  }
  for (_, cancel_tx, _) in &pool {
    let _ = cancel_tx.send(true);
  }
  for (_, _, handle) in pool {
    let _ = handle.await;
  }
}
