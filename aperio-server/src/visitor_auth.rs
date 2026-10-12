//! The compiled visitor gate: what `auth:` becomes once it is in force.
//!
//! One grammar covers both sides of the tunnel (`planned_features.md` #105).
//! An operator writes a scalar `user:password`, one `{method: ...}` block, or
//! a list of them; this module is what the request path actually consults, so
//! the gate never re-parses configuration per request and never has to know
//! which of the three spellings produced it.
//!
//! Five methods exist: `none`, `basic`, `bearer` (#107), `jwt` (#110) and
//! `forward` (#104). The set is closed on purpose, the open version was
//! considered and withdrawn (#103), and `forward` is what lets it stay closed:
//! anything deliberately left out is an endpoint away, in a process that is
//! not ours. `oidc` on this plane is still #106.
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
  /// An opaque secret presented as `Authorization: Bearer <secret>`, and,
  /// when `query` says so, as `?aperio_token=<secret>`.
  ///
  /// The secret is held and compared verbatim rather than hashed, which is
  /// deliberate and worth the sentence: it is a high-entropy value the
  /// operator generated, so there is no dictionary to defend against, and it
  /// has to be comparable against whatever they wrote in the file. That is a
  /// different shape from `basic`, whose password is chosen by a person.
  Bearer { secrets: Vec<String>, query: bool },
  /// A token the visitor already holds, signed by an issuer whose keys the
  /// server can check. Verified in [`crate::jwt`], which is where the fetch
  /// and the cache live, because this is the one method whose answer needs
  /// state and the network rather than only the request.
  Jwt(Box<crate::jwt::JwtConfig>),
  /// Delegated: an endpoint the operator runs is asked about each request.
  /// The escape hatch that lets this set stay closed.
  Forward(Box<crate::forward_auth::ForwardConfig>),
  /// This server's own sign-in (`planned_features.md` #155): a dashboard
  /// session, on the admin plane, reaching the hostname's organization. The
  /// admission the closed posture already makes for a signed-in user, with a
  /// way to say it, so a route can be shown the door rather than the stealth
  /// answer without inventing a password nobody will type.
  Aperio,
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
    Policy::compile_from(setting, false)
  }

  /// The same, marking every `jwt` method with where the declaration came
  /// from. A client's `jwks_url` is fetched by this server from this server's
  /// network, so it is fenced as a client-declared destination; the
  /// operator's own is not.
  pub(crate) fn compile_from(setting: &AuthSetting, declared_by_client: bool) -> Policy {
    let methods = setting
      .methods()
      .iter()
      .filter_map(
        |spec| match spec.method.trim().to_ascii_lowercase().as_str() {
          "none" => Some(Method::Open),
          "aperio" => Some(Method::Aperio),
          "basic" => Some(Method::Basic {
            users: spec
              .users
              .as_ref()
              .map(|u| u.as_slice().to_vec())
              .unwrap_or_default(),
          }),
          "forward" => {
            // A client's `forward` is always asked of that client: the
            // server never dials a URL a client chose, whatever the entry
            // says, and the client refuses to start without `via: client`
            // anyway. Over the tunnel the cold path is two round trips, so a
            // verdict is remembered for thirty seconds unless told otherwise.
            let via_client = declared_by_client || aperio_config::forward_via_client(spec);
            Some(Method::Forward(Box::new(
              crate::forward_auth::ForwardConfig {
                url: spec.url.clone().unwrap_or_default(),
                request_headers: spec.request_headers.clone().unwrap_or_default(),
                response_headers: spec.response_headers.clone().unwrap_or_default(),
                timeout: std::time::Duration::from_secs(spec.timeout.unwrap_or(5).max(1)),
                cache: std::time::Duration::from_secs(spec.cache.unwrap_or(if via_client {
                  30
                } else {
                  0
                })),
                via_client,
              },
            )))
          }
          "jwt" => Some(Method::Jwt(Box::new(crate::jwt::JwtConfig {
            jwks_url: spec.jwks_url.clone(),
            hmac_secret: spec.hmac_secret.clone(),
            issuer: spec.issuer.clone(),
            audience: spec
              .audience
              .as_ref()
              .map(|a| a.as_slice().to_vec())
              .unwrap_or_default(),
            claims: spec.claims.clone().unwrap_or_default(),
            cookie: spec.cookie.clone(),
            declared_by_client,
          }))),
          "bearer" => Some(Method::Bearer {
            secrets: spec
              .secret
              .as_ref()
              .map(|s| s.as_slice().to_vec())
              .unwrap_or_default(),
            query: spec.query.unwrap_or(false),
          }),
          _ => None,
        },
      )
      .collect();
    Policy { methods }
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

  /// The methods in force, for the startup summary and the tests.
  pub(crate) fn method_names(&self) -> Vec<&'static str> {
    self
      .methods
      .iter()
      .map(|m| match m {
        Method::Open => "none",
        Method::Basic { .. } => "basic",
        Method::Bearer { .. } => "bearer",
        Method::Jwt(_) => "jwt",
        Method::Forward(_) => "forward",
        Method::Aperio => "aperio",
      })
      .collect()
  }

  /// True when the `aperio` method is here: an Aperio sign-in opens this
  /// gate, and a caller without a browser is told `401` rather than sent to
  /// a login form.
  pub(crate) fn has_aperio(&self) -> bool {
    self.methods.iter().any(|m| matches!(m, Method::Aperio))
  }

  /// True when the `aperio` method is the only one: then the site's own
  /// visitor password, which mints a session too, does not count. The plane
  /// distinction of #106 is exactly this, and a mixed policy keeps the
  /// wider reading its other methods always had.
  pub(crate) fn aperio_only(&self) -> bool {
    !self.methods.is_empty() && self.methods.iter().all(|m| matches!(m, Method::Aperio))
  }

  /// Does a presented bearer secret open this gate?
  ///
  /// `from_query` is true when the secret arrived in the URL rather than in a
  /// header, and a method that did not opt into that is not consulted: the
  /// query form is a per-method decision because it is the form that ends up
  /// in logs, so a gate that never asked for it must not be openable by it.
  ///
  /// Every candidate is compared without an early exit, so the answer takes
  /// the same time whichever secret matched.
  pub(crate) fn admits_bearer(&self, presented: &str, from_query: bool) -> bool {
    let mut ok = false;
    for method in &self.methods {
      if let Method::Bearer { secrets, query } = method {
        if from_query && !query {
          continue;
        }
        for candidate in secrets {
          ok |= constant_time_eq_str(presented, candidate);
        }
      }
    }
    ok
  }

  /// True when some method here accepts a secret in the URL, which is what
  /// decides whether the query parameter is looked at (and then stripped)
  /// for this route at all.
  pub(crate) fn accepts_query_token(&self) -> bool {
    self
      .methods
      .iter()
      .any(|m| matches!(m, Method::Bearer { query: true, .. }))
  }

  /// True when some method here can be satisfied by a credential presented on
  /// the request itself, rather than by a session cookie. Such a request is
  /// answered `401` with a challenge instead of being redirected to a login
  /// page, because a caller that speaks in headers has no browser to send.
  pub(crate) fn has_direct_method(&self) -> bool {
    self
      .methods
      .iter()
      .any(|m| matches!(m, Method::Bearer { .. } | Method::Jwt(_)))
  }

  /// The `forward` methods in force, in the order they were written.
  pub(crate) fn forward_methods(
    &self,
  ) -> impl Iterator<Item = &crate::forward_auth::ForwardConfig> {
    self.methods.iter().filter_map(|m| match m {
      Method::Forward(cfg) => Some(cfg.as_ref()),
      _ => None,
    })
  }

  /// The `jwt` methods in force, in the order they were written.
  pub(crate) fn jwt_methods(&self) -> impl Iterator<Item = &crate::jwt::JwtConfig> {
    self.methods.iter().filter_map(|m| match m {
      Method::Jwt(cfg) => Some(cfg.as_ref()),
      _ => None,
    })
  }

  /// The `WWW-Authenticate` value for a refusal, when this policy has
  /// something for a caller to answer with.
  pub(crate) fn challenge(&self) -> Option<&'static str> {
    self.methods.iter().find_map(|m| match m {
      Method::Bearer { .. } | Method::Jwt(_) => Some("Bearer"),
      _ => None,
    })
  }
}

/// Validates the server's default gate (`access.gate` in the settings).
pub(crate) fn validate_gate(setting: &AuthSetting) -> Result<(), String> {
  aperio_config::validate_auth_setting(setting).map_err(|e| format!("access.gate: {e}"))
}

#[cfg(test)]
#[path = "visitor_auth_tests.rs"]
mod tests;
