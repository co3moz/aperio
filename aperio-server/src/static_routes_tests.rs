use super::{RouteRule, StaticRoutes};

fn compile(yaml: &str) -> StaticRoutes {
  let rules: Vec<RouteRule> = serde_yaml::from_str(yaml).unwrap();
  StaticRoutes::compile(rules).unwrap()
}

#[test]
fn redirect_route_matches_hostname_and_preserves_path() {
  let routes = compile(
    r#"
- hostname: old.example.com
  redirect: https://new.example.com
  preserve_path: true
- hostname: gone.example.com
  redirect: https://example.com
  permanent: true
"#,
  );

  let res = routes
    .answer(Some("old.example.com"), "/docs/intro", Some("a=1"))
    .unwrap();
  assert_eq!(res.status(), 302);
  assert_eq!(
    res.headers().get("location").unwrap(),
    "https://new.example.com/docs/intro?a=1"
  );

  let res = routes.answer(Some("gone.example.com"), "/", None).unwrap();
  assert_eq!(res.status(), 301);
  assert_eq!(
    res.headers().get("location").unwrap(),
    "https://example.com"
  );

  assert!(
    routes
      .answer(Some("other.example.com"), "/", None)
      .is_none()
  );
  assert!(routes.answer(None, "/", None).is_none());
}

#[test]
fn respond_route_serves_fixed_content_on_a_path_bind() {
  let routes = compile(
    r#"
- hostname: soon.example.com
  respond:
    status: 503
    content_type: text/html
    body: "<h1>Coming soon</h1>"
- path: /robots.txt
  respond:
    content_type: text/plain
    body: "User-agent: *\nDisallow: /\n"
"#,
  );

  let res = routes
    .answer(Some("soon.example.com"), "/any", None)
    .unwrap();
  assert_eq!(res.status(), 503);

  // The path-only rule matches any hostname, but only the exact bind.
  let res = routes
    .answer(Some("x.example.com"), "/robots.txt", None)
    .unwrap();
  assert_eq!(res.status(), 200);
  assert_eq!(res.headers().get("content-type").unwrap(), "text/plain");
  assert!(
    routes
      .answer(Some("x.example.com"), "/robots", None)
      .is_none()
  );
}

#[test]
fn compile_rejects_actionless_and_double_action_rules() {
  let none: Vec<RouteRule> = serde_yaml::from_str("- hostname: a.example.com\n").unwrap();
  assert!(StaticRoutes::compile(none).is_err());

  let both: Vec<RouteRule> = serde_yaml::from_str(
    "- hostname: a.example.com\n  redirect: https://x\n  respond: {body: hi}\n",
  )
  .unwrap();
  assert!(StaticRoutes::compile(both).is_err());
}

#[test]
fn first_matching_rule_wins() {
  let routes = compile(
    r#"
- hostname: a.example.com
  path: /special
  respond: {status: 418, body: teapot}
- hostname: a.example.com
  redirect: https://fallback.example.com
"#,
  );
  assert_eq!(
    routes
      .answer(Some("a.example.com"), "/special", None)
      .unwrap()
      .status(),
    418
  );
  assert_eq!(
    routes
      .answer(Some("a.example.com"), "/other", None)
      .unwrap()
      .status(),
    302
  );
}

// --- policy rules (planned_features #26) ------------------------------------

fn compile_err(yaml: &str) -> String {
  let rules: Vec<RouteRule> = serde_yaml::from_str(yaml).unwrap();
  match StaticRoutes::compile(rules) {
    Ok(_) => panic!("expected the section to be refused"),
    Err(e) => e,
  }
}

#[test]
fn a_policy_route_never_answers_a_request() {
  // The policy entry matches the same path as the redirect below it. It must
  // not swallow the request: an entry with no action is not an answer, and
  // treating it as one would serve an empty 200 where a redirect was meant.
  let routes = compile(
    r#"
- path: /api
  timeout: 90
- path: /api
  redirect: https://example.com/api
"#,
  );
  let answer = routes
    .answer(Some("app.example.com"), "/api/v1", None)
    .expect("the redirect still answers");
  assert_eq!(answer.status(), 302);
  assert_eq!(
    routes
      .policy_for(Some("app.example.com"), "/api/v1")
      .and_then(|r| r.timeout),
    Some(90)
  );
}

