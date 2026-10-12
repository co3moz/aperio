//! `aperio-client init`: the command line and the environment, written down
//! as the `aperio.yaml` they describe.
//!
//! A one-liner is how a client starts life, and a file is where it ends up.
//! The two used to be different shapes with nothing between them, so moving
//! from one to the other meant reading the reference. `init` takes the one
//! service the command line describes, and every other setting it was given,
//! and writes the file that means the same thing.

use aperio_config::layers::{self, Layer, Overlay};
use serde_yaml::{Mapping, Value};

/// The token is not written into the file: a secret typed into a file ends up
/// in a repository. The file names the variable instead.
const TOKEN_PLACEHOLDER: &str = "${APERIO_SERVER_TOKEN}";

/// The name a service gets when none was given: the first label of its
/// hostname, or `web`.
fn service_name(service: &Mapping) -> String {
  let host = match service.get("hostname") {
    Some(Value::String(h)) => h.split(',').next().map(str::to_string),
    Some(Value::Sequence(hs)) => hs.first().and_then(|h| h.as_str()).map(str::to_string),
    _ => None,
  };
  let name: String = host
    .as_deref()
    .and_then(|h| h.trim().split('.').next())
    .unwrap_or("")
    .to_ascii_lowercase()
    .chars()
    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
    .collect();
  let name = name.trim_matches('_').to_string();
  if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
    "web".to_string()
  } else {
    name
  }
}

/// The document the overlays describe, in the order a person would write it:
/// the server, the client's name, what every service shares, the service.
pub(crate) fn document(overlays: &[Overlay], name: Option<&str>) -> Result<Mapping, String> {
  let mut file = Mapping::new();
  let mut service = Mapping::new();
  for overlay in overlays {
    layers::merge(&mut file, overlay.file.clone());
    layers::merge(&mut service, overlay.service.clone());
  }
  let named = |key: &str| service.get(key).is_some_and(|v| !v.is_null());
  if !(named("target") || named("serve") || named("tcp_target")) {
    return Err(
      "init writes the service the command line describes, so it needs one: \
       `aperio-client init 3000 --hostname app.example.com`, or --serve DIR"
        .to_string(),
    );
  }
  if let Some(Value::String(target)) = service.get("target").cloned() {
    service.insert(
      Value::String("target".into()),
      Value::String(crate::config::normalize_target(&target)),
    );
  }
  let name = match name {
    Some(n) => {
      aperio_config::validate_name("service", n)?;
      n.to_string()
    }
    None => service_name(&service),
  };

  let mut doc = Mapping::new();
  if let Some(Value::Mapping(mut server)) = file.remove("server") {
    if server.contains_key("token") {
      server.insert(
        Value::String("token".into()),
        Value::String(TOKEN_PLACEHOLDER.into()),
      );
    }
    doc.insert(Value::String("server".into()), Value::Mapping(server));
  }
  for key in ["name", "client_id"] {
    if let Some(value) = file.remove(key) {
      doc.insert(Value::String(key.into()), value);
    }
  }
  let defaults = file.remove("defaults");
  // Everything else the layers set, as written.
  for (key, value) in file {
    doc.insert(key, value);
  }
  if let Some(defaults) = defaults {
    doc.insert(Value::String("defaults".into()), defaults);
  }
  let mut services = Mapping::new();
  services.insert(Value::String(name), Value::Mapping(service));
  doc.insert(Value::String("services".into()), Value::Mapping(services));

  // Read back the way the client reads a file, so what is written is a file
  // the client accepts.
  layers::check_document(&doc, "the written file")?;
  let mut read = doc.clone();
  if let Some(Value::Mapping(server)) = read.get_mut("server") {
    // The placeholder is expanded from the environment when the file is
    // read; here it only has to parse.
    server.remove("token");
  }
  layers::assemble(vec![(Layer::Local, read)], Vec::new())?;
  Ok(doc)
}

/// Runs `init` and returns the exit code.
pub(crate) fn run(
  cli: &crate::config::CliArgs,
  name: Option<&str>,
  force: bool,
  stdout: bool,
) -> i32 {
  let env = match layers::env_overlay(|k| std::env::var(k).ok()) {
    Ok(env) => env,
    Err(e) => {
      eprintln!("error: {e}");
      return 1;
    }
  };
  let flags = match cli.overlay() {
    Ok(flags) => flags,
    Err(e) => {
      eprintln!("error: {e}");
      return 1;
    }
  };
  let doc = match document(&[env, flags], name) {
    Ok(doc) => doc,
    Err(e) => {
      eprintln!("error: {e}");
      return 1;
    }
  };
  let text = match serde_yaml::to_string(&doc) {
    Ok(text) => text,
    Err(e) => {
      eprintln!("error: {e}");
      return 1;
    }
  };
  if stdout {
    print!("{text}");
    return 0;
  }
  let path = cli.opts.config.as_deref().unwrap_or("aperio.yaml");
  if std::path::Path::new(path).exists() && !force {
    eprintln!("error: {path} exists; pass --force to replace it, or --stdout to print the file");
    return 1;
  }
  if let Err(e) = std::fs::write(path, &text) {
    eprintln!("error: cannot write {path}: {e}");
    return 1;
  }
  println!("Wrote {path}.");
  if text.contains(TOKEN_PLACEHOLDER) {
    println!(
      "The token is not in the file: it reads {TOKEN_PLACEHOLDER}, so set APERIO_SERVER_TOKEN \
       where the client runs."
    );
  }
  println!("Run it with `aperio-client`, and check it with `aperio-client check`.");
  0
}

#[cfg(test)]
#[path = "init_tests.rs"]
mod tests;
