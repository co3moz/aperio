//! Static file serving mode (`--serve <dir>` / yaml `serve:` /
//! `APERIO_SERVE`).
//!
//! Instead of forwarding to an existing backend, the client spins up a tiny
//! loopback HTTP server rooted at the given directory and exposes *that*
//! through the tunnel — one command to publish a `dist/` folder or share a
//! directory of files, no backend required. The listener binds
//! `127.0.0.1:0`, so nothing on the machine can reach it except this
//! process, and every regular tunnel feature (binds, auth, cache, header
//! rules) applies unchanged because the tunnel just sees an HTTP target.

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Options for static serving: SPA history fallback and a custom 404 page.
#[derive(Clone, Default)]
pub(crate) struct ServeOptions {
  /// When true, a navigation request (Accept: text/html) that resolves to no
  /// file is answered with the root `index.html` (status 200) so a client-side
  /// router owns the route — the standard single-page-app deployment.
  pub(crate) spa: bool,
  /// Pre-read HTML served (status 404) for not-found requests that the SPA
  /// fallback does not cover.
  pub(crate) not_found_html: Option<Vec<u8>>,
}

/// Reads serve options from the environment (`APERIO_SERVE_SPA`,
/// `APERIO_SERVE_404`). A missing/unreadable 404 file logs and is ignored.
pub(crate) fn options_from_env() -> ServeOptions {
  let spa = std::env::var("APERIO_SERVE_SPA")
    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    .unwrap_or(false);
  let not_found_html = std::env::var("APERIO_SERVE_404")
    .ok()
    .map(|p| p.trim().to_string())
    .filter(|p| !p.is_empty())
    .and_then(|p| match std::fs::read(&p) {
      Ok(bytes) => {
        info!("Static file mode: custom 404 page loaded from {}", p);
        Some(bytes)
      }
      Err(e) => {
        warn!("serve: cannot read custom 404 page {}: {}", p, e);
        None
      }
    });
  ServeOptions {
    spa,
    not_found_html,
  }
}

/// Starts the loopback static server; returns the bound port and the accept
/// loop's `JoinHandle` so a config reload that drops this directory can abort
/// the listener instead of leaking it.
pub(crate) async fn start(
  dir: &str,
  opts: ServeOptions,
) -> Result<(u16, tokio::task::JoinHandle<()>), String> {
  let root = std::fs::canonicalize(dir).map_err(|e| {
    format!(
      "CRITICAL ERROR: serve: cannot open directory '{}': {}",
      dir, e
    )
  })?;
  if !root.is_dir() {
    return Err(format!(
      "CRITICAL ERROR: serve: '{}' is not a directory",
      dir
    ));
  }
  let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
    .await
    .map_err(|e| format!("CRITICAL ERROR: serve: cannot bind a loopback port: {}", e))?;
  let port = listener
    .local_addr()
    .map_err(|e| format!("CRITICAL ERROR: serve: {}", e))?
    .port();
  info!(
    "Static file mode: serving {} on 127.0.0.1:{}",
    root.display(),
    port
  );
  let handle = tokio::spawn(async move {
    loop {
      let (stream, _) = match listener.accept().await {
        Ok(conn) => conn,
        Err(err) => {
          warn!("serve: accept failed: {}", err);
          tokio::time::sleep(std::time::Duration::from_millis(100)).await;
          continue;
        }
      };
      let root = root.clone();
      let opts = opts.clone();
      tokio::spawn(async move {
        let service = service_fn(move |req| {
          let root = root.clone();
          let opts = opts.clone();
          async move { Ok::<_, std::convert::Infallible>(handle(&root, &opts, &req).await) }
        });
        let _ = hyper::server::conn::http1::Builder::new()
          .serve_connection(TokioIo::new(stream), service)
          .await;
      });
    }
  });
  Ok((port, handle))
}

