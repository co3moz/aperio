use super::*;

#[test]
fn test_parse_bandwidth() {
  assert_eq!(parse_bandwidth("8mbit"), Some(1_000_000));
  assert_eq!(parse_bandwidth("1gbit"), Some(125_000_000));
  assert_eq!(parse_bandwidth("500kbit"), Some(62_500));
  assert_eq!(parse_bandwidth("2MB"), Some(2_000_000));
  assert_eq!(parse_bandwidth("100kb"), Some(100_000));
  assert_eq!(parse_bandwidth("1.5mbit"), Some(187_500));
  assert_eq!(parse_bandwidth("125000"), Some(125_000));
  assert_eq!(parse_bandwidth("8 Mbit"), Some(1_000_000));
  assert_eq!(parse_bandwidth("0"), None);
  assert_eq!(parse_bandwidth("-5mbit"), None);
  assert_eq!(parse_bandwidth("fast"), None);
}

#[test]
fn test_build_ws_url() {
  assert_eq!(
    build_ws_url("http://localhost:8080").unwrap(),
    "ws://localhost:8080/aperio/ws"
  );
  assert_eq!(
    build_ws_url("https://example.com").unwrap(),
    "wss://example.com/aperio/ws"
  );
  assert_eq!(
    build_ws_url("ws://localhost:8080").unwrap(),
    "ws://localhost:8080/aperio/ws"
  );
  assert_eq!(
    build_ws_url("localhost:8080").unwrap(),
    "ws://localhost:8080/aperio/ws"
  );
  assert!(build_ws_url("ftp://localhost").is_err());
}
