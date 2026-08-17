//! What these pin down: that a client cannot damage the metrics namespace it
//! shares with every other client, and that a careless label costs the client
//! that wrote it and nobody else.

use super::*;

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
  pairs
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

#[test]
fn keeps_ordinary_labels_in_name_order() {
  let out = sanitize(&map(&[("region", "eu-west"), ("env", "prod")]));
  // Name order, not announcement order: a series has to be byte-stable
  // between scrapes, and a map's iteration order is not a promise the client
  // made.
  assert_eq!(
    out,
    vec![
      ("env".to_string(), "prod".to_string()),
      ("region".to_string(), "eu-west".to_string()),
    ]
  );
}

#[test]
fn drops_names_prometheus_would_refuse() {
  let out = sanitize(&map(&[
    ("has-dash", "x"),
    ("1leading", "x"),
    ("has space", "x"),
    ("__reserved", "x"),
    ("ok_name", "x"),
  ]));
  assert_eq!(out, vec![("ok_name".to_string(), "x".to_string())]);
}

#[test]
fn drops_labels_the_server_writes_itself() {
  // Two labels of one name is not valid exposition, and `client_id` in
  // particular would let a client label itself as another one.
  let out = sanitize(&map(&[("client_id", "someone-else"), ("env", "prod")]));
  assert_eq!(out, vec![("env".to_string(), "prod".to_string())]);
}

#[test]
fn caps_the_label_count() {
  let pairs: Vec<(String, String)> = (0..20)
    .map(|i| (format!("label_{i:02}"), "v".to_string()))
    .collect();
  let out = sanitize(&pairs.into_iter().collect());
  assert_eq!(out.len(), 8);
}

#[test]
fn drops_oversized_and_empty_values() {
  let long = "v".repeat(65);
  let out = sanitize(&map(&[("a", &long), ("b", "   "), ("c", "fine")]));
  assert_eq!(out, vec![("c".to_string(), "fine".to_string())]);
}

#[test]
fn escapes_what_would_break_a_scrape() {
  let rendered = render(&[("env".to_string(), "pr\"o\\d".to_string())]);
  // Leading comma: these append after `client_id`, so the caller does not
  // have to know whether it is writing the first label or the fifth.
  assert_eq!(rendered, r#",env="pr\"o\\d""#);
  assert!(render(&[]).is_empty());
}

// ---------------------------------------------------------------------------
// The `service` label the server writes on a multiplexed connection
// ---------------------------------------------------------------------------

#[test]
fn a_service_name_a_client_could_weaponize_does_not_reach_the_scrape() {
  // The name arrives on the heartbeat, unvalidated and unbounded, and a client
  // may change it on any heartbeat. Escaping alone would keep the exposition
  // *valid* while letting one tunnel token fill the operator's time-series
  // database, which is the failure the value cap on every other label exists
  // to prevent.
  let huge = "a".repeat(5_000);
  assert_eq!(service_label(Some(&huge), 3).1, "service_3");
  assert_eq!(service_label(Some("  "), 1).1, "service_1");
  assert_eq!(service_label(None, 0).1, "service_0");
  // Exactly at the cap is still the client's own name; one past it is not.
  let at_cap = "b".repeat(MAX_VALUE);
  assert_eq!(service_label(Some(&at_cap), 0).1, at_cap);
  assert_eq!(
    service_label(Some(&"c".repeat(MAX_VALUE + 1)), 0).1,
    "service_0"
  );
}

#[test]
fn a_client_cannot_announce_a_second_service_label() {
  // The server writes `service` itself on a multiplexed connection. A client
  // label of the same name would put two into one series, which is not valid
  // exposition and costs the whole scrape rather than that one client's.
  let mut raw = std::collections::BTreeMap::new();
  raw.insert("service".to_string(), "impostor".to_string());
  raw.insert("env".to_string(), "prod".to_string());
  let kept = sanitize(&raw);
  assert_eq!(kept, vec![("env".to_string(), "prod".to_string())]);
}

#[test]
fn the_service_label_is_escaped_like_any_other_value() {
  // Second line of defence, the way `render`'s own doc puts it: the cap above
  // decides what is kept, and escaping means what is kept cannot break a line.
  let rendered = render(&[service_label(Some("we\"ird\\name\nhere"), 0)]);
  assert_eq!(rendered, r#",service="we\"ird\\name\nhere""#);
}
