//! Unit tests for the `aperio-client api` command surface: duration parsing,
//! the CLI → HTTP call mapping, and credential selection.

use super::*;
use clap::Parser;

/// Parses an `aperio-client api ...` command line into its `ApiCommand` and
/// the global options (`--hostname`, `--path`, …) that scope it.
fn parse(args: &[&str]) -> (ApiCommand, CommonOpts) {
  #[derive(Parser)]
  struct Harness {
    #[command(subcommand)]
    command: ApiCommand,
    #[command(flatten)]
    opts: CommonOpts,
  }
  let mut argv = vec!["aperio-client"];
  argv.extend_from_slice(args);
  let parsed = Harness::parse_from(argv);
  (parsed.command, parsed.opts)
}

/// Builds the call for one parsed command line.
fn call_for(args: &[&str]) -> Result<Call, String> {
  let (command, opts) = parse(args);
  build_call(&command, &settings(), &opts)
}

fn settings() -> ClientSettings {
  let cli = crate::config::CliArgs {
    mode: crate::config::CliMode::Run,
    target: None,
    local_port: None,
    opts: Default::default(),
  };
  crate::config::resolve_settings(&cli, &Default::default(), &Default::default()).unwrap()
}

#[test]
fn test_parse_duration_units_and_never() {
  assert_eq!(parse_duration("45s"), Ok(45));
  assert_eq!(parse_duration("30m"), Ok(1_800));
  assert_eq!(parse_duration("2h"), Ok(7_200));
  assert_eq!(parse_duration("1d"), Ok(86_400));
  assert_eq!(parse_duration("2w"), Ok(1_209_600));
  // A bare number is seconds; `never` and its aliases mean "no expiry".
  assert_eq!(parse_duration("90"), Ok(90));
  assert_eq!(parse_duration("never"), Ok(0));
  assert_eq!(parse_duration("NEVER"), Ok(0));
  assert_eq!(parse_duration("none"), Ok(0));
  assert!(parse_duration("tomorrow").is_err());
  assert!(parse_duration("").is_err());
}

#[test]
fn test_ttl_field_never_omits_for_create_endpoints() {
  // Share links treat 0 as "never expires", so `never` must be sent.
  assert_eq!(ttl_field(&Some("never".into()), false), Ok(Some(0)));
  // Token creation treats an absent field as "never", so 0 is dropped.
  assert_eq!(ttl_field(&Some("never".into()), true), Ok(None));
  assert_eq!(ttl_field(&Some("1d".into()), true), Ok(Some(86_400)));
  assert_eq!(ttl_field(&None, false), Ok(None));
}

#[test]
fn test_share_call_maps_hostname_path_and_expiry() {
  let call = call_for(&[
    "share",
    "--hostname",
    "app.example.com",
    "--path",
    "/test",
    "--expire",
    "1d",
  ])
  .expect("share call builds");
  assert_eq!(call.method, reqwest::Method::POST);
  assert_eq!(call.path, "/aperio/api/share");
  let body = call.body.expect("share sends a body");
  assert_eq!(body["hostname"], "app.example.com");
  assert_eq!(body["path"], "/test");
  assert_eq!(body["ttl_seconds"], 86_400);
}

#[test]
fn test_share_call_omits_absent_optional_fields() {
  let call = call_for(&["share", "--host", "app.example.com"]).unwrap();
  let body = call.body.expect("share sends a body");
  assert!(body.get("path").is_none());
  assert!(body.get("ttl_seconds").is_none());
}

#[test]
fn test_token_create_collects_repeatable_permissions() {
  let call = call_for(&[
    "token",
    "create",
    "--name",
    "ci",
    "--hostname",
    "a.example.com,b.example.com",
    "--path",
    "/api",
    "--allowed-ip",
    "10.0.0.0/8",
    "--expire",
    "2h",
    "--allow-public",
  ])
  .unwrap();
  assert_eq!(call.path, "/aperio/api/tokens");
  let body = call.body.unwrap();
  assert_eq!(
    body["hostnames"],
    serde_json::json!(["a.example.com", "b.example.com"])
  );
  assert_eq!(body["paths"], serde_json::json!(["/api"]));
  assert_eq!(body["allowed_ips"], serde_json::json!(["10.0.0.0/8"]));
  assert_eq!(body["ttl_seconds"], 7_200);
  assert_eq!(body["allow_public"], true);
  assert_eq!(body["canary"], false);
}

