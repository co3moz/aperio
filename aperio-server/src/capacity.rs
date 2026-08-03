//! Warning when a capacity setting does not fit the machine
//! (planned_features #1).
//!
//! ## Why this warns rather than derives
//!
//! The entry started as "auto-tune the limits from cgroup CPU/memory and the
//! file-descriptor ceiling". That was dropped, and the reason is worth keeping
//! here: the 0.7 configuration work spent itself on making "which value is in
//! effect, and where does it come from" answerable, and a number that silently
//! changes because the host changed is the opposite of that. It also changes
//! under the operator who moved the same file to a bigger box and expected the
//! same behavior.
//!
//! So the file still says what it says. This only points out when the file
//! asks for something the machine cannot give, at startup, once, with both
//! numbers named.
//!
//! ## Only checks worth a line
//!
//! Two, and they were chosen by asking what an operator can act on and what
//! fails obscurely without them.
//!
//! * **Connections against the file-descriptor ceiling.** Every tunnel and
//!   every proxied WebSocket is at least one descriptor. Past the ceiling the
//!   server does not refuse politely, `accept` returns `EMFILE`, and the
//!   symptom is connections failing at a number nobody configured.
//! * **The response cache against the memory limit.** The cache is a
//!   deliberate reservation, so a budget larger than the container's memory is
//!   unambiguously a mistake rather than a risk somebody accepted.
//!
//! Deliberately absent: anything derived from `max_body_size` times a
//! concurrency limit. That product is a worst case reached by roughly no
//! deployment, so warning on it would fire constantly and be ignored, which is
//! how a warning stops being read at all.

use tracing::warn;

/// The soft file-descriptor limit for this process.
///
/// Read from `/proc/self/limits` rather than through `getrlimit` so this stays
/// one text read with no unsafe block, and returns `None` everywhere the file
/// does not exist, which is the same thing as "this check does not apply
/// here".
fn fd_soft_limit() -> Option<u64> {
  let text = std::fs::read_to_string("/proc/self/limits").ok()?;
  for line in text.lines() {
    let Some(rest) = line.strip_prefix("Max open files") else {
      continue;
    };
    let soft = rest.split_whitespace().next()?;
    if soft.eq_ignore_ascii_case("unlimited") {
      return None;
    }
    return soft.parse().ok();
  }
  None
}

/// The memory ceiling this process runs under, from cgroup v2 then v1.
///
/// `None` when there is no limit, when it is the "no limit" sentinel, or when
/// the files are absent. A host with no cgroup limit is not a machine this can
/// say anything about: the total RAM is not a budget, it is shared with
/// everything else on the box.
fn memory_limit_bytes() -> Option<u64> {
  let read = |path: &str| -> Option<u64> {
    let raw = std::fs::read_to_string(path).ok()?;
    let raw = raw.trim();
    if raw == "max" {
      return None;
    }
    let value: u64 = raw.parse().ok()?;
    // cgroup v1 writes a number near u64::MAX to mean "no limit".
    (value < u64::MAX / 2).then_some(value)
  };
  read("/sys/fs/cgroup/memory.max").or_else(|| read("/sys/fs/cgroup/memory/memory.limit_in_bytes"))
}

/// Descriptors reserved for everything that is not a counted connection: the
/// listeners, the store and its journal, the log files, the backend side of
/// proxied requests, and whatever the runtime holds.
///
/// A round number rather than a measurement, and generous on purpose: the
/// point of the check is to catch a ceiling that is wrong by an order of
/// magnitude, not to litigate the last hundred descriptors.
const FD_HEADROOM: u64 = 256;

/// One thing the machine cannot give, phrased for the log.
#[derive(PartialEq, Eq, Debug)]
pub(crate) enum Warning {
  NotEnoughDescriptors { wanted: u64, available: u64 },
  CacheLargerThanMemory { cache: u64, memory: u64 },
}

/// The checks themselves, against limits already read.
///
/// Separated from the reading so the decision is testable without a machine
/// that happens to have the right ceilings: what is worth pinning down is when
/// this speaks and when it stays quiet, not whether `/proc` parses.
pub(crate) fn check(
  max_ws_connections: usize,
  max_tunnels: usize,
  cache_max_bytes: u64,
  fd_limit: Option<u64>,
  memory_limit: Option<u64>,
) -> Vec<Warning> {
  let mut out = Vec::new();
  let wanted = max_ws_connections as u64 + max_tunnels as u64;
  if let Some(available) = fd_limit
    && wanted + FD_HEADROOM > available
  {
    out.push(Warning::NotEnoughDescriptors { wanted, available });
  }
  if let Some(memory) = memory_limit
    && cache_max_bytes > memory / 2
  {
    out.push(Warning::CacheLargerThanMemory {
      cache: cache_max_bytes,
      memory,
    });
  }
  out
}

/// Runs the checks against this machine and warns. Called once at startup.
pub(crate) fn warn_if_beyond_the_machine(
  max_ws_connections: usize,
  max_tunnels: usize,
  cache_max_bytes: u64,
) {
  for warning in check(
    max_ws_connections,
    max_tunnels,
    cache_max_bytes,
    fd_soft_limit(),
    memory_limit_bytes(),
  ) {
    match warning {
      Warning::NotEnoughDescriptors { wanted, available } => {
        warn!(
          "max_ws_connections ({}) plus max_tunnels ({}) is {} connections, and this process may \
           open only {} file descriptors. Past that, `accept` fails with EMFILE and connections \
           break at a number nobody configured. Raise the limit (`ulimit -n`, or LimitNOFILE= in \
           a systemd unit) or lower the two settings.",
          max_ws_connections, max_tunnels, wanted, available
        );
      }
      Warning::CacheLargerThanMemory { cache, memory } => {
        warn!(
          "The response cache budget is {} MB and this process runs under a {} MB memory limit. \
           The cache is a deliberate reservation, so it will be filled: lower cache_max_bytes, or \
           give the container more memory.",
          cache / (1024 * 1024),
          memory / (1024 * 1024)
        );
      }
    }
  }
}

#[cfg(test)]
#[path = "capacity_tests.rs"]
mod tests;
