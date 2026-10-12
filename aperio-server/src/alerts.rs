//! Threshold-based alerting (webhook events `alert_triggered` /
//! `alert_resolved`).
//!
//! Deliberately small: two rules that cover the incidents a tunnel operator
//! actually wakes up for, evaluated by one background ticker.
//!
//! - **Error rate** (`alerts.error_rate`, a fraction such as `0.05`, unset
//!   = off): the share of failed (5xx) proxied requests over a sliding
//!   window (`alerts.window`, default `5m`) crosses the threshold. Windows
//!   with fewer than `alerts.min_requests` (default 20) requests never
//!   alert, a single failure in a quiet minute is not an incident.
//! - **Client down** (`alerts.client_down`, a duration, unset = off): a
//!   service entity that was seen connected stays down (or disappears) for
//!   longer than the threshold.
//!
//! One `alert_triggered` fires per episode, and one `alert_resolved` when
//! the condition clears (the error-rate rule resolves at 80% of the
//! threshold, so a value hovering at the limit cannot flap). Alerts ride
//! the existing webhook/audit pipeline, point a Slack/Discord webhook at
//! the `alert_triggered` event and it becomes a pager.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::state::AppState;
use crate::store::uptime::Availability;

/// Alerting thresholds (environment / aperio-server.yaml).
///
/// The default is "both built-in rules off", which is what the ticker runs
/// with when it was started only for operator-defined `alert_rules:`.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(crate) struct AlertConfig {
  /// Failed-request percentage that triggers the error-rate alert (0 = off).
  pub(crate) error_rate_pct: f64,
  /// Sliding window the rate is computed over.
  pub(crate) window: Duration,
  /// Minimum requests inside the window before the rule may fire.
  pub(crate) min_requests: u64,
  /// Seconds a known service may stay down before alerting (0 = off).
  pub(crate) client_down: Duration,
}

impl AlertConfig {
  /// Reads the thresholds from the `alerts` settings. `None` = alerting
  /// off. The setting is a fraction; the rule compares percentages.
  pub(crate) fn from_settings() -> Option<Self> {
    let alerts = crate::live::current().alerts.clone();
    let error_rate_pct = alerts.error_rate.filter(|v| *v > 0.0).map(|v| v * 100.0);
    let client_down = alerts.client_down.filter(|d| !d.is_zero());
    if error_rate_pct.is_none() && client_down.is_none() {
      return None;
    }
    Some(AlertConfig {
      error_rate_pct: error_rate_pct.unwrap_or(0.0),
      window: alerts.window.0.max(Duration::from_secs(1)),
      min_requests: u64::from(alerts.min_requests),
      client_down: client_down.map(|d| d.0).unwrap_or_default(),
    })
  }
}

/// A cumulative counter sample for the sliding error-rate window.
struct Sample {
  at: Instant,
  total: u64,
  failed: u64,
}

