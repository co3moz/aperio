use super::stream::RequestTimeline;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

/// Names of the per-request stages tracked for latency statistics, in
/// timeline order. `queue` and `serve` come from server measurements alone;
/// the middle stages exist only for timing-aware clients.
pub(crate) const STAGE_KEYS: [&str; 7] = [
  "queue",
  "transit_out",
  "client_processing",
  "backend_wait",
  "backend_body",
  "transit_back",
  "serve",
];

/// Rolling per-stage latency window for one route (hostname), feeding the
/// stage-statistics API: mean and standard deviation per stage plus an
/// anomaly verdict for the most recent sample. In-memory only.
pub(crate) struct StageWindow {
  /// Recent samples, one array of per-stage µs durations each (None =
  /// stage not measured for that request).
  samples: std::collections::VecDeque<[Option<u64>; 7]>,
  /// Organization serving this route (`None` = master); the dashboard filters
  /// the per-stage view to the caller's org.
  pub(crate) org_id: Option<String>,
  /// When a sample was last recorded for this route, used to evict the
  /// least-recently-used route when the route cap is hit (bounds memory
  /// under hostname churn, e.g. random preview subdomains).
  last_recorded: Instant,
}

/// Samples kept per route.
const STAGE_WINDOW_CAP: usize = 500;
/// Minimum samples before anomaly verdicts are emitted.
const STAGE_MIN_SAMPLES: usize = 20;
/// Distinct routes tracked; the least-recently-used route is evicted past
/// this so a churn of hostnames cannot grow the map without bound.
pub(crate) const STAGE_ROUTE_CAP: usize = 256;

impl StageWindow {
  fn new(org_id: Option<String>) -> Self {
    StageWindow {
      samples: std::collections::VecDeque::new(),
      org_id,
      last_recorded: Instant::now(),
    }
  }

  /// Extracts per-stage durations from a timeline and records them.
  pub(crate) fn record(&mut self, tl: &RequestTimeline) {
    let diff = |a: Option<u64>, b: Option<u64>| -> Option<u64> {
      match (a, b) {
        (Some(a), Some(b)) => Some(b.saturating_sub(a)),
        _ => None,
      }
    };
    let sample: [Option<u64>; 7] = [
      Some(tl.dispatched_us),
      diff(Some(tl.dispatched_us), tl.client_received_us),
      diff(tl.client_received_us, tl.backend_sent_us),
      diff(tl.backend_sent_us, tl.backend_first_byte_us),
      diff(tl.backend_first_byte_us, tl.backend_done_us),
      diff(tl.client_responded_us, Some(tl.response_received_us)),
      Some(tl.finished_us.saturating_sub(tl.response_received_us)),
    ];
    if self.samples.len() >= STAGE_WINDOW_CAP {
      self.samples.pop_front();
    }
    self.samples.push_back(sample);
    self.last_recorded = Instant::now();
  }

  /// Per-stage statistics of the window. A stage's latest sample is
  /// anomalous when it sits more than three standard deviations above the
  /// mean of a big-enough window.
  pub(crate) fn stats(&self) -> Vec<StageRow> {
    (0..STAGE_KEYS.len())
      .map(|i| {
        let values: Vec<u64> = self.samples.iter().filter_map(|s| s[i]).collect();
        let count = values.len();
        if count == 0 {
          return StageRow {
            stage: STAGE_KEYS[i],
            count: 0,
            mean: 0.0,
            stddev: 0.0,
            last: None,
            anomalous: false,
          };
        }
        let mean = values.iter().sum::<u64>() as f64 / count as f64;
        let var = values
          .iter()
          .map(|v| {
            let d = *v as f64 - mean;
            d * d
          })
          .sum::<f64>()
          / count as f64;
        let stddev = var.sqrt();
        let last = self.samples.back().and_then(|s| s[i]);
        let anomalous = count >= STAGE_MIN_SAMPLES
          && last.is_some_and(|l| l as f64 > mean + 3.0 * stddev && l as f64 > mean * 1.5);
        StageRow {
          stage: STAGE_KEYS[i],
          count,
          mean,
          stddev,
          last,
          anomalous,
        }
      })
      .collect()
  }
}

