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

/// Starts the loopback static server; returns the bound port.
pub(crate) async fn start(dir: &str) -> Result<u16, String> {
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
  tokio::spawn(async move {
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
      tokio::spawn(async move {
        let service = service_fn(move |req| {
          let root = root.clone();
          async move { Ok::<_, std::convert::Infallible>(handle(&root, &req).await) }
        });
        let _ = hyper::server::conn::http1::Builder::new()
          .serve_connection(TokioIo::new(stream), service)
          .await;
      });
    }
  });
  Ok(port)
}

/// Builds the response for one request against the served root.
async fn handle(root: &Path, req: &Request<hyper::body::Incoming>) -> Response<Full<Bytes>> {
  let head_only = req.method() == Method::HEAD;
  if req.method() != Method::GET && !head_only {
    return simple(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
  }
  let Some(path) = resolve(root, req.uri().path()) else {
    return simple(StatusCode::NOT_FOUND, "not found");
  };
  match tokio::fs::read(&path).await {
    Ok(bytes) => {
      let mime = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();
      let body = if head_only { Vec::new() } else { bytes };
      Response::builder()
        .status(StatusCode::OK)
        .header("content-type", mime)
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_default()
    }
    Err(_) => simple(StatusCode::NOT_FOUND, "not found"),
  }
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