#[test]
fn test_token_update_sends_only_the_given_fields() {
  let call = call_for(&[
    "token",
    "update",
    "tok1",
    "--name",
    "renamed",
    "--no-canary",
  ])
  .unwrap();
  assert_eq!(call.method, reqwest::Method::PUT);
  assert_eq!(call.path, "/aperio/api/tokens/tok1");
  let body = call.body.unwrap();
  assert_eq!(body["name"], "renamed");
  // Untouched scopes stay absent so the server keeps them as they are.
  assert!(body.get("hostnames").is_none());
  assert!(body.get("ttl_seconds").is_none());
  // The explicit off-switch is sent as false.
  assert_eq!(body["canary"], false);
}

#[test]
fn test_token_rotate_defaults_to_immediate_cutover() {
  let call = call_for(&["token", "rotate", "tok1"]).unwrap();
  assert_eq!(call.path, "/aperio/api/tokens/tok1/rotate");
  assert_eq!(call.body.unwrap()["grace_seconds"], 0);

  let call = call_for(&["token", "rotate", "tok1", "--grace", "1h"]).unwrap();
  assert_eq!(call.body.unwrap()["grace_seconds"], 3_600);
}

#[test]
fn test_token_refresh_presents_the_token_secret_itself() {
  let call = call_for(&["token", "refresh", "--secret", "apr_secret"]).unwrap();
  assert_eq!(call.path, "/aperio/api/tokens/refresh");
  assert_eq!(call.auth.as_deref(), Some("apr_secret"));
  // Nothing to send: the endpoint authenticates from the header alone.
  assert!(call.body.is_none());
}

#[test]
fn test_maintenance_on_off_toggle_the_enabled_flag() {
  let on = call_for(&["maintenance", "on", "app.example.com"]).unwrap();
  assert_eq!(on.path, "/aperio/api/maintenance");
  assert_eq!(on.body.unwrap()["enabled"], true);

  let off = call_for(&["maintenance", "off", "*"]).unwrap();
  let body = off.body.unwrap();
  assert_eq!(body["hostname"], "*");
  assert_eq!(body["enabled"], false);

  let list = call_for(&["maintenance", "list"]).unwrap();
  assert_eq!(list.method, reqwest::Method::GET);
}

#[test]
fn test_tunnel_create_and_delete() {
  let create = call_for(&[
    "tunnel",
    "create",
    "--hostname",
    "pr-7.example.com",
    "--expire",
    "30m",
  ])
  .unwrap();
  assert_eq!(create.path, "/aperio/api/tunnels");
  let body = create.body.unwrap();
  assert_eq!(body["hostname"], "pr-7.example.com");
  assert_eq!(body["ttl_seconds"], 1_800);

  let delete = call_for(&["tunnel", "delete", "tid"]).unwrap();
  assert_eq!(delete.method, reqwest::Method::DELETE);
  assert_eq!(delete.path, "/aperio/api/tunnels/tid");
}

#[test]
fn test_client_override_clear_sends_empty_strings() {
  let call = call_for(&["client", "override", "c1", "--clear"]).unwrap();
  assert_eq!(call.path, "/aperio/api/clients/c1/override");
  let body = call.body.unwrap();
  assert_eq!(body["hostname_bind"], "");
  assert_eq!(body["path_bind"], "");
}

#[test]
fn test_client_enable_disable() {
  let enable = call_for(&["client", "enable", "c1"]).unwrap();
  assert_eq!(enable.path, "/aperio/api/clients/c1/enabled");
  assert_eq!(enable.body.unwrap()["enabled"], true);
  let disable = call_for(&["client", "disable", "c1"]).unwrap();
  assert_eq!(disable.body.unwrap()["enabled"], false);
}

#[test]
fn test_read_only_reports_use_get_with_query_parameters() {
  let history = call_for(&["history", "--unit", "week", "--count", "8"]).unwrap();
  assert_eq!(history.path, "/aperio/api/stats/history");
  assert_eq!(
    history.query,
    vec![
      ("unit".to_string(), "week".to_string()),
      ("count".to_string(), "8".to_string())
    ]
  );

  let bandwidth = call_for(&["bandwidth"]).unwrap();
  assert!(bandwidth.query.is_empty());
  assert_eq!(bandwidth.path, "/aperio/api/bandwidth");

  for (args, path) in [
    (vec!["stats"], "/aperio/api/stats"),
    (vec!["logs"], "/aperio/api/logs"),
    (vec!["uptime"], "/aperio/api/uptime"),
    (vec!["topology"], "/aperio/api/topology"),
    (vec!["self-health"], "/aperio/api/self-health"),
    (vec!["health"], "/aperio/health"),
    (vec!["audit", "list"], "/aperio/api/audit"),
    (vec!["audit", "verify"], "/aperio/api/audit/verify"),
    (vec!["cache", "stats"], "/aperio/api/cache/stats"),
    (vec!["openapi"], "/aperio/api/openapi.json"),
  ] {
    let call = call_for(&args).unwrap();
    assert_eq!(call.method, reqwest::Method::GET, "{:?}", args);
    assert_eq!(call.path, path, "{:?}", args);
  }
}

