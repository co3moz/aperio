//! What a resilience profile is written as, what it sets, and that the old
//! boolean `resilience` is told what it became rather than read as a profile.

use super::*;

fn parse(yaml: &str) -> Result<ResilienceProfile, String> {
  serde_yaml::from_str::<ResilienceProfile>(yaml).map_err(|e| e.to_string())
}

#[test]
fn the_three_words_and_the_default() {
  assert_eq!(parse("off").unwrap(), ResilienceProfile::Off);
  assert_eq!(parse("Safe").unwrap(), ResilienceProfile::Safe);
  assert_eq!(parse("aggressive").unwrap(), ResilienceProfile::Aggressive);
  assert_eq!(ResilienceProfile::default(), ResilienceProfile::Safe);
}

#[test]
fn the_boolean_of_before_is_told_what_it_became() {
  for old in ["true", "false", "'1'"] {
    let err = parse(old).unwrap_err();
    assert!(
      err.contains("cache: {enabled: true, offline: true}"),
      "{old}: {err}"
    );
  }
  assert!(
    parse("maybe")
      .unwrap_err()
      .contains("not a resilience profile")
  );
}

#[test]
fn off_retries_nothing_and_aggressive_breaks_the_circuit() {
  assert_eq!(ResilienceProfile::Off.client_retry().0, 1);
  assert_eq!(ResilienceProfile::Safe.client_retry().0, 2);
  assert_eq!(ResilienceProfile::Safe.client_breaker().0, 0);
  assert!(ResilienceProfile::Aggressive.client_breaker().0 > 0);
}
