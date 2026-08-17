use super::stream::RequestTimeline;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::VecDeque;

/// A fully captured HTTP transaction for the dashboard inspector. Bodies are
/// capped at [`CAPTURE_BODY_LIMIT`] bytes; larger bodies are truncated for
/// display and cannot be replayed.
#[derive(Serialize, Clone)]
pub(crate) struct CapturedRequest {
  /// Request UUID (matches the RequestLog id).
  pub(crate) id: String,
  /// Timestamp formatted as string.
  pub(crate) timestamp: String,
  pub(crate) method: String,
  /// Full request URI including query string.
  pub(crate) uri: String,
  /// Request headers as forwarded to the tunnel client.
  pub(crate) req_headers: Vec<(String, String)>,
  /// Base64 request body (possibly truncated).
  pub(crate) req_body: Option<String>,
  /// True when the request body exceeded the capture limit.
  pub(crate) req_body_truncated: bool,
  pub(crate) status: u16,
  pub(crate) resp_headers: Vec<(String, String)>,
  /// Base64 response body (buffered responses only, possibly truncated).
  pub(crate) resp_body: Option<String>,
  pub(crate) resp_body_truncated: bool,
  /// True when the response body was streamed (not captured).
  pub(crate) resp_streamed: bool,
  pub(crate) duration_ms: u128,
  /// High-resolution stage timeline (buffered responses of v2+ clients).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) timeline: Option<RequestTimeline>,
  /// Connection id of the client that served it. Unique and unreadable, so
  /// it is what an action addresses rather than what the screen shows.
  pub(crate) client_id: String,
  /// What that client calls itself, if anything: the operator's
  /// `custom_name`, else the `name` of its `services:` entry. `None` for a
  /// client that declared neither, where the id is all there is.
  pub(crate) client_name: Option<String>,
  /// Organization of the client that served the request (None = master). The
  /// inspector and replay are gated to the caller's effective org on this.
  #[serde(skip)]
  pub(crate) org_id: Option<String>,
}

/// Maximum number of captured requests kept in memory.
pub(crate) const CAPTURE_MAX_ENTRIES: usize = 50;

/// Makes room for one capture, evicting from whichever organization is
/// holding the most (planned_features #69).
///
/// The ring used to evict from the front, which is fair only if every
/// organization is equally busy. It is not: one org serving a thousand
/// requests a minute walked a quiet org's captures out of the buffer within
/// seconds, so a tenant investigating one request an hour could never find it.
/// Multi-tenant hygiene the byte and client quotas already had, and this one
/// did not.
///
/// A fair share rather than a fixed per-org ceiling, and that is the design
/// decision worth naming. A ceiling has to be chosen, and it interacts badly
/// with the total: five orgs capped at twenty each is a hundred in a buffer
/// that holds fifty, so the total cap evicts across tenants again and the
/// ceiling bought nothing. Evicting from the largest holder needs no number,
/// converges on an even split by itself, and lets one org use the whole buffer
/// while it is the only one there, which is what an operator would want.
pub(crate) fn evict_for_fairness(captured: &mut VecDeque<CapturedRequest>) {
  // Whoever holds the most. Ties go to the org whose oldest capture is oldest,
  // which is the front-eviction rule applied *within* the tie rather than
  // across tenants, and it is why the winner is found by walking the deque
  // rather than by taking a maximum out of the map: a `HashMap` iterates in an
  // arbitrary order, so a tie would otherwise be broken differently on every
  // call and two equally busy tenants would take turns at random.
  let mut counts: HashMap<Option<&str>, usize> = HashMap::new();
  for entry in captured.iter() {
    *counts.entry(entry.org_id.as_deref()).or_insert(0) += 1;
  }
  let Some(most) = counts.values().copied().max() else {
    return;
  };
  if let Some(at) = captured
    .iter()
    .position(|e| counts.get(&e.org_id.as_deref()) == Some(&most))
  {
    captured.remove(at);
  }
}
/// Maximum captured body size per direction (decoded bytes).
pub(crate) const CAPTURE_BODY_LIMIT: usize = 64 * 1024;
/// Request bodies above this size are streamed to v2 clients as
/// RequestStart/Chunk/End frames instead of being buffered in memory.
pub(crate) const REQUEST_STREAM_THRESHOLD: u64 = 256 * 1024;

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
