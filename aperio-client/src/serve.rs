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

use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::{Bytes, Frame};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::{info, warn};

/// Response body of the static server: files stream from disk instead of
/// being read whole into memory, so peak usage no longer scales with file
/// size times concurrent requests; small generated pages stay buffered.
type ServeBody = BoxBody<Bytes, std::io::Error>;

/// A fully buffered body (generated pages, HEAD answers).
fn buffered(bytes: Bytes) -> ServeBody {
  Full::new(bytes).map_err(|never| match never {}).boxed()
}

/// A body streaming from an open file, bounded to `len` bytes.
fn file_stream(file: tokio::fs::File, len: u64) -> ServeBody {
  let reader = tokio_util::io::ReaderStream::new(file.take(len));
  StreamBody::new(futures_util::StreamExt::map(reader, |chunk| {
    chunk.map(Frame::data)
  }))
  .boxed()
}

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

/// Builds the serve options from the resolved configuration (yaml
/// `serve_spa` / `serve_404`, or `APERIO_SERVE_SPA` / `APERIO_SERVE_404`).
/// A missing/unreadable 404 file logs and is ignored.
pub(crate) fn options(spa: bool, not_found_page: Option<&str>) -> ServeOptions {
  let not_found_html = not_found_page
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
) -> Response<ServeBody> {
  let head_only = req.method() == Method::HEAD;
  if req.method() != Method::GET && !head_only {
    return simple(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
  }
  if let Some(path) = resolve(root, req.uri().path()) {
    let mime = || {
      mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string()
    };
    // A HEAD asks only about the file, so read its size rather than its
    // contents. Reporting the length also makes the answer match what a GET
    // would say, which an empty body alone did not.
    if head_only {
      if let Ok(meta) = tokio::fs::metadata(&path).await
        && meta.is_file()
      {
        return Response::builder()
          .status(StatusCode::OK)
          .header("content-type", mime())
          .header("content-length", meta.len())
          .header("accept-ranges", "bytes")
          .body(buffered(Bytes::new()))
          .unwrap_or_default();
      }
    } else if let Ok(file) = tokio::fs::File::open(&path).await
      && let Ok(meta) = file.metadata().await
      && meta.is_file()
    {
      return serve_file(file, meta.len(), &mime(), req).await;
    }
  }
  not_found(root, opts, req, head_only).await
}

/// Streams an opened file, honoring a single-range `Range` header (video
/// scrubbing, resumable downloads): `206 Partial Content` with a
/// `Content-Range`, or `416` when the range is unsatisfiable. Multi-range
/// requests, malformed values, and requests carrying `If-Range` (this server
/// emits no validators to match it against) fall back to the full `200`.
async fn serve_file(
  mut file: tokio::fs::File,
  len: u64,
  mime: &str,
  req: &Request<hyper::body::Incoming>,
) -> Response<ServeBody> {
  let range_header = req
    .headers()
    .get("range")
    .and_then(|v| v.to_str().ok())
    .filter(|_| !req.headers().contains_key("if-range"));
  if let Some(raw) = range_header {
    match parse_range(raw, len) {
      RangeOutcome::Satisfiable(start, end) => {
        if start > 0 && file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
          return simple(StatusCode::INTERNAL_SERVER_ERROR, "seek failed");
        }
        let span = end - start + 1;
        return Response::builder()
          .status(StatusCode::PARTIAL_CONTENT)
          .header("content-type", mime)
          .header("content-length", span)
          .header("content-range", format!("bytes {}-{}/{}", start, end, len))
          .header("accept-ranges", "bytes")
          .body(file_stream(file, span))
          .unwrap_or_default();
      }
      RangeOutcome::Unsatisfiable => {
        return Response::builder()
          .status(StatusCode::RANGE_NOT_SATISFIABLE)
          .header("content-range", format!("bytes */{}", len))
          .body(buffered(Bytes::new()))
          .unwrap_or_default();
      }
      RangeOutcome::Ignore => {}
    }
  }
  Response::builder()
    .status(StatusCode::OK)
    .header("content-type", mime)
    .header("content-length", len)
    .header("accept-ranges", "bytes")
    .body(file_stream(file, len))
    .unwrap_or_default()
}

