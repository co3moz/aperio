//! Which client and server releases may talk to each other
//! (`planned_features.md` #113).
//!
//! `#89` wrote the compatibility window down and proved it in CI. This is the
//! other half: the window enforced at connect time, so a pairing outside it is
//! refused where the cause is visible instead of establishing and then
//! misbehaving three layers deeper.
//!
//! **Today it refuses nothing, and that is deliberate.** Both floors are set
//! where the documented promise already is, every released client from
//! `v0.1.0` onward, so this ships as a mechanism rather than as a new
//! restriction. A version gate with no incompatibility to enforce only invents
//! outages. What it buys is that when a break does land, narrowing the window
//! is a one-line change in the same commit that breaks something, and the
//! operator gets a sentence naming which side is too old rather than a
//! connection that comes up and fails somewhere else.
//!
//! **Both floors, because either side can be the old one.** A server is one
//! box and its clients are a fleet, so the ordinary shape is a new server
//! ahead of old clients; that is what `MIN_SUPPORTED_CLIENT` covers. The
//! reverse happens too, someone upgrades one client first, and only the client
//! can notice it, since a server cannot know it is too old for something a
//! future client wants. Hence `MIN_SUPPORTED_SERVER`, checked on the client
//! against what the server announces.

use crate::compat::Version;

/// The oldest client release this server will accept.
///
/// Equal to the documented promise, so nothing that works today stops
/// working. Raise it in the same commit as the break that requires it, with
/// the `CHANGELOG.md` entry rule 23 asks for.
pub const MIN_SUPPORTED_CLIENT: &str = "0.1.0";

/// The oldest server release this client will serve against.
///
/// The same rule from the other end, and the same value for the same reason.
pub const MIN_SUPPORTED_SERVER: &str = "0.1.0";

/// Header the client announces its release on, sent with the upgrade request.
pub const CLIENT_RELEASE_HEADER: &str = "x-aperio-release";

/// Header the server answers with its own release on.
pub const SERVER_RELEASE_HEADER: &str = "x-aperio-release";

/// Header the server announces its oldest acceptable client on, so a client
/// refused for age can say why without guessing.
pub const MIN_CLIENT_HEADER: &str = "x-aperio-min-client";

/// Why a pairing was refused, with both versions in it.
///
/// A refusal that does not name the versions is a refusal an operator has to
/// reproduce to understand, which is the failure this whole entry exists to
/// avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
  /// The peer's release, as it announced it.
  pub peer: String,
  /// The floor it failed to meet.
  pub floor: String,
  /// Which side has to be upgraded.
  pub too_old: Side,
}

/// Which side of a refused pairing is the old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
  Client,
  Server,
}

impl Unsupported {
  /// The sentence an operator reads, naming both versions and the fix.
  pub fn message(&self) -> String {
    match self.too_old {
      Side::Client => format!(
        "this client is {}, and this server accepts {} or newer. Upgrade the client.",
        self.peer, self.floor
      ),
      Side::Server => format!(
        "this server is {}, and this client requires {} or newer. Upgrade the server.",
        self.peer, self.floor
      ),
    }
  }
}

/// Judges a peer's announced release against a floor.
///
/// `None` for a peer that announced nothing: a release old enough to predate
/// the header is inside the documented window anyway, and refusing on silence
/// would turn "I did not say" into "I am too old", which is the outage this
/// entry warns about. `None` too for a value that does not parse, for the same
/// reason: a garbled header is not evidence of age.
pub fn check(announced: Option<&str>, floor: &str, too_old: Side) -> Option<Unsupported> {
  let announced = announced.map(str::trim).filter(|v| !v.is_empty())?;
  let peer = Version::parse(announced).ok()?;
  let floor_version = Version::parse(floor).ok()?;
  (peer < floor_version).then(|| Unsupported {
    peer: announced.to_string(),
    floor: floor.to_string(),
    too_old,
  })
}

#[cfg(test)]
#[path = "pairing_tests.rs"]
mod tests;
