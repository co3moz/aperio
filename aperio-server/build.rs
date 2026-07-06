use std::path::Path;
use std::process::Command;

/// Builds the aperio-dashboard frontend (Vite + React) so that its `dist/`
/// output can be embedded into the server binary via rust-embed.
///
/// Set `APERIO_SKIP_DASHBOARD_BUILD=1` to skip the npm invocation and use a
/// previously built `dist/` as-is (used by the Dockerfile, where the
/// dashboard is built in a dedicated Node stage).
fn main() {
  let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
  let dashboard_dir = Path::new(&manifest_dir).join("..").join("aperio-dashboard");

  println!("cargo:rerun-if-env-changed=APERIO_SKIP_DASHBOARD_BUILD");
  for tracked in [
    "src",
    "index.html",
    "auth.html",
    "vite.config.ts",
    "package.json",
    "package-lock.json",
  ] {
    println!(
      "cargo:rerun-if-changed={}",
      dashboard_dir.join(tracked).display()
    );
  }

  if std::env::var("APERIO_SKIP_DASHBOARD_BUILD").is_ok_and(|v| v == "1") {
    return;
  }

  if !dashboard_dir.join("node_modules").exists() {
    run_npm(&dashboard_dir, &["ci"]);
  }
  run_npm(&dashboard_dir, &["run", "build"]);
}

fn run_npm(dir: &Path, args: &[&str]) {
  // On Windows npm is a .cmd shim, which Command cannot spawn directly.
  let mut cmd = if cfg!(windows) {
    let mut c = Command::new("cmd");
    c.arg("/C").arg("npm");
    c
  } else {
    Command::new("npm")
  };
  let status = cmd
    .args(args)
    .current_dir(dir)
    .status()
    .unwrap_or_else(|e| {
      panic!(
        "failed to run `npm {}` in {} (is Node.js installed?): {}",
        args.join(" "),
        dir.display(),
        e
      )
    });
  if !status.success() {
    panic!("`npm {}` failed with {}", args.join(" "), status);
  }
}