#[test]
fn test_purge_requires_at_least_one_filter() {
  assert!(call_for(&["purge"]).is_err());
  let call = call_for(&["purge", "--hostname", "app.example.com"]).unwrap();
  assert_eq!(call.body.unwrap()["hostname"], "app.example.com");
}

#[test]
fn test_cache_purge_without_filters_clears_everything() {
  let call = call_for(&["cache", "purge"]).unwrap();
  assert_eq!(call.path, "/aperio/api/cache/purge");
  assert_eq!(call.body.unwrap(), serde_json::json!({}));
}

#[test]
fn test_session_revoke_needs_an_id_or_all() {
  assert!(call_for(&["user", "revoke"]).is_err());
  let one = call_for(&["user", "revoke", "s1"]).unwrap();
  assert_eq!(one.path, "/aperio/api/sessions/s1");
  let all = call_for(&["user", "revoke", "--all"]).unwrap();
  assert_eq!(all.path, "/aperio/api/sessions");
}

#[test]
fn test_org_quota_maps_only_the_given_limits() {
  let call = call_for(&[
    "org",
    "quota",
    "acme",
    "--max-users",
    "10",
    "--max-tokens",
    "0",
  ])
  .unwrap();
  assert_eq!(call.method, reqwest::Method::PUT);
  assert_eq!(call.path, "/aperio/api/orgs/acme/quota");
  let body = call.body.unwrap();
  assert_eq!(body["max_users"], 10);
  // 0 is meaningful (clears the quota) and must be sent, not dropped.
  assert_eq!(body["max_tokens"], 0);
  assert!(body.get("max_clients").is_none());
}

#[test]
fn test_org_create_and_hostnames_carry_the_allowlist() {
  let create = call_for(&[
    "org",
    "create",
    "--name",
    "acme",
    "--hostname",
    "acme.com,*.acme.example.com",
  ])
  .unwrap();
  assert_eq!(create.path, "/aperio/api/orgs");
  let body = create.body.unwrap();
  assert_eq!(body["name"], "acme");
  assert_eq!(
    body["hostnames"],
    serde_json::json!(["acme.com", "*.acme.example.com"])
  );

  // No --hostname: an unfenced org (and, on the update, a cleared fence).
  let create = call_for(&["org", "create", "--name", "acme"]).unwrap();
  assert_eq!(create.body.unwrap()["hostnames"], serde_json::json!([]));

  let set = call_for(&["org", "hostnames", "o1", "--hostname", "*.acme.com"]).unwrap();
  assert_eq!(set.method, reqwest::Method::PUT);
  assert_eq!(set.path, "/aperio/api/orgs/o1/hostnames");
  assert_eq!(
    set.body.unwrap()["hostnames"],
    serde_json::json!(["*.acme.com"])
  );

  let clear = call_for(&["org", "hostnames", "o1"]).unwrap();
  assert_eq!(clear.body.unwrap()["hostnames"], serde_json::json!([]));
}

#[test]
fn test_admin_key_create_maps_role_and_org() {
  let call = call_for(&[
    "admin-key",
    "create",
    "--name",
    "ci",
    "--role",
    "operator",
    "--org",
    "acme",
  ])
  .unwrap();
  assert_eq!(call.path, "/aperio/api/admin-keys");
  let body = call.body.unwrap();
  assert_eq!(body["role"], "operator");
  assert_eq!(body["org_id"], "acme");
  // No --expire: the key never expires, so the field stays absent.
  assert!(body.get("ttl_seconds").is_none());
}

