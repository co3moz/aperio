//! Names as addresses: what a tunnel is called when nothing named it, that a
//! derived name is stable and stays addressable, and the shapes refused so a
//! name can never be mistaken for a client id or for address syntax.

use super::*;

// ---------------------------------------------------------------------------
// Tunnel names: the handle a binder and an `expose:` entry address.
// ---------------------------------------------------------------------------

fn decl(name: Option<&str>, target: &str, protocol: &str) -> TunnelDecl {
  TunnelDecl {
    custom_name: None,
    name: name.map(str::to_string),
    target: target.to_string(),
    protocol: protocol.to_string(),
    encrypt: false,
    psk: None,
    proxy_protocol: false,
    idle_timeout: None,
    expose: None,
  }
}

#[test]
fn a_declared_name_is_used_verbatim() {
  assert_eq!(
    tunnel_name(&decl(Some("pg_main"), "127.0.0.1:5432", "tcp")),
    "pg_main"
  );
  assert_eq!(
    tunnel_name(&decl(Some("  spaced  "), "127.0.0.1:5432", "tcp")),
    "spaced"
  );
}

#[test]
fn an_undeclared_name_is_derived_from_the_target() {
  // Unnamed tunnels still need a stable handle, or the whole addressing
  // scheme would only work for files that opted in.
  assert_eq!(
    tunnel_name(&decl(None, "192.168.3.100:53", "udp")),
    "192_168_3_100_53_udp"
  );
  // Protocol is part of it: the same address over tcp and udp are two
  // tunnels, and the client refuses a file where two resolve to one name.
  assert_ne!(
    tunnel_name(&decl(None, "192.168.3.100:53", "udp")),
    tunnel_name(&decl(None, "192.168.3.100:53", "tcp"))
  );
}

#[test]
fn a_derived_name_is_stable() {
  let a = tunnel_name(&decl(None, "127.0.0.1:5432", "tcp"));
  let b = tunnel_name(&decl(None, "127.0.0.1:5432", "tcp"));
  assert_eq!(a, b, "the handle must survive a restart");
}

#[test]
fn a_name_shaped_like_a_client_id_is_refused() {
  // `bind-tunnels:` keys are read as a name and fall back to a client id, so
  // the two shapes have to stay disjoint for that fallback to be unambiguous.
  assert!(looks_like_client_id("3beebfdb-079f-4a00-9e03-1bb6eb9222b4"));
  assert!(validate_tunnel_name("3beebfdb-079f-4a00-9e03-1bb6eb9222b4").is_err());
  assert!(!looks_like_client_id("pg_main"));
  assert!(validate_tunnel_name("pg_main").is_ok());
}

#[test]
fn a_name_is_limited_to_addressable_characters() {
  assert!(validate_tunnel_name("db_primary_1a").is_ok());
  assert!(validate_tunnel_name("").is_err());
  assert!(validate_tunnel_name("has space").is_err());
  assert!(validate_tunnel_name("has/slash").is_err());
  // The three that used to pass, and are the whole point of the rule: a name
  // is an identifier, so there is exactly one way to write each one.
  assert!(
    validate_tunnel_name("PgMain").is_err(),
    "case is not a variant"
  );
  assert!(validate_tunnel_name("pg-main").is_err(), "`-` is reserved");
  assert!(
    validate_tunnel_name("db.primary").is_err(),
    "`.` is reserved"
  );
  // Not English is not an identifier: `ı` and `i` are one keystroke apart and
  // a different character, which is a bug waiting in a config file.
  assert!(validate_tunnel_name("kayıt").is_err());
  // The message carries the fix, since almost every rejection is mechanical.
  let why = validate_tunnel_name("PG-Main").unwrap_err();
  assert!(why.contains("pg_main"), "{why}");
}

#[test]
fn a_slug_is_a_name_whatever_it_started_as() {
  assert_eq!(slug("PG-Main"), "pg_main");
  assert_eq!(slug("  Acme Inc.  "), "acme_inc");
  assert_eq!(slug("Ödeme Servisi"), "odeme_servisi");
  assert_eq!(slug("Müşteri Portalı"), "musteri_portali");
  assert_eq!(slug("Größe"), "grosse");
  // A script this cannot read becomes separators rather than a guess.
  assert_eq!(slug("数据库"), "unnamed");
  // Never empty: something has to be addressable even when nothing survives.
  assert_eq!(slug("!!!"), "unnamed");
  for raw in ["PG-Main", "Acme Inc.", "!!!", "çğüş", "数据库", "Ödeme"] {
    assert!(validate_name("test", &slug(raw)).is_ok(), "{raw}");
  }
}

#[test]
fn a_bind_entry_accepts_the_short_and_long_forms() {
  // `pg_main: 15432` is the whole entry for most bindings.
  let short: BindTunnelValue = serde_yaml::from_str("15432").unwrap();
  assert_eq!(short.entry().port, Some(15432));

  let long: BindTunnelValue = serde_yaml::from_str("port: 15432\naddress: 0.0.0.0\n").unwrap();
  let entry = long.entry();
  assert_eq!(entry.port, Some(15432));
  assert_eq!(entry.address.as_deref(), Some("0.0.0.0"));
}

// ---------------------------------------------------------------------------
// The combined `tcp/udp` declaration.
// ---------------------------------------------------------------------------

#[test]
fn a_combined_declaration_serves_both_transports() {
  // DNS is the reason this exists: port 53 is genuinely both, and writing it
  // as two declarations meant two names and two entries in every binder.
  assert!(protocol_serves(PROTOCOL_BOTH, "tcp"));
  assert!(protocol_serves(PROTOCOL_BOTH, "udp"));
  assert!(protocol_serves("tcp", "tcp"));
  assert!(!protocol_serves("tcp", "udp"));
  assert!(protocol_serves("udp", "udp"));
  assert!(!protocol_serves("udp", "tcp"));
  // Spacing and case in a hand-written file must not change the answer.
  assert!(protocol_serves("  TCP/UDP ", "udp"));
}

#[test]
fn a_combined_derived_name_stays_addressable() {
  // The protocol goes into a derived name, and a slash is not a character a
  // name may contain, so it has to be folded rather than passed through.
  let name = tunnel_name(&decl(None, "192.168.3.100:53", PROTOCOL_BOTH));
  assert_eq!(name, "192_168_3_100_53_tcp_udp");
  assert!(validate_tunnel_name(&name).is_ok());
}
