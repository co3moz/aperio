use super::{ServeOptions, options_from_env, percent_decode, resolve, start};

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

#[test]
fn resolve_directory_without_index_is_none() {
  let root = setup();
  // `assets/` exists but holds no index.html, so a directory request yields
  // nothing servable.
  assert_eq!(resolve(&root, "/assets"), None);
  // A path segment carrying a `:` (drive-letter / scheme smell) is rejected.
  assert_eq!(resolve(&root, "/c:/win.ini"), None);
  std::fs::remove_dir_all(&root).unwrap();
}

// --- Live-server integration tests -----------------------------------------

/// Spins up the loopback server against a fresh fixture dir and returns the
/// bound base URL plus the root path (kept so the caller can delete it).
async fn spawn(opts: ServeOptions) -> (String, std::path::PathBuf) {
  let root = setup();
  let (port, _handle) = start(root.to_str().unwrap(), opts).await.unwrap();
  (format!("http://127.0.0.1:{port}"), root)
}

#[tokio::test]
async fn serves_files_with_mime_and_handles_head() {
  let (base, root) = spawn(ServeOptions::default()).await;
  let client = reqwest::Client::new();

  // A known asset is served 200 with a JS content-type.
  let resp = client
    .get(format!("{base}/assets/app.js"))
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 200);
  let ctype = resp.headers()["content-type"].to_str().unwrap().to_string();
  assert!(ctype.contains("javascript"), "unexpected type {ctype}");
  assert_eq!(resp.text().await.unwrap(), "js");

  // The root resolves to index.html (html mime).
  let resp = client.get(format!("{base}/")).send().await.unwrap();
  assert_eq!(resp.status(), 200);
  assert!(
    resp.headers()["content-type"]
      .to_str()
      .unwrap()
      .contains("html")
  );
  assert_eq!(resp.text().await.unwrap(), "<h1>hi</h1>");

  // An extension-less / unknown file falls back to octet-stream.
  std::fs::write(root.join("blob.unknownext"), "raw").unwrap();
  let resp = client
    .get(format!("{base}/blob.unknownext"))
    .send()
    .await
    .unwrap();
  assert_eq!(resp.headers()["content-type"], "application/octet-stream");

  // HEAD yields the status/headers with an empty body.
  let resp = client
    .head(format!("{base}/assets/app.js"))
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 200);
  assert_eq!(resp.text().await.unwrap(), "");

  std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn rejects_non_get_and_missing_paths() {
  let (base, root) = spawn(ServeOptions::default()).await;
  let client = reqwest::Client::new();

  // A non-GET/HEAD verb is refused.
  let resp = client.post(format!("{base}/")).send().await.unwrap();
  assert_eq!(resp.status(), 405);
  assert_eq!(resp.text().await.unwrap(), "method not allowed");

  // A missing path with no SPA / custom 404 is a plain 404.
  let resp = client.get(format!("{base}/nope.txt")).send().await.unwrap();
  assert_eq!(resp.status(), 404);
  assert_eq!(resp.text().await.unwrap(), "not found");

  // Traversal escapes the root and 404s.
  let resp = client
    .get(format!("{base}/../Cargo.toml"))
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 404);

  std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn spa_fallback_serves_index_for_navigations_only() {
  let (base, root) = spawn(ServeOptions {
    spa: true,
    not_found_html: None,
  })
  .await;
  let client = reqwest::Client::new();

  // A navigation (Accept: text/html) to an unknown route gets index.html @200.
  let resp = client
    .get(format!("{base}/app/route"))
    .header("accept", "text/html")
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 200);
  assert_eq!(resp.text().await.unwrap(), "<h1>hi</h1>");

  // A HEAD navigation gets the same 200 with an empty body.
  let resp = client
    .head(format!("{base}/app/route"))
    .header("accept", "text/html")
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 200);
  assert_eq!(resp.text().await.unwrap(), "");

  // A non-HTML fetch (missing hashed asset) still 404s — no fallback.
  let resp = client
    .get(format!("{base}/missing.js"))
    .header("accept", "*/*")
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 404);

  std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn spa_without_index_falls_through_to_404() {
  // A root with no index.html: the SPA fallback cannot fire.
  let root = std::env::temp_dir().join(format!("aperio-serve-noindex-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&root).unwrap();
  let root = std::fs::canonicalize(&root).unwrap();
  let (port, _handle) = start(
    root.to_str().unwrap(),
    ServeOptions {
      spa: true,
      not_found_html: None,
    },
  )
  .await
  .unwrap();
  let resp = reqwest::Client::new()
    .get(format!("http://127.0.0.1:{port}/route"))
    .header("accept", "text/html")
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 404);
  std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn custom_404_page_is_served_for_misses() {
  let opts = ServeOptions {
    spa: false,
    not_found_html: Some(b"<b>gone</b>".to_vec()),
  };
  let (base, root) = spawn(opts).await;
  let client = reqwest::Client::new();

  let resp = client.get(format!("{base}/missing")).send().await.unwrap();
  assert_eq!(resp.status(), 404);
  assert!(
    resp.headers()["content-type"]
      .to_str()
      .unwrap()
      .contains("html")
  );
  assert_eq!(resp.text().await.unwrap(), "<b>gone</b>");

  // HEAD to a miss returns the custom-404 status with an empty body.
  let resp = client.head(format!("{base}/missing")).send().await.unwrap();
  assert_eq!(resp.status(), 404);
  assert_eq!(resp.text().await.unwrap(), "");

  std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn start_rejects_missing_dir_and_non_directory() {
  // A path that does not exist cannot be canonicalized.
  let missing = std::env::temp_dir().join(format!("aperio-nope-{}", uuid::Uuid::new_v4()));
  let err = start(missing.to_str().unwrap(), ServeOptions::default())
    .await
    .unwrap_err();
  assert!(err.contains("cannot open directory"), "{err}");

  // A path that resolves to a file (not a directory) is rejected.
  let file = std::env::temp_dir().join(format!("aperio-file-{}", uuid::Uuid::new_v4()));
  std::fs::write(&file, "x").unwrap();
  let err = start(file.to_str().unwrap(), ServeOptions::default())
    .await
    .unwrap_err();
  assert!(err.contains("is not a directory"), "{err}");
  std::fs::remove_file(&file).unwrap();
}

#[test]
fn options_from_env_reads_spa_and_custom_404() {
  // These env vars are touched only by this test; run its cases sequentially.
  let key_spa = "APERIO_SERVE_SPA";
  let key_404 = "APERIO_SERVE_404";
  let clear = || unsafe {
    std::env::remove_var(key_spa);
    std::env::remove_var(key_404);
  };

  clear();
  let o = options_from_env();
  assert!(!o.spa);
  assert!(o.not_found_html.is_none());

  // SPA accepts "1" and "true" (case-insensitive).
  unsafe { std::env::set_var(key_spa, "true") };
  assert!(options_from_env().spa);
  unsafe { std::env::set_var(key_spa, "1") };
  assert!(options_from_env().spa);
  unsafe { std::env::set_var(key_spa, "no") };
  assert!(!options_from_env().spa);

  // A readable custom-404 file is loaded into memory.
  let page = std::env::temp_dir().join(format!("aperio-404-{}.html", uuid::Uuid::new_v4()));
  std::fs::write(&page, "<x/>").unwrap();
  unsafe { std::env::set_var(key_404, page.to_str().unwrap()) };
  assert_eq!(
    options_from_env().not_found_html.as_deref(),
    Some(&b"<x/>"[..])
  );

  // An empty value is treated as unset.
  unsafe { std::env::set_var(key_404, "  ") };
  assert!(options_from_env().not_found_html.is_none());

  // An unreadable path logs and is ignored.
  unsafe { std::env::set_var(key_404, "/no/such/aperio/404.html") };
  assert!(options_from_env().not_found_html.is_none());

  std::fs::remove_file(&page).unwrap();
  clear();
}
