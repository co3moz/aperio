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
