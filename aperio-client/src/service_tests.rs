use super::*;

#[test]
fn test_reconnect_delay_bounds() {
  // Deterministic cap doubles per attempt: 1s, 2s, 4s ... 60s max; the
  // jittered result must stay within [cap/2, cap].
  for (attempt, cap_ms) in [
    (1u32, 1_000u64),
    (2, 2_000),
    (3, 4_000),
    (7, 60_000),
    (100, 60_000),
  ] {
    for _ in 0..50 {
      let d = reconnect_delay(attempt).as_millis() as u64;
      assert!(
        d >= cap_ms / 2 && d <= cap_ms,
        "attempt {attempt}: delay {d}ms outside [{}ms, {cap_ms}ms]",
        cap_ms / 2
      );
    }
  }
}

#[test]
fn test_fast_reconnect_delay_bounds() {
  // Post-ServerShutdown reconnects skip the backoff: 100–500 ms jitter.
  for _ in 0..50 {
    let d = fast_reconnect_delay().as_millis() as u64;
    assert!(
      (100..=500).contains(&d),
      "fast reconnect delay {d}ms outside [100ms, 500ms]"
    );
  }
}
