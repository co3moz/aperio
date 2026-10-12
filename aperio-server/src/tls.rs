//! Serving HTTPS directly, `tls` in the settings: a listener that ends TLS
//! itself beside the plain one, certificates from files or an ACME
//! authority, and the plain listener's redirect to it.
//!
//! The same router serves both listeners. A request that came over HTTPS
//! carries [`ServedOverTls`], which is how the parts that build a URL back to
//! this server (an identity provider's callback) know the scheme without a
//! proxy telling them.

mod acme;
mod certs;
mod http;
mod listener;
mod plan;

pub(crate) use plan::{TlsPlan, plan};

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::serve::ListenerExt;
use tracing::{error, info, warn};

use crate::state::AppState;

/// Marks a request that arrived over the built-in HTTPS listener.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ServedOverTls;

/// The scheme a visitor reached this server with: `https` over the built-in
/// listener, otherwise what a trusted proxy says, otherwise `http`.
pub(crate) fn scheme(state: &AppState, headers: &HeaderMap, over_tls: bool) -> &'static str {
  if over_tls {
    return "https";
  }
  let proxied = state.config().trust_proxy
    && headers
      .get("x-forwarded-proto")
      .and_then(|v| v.to_str().ok())
      == Some("https");
  if proxied { "https" } else { "http" }
}

/// The paths the plain listener keeps answering when it redirects the rest:
/// what an orchestrator or a load balancer probes, over plain HTTP, inside
/// the network.
const PROBES: &[&str] = &["/aperio/healthz", "/aperio/readyz", "/aperio/health"];

/// The plain listener's answer when `tls.redirect_http` is on: the same URL
/// over HTTPS, with the method kept (`308`).
pub(crate) async fn redirect_to_https(
  State(port): State<u16>,
  req: Request,
  next: Next,
) -> Response {
  if PROBES.contains(&req.uri().path()) {
    return next.run(req).await;
  }
  let Some(host) = crate::routing::extract_request_host(req.headers()) else {
    return (StatusCode::BAD_REQUEST, "this server answers over https").into_response();
  };
  let port = if port == 443 {
    String::new()
  } else {
    format!(":{port}")
  };
  let path = req
    .uri()
    .path_and_query()
    .map(|p| p.as_str())
    .unwrap_or("/");
  (
    StatusCode::PERMANENT_REDIRECT,
    [(header::LOCATION, format!("https://{host}{port}{path}"))],
  )
    .into_response()
}

/// How often certificate files are looked at for a renewal made by another
/// tool.
const FILES_EVERY: Duration = Duration::from_secs(60);

/// Serves `app` over HTTPS on `host` and the plan's port until the server
/// shuts down. A port that cannot be bound stops the process, as the plain
/// listener's does: a server told to serve HTTPS that quietly does not is
/// worse than one that says why it cannot.
pub(crate) async fn serve(
  state: Arc<AppState>,
  app: Router,
  host: String,
  plan: TlsPlan,
  reuseport: bool,
) {
  let dir = std::path::PathBuf::from(crate::boot::data_dir()).join("tls");
  let certs = Arc::new(certs::CertStore::new(
    &plan.certificates,
    plan.acme.as_ref().map(|_| dir.clone()),
  ));
  let challenges = Arc::new(acme::Challenges::default());
  let mut issuer = None;
  if let Some(acme) = plan.acme.clone() {
    // The directory is the operator's, but it is still a destination the
    // server is told to call, and the outbound policy has the last word.
    match state.config().outbound_policy.check(&acme.directory).await {
      Ok(()) => {
        let made = Arc::new(acme::Issuer::new(
          acme,
          dir.clone(),
          certs.clone(),
          challenges.clone(),
          Some(state.clone()),
        ));
        tokio::spawn(made.clone().run());
        issuer = Some(made);
      }
      Err(e) => error!(
        "tls.acme.directory is refused by the outbound policy ({e}); no certificate will be obtained"
      ),
    }
  }
  if !plan.certificates.is_empty() {
    let certs = certs.clone();
    tokio::spawn(async move {
      loop {
        tokio::time::sleep(FILES_EVERY).await;
        certs.refresh_files();
      }
    });
  }
  let tcp = match crate::bind_listener(&host, plan.port, reuseport).await {
    Ok(tcp) => tcp,
    Err(e) => {
      // The usual cause is the port itself, and the error alone says only
      // "permission denied".
      let hint = if e.kind() == std::io::ErrorKind::PermissionDenied && plan.port < 1024 {
        "; a port below 1024 takes root or the CAP_NET_BIND_SERVICE capability, which the packaged unit grants"
      } else {
        ""
      };
      error!(
        "Failed to bind the HTTPS listener on {host}:{}: {e}{hint}",
        plan.port
      );
      std::process::exit(1);
    }
  };
  let handshakes = Arc::new(listener::Handshakes::new(certs, challenges, issuer.clone()));
  let listener = match listener::listen(tcp, handshakes) {
    Ok(listener) => listener,
    Err(e) => {
      error!("Failed to start the HTTPS listener: {e}");
      std::process::exit(1);
    }
  };
  info!(
    "HTTPS on {host}:{}{}{}",
    plan.port,
    if plan.certificates.is_empty() {
      String::new()
    } else {
      format!(", {} certificate file(s)", plan.certificates.len())
    },
    match &issuer {
      Some(issuer) => format!(", certificates from {}", issuer.plan().directory),
      None => String::new(),
    }
  );
  if plan.acme.is_some() && plan.port != 443 {
    warn!(
      "tls.port is {}: an ACME authority proves a name on port 443, so that port has to reach this one",
      plan.port
    );
  }
  let mut shutdown = state.shutdown.subscribe();
  let app = app.layer(axum::Extension(ServedOverTls));
  let served = axum::serve(
    listener.tap_io(|_| {}),
    app.into_make_service_with_connect_info::<SocketAddr>(),
  )
  .with_graceful_shutdown(async move {
    let _ = shutdown.wait_for(|down| *down).await;
  })
  .await;
  if let Err(e) = served {
    error!("The HTTPS listener stopped: {e}");
  }
}

#[cfg(test)]
#[path = "tls_tests.rs"]
mod tests;