#[test]
fn policy_lookup_is_first_match_and_path_scoped() {
  let routes = compile(
    r#"
- path: /uploads
  timeout: 600
- timeout: 30
"#,
  );
  assert_eq!(
    routes
      .policy_for(None, "/uploads/big")
      .and_then(|r| r.timeout),
    Some(600)
  );
  // Anything the narrow rule does not cover falls through to the catch-all.
  assert_eq!(
    routes.policy_for(None, "/other").and_then(|r| r.timeout),
    Some(30)
  );
}

#[test]
fn route_headers_compile_into_transforms() {
  let routes = compile(
    r#"
- path: /static
  headers:
    response:
      add:
        cache-control: "public, max-age=3600"
      remove: [x-powered-by]
"#,
  );
  let rule = routes.policy_for(None, "/static/app.js").unwrap();
  let out = rule.header_transforms.response.apply(vec![
    ("x-powered-by".to_string(), "php".to_string()),
    ("content-type".to_string(), "text/javascript".to_string()),
  ]);
  assert!(
    out
      .iter()
      .any(|(k, v)| k == "cache-control" && v == "public, max-age=3600")
  );
  assert!(!out.iter().any(|(k, _)| k == "x-powered-by"));
  assert!(out.iter().any(|(k, _)| k == "content-type"));
}

#[test]
fn rate_limit_methods_are_uppercased_and_keys_are_unique() {
  let routes = compile(
    r#"
- path: /a
  rate_limit:
    rps: 5
    methods: [post, put]
- path: /b
  rate_limit:
    rps: 5
"#,
  );
  let a = routes.policy_for(None, "/a").unwrap();
  assert_eq!(
    a.rate_limit.as_ref().unwrap().methods.as_deref(),
    Some(&["POST".to_string(), "PUT".to_string()][..])
  );
  let b = routes.policy_for(None, "/b").unwrap();
  assert_ne!(a.rate_key, b.rate_key, "two routes must not share a bucket");
}

#[test]
fn mixing_an_action_with_policy_is_refused() {
  // A static answer never reaches a backend, so a backend timeout on it is a
  // configuration that cannot mean anything; saying so beats ignoring it.
  let err = compile_err(
    r#"
- path: /x
  redirect: https://example.com
  timeout: 30
"#,
  );
  assert!(
    err.contains("cannot sit on a route that answers"),
    "got {err}"
  );
}

#[test]
fn an_entry_with_neither_action_nor_policy_is_refused() {
  let err = compile_err("- path: /x\n");
  assert!(err.contains("needs `redirect` or `respond`"), "got {err}");
}

#[test]
fn invalid_policy_values_are_refused() {
  assert!(compile_err("- path: /x\n  timeout: 0\n").contains("at least 1 second"));
  assert!(compile_err("- path: /x\n  rate_limit:\n    rps: 0\n").contains("must be positive"));
  assert!(
    compile_err("- path: /x\n  rate_limit:\n    rps: 5\n    methods: []\n")
      .contains("would match nothing")
  );
}

// ---------------------------------------------------------------------------
// Canary splits (planned_features #51)
// ---------------------------------------------------------------------------

use super::{CanaryRule, Side, bucket_of};

fn canary(weight: u8, header: Option<&str>, value: Option<&str>) -> CanaryRule {
  CanaryRule {
    service: "web-v2".to_string(),
    weight,
    header: header.map(str::to_string),
    value: value.map(str::to_string),
  }
}

const A: fn(&str) -> std::net::IpAddr = |s| s.parse().unwrap();

#[test]
fn a_weight_of_zero_sends_nobody_and_a_hundred_sends_everybody() {
  let none = canary(0, None, None);
  let all = canary(100, None, None);
  for ip in ["203.0.113.7", "198.51.100.4", "10.0.0.1"] {
    assert_eq!(none.side_for(None, Some(A(ip))), Side::Stable);
    assert_eq!(all.side_for(None, Some(A(ip))), Side::Canary);
  }
}

