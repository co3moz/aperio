//! A hostname whose root is the dashboard (`planned_features.md` #152).
//!
//! `/aperio` is nested once and answers on every hostname the server serves,
//! and it stays that way. What this adds is a second door: a hostname whose
//! root *is* the panel, so the super-admin opens `panel.example.com` instead
//! of typing `/aperio`, and an organization that wants one names a subdomain
//! inside its fence and gets its own dashboard at the root of it.
//!
//! The mechanism is one layer, outside every route: when `Host` is a panel
//! hostname and the path is not already under `/aperio`, the path is prefixed
//! with `/aperio` before the router sees it. The router, the session layer,
//! the API and the 404 catch-all stay exactly as they are, and
//! `panel.example.com/aperio/...` keeps resolving there too, which is what
//! `aperio-client api` needs.
//!
//! A panel hostname serves the panel and nothing else: a bind on it is
//! refused everywhere a bind is checked, and a login on an organization's
//! panel admits that organization's identities and the master super-admin,
//! nobody else, which is #151's rule with the question simplified to "whose
//! panel is this".

use axum::{
  extract::{Request, State},
  http::HeaderMap,
  middleware::Next,
  response::Response,
};
use std::sync::Arc;

use crate::state::AppState;

/// Marker on a request that arrived at a panel hostname and was rewritten
/// under `/aperio`: the path the browser asked for, so a redirect to the
/// login page sends it back to `/` rather than to `/aperio`.
#[derive(Clone, Debug)]
pub(crate) struct PanelRequest {
  pub(crate) original: String,
}

/// The request's hostname, lowercased and without a port; `None` without a
/// usable `Host`.
pub(crate) fn request_host(headers: &HeaderMap) -> Option<String> {
  let raw = headers.get("host")?.to_str().ok()?;
  let host = raw
    .rsplit_once(':')
    .filter(|(name, port)| !name.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
    .map(|(name, _)| name)
    .unwrap_or(raw)
    .trim()
    .trim_end_matches('.')
    .to_ascii_lowercase();
  (!host.is_empty()).then_some(host)
}

/// Where a path on a panel hostname lands: the root is the dashboard, and
/// anything else is the same thing under `/aperio`.
pub(crate) fn rewritten_path(path: &str) -> String {
  if path == "/" || path.is_empty() {
    "/aperio".to_string()
  } else {
    format!("/aperio{path}")
  }
}

/// True when `path` is already the admin surface's namespace.
fn under_aperio(path: &str) -> bool {
  path == "/aperio" || path.starts_with("/aperio/")
}

/// The layer: on a panel hostname, a request outside `/aperio` is moved
/// under it, and remembers where it came from.
pub(crate) async fn rewrite(
  State(state): State<Arc<AppState>>,
  mut req: Request,
  next: Next,
) -> Response {
  let host = request_host(req.headers());
  if let Some(host) = host
    && state.is_panel_hostname(&host)
    && !under_aperio(req.uri().path())
  {
    let original = req
      .uri()
      .path_and_query()
      .map(|pq| pq.to_string())
      .unwrap_or_else(|| "/".to_string());
    let target = match req.uri().query() {
      Some(q) => format!("{}?{q}", rewritten_path(req.uri().path())),
      None => rewritten_path(req.uri().path()),
    };
    let mut parts = req.uri().clone().into_parts();
    if let Ok(pq) = target.parse::<axum::http::uri::PathAndQuery>() {
      parts.path_and_query = Some(pq);
      if let Ok(uri) = axum::http::Uri::from_parts(parts) {
        *req.uri_mut() = uri;
        req.extensions_mut().insert(PanelRequest { original });
      }
    }
  }
  next.run(req).await
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
