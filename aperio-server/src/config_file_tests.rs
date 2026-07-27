use super::*;

#[test]
fn env_name_follows_the_naming_standard() {
  assert_eq!(env_name("max_body_size"), "APERIO_MAX_BODY_SIZE");
  assert_eq!(env_name("server_token"), "APERIO_SERVER_TOKEN");
  // Dashes normalize like the CLI surface does.
  assert_eq!(env_name("lb-strategy"), "APERIO_LB_STRATEGY");
  // Bare keys stay un-prefixed.
  assert_eq!(env_name("host"), "HOST");
  assert_eq!(env_name("port"), "PORT");
  assert_eq!(env_name("log_level"), "LOG_LEVEL");
  // Already-prefixed keys are not double-prefixed.
  assert_eq!(env_name("aperio_cache"), "APERIO_CACHE");
}

#[test]
fn env_value_renders_scalars_and_lists() {
  use serde_yaml::Value;
  assert_eq!(env_value(&Value::Bool(true)).as_deref(), Some("1"));
  assert_eq!(env_value(&Value::Bool(false)).as_deref(), Some("0"));
  assert_eq!(
    env_value(&Value::Number(8080.into())).as_deref(),
    Some("8080")
  );
  assert_eq!(env_value(&Value::String("x".into())).as_deref(), Some("x"));
  let list: Value = serde_yaml::from_str("[10.0.0.0/8, 192.168.0.1]").unwrap();
  assert_eq!(env_value(&list).as_deref(), Some("10.0.0.0/8,192.168.0.1"));
  // Nested structures are not representable.
  let nested: Value = serde_yaml::from_str("[[1]]").unwrap();
  assert_eq!(env_value(&nested), None);
  assert_eq!(env_value(&Value::Null), None);
}

/// Acquires the process-wide config-file test lock, serializing every test that
/// mutates the global document, the `APERIO_SERVER_CONFIG` env var, or the
/// default `aperio-server.yaml` path.
struct CfgGuard(std::path::PathBuf);
impl CfgGuard {
  fn lock() -> Self {
    let lock = std::env::temp_dir().join("aperio-cfgfile-test.lock");
    let start = std::time::Instant::now();
    loop {
      match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
      {
        Ok(_) => return CfgGuard(lock),
        Err(_) => {
          if let Ok(md) = std::fs::metadata(&lock)
            && md
              .modified()
              .ok()
              .and_then(|m| m.elapsed().ok())
              .is_some_and(|e| e.as_secs() > 30)
          {
            let _ = std::fs::remove_file(&lock);
          }
          assert!(
            start.elapsed().as_secs() < 120,
            "config-file test lock timeout"
          );
          std::thread::sleep(std::time::Duration::from_millis(5));
        }
      }
    }
  }
}
impl Drop for CfgGuard {
  fn drop(&mut self) {
    // SAFETY: the loader runs single-threaded in production; here we hold the
    // process-wide lock so no other config-file test races us.
    unsafe { std::env::remove_var("APERIO_SERVER_CONFIG") };
    let _ = std::fs::remove_file("aperio-server.yaml");
    let _ = std::fs::remove_file(&self.0);
  }
}

fn set_config_env(path: &std::path::Path) {
  unsafe { std::env::set_var("APERIO_SERVER_CONFIG", path) };
}
fn clear_config_env() {
  unsafe { std::env::remove_var("APERIO_SERVER_CONFIG") };
}

#[test]
fn config_path_resolves_explicit_and_default() {
  let _g = CfgGuard::lock();
  // Explicit, non-empty path is honored and flagged explicit.
  set_config_env(std::path::Path::new("/tmp/somewhere.yaml"));
  let (p, explicit) = config_path();
  assert_eq!(p, std::path::PathBuf::from("/tmp/somewhere.yaml"));
  assert!(explicit);
  // A blank value falls back to the default (non-explicit).
  set_config_env(std::path::Path::new("   "));
  let (p, explicit) = config_path();
  assert_eq!(p, std::path::PathBuf::from("aperio-server.yaml"));
  assert!(!explicit);
  // Unset falls back to the default too.
  clear_config_env();
  let (p, explicit) = config_path();
  assert_eq!(p, std::path::PathBuf::from("aperio-server.yaml"));
  assert!(!explicit);
}