#[test]
fn test_webhook_create_and_deliveries_query() {
  let create = call_for(&[
    "webhook",
    "create",
    "--name",
    "ops",
    "--url",
    "https://hooks.example.com/x",
    "--event",
    "client_connected",
    "--format",
    "slack",
  ])
  .unwrap();
  let body = create.body.unwrap();
  assert_eq!(body["events"], serde_json::json!(["client_connected"]));
  assert_eq!(body["format"], "slack");

  let deliveries = call_for(&["webhook", "deliveries", "--limit", "10"]).unwrap();
  assert_eq!(deliveries.path, "/aperio/api/webhooks/deliveries");
  assert_eq!(
    deliveries.query,
    vec![("limit".to_string(), "10".to_string())]
  );
}

#[test]
fn test_bodyless_posts_carry_no_json_body() {
  for args in [
    vec!["request", "replay", "r1"],
    vec!["inbox", "refire", "i1"],
    vec!["webhook", "redeliver", "d1"],
  ] {
    let call = call_for(&args).unwrap();
    assert_eq!(call.method, reqwest::Method::POST, "{:?}", args);
    assert!(
      call.body.as_ref().is_none_or(serde_json::Value::is_null),
      "{:?} must not send a JSON body",
      args
    );
  }
}

#[test]
fn test_credential_prefers_the_api_key_over_the_tunnel_token() {
  let mut s = settings();
  s.token = Some("apr_tunnel".into());
  s.api_key = None;
  assert_eq!(credential(&s).as_deref(), Some("apr_tunnel"));
  s.api_key = Some("apk_admin".into());
  assert_eq!(credential(&s).as_deref(), Some("apk_admin"));
  // A blank credential is treated as unset.
  s.api_key = Some("  ".into());
  s.token = None;
  assert!(credential(&s).is_none());
}

// ---------------------------------------------------------------------------
// The command arms nothing exercised: one spot check per family, through the
// same parser a shell would use.
// ---------------------------------------------------------------------------

