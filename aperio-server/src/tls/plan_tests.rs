//! What a `tls` section may say: the combinations that cannot work are
//! refused with the path that is wrong, and a section that can work becomes
//! the plan the listener starts from.

use super::*;
use aperio_config::server_settings::Acme;

fn on() -> Tls {
  Tls {
    enabled: true,
    acme: Acme {
      enabled: true,
      ..Acme::default()
    },
    ..Tls::default()
  }
}

#[test]
fn https_off_is_no_plan_and_asks_for_nothing() {
  assert_eq!(plan(&Tls::default()), Ok(None));
}

#[test]
fn what_needs_the_listener_is_refused_without_it() {
  let mut tls = Tls::default();
  tls.acme.enabled = true;
  assert!(plan(&tls).unwrap_err().starts_with("tls.acme.enabled"));
  let tls = Tls {
    redirect_http: true,
    ..Tls::default()
  };
  assert!(plan(&tls).unwrap_err().starts_with("tls.redirect_http"));
}

#[test]
fn a_listener_with_nothing_to_serve_is_refused() {
  let tls = Tls {
    enabled: true,
    ..Tls::default()
  };
  assert!(
    plan(&tls)
      .unwrap_err()
      .starts_with("tls.enabled: there is nothing to serve")
  );
}

#[test]
fn certificate_files_alone_are_enough() {
  let tls = Tls {
    enabled: true,
    certificates: vec![CertificateFiles {
      cert: "/etc/tls/a.pem".into(),
      key: "/etc/tls/a.key".into(),
    }],
    ..Tls::default()
  };
  let plan = plan(&tls).unwrap().unwrap();
  assert_eq!(plan.port, 443);
  assert!(plan.acme.is_none());
  let tls = Tls {
    certificates: vec![CertificateFiles {
      cert: "/etc/tls/a.pem".into(),
      key: " ".into(),
    }],
    ..tls
  };
  assert!(
    super::plan(&tls)
      .unwrap_err()
      .starts_with("tls.certificates[0]")
  );
}

#[test]
fn the_directory_is_an_https_url() {
  let mut tls = on();
  tls.acme.directory = "http://acme.example.com/directory".into();
  assert!(plan(&tls).unwrap_err().contains("is not https://"));
  tls.acme.directory = "not a url".into();
  assert!(plan(&tls).unwrap_err().starts_with("tls.acme.directory"));
}

#[test]
fn an_email_has_to_look_like_one() {
  let mut tls = on();
  tls.acme.email = Some("ops".into());
  assert!(plan(&tls).unwrap_err().starts_with("tls.acme.email"));
  tls.acme.email = Some(" ops@example.com ".into());
  let acme = plan(&tls).unwrap().unwrap().acme.unwrap();
  assert_eq!(acme.email.as_deref(), Some("ops@example.com"));
  tls.acme.email = Some(String::new());
  assert_eq!(plan(&tls).unwrap().unwrap().acme.unwrap().email, None);
}

#[test]
fn hostnames_are_normalized_and_only_names_an_authority_certifies() {
  let mut tls = on();
  tls.acme.hostnames = vec![
    "Tunnel.Example.com.".into(),
    "tunnel.example.com".into(),
    "api.example.com".into(),
  ];
  let acme = plan(&tls).unwrap().unwrap().acme.unwrap();
  assert_eq!(acme.hostnames, ["tunnel.example.com", "api.example.com"]);

  for (raw, why) in [
    ("*.example.com", "wildcard"),
    ("203.0.113.7", "address"),
    ("localhost", "single label"),
    ("bad_host!.example.com", "not a hostname"),
  ] {
    tls.acme.hostnames = vec![raw.into()];
    let err = plan(&tls).unwrap_err();
    assert!(
      err.starts_with("tls.acme.hostnames") && err.contains(why),
      "{raw}: {err}"
    );
  }
}
