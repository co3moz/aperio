//! The topic filter grammar both ends have to agree on: what `+` and `#`
//! select, where they are legal, and that the server's own namespace cannot be
//! swept up by a leading wildcard.

use super::*;

// ---------------------------------------------------------------------------
// Topic filters.
// ---------------------------------------------------------------------------

#[test]
fn a_filter_matches_the_way_mqtt_says_it_should() {
  assert!(topic_matches("deploy/web", "deploy/web"));
  assert!(!topic_matches("deploy/web", "deploy/api"));
  // `+` is exactly one level, not a substring and not several.
  assert!(topic_matches("deploy/+", "deploy/web"));
  assert!(!topic_matches("deploy/+", "deploy/web/eu"));
  assert!(!topic_matches("deploy/+", "deploy"));
  assert!(topic_matches("+/web", "deploy/web"));
  // `#` is the rest of the tree, including the parent level itself.
  assert!(topic_matches("deploy/#", "deploy/web/eu"));
  assert!(topic_matches("deploy/#", "deploy"));
  assert!(topic_matches("#", "anything/at/all"));
  // A wildcard is a level, never part of one: `dep+` is a literal.
  assert!(!topic_matches("dep+", "deploy"));
  assert!(topic_matches("dep+", "dep+"));
}

#[test]
fn a_bare_wildcard_does_not_sweep_up_server_events() {
  // Subscribing to everything must not silently enroll a client in
  // infrastructure events it never asked to parse, the reason MQTT keeps `#`
  // away from `$SYS`. Asking for them by name still works.
  assert!(!topic_matches("#", "$aperio/client/connected"));
  assert!(!topic_matches(
    "+/client/connected",
    "$aperio/client/connected"
  ));
  assert!(topic_matches("$aperio/#", "$aperio/client/connected"));
  assert!(topic_matches(
    "$aperio/client/+",
    "$aperio/client/connected"
  ));
}

#[test]
fn filters_and_topics_reject_what_would_silently_match_nothing() {
  assert!(validate_topic_filter("deploy/+/eu").is_ok());
  assert!(validate_topic_filter("deploy/#").is_ok());
  assert!(validate_topic_filter("").is_err());
  // A `#` that is not the last level matches nothing and reads like it works.
  assert!(validate_topic_filter("deploy/#/eu").is_err());
  assert!(validate_topic_filter("dep#loy").is_err());
  assert!(validate_topic_filter("dep+loy").is_err());

  assert!(validate_topic("deploy/web").is_ok());
  assert!(validate_topic("").is_err());
  // Publishing to a filter looks like a broadcast and reaches nobody.
  assert!(validate_topic("deploy/#").is_err());
  assert!(validate_topic("deploy/+").is_err());
}

/// Walks one schema's property examples and parses each as a one-key document
/// of the given config type. Returns what was checked, so the caller can
/// assert the walk found anything at all.
fn examples_accepted_by<T: serde::de::DeserializeOwned>(schema: &str, kind: &str) -> usize {
  let root: serde_json::Value = serde_json::from_str(schema).expect("the schema is JSON");
  let mut checked = 0;
  let mut walk = |props: &serde_json::Map<String, serde_json::Value>,
                  wrap: &dyn Fn(&str, &serde_json::Value) -> serde_json::Value| {
    for (key, prop) in props {
      let Some(examples) = prop.get("examples").and_then(|e| e.as_array()) else {
        continue;
      };
      for example in examples {
        let doc = wrap(key, example);
        if let Err(e) = serde_json::from_value::<T>(doc.clone()) {
          panic!(
            "the {kind} schema's example for `{key}` is a config the parser refuses: {e}\n{doc}"
          );
        }
        checked += 1;
      }
    }
  };
  let top = root["properties"].as_object().expect("root properties");
  walk(top, &|key, example| serde_json::json!({ key: example }));
  // The per-service entry carries most of the client's examples; each is
  // checked inside the wrapper it would actually be written in.
  if let Some(entry) = root["$defs"]["ServiceEntry"]["properties"].as_object() {
    walk(
      entry,
      &|key, example| serde_json::json!({ "services": [{ key: example }] }),
    );
  }
  checked
}

