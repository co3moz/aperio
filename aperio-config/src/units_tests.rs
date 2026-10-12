//! What this pins down: a duration and a size are read in the units a person
//! writes, a bare number keeps meaning seconds or bytes, a value is written
//! back in its shortest exact unit, and a typo is an error rather than zero.

use super::*;

#[test]
fn durations_read_every_unit_and_a_bare_number_is_seconds() {
  assert_eq!(Span::parse("30s").unwrap(), Span::secs(30));
  assert_eq!(Span::parse("5m").unwrap(), Span::secs(300));
  assert_eq!(Span::parse("2h").unwrap(), Span::secs(7200));
  assert_eq!(Span::parse("1d").unwrap(), Span::secs(86_400));
  assert_eq!(Span::parse("1w").unwrap(), Span::secs(604_800));
  assert_eq!(Span::parse("250ms").unwrap(), Span::millis(250));
  assert_eq!(Span::parse(" 45 ").unwrap(), Span::secs(45));
  assert_eq!(Span::parse("10 S").unwrap(), Span::secs(10));
}

#[test]
fn a_duration_typo_is_an_error() {
  for bad in ["", "abc", "10x", "-5s", "1.5h"] {
    assert!(Span::parse(bad).is_err(), "{bad:?} should not parse");
  }
}

#[test]
fn a_duration_is_written_in_its_shortest_exact_unit() {
  assert_eq!(Span::secs(90).to_string(), "90s");
  assert_eq!(Span::secs(120).to_string(), "2m");
  assert_eq!(Span::secs(3600).to_string(), "1h");
  assert_eq!(Span::millis(1500).to_string(), "1500ms");
  assert_eq!(Span::secs(0).to_string(), "0s");
}

#[test]
fn durations_round_trip_through_yaml_and_accept_numbers() {
  let v: Span = serde_yaml::from_str("15").unwrap();
  assert_eq!(v, Span::secs(15));
  let v: Span = serde_yaml::from_str("\"2m\"").unwrap();
  assert_eq!(v, Span::secs(120));
  assert_eq!(
    serde_yaml::to_string(&Span::secs(120)).unwrap().trim(),
    "2m"
  );
  assert!(serde_yaml::from_str::<Span>("\"soon\"").is_err());
}

#[test]
fn sizes_read_binary_units_and_a_bare_number_is_bytes() {
  assert_eq!(ByteSize::parse("512KB").unwrap(), ByteSize::kb(512));
  assert_eq!(ByteSize::parse("10MB").unwrap(), ByteSize::mb(10));
  assert_eq!(ByteSize::parse("10MiB").unwrap(), ByteSize::mb(10));
  assert_eq!(ByteSize::parse("1g").unwrap(), ByteSize::gb(1));
  assert_eq!(ByteSize::parse("10485760").unwrap(), ByteSize::mb(10));
  assert_eq!(ByteSize::parse("64 MB").unwrap(), ByteSize::mb(64));
  for bad in ["", "ten", "10TB", "1.5MB"] {
    assert!(ByteSize::parse(bad).is_err(), "{bad:?} should not parse");
  }
}

#[test]
fn a_size_is_written_in_its_largest_exact_unit() {
  assert_eq!(ByteSize::mb(10).to_string(), "10MB");
  assert_eq!(ByteSize::kb(1536).to_string(), "1536KB");
  assert_eq!(ByteSize::bytes(1000).to_string(), "1000");
  let v: ByteSize = serde_yaml::from_str("67108864").unwrap();
  assert_eq!(serde_yaml::to_string(&v).unwrap().trim(), "64MB");
}
