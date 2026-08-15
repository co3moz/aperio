//! The `jwt` visitor-auth method: a token the visitor already holds, verified
//! against keys the issuer publishes (`planned_features.md` #110).
//!
//! The operational difference from `forward` (#104) is the whole reason both
//! belong in the set: this costs a signature check and no round trip, so it
//! scales with CPU rather than with someone else's availability. It also
//! subsumes a shelf of provider integrations that would otherwise each arrive
//! as their own method, since Cloudflare Access, an ALB doing OIDC and
//! anybody's own auth service all hand out the same thing.
//!
//! **The dependency was priced before this was written.** `jsonwebtoken` 9
//! builds on `ring`, which rustls already puts in every Aperio binary, so
//! this adds no crypto implementation and no cross-compilation surface. The
//! pin is deliberate and `aperio-server/Cargo.toml` says why.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::{Algorithm, DecodingKey, Validation, jwk::JwkSet};

use crate::state::AppState;

/// How long a fetched key set is trusted before it is fetched again.
///
/// A key set changes when an issuer rotates, which is rare and announced by a
/// new `kid` rather than by a clock, so this is a backstop rather than the
/// mechanism: an unknown `kid` refetches immediately (see [`verify`]).
const JWKS_TTL: Duration = Duration::from_secs(3600);

/// Shortest gap between two fetches of the same key set.
///
/// The refetch-on-unknown-kid rule is what makes rotation seamless, and it is
/// also a way to make the server fetch on demand: a stream of tokens naming
/// random key ids would otherwise be a request amplifier pointed at the
/// issuer. Past this floor an unknown `kid` is simply refused.
const JWKS_MIN_REFETCH: Duration = Duration::from_secs(30);

/// Largest key-set document accepted.
///
/// Enforced while reading rather than after it: a limit checked on a document
/// already in memory discards what it has just finished allocating, which
/// leaves how much that is up to whatever answered.
const JWKS_MAX_BYTES: usize = 256 * 1024;

/// One issuer's keys, and when they were last fetched.
pub(crate) struct CachedJwks {
  keys: Arc<JwkSet>,
  fetched: Instant,
}

/// What a `jwt` method needs in order to accept or refuse a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JwtConfig {
  /// Where the issuer's public keys live, for asymmetric algorithms.
  pub(crate) jwks_url: Option<String>,
  /// A shared secret, for `HS256`. Exactly one of the two is set.
  pub(crate) hmac_secret: Option<String>,
  pub(crate) issuer: Option<String>,
  pub(crate) audience: Vec<String>,
  /// Claims that must be present and equal to these values.
  pub(crate) claims: BTreeMap<String, String>,
  /// Cookie the token is read from, when it does not arrive as a bearer.
  pub(crate) cookie: Option<String>,
  /// True when this method came from a client over the tunnel rather than
  /// from the server's own configuration.
  ///
  /// It decides how `jwks_url` is fenced, and it has to, because the two
  /// sources are not the same trust. An operator naming an internal issuer is
  /// describing their own network; a tunnel-token holder naming one is aiming
  /// this server at it. `forward` was kept out of the client-declarable set
  /// for exactly this reason and `jwt` was not, which is the hole this closes.
  pub(crate) declared_by_client: bool,
}

/// The verified claims, reduced to what the gate does anything with.
pub(crate) struct VerifiedToken {
  /// `email` where the issuer sends one, else `sub`. What a backend is told
  /// (#109), and the reason to prefer the email: `sub` is an opaque provider
  /// id that means nothing to the application behind the tunnel.
  pub(crate) who: Option<String>,
}

