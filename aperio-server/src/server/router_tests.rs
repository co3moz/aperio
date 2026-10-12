//! The composed application, driven in-process: that `build_state` and
//! `build_router` together produce something that answers, and answers the way
//! the middleware stack in between says it should.

use axum::http::StatusCode;

// ---------------------------------------------------------------------------
// build_state + build_router: the composed app, driven in-process.
// ---------------------------------------------------------------------------

use crate::state::AppState as ComposedState;
use crate::{build_router, build_state};
use axum::Router;
use axum::body::Body as ComposedBody;
use axum::response::Response as ComposedResponse;

fn composed_app<T>(
  f: impl FnOnce(std::sync::Arc<ComposedState>, Router, &tokio::runtime::Runtime) -> T,
) -> T {
  let boot = boot_in_a_fresh_dir();
  let settings =
    aperio_config::server_settings::ServerSettings::from_yaml("metrics:\n  enabled: true\n")
      .unwrap();
  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  let (state, app) = rt.block_on(async {
    let bundle = build_state(boot, settings, None)
      .await
      .expect("the defaults must build");
    let app = build_router(bundle.state.clone(), bundle.metrics_enabled);
    (bundle.state, app)
  });
  f(state, app, &rt)
}

/// The boot settings a server needs before its database opens, over a
/// throwaway data directory.
fn boot_in_a_fresh_dir() -> crate::boot::Boot {
  let dir = crate::test_support::test_temp_root().join(format!("boot-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  crate::boot::Boot {
    token: "0123456789abcdef0123456789abcdef".to_string(),
    data_dir: dir.to_string_lossy().into_owned(),
  }
}

/// One in-process request against the composed router. The connect-info the
/// serve loop would attach per socket is injected as an extension, since no
/// socket exists here.
async fn drive(app: &Router, mut request: axum::http::Request<ComposedBody>) -> ComposedResponse {
  use tower::ServiceExt;
  request
    .extensions_mut()
    .insert(axum::extract::connect_info::ConnectInfo(
      std::net::SocketAddr::from(([127, 0, 0, 1], 40000)),
    ));
  app.clone().oneshot(request).await.unwrap()
}

fn get_req(path: &str) -> axum::http::Request<ComposedBody> {
  let mut req = axum::http::Request::builder()
    .uri(path)
    .body(ComposedBody::empty())
    .unwrap();
  // What the real listener attaches: the handlers that read a peer address
  // (health, the tunnel API) extract it from here.
  req
    .extensions_mut()
    .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
      [127, 0, 0, 1],
      40000,
    ))));
  req
}

#[test]
fn the_composed_router_answers_its_own_surface() {
  composed_app(|state, app, rt| {
    rt.block_on(async {
      // Liveness needs no credential; monitors depend on that.
      let resp = drive(&app, get_req("/aperio/health")).await;
      assert_eq!(resp.status(), StatusCode::OK);

      // The container probes, also uncredentialed.
      let resp = drive(&app, get_req("/aperio/healthz")).await;
      assert_eq!(resp.status(), StatusCode::OK);
      let resp = drive(&app, get_req("/aperio/readyz")).await;
      assert_eq!(resp.status(), StatusCode::OK);

      // Readiness is the one that turns on a shutdown signal, so a load
      // balancer stops sending traffic while the process is still serving what
      // it already has. Liveness must not: restarting here would kill the
      // drain it is meant to protect.
      // send_replace, not send: with no subscriber a plain send fails and
      // leaves the value untouched, which would make this pass for the wrong
      // reason.
      state.shutdown.send_replace(true);
      let resp = drive(&app, get_req("/aperio/readyz")).await;
      assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
      let resp = drive(&app, get_req("/aperio/healthz")).await;
      assert_eq!(resp.status(), StatusCode::OK);
      state.shutdown.send_replace(false);

      // The admin 404 fence: a path matching nothing in the namespace is a
      // 404, never proxied to a tunnel client.
      let resp = drive(&app, get_req("/aperio/api/definitely-not-a-route")).await;
      assert_eq!(resp.status(), StatusCode::NOT_FOUND);

      // The trailing-slash redirect keeps the query string.
      let resp = drive(&app, get_req("/aperio/?tab=tokens")).await;
      assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
      assert_eq!(
        resp.headers().get("location").unwrap(),
        "/aperio?tab=tokens"
      );

      // The dashboard API without a session is refused, not served.
      let resp = drive(&app, get_req("/aperio/api/stats")).await;
      assert!(
        resp.status() == StatusCode::UNAUTHORIZED || resp.status().is_redirection(),
        "unauthenticated admin API answered {}",
        resp.status()
      );

      // The metrics endpoint exists (APERIO_METRICS=1) and is gated by its
      // token rather than open.
      let resp = drive(&app, get_req("/aperio/metrics")).await;
      assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

      // A request through the proxy fallback is answered by the composed
      // stack; what it answers is routing's business, the assertion is that
      // it answered and that no handler panicked (the catch-panic layer
      // would turn that into a 500).
      let resp = drive(&app, get_req("/")).await;
      assert_ne!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

      // The state the router serves is the one build_state assembled.
      assert!(state.dashboard_enabled);
      assert!(state.config().metrics_token.is_some());
    });
  });
}

#[test]
fn build_state_refuses_a_partial_trust_configuration() {
  let settings = aperio_config::server_settings::ServerSettings::from_yaml(
    "proxy:\n  trusted: [not-an-ip-range]\n",
  )
  .unwrap();
  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  let refused = rt.block_on(build_state(boot_in_a_fresh_dir(), settings, None));
  assert!(
    refused.is_none(),
    "a partial trusted-proxy list must refuse startup"
  );
}
