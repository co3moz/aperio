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
      // helpers, scanning only for `env::var` would find the helper's own
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
      "only {} environment reads found in {crate_dir}, the scanner is broken",
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
    custom_name: None,
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
    tunnel_name(&decl(Some("pg_main"), "127.0.0.1:5432", "tcp")),
    "pg_main"
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
    "192_168_3_100_53_udp"
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
  assert!(!looks_like_client_id("pg_main"));
  assert!(validate_tunnel_name("pg_main").is_ok());
}

#[test]
fn a_name_is_limited_to_addressable_characters() {
  assert!(validate_tunnel_name("db_primary_1a").is_ok());
  assert!(validate_tunnel_name("").is_err());
  assert!(validate_tunnel_name("has space").is_err());
  assert!(validate_tunnel_name("has/slash").is_err());
  // The three that used to pass, and are the whole point of the rule: a name
  // is an identifier, so there is exactly one way to write each one.
  assert!(
    validate_tunnel_name("PgMain").is_err(),
    "case is not a variant"
  );
  assert!(validate_tunnel_name("pg-main").is_err(), "`-` is reserved");
  assert!(
    validate_tunnel_name("db.primary").is_err(),
    "`.` is reserved"
  );
  // Not English is not an identifier: `ı` and `i` are one keystroke apart and
  // a different character, which is a bug waiting in a config file.
  assert!(validate_tunnel_name("kayıt").is_err());
  // The message carries the fix, since almost every rejection is mechanical.
  let why = validate_tunnel_name("PG-Main").unwrap_err();
  assert!(why.contains("pg_main"), "{why}");
}

#[test]
fn a_slug_is_a_name_whatever_it_started_as() {
  assert_eq!(slug("PG-Main"), "pg_main");
  assert_eq!(slug("  Acme Inc.  "), "acme_inc");
  assert_eq!(slug("Ödeme Servisi"), "odeme_servisi");
  assert_eq!(slug("Müşteri Portalı"), "musteri_portali");
  assert_eq!(slug("Größe"), "grosse");
  // A script this cannot read becomes separators rather than a guess.
  assert_eq!(slug("数据库"), "unnamed");
  // Never empty: something has to be addressable even when nothing survives.
  assert_eq!(slug("!!!"), "unnamed");
  for raw in ["PG-Main", "Acme Inc.", "!!!", "çğüş", "数据库", "Ödeme"] {
    assert!(validate_name("test", &slug(raw)).is_ok(), "{raw}");
  }
}

#[test]
fn a_bind_entry_accepts_the_short_and_long_forms() {
  // `pg_main: 15432` is the whole entry for most bindings.
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
  assert_eq!(name, "192_168_3_100_53_tcp_udp");
  assert!(validate_tunnel_name(&name).is_ok());
}

// ---------------------------------------------------------------------------
// Single-service keys in a config file (deprecated; removed in 0.9.0).
// ---------------------------------------------------------------------------

#[test]
fn a_file_reports_the_single_service_keys_it_writes() {
  let cfg: FileConfig = serde_yaml::from_str(
    r#"
server:
  url: wss://tunnel.example.com
  token: apr_x
target: http://localhost:3000
hostname: app.example.com
path: /api
"#,
  )
  .unwrap();
  // In file order, so the warning reads the way the file does.
  assert_eq!(
    cfg.single_service_keys(),
    vec!["target", "hostname", "path"]
  );
}

#[test]
fn a_services_file_reports_nothing() {
  // The keys that stay legitimately top-level in the multi-service shape are
  // per-entry *fallbacks*, so none of them may trip the deprecation warning.
  let cfg: FileConfig = serde_yaml::from_str(
    r#"
server:
  url: wss://tunnel.example.com
  token: apr_x
max_concurrent: 8
trim_bind: true
pass_hostname: true
serve_spa: true
services:
  - target: http://localhost:3000
    hostname: app.example.com
"#,
  )
  .unwrap();
  assert!(cfg.single_service_keys().is_empty());
}

#[test]
fn an_empty_single_service_key_is_not_written() {
  // `target: ""` is how a value gets cleared in a templated file; reporting
  // it would tell someone to migrate a key they already removed.
  let cfg: FileConfig = serde_yaml::from_str("target: \"  \"\nserve: \"\"\n").unwrap();
  assert!(cfg.single_service_keys().is_empty());
}

