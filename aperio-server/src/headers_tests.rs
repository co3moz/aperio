use super::{HeaderRules, HeaderTransform, HeaderTransforms};

fn headers(list: &[(&str, &str)]) -> Vec<(String, String)> {
  list
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

#[test]
fn parses_the_yaml_section_and_applies_both_directions() {
  let rules: HeaderRules = serde_yaml::from_str(
    r#"
request:
  add:
    X-Proxied-By: aperio
  remove: [X-Internal]
response:
  add:
    Strict-Transport-Security: max-age=63072000
  remove: [Server, X-Powered-By]
"#,
  )
  .unwrap();
  let t = HeaderTransforms::compile(&rules);

  let req = t
    .request
    .apply(headers(&[("host", "a.example.com"), ("X-Internal", "1")]));
  assert_eq!(
    req,
    headers(&[("host", "a.example.com"), ("X-Proxied-By", "aperio")])
  );

  let res = t.response.apply(headers(&[
    ("content-type", "text/html"),
    ("server", "nginx"),
    ("x-powered-by", "php"),
  ]));
  assert_eq!(
    res,
    headers(&[
      ("content-type", "text/html"),
      ("Strict-Transport-Security", "max-age=63072000"),
    ])
  );
}

#[test]
fn add_replaces_existing_values_case_insensitively() {
  let rules: HeaderRules =
    serde_yaml::from_str("response:\n  add:\n    Cache-Control: no-store\n").unwrap();
  let t = HeaderTransforms::compile(&rules);
  let res = t
    .response
    .apply(headers(&[("cache-control", "max-age=60"), ("etag", "x")]));
  assert_eq!(
    res,
    headers(&[("etag", "x"), ("Cache-Control", "no-store")])
  );
}

#[test]
fn empty_transform_is_a_no_op() {
  let t = HeaderTransform::default();
  assert!(t.is_empty());
  let original = headers(&[("a", "1")]);
  assert_eq!(t.apply(original.clone()), original);
}