/// One stage's statistics over the rolling window.
pub(crate) struct StageRow {
  pub(crate) stage: &'static str,
  pub(crate) count: usize,
  pub(crate) mean: f64,
  pub(crate) stddev: f64,
  pub(crate) last: Option<u64>,
  pub(crate) anomalous: bool,
}

/// All routes' stage windows, keyed by request hostname (or `*`).
#[derive(Default)]
pub(crate) struct StageStats {
  pub(crate) routes: std::collections::HashMap<String, StageWindow>,
}

impl StageStats {
  pub(crate) fn record(&mut self, host: Option<&str>, org: Option<&str>, tl: &RequestTimeline) {
    let key = host.unwrap_or("*").to_string();
    // Bound the number of tracked routes: evict the least-recently-used one
    // before admitting a new route past the cap, so a churn of distinct
    // hostnames (wildcard/preview subdomains) cannot grow the map without
    // bound. Only runs when at capacity and a genuinely new route arrives.
    if !self.routes.contains_key(&key)
      && self.routes.len() >= STAGE_ROUTE_CAP
      && let Some(lru) = self
        .routes
        .iter()
        .min_by_key(|(_, w)| w.last_recorded)
        .map(|(k, _)| k.clone())
    {
      self.routes.remove(&lru);
    }
    let window = self
      .routes
      .entry(key)
      .or_insert_with(|| StageWindow::new(org.map(str::to_string)));
    // A route is served by one org; keep its label current.
    window.org_id = org.map(str::to_string);
    window.record(tl);
  }
}

/// Rolling latency window for one endpoint (`host|path`), feeding the
/// slowest-endpoints report. In-memory only, like the stage windows.
pub(crate) struct EndpointWindow {
  /// Durations (ms) of the most recent requests, insertion order.
  durations: std::collections::VecDeque<u64>,
  /// Lifetime request count for this endpoint since server start.
  pub(crate) count: u64,
  /// Lifetime 5xx/error count since server start.
  pub(crate) errors: u64,
  /// Organization serving this endpoint (`None` = master).
  pub(crate) org_id: Option<String>,
}

/// Samples kept per endpoint.
const ENDPOINT_WINDOW_CAP: usize = 200;
/// Distinct endpoints tracked; overflow folds into `__other`.
const ENDPOINT_KEY_CAP: usize = 300;
/// Endpoints with fewer recent samples than this are left out of the report.
pub(crate) const ENDPOINT_MIN_SAMPLES: usize = 5;