#[test]
fn every_command_family_builds_its_call() {
  for (args, method, path_part) in [
    (vec!["token", "list"], "GET", "/aperio/api/tokens"),
    (vec!["token", "revoke", "tok-1"], "DELETE", "/tokens/tok-1"),
    (vec!["webhook", "list"], "GET", "/aperio/api/webhooks"),
    (vec!["webhook", "delete", "w-1"], "DELETE", "/webhooks/w-1"),
    (vec!["inbox", "list"], "GET", "/aperio/api/inbox"),
    (
      vec!["user", "reset-totp", "u-1"],
      "DELETE",
      "/users/u-1/totp",
    ),
    (vec!["org", "delete", "o-1"], "DELETE", "/orgs/o-1"),
    (vec!["org", "usage", "o-1"], "GET", "/orgs/o-1/usage"),
    (
      vec!["admin-key", "revoke", "k-1"],
      "DELETE",
      "/admin-keys/k-1",
    ),
    (vec!["request", "show", "r-1"], "GET", "/requests/r-1"),
    (vec!["settings", "get"], "GET", "/aperio/api/settings"),
    (vec!["slow-endpoints"], "GET", "/slow-endpoints"),
    (vec!["route-trends"], "GET", "/route-trends"),
    (vec!["traffic-csv"], "GET", "/export/traffic.csv"),
    (vec!["export"], "GET", "/aperio/api/export"),
  ] {
    let call = call_for(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
    assert_eq!(call.method.as_str(), method, "{args:?}");
    assert!(call.path.contains(path_part), "{args:?} -> {}", call.path);
  }
}

#[test]
fn token_create_carries_the_scope_and_flags() {
  let (command, mut opts) = parse(&[
    "token",
    "create",
    "--name",
    "deploy",
    "--allow-public",
    "--allowed-ip",
    "10.0.0.0/8",
    "--expire",
    "2h",
  ]);
  opts.hostname = Some("a.example.com,b.example.com".to_string());
  opts.path = Some("/api".to_string());
  let call = build_call(&command, &settings(), &opts).unwrap();
  let body = call.body.unwrap();
  assert_eq!(
    body["hostnames"],
    serde_json::json!(["a.example.com", "b.example.com"])
  );
  assert_eq!(body["paths"], serde_json::json!(["/api"]));
  assert_eq!(body["allowed_ips"], serde_json::json!(["10.0.0.0/8"]));
  assert_eq!(body["allow_public"], serde_json::json!(true));
  assert_eq!(body["ttl_seconds"], serde_json::json!(7200));
}

// ---------------------------------------------------------------------------
// send(): the HTTP half, against a local server answering canned responses.
// ---------------------------------------------------------------------------

/// Answers each accepted connection with one canned response and remembers
/// what arrived.
fn canned_server(
  response: &'static str,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
  let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
  let addr = listener.local_addr().unwrap();
  let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
  let seen_writer = seen.clone();
  std::thread::spawn(move || {
    while let Ok((mut socket, _)) = listener.accept() {
      use std::io::{Read, Write};
      let mut buf = [0u8; 4096];
      let n = socket.read(&mut buf).unwrap_or(0);
      seen_writer
        .lock()
        .unwrap()
        .push(String::from_utf8_lossy(&buf[..n]).to_string());
      let _ = socket.write_all(response.as_bytes());
    }
  });
  (format!("http://{addr}"), seen)
}

fn http() -> reqwest::Client {
  reqwest::Client::builder()
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .unwrap()
}

#[tokio::test]
async fn send_decodes_json_passes_text_through_and_maps_the_empty_body() {
  let (server, seen) = canned_server(
    "HTTP/1.1 200 OK\r\ncontent-length: 13\r\nconnection: close\r\n\r\n{\"tokens\":[]}",
  );
  let call = Call::get("/aperio/api/tokens").query("limit", Some("5".to_string()));
  let value = send(&http(), &server, Some("apk_secret"), call)
    .await
    .unwrap();
  assert_eq!(value["tokens"], serde_json::json!([]));
  let request = seen.lock().unwrap().join("");
  assert!(request.contains("limit=5"), "{request}");
  assert!(
    request.contains("authorization: Bearer apk_secret"),
    "{request}"
  );

  // Plain text (the CSV export) comes back as a string to print verbatim.
  let (server, _) =
    canned_server("HTTP/1.1 200 OK\r\ncontent-length: 8\r\nconnection: close\r\n\r\na,b\n1,2\n");
  let value = send(&http(), &server, None, Call::get("/x")).await.unwrap();
  assert_eq!(value, Value::String("a,b\n1,2\n".to_string()));

  // An empty success body is Null, the caller's "done, nothing to print".
  let (server, _) =
    canned_server("HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
  let value = send(&http(), &server, None, Call::get("/x")).await.unwrap();
  assert_eq!(value, Value::Null);
}

#[tokio::test]
async fn send_reads_a_redirect_as_the_authentication_error_it_is() {
  let (server, _) = canned_server(
    "HTTP/1.1 302 Found\r\nlocation: /aperio/login\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
  );
  let err = send(&http(), &server, None, Call::get("/x"))
    .await
    .unwrap_err();
  assert!(err.contains("authentication required"), "{err}");
  assert!(err.contains("--api-key"), "{err}");
}

#[tokio::test]
async fn send_surfaces_the_servers_own_words_on_an_error() {
  let (server, _) = canned_server(
    "HTTP/1.1 403 Forbidden\r\ncontent-length: 26\r\nconnection: close\r\n\r\nthat hostname is not yours",
  );
  let err = send(&http(), &server, None, Call::get("/x"))
    .await
    .unwrap_err();
  assert!(err.contains("403"), "{err}");
  assert!(err.contains("that hostname is not yours"), "{err}");

  // And a bodyless failure still names the status.
  let (server, _) =
    canned_server("HTTP/1.1 500 Oops\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
  let err = send(&http(), &server, None, Call::get("/x"))
    .await
    .unwrap_err();
  assert!(err.contains("server returned 500"), "{err}");

  // A body travels as JSON; prove it reached the wire.
  let (server, seen) =
    canned_server("HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}");
  let call = Call::post("/x", serde_json::json!({"topic": "deploy"}));
  send(&http(), &server, None, call).await.unwrap();
  let request = seen.lock().unwrap().join("");
  assert!(request.contains("\"topic\":\"deploy\""), "{request}");
}

#[test]
fn stdin_and_file_readers_answer_their_error_cases() {
  assert_eq!(read_maybe_stdin("plain-value").unwrap(), "plain-value");
  let err = read_json_file("/definitely/not/here.json").unwrap_err();
  assert!(err.contains("failed to read"), "{err}");
  let dir = std::env::temp_dir().join(format!("aperio-api-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  let bad = dir.join("not-json.json");
  std::fs::write(&bad, "{nope").unwrap();
  let err = read_json_file(bad.to_str().unwrap()).unwrap_err();
  assert!(err.contains("not valid JSON"), "{err}");
  let good = dir.join("good.json");
  std::fs::write(&good, "{\"a\":1}").unwrap();
  assert_eq!(read_json_file(good.to_str().unwrap()).unwrap()["a"], 1);
}
