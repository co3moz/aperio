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

#[test]
fn every_server_group_child_matches_a_flat_key() {
  // The grouped and flat spellings must stay two ways of writing the same
  // setting: `alert: { window: 60 }` has to land on APERIO_ALERT_WINDOW, the
  // very variable `alert_window:` maps to. This walks the generated schema so
  // a group child added without its flat counterpart (or renamed on one side
  // only) fails here rather than silently reaching no env var.
  let schema: serde_json::Value = serde_json::from_str(&server_schema_json()).unwrap();
  let defs = &schema["$defs"];
  let props = schema["properties"]
    .as_object()
    .expect("the server schema has properties");

  /// Property names of a schema node, following a single `$ref` and the
  /// `anyOf`/`oneOf` a nullable or untagged field generates.
  fn properties(node: &serde_json::Value, defs: &serde_json::Value) -> Vec<String> {
    if let Some(reference) = node["$ref"].as_str() {
      let name = reference.rsplit('/').next().unwrap_or_default();
      return properties(&defs[name], defs);
    }
    if let Some(obj) = node["properties"].as_object() {
      return obj.keys().cloned().collect();
    }
    for key in ["anyOf", "oneOf"] {
      if let Some(items) = node[key].as_array() {
        let mut out: Vec<String> = Vec::new();
        for item in items {
          out.extend(properties(item, defs));
        }
        if !out.is_empty() {
          return out;
        }
      }
    }
    Vec::new()
  }

  for group in SERVER_GROUPS {
    let node = props
      .get(group.key)
      .unwrap_or_else(|| panic!("`{}` is in SERVER_GROUPS but not in the schema", group.key));
    let children = properties(node, defs);
    assert!(
      !children.is_empty(),
      "`{}` has no children in the schema",
      group.key
    );
    for child in children {
      if group.self_key == Some(child.as_str()) {
        assert!(
          props.contains_key(group.key),
          "`{}.{}` stands for the group's own key",
          group.key,
          child
        );
        continue;
      }
      let flat = format!("{}_{}", group.key, child);
      assert!(
        props.contains_key(&flat),
        "`{}.{}` has no flat counterpart `{}`: the block would reach APERIO_{} while nothing else does",
        group.key,
        child,
        flat,
        flat.to_uppercase()
      );
    }
  }
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
];

