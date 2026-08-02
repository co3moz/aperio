//! Operator-defined alert rules (the `alert_rules:` section of
//! `aperio-server.yaml`, planned_features #49).
//!
//! Two threshold rules were built in, error rate and client-down, and every
//! other thing an operator might want to be told about needed another pair of
//! environment variables and another branch in the alert loop. The two the
//! backlog named first, the disk filling up and the server's own memory
//! climbing, are both quantities the server already measures for the
//! self-health panel; what was missing was a way to say "tell me when this
//! crosses that".
//!
//! A rule is deliberately small: one measured quantity, one bound, and how
//! long the condition has to hold. It is not an expression language, because
//! the value here is in being able to write the rule at all, and an expression
//! language is a thing to maintain, document and get wrong at 3am.
//!
//! Firing and resolving are symmetric: the condition must hold for `for`
//! seconds to fire and must be false for `for` seconds to resolve. That is
//! what stops a quantity sitting on its threshold from alerting every tick,
//! without inventing a second hidden threshold for hysteresis.

use serde::Deserialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A quantity a rule can watch. Every one of these is something the alert
/// loop can read cheaply on its own tick; nothing here needs a new
/// measurement or a new background task.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Metric {
  /// Tunnel clients currently connected.
  ConnectedClients,
  /// Proxied requests in flight.
  PendingRequests,
  /// On-disk size of the SQLite store and its sidecars, in bytes.
  StoreBytes,
  /// Resident memory of the server process, in bytes. Linux only.
  RssBytes,
}

impl Metric {
  fn parse(raw: &str) -> Option<Metric> {
    match raw.trim().to_ascii_lowercase().as_str() {
      "connected_clients" | "clients" => Some(Metric::ConnectedClients),
      "pending_requests" | "pending" => Some(Metric::PendingRequests),
      "store_bytes" | "disk_bytes" => Some(Metric::StoreBytes),
      "rss_bytes" | "memory_bytes" => Some(Metric::RssBytes),
      _ => None,
    }
  }

  pub(crate) fn as_str(self) -> &'static str {
    match self {
      Metric::ConnectedClients => "connected_clients",
      Metric::PendingRequests => "pending_requests",
      Metric::StoreBytes => "store_bytes",
      Metric::RssBytes => "rss_bytes",
    }
  }

  /// True where this quantity cannot be read on the running platform, so the
  /// operator is told at startup rather than wondering why a rule is quiet.
  pub(crate) fn readable_here(self) -> bool {
    !matches!(self, Metric::RssBytes) || cfg!(target_os = "linux")
  }
}

/// One `alert_rules:` entry as written in the file.
#[derive(Deserialize, Clone, Debug)]
pub(crate) struct RuleRaw {
  name: String,
  metric: String,
  above: Option<f64>,
  below: Option<f64>,
  #[serde(rename = "for")]
  for_secs: Option<u64>,
}

/// One compiled rule.
#[derive(Clone, Debug)]
pub(crate) struct Rule {
  /// Names the alert: it becomes the event's `kind`, so it is what a webhook
  /// receiver switches on.
  pub(crate) name: String,
  pub(crate) metric: Metric,
  /// The bound, and which side of it fires.
  pub(crate) threshold: f64,
  pub(crate) above: bool,
  /// How long the condition must hold, in either direction.
  pub(crate) sustain: Duration,
}

impl Rule {
  /// True when `value` is on the firing side of the bound.
  pub(crate) fn breached(&self, value: f64) -> bool {
    if self.above {
      value > self.threshold
    } else {
      value < self.threshold
    }
  }
}

/// The compiled rule list carried in the server configuration.
#[derive(Default, Clone)]
pub(crate) struct AlertRules {
  rules: Vec<Rule>,
}

impl AlertRules {
  pub(crate) fn is_empty(&self) -> bool {
    self.rules.is_empty()
  }

  pub(crate) fn rules(&self) -> &[Rule] {
    &self.rules
  }