/// Builds the response for one request against the served root.
async fn handle(
  root: &Path,
  opts: &ServeOptions,
  req: &Request<hyper::body::Incoming>,
) -> Response<Full<Bytes>> {
  let head_only = req.method() == Method::HEAD;
  if req.method() != Method::GET && !head_only {
    return simple(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
  }
  if let Some(path) = resolve(root, req.uri().path())
    && let Ok(bytes) = tokio::fs::read(&path).await
  {
    let mime = mime_guess::from_path(&path)
      .first_or_octet_stream()
      .to_string();
    let body = if head_only { Vec::new() } else { bytes };
    return Response::builder()
      .status(StatusCode::OK)
      .header("content-type", mime)
      .body(Full::new(Bytes::from(body)))
      .unwrap_or_default();
  }
  not_found(root, opts, req, head_only).await
}

/// Handles a request that resolved to no file: SPA history fallback (serve the
/// root index.html with 200 for a navigation) first, then a custom 404 page,
/// then a plain 404.
async fn not_found(
  root: &Path,
  opts: &ServeOptions,
  req: &Request<hyper::body::Incoming>,
  head_only: bool,
) -> Response<Full<Bytes>> {
  if opts.spa && wants_html(req) {
    let index = root.join("index.html");
    if index.is_file()
      && let Ok(bytes) = tokio::fs::read(&index).await
    {
      let body = if head_only { Vec::new() } else { bytes };
      return Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/html; charset=utf-8")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_default();
    }
  }
  if let Some(html) = &opts.not_found_html {
    let body = if head_only { Vec::new() } else { html.clone() };
    return Response::builder()
      .status(StatusCode::NOT_FOUND)
      .header("content-type", "text/html; charset=utf-8")
      .body(Full::new(Bytes::from(body)))
      .unwrap_or_default();
  }
  simple(StatusCode::NOT_FOUND, "not found")
}

/// True when the request is a browser navigation (its `Accept` explicitly
/// prefers HTML), used to decide whether the SPA fallback applies. A generic
/// `*/*` (scripts, styles, fonts, `fetch()`) is deliberately excluded, so a
/// missing hashed asset still 404s instead of being served `index.html`.
fn wants_html(req: &Request<hyper::body::Incoming>) -> bool {
  req
    .headers()
    .get("accept")
    .and_then(|v| v.to_str().ok())
    .is_some_and(|a| a.contains("text/html"))
}

/// Plain-text response helper.
fn simple(status: StatusCode, msg: &str) -> Response<Full<Bytes>> {
  Response::builder()
    .status(status)
    .header("content-type", "text/plain; charset=utf-8")
    .body(Full::new(Bytes::from(msg.to_string())))
    .unwrap_or_default()
}

/// Maps a request path to a file under `root`, or `None` when it escapes the
/// root, contains traversal segments, or points at nothing servable.
/// Directories resolve to their `index.html`.
fn resolve(root: &Path, uri_path: &str) -> Option<PathBuf> {
  let decoded = percent_decode(uri_path);
  let mut path = root.to_path_buf();
  for segment in decoded.split('/') {
    if segment.is_empty() || segment == "." {
      continue;
    }
    // Reject traversal and anything OS-special before touching the fs.
    if segment == ".." || segment.contains('\\') || segment.contains(':') {
      return None;
    }
    path.push(segment);
  }
  // Symlinks could still point outside the root; canonicalize and re-check.
  let canonical = std::fs::canonicalize(&path).ok()?;
  if !canonical.starts_with(root) {
    return None;
  }
  if canonical.is_dir() {
    let index = canonical.join("index.html");
    return index.is_file().then_some(index);
  }
  canonical.is_file().then_some(canonical)
}

/// Minimal percent-decoding for URL paths (leaves invalid escapes as-is).
fn percent_decode(s: &str) -> String {
  let bytes = s.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'%'
      && i + 2 < bytes.len()
      && let (Some(hi), Some(lo)) = (
        (bytes[i + 1] as char).to_digit(16),
        (bytes[i + 2] as char).to_digit(16),
      )
    {
      out.push((hi * 16 + lo) as u8);
      i += 3;
    } else {
      out.push(bytes[i]);
      i += 1;
    }
  }
  String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