/// What to do with a `Range` header value against a body of `len` bytes.
#[derive(Debug, PartialEq)]
enum RangeOutcome {
  /// Serve `206` for the inclusive byte span.
  Satisfiable(u64, u64),
  /// Serve `416`: the request is well-formed but lies beyond the body.
  Unsatisfiable,
  /// Not a single well-formed byte range: serve the full `200` instead,
  /// which RFC 9110 permits for any Range a server chooses not to honor.
  Ignore,
}

/// Parses a single-range `Range` value (`bytes=0-99`, `bytes=100-`,
/// `bytes=-100`). Multi-range and malformed values yield `Ignore`.
fn parse_range(raw: &str, len: u64) -> RangeOutcome {
  let Some(spec) = raw.trim().strip_prefix("bytes=") else {
    return RangeOutcome::Ignore;
  };
  if spec.contains(',') {
    return RangeOutcome::Ignore;
  }
  let Some((start_s, end_s)) = spec.trim().split_once('-') else {
    return RangeOutcome::Ignore;
  };
  let (start_s, end_s) = (start_s.trim(), end_s.trim());
  match (start_s.is_empty(), end_s.is_empty()) {
    // bytes=-N : the final N bytes.
    (true, false) => match end_s.parse::<u64>() {
      Ok(0) => RangeOutcome::Unsatisfiable,
      Ok(suffix) if len == 0 => {
        let _ = suffix;
        RangeOutcome::Unsatisfiable
      }
      Ok(suffix) => {
        let start = len.saturating_sub(suffix);
        RangeOutcome::Satisfiable(start, len - 1)
      }
      Err(_) => RangeOutcome::Ignore,
    },
    // bytes=N- : from N to the end.
    (false, true) => match start_s.parse::<u64>() {
      Ok(start) if start < len => RangeOutcome::Satisfiable(start, len - 1),
      Ok(_) => RangeOutcome::Unsatisfiable,
      Err(_) => RangeOutcome::Ignore,
    },
    // bytes=N-M inclusive.
    (false, false) => match (start_s.parse::<u64>(), end_s.parse::<u64>()) {
      (Ok(start), Ok(end)) if start > end => RangeOutcome::Ignore,
      (Ok(start), Ok(_)) if start >= len => RangeOutcome::Unsatisfiable,
      (Ok(start), Ok(end)) => RangeOutcome::Satisfiable(start, end.min(len - 1)),
      _ => RangeOutcome::Ignore,
    },
    (true, true) => RangeOutcome::Ignore,
  }
}

/// Handles a request that resolved to no file: SPA history fallback (serve the
/// root index.html with 200 for a navigation) first, then a custom 404 page,
/// then a plain 404.
async fn not_found(
  root: &Path,
  opts: &ServeOptions,
  req: &Request<hyper::body::Incoming>,
  head_only: bool,
) -> Response<ServeBody> {
  if opts.spa && wants_html(req) {
    let index = root.join("index.html");
    if index.is_file()
      && let Ok(bytes) = tokio::fs::read(&index).await
    {
      let body = if head_only { Vec::new() } else { bytes };
      return Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/html; charset=utf-8")
        .body(buffered(Bytes::from(body)))
        .unwrap_or_default();
    }
  }
  if let Some(html) = &opts.not_found_html {
    let body = if head_only { Vec::new() } else { html.clone() };
    return Response::builder()
      .status(StatusCode::NOT_FOUND)
      .header("content-type", "text/html; charset=utf-8")
      .body(buffered(Bytes::from(body)))
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
fn simple(status: StatusCode, msg: &str) -> Response<ServeBody> {
  Response::builder()
    .status(status)
    .header("content-type", "text/plain; charset=utf-8")
    .body(buffered(Bytes::from(msg.to_string())))
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
