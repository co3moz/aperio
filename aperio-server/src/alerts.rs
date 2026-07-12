//! Threshold-based alerting (webhook events `alert_triggered` /
//! `alert_resolved`).
//!
//! Deliberately small: two rules that cover the incidents a tunnel operator
//! actually wakes up for, evaluated by one background ticker.
//!
//! - **Error rate** (`APERIO_ALERT_ERROR_RATE`, percent, 0 = off): the share
//!   of failed (5xx) proxied requests over a sliding window
//!   (`APERIO_ALERT_WINDOW`, default 300 s) crosses the threshold. Windows
//!   with fewer than `APERIO_ALERT_MIN_REQUESTS` (default 20) requests never
//!   alert — a single failure in a quiet minute is not an incident.
//! - **Client down** (`APERIO_ALERT_CLIENT_DOWN`, seconds, 0 = off): a
//!   service entity that was seen connected stays down (or disappears) for
//!   longer than the threshold.
//!
//! One `alert_triggered` fires per episode, and one `alert_resolved` when
//! the condition clears (the error-rate rule resolves at 80% of the
//! threshold, so a value hovering at the limit cannot flap). Alerts ride
//! the existing webhook/audit pipeline — point a Slack/Discord webhook at
//! the `alert_triggered` event and it becomes a pager.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::state::AppState;
use crate::store::uptime::Availability;

/// Alerting thresholds (environment / aperio-server.yaml).
#[derive(Clone, Copy)]
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
  /// Reads the thresholds from the environment. `None` = alerting off.
  pub(crate) fn from_env() -> Option<Self> {
    let parse = |key: &str| {
      std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
    };
    let error_rate_pct = parse("APERIO_ALERT_ERROR_RATE");
    let client_down_secs = parse("APERIO_ALERT_CLIENT_DOWN");
    if error_rate_pct.is_none() && client_down_secs.is_none() {
      return None;
    }
    Some(AlertConfig {
      error_rate_pct: error_rate_pct.unwrap_or(0.0),
      window: Duration::from_secs(
        parse("APERIO_ALERT_WINDOW")
          .map(|v| v as u64)
          .unwrap_or(300),
      ),
      min_requests: parse("APERIO_ALERT_MIN_REQUESTS")
        .map(|v| v as u64)
        .unwrap_or(20),
      client_down: Duration::from_secs(client_down_secs.map(|v| v as u64).unwrap_or(0)),
    })
  }
}

/// A cumulative counter sample for the sliding error-rate window.
struct Sample {
  at: Instant,
  total: u64,
  failed: u64,
}

/// Spawns the alert evaluation ticker (15 s cadence).
pub(crate) fn spawn(state: Arc<AppState>, cfg: AlertConfig) {
  info!(
    "Alerting enabled (error_rate: {}, client_down: {})",
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
  tokio::spawn(async move {
    let mut samples: VecDeque<Sample> = VecDeque::new();
    let mut error_alert_active = false;
    // Per service entity: when it was last seen not-down, and whether a
    // down alert is currently active for it.
    let mut last_ok: HashMap<String, Instant> = HashMap::new();
    let mut down_alerted: HashMap<String, bool> = HashMap::new();
    loop {
      tokio::time::sleep(Duration::from_secs(15)).await;
      let now = Instant::now();

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
        for (name, status) in &live {
          if *status != Availability::Down {
            last_ok.insert(name.clone(), now);
          } else {
            last_ok.entry(name.clone()).or_insert(now);
          }
        }
        for (name, seen_ok) in &last_ok {
          let currently_ok = live.get(name).is_some_and(|s| *s != Availability::Down);
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
  });
}

/// Audits and emits one alert event.
async fn emit(state: &Arc<AppState>, event: &str, data: serde_json::Value) {
  state.audit(event, "system", &data.to_string()).await;
  state.emit_event(event, data).await;
}