#[test]
fn the_same_visitor_always_lands_on_the_same_side() {
  let rule = canary(20, None, None);
  let ip = A("203.0.113.7");
  let first = rule.side_for(None, Some(ip));
  // The whole point: a per-request decision would send one page load's twenty
  // assets to both versions, which is a mixture rather than a canary.
  for _ in 0..100 {
    assert_eq!(rule.side_for(None, Some(ip)), first);
  }
}

#[test]
fn the_split_is_roughly_the_weight_over_many_visitors() {
  let rule = canary(20, None, None);
  let canaried = (0u32..2000)
    .filter(|i| {
      let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::from(i.wrapping_mul(2_654_435_761)));
      rule.side_for(None, Some(ip)) == Side::Canary
    })
    .count();
  // Not exact, and the doc says so: the split is only as even as the
  // addresses are spread. A band rather than a number is the honest assertion.
  assert!(
    (200..600).contains(&canaried),
    "{canaried} of 2000 landed on the canary"
  );
}

#[test]
fn the_header_wins_over_the_weight() {
  let rule = canary(0, Some("x-canary"), None);
  let ip = A("203.0.113.7");
  // Nobody is sent by weight, and this visitor asked.
  assert_eq!(rule.side_for(Some("1"), Some(ip)), Side::Canary);
  assert_eq!(rule.side_for(None, Some(ip)), Side::Stable);
  // An empty value is not asking.
  assert_eq!(rule.side_for(Some("  "), Some(ip)), Side::Stable);
}

#[test]
fn a_required_value_must_match() {
  let rule = canary(0, Some("x-canary"), Some("on"));
  let ip = A("203.0.113.7");
  assert_eq!(rule.side_for(Some("on"), Some(ip)), Side::Canary);
  assert_eq!(rule.side_for(Some("ON"), Some(ip)), Side::Canary);
  assert_eq!(rule.side_for(Some("off"), Some(ip)), Side::Stable);
}

#[test]
fn a_visitor_with_no_address_gets_the_stable_side() {
  // An inconsistent canary is worse than a small one, and there is nothing to
  // be consistent about here.
  assert_eq!(canary(50, None, None).side_for(None, None), Side::Stable);
}

#[test]
fn the_bucket_does_not_move_between_processes() {
  // Not `DefaultHasher`, which is randomly seeded per process: two servers
  // behind a load balancer would otherwise disagree about who is in the
  // canary, and the same visitor would move on every restart.
  assert_eq!(bucket_of(A("203.0.113.7")), bucket_of(A("203.0.113.7")));
  assert_ne!(bucket_of(A("203.0.113.7")), bucket_of(A("203.0.113.8")));
  assert!(bucket_of(A("2001:db8::1")) < 100);
}

#[test]
fn a_canary_alone_is_a_policy_rule_not_an_answer() {
  let routes =
    compile("- hostname: app.example.com\n  canary:\n    service: web-v2\n    weight: 20\n");
  assert!(routes.answer(Some("app.example.com"), "/", None).is_none());
  assert!(routes.policy_for(Some("app.example.com"), "/").is_some());
}

#[test]
fn a_canary_on_an_answering_route_is_refused_and_named() {
  let rules: Vec<RouteRule> = serde_yaml::from_str(
    "- hostname: a.example.com\n  redirect: https://b.example.com\n  canary:\n    service: web-v2\n",
  )
  .unwrap();
  let err = StaticRoutes::compile(rules).err().expect("refused");
  // The message has to name the field the operator actually wrote, or it
  // sends them looking at three settings they did not use.
  assert!(err.contains("canary"), "{err}");
}

#[test]
fn a_weight_past_a_hundred_is_refused_rather_than_read_as_everybody() {
  let rules: Vec<RouteRule> = serde_yaml::from_str(
    "- hostname: a.example.com\n  canary:\n    service: web-v2\n    weight: 200\n",
  )
  .unwrap();
  let err = StaticRoutes::compile(rules).err().expect("refused");
  assert!(err.contains("percentage"), "{err}");
}

#[test]
fn an_empty_canary_service_is_refused_rather_than_doing_nothing() {
  let rules: Vec<RouteRule> = serde_yaml::from_str(
    "- hostname: a.example.com\n  canary:\n    service: \"\"\n    weight: 20\n",
  )
  .unwrap();
  assert!(StaticRoutes::compile(rules).err().is_some());
}
