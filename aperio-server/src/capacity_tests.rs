//! What these pin down: that the warning fires when the machine genuinely
//! cannot deliver what the file asks for, and stays silent otherwise. A
//! startup warning that cries wolf is worse than none, because the next one is
//! not read either.

use super::*;

const MB: u64 = 1024 * 1024;

#[test]
fn a_configuration_that_fits_says_nothing() {
  assert!(check(1_000, 500, 64 * MB, Some(65_536), Some(2048 * MB)).is_empty());
}

#[test]
fn connections_beyond_the_descriptor_ceiling_are_named_with_both_numbers() {
  // The default container ceiling against a config that asks for more
  // connections than the process can hold descriptors for. Without this the
  // symptom is `accept` failing with EMFILE at a number nobody configured.
  let warnings = check(4_000, 1_000, 0, Some(1_024), None);
  assert_eq!(
    warnings,
    vec![Warning::NotEnoughDescriptors {
      wanted: 5_000,
      available: 1_024
    }]
  );
}

#[test]
fn the_headroom_is_counted_rather_than_only_the_connections() {
  // Exactly at the ceiling is still wrong: the listeners, the store, the log
  // files and the backend side of every proxied request all need descriptors
  // too, and a config that leaves none for them fails just the same.
  assert!(!check(1_000, 0, 0, Some(1_000), None).is_empty());
  assert!(check(1_000, 0, 0, Some(1_000 + FD_HEADROOM), None).is_empty());
}

#[test]
fn a_cache_larger_than_half_the_memory_limit_is_named() {
  let warnings = check(0, 0, 800 * MB, None, Some(1024 * MB));
  assert_eq!(
    warnings,
    vec![Warning::CacheLargerThanMemory {
      cache: 800 * MB,
      memory: 1024 * MB
    }]
  );
  // A cache that leaves the process room is not worth a line.
  assert!(check(0, 0, 256 * MB, None, Some(1024 * MB)).is_empty());
}

#[test]
fn a_machine_with_no_limits_to_read_is_not_second_guessed() {
  // No cgroup limit and no descriptor ceiling: total RAM is not a budget, it
  // is shared with everything else on the box, and this has nothing to say
  // about it. Silence is the correct answer, not a guess.
  assert!(check(100_000, 100_000, 64 * 1024 * MB, None, None).is_empty());
}

#[test]
fn both_can_be_wrong_at_once() {
  let warnings = check(10_000, 0, 900 * MB, Some(1_024), Some(1024 * MB));
  assert_eq!(warnings.len(), 2, "{warnings:?}");
}
