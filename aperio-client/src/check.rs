//! `aperio-client check`: configuration & connectivity diagnostics.

use std::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue};

use crate::config::{ClientSettings, SettingsSources, build_http_url, build_ws_url};
use crate::protocol::PROTOCOL_VERSION;

/// `aperio-client check`: diagnoses configuration and connectivity, config
/// resolution (with the layer each value came from), server reachability and
/// version skew, token validity, local targets and the health endpoint.
/// Exits 0 when everything passes, 1 otherwise.
pub(crate) async fn run_check(settings: &ClientSettings, sources: &SettingsSources) -> ! {
  println!(
    "aperio-client {}, configuration & connectivity check\n",
    env!("CARGO_PKG_VERSION")
  );

  let mut failures = 0u32;
  let pass = |label: &str, detail: String| println!("  ok    {label}: {detail}");
  // Not a failure, the client starts and serves, but the file says something
  // it will not do, which is exactly what this command exists to surface.
  let warn = |label: &str, detail: String| println!("  warn  {label}: {detail}");
  let fail = |label: &str, detail: String, failures: &mut u32| {
    *failures += 1;
    println!("  FAIL  {label}: {detail}");
  };

  // Both probes below have to take the route the running client takes, or
  // this command diagnoses a connection nobody makes. The WebSocket handshake
  // goes through `dial`, so it needs the process-wide setting applied here as
  // startup would; the health request is an ordinary HTTP call and carries
  // the proxy itself.
  crate::dial::set_egress_proxy(settings.egress_proxy.clone());
  crate::ensure_crypto_provider();
  let mut http_builder = reqwest::Client::builder().timeout(Duration::from_secs(5));
  if let Some(ref proxy) = settings.egress_proxy {
    match crate::egress::as_reqwest(proxy) {
      Ok(configured) => http_builder = http_builder.proxy(configured),
      Err(e) => println!("  warn  egress proxy: {e}"),
    }
  }
  let http = http_builder.build().unwrap_or_default();

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
  // Named when set, because the two probes below then say something about a
  // route the reader cannot otherwise see, and a "server unreachable" whose
  // real subject is the proxy is the least useful answer this command gives.
  // Host and port only: the value may carry a credential.
  if let Some(ref proxy) = settings.egress_proxy {
    pass(
      "egress proxy",
      format!(
        "dialing through {}{}",
        proxy.redacted(),
        if proxy.has_credentials() {
          " with a credential"
        } else {
          ""
        }
      ),
    );
  }
  // Every visitor gate is checked here rather than at the moment a visitor
  // fails to get in: a method that does not exist, or a credential missing
  // half of itself, presents hours later as "the password does not work".
  let auth_probes: Vec<(String, &aperio_config::AuthSetting)> = settings
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
    match aperio_config::validate_auth_setting(auth) {
      Ok(()) => {
        let methods: Vec<String> = auth
          .methods()
          .iter()
          .map(|m| m.method.trim().to_ascii_lowercase())
          .collect();
        pass(&label, format!("method(s): {}", methods.join(", ")))
      }
      Err(why) => fail(&label, why, &mut failures),
    }
  }

  // Which shape actually drives the client, mirroring `build_specs`: a
  // `services:` list wins over a single service named at the top level, unless
  // that target came from the command line, where the positional argument is
  // deliberately an override. Getting this backwards made the whole section a
  // lie for a file that had both, it named, and probed, the one backend the
  // client was going to ignore.
  let cli_target = matches!(sources.target, Some(crate::config::Source::Cli));
  let services_win = !settings.services.is_empty() && !cli_target;
  if services_win {
    let shadowed: Vec<&str> = [
      ("target", target.is_some()),
      ("serve", settings.serve.is_some()),
      ("hostname", !settings.hostnames.is_empty()),
      ("path", settings.path.is_some()),
      ("tcp_target", settings.tcp_target.is_some()),
      ("target_health", target_health.is_some()),
    ]
    .into_iter()
    .filter(|(_, set)| *set)
    .map(|(key, _)| key)
    .collect();
    if !shadowed.is_empty() {
      warn(
        "single-service keys",
        format!(
          "`{}` come from the command line or the environment and are ignored while a services: list exists, the entries below are what runs. A config file no longer accepts them at all.",
          shadowed.join("`, `")
        ),
      );
    }
  }

  match &target {
    Some(t) if !services_win => pass("target", format!("{}{}", t, from(sources.target))),
    _ if settings.serve.is_some() && !services_win => pass(
      "target",
      format!(
        "static directory '{}' (serve mode)",
        settings.serve.as_deref().unwrap_or_default()
      ),
    ),
    _ if services_win => pass(
      "target",
      format!(
        "{} service(s) configured (from ./aperio.yaml)",
        settings.services.len()
      ),
    ),
    _ if !settings.tunnels.is_empty() => pass(
      "target",
      format!(
        "none, {} tunnel(s) declared (from ./aperio.yaml)",
        settings.tunnels.len()
      ),
    ),
    // A binder exposes nothing: it exists to open local listeners onto tunnels
    // someone else declared. Reporting a missing target for one was wrong
    // twice over, since the message did not even mention the section the file
    // is made of.
    _ if !settings.bind_tunnels.is_empty() => pass(
      "target",
      format!(
        "none, binds {} tunnel(s) (from ./aperio.yaml)",
        settings.bind_tunnels.len()
      ),
    ),
    // A client that only sends and receives messages exposes nothing either,
    // and is as complete a configuration as a binder.
    _ if !settings.subscribe.is_empty()
      || settings.messages_listen.is_some()
      || settings.messages_mqtt_listen.is_some() =>
    {
      pass(
        "target",
        format!(
          "none, messaging only, {} subscription(s) (from ./aperio.yaml)",
          settings.subscribe.len()
        ),
      )
    }
    _ => fail(
      "target",
      "missing (--target / APERIO_TARGET / yaml: services:, tunnels:, bind-tunnels: or subscribe:)"
        .to_string(),
      &mut failures,
    ),
  }

  // Literal credentials in the file. A warning rather than a failure: it is
  // a working configuration, and where the file is a private, unreadable
  // deploy artifact it may even be the deliberate one. But a secret typed
  // into a file is a secret that ends up in a repository, a backup and a
  // support ticket, and the alternative costs one `${VAR}`.
  for (path, findings) in literal_secrets_in_config_files() {
    for finding in findings {
      warn(
        "secrets",
        format!(
          "{path}: `{finding}` holds a literal value; write `${{{}}}` and keep the secret in the environment",
          finding.to_ascii_uppercase()
        ),
      );
    }
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
                "server speaks v{p}, this client speaks v{PROTOCOL_VERSION}, update the older side"
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
            match tokio::time::timeout(Duration::from_secs(5), crate::dial::connect_ws(req, None))
              .await
            {
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
  // A `serve:` directory is the backend itself, so its check is a directory
  // check rather than an HTTP probe.
  let check_serve_dir = |label: &str, dir: &str, failures: &mut u32| {
    if std::path::Path::new(dir).is_dir() {
      pass(label, format!("serve directory '{}' exists", dir));
    } else {
      fail(
        label,
        format!("serve directory '{}' is missing or not a directory", dir),
        failures,
      );
    }
  };
  // Names are identifiers, and the check is where a file learns that before
  // a deploy does. Both lists, since a tunnel name is addressed from another
  // machine entirely.
  for (i, entry) in settings.services.iter().enumerate() {
    if let Some(name) = entry.name.as_deref()
      && let Err(why) = aperio_config::validate_name("service", name)
    {
      fail(&format!("services[{i}]"), why, &mut failures);
    }
  }
  for decl in &settings.tunnels {
    if let Some(name) = decl.name.as_deref()
      && let Err(why) = aperio_config::validate_tunnel_name(name)
    {
      fail("tunnels", why, &mut failures);
    }
  }
  let mut probes: Vec<(String, String, Option<String>)> = Vec::new();
  if let Some(t) = target.as_ref().filter(|_| !services_win) {
    probes.push(("target".to_string(), t.clone(), target_health.clone()));
  } else if let Some(dir) = settings.serve.as_ref().filter(|_| !services_win) {
    check_serve_dir("target", dir, &mut failures);
  } else {
    for (i, entry) in settings.services.iter().enumerate() {
      let label = format!(
        "service '{}'",
        entry
          .name
          .clone()
          .unwrap_or_else(|| format!("services[{}]", i))
      );
      if let Some(t) = &entry.target {
        probes.push((label, t.clone(), entry.target_health.clone()));
      } else if let Some(dir) = &entry.serve {
        check_serve_dir(&label, dir, &mut failures);
      }
    }
  }
  for (label, target, health) in &probes {
    // h2c/h2 schemes are aperio vocabulary; probe over plain HTTP(S).
    let target = &target
      .replacen("h2c://", "http://", 1)
      .replacen("h2://", "https://", 1);
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

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;

/// Keys whose value is a credential.
///
/// Named rather than guessed at from the value: a heuristic over values would
/// flag every long random-looking string, and a config file is full of those.
const SECRET_KEYS: &[&str] = &[
  "token",
  "psk",
  "client_secret",
  "secret",
  "password",
  "api_key",
  "device_key",
];

/// Secret-bearing keys in one config file's text whose value is written out
/// literally.
///
/// Reads the file as text rather than through the parsed configuration: by the
/// time a value is a `String` in memory, a literal and an expanded `${VAR}`
/// are the same thing, and the distinction is the entire point.
pub(crate) fn literal_secrets_in(text: &str) -> Vec<String> {
  let mut found: Vec<String> = Vec::new();
  for line in text.lines() {
    let line = line.trim();
    if line.starts_with('#') {
      continue;
    }
    let Some((key, value)) = line.split_once(':') else {
      continue;
    };
    let key = key.trim_start_matches(['-', ' ']).trim();
    let value = value.trim().trim_matches(['"', '\'']);
    if value.is_empty() || value.starts_with("${") {
      continue;
    }
    if SECRET_KEYS.contains(&key) && !found.iter().any(|k| k == key) {
      found.push(key.to_string());
    }
  }
  found
}

/// The same, over the config files this client would load.
fn literal_secrets_in_config_files() -> Vec<(String, Vec<String>)> {
  let mut out = Vec::new();
  for path in ["aperio.yaml".to_string(), "aperio.yml".to_string()]
    .into_iter()
    .chain(dirs_home_config())
  {
    let Ok(text) = std::fs::read_to_string(&path) else {
      continue;
    };
    let found = literal_secrets_in(&text);
    if !found.is_empty() {
      out.push((path, found));
    }
  }
  out
}

/// `~/.aperio.yaml`, when there is a home directory to look in.
fn dirs_home_config() -> Option<String> {
  std::env::var("HOME")
    .or_else(|_| std::env::var("USERPROFILE"))
    .ok()
    .map(|h| format!("{h}/.aperio.yaml"))
}
