//! `aperio-client check`: configuration & connectivity diagnostics.

use std::time::Duration;
use tokio_tungstenite::{
  connect_async,
  tungstenite::{client::IntoClientRequest, http::HeaderValue},
};

use crate::config::{ClientSettings, SettingsSources, build_http_url, build_ws_url};
use crate::protocol::PROTOCOL_VERSION;

/// `aperio-client check`: diagnoses configuration and connectivity — config
/// resolution (with the layer each value came from), server reachability and
/// version skew, token validity, local targets and the health endpoint.
/// Exits 0 when everything passes, 1 otherwise.
pub(crate) async fn run_check(settings: &ClientSettings, sources: &SettingsSources) -> ! {
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
  let server = settings.server.clone();
  let token = settings.token.clone().filter(|t| !t.trim().is_empty());
  let target = settings.target.clone();
  let target_health = settings.target_health.clone();
  let from = |src: Option<crate::config::Source>| {
    src
      .map(|s| format!(" (from {})", s.label()))
      .unwrap_or_default()
  };

  match &server {
    Some(s) => pass("server url", format!("{}{}", s, from(sources.server))),
    None => fail(
      "server url",
      "missing (--server-url / APERIO_SERVER_URL / yaml: server.url)".to_string(),
      &mut failures,
    ),
  }
  match &token {
    Some(_) => pass("token", format!("configured{}", from(sources.token))),
    None => fail(
      "token",
      "missing (--server-token / APERIO_SERVER_TOKEN / yaml: server.token)".to_string(),
      &mut failures,
    ),
  }
  // Visitor-auth overrides must be "user:password" — a value without the
  // colon separator would be silently unusable at login time.
  let auth_probes: Vec<(String, &String)> = settings
    .visitor_auth
    .iter()
    .map(|a| ("visitor auth".to_string(), a))
    .chain(settings.services.iter().enumerate().filter_map(|(i, s)| {
      s.auth.as_ref().map(|a| {
        let name = s.name.clone().unwrap_or_else(|| format!("services[{}]", i));
        (format!("visitor auth for '{}'", name), a)
      })
    }))
    .collect();
  for (label, auth) in auth_probes {
    match auth.split_once(':') {
      Some((user, pass_part)) if !user.is_empty() && !pass_part.is_empty() => {
        pass(&label, "user:password format valid".to_string())
      }
      _ => fail(
        &label,
        "must be \"user:password\" with a non-empty user and password".to_string(),
        &mut failures,
      ),
    }
  }

  match &target {
    Some(t) => pass("target", format!("{}{}", t, from(sources.target))),
    None if !settings.services.is_empty() => pass(
      "target",
      format!(
        "{} service(s) configured (from ./aperio.yaml)",
        settings.services.len()
      ),
    ),
    None if !settings.tunnels.is_empty() => pass(
      "target",
      format!(
        "none — {} tunnel(s) declared (from ./aperio.yaml)",
        settings.tunnels.len()
      ),
    ),
    None => fail(
      "target",
      "missing (--target / APERIO_TARGET / yaml: target / services: or tunnels: list)".to_string(),
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
          Some(req) => {
            let started = std::time::Instant::now();
            match tokio::time::timeout(Duration::from_secs(5), connect_async(req)).await {
              Ok(Ok((mut ws, _))) => {
                let rtt = started.elapsed();
                let _ = ws.close(None).await;
                pass(
                  "token check",
                  format!(
                    "accepted by the server (WS handshake {} ms)",
                    rtt.as_millis()
                  ),
                );
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
            }
          }
        }
      }
    }
  }

  // --- 4. Local targets (and their health endpoints) ----------------------
  // Single-service mode probes the one target; multi-service mode probes
  // every entry of the services: list.
  let mut probes: Vec<(String, String, Option<String>)> = Vec::new();
  if let Some(t) = &target {
    probes.push(("target".to_string(), t.clone(), target_health.clone()));
  } else {
    for (i, entry) in settings.services.iter().enumerate() {
      if let Some(t) = &entry.target {
        let label = format!(
          "service '{}'",
          entry
            .name
            .clone()
            .unwrap_or_else(|| format!("services[{}]", i))
        );
        probes.push((label, t.clone(), entry.target_health.clone()));
      }
    }
  }
  for (label, target, health) in &probes {
    match http.get(target).send().await {
      Ok(resp) => pass(label, format!("reachable (HTTP {})", resp.status())),
      Err(e) => fail(label, format!("{target} unreachable: {e}"), &mut failures),
    }
    if let Some(health_path) = health {
      let url = if health_path.starts_with("http://") || health_path.starts_with("https://") {
        health_path.clone()
      } else {
        format!(
          "{}/{}",
          target.trim_end_matches('/'),
          health_path.trim_start_matches('/')
        )
      };
      let health_label = format!("{label} health");
      match http.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
          pass(&health_label, format!("{url} → HTTP {}", resp.status()))
        }
        Ok(resp) => fail(
          &health_label,
          format!("{url} → HTTP {}", resp.status()),
          &mut failures,
        ),
        Err(e) => fail(
          &health_label,
          format!("{url} unreachable: {e}"),
          &mut failures,
        ),
      }
    }
  }

  // --- 5. TCP targets (legacy tcp_target + declared tunnels) --------------
  // These are raw TCP services, so reachability is probed with a plain
  // connect instead of an HTTP request.
  let mut tcp_probes: Vec<(String, String)> = Vec::new();
  if let Some(t) = &settings.tcp_target {
    tcp_probes.push(("tcp target".to_string(), t.clone()));
  }
  for decl in &settings.tunnels {
    tcp_probes.push((format!("tunnel '{}'", decl.target), decl.target.clone()));
  }
  for (label, addr) in &tcp_probes {
    match tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr)).await {
      Ok(Ok(_)) => pass(label, format!("{addr} accepts TCP connections")),
      Ok(Err(e)) => fail(label, format!("{addr} unreachable: {e}"), &mut failures),
      Err(_) => fail(label, format!("{addr} connect timed out"), &mut failures),
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
