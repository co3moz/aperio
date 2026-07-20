use super::{HeaderRules, HeaderTransform, HeaderTransforms, from_config_file};

/// Serializes tests that touch the process-global config document / default
/// `aperio-server.yaml`. Loads `yaml` as the default document, runs `f`.
fn with_config(yaml: &str, f: impl FnOnce()) {
  let lock = std::env::temp_dir().join("aperio-cfgfile-test.lock");
  let start = std::time::Instant::now();
  loop {
    match std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&lock)
    {
      Ok(_) => break,
      Err(_) => {
        if let Ok(md) = std::fs::metadata(&lock)
          && md
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|e| e.as_secs() > 30)
        {
          let _ = std::fs::remove_file(&lock);
        }
        assert!(
          start.elapsed().as_secs() < 120,
          "config-file test lock timeout"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
      }
    }
  }
  struct Cleanup(std::path::PathBuf);
  impl Drop for Cleanup {
    fn drop(&mut self) {
      let _ = std::fs::remove_file("aperio-server.yaml");
      let _ = std::fs::remove_file(&self.0);
    }
  }
  let _cleanup = Cleanup(lock);
  std::fs::write("aperio-server.yaml", yaml).unwrap();
  crate::config_file::reload().unwrap();
  f();
}

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

#[test]
fn from_config_file_absent_section_is_default() {
  with_config("other: 1\n", || {
    // No `headers:` key → both directions are empty no-ops.
    let t = from_config_file();
    assert!(t.request.is_empty());
    assert!(t.response.is_empty());
  });
}

#[test]
fn from_config_file_parses_the_section() {
  with_config(
    "headers:\n  response:\n    add:\n      X-Frame-Options: DENY\n    remove: [Server]\n",
    || {
      let t = from_config_file();
      assert!(t.request.is_empty());
      let out = t
        .response
        .apply(vec![("server".to_string(), "nginx".to_string())]);
      assert_eq!(
        out,
        vec![("X-Frame-Options".to_string(), "DENY".to_string())]
      );
    },
  );
}
