//! What this pins down: `init` writes the service the command line
//! describes, under a name, with the token left out, and what it writes is a
//! file the client itself accepts.

use super::*;
use aperio_config::layers::Scope;

fn overlay(pairs: &[(Scope, &str, &str)]) -> Overlay {
  let mut o = Overlay::default();
  for (scope, path, value) in pairs {
    o.set(*scope, path, Value::String(value.to_string()));
  }
  o
}

#[test]
fn the_command_line_service_becomes_a_named_entry() {
  let doc = document(
    &[overlay(&[
      (Scope::File, "server.url", "https://tunnel.example.com"),
      (Scope::File, "server.token", "apr_secret"),
      (Scope::Defaults, "access", "public"),
      (Scope::Service, "target", "3000"),
      (Scope::Service, "hostname", "App.example.com"),
    ])],
    None,
  )
  .unwrap();
  let text = serde_yaml::to_string(&doc).unwrap();
  // Named after the hostname, target written the way it is read.
  assert!(text.contains("app:"), "{text}");
  assert!(text.contains("target: http://localhost:3000"), "{text}");
  // What every service shares goes where the file keeps it.
  assert!(text.contains("defaults:\n  access: public"), "{text}");
  // The token is named, not written.
  assert!(!text.contains("apr_secret"), "{text}");
  assert!(text.contains("${APERIO_SERVER_TOKEN}"), "{text}");
  // The server block leads.
  assert!(text.starts_with("server:"), "{text}");
}

#[test]
fn a_later_overlay_wins_and_a_name_can_be_given() {
  let env = overlay(&[(Scope::Service, "target", "3000")]);
  let flags = overlay(&[(Scope::Service, "target", "4000")]);
  let doc = document(&[env, flags], Some("api")).unwrap();
  let text = serde_yaml::to_string(&doc).unwrap();
  assert!(
    text.contains("api:\n    target: http://localhost:4000"),
    "{text}"
  );
  assert!(
    document(
      &[overlay(&[(Scope::Service, "target", "1")])],
      Some("Bad-Name")
    )
    .is_err()
  );
}

#[test]
fn a_service_with_no_hostname_is_called_web() {
  let doc = document(&[overlay(&[(Scope::Service, "serve", "./dist")])], None).unwrap();
  assert!(
    serde_yaml::to_string(&doc)
      .unwrap()
      .contains("web:\n    serve: ./dist")
  );
}

#[test]
fn there_is_nothing_to_write_without_a_target() {
  let err = document(
    &[overlay(&[(Scope::Service, "hostname", "a.example.com")])],
    None,
  )
  .unwrap_err();
  assert!(err.contains("needs one"), "{err}");
}