/// Environment variables the rule deliberately exempts, with the reason.
/// Everything else read from the environment must have a yaml key.
const ENV_EXEMPT: &[(&str, &str)] = &[
  // Bootstrap: selects the yaml file itself, so it cannot live inside it.
  ("APERIO_SERVER_CONFIG", "names the config file"),
  // Third-party spellings the OpenTelemetry spec defines; `env_name` would
  // prefix any yaml key with APERIO_, so these are unreachable by construction
  // and exist only as fallbacks behind APERIO_OTEL_*, which do have keys.
  ("OTEL_EXPORTER_OTLP_ENDPOINT", "OpenTelemetry spec fallback"),
  ("OTEL_SERVICE_NAME", "OpenTelemetry spec fallback"),
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
      // helpers — scanning only for `env::var` would find the helper's own
      // parameter and silently pass the whole client crate.
      for pattern in ["env::var(\"", "env_str(\"", "env_parse(\"", "env_bool(\""] {
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
  // Project rule: yaml is the primary configuration surface and every setting
  // must be reachable from it. A setting that exists only in the environment
  // is invisible to the JSON Schema, so editors neither complete nor validate
  // it and an operator reading the config file cannot see it at all. This
  // scans both binaries for what they actually read and fails on anything the
  // schemas do not declare, so the gap cannot reopen silently.
  let exempt: std::collections::BTreeSet<&str> = ENV_EXEMPT
    .iter()
    .map(|(k, _)| *k)
    .chain(ENV_ALIASES.iter().map(|(k, _)| *k))
    .collect();
  let server_groups: Vec<&str> = SERVER_GROUPS.iter().map(|g| g.key).collect();

  let mut missing: Vec<String> = Vec::new();
  for (crate_dir, schema, groups) in [
    (
      "../aperio-server/src",
      server_schema_json(),
      server_groups.clone(),
    ),
    (
      "../aperio-client/src",
      schema_json(),
      // The client's nested blocks: their children map to
      // APERIO_<BLOCK>_<CHILD> exactly as the server's groups do.
      vec!["health", "server", "scaling", "security_headers"],
    ),
  ] {
    let declared = schema_env_names(&schema, &groups);
    let read = env_vars_read(crate_dir);
    // A scan that finds nothing would pass vacuously; both crates read dozens
    // of variables, so anything near zero means the scanner broke, not that
    // the code got clean.
    assert!(
      read.len() > 20,
      "only {} environment reads found in {crate_dir} — the scanner is broken",
      read.len()
    );
    for var in read {
      if exempt.contains(var.as_str()) || declared.contains(&var) {
        continue;
      }
      missing.push(format!("{crate_dir}: {var}"));
    }
  }
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

// ---------------------------------------------------------------------------
// Tunnel names: the handle a binder and an `expose:` entry address.
// ---------------------------------------------------------------------------

fn decl(name: Option<&str>, target: &str, protocol: &str) -> TunnelDecl {
  TunnelDecl {
    name: name.map(str::to_string),
    target: target.to_string(),
    protocol: protocol.to_string(),
    encrypt: false,
    psk: None,
    idle_timeout: None,
    expose: None,
  }
}

#[test]
fn a_declared_name_is_used_verbatim() {
  assert_eq!(
    tunnel_name(&decl(Some("pg-main"), "127.0.0.1:5432", "tcp")),
    "pg-main"
  );
  assert_eq!(
    tunnel_name(&decl(Some("  spaced  "), "127.0.0.1:5432", "tcp")),
    "spaced"
  );
}

#[test]
fn an_undeclared_name_is_derived_from_the_target() {
  // Unnamed tunnels still need a stable handle, or the whole addressing
  // scheme would only work for files that opted in.
  assert_eq!(
    tunnel_name(&decl(None, "192.168.3.100:53", "udp")),
    "192-168-3-100-53-udp"
  );
  // Protocol is part of it: the same address over tcp and udp are two
  // tunnels, and the client refuses a file where two resolve to one name.
  assert_ne!(
    tunnel_name(&decl(None, "192.168.3.100:53", "udp")),
    tunnel_name(&decl(None, "192.168.3.100:53", "tcp"))
  );
}

#[test]
fn a_derived_name_is_stable() {
  let a = tunnel_name(&decl(None, "127.0.0.1:5432", "tcp"));
  let b = tunnel_name(&decl(None, "127.0.0.1:5432", "tcp"));
  assert_eq!(a, b, "the handle must survive a restart");
}

#[test]
fn a_name_shaped_like_a_client_id_is_refused() {
  // `bind-tunnels:` keys are read as a name and fall back to a client id, so
  // the two shapes have to stay disjoint for that fallback to be unambiguous.
  assert!(looks_like_client_id("3beebfdb-079f-4a00-9e03-1bb6eb9222b4"));
  assert!(validate_tunnel_name("3beebfdb-079f-4a00-9e03-1bb6eb9222b4").is_err());
  assert!(!looks_like_client_id("pg-main"));
  assert!(validate_tunnel_name("pg-main").is_ok());
}

#[test]
fn a_name_is_limited_to_addressable_characters() {
  assert!(validate_tunnel_name("db.primary_1-a").is_ok());
  assert!(validate_tunnel_name("").is_err());
  assert!(validate_tunnel_name("has space").is_err());
  assert!(validate_tunnel_name("has/slash").is_err());
}

#[test]
fn a_bind_entry_accepts_the_short_and_long_forms() {
  // `pg-main: 15432` is the whole entry for most bindings.
  let short: BindTunnelValue = serde_yaml::from_str("15432").unwrap();
  assert_eq!(short.entry().port, Some(15432));

  let long: BindTunnelValue = serde_yaml::from_str("port: 15432\naddress: 0.0.0.0\n").unwrap();
  let entry = long.entry();
  assert_eq!(entry.port, Some(15432));
  assert_eq!(entry.address.as_deref(), Some("0.0.0.0"));
}

// ---------------------------------------------------------------------------
// The combined `tcp/udp` declaration.
// ---------------------------------------------------------------------------

#[test]
fn a_combined_declaration_serves_both_transports() {
  // DNS is the reason this exists: port 53 is genuinely both, and writing it
  // as two declarations meant two names and two entries in every binder.
  assert!(protocol_serves(PROTOCOL_BOTH, "tcp"));
  assert!(protocol_serves(PROTOCOL_BOTH, "udp"));
  assert!(protocol_serves("tcp", "tcp"));
  assert!(!protocol_serves("tcp", "udp"));
  assert!(protocol_serves("udp", "udp"));
  assert!(!protocol_serves("udp", "tcp"));
  // Spacing and case in a hand-written file must not change the answer.
  assert!(protocol_serves("  TCP/UDP ", "udp"));
}

#[test]
fn a_combined_derived_name_stays_addressable() {
  // The protocol goes into a derived name, and a slash is not a character a
  // name may contain, so it has to be folded rather than passed through.
  let name = tunnel_name(&decl(None, "192.168.3.100:53", PROTOCOL_BOTH));
  assert_eq!(name, "192-168-3-100-53-tcp-udp");
  assert!(validate_tunnel_name(&name).is_ok());
}