#[test]
fn load_materializes_scalars_and_keeps_structured_sections() {
  let _g = CfgGuard::lock();
  let file = std::env::temp_dir().join(format!("aperio-cfg-{}.yaml", uuid::Uuid::new_v4()));
  // A mix of every value shape the loader has to handle.
  std::fs::write(
    &file,
    concat!(
      "max_body_size: 4242\n",
      "tunnel_compression: true\n",
      "lb_strategy: round_robin\n",
      "trusted_proxies: [10.0.0.0/8, 192.168.0.0/16]\n",
      "headers:\n  request:\n    add:\n      X-A: b\n",
      "fallbacks:\n  - hostname: a.example.com\n    url: https://x\n",
      "1: numeric-key-ignored\n",
      "nullish: ~\n",
    ),
  )
  .unwrap();
  set_config_env(&file);
  load();

  // Scalars were materialized into their env vars.
  assert_eq!(std::env::var("APERIO_MAX_BODY_SIZE").unwrap(), "4242");
  assert_eq!(std::env::var("APERIO_TUNNEL_COMPRESSION").unwrap(), "1");
  assert_eq!(std::env::var("APERIO_LB_STRATEGY").unwrap(), "round_robin");
  assert_eq!(
    std::env::var("APERIO_TRUSTED_PROXIES").unwrap(),
    "10.0.0.0/8,192.168.0.0/16"
  );
  // A mapping key was NOT turned into an env var but is available structured.
  assert!(std::env::var("APERIO_HEADERS").is_err());
  assert!(structured("headers").is_some());
  // A list-of-mappings key is structured, not an env var.
  assert!(std::env::var("APERIO_FALLBACKS").is_err());
  assert!(structured("fallbacks").is_some());
  // An unknown key returns None from structured.
  assert!(structured("does-not-exist").is_none());
  // The whole document is retained.
  let doc = document().unwrap();
  assert!(doc.contains_key(serde_yaml::Value::String("max_body_size".into())));

  let _ = std::fs::remove_file(&file);
}

#[test]
fn load_flattens_grouped_blocks_into_env_vars() {
  let _g = CfgGuard::lock();
  let file = std::env::temp_dir().join(format!("aperio-cfg-{}.yaml", uuid::Uuid::new_v4()));
  for name in [
    "APERIO_ALERT_ERROR_RATE",
    "APERIO_ALERT_WINDOW",
    "APERIO_CACHE",
    "APERIO_CACHE_MAX_BYTES",
    "APERIO_FAILOVER",
    "APERIO_FAILOVER_WINDOW",
    "APERIO_OIDC_SCOPES",
    "APERIO_RETENTION_AUDIT",
    "APERIO_SERVER_TOKEN",
  ] {
    unsafe { std::env::remove_var(name) };
  }
  std::fs::write(
    &file,
    concat!(
      "alert:\n  error_rate: 0.25\n  window: 60\n",
      // A group whose own key is also a setting: `enabled`/`mode` carry it.
      "cache:\n  enabled: true\n  max_bytes: 1024\n",
      "failover:\n  mode: retry\n  window: 30\n",
      "oidc:\n  scopes: [openid, email]\n",
      "retention:\n  audit: 90\n",
      "server:\n  token: apr_grouped\n",
      // Structured sections keep their meaning: still not env vars.
      "headers:\n  request:\n    add:\n      X-A: b\n",
    ),
  )
  .unwrap();
  set_config_env(&file);
  load();

  assert_eq!(std::env::var("APERIO_ALERT_ERROR_RATE").unwrap(), "0.25");
  assert_eq!(std::env::var("APERIO_ALERT_WINDOW").unwrap(), "60");
  assert_eq!(
    std::env::var("APERIO_CACHE").unwrap(),
    "1",
    "the group's own child keeps the group's own variable"
  );
  assert_eq!(std::env::var("APERIO_CACHE_MAX_BYTES").unwrap(), "1024");
  assert_eq!(std::env::var("APERIO_FAILOVER").unwrap(), "retry");
  assert_eq!(std::env::var("APERIO_FAILOVER_WINDOW").unwrap(), "30");
  assert_eq!(std::env::var("APERIO_OIDC_SCOPES").unwrap(), "openid,email");
  assert_eq!(std::env::var("APERIO_RETENTION_AUDIT").unwrap(), "90");
  assert_eq!(std::env::var("APERIO_SERVER_TOKEN").unwrap(), "apr_grouped");
  // A mapping that is not a known group is still a structured section.
  assert!(std::env::var("APERIO_HEADERS").is_err());
  assert!(structured("headers").is_some());

  let _ = std::fs::remove_file(&file);
}

