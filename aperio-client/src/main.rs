//! The Aperio client binary: everything lives in the library crate, the same
//! split the server got (planned_features #21), so the service supervisor and
//! the proxy paths are reachable from integration tests; this file only
//! starts it.

fn main() {
  aperio_client::run();
}