  /// Validates and compiles parsed entries. Every rejection names the rule,
  /// because a rule that silently does not fire is the failure this feature
  /// exists to prevent.
  pub(crate) fn compile(raw: Vec<RuleRaw>) -> Result<Self, String> {
    let mut rules: Vec<Rule> = Vec::with_capacity(raw.len());
    for (i, r) in raw.into_iter().enumerate() {
      let at = |what: &str| format!("alert_rules #{} ({}): {}", i + 1, r.name, what);
      let name = r.name.trim().to_string();
      if name.is_empty() {
        return Err(format!("alert_rules #{}: `name` is required", i + 1));
      }
      if rules.iter().any(|existing| existing.name == name) {
        return Err(at(
          "another rule already has this name; the name becomes the alert's kind, so it has to be unique",
        ));
      }
      let Some(metric) = Metric::parse(&r.metric) else {
        return Err(at(&format!(
          "`{}` is not a metric (connected_clients, pending_requests, store_bytes, rss_bytes)",
          r.metric
        )));
      };
      let (threshold, above) = match (r.above, r.below) {
        (Some(_), Some(_)) => {
          return Err(at("set `above` or `below`, not both"));
        }
        (None, None) => return Err(at("needs `above` or `below`")),
        (Some(v), None) => (v, true),
        (None, Some(v)) => (v, false),
      };
      if !threshold.is_finite() {
        return Err(at("the threshold is not a number"));
      }
      rules.push(Rule {
        name,
        metric,
        threshold,
        above,
        sustain: Duration::from_secs(r.for_secs.unwrap_or(0)),
      });
    }
    Ok(AlertRules { rules })
  }
}

/// Reads and compiles the `alert_rules:` section. A malformed section is a
/// startup error, like `routes:`: an alert rule the operator believes is
/// armed, silently dropped, is worse than no rule at all.
pub(crate) fn from_config_file() -> AlertRules {
  let Some(section) = crate::config_file::structured("alert_rules") else {
    return AlertRules::default();
  };
  let parsed: Result<Vec<RuleRaw>, _> = serde_yaml::from_value(section);
  match parsed
    .map_err(|e| e.to_string())
    .and_then(AlertRules::compile)
  {
    Ok(rules) => rules,
    Err(err) => {
      tracing::error!("invalid `alert_rules:` section in aperio-server.yaml: {err}");
      std::process::exit(1);
    }
  }
}

/// Where one rule stands: how long the condition has held in its current
/// direction, and whether an alert is outstanding.
#[derive(Default)]
struct RuleState {
  /// When the value first crossed to the current side of the threshold.
  since: Option<Instant>,
  /// True while an `alert_triggered` for this rule is outstanding.
  firing: bool,
}

/// Tracks every rule's state across ticks and says what changed.
#[derive(Default)]
pub(crate) struct RuleTracker {
  states: HashMap<String, RuleState>,
}

/// What a tick decided about one rule.
pub(crate) enum Transition {
  /// The condition has held long enough: alert.
  Fired,
  /// It has been clear long enough: resolve.
  Resolved,
}

impl RuleTracker {
  /// Feeds one observation and reports a transition, if any.
  ///
  /// `now` is passed in rather than read here so the whole thing is testable
  /// on a clock the test controls, which is the only way to check a sustain
  /// window without sleeping through it.
  pub(crate) fn observe(&mut self, rule: &Rule, value: f64, now: Instant) -> Option<Transition> {
    let breached = rule.breached(value);
    let state = self.states.entry(rule.name.clone()).or_default();
    // A change of side restarts the clock; staying on the same side keeps it.
    let same_side_as_firing = breached == state.firing;
    if same_side_as_firing {
      state.since = None;
      return None;
    }
    let since = *state.since.get_or_insert(now);
    if now.duration_since(since) < rule.sustain {
      return None;
    }
    state.since = None;
    state.firing = breached;
    Some(if breached {
      Transition::Fired
    } else {
      Transition::Resolved
    })
  }
}

#[cfg(test)]
#[path = "alert_rules_tests.rs"]
mod tests;
