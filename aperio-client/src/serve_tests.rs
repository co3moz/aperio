use super::{percent_decode, resolve};

fn setup() -> std::path::PathBuf {
  let root = std::env::temp_dir().join(format!("aperio-serve-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(root.join("assets")).unwrap();
  std::fs::write(root.join("index.html"), "<h1>hi</h1>").unwrap();
  std::fs::write(root.join("assets/app.js"), "js").unwrap();
  std::fs::write(root.join("a file.txt"), "spaced").unwrap();
  std::fs::canonicalize(root).unwrap()
}

#[test]
fn resolves_files_directories_and_encoded_names() {
  let root = setup();
  assert_eq!(
    resolve(&root, "/assets/app.js"),
    Some(root.join("assets/app.js"))
  );
  // A directory resolves to its index.html.
  assert_eq!(resolve(&root, "/"), Some(root.join("index.html")));
  // Percent-encoded names decode before hitting the filesystem.
  assert_eq!(
    resolve(&root, "/a%20file.txt"),
    Some(root.join("a file.txt"))
  );
  // Missing files are None.
  assert_eq!(resolve(&root, "/nope.txt"), None);
  std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn rejects_traversal_out_of_the_root() {
  let root = setup();
  assert_eq!(resolve(&root, "/../secrets.txt"), None);
  assert_eq!(resolve(&root, "/assets/../../secrets.txt"), None);
  // Encoded traversal decodes first — still rejected.
  assert_eq!(resolve(&root, "/%2e%2e/secrets.txt"), None);
  assert_eq!(resolve(&root, "/..%2fsecrets.txt"), None);
  std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn percent_decode_handles_escapes_and_leaves_garbage() {
  assert_eq!(percent_decode("/a%20b"), "/a b");
  assert_eq!(percent_decode("/a%2Fb"), "/a/b");
  assert_eq!(percent_decode("/a%zz"), "/a%zz");
  assert_eq!(percent_decode("/plain"), "/plain");
}
