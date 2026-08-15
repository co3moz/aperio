//! The embedded profile: the written minimum of the tunnel protocol
//! (`planned_features.md` #101).
//!
//! An ESP32 cannot run `aperio-client`, and the reason is not the tunnel. It
//! is TLS with a root store, a full HTTP client, yaml plus a JSON Schema, the
//! admin CLI, the messaging faces, the OTel bridge, the health prober and the
//! autoscaling hooks. Porting is the wrong verb. What was missing is a
//! statement of **which messages a device must speak, which it may ignore,
//! and which the server undertakes never to send it.**
//!
//! That statement lives here rather than in a document, and the difference is
//! the point: [`reach`] is an exhaustive `match` over every variant of
//! [`TunnelMessage`], so **a new message type cannot be added without
//! classifying it.** A document would have been true on the day it was
//! written. `docs/embedded-profile.md` is checked against this table by a
//! test, so the two cannot drift either.
//!
//! What this is not, yet: the server does not gate itself on a declared
//! profile. `Reach::NeverSent` is a promise about the traffic a device-shaped
//! client attracts, which today it earns by not declaring the features that
//! produce those messages, rather than by the server refusing to send them.
//! Making it a negotiated capability the server enforces is `#116`, and until
//! that ships a device implementer should read this as the shape to build
//! for, not as a fence someone else is holding.

// Nothing calls `reach` yet, and that is the state this module documents
// rather than a loose end. Its value today is entirely in the compile error:
// adding a variant to `TunnelMessage` without saying what a device does about
// it stops the build. `#116` is where the server starts calling it, to gate
// what it sends to a client that declared the profile. Deleting it until then
// would mean the next message type is added with nobody asked the question.
#![allow(dead_code)]

use crate::protocol::TunnelMessage;

/// How far a message reaches into a device-sized client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
  /// The device has to handle it. There are seven, and that is the profile.
  MustHandle,
  /// The device may parse it and do nothing, or not parse it at all. It is
  /// informational, or it concerns a capability the device did not ask for.
  MayIgnore,
  /// Only reaches a client that declared the feature behind it. A device that
  /// serves one HTTP target never sees these, and one that receives one has a
  /// server or a configuration problem rather than a protocol obligation.
  NeverSent,
}

/// Where a message travels. A device implementer's first question is which
/// half of the wire they are writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
  ToServer,
  ToClient,
  BothWays,
}

