//! `aperio-client api ...`: command-line access to the Aperio server's admin
//! API: the same operations the dashboard performs (share links, dynamic
//! tokens, ephemeral tunnels, maintenance mode, cache/webhooks/users/orgs,
//! …), so they can be scripted from CI without a browser session.
//!
//! Authentication uses a programmatic admin key (`--api-key`, yaml
//! `server.api_key`, env `APERIO_API_KEY`) sent as `Authorization: Bearer`.
//! The tunnel token (`--server-token`) is accepted as a fallback credential;
//! the server only honours it on the endpoints that take the master token
//! (`api tunnel create|delete`), so everything else needs an admin key.
//!
//! Every command prints the server's JSON response (pretty-printed) on
//! stdout and exits 0; a transport error or a non-2xx status prints to
//! stderr and exits 1, so shell scripts can branch on the exit code.

use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};
use std::time::Duration;

use crate::config::{ClientSettings, CommonOpts, build_http_url};

/// Parses a human duration into seconds: `45s`, `30m`, `2h`, `1d`, `2w`, a
/// bare number (seconds), or `never` / `none` / `0` for "no expiry" (0).
pub(crate) fn parse_duration(raw: &str) -> Result<u64, String> {
  let value = raw.trim().to_ascii_lowercase();
  if value.is_empty() {
    return Err("empty duration".to_string());
  }
  if matches!(value.as_str(), "never" | "none" | "infinite" | "0") {
    return Ok(0);
  }
  let (number, multiplier) = match value.chars().last() {
    Some('s') => (&value[..value.len() - 1], 1u64),
    Some('m') => (&value[..value.len() - 1], 60),
    Some('h') => (&value[..value.len() - 1], 3_600),
    Some('d') => (&value[..value.len() - 1], 86_400),
    Some('w') => (&value[..value.len() - 1], 604_800),
    _ => (value.as_str(), 1),
  };
  let parsed: u64 = number
    .parse()
    .map_err(|_| format!("invalid duration '{}' (use 30m, 2h, 1d, 1w, or never)", raw))?;
  parsed
    .checked_mul(multiplier)
    .ok_or_else(|| format!("duration '{}' is too large", raw))
}

// --- CLI surface -----------------------------------------------------------

/// `aperio-client api <group> <action>`.
#[derive(Subcommand)]
pub(crate) enum ApiCommand {
  /// Mint a signed share link for an auth-protected host/path
  Share(ShareArgs),
  /// Dynamic tunnel tokens
  #[command(subcommand)]
  Token(TokenCmd),
  /// Ephemeral tunnels (short-lived token + hostname)
  #[command(subcommand)]
  Tunnel(TunnelCmd),
  /// Maintenance mode per hostname
  #[command(subcommand)]
  Maintenance(MaintenanceCmd),
  /// Connected clients: kill switch and bind overrides
  #[command(subcommand)]
  Client(ClientCmd),
  /// Response cache statistics and purging
  #[command(subcommand)]
  Cache(CacheCmd),
  /// Webhook definitions and deliveries
  #[command(subcommand)]
  Webhook(WebhookCmd),
  /// Inbound webhook inbox
  #[command(subcommand)]
  Inbox(InboxCmd),
  /// Dashboard users
  #[command(subcommand)]
  User(UserCmd),
  /// Organizations (master super-admin only)
  #[command(subcommand)]
  Org(OrgCmd),
  /// Programmatic admin API keys
  #[command(subcommand, name = "admin-key")]
  AdminKey(AdminKeyCmd),
  /// Captured requests (inspector)
  #[command(subcommand)]
  Request(RequestCmd),
  /// Server settings overrides
  #[command(subcommand)]
  Settings(SettingsCmd),
  /// Audit trail
  #[command(subcommand)]
  Audit(AuditCmd),
  /// Live server statistics snapshot
  Stats,
  /// Traffic history buckets
  History(HistoryArgs),
  /// Per-client uptime ratios
  Uptime,
  /// The most recent proxied requests
  Logs,
  /// Client/route topology
  Topology,
  /// Slowest endpoints by latency
  #[command(name = "slow-endpoints")]
  SlowEndpoints,
  /// Bytes in/out per token and hostname
  Bandwidth(BandwidthArgs),
  /// Per-route traffic trends
  #[command(name = "route-trends")]
  RouteTrends,
  /// Per-stage latency breakdown
  #[command(name = "stage-stats")]
  StageStats,
  /// The server's own health report
  #[command(name = "self-health")]
  SelfHealth,
  /// Liveness probe (needs no credential)
  Health,
  /// Traffic history as CSV
  #[command(name = "traffic-csv")]
  TrafficCsv(HistoryArgs),
  /// Erase stored records for a hostname, token label, or visitor IP
  Purge(PurgeArgs),
  /// Dump tokens, webhooks, users, and orgs as JSON
  Export,
  /// Apply a dump created by `api export`
  Import(FileArgs),
  /// Fetch the OpenAPI document describing this server's API
  Openapi,
}

