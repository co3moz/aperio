use super::*;

#[test]
fn parse_patterns_accepts_the_grammar_and_rejects_garbage() {
  let list =
    parse_patterns("hooks.example.com, *.internal.example, 10.0.0.0/8, 192.0.2.7").unwrap();
  assert_eq!(list[0], OutboundPattern::Host("hooks.example.com".into()));
  assert_eq!(list[1], OutboundPattern::Suffix("internal.example".into()));
  assert!(matches!(list[2], OutboundPattern::Cidr(_, 8)));
  assert!(matches!(list[3], OutboundPattern::Cidr(_, 32)));
  // Empty entries are skipped, an empty list parses to nothing.
  assert!(parse_patterns(" , ").unwrap().is_empty());
  // Bad shapes fail loudly instead of becoming a partial allowlist.
  assert!(parse_patterns("*.").is_err());
  assert!(parse_patterns("a.*.b").is_err());
  assert!(parse_patterns("10.0.0.0/40").is_err());
}

#[tokio::test]
async fn empty_policy_allows_everything() {
  let policy = OutboundPolicy::default();
  assert!(!policy.restricted());
  // No parsing, no resolution: even a garbage URL passes the empty policy,
  // preserving today's behaviour exactly.
  assert!(policy.check("http://127.0.0.1:9/x").await.is_ok());
  assert!(policy.check("not a url").await.is_ok());
}

#[tokio::test]
async fn allowlist_gates_by_host_suffix_and_cidr() {
  let policy = OutboundPolicy {
    allowlist: parse_patterns("hooks.example.com, *.corp.example, 10.1.0.0/16").unwrap(),
    block_private: false,
  };

  // Exact host, case-insensitive, trailing dot tolerated.
  assert!(policy.check("https://hooks.example.com/x").await.is_ok());
  assert!(policy.check("https://HOOKS.Example.COM./x").await.is_ok());
  // Wildcard covers subdomains only, not the bare suffix.
  assert!(policy.check("https://a.corp.example/x").await.is_ok());
  assert!(policy.check("https://corp.example/x").await.is_err());
  assert!(policy.check("https://evilcorp.example/x").await.is_err());
  // CIDR covers IP-literal destinations; an allowlisted private range is
  // allowed on purpose (the operator named it).
  assert!(policy.check("http://10.1.2.3:8080/x").await.is_ok());
  assert!(policy.check("http://10.2.0.1/x").await.is_err());
  // Anything else is refused.
  assert!(policy.check("https://other.example.com/x").await.is_err());
}

#[tokio::test]
async fn block_private_refuses_internal_literals_and_allows_public_ones() {
  let policy = OutboundPolicy {
    allowlist: Vec::new(),
    block_private: true,
  };
  for refused in [
    "http://127.0.0.1/x",
    "http://10.0.0.5/x",
    "http://192.168.1.1/x",
    "http://169.254.169.254/latest/meta-data",
    "http://100.100.1.1/x",
    "http://[::1]/x",
    "http://[fd00::1]/x",
  ] {
    assert!(
      policy.check(refused).await.is_err(),
      "{refused} must be refused"
    );
  }
  // A public literal passes without any DNS involvement.
  assert!(policy.check("https://193.0.2.10/x").await.is_ok());
}

