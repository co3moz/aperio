//! Tests for config parsing helpers and schema generation.

use super::*;

#[test]
fn security_headers_flag_false_is_empty() {
  assert!(SecurityHeaders::Flag(false).headers().is_empty());
}

#[test]
fn security_headers_flag_true_is_the_standard_preset() {
  let h = SecurityHeaders::Flag(true).headers();
  let names: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
  assert!(names.contains(&"Strict-Transport-Security"));
  assert!(names.contains(&"X-Frame-Options"));
  assert!(names.contains(&"X-Content-Type-Options"));
  assert!(names.contains(&"Referrer-Policy"));
  // HSTS carries the two-year default max-age.
  let hsts = &h
    .iter()
    .find(|(k, _)| k == "Strict-Transport-Security")
    .unwrap()
    .1;
  assert_eq!(hsts, "max-age=63072000");
}

#[test]
fn security_headers_detailed_selects_individually() {
  let opts: SecurityHeaders = serde_json::from_str(
    r#"{"hsts": true, "hsts_max_age": 100, "frame_options": "SAMEORIGIN",
        "nosniff": true, "referrer_policy": "no-referrer", "csp": "default-src 'self'"}"#,
  )
  .unwrap();
  let h = opts.headers();
  assert_eq!(
    h.iter()
      .find(|(k, _)| k == "Strict-Transport-Security")
      .unwrap()
      .1,
    "max-age=100"
  );
  assert_eq!(
    h.iter().find(|(k, _)| k == "X-Frame-Options").unwrap().1,
    "SAMEORIGIN"
  );
  // A browser acts on a fixed set of values and ignores everything else, so a
  // typo would otherwise be a header that looks like protection and is not.
  for (written, sent) in [
    ("no-referrer", "no-referrer"),
    ("Strict-Origin", "strict-origin"),
    ("no-referer", "strict-origin-when-cross-origin"),
  ] {
    let opts: SecurityHeaders =
      serde_json::from_str(&format!(r#"{{"referrer_policy": "{written}"}}"#)).unwrap();
    assert_eq!(
      opts
        .headers()
        .iter()
        .find(|(k, _)| k == "Referrer-Policy")
        .unwrap()
        .1,
      sent,
      "{written}"
    );
  }
  for (written, sent) in [
    ("sameorigin", "SAMEORIGIN"),
    ("DENY", "DENY"),
    ("DENNY", "DENY"),
    ("ALLOW-FROM https://example.com", "DENY"),
  ] {
    let opts: SecurityHeaders =
      serde_json::from_str(&format!(r#"{{"frame_options": "{written}"}}"#)).unwrap();
    assert_eq!(
      opts
        .headers()
        .iter()
        .find(|(k, _)| k == "X-Frame-Options")
        .unwrap()
        .1,
      sent,
      "{written}"
    );
  }
  assert!(h.iter().any(|(k, _)| k == "X-Content-Type-Options"));
  assert_eq!(
    h.iter().find(|(k, _)| k == "Referrer-Policy").unwrap().1,
    "no-referrer"
  );
  assert_eq!(
    h.iter()
      .find(|(k, _)| k == "Content-Security-Policy")
      .unwrap()
      .1,
    "default-src 'self'"
  );
}

#[test]
fn security_headers_detailed_max_age_alone_enables_hsts() {
  let opts: SecurityHeaders = serde_json::from_str(r#"{"hsts_max_age": 42}"#).unwrap();
  let h = opts.headers();
  assert_eq!(
    h.iter()
      .find(|(k, _)| k == "Strict-Transport-Security")
      .unwrap()
      .1,
    "max-age=42"
  );
}

#[test]
fn security_headers_detailed_empty_and_blank_values_are_skipped() {
  // All unset → nothing injected.
  let empty: SecurityHeaders = serde_json::from_str("{}").unwrap();
  assert!(empty.headers().is_empty());
  // Blank string values are trimmed away, not injected.
  let blank: SecurityHeaders =
    serde_json::from_str(r#"{"frame_options": "  ", "referrer_policy": "", "csp": " "}"#).unwrap();
  assert!(blank.headers().is_empty());
}

#[test]
fn security_headers_rejects_unknown_fields() {
  // deny_unknown_fields: a typo'd field is an error, not silently ignored.
  assert!(serde_json::from_str::<SecurityHeaders>(r#"{"frame_option": "DENY"}"#).is_err());
}

#[test]
fn hostnames_flatten_trims_and_drops_empties() {
  assert_eq!(
    Hostnames::One("  app.example.com  ".to_string()).into_vec(),
    vec!["app.example.com".to_string()]
  );
  assert_eq!(
    Hostnames::Many(vec![
      " a.com ".to_string(),
      "".to_string(),
      "b.com".to_string()
    ])
    .into_vec(),
    vec!["a.com".to_string(), "b.com".to_string()]
  );
}

#[test]
fn file_config_resolves_server_url_and_token() {
  // Bare-URL form; token from the flat key.
  let c: FileConfig =
    serde_json::from_str(r#"{"server": "https://t.example.com", "token": "flat"}"#).unwrap();
  assert_eq!(c.server_url().as_deref(), Some("https://t.example.com"));
  assert_eq!(c.server_token().as_deref(), Some("flat"));

  // Section form: nested url + token win.
  let c: FileConfig =
    serde_json::from_str(r#"{"server": {"url": "https://s.example.com", "token": "nested"}}"#)
      .unwrap();
  assert_eq!(c.server_url().as_deref(), Some("https://s.example.com"));
  assert_eq!(c.server_token().as_deref(), Some("nested"));

  // Section without token → falls back to the flat token.
  let c: FileConfig =
    serde_json::from_str(r#"{"server": {"url": "https://s.example.com"}, "token": "fallback"}"#)
      .unwrap();
  assert_eq!(c.server_token().as_deref(), Some("fallback"));

  // No server section at all.
  let c: FileConfig = serde_json::from_str("{}").unwrap();
  assert!(c.server_url().is_none());
  assert!(c.server_token().is_none());
}

#[test]
fn schema_json_outputs_are_valid_json() {
  let client = schema_json();
  let server = server_schema_json();
  assert!(!client.is_empty() && !server.is_empty());
  // Both parse back as JSON objects.
  let cv: serde_json::Value = serde_json::from_str(&client).unwrap();
  let sv: serde_json::Value = serde_json::from_str(&server).unwrap();
  assert!(cv.is_object());
  assert!(sv.is_object());
  // The two schemas are different documents (client vs server config).
  assert_ne!(client, server);
}

/// Environment variables whose name predates the naming standard: each does
/// have a yaml key, it simply is not the one the mechanical mapping would
/// produce. Renaming them would break existing deployments for no functional
/// gain, so they are recorded here instead, with the key they belong to.
const ENV_ALIASES: &[(&str, &str)] = &[
  // yaml `auth`; the APERIO_AUTH spelling was avoided because it reads as a
  // sibling of the server's APERIO_SERVER_AUTH, which is a different setting.
  ("APERIO_VISITOR_AUTH", "auth"),
  // yaml `server.api_key`; predates the `server:` block, when it was a
  // top-level key.
  ("APERIO_API_KEY", "server.api_key"),
  // yaml `circuit_breaker.*`; the block is spelled out in the file, where it
  // is read once and has to be unambiguous, and shortened in the environment,
  // where APERIO_CIRCUIT_BREAKER_FAILURES buys nothing over APERIO_BREAKER_.
  ("APERIO_BREAKER_FAILURES", "circuit_breaker.failures"),
  ("APERIO_BREAKER_OPEN_FOR", "circuit_breaker.open_for"),
];

/// Environment variables the client rule deliberately exempts, with the
/// reason. Everything else the client reads from the environment must have a
/// yaml key.
const ENV_EXEMPT: &[(&str, &str)] = &[
  // Standard tracing override, not a setting of ours.
  ("RUST_LOG", "tracing's own override"),
  // Not settings: process/toolchain environment.
  ("HOME", "home directory lookup"),
  ("USERPROFILE", "home directory lookup"),
  ("CARGO_PKG_VERSION", "build metadata"),
  ("CARGO_MANIFEST_DIR", "build metadata"),
];

/// Collects the `APERIO_*`-style variables a crate reads, by scanning its
/// sources for `env::var("…")`. Test-only files are skipped: a harness may
/// legitimately invent variables that are not settings.
fn env_vars_read(crate_dir: &str) -> std::collections::BTreeSet<String> {
  fn walk(dir: &std::path::Path, out: &mut std::collections::BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
      return;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        walk(&path, out);
        continue;
      }
      let name = path.file_name().unwrap_or_default().to_string_lossy();
      if !name.ends_with(".rs") || name.ends_with("_tests.rs") || name == "test_support.rs" {
        continue;
      }
      let Ok(src) = std::fs::read_to_string(&path) else {
        continue;
      };
      // Direct reads, plus the client's `env_str`/`env_parse`/`env_bool`
      // helpers, scanning only for `env::var` would find the helper's own
      // parameter and silently pass the whole client crate.
      for pattern in [
        "env::var(\"",
        "env_str(\"",
        "env_parse(\"",
        "env_bool(\"",
        "env_nonempty(\"",
      ] {
        for (i, _) in src.match_indices(pattern) {
          let rest = &src[i + pattern.len()..];
          if let Some(end) = rest.find('"') {
            out.insert(rest[..end].to_string());
          }
        }
      }
    }
  }
  let mut out = std::collections::BTreeSet::new();
  walk(std::path::Path::new(crate_dir), &mut out);
  out
}

/// Every key a schema exposes, including the children of grouped blocks,
/// rendered as the environment variable each one maps to.
fn schema_env_names(schema_json: &str, groups: &[&str]) -> std::collections::BTreeSet<String> {
  let schema: serde_json::Value = serde_json::from_str(schema_json).unwrap();
  let defs = &schema["$defs"];
  let mut out = std::collections::BTreeSet::new();
  let Some(props) = schema["properties"].as_object() else {
    return out;
  };
  for (key, node) in props {
    // `host`/`port`/`log_level` map to bare names; everything else is
    // prefixed. Both spellings are recorded so either satisfies a read.
    out.insert(key.to_ascii_uppercase());
    out.insert(format!("APERIO_{}", key.to_ascii_uppercase()));
    if !groups.contains(&key.as_str()) {
      continue;
    }
    for child in group_children(node, defs) {
      out.insert(format!(
        "APERIO_{}_{}",
        key.to_ascii_uppercase(),
        child.to_ascii_uppercase()
      ));
    }
  }
  out
}

/// Property names of a grouped block, following `$ref` and the `anyOf` a
/// nullable or untagged field generates.
fn group_children(node: &serde_json::Value, defs: &serde_json::Value) -> Vec<String> {
  if let Some(reference) = node["$ref"].as_str() {
    let name = reference.rsplit('/').next().unwrap_or_default();
    return group_children(&defs[name], defs);
  }
  if let Some(obj) = node["properties"].as_object() {
    return obj.keys().cloned().collect();
  }
  for key in ["anyOf", "oneOf"] {
    if let Some(items) = node[key].as_array() {
      let mut out = Vec::new();
      for item in items {
        out.extend(group_children(item, defs));
      }
      if !out.is_empty() {
        return out;
      }
    }
  }
  Vec::new()
}

#[test]
fn every_environment_variable_has_a_yaml_key() {
  // Project rule: yaml is the client's primary configuration surface and
  // every setting must be reachable from it. A setting that exists only in
  // the environment is invisible to the JSON Schema, so editors neither
  // complete nor validate it and an operator reading the config file cannot
  // see it at all. This scans the client for what it actually reads and
  // fails on anything the schema does not declare.
  let exempt: std::collections::BTreeSet<&str> = ENV_EXEMPT
    .iter()
    .map(|(k, _)| *k)
    .chain(ENV_ALIASES.iter().map(|(k, _)| *k))
    .collect();
  // The client's nested blocks: their children map to APERIO_<BLOCK>_<CHILD>.
  // `circuit_breaker` is the one exception to the mechanical rule: its
  // variables are APERIO_BREAKER_*, since APERIO_CIRCUIT_BREAKER_FAILURES is
  // a mouthful for no added clarity, so the alias is declared above.
  let groups = vec![
    "health",
    "server",
    "scaling",
    "security_headers",
    "retry",
    "circuit_breaker",
    // `connections:` is a scalar or a `{min, max}` block, so its children are
    // reachable as APERIO_CONNECTIONS_MIN / _MAX like any other grouped key.
    "connections",
    "otel_bridge",
  ];
  let declared = schema_env_names(&schema_json(), &groups);
  let read = env_vars_read("../aperio-client/src");
  // A scan that finds nothing would pass vacuously; the client reads dozens
  // of variables, so anything near zero means the scanner broke.
  assert!(
    read.len() > 20,
    "only {} environment reads found in the client, the scanner is broken",
    read.len()
  );
  let missing: Vec<String> = read
    .into_iter()
    .filter(|var| !exempt.contains(var.as_str()) && !declared.contains(var))
    .collect();
  assert!(
    missing.is_empty(),
    "environment variables with no yaml key (add the field, or exempt it with a reason in ENV_EXEMPT):\n  {}",
    missing.join("\n  ")
  );

  // An alias claims "this variable does have a yaml key, just under another
  // name". Verify that key exists, so the table cannot become a place to hide
  // a genuine gap.
  let client: serde_json::Value = serde_json::from_str(&schema_json()).unwrap();
  let defs = client["$defs"].clone();

  /// Resolves a schema node to a named child, following `$ref` and the
  /// `anyOf` a nullable or untagged field generates.
  fn child(
    node: &serde_json::Value,
    defs: &serde_json::Value,
    name: &str,
  ) -> Option<serde_json::Value> {
    if let Some(reference) = node["$ref"].as_str() {
      let target = reference.rsplit('/').next().unwrap_or_default();
      return child(&defs[target], defs, name);
    }
    if let Some(props) = node["properties"].as_object()
      && let Some(found) = props.get(name)
    {
      return Some(found.clone());
    }
    for key in ["anyOf", "oneOf"] {
      if let Some(items) = node[key].as_array() {
        for item in items {
          if let Some(found) = child(item, defs, name) {
            return Some(found);
          }
        }
      }
    }
    None
  }

  for (var, key) in ENV_ALIASES {
    let mut node = client.clone();
    for part in key.split('.') {
      node = child(&node, &defs, part)
        .unwrap_or_else(|| panic!("{var}: the client schema has no '{key}'"));
    }
  }
}

/// The server's environment is its boot settings, the few it needs before the
/// database opens, and nothing else: everything else is a setting in the
/// database, and a variable read beside them would be a second, invisible
/// surface. Both directions, so a boot setting nobody reads is caught too.
#[test]
fn the_server_reads_exactly_its_boot_settings_from_the_environment() {
  let boot: std::collections::BTreeSet<String> = crate::server_settings::BOOT_ENV
    .iter()
    .map(|(k, _)| k.to_string())
    .collect();
  let read = env_vars_read("../aperio-server/src");
  let stray: Vec<&String> = read
    .iter()
    .filter(|v| !boot.contains(*v) && !ENV_EXEMPT.iter().any(|(k, _)| k == v))
    .collect();
  assert!(
    stray.is_empty(),
    "the server reads environment variables that are not boot settings (make them settings, or add them to BOOT_ENV with a reason): {stray:?}"
  );
  let unread: Vec<&String> = boot.iter().filter(|v| !read.contains(*v)).collect();
  assert!(
    unread.is_empty(),
    "boot settings the server never reads: {unread:?}"
  );
}

// ---------------------------------------------------------------------------
// Schema guidance: what an editor has to work with.
// ---------------------------------------------------------------------------

/// Every property of a schema, as `Scope.key` paths, with the property itself.
fn schema_properties(schema: &serde_json::Value) -> Vec<(String, &serde_json::Value)> {
  let mut out = Vec::new();
  let mut scopes: Vec<(String, &serde_json::Value)> = vec![(String::new(), schema)];
  if let Some(defs) = schema.get("$defs").and_then(|d| d.as_object()) {
    for (name, def) in defs {
      scopes.push((format!("{name}."), def));
    }
  }
  for (prefix, scope) in scopes {
    let Some(props) = scope.get("properties").and_then(|p| p.as_object()) else {
      continue;
    };
    for (key, prop) in props {
      out.push((format!("{prefix}{key}"), prop));
    }
  }
  out
}

/// `object` / `array` / a `$ref`, a shape you cannot guess from the type.
fn is_structured(prop: &serde_json::Value) -> bool {
  let named = |t: &serde_json::Value| matches!(t.as_str(), Some("object") | Some("array"));
  if prop.get("$ref").is_some() {
    return true;
  }
  match prop.get("type") {
    Some(serde_json::Value::String(_)) => named(prop.get("type").unwrap()),
    Some(serde_json::Value::Array(types)) => types.iter().any(named),
    _ => prop
      .get("anyOf")
      .or_else(|| prop.get("oneOf"))
      .and_then(|b| b.as_array())
      .is_some_and(|branches| {
        branches
          .iter()
          .any(|b| b.get("$ref").is_some() || b.get("type").is_some_and(named))
      }),
  }
}

/// A property whose type is a struct of its own, documented field by field.
fn is_section(schema: &serde_json::Value, prop: &serde_json::Value) -> bool {
  prop
    .get("$ref")
    .and_then(|r| r.as_str())
    .and_then(|r| r.rsplit('/').next())
    .and_then(|name| schema["$defs"][name].get("properties"))
    .is_some()
}

/// A key on its way out. An example would invite writing it.
fn is_deprecated(prop: &serde_json::Value) -> bool {
  prop.get("deprecated") == Some(&serde_json::Value::Bool(true))
    || prop
      .get("description")
      .and_then(|d| d.as_str())
      .is_some_and(|d| d.to_lowercase().contains("deprecated"))
}

/// The schemas are the documentation an editor can actually reach: a YAML
/// extension pointed at `aperio-client.schema.json` shows the description and
/// the examples on hover and completion, and nothing else. A key with neither
/// sends the reader to the website, which is exactly the trip the schema
/// exists to save.
///
/// So: a description always, an example on anything whose *shape* cannot be
/// guessed from its type, and an example or a default on the rest.
fn assert_schema_is_self_documenting(label: &str, schema: serde_json::Value, sections: bool) {
  let mut missing_description = Vec::new();
  let mut missing_example = Vec::new();
  for (path, prop) in schema_properties(&schema) {
    if prop
      .get("description")
      .and_then(|d| d.as_str())
      .is_none_or(|d| d.trim().is_empty())
    {
      missing_description.push(path.clone());
    }
    if is_deprecated(prop) {
      continue;
    }
    let has_example = prop.get("examples").is_some();
    let has_default = prop.get("default").is_some();
    // A section of a settings document is a named group of documented
    // fields, which is what an editor completes; its example would be the
    // section's own fields again.
    if sections && is_section(&schema, prop) {
      continue;
    }
    if is_structured(prop) {
      if !has_example {
        missing_example.push(format!("{path} (structured)"));
      }
    } else if !has_example && !has_default {
      missing_example.push(path);
    }
  }
  assert!(
    missing_description.is_empty(),
    "{label}: {} propert(ies) without a doc comment:\n  {}",
    missing_description.len(),
    missing_description.join("\n  ")
  );
  assert!(
    missing_example.is_empty(),
    "{label}: {} propert(ies) an editor can show nothing concrete for. Add `#[schemars(extend(\"examples\" = [...]))]`:\n  {}",
    missing_example.len(),
    missing_example.join("\n  ")
  );
}

#[test]
fn the_client_schema_documents_every_key() {
  assert_schema_is_self_documenting(
    "aperio.yaml",
    serde_json::to_value(schemars::schema_for!(FileConfig)).unwrap(),
    false,
  );
}

#[test]
fn the_server_settings_schema_documents_every_key() {
  assert_schema_is_self_documenting(
    "server settings",
    serde_json::to_value(schemars::schema_for!(
      crate::server_settings::ServerSettings
    ))
    .unwrap(),
    true,
  );
}

/// Builds a document out of every top-level example and parses it back.
///
/// An example that does not deserialize is worse than no example: an editor
/// offers it as the shape to copy, and it produces a file the binary refuses.
/// Three of them were wrong when this check was first written, `rate_limits`
/// carried `max`/`refill` instead of `rps`, `fallbacks` a `respond` block
/// instead of a `url`, `waf` a `contains` instead of a `regex`, all of them
/// written from memory of a neighbouring section and none of them caught by
/// the type system, because an example is just JSON until someone parses it.
fn assert_examples_parse<T: serde::de::DeserializeOwned>(label: &str, schema: serde_json::Value) {
  let props = schema["properties"].as_object().unwrap();
  let mut doc = serde_json::Map::new();
  for (key, prop) in props {
    if is_deprecated(prop) {
      continue;
    }
    let Some(example) = prop
      .get("examples")
      .and_then(|e| e.as_array())
      .and_then(|e| e.first())
    else {
      continue;
    };
    doc.insert(key.clone(), example.clone());
  }
  assert!(
    doc.len() > 20,
    "{label}: only {} examples collected",
    doc.len()
  );
  let yaml = serde_yaml::to_string(&serde_json::Value::Object(doc)).unwrap();
  if let Err(e) = serde_yaml::from_str::<T>(&yaml) {
    panic!(
      "{label}: a document built from the schema's own examples does not parse: {e}\n\n{yaml}"
    );
  }
}

/// Every key inside an example object exists on the type that example is for.
///
/// `assert_examples_parse` cannot see this: an example for a *nested* type is
/// serialized into a document, and a struct without `deny_unknown_fields`
/// accepts a key it has never heard of. So `routes:` shipped an example
/// saying `status: 301` for a type whose field is `permanent: true`, the
/// example parsed, and quietly produced a 302 for anyone who copied it.
fn assert_example_keys_exist(label: &str, schema: &serde_json::Value) {
  let defs = &schema["$defs"];
  // Every `$ref` reachable from a property, through arrays, maps and the
  // `anyOf` a nullable or multi-spelling field turns into.
  fn refs(prop: &serde_json::Value, out: &mut Vec<String>) {
    if let Some(r) = prop.get("$ref").and_then(|r| r.as_str()) {
      out.push(r.rsplit('/').next().unwrap_or(r).to_string());
    }
    for key in ["items", "additionalProperties"] {
      if let Some(child) = prop.get(key) {
        refs(child, out);
      }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
      if let Some(list) = prop.get(key).and_then(|v| v.as_array()) {
        for child in list {
          refs(child, out);
        }
      }
    }
  }
  let mut checked = 0;
  let mut problems: Vec<String> = Vec::new();
  for (key, prop) in schema["properties"].as_object().unwrap() {
    let Some(examples) = prop.get("examples").and_then(|e| e.as_array()) else {
      continue;
    };
    let mut names = Vec::new();
    refs(prop, &mut names);
    // The keys of every type this property could be, together: an untagged
    // enum (the short and long spelling of a key) is genuinely several.
    let known: std::collections::BTreeSet<String> = names
      .iter()
      .filter_map(|name| defs[name].get("properties"))
      .flat_map(|p| p.as_object().unwrap().keys().cloned())
      .collect();
    if known.is_empty() {
      continue;
    }
    for example in examples {
      let entries = match example {
        serde_json::Value::Array(items) => items.clone(),
        other => vec![other.clone()],
      };
      for entry in entries {
        let serde_json::Value::Object(map) = entry else {
          continue;
        };
        checked += 1;
        for field in map.keys() {
          if !known.contains(field) {
            problems.push(format!(
              "{key}: the example uses `{field}`, which no type behind it has (valid: {})",
              known.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
          }
        }
      }
    }
  }
  assert!(
    checked > 3,
    "{label}: only {checked} example object(s) checked"
  );
  assert!(problems.is_empty(), "{label}:\n  {}", problems.join("\n  "));
}

#[test]
fn the_client_schema_examples_use_keys_that_exist() {
  assert_example_keys_exist(
    "aperio.yaml",
    &serde_json::to_value(schemars::schema_for!(FileConfig)).unwrap(),
  );
}

#[test]
fn the_server_settings_examples_use_keys_that_exist() {
  assert_example_keys_exist(
    "server settings",
    &serde_json::to_value(schemars::schema_for!(
      crate::server_settings::ServerSettings
    ))
    .unwrap(),
  );
}

#[test]
fn the_client_schema_examples_are_valid_config() {
  assert_examples_parse::<FileConfig>(
    "aperio.yaml",
    serde_json::to_value(schemars::schema_for!(FileConfig)).unwrap(),
  );
}

/// The server's examples sit one level down, on the fields of each section,
/// so the document is built section by section and parsed as a whole.
#[test]
fn the_server_settings_examples_are_valid_settings() {
  let schema = serde_json::to_value(schemars::schema_for!(
    crate::server_settings::ServerSettings
  ))
  .unwrap();
  let mut doc = serde_json::Map::new();
  let mut count = 0;
  for (key, prop) in schema["properties"].as_object().unwrap() {
    if let Some(example) = prop
      .get("examples")
      .and_then(|e| e.as_array())
      .and_then(|e| e.first())
    {
      doc.insert(key.clone(), example.clone());
      count += 1;
      continue;
    }
    let Some(fields) = prop
      .get("$ref")
      .and_then(|r| r.as_str())
      .and_then(|r| r.rsplit('/').next())
      .and_then(|name| schema["$defs"][name].get("properties"))
      .and_then(|p| p.as_object())
    else {
      continue;
    };
    let mut section = serde_json::Map::new();
    for (field, spec) in fields {
      if let Some(example) = spec
        .get("examples")
        .and_then(|e| e.as_array())
        .and_then(|e| e.first())
      {
        section.insert(field.clone(), example.clone());
        count += 1;
      }
    }
    doc.insert(key.clone(), serde_json::Value::Object(section));
  }
  assert!(count > 10, "only {count} examples collected");
  let yaml = serde_yaml::to_string(&serde_json::Value::Object(doc)).unwrap();
  if let Err(e) = crate::server_settings::ServerSettings::from_yaml(&yaml) {
    panic!("a document built from the settings' own examples does not parse: {e}\n\n{yaml}");
  }
}