#[test]
fn load_gives_the_block_precedence_over_flat_keys() {
  let _g = CfgGuard::lock();
  let file = std::env::temp_dir().join(format!("aperio-cfg-{}.yaml", uuid::Uuid::new_v4()));
  for name in ["APERIO_CACHE_MAX_BYTES", "APERIO_FAILOVER_WINDOW"] {
    unsafe { std::env::remove_var(name) };
  }
  // The same setting spelled both ways, with the flat key once before and
  // once after its block: the block must win per field either way, as the
  // documentation promises, not whichever spelling happens to come last.
  std::fs::write(
    &file,
    concat!(
      "cache:\n  max_bytes: 2048\n",
      "cache_max_bytes: 1\n",
      "failover_window: 99\n",
      "failover:\n  window: 30\n",
    ),
  )
  .unwrap();
  set_config_env(&file);
  load();

  assert_eq!(std::env::var("APERIO_CACHE_MAX_BYTES").unwrap(), "2048");
  assert_eq!(std::env::var("APERIO_FAILOVER_WINDOW").unwrap(), "30");

  let _ = std::fs::remove_file(&file);
}

#[test]
fn grouped_blocks_survive_into_the_hot_reload_layer() {
  let _g = CfgGuard::lock();
  let file = std::env::temp_dir().join(format!("aperio-cfg-{}.yaml", uuid::Uuid::new_v4()));
  // A document mixing flat keys, grouped blocks and a structured section:
  // the hot-reload settings layer must see all of the live-editable values.
  // Deserializing the raw document instead of the flattened one failed on
  // the `cache:`/`failover:` blocks (typed as scalars in the settings
  // struct) and silently dropped the whole file layer, so nothing in such a
  // file could be hot-reloaded any more.
  std::fs::write(
    &file,
    concat!(
      "max_body_size: 4242\n",
      "cache:\n  enabled: true\n  max_bytes: 1024\n  max_stale: 60\n",
      "failover:\n  mode: retry\n  window: 30\n",
      "login_lockout:\n  threshold: 9\n",
      "server:\n  auth: \"u:p\"\n",
      "stream:\n  pause_bytes: 111\n  backlog_limit: 999\n",
      "headers:\n  request:\n    add:\n      X-A: b\n",
    ),
  )
  .unwrap();
  set_config_env(&file);
  load();

  let flat = flattened_document().unwrap();
  let key = |k: &str| serde_yaml::Value::String(k.to_string());
  assert_eq!(flat.get(key("cache")), Some(&serde_yaml::Value::Bool(true)));
  assert!(flat.contains_key(key("cache_max_bytes")));
  assert!(
    !flat.contains_key(key("headers")),
    "structured sections stay out of the flattened settings document"
  );

  let o = crate::settings::file_overrides();
  assert_eq!(o.max_body_size, Some(4242));
  assert_eq!(o.cache_enabled, Some(true));
  assert_eq!(o.cache_max_bytes, Some(1024));
  assert_eq!(o.cache_max_stale, Some(60));
  assert_eq!(o.failover_mode.as_deref(), Some("retry"));
  assert_eq!(o.failover_window_secs, Some(30));
  assert_eq!(o.login_lockout_threshold, Some(9));
  assert_eq!(o.auth_credentials.as_deref(), Some("u:p"));
  assert_eq!(o.stream_pause_bytes, Some(111));
  assert_eq!(o.stream_backlog_limit, Some(999));
  assert_eq!(o.stream_resume_bytes, None);

  // `--print-config` attributes block-derived variables to the file, not
  // to the environment.
  let names = materialized_env_names();
  assert!(names.contains("APERIO_MAX_BODY_SIZE"));
  assert!(names.contains("APERIO_CACHE_MAX_BYTES"));
  assert!(names.contains("APERIO_CACHE"));

  let _ = std::fs::remove_file(&file);
}

