//! The Aperio server binary: everything lives in the library crate, so the
//! router, the state and the tunnel loop are reachable from integration
//! tests, benches and fuzz targets; this file only starts it.

fn main() {
  aperio_server::run();
}
