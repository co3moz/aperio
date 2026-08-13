//! The compiled visitor gate: what `auth:` becomes once it is in force.
//!
//! One grammar covers both sides of the tunnel (`planned_features.md` #105).
//! An operator writes a scalar `user:password`, one `{method: ...}` block, or
//! a list of them; this module is what the request path actually consults, so
//! the gate never re-parses configuration per request and never has to know
//! which of the three spellings produced it.
//!
//! Two methods exist today, `none` and `basic`. The set is closed on purpose:
//! the open version was considered and withdrawn (#103). Each further method
//! is its own entry (`bearer` #107, `oidc` #106, `jwt` #110, `forward` #104),
//! and each lands here as another variant rather than as another top-level
//! setting somewhere else in the file.
//!
//! **A policy of several methods admits on the first that admits.** That is
//! what lets one route say "a browser signs in, a script presents a key", and
//! it is why the type is a list rather than a choice.

use aperio_config::AuthSetting;

use crate::auth::constant_time_eq_str;

/// One gate, compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Method {
  /// Deliberately open. Admits everyone, and is the honest spelling of what
  /// a client declares as `public: true`.
  Open,
  /// A `user:password` login. Several credentials may open one gate; they are
  /// alternatives, not a sequence.
  Basic { users: Vec<String> },
}

/// A route's visitor gate: the methods that may admit a visitor, in the order
/// they were written.
///
/// The empty policy is "no gate configured", which is not the same as
/// [`Method::Open`]: one says nothing, the other says something. Today both
/// end in the visitor being admitted, and #108 is where the difference starts
/// to matter, so they are distinguishable from the start rather than being
/// collapsed now and separated later.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Policy {
  methods: Vec<Method>,
}

impl Policy {
  /// Compiles what the configuration said into what the request path uses.
  ///
  /// **Every ambiguity here resolves closed.** This runs on every hot reload
  /// and cannot refuse (the file was already validated where it was read, by
  /// `aperio_config::validate_auth_setting`, which is where an operator is
  /// told), so the only safety it has is the direction it fails in. A method
  /// it cannot read is dropped and the ones beside it stay in force; a
  /// credential that no login could ever match still gates, admitting nobody,
  /// because a door nobody can open is a misconfiguration an operator will
  /// find, and a door that opened itself is one they will not.
  pub(crate) fn compile(setting: &AuthSetting) -> Policy {
    let methods = setting
      .methods()
      .iter()
      .filter_map(
        |spec| match spec.method.trim().to_ascii_lowercase().as_str() {
          "none" => Some(Method::Open),
          "basic" => Some(Method::Basic {
            users: spec
              .users
              .as_ref()
              .map(|u| u.as_slice().to_vec())
              .unwrap_or_default(),
          }),
          _ => None,
        },
      )
      .collect();
    Policy { methods }
  }

  /// The policy a bare `user:password` describes, which is what the
  /// environment variable, the CLI flag and the dashboard's editable field
  /// carry.
  ///
  /// Only an empty value means "no gate". A value that is present but could
  /// never match a login, `APERIO_SERVER_AUTH=secret` with no separator,
  /// still gates and admits nobody: that is what it has always done, and the
  /// alternative would be a typo in one environment variable quietly opening
  /// every route on the server.
  pub(crate) fn from_credentials(raw: &str) -> Policy {
    if raw.trim().is_empty() {
      return Policy::default();
    }
    Policy {
      methods: vec![Method::Basic {
        users: vec![raw.to_string()],
      }],
    }
  }

  /// True when nothing here stands between a visitor and the route: either no
  /// method was configured at all, or one of them is the open gate.
  pub(crate) fn admits_everyone(&self) -> bool {
    self.methods.is_empty() || self.methods.iter().any(|m| matches!(m, Method::Open))
  }

  /// True when this policy actually gates something, which is the question the
  /// request path asks.
  pub(crate) fn gates(&self) -> bool {
    !self.admits_everyone()
  }

  /// Does a presented `user:password` open this gate?
  ///
  /// Every candidate is compared, without an early exit, so the answer takes
  /// the same time whether the first credential matched or the last one did.
  pub(crate) fn admits_credential(&self, presented: &str) -> bool {
    let mut ok = false;
    for method in &self.methods {
      if let Method::Basic { users } = method {
        for candidate in users {
          ok |= constant_time_eq_str(presented, candidate);
        }
      }
    }
    ok
  }

  /// The single `user:password` this policy is equivalent to, when it is
  /// equivalent to one. What the scalar surfaces still read and show.
  pub(crate) fn as_single_credential(&self) -> Option<&str> {
    match self.methods.as_slice() {
      [Method::Basic { users }] => match users.as_slice() {
        [only] => Some(only.as_str()),
        _ => None,
      },
      _ => None,
    }
  }

  /// The methods in force, for the startup summary and the tests.
  pub(crate) fn method_names(&self) -> Vec<&'static str> {
    self
      .methods
      .iter()
      .map(|m| match m {
        Method::Open => "none",
        Method::Basic { .. } => "basic",
      })
      .collect()
  }
}

/// Reads the server's default gate from `aperio-server.yaml`, as either
/// `server: { auth: ... }` or the flat `server_auth:`.
///
/// Read as a structured section rather than through the generic scalar
/// materialization, because a block cannot become an environment variable and
/// the generic path deliberately skips mappings. A section that does not
/// parse refuses the start: an operator who wrote a gate and got no gate is
/// the failure this is here to prevent, and it is exactly what a silent
/// fallback to `APERIO_SERVER_AUTH` would produce.
pub(crate) fn block_from_config_file() -> Option<AuthSetting> {
  let value = crate::config_file::structured("server")
    .and_then(|server| server.get("auth").cloned())
    .or_else(|| crate::config_file::structured("server_auth"))?;
  let parsed: Result<AuthSetting, String> =
    serde_yaml::from_value(value).map_err(|e| e.to_string());
  match parsed.and_then(|setting| aperio_config::validate_auth_setting(&setting).map(|()| setting))
  {
    Ok(setting) => Some(setting),
    Err(err) => {
      tracing::error!("invalid `auth:` in aperio-server.yaml: {err}");
      std::process::exit(1);
    }
  }
}

#[cfg(test)]
#[path = "visitor_auth_tests.rs"]
mod tests;
