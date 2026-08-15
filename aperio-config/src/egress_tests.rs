//! What these pin down: the spellings an operator may write for a proxy, that
//! a credential never appears in anything printable, and which destinations
//! the bypass list keeps off the proxy.

use super::*;

#[test]
fn the_spellings_an_operator_may_write_all_parse() {
  for (raw, host, port) in [
    ("http://proxy.corp:3128", "proxy.corp", 3128),
    ("proxy.corp:3128", "proxy.corp", 3128),
    ("HTTP://Proxy.Corp:3128", "Proxy.Corp", 3128),
    ("http://proxy.corp:3128/", "proxy.corp", 3128),
    ("  proxy.corp:3128  ", "proxy.corp", 3128),
    ("10.0.0.9:8080", "10.0.0.9", 8080),
    ("[2001:db8::1]:3128", "2001:db8::1", 3128),
    ("http://proxy.corp", "proxy.corp", 80),
  ] {
    let proxy = EgressProxy::parse(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
    assert_eq!((proxy.host(), proxy.port()), (host, port), "{raw}");
    assert!(!proxy.has_credentials(), "{raw} carries no credential");
  }
}

#[test]
fn a_credential_is_kept_but_never_printed() {
  let proxy = EgressProxy::parse("http://alice:s3cret@proxy.corp:3128").unwrap();
  assert_eq!(proxy.credentials(), Some(("alice", "s3cret")));
  assert_eq!(proxy.redacted(), "proxy.corp:3128");
  let debugged = format!("{proxy:?}");
  assert!(!debugged.contains("s3cret"), "Debug leaked it: {debugged}");
  assert!(!debugged.contains("alice"), "Debug leaked it: {debugged}");
}

#[test]
fn a_password_may_contain_the_separators() {
  // Last `@` and first `:`, or `p@ss` takes the host with it and `a:b` splits
  // in the wrong place.
  let proxy = EgressProxy::parse("http://alice:p@ss:word@proxy.corp:3128").unwrap();
  assert_eq!(proxy.redacted(), "proxy.corp:3128");
  assert_eq!(proxy.credentials(), Some(("alice", "p@ss:word")));
}

#[test]
fn the_values_that_cannot_work_are_refused_with_the_reason() {
  for (raw, needle) in [
    ("", "empty"),
    ("https://proxy.corp:3128", "https://"),
    ("socks5://proxy.corp:1080", "not supported"),
    ("http://proxy.corp:3128/some/path", "has a path"),
    ("http://proxy.corp:notaport", "no usable port"),
    ("[2001:db8::1", "never closes"),
  ] {
    let err = EgressProxy::parse(raw).unwrap_err();
    assert!(err.contains(needle), "{raw} -> {err}");
  }
}

#[test]
fn the_url_brackets_an_ipv6_literal() {
  // Unbracketed, `http://2001:db8::1:3128` is not a URL at all: it fails with
  // InvalidPort. Both callers formatted this by hand and both got it wrong,
  // and on the server the parse failure read as "do not proxy this request",
  // so a configured proxy was skipped in silence.
  let v6 = EgressProxy::parse("[2001:db8::1]:3128").unwrap();
  assert_eq!(v6.url(), "http://[2001:db8::1]:3128");

  let named = EgressProxy::parse("proxy.corp:3128").unwrap();
  assert_eq!(named.url(), "http://proxy.corp:3128");
  let v4 = EgressProxy::parse("10.0.0.9:8080").unwrap();
  assert_eq!(v4.url(), "http://10.0.0.9:8080");

  // The credential stays out of the URL, so it cannot be logged as part of a
  // destination; it travels in a header the caller adds.
  let with = EgressProxy::parse("alice:s3cret@proxy.corp:3128").unwrap();
  assert_eq!(with.url(), "http://proxy.corp:3128");
}

#[test]
fn a_port_with_no_host_is_refused() {
  // It used to fall through to the no-port branch and become a *hostname* of
  // ":3128" on port 80, which parses, starts, and fails somewhere else.
  let err = EgressProxy::parse(":3128").unwrap_err();
  assert!(err.contains("names a port but no host"), "{err}");
  let err = EgressProxy::parse("http://:3128").unwrap_err();
  assert!(err.contains("names a port but no host"), "{err}");
}

#[test]
fn an_at_sign_with_no_credential_is_refused() {
  // Left as a credential it is worse than ignored: `has_credentials` says
  // yes, the startup line reports the proxy as authenticated, and an empty
  // `Basic` goes on the wire, so a 407 is reported as a rejected password
  // that was never written.
  let err = EgressProxy::parse("@proxy.corp:3128").unwrap_err();
  assert!(err.contains("no credential in front of it"), "{err}");

  // A password-only credential is still a credential, and is not refused.
  let only_password = EgressProxy::parse(":s3cret@proxy.corp:3128").unwrap();
  assert_eq!(only_password.credentials(), Some(("", "s3cret")));
}

#[test]
fn a_credential_is_hidden_even_when_the_value_fails_to_parse() {
  let err = EgressProxy::parse("http://alice:s3cret@proxy.corp:3128/path").unwrap_err();
  assert!(!err.contains("s3cret"), "{err}");
  assert!(err.contains("***@"), "{err}");
}

#[test]
fn the_bypass_list_covers_a_name_and_what_is_under_it() {
  let bypass = EgressBypass::parse("auth.internal, .corp.example, *.svc.cluster.local");
  assert_eq!(bypass.len(), 3);

  assert!(bypass.covers("auth.internal"));
  assert!(bypass.covers("AUTH.INTERNAL"), "matched case-insensitively");
  assert!(
    bypass.covers("auth.internal."),
    "a trailing dot is the same name"
  );
  assert!(!bypass.covers("other.internal"));
  assert!(!bypass.covers("xauth.internal"), "not a suffix match");

  // Both spellings of a domain cover the domain itself and anything under it.
  assert!(bypass.covers("corp.example"));
  assert!(bypass.covers("hooks.corp.example"));
  assert!(bypass.covers("a.b.corp.example"));
  assert!(!bypass.covers("notcorp.example"));

  assert!(bypass.covers("api.svc.cluster.local"));
  assert!(bypass.covers("svc.cluster.local"));
}

#[test]
fn loopback_is_always_direct_whether_listed_or_not() {
  // Asking a proxy elsewhere to fetch an address on this machine cannot work,
  // so the only correct value for this rule is the one we can choose.
  let empty = EgressBypass::parse("");
  assert!(empty.is_empty());
  for host in ["localhost", "127.0.0.1", "::1", "[::1]", "LOCALHOST"] {
    assert!(empty.covers(host), "{host} must be direct");
  }
  assert!(!empty.covers("example.com"));
}

#[test]
fn an_empty_entry_is_skipped_rather_than_refused() {
  let bypass = EgressBypass::parse(" , auth.internal ,, ");
  assert_eq!(bypass.len(), 1);
  assert!(bypass.covers("auth.internal"));
}
