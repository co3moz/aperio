//! Turning a parsed subcommand into the one HTTP call it means.
//!
//! One function, deliberately whole. It is a single `match` over every
//! subcommand, and each arm is two or three lines that name a method, a path
//! and a body. Split by command family it becomes a set of functions that each
//! return the same type and are dispatched by a second match, which is the
//! same match written twice.

use serde_json::json;

use super::*;

/// Translates one CLI command into the HTTP call that serves it.
pub(crate) fn build_call(
  command: &ApiCommand,
  settings: &ClientSettings,
  opts: &CommonOpts,
) -> Result<Call, String> {
  let scope = Scope::from_opts(opts);
  Ok(match command {
    ApiCommand::Expose(command) => build_expose_call(command)?,
    ApiCommand::Share(a) => {
      let mut body = Map::new();
      body.insert("hostname".into(), Value::String(scope.require_hostname()?));
      put_opt(&mut body, "path", scope.path());
      put_opt(&mut body, "ttl_seconds", ttl_field(&a.expire, false)?);
      Call::post("/aperio/api/share", Value::Object(body))
    }

    ApiCommand::Token(TokenCmd::List) => Call::get("/aperio/api/tokens"),
    ApiCommand::Token(TokenCmd::Create(a)) => {
      let mut body = Map::new();
      body.insert("name".into(), Value::String(a.name.clone()));
      body.insert("hostnames".into(), json!(scope.hostnames));
      body.insert("paths".into(), json!(scope.paths));
      body.insert("allowed_ips".into(), json!(a.allowed_ips));
      body.insert("allow_public".into(), Value::Bool(a.allow_public));
      body.insert("allow_otel".into(), Value::Bool(a.allow_otel));
      body.insert("allow_bind".into(), Value::Bool(a.allow_bind));
      body.insert("allow_server_side".into(), Value::Bool(a.allow_server_side));
      body.insert("topics".into(), json!(a.topics));
      body.insert("canary".into(), Value::Bool(a.canary));
      put_opt(&mut body, "ttl_seconds", ttl_field(&a.expire, true)?);
      put_opt(&mut body, "max_rps", a.max_rps);
      put_opt(&mut body, "daily_max_bytes", a.daily_max_bytes);
      Call::post("/aperio/api/tokens", Value::Object(body))
    }
    ApiCommand::Token(TokenCmd::Update(a)) => {
      let mut body = Map::new();
      put_opt(&mut body, "name", a.name.clone());
      if !scope.hostnames.is_empty() {
        body.insert("hostnames".into(), json!(scope.hostnames));
      }
      if !scope.paths.is_empty() {
        body.insert("paths".into(), json!(scope.paths));
      }
      if !a.allowed_ips.is_empty() {
        body.insert("allowed_ips".into(), json!(a.allowed_ips));
      }
      put_opt(&mut body, "ttl_seconds", ttl_field(&a.expire, false)?);
      put_opt(&mut body, "max_rps", a.max_rps);
      put_opt(&mut body, "daily_max_bytes", a.daily_max_bytes);
      if a.allow_otel || a.no_allow_otel {
        body.insert("allow_otel".into(), Value::Bool(a.allow_otel));
      }
      if a.allow_public || a.no_allow_public {
        body.insert("allow_public".into(), Value::Bool(a.allow_public));
      }
      if a.allow_bind || a.no_allow_bind {
        body.insert("allow_bind".into(), Value::Bool(a.allow_bind));
      }
      if a.allow_server_side || a.no_allow_server_side {
        body.insert("allow_server_side".into(), Value::Bool(a.allow_server_side));
      }
      // An empty list means "keep" on the wire, so clearing is its own flag.
      if a.clear_topics {
        body.insert("topics".into(), json!([]));
      } else if !a.topics.is_empty() {
        body.insert("topics".into(), json!(a.topics));
      }
      if a.canary || a.no_canary {
        body.insert("canary".into(), Value::Bool(a.canary));
      }
      Call::put(format!("/aperio/api/tokens/{}", a.id), Value::Object(body))
    }
    ApiCommand::Token(TokenCmd::Revoke { id }) => {
      Call::delete(format!("/aperio/api/tokens/{}", id))
    }
    ApiCommand::Token(TokenCmd::Rotate { id, grace }) => {
      let seconds = match grace {
        Some(raw) => parse_duration(raw)?,
        None => 0,
      };
      Call::post(
        format!("/aperio/api/tokens/{}/rotate", id),
        json!({ "grace_seconds": seconds }),
      )
    }
    ApiCommand::Token(TokenCmd::Refresh { secret }) => {
      // Authenticates with the token secret itself, not the admin key.
      let token = secret
        .clone()
        .or_else(|| settings.token.clone())
        .ok_or_else(|| {
          "a token secret is required (--secret, --server-token, or APERIO_SERVER_TOKEN)"
            .to_string()
        })?;
      Call::new(reqwest::Method::POST, "/aperio/api/tokens/refresh").with_auth(token)
    }

    ApiCommand::Tunnel(TunnelCmd::Create(a)) => {
      let mut body = Map::new();
      put_opt(&mut body, "name", a.name.clone());
      put_opt(&mut body, "hostname", scope.hostname());
      body.insert("allowed_ips".into(), json!(a.allowed_ips));
      put_opt(&mut body, "ttl_seconds", ttl_field(&a.expire, true)?);
      Call::post("/aperio/api/tunnels", Value::Object(body))
    }
    ApiCommand::Tunnel(TunnelCmd::Delete { id }) => {
      Call::delete(format!("/aperio/api/tunnels/{}", id))
    }

    ApiCommand::Maintenance(MaintenanceCmd::List) => Call::get("/aperio/api/maintenance"),
    ApiCommand::Maintenance(MaintenanceCmd::On {
      hostname,
      reason,
      ttl,
    }) => {
      let mut body = Map::new();
      body.insert("hostname".into(), json!(hostname));
      body.insert("enabled".into(), json!(true));
      put_opt(&mut body, "reason", reason.clone());
      if let Some(ttl) = ttl {
        body.insert("ttl_seconds".into(), json!(ttl));
      }
      Call::post("/aperio/api/maintenance", Value::Object(body))
    }
    ApiCommand::Maintenance(MaintenanceCmd::Off { hostname }) => Call::post(
      "/aperio/api/maintenance",
      json!({ "hostname": hostname, "enabled": false }),
    ),

    ApiCommand::Client(ClientCmd::Enable { id }) => Call::post(
      format!("/aperio/api/clients/{}/enabled", id),
      json!({ "enabled": true }),
    ),
    ApiCommand::Client(ClientCmd::Disable { id }) => Call::post(
      format!("/aperio/api/clients/{}/enabled", id),
      json!({ "enabled": false }),
    ),
    ApiCommand::Client(ClientCmd::Override(a)) => {
      // Each field fully replaces its override; an empty string clears it.
      let (hostname, path) = if a.clear {
        (Some(String::new()), Some(String::new()))
      } else {
        (scope.hostname(), scope.path())
      };
      let mut body = Map::new();
      put_opt(&mut body, "hostname_bind", hostname);
      put_opt(&mut body, "path_bind", path);
      Call::post(
        format!("/aperio/api/clients/{}/override", a.id),
        Value::Object(body),
      )
    }
    ApiCommand::Client(ClientCmd::Config { id }) => {
      Call::get(format!("/aperio/api/clients/{}/config", id))
    }

    ApiCommand::Cache(CacheCmd::Stats) => Call::get("/aperio/api/cache/stats"),
    ApiCommand::Cache(CacheCmd::Purge(a)) => {
      let mut body = Map::new();
      put_opt(&mut body, "hostname", scope.hostname());
      put_opt(&mut body, "path_prefix", a.path_prefix.clone());
      put_opt(&mut body, "surrogate_key", a.surrogate_key.clone());
      Call::post("/aperio/api/cache/purge", Value::Object(body))
    }

    ApiCommand::Webhook(WebhookCmd::List) => Call::get("/aperio/api/webhooks"),
    ApiCommand::Webhook(WebhookCmd::Create(a)) => {
      let mut body = Map::new();
      body.insert("name".into(), Value::String(a.name.clone()));
      body.insert("url".into(), Value::String(a.url.clone()));
      body.insert("events".into(), json!(a.events));
      put_opt(&mut body, "secret", a.secret.clone());
      put_opt(&mut body, "format", a.format.clone());
      Call::post("/aperio/api/webhooks", Value::Object(body))
    }
    ApiCommand::Webhook(WebhookCmd::Delete { id }) => {
      Call::delete(format!("/aperio/api/webhooks/{}", id))
    }
    ApiCommand::Webhook(WebhookCmd::Deliveries { webhook_id, limit }) => {
      Call::get("/aperio/api/webhooks/deliveries")
        .query("webhook_id", webhook_id.clone())
        .query("limit", *limit)
    }
    ApiCommand::Webhook(WebhookCmd::Redeliver { id }) => Call::post(
      format!("/aperio/api/webhooks/deliveries/{}/redeliver", id),
      Value::Null,
    ),
    ApiCommand::Webhook(WebhookCmd::Test { id }) => {
      Call::post(format!("/aperio/api/webhooks/{}/test", id), Value::Null)
    }

    ApiCommand::Inbox(InboxCmd::List) => Call::get("/aperio/api/inbox"),
    ApiCommand::Inbox(InboxCmd::Show { id }) => Call::get(format!("/aperio/api/inbox/{}", id)),
    ApiCommand::Inbox(InboxCmd::Refire { id }) => {
      Call::post(format!("/aperio/api/inbox/{}/refire", id), Value::Null)
    }
    ApiCommand::Inbox(InboxCmd::Delete { id }) => Call::delete(format!("/aperio/api/inbox/{}", id)),
    ApiCommand::Inbox(InboxCmd::Clear) => Call::delete("/aperio/api/inbox"),

    ApiCommand::User(UserCmd::List) => Call::get("/aperio/api/users"),
    ApiCommand::User(UserCmd::Create(a)) => {
      let mut body = Map::new();
      body.insert("username".into(), Value::String(a.username.clone()));
      if let Some(password) = &a.password {
        body.insert(
          "password".into(),
          Value::String(read_maybe_stdin(password)?),
        );
      }
      put_opt(&mut body, "role", a.role.clone());
      if !a.grants.is_empty() {
        body.insert("grants".into(), Value::Array(parse_grants(&a.grants)?));
      }
      Call::post("/aperio/api/users", Value::Object(body))
    }
    ApiCommand::User(UserCmd::Update(a)) => {
      let mut body = Map::new();
      put_opt(&mut body, "role", a.role.clone());
      if !a.grants.is_empty() {
        body.insert("grants".into(), Value::Array(parse_grants(&a.grants)?));
      }
      if a.enable || a.disable {
        body.insert("enabled".into(), Value::Bool(a.enable));
      }
      if let Some(raw) = &a.password {
        body.insert("password".into(), Value::String(read_maybe_stdin(raw)?));
      }
      Call::put(format!("/aperio/api/users/{}", a.id), Value::Object(body))
    }
    ApiCommand::User(UserCmd::Delete { id }) => Call::delete(format!("/aperio/api/users/{}", id)),
    ApiCommand::User(UserCmd::Sessions) => Call::get("/aperio/api/sessions"),
    ApiCommand::User(UserCmd::Revoke { id, all }) => match (id, all) {
      (Some(id), _) => Call::delete(format!("/aperio/api/sessions/{}", id)),
      (None, true) => Call::delete("/aperio/api/sessions"),
      (None, false) => return Err("pass a session id, or --all to revoke every session".into()),
    },
    ApiCommand::User(UserCmd::ResetTotp { id }) => {
      Call::delete(format!("/aperio/api/users/{}/totp", id))
    }

    ApiCommand::Org(OrgCmd::List) => Call::get("/aperio/api/orgs"),
    ApiCommand::Org(OrgCmd::Create {
      name,
      panel_hostname,
    }) => Call::post(
      "/aperio/api/orgs",
      json!({ "name": name, "hostnames": scope.hostnames, "panel_hostname": panel_hostname }),
    ),
    ApiCommand::Org(OrgCmd::Panel { id, hostname }) => Call::put(
      format!("/aperio/api/orgs/{}/panel", id),
      json!({ "hostname": hostname.clone().unwrap_or_default() }),
    ),
    ApiCommand::Org(OrgCmd::Hostnames { id }) => Call::put(
      format!("/aperio/api/orgs/{}/hostnames", id),
      json!({ "hostnames": scope.hostnames }),
    ),
    ApiCommand::Org(OrgCmd::Delete { id }) => Call::delete(format!("/aperio/api/orgs/{}", id)),
    ApiCommand::Org(OrgCmd::Quota(a)) => {
      let mut body = Map::new();
      put_opt(&mut body, "max_clients", a.max_clients);
      put_opt(&mut body, "max_tokens", a.max_tokens);
      put_opt(&mut body, "max_users", a.max_users);
      put_opt(&mut body, "max_bytes_month", a.max_bytes_month);
      Call::put(
        format!("/aperio/api/orgs/{}/quota", a.id),
        Value::Object(body),
      )
    }
    ApiCommand::Org(OrgCmd::CustomName { id, name }) => Call::put(
      format!("/aperio/api/orgs/{}/custom-name", id),
      json!({ "custom_name": name }),
    ),
    ApiCommand::Org(OrgCmd::Oidc(a)) => Call::put(
      format!("/aperio/api/orgs/{}/oidc", a.id),
      json!({
        "issuer": a.issuer.clone().unwrap_or_default(),
        "client_id": a.client_id.clone().unwrap_or_default(),
        "client_secret": a.client_secret.clone().unwrap_or_default(),
        "allowed_emails": a.allowed_emails.clone(),
        "default_role": a.default_role.clone(),
        "group_grants": a.group_grants.clone(),
      }),
    ),
    ApiCommand::Org(OrgCmd::Usage { id }) => Call::get(format!("/aperio/api/orgs/{}/usage", id)),
    ApiCommand::Org(OrgCmd::Select { id }) => {
      Call::post("/aperio/api/orgs/select", json!({ "id": id }))
    }

    ApiCommand::AdminKey(AdminKeyCmd::List) => Call::get("/aperio/api/admin-keys"),
    ApiCommand::AdminKey(AdminKeyCmd::Create(a)) => {
      let mut body = Map::new();
      body.insert("name".into(), Value::String(a.name.clone()));
      body.insert("role".into(), Value::String(a.role.clone()));
      if !a.expose.is_empty() {
        body.insert("expose".into(), json!(a.expose));
      }
      put_opt(&mut body, "org_id", a.org_id.clone());
      put_opt(&mut body, "ttl_seconds", ttl_field(&a.expire, true)?);
      Call::post("/aperio/api/admin-keys", Value::Object(body))
    }
    ApiCommand::AdminKey(AdminKeyCmd::Revoke { id }) => {
      Call::delete(format!("/aperio/api/admin-keys/{}", id))
    }

    ApiCommand::Scaling(ScalingCmd::List) => Call::get("/aperio/api/scaling"),
    ApiCommand::Scaling(ScalingCmd::Disarm { id }) => {
      Call::delete(format!("/aperio/api/scaling/{}", id))
    }

    ApiCommand::Request(RequestCmd::Show { id }) => {
      Call::get(format!("/aperio/api/requests/{}", id))
    }
    ApiCommand::Request(RequestCmd::Replay { id }) => {
      Call::post(format!("/aperio/api/requests/{}/replay", id), Value::Null)
    }

    ApiCommand::Settings(SettingsCmd::Get) => Call::get("/aperio/api/settings"),
    ApiCommand::Settings(SettingsCmd::Set(a)) => {
      Call::put("/aperio/api/settings", read_json_file(&a.file)?)
    }

    ApiCommand::Audit(AuditCmd::List) => Call::get("/aperio/api/audit"),
    ApiCommand::Audit(AuditCmd::Verify) => Call::get("/aperio/api/audit/verify"),

    ApiCommand::Publish(a) => {
      let mut body = Map::new();
      body.insert("topic".into(), json!(a.topic));
      body.insert("qos".into(), json!(a.qos));
      if let Some(text) = &a.payload {
        body.insert("payload".into(), json!(text));
      }
      if let Some(b64) = &a.payload_base64 {
        body.insert("payload_base64".into(), json!(b64));
      }
      Call::post("/aperio/api/publish", Value::Object(body))
    }
    ApiCommand::Subscribers => Call::get("/aperio/api/subscribers"),
    ApiCommand::Explain(a) => Call::get("/aperio/api/explain")
      .query("hostname", Some(a.hostname.clone()))
      .query("path", a.path.clone())
      .query("method", a.method.clone()),
    ApiCommand::Schema { kind } => Call::get(format!("/aperio/api/config/schema/{}", kind)),
    ApiCommand::EdgeTraefik => Call::get("/aperio/api/edge/traefik"),
    ApiCommand::EdgeAsk { hostname } => {
      Call::get("/aperio/api/edge/ask").query("hostname", Some(hostname.clone()))
    }
    ApiCommand::Activity => Call::get("/aperio/api/activity"),
    ApiCommand::AuditCsv => Call::get("/aperio/api/export/audit.csv"),
    ApiCommand::Stats => Call::get("/aperio/api/stats"),
    ApiCommand::History(a) => Call::get("/aperio/api/stats/history")
      .query("unit", a.unit.clone())
      .query("count", a.count)
      .query("from", a.from.clone())
      .query("to", a.to.clone()),
    ApiCommand::Uptime => Call::get("/aperio/api/uptime"),
    ApiCommand::Logs => Call::get("/aperio/api/logs"),
    ApiCommand::Topology => Call::get("/aperio/api/topology"),
    ApiCommand::SlowEndpoints => Call::get("/aperio/api/slow-endpoints"),
    ApiCommand::Bandwidth(a) => Call::get("/aperio/api/bandwidth")
      .query("unit", a.unit.clone())
      .query("count", a.count),
    ApiCommand::RouteTrends => Call::get("/aperio/api/route-trends"),
    ApiCommand::StageStats => Call::get("/aperio/api/stage-stats"),
    ApiCommand::SelfHealth => Call::get("/aperio/api/self-health"),
    ApiCommand::Health => Call::get("/aperio/health"),
    ApiCommand::TrafficCsv(a) => Call::get("/aperio/api/export/traffic.csv")
      .query("unit", a.unit.clone())
      .query("count", a.count),
    ApiCommand::Purge(a) => {
      let mut body = Map::new();
      put_opt(&mut body, "hostname", scope.hostname());
      put_opt(&mut body, "token", a.token_name.clone());
      put_opt(&mut body, "ip", a.ip.clone());
      if body.is_empty() {
        return Err("pass at least one of --hostname, --token-name, or --ip".into());
      }
      Call::post("/aperio/api/purge", Value::Object(body))
    }
    ApiCommand::Export(a) => Call::get("/aperio/api/export").query("include", a.include.clone()),
    ApiCommand::Import(a) => Call::post("/aperio/api/import", read_json_file(&a.file)?),
    ApiCommand::Openapi => Call::get("/aperio/api/openapi.json"),
  })
}

