use super::{RangeOutcome, ServeOptions, options, parse_range, percent_decode, resolve, start};

fn setup() -> std::path::PathBuf {
  let root = std::env::temp_dir().join(format!("aperio-serve-test-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(root.join("assets")).unwrap();
  std::fs::write(root.join("index.html"), "<h1>hi</h1>").unwrap();
  std::fs::write(root.join("assets/app.js"), "js").unwrap();
  std::fs::write(root.join("a file.txt"), "spaced").unwrap();
  std::fs::canonicalize(root).unwrap()
}

#[tokio::test]
async fn resolves_files_directories_and_encoded_names() {
  let root = setup();
  assert_eq!(
    resolve(&root, "/assets/app.js").await,
    Some(root.join("assets/app.js"))
  );
  // A directory resolves to its index.html.
  assert_eq!(resolve(&root, "/").await, Some(root.join("index.html")));
  // Percent-encoded names decode before hitting the filesystem.
  assert_eq!(
    resolve(&root, "/a%20file.txt").await,
    Some(root.join("a file.txt"))
  );
  // Missing files are None.
  assert_eq!(resolve(&root, "/nope.txt").await, None);
  std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn rejects_traversal_out_of_the_root() {
  let root = setup();
  assert_eq!(resolve(&root, "/../secrets.txt").await, None);
  assert_eq!(resolve(&root, "/assets/../../secrets.txt").await, None);
  // Encoded traversal decodes first, still rejected.
  assert_eq!(resolve(&root, "/%2e%2e/secrets.txt").await, None);
  assert_eq!(resolve(&root, "/..%2fsecrets.txt").await, None);
  std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn percent_decode_handles_escapes_and_leaves_garbage() {
  assert_eq!(percent_decode("/a%20b"), "/a b");
  assert_eq!(percent_decode("/a%2Fb"), "/a/b");
  assert_eq!(percent_decode("/a%zz"), "/a%zz");
  assert_eq!(percent_decode("/plain"), "/plain");
}

#[tokio::test]
async fn resolve_directory_without_index_is_none() {
  let root = setup();
  // `assets/` exists but holds no index.html, so a directory request yields
  // nothing servable.
  assert_eq!(resolve(&root, "/assets").await, None);
  // A path segment carrying a `:` (drive-letter / scheme smell) is rejected.
  assert_eq!(resolve(&root, "/c:/win.ini").await, None);
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

  // HEAD yields the status/headers with an empty body, and reports the size a
  // GET would return without ever reading the file's contents.
  let resp = client
    .head(format!("{base}/assets/app.js"))
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 200);
  assert_eq!(resp.headers()["content-length"], "2");
  let ctype = resp.headers()["content-type"].to_str().unwrap().to_string();
  assert!(ctype.contains("javascript"), "unexpected type {ctype}");
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

  // A HEAD navigation gets the same 200 with an empty body, and now says how
  // long that body would have been: the answer has to match what the GET
  // above said, and an empty response with no length did not.
  let resp = client
    .head(format!("{base}/app/route"))
    .header("accept", "text/html")
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 200);
  assert_eq!(resp.headers()["content-length"], "11");
  assert_eq!(resp.text().await.unwrap(), "");

  // A non-HTML fetch (missing hashed asset) still 404s, no fallback.
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
fn options_loads_the_custom_404_page() {
  // The values themselves come from the layered configuration (yaml
  // serve_spa / serve_404 or their env spellings); this only covers turning
  // them into ServeOptions.
  let o = options(false, None);
  assert!(!o.spa);
  assert!(o.not_found_html.is_none());
  assert!(options(true, None).spa);

  // A readable page is loaded into memory once.
  let page = std::env::temp_dir().join(format!("aperio-404-{}.html", uuid::Uuid::new_v4()));
  std::fs::write(&page, "<x/>").unwrap();
  assert_eq!(
    options(false, page.to_str()).not_found_html.as_deref(),
    Some(&b"<x/>"[..])
  );

  // A blank value is treated as unset, and an unreadable path is ignored
  // rather than being fatal.
  assert!(options(false, Some("  ")).not_found_html.is_none());
  assert!(
    options(false, Some("/no/such/aperio/404.html"))
      .not_found_html
      .is_none()
  );

  std::fs::remove_file(&page).unwrap();
}

#[test]
fn parse_range_covers_the_forms_and_the_edges() {
  use RangeOutcome::*;
  // The three well-formed single-range shapes.
  assert_eq!(parse_range("bytes=0-4", 10), Satisfiable(0, 4));
  assert_eq!(parse_range("bytes=5-", 10), Satisfiable(5, 9));
  assert_eq!(parse_range("bytes=-3", 10), Satisfiable(7, 9));
  // An end past the body is clamped, per RFC 9110.
  assert_eq!(parse_range("bytes=8-99", 10), Satisfiable(8, 9));
  // A suffix longer than the body means the whole body.
  assert_eq!(parse_range("bytes=-99", 10), Satisfiable(0, 9));
  // Beyond the body: well-formed but unsatisfiable -> 416.
  assert_eq!(parse_range("bytes=10-", 10), Unsatisfiable);
  assert_eq!(parse_range("bytes=10-12", 10), Unsatisfiable);
  assert_eq!(parse_range("bytes=-0", 10), Unsatisfiable);
  assert_eq!(parse_range("bytes=-5", 0), Unsatisfiable);
  // Anything else is ignored and served as a full 200: multi-range, inverted
  // bounds, non-byte units, garbage.
  assert_eq!(parse_range("bytes=0-1,3-4", 10), Ignore);
  assert_eq!(parse_range("bytes=4-2", 10), Ignore);
  assert_eq!(parse_range("items=0-4", 10), Ignore);
  assert_eq!(parse_range("bytes=-", 10), Ignore);
  assert_eq!(parse_range("bytes=a-b", 10), Ignore);
}

#[tokio::test]
async fn range_requests_get_partial_content() {
  let (base, root) = spawn(ServeOptions::default()).await;
  std::fs::write(root.join("video.bin"), "0123456789").unwrap();
  let client = reqwest::Client::new();

  // A middle slice comes back 206 with the exact bytes and a Content-Range.
  let resp = client
    .get(format!("{base}/video.bin"))
    .header("range", "bytes=2-5")
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 206);
  assert_eq!(resp.headers()["content-range"], "bytes 2-5/10");
  assert_eq!(resp.headers()["content-length"], "4");
  assert_eq!(resp.headers()["accept-ranges"], "bytes");
  assert_eq!(resp.text().await.unwrap(), "2345");

  // An open-ended tail and a suffix range both resolve correctly.
  let resp = client
    .get(format!("{base}/video.bin"))
    .header("range", "bytes=7-")
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 206);
  assert_eq!(resp.text().await.unwrap(), "789");
  let resp = client
    .get(format!("{base}/video.bin"))
    .header("range", "bytes=-2")
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 206);
  assert_eq!(resp.headers()["content-range"], "bytes 8-9/10");
  assert_eq!(resp.text().await.unwrap(), "89");

  // A range beyond the file answers 416 with the total size.
  let resp = client
    .get(format!("{base}/video.bin"))
    .header("range", "bytes=99-")
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 416);
  assert_eq!(resp.headers()["content-range"], "bytes */10");

  // A multi-range request is served as the full 200 instead.
  let resp = client
    .get(format!("{base}/video.bin"))
    .header("range", "bytes=0-1,4-5")
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 200);
  assert_eq!(resp.text().await.unwrap(), "0123456789");

  // A plain GET advertises range support and carries its length while the
  // body now streams from disk instead of being read whole into memory.
  let resp = client
    .get(format!("{base}/video.bin"))
    .send()
    .await
    .unwrap();
  assert_eq!(resp.status(), 200);
  assert_eq!(resp.headers()["accept-ranges"], "bytes");
  assert_eq!(resp.headers()["content-length"], "10");
  assert_eq!(resp.text().await.unwrap(), "0123456789");

  std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn a_symlink_out_of_the_root_is_still_refused_after_the_async_move() {
  // `resolve` does its filesystem work through tokio now, and the check that
  // matters is the one after canonicalization: a symlink inside the root
  // pointing outside it must not be served.
  let root = setup();
  let outside = std::env::temp_dir().join(format!("aperio-serve-outside-{}", uuid::Uuid::new_v4()));
  std::fs::write(&outside, "secret").unwrap();
  #[cfg(unix)]
  std::os::unix::fs::symlink(&outside, root.join("leak.txt")).unwrap();

  #[cfg(unix)]
  assert_eq!(resolve(&root, "/leak.txt").await, None);
  // And a file that is really inside still resolves, so the check is not
  // simply refusing everything.
  assert_eq!(
    resolve(&root, "/assets/app.js").await,
    Some(root.join("assets/app.js"))
  );

  let _ = std::fs::remove_file(&outside);
  std::fs::remove_dir_all(&root).unwrap();
}

