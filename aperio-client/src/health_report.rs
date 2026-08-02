//! What the client tells the server about *itself* (planned_features #37).
//!
//! Everything the server knew about a client was "is it pinging" and "does its
//! backend answer". Neither says whether the client is the reason a service
//! feels slow: a host at its CPU limit, a machine swapping, or a link whose
//! round trip has quietly gone from 8 ms to 400 ms all look identical from the
//! other end.
//!
//! Two families are measured here, and they answer different questions:
//!
//! * **The process** (CPU, resident memory). Read from `/proc` on Linux and
//!   absent elsewhere, which is honest rather than unfortunate: a wrong number
//!   is worse than no number, and the naive readings on other platforms are
//!   wrong often enough to mislead. Inside a container these are the
//!   *process's* figures, not the cgroup's, so they say what this client is
//!   using and not how close the container is to being killed.
//! * **The link** (round-trip time, jitter, reconnects). Measured from the
//!   client's own ping/pong, so it needs no protocol change beyond reporting
//!   it, and it is the number that distinguishes a slow backend from a slow
//!   tunnel.

use std::sync::Mutex;
use std::time::Instant;

/// Resident-set size of this process in bytes, or `None` where it cannot be
/// read without guessing.
pub(crate) fn rss_bytes() -> Option<u64> {
  #[cfg(target_os = "linux")]
  {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // `sysconf(_SC_PAGESIZE)` is 4 KiB on the platforms Aperio targets, which
    // is the same assumption the server's own self-health figure makes.
    Some(rss_pages * 4096)
  }
  #[cfg(not(target_os = "linux"))]
  {
    None
  }
}

/// Total CPU time this process has used, in seconds.
#[cfg(target_os = "linux")]
fn cpu_seconds() -> Option<f64> {
  let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
  // Fields 14 and 15 (1-based) are utime and stime in clock ticks. The
  // command name in field 2 can contain spaces and parentheses, so the split
  // starts after the closing parenthesis rather than at the first space.
  let rest = stat.rsplit_once(')')?.1;
  let mut fields = rest.split_whitespace();
  let utime: u64 = fields.nth(11)?.parse().ok()?;
  let stime: u64 = fields.next()?.parse().ok()?;
  // `sysconf(_SC_CLK_TCK)` is 100 on every Linux target this ships for.
  Some((utime + stime) as f64 / 100.0)
}

#[cfg(not(target_os = "linux"))]
fn cpu_seconds() -> Option<f64> {
  None
}

/// One CPU sample: the process's CPU time and when it was taken.
struct CpuSample {
  cpu_secs: f64,
  at: Instant,
}

/// Rolling client-health figures, shared between the heartbeat that reports
/// them and the read loop that observes the link.
#[derive(Default)]
pub(crate) struct HealthReport {
  cpu: Mutex<Option<CpuSample>>,
  link: Mutex<LinkStats>,
}

#[derive(Default)]
struct LinkStats {
  /// When the last heartbeat went out, so a Pong can be timed against it.
  ping_sent_at: Option<Instant>,
  /// Smoothed round-trip time in milliseconds.
  rtt_ms: Option<f64>,
  /// Smoothed absolute variation between consecutive round trips, the
  /// definition RTP uses for jitter and the one that answers "is this link
  /// steady" rather than "is it fast".
  jitter_ms: Option<f64>,
  /// Times this connection has been re-established since the process started.
  reconnects: u32,
}

/// Weight of the newest sample in the smoothed values. Low enough that one
/// slow round trip does not become the reported figure, high enough that a
/// link which has genuinely degraded is visible within a few heartbeats.
const SMOOTHING: f64 = 0.25;

impl HealthReport {
  /// CPU used since the previous call, as a percentage of one core. The first
  /// call establishes the baseline and reports nothing, because a percentage
  /// needs two points in time and inventing one from process start would
  /// report an average over the whole run rather than what is happening now.
  pub(crate) fn cpu_percent(&self) -> Option<f64> {
    let now_cpu = cpu_seconds()?;
    let now = Instant::now();
    let mut slot = self.cpu.lock().unwrap_or_else(|e| e.into_inner());
    let previous = slot.replace(CpuSample {
      cpu_secs: now_cpu,
      at: now,
    })?;
    let wall = now.duration_since(previous.at).as_secs_f64();
    if wall <= 0.0 {
      return None;
    }
    let used = (now_cpu - previous.cpu_secs).max(0.0);
    Some(used / wall * 100.0)
  }

  /// Notes that a heartbeat just went out.
  pub(crate) fn ping_sent(&self) {
    self
      .link
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .ping_sent_at = Some(Instant::now());
  }

  /// Notes a Pong, timing it against the heartbeat that prompted it.
  ///
  /// A Pong with no outstanding Ping is ignored rather than timed from
  /// nothing: after a reconnect the two can cross, and a round trip measured
  /// against the wrong send is worse than a missing one.
  pub(crate) fn pong_received(&self) {
    let mut link = self.link.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sent) = link.ping_sent_at.take() else {
      return;
    };
    let sample = sent.elapsed().as_secs_f64() * 1000.0;
    let previous = link.rtt_ms;
    link.rtt_ms = Some(match previous {
      None => sample,
      Some(current) => current * (1.0 - SMOOTHING) + sample * SMOOTHING,
    });
    if let Some(current) = previous {
      let deviation = (sample - current).abs();
      link.jitter_ms = Some(match link.jitter_ms {
        None => deviation,
        Some(j) => j * (1.0 - SMOOTHING) + deviation * SMOOTHING,
      });
    }
  }

  /// Notes that the tunnel connection was re-established.
  ///
  /// The link measurements are reset with it: a round trip over the previous
  /// connection says nothing about this one, and carrying it over would let a
  /// dead link's last reading describe a healthy one.
  pub(crate) fn reconnected(&self) {
    let mut link = self.link.lock().unwrap_or_else(|e| e.into_inner());
    link.reconnects = link.reconnects.saturating_add(1);
    link.ping_sent_at = None;
    link.rtt_ms = None;
    link.jitter_ms = None;
  }

  /// The link figures to announce: round trip, jitter, reconnect count.
  pub(crate) fn link(&self) -> (Option<u64>, Option<u64>, u32) {
    let link = self.link.lock().unwrap_or_else(|e| e.into_inner());
    (
      link.rtt_ms.map(|v| v.round() as u64),
      link.jitter_ms.map(|v| v.round() as u64),
      link.reconnects,
    )
  }
}

/// Test seam: a report whose CPU baseline is already set, so `cpu_percent`
/// answers on the first call.
#[cfg(test)]
impl HealthReport {
  pub(crate) fn with_cpu_baseline(cpu_secs: f64, ago: std::time::Duration) -> Self {
    let report = HealthReport::default();
    *report.cpu.lock().unwrap() = Some(CpuSample {
      cpu_secs,
      at: Instant::now() - ago,
    });
    report
  }
}

#[cfg(test)]
#[path = "health_report_tests.rs"]
mod tests;