#[test]
fn every_schema_example_is_a_configuration_the_parser_accepts() {
  // The examples are what an editor completes and what the docs quote, so a
  // wrong one is a config file that refuses to start, written on our advice.
  // The case that prompted this: the `dashboard:` example still carried the
  // `auth` key whose removal was 0.6.0's Security entry, and the block is
  // deny_unknown_fields, so pasting the example was fatal.
  let client = examples_accepted_by::<FileConfig>(&schema_json(), "client");
  let server = examples_accepted_by::<ServerFileConfig>(&server_schema_json(), "server");
  assert!(client > 80, "the client walk found only {client} examples");
  assert!(server > 60, "the server walk found only {server} examples");
}

/// Every configuration file under docs/examples parses with the type it is
/// written for, and every key a client file writes is a key the schema knows.
///
/// The examples are the copy-and-adapt surface: a pair that does not parse, or
/// a key that drifted from the struct it once matched, is a broken deployment
/// handed out as documentation. The server file's top level tolerates unknown
/// keys by design (they pass through as environment variables), so for it the
/// parse alone is the check; the client has no such pass-through, so an
/// unknown key there is a typo and is treated as one.
#[test]
fn every_docs_example_file_is_a_valid_configuration() {
  let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/examples");
  let schema: serde_json::Value = serde_json::from_str(&schema_json()).unwrap();
  let keys_of = |props: &serde_json::Value| -> std::collections::BTreeSet<String> {
    props.as_object().unwrap().keys().cloned().collect()
  };
  let mut client_keys = keys_of(&schema["properties"]);
  // The schema spells it with the dash; the file may use the serde alias.
  client_keys.insert("bind_tunnels".to_string());
  let service_keys = keys_of(&schema["$defs"]["ServiceEntry"]["properties"]);

  let (mut clients, mut servers) = (0usize, 0usize);
  let mut problems: Vec<String> = Vec::new();
  for folder in std::fs::read_dir(&dir).expect("docs/examples exists") {
    let folder = folder.unwrap().path();
    if !folder.is_dir() {
      continue;
    }
    for file in std::fs::read_dir(&folder).unwrap() {
      let file = file.unwrap().path();
      let name = file.file_name().unwrap().to_string_lossy().to_string();
      if !name.ends_with(".yaml") {
        continue;
      }
      let text = std::fs::read_to_string(&file).unwrap();
      let shown = format!("{}/{name}", folder.file_name().unwrap().to_string_lossy());
      if name == "aperio-server.yaml" {
        servers += 1;
        if let Err(e) = serde_yaml::from_str::<ServerFileConfig>(&text) {
          problems.push(format!("{shown}: {e}"));
        }
        continue;
      }
      clients += 1;
      if let Err(e) = serde_yaml::from_str::<FileConfig>(&text) {
        problems.push(format!("{shown}: {e}"));
        continue;
      }
      let value: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
      let Some(map) = value.as_mapping() else {
        continue;
      };
      for (key, val) in map {
        let key = key.as_str().unwrap_or_default().to_string();
        if !client_keys.contains(&key) {
          problems.push(format!("{shown}: unknown top-level key `{key}`"));
        }
        if key == "services"
          && let Some(entries) = val.as_sequence()
        {
          for entry in entries.iter().filter_map(|e| e.as_mapping()) {
            for field in entry.keys().filter_map(|k| k.as_str()) {
              if !service_keys.contains(field) {
                problems.push(format!("{shown}: unknown service key `{field}`"));
              }
            }
          }
        }
      }
    }
  }
  assert!(problems.is_empty(), "{}", problems.join("\n"));
  // The walk found the tree: both counts move when a folder is added, and a
  // path mistake here must fail loudly rather than check nothing.
  assert!(clients >= 29, "only {clients} client files found");
  assert!(servers >= 29, "only {servers} server files found");
}