// --- validators (planned_features #50) --------------------------------------

#[test]
fn if_none_match_compares_weakly_and_understands_a_list() {
  use super::if_none_match_hits;
  let tag = "\"68a1-2f\"";
  assert!(if_none_match_hits(Some(tag), tag));
  assert!(
    if_none_match_hits(Some("*"), tag),
    "* matches anything that exists"
  );
  // The RFC asks for weak comparison here, so a weak form of our own tag is
  // still "you already have this".
  assert!(if_none_match_hits(Some("W/\"68a1-2f\""), tag));
  // A list, as a browser revalidating several candidates sends it.
  assert!(if_none_match_hits(Some("\"other\", \"68a1-2f\""), tag));
  assert!(!if_none_match_hits(Some("\"other\""), tag));
  assert!(!if_none_match_hits(None, tag), "no header is not a hit");
  assert!(!if_none_match_hits(Some(""), tag));
}

#[tokio::test]
async fn a_request_carrying_the_validator_gets_304_without_a_body() {
  let (base, root) = spawn(ServeOptions::default()).await;
  let client = reqwest::Client::new();

  let first = client
    .get(format!("{base}/assets/app.js"))
    .send()
    .await
    .unwrap();
  assert_eq!(first.status(), 200);
  let etag = first.headers()["etag"].to_str().unwrap().to_string();

  let second = client
    .get(format!("{base}/assets/app.js"))
    .header("if-none-match", &etag)
    .send()
    .await
    .unwrap();
  assert_eq!(second.status(), 304);
  assert_eq!(
    second.headers()["etag"].to_str().unwrap(),
    etag,
    "the 304 repeats the validator it matched"
  );
  assert!(
    second
      .headers()
      .get("content-length")
      .is_none_or(|v| v == "0"),
    "a 304 carries no body"
  );

  // A stale validator gets the file itself, not a 304.
  let stale = client
    .get(format!("{base}/assets/app.js"))
    .header("if-none-match", "\"nope\"")
    .send()
    .await
    .unwrap();
  assert_eq!(stale.status(), 200);

  // HEAD answers the same way, so a cache can revalidate either way.
  let head = client
    .head(format!("{base}/assets/app.js"))
    .header("if-none-match", &etag)
    .send()
    .await
    .unwrap();
  assert_eq!(head.status(), 304);
  std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn if_range_continues_a_download_only_while_the_file_is_unchanged() {
  let (base, root) = spawn(ServeOptions::default()).await;
  let client = reqwest::Client::new();
  std::fs::write(root.join("big.bin"), vec![b'x'; 100]).unwrap();

  let head = client.get(format!("{base}/big.bin")).send().await.unwrap();
  let etag = head.headers()["etag"].to_str().unwrap().to_string();

  // Matching validator: the range is honored, so a resume continues.
  let cont = client
    .get(format!("{base}/big.bin"))
    .header("range", "bytes=10-19")
    .header("if-range", &etag)
    .send()
    .await
    .unwrap();
  assert_eq!(cont.status(), 206);

  // Stale validator: the whole file rather than a splice of two versions.
  let restart = client
    .get(format!("{base}/big.bin"))
    .header("range", "bytes=10-19")
    .header("if-range", "\"stale\"")
    .send()
    .await
    .unwrap();
  assert_eq!(restart.status(), 200);
  std::fs::remove_dir_all(&root).unwrap();
}
