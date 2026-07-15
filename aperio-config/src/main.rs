//! Prints a JSON Schema for the Aperio config files to stdout.
//!
//! - `cargo run -p aperio-config`            → the `aperio.yaml` client schema
//! - `cargo run -p aperio-config -- --server` → the `aperio-server.yaml` schema
//!
//! Handy for CI (the release workflow versions both) and for regenerating a
//! schema by hand.

fn main() {
  let server = std::env::args().any(|a| a == "--server");
  if server {
    println!("{}", aperio_config::server_schema_json());
  } else {
    println!("{}", aperio_config::schema_json());
  }
}