#[test]
fn load_leaves_an_unknown_mapping_alone() {
  let _g = CfgGuard::lock();
  let file = std::env::temp_dir().join(format!("aperio-cfg-{}.yaml", uuid::Uuid::new_v4()));
  unsafe { std::env::remove_var("APERIO_MYSTERY_A") };
  // Only the groups in the shared table flatten: anything else stays a
  // structured section, so a new feature block (or a typo) is never turned
  // into environment variables behind the operator's back.
  std::fs::write(&file, "mystery:\n  a: 1\n").unwrap();
  set_config_env(&file);
  load();
  assert!(std::env::var("APERIO_MYSTERY_A").is_err());
  assert!(structured("mystery").is_some());
  let _ = std::fs::remove_file(&file);
}

#[test]
fn load_is_a_noop_when_the_default_file_is_absent() {
  let _g = CfgGuard::lock();
  clear_config_env();
  // Ensure the default path does not exist, then load returns without exiting.
  let _ = std::fs::remove_file("aperio-server.yaml");
  load();
}

#[test]
fn watched_path_tracks_explicit_and_existing_files() {
  let _g = CfgGuard::lock();
  // Explicit path is always watched, even if it does not exist yet.
  set_config_env(std::path::Path::new("/tmp/aperio-watched-explicit.yaml"));
  assert_eq!(
    watched_path(),
    Some(std::path::PathBuf::from(
      "/tmp/aperio-watched-explicit.yaml"
    ))
  );
  // Default path: watched only when the file exists.
  clear_config_env();
  let _ = std::fs::remove_file("aperio-server.yaml");
  assert!(watched_path().is_none());
  std::fs::write("aperio-server.yaml", "host: 0.0.0.0\n").unwrap();
  assert_eq!(
    watched_path(),
    Some(std::path::PathBuf::from("aperio-server.yaml"))
  );
  let _ = std::fs::remove_file("aperio-server.yaml");
}

#[test]
fn reload_handles_valid_null_and_error_documents() {
  let _g = CfgGuard::lock();
  clear_config_env();

  // A valid mapping reloads and reports its key count.
  std::fs::write("aperio-server.yaml", "a: 1\nb: 2\n").unwrap();
  assert_eq!(reload().unwrap(), 2);

  // An empty/`null` document is treated as an empty mapping.
  std::fs::write("aperio-server.yaml", "\n").unwrap();
  assert_eq!(reload().unwrap(), 0);

  // A non-mapping top-level (sequence) is a hard error.
  std::fs::write("aperio-server.yaml", "- a\n- b\n").unwrap();
  let err = reload().unwrap_err();
  assert!(err.contains("must be a mapping"));

  // Invalid YAML is an error.
  std::fs::write("aperio-server.yaml", "a: [unterminated\n").unwrap();
  let err = reload().unwrap_err();
  assert!(err.contains("invalid yaml"));

  // A missing file is an error too (explicit path to a nonexistent file).
  let _ = std::fs::remove_file("aperio-server.yaml");
  set_config_env(std::path::Path::new("/tmp/aperio-nonexistent-cfg.yaml"));
  let err = reload().unwrap_err();
  assert!(err.contains("cannot read"));
}

#[test]
fn a_removed_setting_is_still_materialized_so_the_guard_can_see_it() {
  let _g = CfgGuard::lock();
  let file = std::env::temp_dir().join(format!("aperio-cfg-{}.yaml", uuid::Uuid::new_v4()));
  unsafe { std::env::remove_var("APERIO_DASHBOARD_AUTH") };
  // `dashboard_auth` and `dashboard.auth` were both removed from the schema,
  // but the loader is generic: an unknown scalar key still becomes its
  // environment variable. That is what lets the startup guard notice a config
  // that still sets it, in either spelling, rather than ignoring it silently.
  std::fs::write(&file, "dashboard:\n  auth: leftover\n").unwrap();
  set_config_env(&file);
  load();
  assert_eq!(std::env::var("APERIO_DASHBOARD_AUTH").unwrap(), "leftover");

  unsafe { std::env::remove_var("APERIO_DASHBOARD_AUTH") };
  std::fs::write(&file, "dashboard_auth: flat-leftover\n").unwrap();
  load();
  assert_eq!(
    std::env::var("APERIO_DASHBOARD_AUTH").unwrap(),
    "flat-leftover"
  );
  unsafe { std::env::remove_var("APERIO_DASHBOARD_AUTH") };

  let _ = std::fs::remove_file(&file);
}
