//! How hard Aperio works to answer when something behind it fails, in one word
//! per side: `off`, `safe` or `aggressive`.
//!
//! Six mechanisms used to answer that question separately: on the server
//! in-flight failover, retrying a 5xx, and passive outlier ejection; on the
//! client retrying a backend request and a circuit breaker. Each had its own
//! knobs and its own default, and the sensible combination was something an
//! operator had to assemble. A profile is that combination, named; each knob
//! stays writable, and one that is written wins over the profile's value.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A resilience profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResilienceProfile {
  /// Nothing is retried or re-dispatched, and nothing is ejected: a failure
  /// is the visitor's answer. For backends where a request must never run
  /// twice, or to see failures exactly as they happen.
  Off,
  /// The default. Only what cannot run twice by accident: an idempotent
  /// request is retried or re-dispatched once elsewhere, and a client that
  /// keeps failing is taken out of routing for a while, never emptying it.
  #[default]
  Safe,
  /// Works harder to answer, and takes longer to give up: more attempts, a
  /// retryable `5xx` re-dispatched too, a waiting failover, a circuit breaker
  /// on the client.
  Aggressive,
}

impl JsonSchema for ResilienceProfile {
  fn schema_name() -> std::borrow::Cow<'static, str> {
    "ResilienceProfile".into()
  }

  fn inline_schema() -> bool {
    true
  }

  fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
      "description": "`off`, `safe` (the default) or `aggressive`.",
      "type": "string",
      "enum": ["off", "safe", "aggressive"]
    })
  }
}

impl<'de> Deserialize<'de> for ResilienceProfile {
  fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    use serde::de::Error;
    match serde_json::Value::deserialize(d)? {
      serde_json::Value::String(word) => match word.trim().to_ascii_lowercase().as_str() {
        "off" => Ok(ResilienceProfile::Off),
        "safe" => Ok(ResilienceProfile::Safe),
        "aggressive" => Ok(ResilienceProfile::Aggressive),
        // `resilience: true` in a file from before 0.13.0, through the
        // environment, where every value arrives as a word.
        "true" | "false" | "1" | "0" | "yes" | "no" | "on" => Err(D::Error::custom(OLD_MEANING)),
        other => Err(D::Error::custom(format!(
          "`{other}` is not a resilience profile; write `off`, `safe` or `aggressive`"
        ))),
      },
      serde_json::Value::Bool(_) => Err(D::Error::custom(OLD_MEANING)),
      other => Err(D::Error::custom(format!(
        "`{other}` is not a resilience profile; write `off`, `safe` or `aggressive`"
      ))),
    }
  }
}

/// What `resilience: true` meant before 0.13.0, and what the key means now.
const OLD_MEANING: &str = "`resilience` picks a profile since 0.13.0: `off`, `safe` or \
  `aggressive`. Serving a cached answer while the backend is down, which `resilience: true` \
  meant, is `cache: {enabled: true, offline: true}`";

impl ResilienceProfile {
  /// The client's backend retry: total attempts, and the first delay in
  /// milliseconds (doubled before each further attempt).
  pub fn client_retry(self) -> (u32, u64) {
    match self {
      ResilienceProfile::Off => (1, 100),
      ResilienceProfile::Safe => (2, 100),
      ResilienceProfile::Aggressive => (3, 50),
    }
  }

  /// The client's circuit breaker: consecutive failures that open it (`0` =
  /// off), and seconds it stays open.
  pub fn client_breaker(self) -> (u32, u64) {
    match self {
      ResilienceProfile::Off | ResilienceProfile::Safe => (0, 30),
      ResilienceProfile::Aggressive => (3, 15),
    }
  }
}

#[cfg(test)]
#[path = "resilience_tests.rs"]
mod tests;
