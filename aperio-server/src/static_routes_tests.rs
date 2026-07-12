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