/// `--grant <org>:<role>` into the API's `{org, role}` objects. The role is
/// the last segment, so an organization id may not contain a colon, which a
/// handle cannot anyway.
fn parse_grants(raw: &[String]) -> Result<Vec<Value>, String> {
  raw
    .iter()
    .map(|entry| {
      let (org, role) = entry
        .rsplit_once(':')
        .ok_or_else(|| format!("--grant takes <org>:<role>, got {entry:?}"))?;
      if org.trim().is_empty() || role.trim().is_empty() {
        return Err(format!("--grant takes <org>:<role>, got {entry:?}"));
      }
      let mut parts = role.trim().split('+');
      let role = parts.next().unwrap_or_default();
      if !matches!(role, "viewer" | "operator" | "admin") {
        return Err(format!("unknown grant role {role:?}"));
      }
      let mut actions = Vec::new();
      for part in parts {
        let action = part
          .strip_prefix("expose.")
          .ok_or_else(|| format!("unknown grant capability {part:?}"))?;
        if !aperio_config::expose::ExposeAction::ALL
          .iter()
          .any(|a| a.as_str() == action)
        {
          return Err(format!("unknown expose capability {action:?}"));
        }
        if !actions.contains(&action) {
          actions.push(action);
        }
      }
      let mut grant = json!({ "org": org.trim(), "role": role });
      if !actions.is_empty() {
        grant["expose"] = json!(actions);
      }
      Ok(grant)
    })
    .collect()
}