/// The classification, and the only copy of it.
///
/// Exhaustive on purpose: this is a `match` without a wildcard arm, so adding
/// a variant to `TunnelMessage` stops the build here until someone says what
/// a device is supposed to do about it. That is the whole mechanism keeping
/// the written minimum from becoming a document about an older protocol.
pub(crate) fn reach(message: &TunnelMessage) -> (Reach, Direction, &'static str) {
  use Direction::*;
  use Reach::*;
  match message {
    // --- The profile: seven message types serve HTTP. ---
    TunnelMessage::Ping { .. } => (
      MustHandle,
      ToServer,
      "the client's whole declaration, sent on connect and as a heartbeat",
    ),
    TunnelMessage::Pong { .. } => (MustHandle, ToClient, "the answer to a Ping"),
    TunnelMessage::Request { .. } => (
      MustHandle,
      ToClient,
      "a buffered request: the common case, and the only one a device with a \
       small ceiling needs to accept",
    ),
    TunnelMessage::Response { .. } => (MustHandle, ToServer, "the buffered answer to it"),
    TunnelMessage::RequestStart { .. } => (
      MustHandle,
      ToClient,
      "the head of a streamed request; a device may answer with an error \
       status rather than assembling one, but it must not be confused by it",
    ),
    TunnelMessage::RequestChunk { .. } => (MustHandle, ToClient, "a body piece of that request"),
    TunnelMessage::RequestEnd { .. } => (MustHandle, ToClient, "the end of it"),

    // --- Streaming out, and the back-pressure that comes with it. ---
    TunnelMessage::ResponseStart { .. } => (
      MayIgnore,
      ToServer,
      "only produced by a client that chooses to stream a response; a device \
       that always buffers never sends one",
    ),
    TunnelMessage::ResponseChunk { .. } => (MayIgnore, ToServer, "a piece of such a response"),
    TunnelMessage::ResponseEnd { .. } => (MayIgnore, ToServer, "the end of one"),
    TunnelMessage::ResponseAbort { .. } => (
      MayIgnore,
      BothWays,
      "a streamed response given up on; a device that does not stream neither \
       sends nor receives it",
    ),
    TunnelMessage::StreamPause { .. } => (
      MustHandle,
      BothWays,
      "flow control, and the one thing a device cannot ignore if it streams: \
       ignoring a pause is how a 300 KB device meets a backlog it cannot hold",
    ),
    TunnelMessage::StreamResume { .. } => (MustHandle, BothWays, "the other half of it"),

    // --- Informational. ---
    TunnelMessage::HostnameAssigned { .. } => (
      MayIgnore,
      ToClient,
      "the random subdomain the server picked, for logging",
    ),
    TunnelMessage::Draining { .. } => (
      MayIgnore,
      ToClient,
      "the server is going away; reconnecting on close covers it",
    ),
    TunnelMessage::ServerShutdown { .. } => (
      MayIgnore,
      ToClient,
      "the same, at the end; the socket closing says it too",
    ),

    // --- Negotiated, and declinable by never accepting. ---
    TunnelMessage::CompressionStart { .. } => (
      MayIgnore,
      ToClient,
      "an offer, and a device that never answers it is never compressed to",
    ),
    TunnelMessage::CompressionAck { .. } => (
      MayIgnore,
      ToServer,
      "the acceptance a device simply never sends",
    ),

    // --- Only for a client that asked for the feature. ---
    TunnelMessage::UpgradeRequest { .. } => (NeverSent, ToClient, "WebSocket relay"),
    TunnelMessage::UpgradeResponse { .. } => (NeverSent, ToServer, "WebSocket relay"),
    TunnelMessage::WsData { .. } => (NeverSent, BothWays, "WebSocket relay"),
    TunnelMessage::WsClose { .. } => (NeverSent, BothWays, "WebSocket relay"),
    TunnelMessage::TcpOpen { .. } => (NeverSent, ToClient, "a declared TCP tunnel"),
    TunnelMessage::TcpData { .. } => (NeverSent, BothWays, "a declared TCP tunnel"),
    TunnelMessage::TcpClose { .. } => (NeverSent, BothWays, "a declared TCP tunnel"),
    TunnelMessage::UdpOpen { .. } => (NeverSent, ToClient, "a declared UDP tunnel"),
    TunnelMessage::UdpDatagram { .. } => (NeverSent, BothWays, "a declared UDP tunnel"),
    TunnelMessage::UdpClose { .. } => (NeverSent, BothWays, "a declared UDP tunnel"),
    TunnelMessage::OtlpExport { .. } => (NeverSent, ToServer, "the OTel bridge"),
    TunnelMessage::Subscribe { .. } => (NeverSent, ToServer, "messaging"),
    TunnelMessage::Unsubscribe { .. } => (NeverSent, ToServer, "messaging"),
    TunnelMessage::SubscribeRefused { .. } => (NeverSent, ToClient, "messaging"),
    TunnelMessage::Publish { .. } => (NeverSent, BothWays, "messaging"),
    TunnelMessage::PublishAck { .. } => (NeverSent, ToClient, "messaging"),
    TunnelMessage::PublishRefused { .. } => (NeverSent, ToClient, "messaging"),
  }
}

/// The variant names, read from the protocol's source at test time.
///
/// Two mechanisms guard this profile and they guard different things.
/// `reach`'s exhaustive match means a new variant cannot go *unclassified*;
/// this means it cannot go *undocumented*. Reading the enum from source
/// avoids the one alternative, constructing an instance of every variant,
/// which would be a second list of fields to keep in step and would fail for
/// the wrong reason the moment a field was added.
#[cfg(test)]
pub(crate) fn variant_names() -> Vec<String> {
  let source = include_str!("protocol.rs");
  let start = source
    .find("pub enum TunnelMessage {")
    .expect("the protocol still declares TunnelMessage");
  let body = &source[start..];
  let mut names = Vec::new();
  for line in body.lines().skip(1) {
    if line == "}" {
      break;
    }
    // Top-level variants are indented exactly two spaces; fields inside them
    // are deeper, and attributes and comments start with other characters.
    let Some(rest) = line.strip_prefix("  ") else {
      continue;
    };
    if rest.starts_with(' ') || rest.starts_with('/') || rest.starts_with('#') {
      continue;
    }
    let name: String = rest
      .chars()
      .take_while(|c| c.is_ascii_alphanumeric())
      .collect();
    if !name.is_empty() && name.starts_with(|c: char| c.is_ascii_uppercase()) {
      names.push(name);
    }
  }
  names
}

#[cfg(test)]
#[path = "protocol_profile_tests.rs"]
mod tests;
