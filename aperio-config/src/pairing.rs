//! Which client and server releases may talk to each other.
//!
//! **One rule: a client and a server must share a minor release.** In a 0.x
//! version a minor bump is where the tunnel protocol, the handshake and the
//! configuration are allowed to change, so `0.13.0` and `0.13.4` talk to each
//! other and `0.13.x` and `0.12.x` do not, in either direction. There is no
//! compatibility window and no negotiation of what an older peer can carry:
//! the code on both sides assumes the peer is its own minor, and the gate is
//! what makes that assumption true.
//!
//! The check runs on the WebSocket upgrade, before a socket exists, on both
//! ends. The server refuses a client it does not pair with by answering `426`,
//! and a client refuses a server that announced a different minor, so the
//! operator always reads a sentence naming both versions and the side to
//! upgrade rather than a connection that comes up and misbehaves later.
//!
//! **Silence is refused.** A peer that announces no release predates this
//! rule, which makes it an older minor by construction.

use std::cmp::Ordering;
use std::fmt;

/// Header both sides announce their release on: the client with the upgrade
/// request, the server with the upgrade response.
pub const RELEASE_HEADER: &str = "x-aperio-release";

/// A `MAJOR.MINOR.PATCH` version, tolerant of a missing patch (`0.5`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
  pub major: u64,
  pub minor: u64,
  pub patch: u64,
}

impl Version {
  /// Parses `1.2.3`, `1.2` or `1`, ignoring a leading `v` and any
  /// pre-release/build suffix. Anything else is an error.
  pub fn parse(raw: &str) -> Result<Version, String> {
    let cleaned = raw.trim().trim_start_matches(['v', 'V']);
    let core = cleaned
      .split(['-', '+'])
      .next()
      .unwrap_or_default()
      .trim_end_matches('.');
    if core.is_empty() {
      return Err(format!("'{raw}' is not a version"));
    }
    let mut parts = [0u64; 3];
    for (i, part) in core.split('.').enumerate() {
      if i >= 3 {
        return Err(format!("'{raw}' has more than three components"));
      }
      parts[i] = part
        .parse::<u64>()
        .map_err(|_| format!("'{raw}' is not a version: '{part}' is not a number"))?;
    }
    Ok(Version {
      major: parts[0],
      minor: parts[1],
      patch: parts[2],
    })
  }

  /// The `MAJOR.MINOR` line this version belongs to, as written in messages.
  pub fn line(&self) -> String {
    format!("{}.{}.x", self.major, self.minor)
  }

  /// Whether two versions are on the same minor line.
  pub fn pairs_with(&self, other: &Version) -> bool {
    self.major == other.major && self.minor == other.minor
  }
}

impl fmt::Display for Version {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
  }
}

impl Ord for Version {
  fn cmp(&self, other: &Self) -> Ordering {
    (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
  }
}

impl PartialOrd for Version {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

/// One side of a pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
  Client,
  Server,
}

impl Side {
  fn other(self) -> Side {
    match self {
      Side::Client => Side::Server,
      Side::Server => Side::Client,
    }
  }

  fn word(self) -> &'static str {
    match self {
      Side::Client => "client",
      Side::Server => "server",
    }
  }
}

/// Why a pairing was refused, with both releases and the side to upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
  /// The release of the side doing the check.
  pub local: String,
  /// Which side did the check.
  pub local_side: Side,
  /// What the peer announced, `None` when it announced nothing usable.
  pub peer: Option<String>,
  /// The side that has to be upgraded.
  pub upgrade: Side,
}

impl Unsupported {
  /// The sentence an operator reads, naming both releases and the fix.
  pub fn message(&self) -> String {
    let peer_side = self.local_side.other();
    let line = Version::parse(&self.local)
      .map(|v| v.line())
      .unwrap_or_else(|_| self.local.clone());
    let peer = match &self.peer {
      Some(v) => format!("this {} is {v}", peer_side.word()),
      None => format!(
        "this {} announced no release, so it predates 0.13.0",
        peer_side.word()
      ),
    };
    format!(
      "{peer}, and this {} is {}: a client and a server must share a minor release ({line}). Upgrade the {}.",
      self.local_side.word(),
      self.local,
      self.upgrade.word()
    )
  }
}

/// Judges the peer's announced release against this side's own.
///
/// `local` is this binary's release and `local_side` says which end this is.
/// `None` means the two pair. A peer that announced nothing, or something that
/// does not parse, is refused and asked to upgrade: silence predates the rule.
pub fn check(local: &str, local_side: Side, announced: Option<&str>) -> Option<Unsupported> {
  let own = Version::parse(local).ok()?;
  let announced = announced.map(str::trim).filter(|v| !v.is_empty());
  let Some(peer) = announced.and_then(|v| Version::parse(v).ok()) else {
    return Some(Unsupported {
      local: local.to_string(),
      local_side,
      peer: announced.map(str::to_string),
      upgrade: local_side.other(),
    });
  };
  if own.pairs_with(&peer) {
    return None;
  }
  let upgrade = if peer < own {
    local_side.other()
  } else {
    local_side
  };
  Some(Unsupported {
    local: local.to_string(),
    local_side,
    peer: announced.map(str::to_string),
    upgrade,
  })
}

#[cfg(test)]
#[path = "pairing_tests.rs"]
mod tests;
