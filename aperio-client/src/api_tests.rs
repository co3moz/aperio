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