/// Spawns the alert evaluation ticker (15 s cadence). The built-in
/// thresholds and the operator's rules are read from the settings every
/// tick, so a change applies from the next one and a server started with no
/// alerts can be given some without a restart.
pub(crate) fn spawn(state: Arc<AppState>) {
  if let Some(cfg) = AlertConfig::from_settings() {
    log_thresholds(&cfg);
  }
  crate::supervise::spawn_supervised("alerts", move || {
    let state = state.clone();
    async move {
      let mut samples: VecDeque<Sample> = VecDeque::new();
      let mut error_alert_active = false;
      // Per service entity: when it was last seen not-down, and whether a
      // down alert is currently active for it.
      let mut last_ok: HashMap<String, Instant> = HashMap::new();
      let mut down_alerted: HashMap<String, bool> = HashMap::new();
      // Operator-defined rules keep their own state across ticks
      // (planned_features #49). Read from the live config each tick, so a
      // hot-reload adds or removes a rule without a restart; a rule that
      // disappears simply stops being observed, and its tracker entry is inert.
      let mut rule_tracker = crate::alert_rules::RuleTracker::default();
      let mut in_force = AlertConfig::from_settings().unwrap_or_default();
      loop {
        tokio::time::sleep(Duration::from_secs(15)).await;
        let now = Instant::now();
        let cfg = AlertConfig::from_settings().unwrap_or_default();
        if cfg != in_force {
          log_thresholds(&cfg);
          in_force = cfg;
        }

        let rules = state.config().alert_rules.clone();
        if !rules.is_empty() {
          for rule in rules.rules() {
            let Some(value) = read_metric(&state, rule.metric).await else {
              continue;
            };
            match rule_tracker.observe(rule, value, now) {
              Some(crate::alert_rules::Transition::Fired) => {
                warn!(
                  "ALERT: rule '{}' fired ({} = {}, {} {})",
                  rule.name,
                  rule.metric.as_str(),
                  value,
                  if rule.above { "above" } else { "below" },
                  rule.threshold
                );
                emit(
                  &state,
                  "alert_triggered",
                  serde_json::json!({
                    "kind": rule.name,
                    "metric": rule.metric.as_str(),
                    "value": value,
                    "threshold": rule.threshold,
                    "direction": if rule.above { "above" } else { "below" },
                    "for_secs": rule.sustain.as_secs(),
                  }),
                )
                .await;
              }
              Some(crate::alert_rules::Transition::Resolved) => {
                info!(
                  "ALERT RESOLVED: rule '{}' ({} = {})",
                  rule.name,
                  rule.metric.as_str(),
                  value
                );
                emit(
                  &state,
                  "alert_resolved",
                  serde_json::json!({
                    "kind": rule.name,
                    "metric": rule.metric.as_str(),
                    "value": value,
                  }),
                )
                .await;
              }
              None => {}
            }
          }
        }

        if cfg.error_rate_pct > 0.0 {
          let (successful, failed) = {
            let stats = state.stats.lock().await;
            (stats.successful_requests, stats.failed_requests)
          };
          samples.push_back(Sample {
            at: now,
            total: successful + failed,
            failed,
          });
          while samples
            .front()
            .is_some_and(|s| now.duration_since(s.at) > cfg.window)
          {
            samples.pop_front();
          }
          if let (Some(first), Some(last)) = (samples.front(), samples.back()) {
            let d_total = last.total.saturating_sub(first.total);
            let d_failed = last.failed.saturating_sub(first.failed);
            if d_total >= cfg.min_requests {
              let rate = d_failed as f64 / d_total as f64 * 100.0;
              if !error_alert_active && rate >= cfg.error_rate_pct {
                error_alert_active = true;
                warn!(
                  "ALERT: error rate {:.1}% over the last {}s ({} of {} requests failed)",
                  rate,
                  cfg.window.as_secs(),
                  d_failed,
                  d_total
                );
                emit(
                  &state,
                  "alert_triggered",
                  serde_json::json!({
                    "kind": "error_rate",
                    "rate_pct": (rate * 10.0).round() / 10.0,
                    "threshold_pct": cfg.error_rate_pct,
                    "window_secs": cfg.window.as_secs(),
                    "failed": d_failed,
                    "total": d_total,
                  }),
                )
                .await;
              } else if error_alert_active && rate < cfg.error_rate_pct * 0.8 {
                error_alert_active = false;
                info!("Error-rate alert resolved ({:.1}%)", rate);
                emit(
                  &state,
                  "alert_resolved",
                  serde_json::json!({
                    "kind": "error_rate",
                    "rate_pct": (rate * 10.0).round() / 10.0,
                  }),
                )
                .await;
              }
            }
          }
        }

        if cfg.client_down > Duration::ZERO {
          let live = crate::observe_service_availability(&state).await;
          // Entities currently visible refresh their last-ok time unless down.
          for (name, (status, _org)) in &live {
            if *status != Availability::Down {
              last_ok.insert(name.clone(), now);
            } else {
              last_ok.entry(name.clone()).or_insert(now);
            }
          }
          for (name, seen_ok) in &last_ok {
            let currently_ok = live
              .get(name)
              .is_some_and(|(s, _)| *s != Availability::Down);
            let alerted = down_alerted.get(name).copied().unwrap_or(false);
            if currently_ok {
              if alerted {
                down_alerted.insert(name.clone(), false);
                info!("Client-down alert resolved for '{}'", name);
                emit(
                  &state,
                  "alert_resolved",
                  serde_json::json!({"kind": "client_down", "service": name}),
                )
                .await;
              }
            } else if !alerted && now.duration_since(*seen_ok) >= cfg.client_down {
              down_alerted.insert(name.clone(), true);
              warn!(
                "ALERT: service '{}' has been down for over {}s",
                name,
                cfg.client_down.as_secs()
              );
              emit(
                &state,
                "alert_triggered",
                serde_json::json!({
                  "kind": "client_down",
                  "service": name,
                  "down_secs": now.duration_since(*seen_ok).as_secs(),
                }),
              )
              .await;
            }
          }
          // Forget entities that stayed down for a day past their alert, so
          // decommissioned services do not accumulate forever.
          let forget_after = cfg.client_down + Duration::from_secs(24 * 3600);
          last_ok.retain(|name, seen_ok| {
            let keep = now.duration_since(*seen_ok) < forget_after;
            if !keep {
              down_alerted.remove(name);
            }
            keep
          });
        }
      }
    }
  });
}

/// Says which built-in thresholds are armed.
fn log_thresholds(cfg: &AlertConfig) {
  info!(
    "Alerting thresholds (error_rate: {}, client_down: {})",
    if cfg.error_rate_pct > 0.0 {
      format!(
        "≥{}% over {}s, min {} requests",
        cfg.error_rate_pct,
        cfg.window.as_secs(),
        cfg.min_requests
      )
    } else {
      "off".to_string()
    },
    if cfg.client_down > Duration::ZERO {
      format!("≥{}s", cfg.client_down.as_secs())
    } else {
      "off".to_string()
    },
  );
}

/// Reads one watched quantity. `None` where the platform cannot answer, which
/// keeps a rule quiet rather than firing it on a fabricated zero.
async fn read_metric(state: &Arc<AppState>, metric: crate::alert_rules::Metric) -> Option<f64> {
  use crate::alert_rules::Metric;
  Some(match metric {
    Metric::ConnectedClients => state.clients.read().await.len() as f64,
    Metric::PendingRequests => state.pending_requests.lock().await.len() as f64,
    Metric::StoreBytes => {
      // The same measurement the self-health panel reports, so a rule and the
      // dashboard cannot disagree about how big the store is.
      let data_dir = std::path::PathBuf::from(&state.data_dir);
      crate::api::observe::store_bytes(&data_dir) as f64
    }
    Metric::RssBytes => crate::api::observe::process_rss_bytes()? as f64,
  })
}

async fn emit(state: &Arc<AppState>, event: &str, data: serde_json::Value) {
  state
    .audit(event, "system", "system", &data.to_string())
    .await;
  state.emit_event(event, data).await;
}

#[cfg(test)]
#[path = "alerts_tests.rs"]
mod tests;
