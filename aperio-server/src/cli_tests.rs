//! What this pins down: the command line parses the commands an operator
//! types, and resetting a path puts exactly that setting back while leaving
//! the rest, including an optional setting that is cleared rather than
//! defaulted.

use super::*;
use clap::Parser;

#[test]
fn no_command_is_run_and_the_commands_parse() {
  assert!(
    Cli::try_parse_from(["aperio-server"])
      .unwrap()
      .command
      .is_none()
  );
  for args in [
    vec!["aperio-server", "run"],
    vec!["aperio-server", "settings", "show", "--full"],
    vec!["aperio-server", "settings", "reset", "proxy.trusted"],
    vec!["aperio-server", "apply", "server.yaml"],
    vec!["aperio-server", "check", "server.yaml"],
    vec!["aperio-server", "schema"],
    vec!["aperio-server", "audit", "verify"],
    vec!["aperio-server", "backup", "decrypt", "snap.db.enc"],
  ] {
    assert!(Cli::try_parse_from(&args).is_ok(), "{args:?}");
  }
  // The flags this replaced are refused rather than ignored: a server that
  // started when asked to check would be the worst answer.
  assert!(Cli::try_parse_from(["aperio-server", "--check-config"]).is_err());
}

#[test]
fn reset_puts_back_one_setting_and_leaves_the_rest() {
  let mut s = ServerSettings::default();
  s.proxy.trusted = vec!["10.0.0.0/8".into()];
  s.proxy.trust = true;
  s.cache.enabled = true;
  let r = reset_path(&s, "proxy.trusted").unwrap();
  assert!(r.proxy.trusted.is_empty());
  assert!(r.proxy.trust, "a sibling keeps its value");
  assert!(r.cache.enabled, "another section keeps its value");

  let r = reset_path(&s, "proxy").unwrap();
  assert_eq!(r.proxy, ServerSettings::default().proxy);
  assert!(r.cache.enabled);
}

#[test]
fn reset_clears_an_optional_setting_and_refuses_an_unknown_path() {
  let mut s = ServerSettings::default();
  s.routing.random_subdomain = Some("example.com".into());
  let r = reset_path(&s, "routing.random_subdomain").unwrap();
  assert_eq!(r.routing.random_subdomain, None);
  assert!(reset_path(&s, "routing.no_such_key").is_err());
  assert!(reset_path(&s, "nonsense").is_err());
}