fn expose_path(id: &str) -> String {
  let mut url = url::Url::parse("http://localhost/aperio/api/exposes").expect("fixed URL");
  url.path_segments_mut().expect("hierarchical URL").push(id);
  url.path().to_string()
}

fn expose_spec(file: &str) -> Result<Value, String> {
  let spec: aperio_config::expose::ExposeSpec = serde_json::from_value(read_json_file(file)?)
    .map_err(|e| format!("invalid expose specification: {e}"))?;
  let errors = spec.validate();
  if !errors.is_empty() {
    return Err(
      errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; "),
    );
  }
  serde_json::to_value(spec).map_err(|e| e.to_string())
}

fn build_expose_call(command: &ExposeCmd) -> Result<Call, String> {
  Ok(match command {
    ExposeCmd::List {
      org,
      protocol,
      search,
      offset,
      limit,
    } => Call::get("/aperio/api/exposes")
      .query("org", org.as_deref())
      .query("protocol", protocol.as_deref())
      .query("search", search.as_deref())
      .query("offset", Some(offset))
      .query("limit", Some(limit)),
    ExposeCmd::Show { id } => Call::get(expose_path(id)),
    ExposeCmd::Create { file, id } => Call::post(
      "/aperio/api/exposes",
      json!({"id": id, "spec": expose_spec(file)?}),
    ),
    ExposeCmd::Update { id, revision, file } => Call::put(
      expose_path(id),
      json!({"revision": revision, "spec": expose_spec(file)?}),
    ),
    ExposeCmd::Enable(a) | ExposeCmd::Disable(a) | ExposeCmd::Retry(a) => {
      let action = match command {
        ExposeCmd::Enable(_) => "enable",
        ExposeCmd::Disable(_) => "disable",
        _ => "retry",
      };
      Call::post(
        format!("{}/actions", expose_path(&a.id)),
        json!({"revision": a.revision, "action": action}),
      )
    }
    ExposeCmd::Delete(a) => {
      Call::delete(expose_path(&a.id)).with_body(json!({"revision": a.revision}))
    }
    ExposeCmd::Drain {
      id,
      revision,
      seconds,
    } => Call::post(
      format!("{}/actions", expose_path(id)),
      json!({"revision": revision, "action": "drain", "drain_seconds": seconds}),
    ),
    ExposeCmd::Sessions {
      id,
      history,
      offset,
      limit,
    } => Call::get(format!("{}/sessions", expose_path(id)))
      .query("history", Some(history))
      .query("offset", Some(offset))
      .query("limit", Some(limit)),
    ExposeCmd::Disconnect {
      id,
      revision,
      session,
    } => Call::post(
      format!("{}/actions", expose_path(id)),
      json!({"revision": revision, "action": "disconnect", "session_id": session}),
    ),
    ExposeCmd::Export => Call::get("/aperio/api/exposes/export"),
    ExposeCmd::Import {
      file,
      apply,
      revision,
      org_map,
      omit_policies,
    } => {
      let mut backup = read_json_file(file)?;
      if *omit_policies {
        backup["policies"] = json!([]);
      }
      let mut mapping = Map::new();
      for pair in org_map {
        let (old, new) = pair
          .split_once('=')
          .filter(|(old, new)| !old.is_empty() && !new.is_empty())
          .ok_or_else(|| format!("--org-map requires OLD=NEW, got {pair:?}"))?;
        if mapping.insert(old.into(), json!(new)).is_some() {
          return Err(format!("duplicate --org-map source {old:?}"));
        }
      }
      Call::post(
        "/aperio/api/exposes/import",
        json!({"backup": backup, "preview": !apply, "revision": revision, "org_map": mapping}),
      )
    }
    ExposeCmd::Policies => Call::get("/aperio/api/exposes/policies"),
    ExposeCmd::SetPolicy { file } => {
      let policy: aperio_config::expose_policy::ExposePolicy =
        serde_json::from_value(read_json_file(file)?)
          .map_err(|e| format!("invalid expose policy: {e}"))?;
      let errors = policy.validate();
      if !errors.is_empty() {
        return Err(
          errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "),
        );
      }
      let mut url =
        url::Url::parse("http://localhost/aperio/api/exposes/policies/").expect("fixed URL");
      url
        .path_segments_mut()
        .expect("hierarchical URL")
        .pop_if_empty()
        .push(&policy.org_id);
      Call::put(
        url.path(),
        serde_json::to_value(policy).map_err(|e| e.to_string())?,
      )
    }
  })
}