/// Verifies `token` against `cfg`, returning its identity when it is good.
///
/// Every refusal is the same `None`, on purpose. The reasons a token fails,
/// wrong signature, wrong issuer, expired, missing a claim, are useful to an
/// operator and are logged at debug; handing them back to whoever presented
/// the token turns the gate into an oracle for shaping the next attempt.
pub(crate) async fn verify(
  state: &AppState,
  cfg: &JwtConfig,
  token: &str,
) -> Option<VerifiedToken> {
  let header = jsonwebtoken::decode_header(token).ok()?;
  let (key, algorithm) = match (&cfg.hmac_secret, &cfg.jwks_url) {
    (Some(secret), _) => (
      DecodingKey::from_secret(secret.as_bytes()),
      Algorithm::HS256,
    ),
    (None, Some(url)) => {
      let kid = header.kid.as_deref();
      let jwk = match find_key(state, url, kid, false, cfg.declared_by_client).await {
        Some(jwk) => Some(jwk),
        // A `kid` the cache does not know is what a rotation looks like from
        // here, so it is worth one fetch rather than a refusal that clears up
        // on its own an hour later.
        None => find_key(state, url, kid, true, cfg.declared_by_client).await,
      }?;
      let algorithm = jwk
        .common
        .key_algorithm
        .and_then(|a| a.to_string().parse::<Algorithm>().ok())
        .unwrap_or(Algorithm::RS256);
      (DecodingKey::from_jwk(&jwk).ok()?, algorithm)
    }
    (None, None) => return None,
  };

  let mut validation = Validation::new(algorithm);
  // Both are opt-in in the file, and the library's defaults do not line up
  // with what "the operator asked for this" has to mean here. Two rules:
  //
  // Unset is unchecked. An empty `aud` list must not read as "accept any
  // audience" by accident, and an unset `iss` must not be required.
  //
  // Set means *present* and equal. Configuring an audience while the library
  // only requires it to match when it happens to be there would admit a token
  // carrying no `aud` at all, which is precisely the token the requirement
  // was written to keep out. Naming a claim as required is what closes that,
  // and `exp` is required whatever the file says: a token with no expiry is
  // one that never stops working.
  let mut required: Vec<&str> = vec!["exp"];
  match cfg.issuer.as_deref() {
    Some(issuer) => {
      validation.set_issuer(&[issuer]);
      required.push("iss");
    }
    None => validation.iss = None,
  }
  if cfg.audience.is_empty() {
    validation.validate_aud = false;
  } else {
    validation.set_audience(&cfg.audience);
    required.push("aud");
  }
  validation.set_required_spec_claims(&required);

  let data = jsonwebtoken::decode::<serde_json::Value>(token, &key, &validation)
    .inspect_err(|e| tracing::debug!("jwt refused: {e}"))
    .ok()?;

  for (name, want) in &cfg.claims {
    let got = data.claims.get(name).and_then(claim_as_str);
    if got.as_deref() != Some(want.as_str()) {
      tracing::debug!("jwt refused: claim `{name}` is not `{want}`");
      return None;
    }
  }

  Some(VerifiedToken {
    who: data
      .claims
      .get("email")
      .and_then(claim_as_str)
      .or_else(|| data.claims.get("sub").and_then(claim_as_str)),
  })
}

/// A claim's value as a string, for the exact-match requirements.
///
/// Numbers and booleans are rendered rather than refused, because a claim
/// written `{"tier": 2}` by the issuer and `tier: "2"` in the file is the
/// same intention twice, and refusing it would read as the claim missing.
fn claim_as_str(value: &serde_json::Value) -> Option<String> {
  match value {
    serde_json::Value::String(s) => Some(s.clone()),
    serde_json::Value::Number(n) => Some(n.to_string()),
    serde_json::Value::Bool(b) => Some(b.to_string()),
    _ => None,
  }
}

/// The key with this id from `url`'s key set, fetching when asked to.
async fn find_key(
  state: &AppState,
  url: &str,
  kid: Option<&str>,
  refetch: bool,
  declared_by_client: bool,
) -> Option<jsonwebtoken::jwk::Jwk> {
  let cached = {
    let cache = state.jwks_cache.lock().await;
    cache.get(url).map(|c| (c.keys.clone(), c.fetched))
  };
  let keys = match cached {
    Some((keys, fetched)) if !refetch && fetched.elapsed() < JWKS_TTL => keys,
    Some((keys, fetched)) if refetch && fetched.elapsed() < JWKS_MIN_REFETCH => {
      // Too soon. A token naming an unknown key id is refused rather than
      // turned into a fetch, so a stream of them cannot be aimed at the
      // issuer through us.
      return pick(&keys, kid);
    }
    _ => fetch(state, url, declared_by_client).await?,
  };
  pick(&keys, kid)
}

