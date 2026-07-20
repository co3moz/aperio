//! Integration test for the `aperio-config` binary: it must print a valid
//! JSON Schema for both the client and server config on stdout. Running the
//! instrumented binary is also what gives `src/main.rs` its coverage.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_aperio-config");

fn run(args: &[&str]) -> String {
  let out = Command::new(BIN)
    .args(args)
    .output()
    .expect("run aperio-config");
  assert!(out.status.success(), "binary exited with failure");
  String::from_utf8(out.stdout).expect("utf-8 stdout")
}

#[test]
fn prints_client_schema_by_default() {
  let stdout = run(&[]);
  let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON schema");
  assert!(v.is_object());
}

#[test]
fn prints_server_schema_with_flag() {
  let client = run(&[]);
  let server = run(&["--server"]);
  let v: serde_json::Value = serde_json::from_str(&server).expect("valid JSON schema");
  assert!(v.is_object());
  // The `--server` schema differs from the default client schema.
  assert_ne!(client, server);
}
