//! Unit tests for `--print-config`.
//!
//! [`render`] reads process-global state (the `APERIO_*` environment and the
//! retained config document) and the filesystem (`settings.json`), so every
//! test serializes on the same cross-thread file lock used by
//! `config_file_tests` / `check_config_tests` and restores the environment on
//! drop.

use super::*;

/// Env vars these tests set; snapshotted and cleared on guard construction,
/// restored on drop.
const KEYS: &[&str] = &[
  "APERIO_SERVER_CONFIG",
  "APERIO_DATA_DIR",
  "APERIO_SERVER_TOKEN",
  "APERIO_MAX_BODY_SIZE",
  "APERIO_TRUSTED_PROXIES",
  "APERIO_RANDOM_SUBDOMAIN",
];

/// Holds the shared config-file lock (same path as the sibling config test
/// modules), snapshots + clears the relevant env vars, and restores them on
/// drop.
struct EnvGuard {
  lock: std::path::PathBuf,
  saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
  fn acquire() -> Self {
    let lock = std::env::temp_dir().join("aperio-cfgfile-test.lock");
    let start = std::time::Instant::now();
    loop {
      match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
      {
        Ok(_) => break,
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
    let saved = KEYS.iter().map(|k| (*k, std::env::var(k).ok())).collect();
    for k in KEYS {
      unsafe { std::env::remove_var(k) };
    }
    EnvGuard { lock, saved }
  }
}

impl Drop for EnvGuard {
  fn drop(&mut self) {
    for (k, v) in &self.saved {
      match v {
        Some(val) => unsafe { std::env::set_var(k, val) },
        None => unsafe { std::env::remove_var(k) },
      }
    }
    let _ = std::fs::remove_file(&self.lock);
  }
}

/// Writes `yaml` to a fresh temp file, points `APERIO_SERVER_CONFIG` at it and
/// loads it so the document/env are populated. Returns the file path.
fn load_config(yaml: &str) -> std::path::PathBuf {
  let file = std::env::temp_dir().join(format!("aperio-printcfg-{}.yaml", uuid::Uuid::new_v4()));
  std::fs::write(&file, yaml).unwrap();
  unsafe { std::env::set_var("APERIO_SERVER_CONFIG", file.to_str().unwrap()) };
  crate::config_file::load();
  file
}

/// Points `APERIO_DATA_DIR` at a fresh temp dir and returns it.
fn fresh_data_dir() -> std::path::PathBuf {
  let dir = std::env::temp_dir().join(format!("aperio-printcfg-data-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  unsafe { std::env::set_var("APERIO_DATA_DIR", dir.to_str().unwrap()) };
  dir
}

#[test]
fn reports_env_and_yaml_sources_with_masking() {
  let _g = EnvGuard::acquire();
  let data = fresh_data_dir();
  // A real-environment secret, plus file-sourced scalars and a structured
  // section.
  unsafe { std::env::set_var("APERIO_SERVER_TOKEN", "super-secret-token") };
  let file = load_config(concat!(
    "max_body_size: 4242\n",
    "trusted_proxies: [10.0.0.0/8, 192.168.0.0/16]\n",
    "headers:\n  request:\n    add:\n      X-A: b\n",
  ));

  let out = render();

  // File-materialized scalars are attributed to the file. (Keys are padded to
  // align, so assert on the key and on the value+source tail separately.)
  assert!(out.contains("APERIO_MAX_BODY_SIZE"), "{out}");
  assert!(out.contains("= 4242  [aperio-server.yaml]"), "{out}");
  assert!(out.contains("APERIO_TRUSTED_PROXIES"), "{out}");
  assert!(
    out.contains("= 10.0.0.0/8,192.168.0.0/16  [aperio-server.yaml]"),
    "{out}"
  );
  // The real-environment token is attributed to the environment and masked.
  assert!(out.contains("APERIO_SERVER_TOKEN"), "{out}");
  assert!(out.contains("[env]"), "{out}");
  assert!(!out.contains("super-secret-token"), "token leaked: {out}");
  assert!(out.contains(crate::redact::mask()), "{out}");
  // The structured section is listed, not turned into a variable.
  assert!(
    out.contains("Structured aperio-server.yaml sections: headers"),
    "{out}"
  );
  assert!(!out.contains("APERIO_HEADERS ="), "{out}");
  // No settings.json in the fresh data dir.
  assert!(out.contains("Dashboard overrides: none"), "{out}");
  assert!(out.contains(&data.display().to_string()), "{out}");

  let _ = std::fs::remove_file(&file);
  let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn lists_dashboard_overrides_and_masks_secret_keys() {
  let _g = EnvGuard::acquire();
  let data = fresh_data_dir();
  load_config("max_body_size: 10\n");
  std::fs::write(
    data.join("settings.json"),
    r#"{"cache_enabled": true, "auth_credentials": "user:pass", "ui_language": null}"#,
  )
  .unwrap();

  let out = render();

  assert!(out.contains("Dashboard overrides"), "{out}");
  assert!(out.contains("cache_enabled"), "{out}");
  assert!(out.contains("= true"), "{out}");
  // Secret-named key masked; null-valued key omitted.
  assert!(!out.contains("user:pass"), "credentials leaked: {out}");
  assert!(out.contains("auth_credentials ="), "{out}");
  assert!(
    !out.contains("ui_language"),
    "null override should be omitted: {out}"
  );

  let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn summarizes_over_long_values() {
  let _g = EnvGuard::acquire();
  let data = fresh_data_dir();
  let long = "x".repeat(90);
  unsafe { std::env::set_var("APERIO_RANDOM_SUBDOMAIN", &long) };
  load_config("max_body_size: 1\n");

  let out = render();

  // Key padding depends on the widest variable in the (process-global) env,
  // so assert on the key and the summarized-value tail separately.
  assert!(out.contains("APERIO_RANDOM_SUBDOMAIN"), "{out}");
  assert!(out.contains("= <90 chars>"), "{out}");
  assert!(
    !out.contains(&long),
    "long value should be summarized: {out}"
  );

  let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn reports_missing_explicit_config_file() {
  let _g = EnvGuard::acquire();
  let _data = fresh_data_dir();
  // Explicit path that does not exist.
  let missing = std::env::temp_dir().join(format!("aperio-absent-{}.yaml", uuid::Uuid::new_v4()));
  unsafe { std::env::set_var("APERIO_SERVER_CONFIG", missing.to_str().unwrap()) };

  let out = render();
  assert!(out.contains("(not present)"), "{out}");
  let _ = std::fs::remove_dir_all(&_data);
}

#[test]
fn run_prints_and_returns_zero() {
  let _g = EnvGuard::acquire();
  let data = fresh_data_dir();
  load_config("max_body_size: 1\n");
  assert_eq!(run(), 0);
  let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn display_value_passes_short_nonsecret_through() {
  assert_eq!(display_value("APERIO_MAX_BODY_SIZE", "4242"), "4242");
  assert_eq!(
    display_value("APERIO_SERVER_TOKEN", "abc"),
    crate::redact::mask()
  );
}
