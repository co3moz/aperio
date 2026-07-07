//! `aperio-client check`: configuration & connectivity diagnostics.

use std::time::Duration;
use tokio_tungstenite::{
  connect_async,
  tungstenite::{client::IntoClientRequest, http::HeaderValue},
};

use crate::config::{CliArgs, FileConfig, build_http_url, build_ws_url, resolve};
use crate::protocol::PROTOCOL_VERSION;

/// `aperio-client check`: diagnoses configuration and connectivity — config
/// resolution, server reachability and version skew, token validity, local
/// target and health endpoint. Exits 0 when everything passes, 1 otherwise.
pub(crate) async fn run_check(cli: &CliArgs, file_cfg: &FileConfig) -> ! {
  println!(
    "aperio-client {} — configuration & connectivity check\n",
    env!("CARGO_PKG_VERSION")
  );

  let mut failures = 0u32;
  let pass = |label: &str, detail: String| println!("  ok    {label}: {detail}");
  let fail = |label: &str, detail: String, failures: &mut u32| {
    *failures += 1;
    println!("  FAIL  {label}: {detail}");
  };

  let http = reqwest::Client::builder()
    .timeout(Duration::from_secs(5))
    .build()
    .unwrap_or_default();

  // --- 1. Configuration resolution ---------------------------------------
  let server = resolve(
    cli.server.clone(),
    "APERIO_SERVER_URL",
    file_cfg.server.clone(),
  );
  let token = resolve(
    cli.token.clone(),
    "APERIO_SERVER_TOKEN",
    file_cfg.token.clone(),
  )
  .filter(|t| !t.trim().is_empty());
  let target = resolve(
    cli.target.clone(),
    "APERIO_CLIENT_TARGET",
    file_cfg.target.clone(),
  );
  let target_health = std::env::var("APERIO_CLIENT_TARGET_HEALTH")
    .ok()
    .or(file_cfg.target_health.clone())
    .filter(|s| !s.trim().is_empty());

  match &server {
    Some(s) => pass("server url", s.clone()),
    None => fail(
      "server url",
      "missing (APERIO_SERVER_URL / --server / yaml: server)".to_string(),
      &mut failures,
    ),
  }
  match &token {
    Some(_) => pass("token", "configured".to_string()),
    None => fail(
      "token",
      "missing (APERIO_SERVER_TOKEN / --token / yaml: token)".to_string(),
      &mut failures,
    ),
  }
  match &target {
    Some(t) => pass("target", t.clone()),
    None => fail(
      "target",
      "missing (APERIO_CLIENT_TARGET / 'http <port>' / yaml: target)".to_string(),
      &mut failures,
    ),
  }

  // --- 2. Server health + version skew -----------------------------------
  if let Some(server) = &server {
    match build_http_url(server, "/aperio/health") {
      Err(e) => fail("server health", e, &mut failures),
      Ok(health_url) => match http.get(&health_url).send().await {
        Err(e) => fail(
          "server health",
          format!("{health_url} unreachable: {e}"),
          &mut failures,
        ),
        Ok(resp) if !resp.status().is_success() => fail(
          "server health",
          format!("{} returned HTTP {}", health_url, resp.status()),
          &mut failures,
        ),
        Ok(resp) => {
          let body: serde_json::Value = resp.json().await.unwrap_or_default();
          let version = body
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
          pass(
            "server health",
            format!(
              "healthy (server v{version}, {} client(s) connected)",
              body
                .get("connected_clients")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
            ),
          );
          match body.get("protocol").and_then(|v| v.as_u64()) {
            Some(p) if p == PROTOCOL_VERSION as u64 => {
              pass("protocol", format!("v{PROTOCOL_VERSION} on both sides"))
            }
            Some(p) => fail(
              "protocol",
              format!(
                "server speaks v{p}, this client speaks v{PROTOCOL_VERSION} — update the older side"
              ),
              &mut failures,
            ),
            None => pass(
              "protocol",
              "server predates protocol reporting (assuming compatible)".to_string(),
            ),
          }
        }
      },
    }
  }

  // --- 3. Token validity (WebSocket handshake) ----------------------------
  if let (Some(server), Some(token)) = (&server, &token) {
    match build_ws_url(server) {
      Err(e) => fail("token check", e, &mut failures),
      Ok(ws_url) => {
        let req = ws_url.clone().into_client_request().ok().and_then(|mut r| {
          HeaderValue::from_str(&format!("Bearer {token}"))
            .ok()
            .map(|v| {
              r.headers_mut().insert("Authorization", v);
              r
            })
        });
        match req {
          None => fail(
            "token check",
            "could not build handshake request".to_string(),
            &mut failures,
          ),
          Some(req) => match tokio::time::timeout(Duration::from_secs(5), connect_async(req)).await
          {
            Ok(Ok((mut ws, _))) => {
              let _ = ws.close(None).await;
              pass("token check", "accepted by the server".to_string());
            }
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp))) => fail(
              "token check",
              format!(
                "server rejected the handshake with HTTP {} (invalid or expired token?)",
                resp.status()
              ),
              &mut failures,
            ),
            Ok(Err(e)) => fail(
              "token check",
              format!("handshake failed: {e}"),
              &mut failures,
            ),
            Err(_) => fail(
              "token check",
              "handshake timed out".to_string(),
              &mut failures,
            ),
          },
        }
      }
    }
  }

  // --- 4. Local target (and its health endpoint) --------------------------
  if let Some(target) = &target {
    match http.get(target).send().await {
      Ok(resp) => pass("target", format!("reachable (HTTP {})", resp.status())),
      Err(e) => fail(
        "target",
        format!("{target} unreachable: {e}"),
        &mut failures,
      ),
    }
    if let Some(health_path) = &target_health {
      let url = if health_path.starts_with("http://") || health_path.starts_with("https://") {
        health_path.clone()
      } else {
        format!(
          "{}/{}",
          target.trim_end_matches('/'),
          health_path.trim_start_matches('/')
        )
      };
      match http.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
          pass("target health", format!("{url} → HTTP {}", resp.status()))
        }
        Ok(resp) => fail(
          "target health",
          format!("{url} → HTTP {}", resp.status()),
          &mut failures,
        ),
        Err(e) => fail(
          "target health",
          format!("{url} unreachable: {e}"),
          &mut failures,
        ),
      }
    }
  }

  println!();
  if failures == 0 {
    println!("All checks passed.");
    std::process::exit(0);
  }
  println!("{failures} check(s) failed.");
  std::process::exit(1);
}