#[test]
fn is_internal_judges_the_special_ranges() {
  use std::net::{Ipv4Addr, Ipv6Addr};
  assert!(is_internal(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
  assert!(is_internal(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
  assert!(is_internal(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
  assert!(!is_internal(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 1))));
  assert!(!is_internal(IpAddr::V4(Ipv4Addr::new(193, 0, 2, 10))));
  assert!(is_internal(IpAddr::V6(Ipv6Addr::LOCALHOST)));
  // An IPv4-mapped private address is judged by its IPv4 form.
  assert!(is_internal(IpAddr::V6(
    Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped()
  )));
}

#[tokio::test]
async fn a_hostname_is_resolved_for_the_cidr_entries() {
  // An all-IP allowlist still covers named destinations: the name is
  // resolved and each address is offered to the CIDR entries. `localhost`
  // is the one name every resolver answers without a network.
  let policy = OutboundPolicy {
    allowlist: parse_patterns("127.0.0.0/8, ::1/128").unwrap(),
    block_private: false,
  };
  assert!(policy.check("http://localhost:9999/hook").await.is_ok());

  // A CIDR that does not contain what the name resolves to refuses it.
  let policy = OutboundPolicy {
    allowlist: parse_patterns("192.0.2.0/24").unwrap(),
    block_private: false,
  };
  assert!(policy.check("http://localhost:9999/hook").await.is_err());
}

#[tokio::test]
async fn block_private_resolves_a_hostname_before_judging_it() {
  // The classic bypass is a public-looking name resolving to loopback or
  // the metadata address; the gate must judge the addresses, not the name.
  let policy = OutboundPolicy {
    allowlist: Vec::new(),
    block_private: true,
  };
  let err = policy
    .check("http://localhost:9999/hook")
    .await
    .unwrap_err();
  assert!(err.contains("internal address"), "{err}");

  // A name that does not resolve at all is refused with the resolver's
  // reason rather than silently allowed.
  let err = policy
    .check("http://does-not-exist.invalid/hook")
    .await
    .unwrap_err();
  assert!(err.contains("cannot resolve"), "{err}");
}

#[test]
fn a_bad_cidr_fails_the_whole_parse() {
  // An entry with a `/` is a CIDR claim and is held to the CIDR grammar; a
  // malformed one must fail the parse rather than become a partial policy.
  // (`999.0.0.1` without a slash is deliberately NOT an error: it does not
  // parse as an IP, so it is a hostname, however unlikely.)
  assert!(parse_patterns("10.0.0.0/999").is_err());
  assert!(parse_patterns("10.0.0.0/8/extra").is_err());
}

#[test]
fn every_spelling_of_a_proxy_variable_is_noticed() {
  // Both cases of all three names, because whichever is found first is the
  // one that decides, so any of them is a proxy.
  for name in [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
  ] {
    let found = proxy_vars_from(|n| (n == name).then(|| "http://proxy.corp:3128".to_string()));
    assert_eq!(found, vec![name], "{name} was not noticed");
  }
}

#[test]
fn an_empty_or_absent_proxy_variable_is_not_a_proxy() {
  assert!(proxy_vars_from(|_| None).is_empty(), "nothing set");
  // Set-but-empty is how a wrapper script clears one, and it proxies nothing.
  assert!(
    proxy_vars_from(|_| Some(String::new())).is_empty(),
    "empty value"
  );
  assert!(
    proxy_vars_from(|_| Some("   ".to_string())).is_empty(),
    "whitespace value"
  );
}

#[test]
fn no_proxy_is_not_treated_as_an_escape() {
  // Measured, not assumed: with `NO_PROXY=*` set, a request to a loopback
  // address still went to the proxy. So a `NO_PROXY` of any shape leaves the
  // conflict standing, and the policy must still refuse to start.
  let found = proxy_vars_from(|n| match n {
    "HTTPS_PROXY" => Some("http://proxy.corp:3128".to_string()),
    "NO_PROXY" => Some("*".to_string()),
    _ => None,
  });
  assert_eq!(found, vec!["HTTPS_PROXY"]);
}

#[tokio::test]
async fn a_url_without_a_host_is_refused_by_a_restricted_policy() {
  let policy = OutboundPolicy {
    allowlist: Vec::new(),
    block_private: true,
  };
  let err = policy.check("not a url").await.unwrap_err();
  assert!(err.contains("invalid url"), "{err}");
  let err = policy.check("unix:/var/run/x.sock").await.unwrap_err();
  assert!(err.contains("no host"), "{err}");
}
