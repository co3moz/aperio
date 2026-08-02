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
