//! Who is dialing whose tunnel (planned_features #56).
//!
//! `--bind-tunnels` lets one client dial another client's exposed tunnel, so
//! that dependency is real: a database bound locally by three machines is
//! three services that stop working when the declaring client goes away. The
//! topology graph drew clients and routes and knew nothing about it, and a
//! dependency the graph does not show is the one nobody remembers at the
//! moment it breaks.
//!
//! ## What a consumer is identified by
//!
//! A consumer is not a registered client. It opens a fresh WebSocket to
//! `/aperio/tcp` per connection, with a token and nothing else, so there is no
//! id to group by: counted naively, every `--bind-tunnels` invocation and
//! every reconnect would be a new node, and the graph would fill with ghosts
//! of the same machine.
//!
//! So an edge is keyed by **peer address plus token**, and the node is the
//! address. That is deliberately coarse: several clients behind one NAT or one
//! Kubernetes node collapse into a single node. It is the right trade for what
//! this view answers, "who depends on this tunnel", where the answer is about
//! machines and networks rather than processes, and it is honest, an address
//! is something the server observed rather than something a caller claimed.

use std::collections::HashMap;
use std::net::IpAddr;

/// How long an edge with no live connection stays on the graph.
///
/// A dependency does not stop existing because the connection is momentarily
/// idle: `--bind-tunnels` listeners open a connection per accepted socket, so
/// a database client that has just finished a query has zero connections and
/// is still very much a dependency. Fifteen minutes is long enough to survive
/// that and short enough that a machine which is genuinely gone leaves.
pub(crate) const EDGE_TTL_SECS: u64 = 15 * 60;

/// One consumer's dependency on one serving client.
#[derive(Clone, Debug)]
pub(crate) struct Edge {
  /// Peer address the consumer dialed from.
  pub(crate) from_ip: String,
  /// Connection id of the client that serves the tunnel.
  pub(crate) to_client: String,
  /// Tunnel name, where it was dialed by name rather than by client and
  /// target.
  pub(crate) tunnel: Option<String>,
  /// Label of the token the consumer authenticated with.
  pub(crate) token_name: String,
  /// Connections currently open over this edge.
  pub(crate) active: u32,
  /// Connections opened over this edge since the server started.
  pub(crate) total: u64,
  /// Unix second of the most recent connection.
  pub(crate) last_seen: u64,
}

/// Key that makes repeated dials from one machine one edge rather than many.
type EdgeKey = (String, String, Option<String>, String);

/// Every client-to-client dependency the server has observed.
#[derive(Default)]
pub(crate) struct Consumers {
  edges: HashMap<EdgeKey, Edge>,
}

impl Consumers {
  /// Records a connection opening.
  pub(crate) fn opened(
    &mut self,
    from: IpAddr,
    to_client: &str,
    tunnel: Option<&str>,
    token_name: &str,
    now: u64,
  ) {
    let from_ip = from.to_string();
    let key = (
      from_ip.clone(),
      to_client.to_string(),
      tunnel.map(str::to_string),
      token_name.to_string(),
    );
    let edge = self.edges.entry(key).or_insert_with(|| Edge {
      from_ip,
      to_client: to_client.to_string(),
      tunnel: tunnel.map(str::to_string),
      token_name: token_name.to_string(),
      active: 0,
      total: 0,
      last_seen: now,
    });
    edge.active = edge.active.saturating_add(1);
    edge.total = edge.total.saturating_add(1);
    edge.last_seen = now;
  }

  /// Records a connection closing. The edge stays: the dependency outlives the
  /// connection, which is the whole reason this view exists.
  pub(crate) fn closed(
    &mut self,
    from: IpAddr,
    to_client: &str,
    tunnel: Option<&str>,
    token_name: &str,
    now: u64,
  ) {
    let key = (
      from.to_string(),
      to_client.to_string(),
      tunnel.map(str::to_string),
      token_name.to_string(),
    );
    if let Some(edge) = self.edges.get_mut(&key) {
      edge.active = edge.active.saturating_sub(1);
      edge.last_seen = now;
    }
  }

  /// The edges worth drawing, newest first, dropping the ones that have gone
  /// quiet.
  ///
  /// Expiry happens here rather than on a timer: this map is only read when
  /// somebody opens the topology view, and a background task to prune a
  /// handful of entries nobody is looking at is a cost with no reader.
  pub(crate) fn live(&mut self, now: u64) -> Vec<Edge> {
    self
      .edges
      .retain(|_, e| e.active > 0 || now.saturating_sub(e.last_seen) < EDGE_TTL_SECS);
    let mut out: Vec<Edge> = self.edges.values().cloned().collect();
    out.sort_by(|a, b| {
      b.last_seen
        .cmp(&a.last_seen)
        .then_with(|| a.from_ip.cmp(&b.from_ip))
    });
    out
  }
}

#[cfg(test)]
#[path = "consumers_tests.rs"]
mod tests;