#[test]
fn the_schema_marks_the_single_service_keys_deprecated() {
  // The dashboard's config builder hides a deprecated key unless an imported
  // file already writes it, and editors grey it out. Both read this flag, so
  // the form stops offering the shape we want retired.
  let schema = serde_json::to_value(schemars::schema_for!(FileConfig)).unwrap();
  let props = schema["properties"].as_object().unwrap();
  for key in SINGLE_SERVICE_KEYS {
    assert_eq!(
      props[*key].get("deprecated"),
      Some(&serde_json::Value::Bool(true)),
      "`{key}` must be marked deprecated in the emitted schema"
    );
  }
  // The block spelling of the same claim, and only its `endpoint`: the other
  // children stay top-level defaults, so flagging them would be wrong.
  let defs = schema["$defs"].as_object().unwrap();
  let top = defs["TopHealthConfig"]["properties"].as_object().unwrap();
  assert_eq!(
    top["endpoint"].get("deprecated"),
    Some(&serde_json::Value::Bool(true))
  );
  assert_eq!(top["interval"].get("deprecated"), None);
  // And never on a services: entry, which is where it is now supposed to go.
  let entry = defs["HealthConfig"]["properties"].as_object().unwrap();
  assert_eq!(entry["endpoint"].get("deprecated"), None);
  // A key that is still the right way to write something must not be.
  assert_eq!(props["services"].get("deprecated"), None);
  assert_eq!(props["trim_bind"].get("deprecated"), None);
}

#[test]
fn a_top_level_health_endpoint_counts_as_a_single_service_key() {
  // Both spellings, and only the endpoint: the rest of the block is a real
  // per-entry default and reporting it would be advice to delete a working key.
  let block: FileConfig =
    serde_yaml::from_str("health:\n  endpoint: /health\n  interval: 30\n").unwrap();
  assert_eq!(block.single_service_keys(), vec!["target_health"]);

  let flat: FileConfig = serde_yaml::from_str("target_health: /health\n").unwrap();
  assert_eq!(flat.single_service_keys(), vec!["target_health"]);

  let defaults_only: FileConfig =
    serde_yaml::from_str("health:\n  interval: 30\n  wait_for_backend: true\n").unwrap();
  assert!(defaults_only.single_service_keys().is_empty());
}

#[test]
fn the_top_level_health_block_still_parses_every_field() {
  // The top level has its own type now so `endpoint` can be marked withdrawn
  // there and not on a services: entry. Same fields, so a file written either
  // way must load identically, a schema-only split must not become a parse
  // change.
  let cfg: FileConfig = serde_yaml::from_str(
    "health:\n  endpoint: /h\n  interval: 7\n  timeout: 3\n  threshold: 4\n  wait_for_backend: true\n",
  )
  .unwrap();
  let health = cfg.health.clone().unwrap();
  assert_eq!(health.endpoint.as_deref(), Some("/h"));
  assert_eq!(health.interval, Some(7));
  assert_eq!(health.timeout, Some(3));
  assert_eq!(health.threshold, Some(4));
  assert_eq!(health.wait_for_backend, Some(true));

  let mut folded = cfg;
  folded.fold_groups();
  assert_eq!(folded.target_health.as_deref(), Some("/h"));
  assert_eq!(folded.health_interval, Some(7));
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
fn assert_schema_is_self_documenting(label: &str, schema: serde_json::Value) {
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
  );
}

#[test]
fn the_server_schema_documents_every_key() {
  assert_schema_is_self_documenting(
    "aperio-server.yaml",
    serde_json::to_value(schemars::schema_for!(ServerFileConfig)).unwrap(),
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
fn the_server_schema_examples_use_keys_that_exist() {
  assert_example_keys_exist(
    "aperio-server.yaml",
    &serde_json::to_value(schemars::schema_for!(ServerFileConfig)).unwrap(),
  );
}

#[test]
fn the_client_schema_examples_are_valid_config() {
  assert_examples_parse::<FileConfig>(
    "aperio.yaml",
    serde_json::to_value(schemars::schema_for!(FileConfig)).unwrap(),
  );
}

#[test]
fn the_server_schema_examples_are_valid_config() {
  assert_examples_parse::<ServerFileConfig>(
    "aperio-server.yaml",
    serde_json::to_value(schemars::schema_for!(ServerFileConfig)).unwrap(),
  );
}

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