impl EndpointWindow {
  /// Latency summary over the recent window: (avg, p50, p95, max) in ms.
  pub(crate) fn summary(&self) -> (f64, u64, u64, u64) {
    if self.durations.is_empty() {
      return (0.0, 0, 0, 0);
    }
    let mut sorted: Vec<u64> = self.durations.iter().copied().collect();
    sorted.sort_unstable();
    let avg = sorted.iter().sum::<u64>() as f64 / sorted.len() as f64;
    let pick =
      |p: f64| sorted[((p * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1)];
    (avg, pick(0.50), pick(0.95), *sorted.last().unwrap_or(&0))
  }

  /// Recent samples in the window.
  pub(crate) fn samples(&self) -> usize {
    self.durations.len()
  }
}

/// All endpoints' latency windows, keyed `host|path` (path without query).
#[derive(Default)]
pub(crate) struct EndpointStats {
  pub(crate) endpoints: std::collections::HashMap<String, EndpointWindow>,
}

impl EndpointStats {
  /// Records one served request for the slowest-endpoints report.
  pub(crate) fn record(
    &mut self,
    host: Option<&str>,
    path: &str,
    status: u16,
    duration_ms: u64,
    org: Option<&str>,
  ) {
    let key = format!("{}|{}", host.unwrap_or("*"), path);
    let key = if self.endpoints.contains_key(&key) || self.endpoints.len() < ENDPOINT_KEY_CAP {
      key
    } else {
      "__other|__other".to_string()
    };
    let w = self.endpoints.entry(key).or_insert_with(|| EndpointWindow {
      durations: std::collections::VecDeque::new(),
      count: 0,
      errors: 0,
      org_id: org.map(str::to_string),
    });
    // A route is served by one org; keep its label current.
    w.org_id = org.map(str::to_string);
    if w.durations.len() >= ENDPOINT_WINDOW_CAP {
      w.durations.pop_front();
    }
    w.durations.push_back(duration_ms);
    w.count += 1;
    if status >= 500 {
      w.errors += 1;
    }
  }
}

/// One one-minute status-class bucket of a route's traffic.
#[derive(Clone, Copy, Default, Serialize)]
pub(crate) struct RouteTrendBucket {
  /// Minute index (unix seconds / 60) this bucket covers.
  pub(crate) minute: u64,
  pub(crate) total: u32,
  pub(crate) s2xx: u32,
  pub(crate) s3xx: u32,
  pub(crate) s4xx: u32,
  pub(crate) s5xx: u32,
}

/// Rolling minute-bucketed status trend for one route (hostname).
pub(crate) struct RouteTrend {
  buckets: VecDeque<RouteTrendBucket>,
  /// Organization serving this route (`None` = master).
  pub(crate) org_id: Option<String>,
}

/// Minute buckets kept per route.
const ROUTE_TREND_MINUTES: usize = 60;
/// Distinct routes tracked (overflow is simply not trended).
const ROUTE_TREND_CAP: usize = 100;

impl RouteTrend {
  /// The last `minutes` buckets ending at `now_minute`, gaps zero-filled,
  /// chronological. Feeds the dashboard sparklines directly.
  pub(crate) fn series(&self, minutes: usize, now_minute: u64) -> Vec<RouteTrendBucket> {
    // Defensive: never let a large `minutes` underflow the start minute (a
    // debug panic / release wrap). The window can't extend before minute 0.
    let start = (now_minute + 1).saturating_sub(minutes as u64);
    (0..minutes)
      .map(|i| {
        let minute = start + i as u64;
        self
          .buckets
          .iter()
          .find(|b| b.minute == minute)
          .copied()
          .unwrap_or(RouteTrendBucket {
            minute,
            ..Default::default()
          })
      })
      .collect()
  }
}

/// All routes' status trends, keyed by request hostname (or `*`).
#[derive(Default)]
pub(crate) struct RouteTrends {
  pub(crate) routes: HashMap<String, RouteTrend>,
}

impl RouteTrends {
  /// Records one served request into its route's current minute bucket.
  pub(crate) fn record(&mut self, host: Option<&str>, status: u16, org: Option<&str>, now: u64) {
    let key = host.unwrap_or("*").to_string();
    if !self.routes.contains_key(&key) && self.routes.len() >= ROUTE_TREND_CAP {
      return;
    }
    let trend = self.routes.entry(key).or_insert_with(|| RouteTrend {
      buckets: VecDeque::new(),
      org_id: org.map(str::to_string),
    });
    trend.org_id = org.map(str::to_string);
    let minute = now / 60;
    if trend.buckets.back().map(|b| b.minute) != Some(minute) {
      if trend.buckets.len() >= ROUTE_TREND_MINUTES {
        trend.buckets.pop_front();
      }
      trend.buckets.push_back(RouteTrendBucket {
        minute,
        ..Default::default()
      });
    }
    let b = trend.buckets.back_mut().expect("bucket just ensured");
    b.total += 1;
    match status {
      200..=299 => b.s2xx += 1,
      300..=399 => b.s3xx += 1,
      400..=499 => b.s4xx += 1,
      _ => b.s5xx += 1,
    }
  }
}

#[cfg(test)]
#[path = "latency_tests.rs"]
mod tests;