/// Picks the key a token names, or the only one there is.
///
/// A key set with exactly one key and a token with no `kid` is the ordinary
/// small deployment. Beyond that a token must name its key: guessing which
/// of several keys signed something is how a verifier accepts a signature the
/// issuer did not mean to make.
fn pick(keys: &JwkSet, kid: Option<&str>) -> Option<jsonwebtoken::jwk::Jwk> {
  match kid {
    Some(kid) => keys.find(kid).cloned(),
    None => match keys.keys.as_slice() {
      [only] => Some(only.clone()),
      _ => None,
    },
  }
}

/// Fetches and caches a key set.
///
/// The destination goes through the same outbound policy as webhooks and
/// scaling hooks: this is a URL from a configuration file that makes the
/// server issue a request, which is the shape the policy exists to fence.
async fn fetch(state: &AppState, url: &str, declared_by_client: bool) -> Option<Arc<JwkSet>> {
  if let Err(why) = state.config().outbound_policy.check(url).await {
    tracing::warn!("Refusing to fetch the key set at {url}: {why}");
    return None;
  }
  // A URL a *client* named is fenced whatever the operator's own outbound
  // policy says, because that policy is the operator's preference about their
  // own callbacks and is permissive by default. This one is not a preference:
  // the fetch happens before any signature is checked, so anyone holding a
  // tunnel token could otherwise aim the server at a metadata service or an
  // internal admin port and read the answer off the timing. The same fence
  // `scaling.url` has always had, for the same reason.
  if declared_by_client {
    match url::Url::parse(url) {
      Ok(parsed) => {
        let allow_insecure = std::env::var("APERIO_JWKS_ALLOW_HTTP")
          .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
          .unwrap_or(false);
        let allow_private = std::env::var("APERIO_JWKS_ALLOW_PRIVATE")
          .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
          .unwrap_or(false);
        if let Err(why) = crate::outbound::client_declared_destination_allowed(
          &parsed,
          allow_insecure,
          allow_private,
          "APERIO_JWKS",
        )
        .await
        {
          tracing::warn!("Refusing to fetch the client-declared key set at {url}: {why}");
          return None;
        }
      }
      Err(e) => {
        tracing::warn!("Refusing to fetch the client-declared key set at {url}: {e}");
        return None;
      }
    }
  }
  // Built here rather than shared, matching the OIDC discovery fetch: a
  // default client would drop the timeout, and an unbounded fetch on the
  // request path is the one thing this must not be.
  let http = crate::outbound::client_builder()
    .timeout(Duration::from_secs(10))
    // Not followed, for the reason the policy check above exists: a key-set
    // URL that passes the fence must not be able to hand the server a
    // `Location` pointing behind it. An issuer that genuinely moved its keys
    // should say so in its configuration.
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .inspect_err(|e| tracing::error!("Could not build the key-set HTTP client: {e}"))
    .ok()?;
  let res = http
    .get(url)
    .send()
    .await
    .inspect_err(|e| tracing::warn!("Could not fetch the key set at {url}: {e}"))
    .ok()?;
  if !res.status().is_success() {
    tracing::warn!("The key set at {url} answered {}", res.status());
    return None;
  }
  let Some(body) = crate::outbound::read_bounded(res, JWKS_MAX_BYTES).await else {
    tracing::warn!(
      "The key set at {url} is unreadable or larger than {JWKS_MAX_BYTES} bytes; ignoring it"
    );
    return None;
  };
  let keys: JwkSet = serde_json::from_str(&body)
    .inspect_err(|e| tracing::warn!("The key set at {url} is not a JWKS: {e}"))
    .ok()?;
  let keys = Arc::new(keys);
  let mut cache = state.jwks_cache.lock().await;
  // Bounded: the key of this map is a URL from the configuration, so it is
  // already small, but a hot reload that rewrites the URL repeatedly should
  // not grow it forever.
  if cache.len() > 32 {
    cache.clear();
  }
  cache.insert(
    url.to_string(),
    CachedJwks {
      keys: keys.clone(),
      fetched: Instant::now(),
    },
  );
  tracing::info!("Fetched {} key(s) from {url}", keys.keys.len());
  Some(keys)
}

#[cfg(test)]
#[path = "jwt_tests.rs"]
mod tests;