#[derive(Args)]
pub(crate) struct ShareArgs {
  /// Lifetime: 30m, 2h, 1d, 1w, or never (default: the server's 3 days)
  #[arg(long, value_name = "DURATION")]
  expire: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum TokenCmd {
  /// List the tokens of the caller's organization
  List,
  /// Create a token; the secret is printed exactly once
  Create(TokenCreateArgs),
  /// Change an existing token's scope without touching its secret
  Update(TokenUpdateArgs),
  /// Revoke a token
  Revoke {
    /// Token record id
    id: String,
  },
  /// Rotate a token's secret, optionally keeping the old one alive
  Rotate {
    /// Token record id
    id: String,
    /// How long the old secret stays accepted (default: immediate cutover)
    #[arg(long, value_name = "DURATION")]
    grace: Option<String>,
  },
  /// Refresh a short-lived token using the token secret itself
  Refresh {
    /// The token secret to refresh (defaults to --server-token)
    #[arg(long, value_name = "TOKEN")]
    secret: Option<String>,
  },
}

#[derive(Args)]
pub(crate) struct TokenCreateArgs {
  /// Label for the token
  #[arg(long, value_name = "NAME")]
  name: String,
  /// Source IP/CIDR allowed to connect; repeatable, omitted = any
  #[arg(long = "allowed-ip", value_name = "IP_OR_CIDR")]
  allowed_ips: Vec<String>,
  /// Lifetime: 30m, 2h, 1d, or never (default)
  #[arg(long, value_name = "DURATION")]
  expire: Option<String>,
  /// Request rate limit for traffic served through this token
  #[arg(long = "max-rps", value_name = "REQ_PER_SEC")]
  max_rps: Option<f64>,
  /// Daily byte quota (request + response payload)
  #[arg(long = "daily-max-bytes", value_name = "BYTES")]
  daily_max_bytes: Option<u64>,
  /// Allow clients using this token to publish services as public
  #[arg(long = "allow-public")]
  allow_public: bool,
  /// Mark the token as a canary: any successful auth raises an alert
  #[arg(long)]
  canary: bool,
}

#[derive(Args)]
pub(crate) struct TokenUpdateArgs {
  /// Token record id
  id: String,
  /// New label
  #[arg(long, value_name = "NAME")]
  name: Option<String>,
  /// Replacement source IP permission; repeatable
  #[arg(long = "allowed-ip", value_name = "IP_OR_CIDR")]
  allowed_ips: Vec<String>,
  /// New lifetime from now, or `never` to clear the expiry
  #[arg(long, value_name = "DURATION")]
  expire: Option<String>,
  /// New rate limit; 0 clears it
  #[arg(long = "max-rps", value_name = "REQ_PER_SEC")]
  max_rps: Option<f64>,
  /// New daily byte quota; 0 clears it
  #[arg(long = "daily-max-bytes", value_name = "BYTES")]
  daily_max_bytes: Option<u64>,
  /// Permit publishing public services
  #[arg(long = "allow-public")]
  allow_public: bool,
  /// Forbid publishing public services
  #[arg(long = "no-allow-public", conflicts_with = "allow_public")]
  no_allow_public: bool,
  /// Turn the canary flag on
  #[arg(long)]
  canary: bool,
  /// Turn the canary flag off
  #[arg(long = "no-canary", conflicts_with = "canary")]
  no_canary: bool,
}

#[derive(Subcommand)]
pub(crate) enum TunnelCmd {
  /// Provision an ephemeral tunnel: a scoped short-lived token + hostname
  Create(TunnelCreateArgs),
  /// Delete an ephemeral tunnel by its token id
  Delete {
    /// Tunnel (token) id returned by `api tunnel create`
    id: String,
  },
}

#[derive(Args)]
pub(crate) struct TunnelCreateArgs {
  /// Label for the minted token
  #[arg(long, value_name = "NAME")]
  name: Option<String>,
  /// Source IP/CIDR allowed to connect; repeatable
  #[arg(long = "allowed-ip", value_name = "IP_OR_CIDR")]
  allowed_ips: Vec<String>,
  /// Lifetime: 30m, 2h, 1d (default 1h, max 7d)
  #[arg(long, value_name = "DURATION")]
  expire: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum MaintenanceCmd {
  /// List the hostnames currently in maintenance mode
  List,
  /// Turn maintenance mode on for a hostname (`*` = every hostname)
  On {
    /// Hostname, or `*`
    hostname: String,
  },
  /// Turn maintenance mode off for a hostname (`*` = every hostname)
  Off {
    /// Hostname, or `*`
    hostname: String,
  },
}

#[derive(Subcommand)]
pub(crate) enum ClientCmd {
  /// Put a connected client back into the routing pool
  Enable {
    /// Client connection id
    id: String,
  },
  /// Remove a connected client from the routing pool (kill switch)
  Disable {
    /// Client connection id
    id: String,
  },
  /// Overrule a client's hostname/path bind server-side
  Override(ClientOverrideArgs),
}

#[derive(Args)]
pub(crate) struct ClientOverrideArgs {
  /// Client connection id
  id: String,
  /// Clear both overrides (otherwise --hostname / --path set them)
  #[arg(long)]
  clear: bool,
}

#[derive(Subcommand)]
pub(crate) enum CacheCmd {
  /// Cache occupancy and hit-rate statistics
  Stats,
  /// Drop cached entries (no filter = the whole cache)
  Purge(CachePurgeArgs),
}

#[derive(Args)]
pub(crate) struct CachePurgeArgs {
  /// Only entries whose URI starts with this prefix
  #[arg(long = "path-prefix", value_name = "PREFIX")]
  path_prefix: Option<String>,
  /// Only entries tagged with this backend Surrogate-Key
  #[arg(long = "surrogate-key", value_name = "KEY")]
  surrogate_key: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum WebhookCmd {
  /// List webhook definitions
  List,
  /// Create a webhook
  Create(WebhookCreateArgs),
  /// Delete a webhook
  Delete {
    /// Webhook id
    id: String,
  },
  /// Recent delivery outcomes
  Deliveries {
    /// Only this webhook's deliveries
    #[arg(long = "webhook-id", value_name = "ID")]
    webhook_id: Option<String>,
    /// Rows to return (default 50, max 200)
    #[arg(long, value_name = "N")]
    limit: Option<usize>,
  },
  /// Re-send one delivery
  Redeliver {
    /// Delivery id
    id: String,
  },
}

#[derive(Args)]
pub(crate) struct WebhookCreateArgs {
  /// Label for the webhook
  #[arg(long, value_name = "NAME")]
  name: String,
  /// Destination URL
  #[arg(long, value_name = "URL")]
  url: String,
  /// Event to subscribe to; repeatable, omitted = all events
  #[arg(long = "event", value_name = "EVENT")]
  events: Vec<String>,
  /// HMAC signing secret for the delivery signature header
  #[arg(long, value_name = "SECRET")]
  secret: Option<String>,
  /// Payload format: generic (default), slack, discord, or teams
  #[arg(long, value_name = "FORMAT")]
  format: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum InboxCmd {
  /// List inbox entries (payloads omitted)
  List,
  /// Show one entry with headers and body
  Show {
    /// Inbox entry id
    id: String,
  },
  /// Re-deliver one entry to its service
  Refire {
    /// Inbox entry id
    id: String,
  },
  /// Delete one entry
  Delete {
    /// Inbox entry id
    id: String,
  },
  /// Delete every entry
  Clear,
}

#[derive(Subcommand)]
pub(crate) enum UserCmd {
  /// List dashboard users
  List,
  /// Create a dashboard user
  Create(UserCreateArgs),
  /// Update a user's role, state, or password
  Update(UserUpdateArgs),
  /// Delete a user
  Delete {
    /// User record id
    id: String,
  },
  /// List active dashboard sessions
  Sessions,
  /// Revoke one session, or every session with --all
  Revoke {
    /// Session id (omit with --all)
    id: Option<String>,
    /// Sign every user out everywhere
    #[arg(long, conflicts_with = "id")]
    all: bool,
  },
  /// Reset a user's TOTP enrollment
  #[command(name = "reset-totp")]
  ResetTotp {
    /// User record id
    id: String,
  },
}

#[derive(Args)]
pub(crate) struct UserCreateArgs {
  /// Login name
  #[arg(long, value_name = "USERNAME")]
  username: String,
  /// Password, at least 8 characters; `-` reads it from stdin
  #[arg(long, value_name = "PASSWORD")]
  password: String,
  /// Role: viewer, operator, or admin
  #[arg(long, value_name = "ROLE")]
  role: String,
}

#[derive(Args)]
pub(crate) struct UserUpdateArgs {
  /// User record id
  id: String,
  /// New role: viewer, operator, or admin
  #[arg(long, value_name = "ROLE")]
  role: Option<String>,
  /// Enable the account
  #[arg(long)]
  enable: bool,
  /// Disable the account
  #[arg(long, conflicts_with = "enable")]
  disable: bool,
  /// New password, at least 8 characters; `-` reads it from stdin
  #[arg(long, value_name = "PASSWORD")]
  password: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum OrgCmd {
  /// List organizations
  List,
  /// Create an organization; --hostname fences which hostnames it may claim
  Create {
    /// Organization name
    #[arg(long, value_name = "NAME")]
    name: String,
  },
  /// Replace an organization's hostname allowlist (no --hostname clears it)
  Hostnames {
    /// Organization id
    id: String,
  },
  /// Delete an organization
  Delete {
    /// Organization id
    id: String,
  },
  /// Set an organization's quotas (0 clears one)
  Quota(OrgQuotaArgs),
  /// Show an organization's resource usage
  Usage {
    /// Organization id
    id: String,
  },
  /// Switch the master super-admin's active organization
  Select {
    /// Organization id; omit for the master organization
    id: Option<String>,
  },
}

#[derive(Args)]
pub(crate) struct OrgQuotaArgs {
  /// Organization id
  id: String,
  /// Maximum connected clients; 0 clears the quota
  #[arg(long = "max-clients", value_name = "N")]
  max_clients: Option<u64>,
  /// Maximum tokens; 0 clears the quota
  #[arg(long = "max-tokens", value_name = "N")]
  max_tokens: Option<u64>,
  /// Maximum users; 0 clears the quota
  #[arg(long = "max-users", value_name = "N")]
  max_users: Option<u64>,
  /// Monthly byte budget; 0 clears the quota
  #[arg(long = "max-bytes-month", value_name = "BYTES")]
  max_bytes_month: Option<u64>,
}

#[derive(Subcommand)]
pub(crate) enum AdminKeyCmd {
  /// List admin keys
  List,
  /// Create an admin key; the secret is printed exactly once
  Create(AdminKeyCreateArgs),
  /// Revoke an admin key
  Revoke {
    /// Admin key id
    id: String,
  },
}

#[derive(Args)]
pub(crate) struct AdminKeyCreateArgs {
  /// Label for the key
  #[arg(long, value_name = "NAME")]
  name: String,
  /// Role the key authenticates as: viewer, operator, or admin
  #[arg(long, value_name = "ROLE")]
  role: String,
  /// Organization the key acts within; omitted = master
  #[arg(long = "org", value_name = "ORG_ID")]
  org_id: Option<String>,
  /// Lifetime: 30m, 2h, 1d, or never (default)
  #[arg(long, value_name = "DURATION")]
  expire: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum RequestCmd {
  /// Show a captured request with its headers and body
  Show {
    /// Request id from the traffic log
    id: String,
  },
  /// Replay a captured request through the tunnel
  Replay {
    /// Request id from the traffic log
    id: String,
  },
}

#[derive(Subcommand)]
pub(crate) enum SettingsCmd {
  /// Show the current settings overrides
  Get,
  /// Replace the settings overrides with a JSON document
  Set(FileArgs),
}

#[derive(Subcommand)]
pub(crate) enum AuditCmd {
  /// Recent audit events for the caller's organization
  List,
  /// Verify the audit log's tamper-evident hash chain
  Verify,
}

/// A JSON document read from a file, or from stdin when the path is `-`.
#[derive(Args)]
pub(crate) struct FileArgs {
  /// Path to a JSON file; `-` reads stdin
  #[arg(long, value_name = "FILE")]
  file: String,
}

#[derive(Args)]
pub(crate) struct HistoryArgs {
  /// Bucket unit: day (default), week, month, or year
  #[arg(long, value_name = "UNIT")]
  unit: Option<String>,
  /// Number of buckets, newest last
  #[arg(long, value_name = "N")]
  count: Option<usize>,
  /// Custom range start, YYYY-MM-DD (day buckets)
  #[arg(long, value_name = "DATE")]
  from: Option<String>,
  /// Custom range end, YYYY-MM-DD
  #[arg(long, value_name = "DATE")]
  to: Option<String>,
}

#[derive(Args)]
pub(crate) struct BandwidthArgs {
  /// Bucket granularity: day (default) or month
  #[arg(long, value_name = "UNIT")]
  unit: Option<String>,
  /// Buckets to return (max 62)
  #[arg(long, value_name = "N")]
  count: Option<usize>,
}

#[derive(Args)]
pub(crate) struct PurgeArgs {
  /// Token label whose aggregate records should be erased
  #[arg(long = "token-name", value_name = "NAME")]
  token_name: Option<String>,
  /// Visitor IP whose inspector captures should be erased
  #[arg(long, value_name = "IP")]
  ip: Option<String>,
}

// --- HTTP plumbing ---------------------------------------------------------

/// A prepared call: method, path under the server root, query pairs, body.
struct Call {
  method: reqwest::Method,
  path: String,
  query: Vec<(String, String)>,
  body: Option<Value>,
  /// Bearer credential for this call when it differs from the configured
  /// admin key (token self-refresh presents the token secret itself).
  auth: Option<String>,
}

impl Call {
  fn new(method: reqwest::Method, path: impl Into<String>) -> Self {
    Self {
      method,
      path: path.into(),
      query: Vec::new(),
      body: None,
      auth: None,
    }
  }
  fn get(path: impl Into<String>) -> Self {
    Self::new(reqwest::Method::GET, path)
  }
  fn post(path: impl Into<String>, body: Value) -> Self {
    Self::new(reqwest::Method::POST, path).with_body(body)
  }
  fn put(path: impl Into<String>, body: Value) -> Self {
    Self::new(reqwest::Method::PUT, path).with_body(body)
  }
  fn delete(path: impl Into<String>) -> Self {
    Self::new(reqwest::Method::DELETE, path)
  }
  fn with_body(mut self, body: Value) -> Self {
    self.body = Some(body);
    self
  }
  fn with_auth(mut self, secret: impl Into<String>) -> Self {
    self.auth = Some(secret.into());
    self
  }
  fn query(mut self, key: &str, value: Option<impl ToString>) -> Self {
    if let Some(v) = value {
      self.query.push((key.to_string(), v.to_string()));
    }
    self
  }
}

/// Inserts `key` into a JSON object only when the value is present, so the
/// server's `Option`/`#[serde(default)]` fields keep their "absent" meaning.
fn put_opt(map: &mut Map<String, Value>, key: &str, value: Option<impl Into<Value>>) {
  if let Some(v) = value {
    map.insert(key.to_string(), v.into());
  }
}

/// Resolves an `--expire` flag into a `ttl_seconds` value. `never` yields
/// `Some(0)` for endpoints where 0 means "no expiry"; when `never_omits` is
/// true (token/admin-key creation, where an absent field means "never"), it
/// yields `None` instead.
fn ttl_field(expire: &Option<String>, never_omits: bool) -> Result<Option<u64>, String> {
  match expire {
    None => Ok(None),
    Some(raw) => {
      let secs = parse_duration(raw)?;
      if secs == 0 && never_omits {
        Ok(None)
      } else {
        Ok(Some(secs))
      }
    }
  }
}

/// Reads a value that may be `-` (meaning: read it from stdin), used for
/// passwords and JSON documents so secrets need not appear in shell history.
fn read_maybe_stdin(value: &str) -> Result<String, String> {
  if value != "-" {
    return Ok(value.to_string());
  }
  use std::io::Read;
  let mut buf = String::new();
  std::io::stdin()
    .read_to_string(&mut buf)
    .map_err(|e| format!("failed to read stdin: {}", e))?;
  Ok(buf.trim_end_matches(['\n', '\r']).to_string())
}

/// Loads a JSON document from a file path (or stdin for `-`).
fn read_json_file(path: &str) -> Result<Value, String> {
  let raw = if path == "-" {
    read_maybe_stdin("-")?
  } else {
    std::fs::read_to_string(path).map_err(|e| format!("failed to read {}: {}", path, e))?
  };
  serde_json::from_str(&raw).map_err(|e| format!("{} is not valid JSON: {}", path, e))
}

/// Performs one admin API call and returns the decoded response. A JSON body
/// decodes into a `Value`; anything else (CSV, plain text) comes back as a
/// JSON string so the caller can print it verbatim.
async fn send(
  http: &reqwest::Client,
  server: &str,
  credential: Option<&str>,
  call: Call,
) -> Result<Value, String> {
  let url = build_http_url(server, &call.path)?;
  let mut parsed = url::Url::parse(&url).map_err(|e| e.to_string())?;
  if !call.query.is_empty() {
    let mut pairs = parsed.query_pairs_mut();
    for (k, v) in &call.query {
      pairs.append_pair(k, v);
    }
    drop(pairs);
  }

  let mut req = http.request(call.method.clone(), parsed.as_str());
  if let Some(secret) = call.auth.as_deref().or(credential) {
    req = req.bearer_auth(secret);
  }
  // A `null` body means "no body": the bodyless POST endpoints (replay,
  // refire, redeliver) have no JSON extractor to satisfy.
  if let Some(body) = call.body.as_ref().filter(|b| !b.is_null()) {
    req = req.json(body);
  }
  let response = req
    .send()
    .await
    .map_err(|e| format!("request to {} failed: {}", parsed, e))?;

  let status = response.status();
  // The dashboard router answers an unauthenticated API call with a redirect
  // to the login page. Following it would yield a 200 with HTML, so surface
  // it as the authentication error it actually is.
  if status.is_redirection() {
    return Err(
      "authentication required: pass an admin key with --api-key (or APERIO_API_KEY / yaml server.api_key)"
        .to_string(),
    );
  }
  let text = response.text().await.unwrap_or_default();
  if !status.is_success() {
    let detail = text.trim();
    return Err(if detail.is_empty() {
      format!("server returned {}", status)
    } else {
      format!("server returned {}: {}", status, detail)
    });
  }
  if text.trim().is_empty() {
    return Ok(Value::Null);
  }
  Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

// --- Command → call mapping -----------------------------------------------

/// The host/path scope of an api command. It comes from the client's own
/// global `--hostname` / `--path` flags, which mean the same thing here as
/// they do for a tunnel: comma-separated lists are accepted wherever the
/// endpoint takes several.
struct Scope {
  hostnames: Vec<String>,
  paths: Vec<String>,
}

impl Scope {
  fn from_opts(opts: &CommonOpts) -> Self {
    let split = |raw: &Option<String>| -> Vec<String> {
      raw
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
    };
    Self {
      hostnames: split(&opts.hostname),
      paths: split(&opts.path),
    }
  }
  /// The single hostname the command acts on, if one was given.
  fn hostname(&self) -> Option<String> {
    self.hostnames.first().cloned()
  }
  /// The single path the command acts on, if one was given.
  fn path(&self) -> Option<String> {
    self.paths.first().cloned()
  }
  /// The hostname of a command that cannot work without one.
  fn require_hostname(&self) -> Result<String, String> {
    self
      .hostname()
      .ok_or_else(|| "a hostname is required (--hostname app.example.com)".to_string())
  }
}

/// Translates one CLI command into the HTTP call that serves it.
fn build_call(
  command: &ApiCommand,
  settings: &ClientSettings,
  opts: &CommonOpts,
) -> Result<Call, String> {
  let scope = Scope::from_opts(opts);
  Ok(match command {
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
      if a.allow_public || a.no_allow_public {
        body.insert("allow_public".into(), Value::Bool(a.allow_public));
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
    ApiCommand::Maintenance(MaintenanceCmd::On { hostname }) => Call::post(
      "/aperio/api/maintenance",
      json!({ "hostname": hostname, "enabled": true }),
    ),
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

    ApiCommand::Inbox(InboxCmd::List) => Call::get("/aperio/api/inbox"),
    ApiCommand::Inbox(InboxCmd::Show { id }) => Call::get(format!("/aperio/api/inbox/{}", id)),
    ApiCommand::Inbox(InboxCmd::Refire { id }) => {
      Call::post(format!("/aperio/api/inbox/{}/refire", id), Value::Null)
    }
    ApiCommand::Inbox(InboxCmd::Delete { id }) => Call::delete(format!("/aperio/api/inbox/{}", id)),
    ApiCommand::Inbox(InboxCmd::Clear) => Call::delete("/aperio/api/inbox"),

    ApiCommand::User(UserCmd::List) => Call::get("/aperio/api/users"),
    ApiCommand::User(UserCmd::Create(a)) => Call::post(
      "/aperio/api/users",
      json!({
        "username": a.username,
        "password": read_maybe_stdin(&a.password)?,
        "role": a.role,
      }),
    ),
    ApiCommand::User(UserCmd::Update(a)) => {
      let mut body = Map::new();
      put_opt(&mut body, "role", a.role.clone());
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
    ApiCommand::Org(OrgCmd::Create { name }) => Call::post(
      "/aperio/api/orgs",
      json!({ "name": name, "hostnames": scope.hostnames }),
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
    ApiCommand::Org(OrgCmd::Usage { id }) => Call::get(format!("/aperio/api/orgs/{}/usage", id)),
    ApiCommand::Org(OrgCmd::Select { id }) => {
      Call::post("/aperio/api/orgs/select", json!({ "id": id }))
    }

    ApiCommand::AdminKey(AdminKeyCmd::List) => Call::get("/aperio/api/admin-keys"),
    ApiCommand::AdminKey(AdminKeyCmd::Create(a)) => {
      let mut body = Map::new();
      body.insert("name".into(), Value::String(a.name.clone()));
      body.insert("role".into(), Value::String(a.role.clone()));
      put_opt(&mut body, "org_id", a.org_id.clone());
      put_opt(&mut body, "ttl_seconds", ttl_field(&a.expire, true)?);
      Call::post("/aperio/api/admin-keys", Value::Object(body))
    }
    ApiCommand::AdminKey(AdminKeyCmd::Revoke { id }) => {
      Call::delete(format!("/aperio/api/admin-keys/{}", id))
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
    ApiCommand::Export => Call::get("/aperio/api/export"),
    ApiCommand::Import(a) => Call::post("/aperio/api/import", read_json_file(&a.file)?),
    ApiCommand::Openapi => Call::get("/aperio/api/openapi.json"),
  })
}

/// The credential sent as `Authorization: Bearer`: the admin API key when one
/// is configured, otherwise the tunnel token (accepted by the endpoints that
/// take the master token, such as `api tunnel create`).
fn credential(settings: &ClientSettings) -> Option<String> {
  settings
    .api_key
    .clone()
    .or_else(|| settings.token.clone())
    .filter(|s| !s.trim().is_empty())
}

/// Runs one `aperio-client api ...` command and exits the process: the JSON
/// response goes to stdout with exit code 0, any failure to stderr with 1.
pub(crate) async fn run_api(
  settings: &ClientSettings,
  opts: &CommonOpts,
  command: &ApiCommand,
) -> ! {
  let Some(server) = settings.server.clone() else {
    eprintln!(
      "error: the server URL is required (--server-url, APERIO_SERVER_URL, or yaml: server.url)"
    );
    std::process::exit(1);
  };

  let call = match build_call(command, settings, opts) {
    Ok(call) => call,
    Err(e) => {
      eprintln!("error: {}", e);
      std::process::exit(1);
    }
  };

  let http = match reqwest::Client::builder()
    .timeout(Duration::from_secs(30))
    // Never follow the login redirect the dashboard router answers with; it
    // would turn a 401-in-spirit into a 200 page of HTML.
    .redirect(reqwest::redirect::Policy::none())
    .build()
  {
    Ok(client) => client,
    Err(e) => {
      eprintln!("error: failed to build the HTTP client: {}", e);
      std::process::exit(1);
    }
  };

  match send(&http, &server, credential(settings).as_deref(), call).await {
    Ok(Value::Null) => std::process::exit(0),
    // Non-JSON responses (the CSV export, plain-text acknowledgements) print
    // verbatim rather than as a quoted JSON string.
    Ok(Value::String(text)) => {
      println!("{}", text);
      std::process::exit(0);
    }
    Ok(value) => {
      println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
      );
      std::process::exit(0);
    }
    Err(e) => {
      eprintln!("error: {}", e);
      std::process::exit(1);
    }
  }
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
