//! Every `aperio-client api` subcommand and its arguments.
//!
//! Declarations only: what the command line accepts, in clap's vocabulary.
//! Which HTTP call each one becomes is [`super::build`], kept apart because
//! the two change for different reasons, a new flag here and a new endpoint
//! there.

use clap::{Args, Subcommand};

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
  /// Autoscaling records armed by clients
  #[command(subcommand)]
  Scaling(ScalingCmd),
  /// Captured requests (inspector)
  #[command(subcommand)]
  Request(RequestCmd),
  /// Server settings overrides
  #[command(subcommand)]
  Settings(SettingsCmd),
  /// Audit trail
  #[command(subcommand)]
  Audit(AuditCmd),
  /// Message bus: publish to this organization's subscribers
  Publish(PublishArgs),
  /// Message bus: who is subscribed, and to what
  Subscribers,
  /// Dry run: which rule would answer a request, and what each stage saw
  Explain(ExplainArgs),
  /// A JSON Schema for a config file: `client` or `server`
  Schema {
    /// Which schema: client or server
    kind: String,
  },
  /// Edge integration: Traefik dynamic configuration for the served hostnames
  #[command(name = "edge-traefik")]
  EdgeTraefik,
  /// Edge integration: whether this server serves a hostname
  #[command(name = "edge-ask")]
  EdgeAsk {
    /// Hostname to ask about
    hostname: String,
  },
  /// Request volume in five-second buckets over the last 15 minutes
  Activity,
  /// The audit trail as CSV
  #[command(name = "audit-csv")]
  AuditCsv,
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
  /// Dump the server's stored state as JSON
  Export(ExportArgs),
  /// Apply a dump created by `api export`
  Import(FileArgs),
  /// Fetch the OpenAPI document describing this server's API
  Openapi,
}

#[derive(Args)]
pub(crate) struct PublishArgs {
  /// Topic to publish on (wildcards are filter syntax and are refused here)
  pub(crate) topic: String,
  /// The message, as text
  #[arg(long, value_name = "TEXT", conflicts_with = "payload_base64")]
  pub(crate) payload: Option<String>,
  /// The message, Base64-encoded, for anything that is not text
  #[arg(long = "payload-base64", value_name = "B64")]
  pub(crate) payload_base64: Option<String>,
  /// 1 keeps the message until each subscriber acknowledges it
  #[arg(long, value_name = "0|1", default_value_t = 0)]
  pub(crate) qos: u8,
}

#[derive(Args)]
pub(crate) struct ExplainArgs {
  /// Hostname, or a full URL
  pub(crate) hostname: String,
  /// Request path (default /)
  #[arg(long, value_name = "PATH")]
  pub(crate) path: Option<String>,
  /// Request method (default GET)
  #[arg(long, value_name = "METHOD")]
  pub(crate) method: Option<String>,
}

#[derive(Args)]
pub(crate) struct ShareArgs {
  /// Lifetime: 30m, 2h, 1d, 1w, or never (default: the server's 3 days)
  #[arg(long, value_name = "DURATION")]
  pub(crate) expire: Option<String>,
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
  pub(crate) name: String,
  /// Source IP/CIDR allowed to connect; repeatable, omitted = any
  #[arg(long = "allowed-ip", value_name = "IP_OR_CIDR")]
  pub(crate) allowed_ips: Vec<String>,
  /// Lifetime: 30m, 2h, 1d, or never (default)
  #[arg(long, value_name = "DURATION")]
  pub(crate) expire: Option<String>,
  /// Request rate limit for traffic served through this token
  #[arg(long = "max-rps", value_name = "REQ_PER_SEC")]
  pub(crate) max_rps: Option<f64>,
  /// Daily byte quota (request + response payload)
  #[arg(long = "daily-max-bytes", value_name = "BYTES")]
  pub(crate) daily_max_bytes: Option<u64>,
  /// Allow clients using this token to publish services as public
  #[arg(long = "allow-public")]
  pub(crate) allow_public: bool,
  /// Allow clients using this token to send OpenTelemetry exports through the
  /// server's OTel bridge (the server's own otel_bridge must be on too)
  #[arg(long = "allow-otel")]
  pub(crate) allow_otel: bool,
  /// Mark the token as a canary: any successful auth raises an alert
  #[arg(long)]
  pub(crate) canary: bool,
}

#[derive(Args)]
pub(crate) struct TokenUpdateArgs {
  /// Token record id
  pub(crate) id: String,
  /// New label
  #[arg(long, value_name = "NAME")]
  pub(crate) name: Option<String>,
  /// Replacement source IP permission; repeatable
  #[arg(long = "allowed-ip", value_name = "IP_OR_CIDR")]
  pub(crate) allowed_ips: Vec<String>,
  /// New lifetime from now, or `never` to clear the expiry
  #[arg(long, value_name = "DURATION")]
  pub(crate) expire: Option<String>,
  /// New rate limit; 0 clears it
  #[arg(long = "max-rps", value_name = "REQ_PER_SEC")]
  pub(crate) max_rps: Option<f64>,
  /// New daily byte quota; 0 clears it
  #[arg(long = "daily-max-bytes", value_name = "BYTES")]
  pub(crate) daily_max_bytes: Option<u64>,
  /// Permit publishing public services
  #[arg(long = "allow-public")]
  pub(crate) allow_public: bool,
  /// Forbid publishing public services
  #[arg(long = "no-allow-public", conflicts_with = "allow_public")]
  pub(crate) no_allow_public: bool,
  /// Permit the OTel bridge
  #[arg(long = "allow-otel")]
  pub(crate) allow_otel: bool,
  /// Withdraw the OTel bridge
  #[arg(long = "no-allow-otel", conflicts_with = "allow_otel")]
  pub(crate) no_allow_otel: bool,
  /// Turn the canary flag on
  #[arg(long)]
  pub(crate) canary: bool,
  /// Turn the canary flag off
  #[arg(long = "no-canary", conflicts_with = "canary")]
  pub(crate) no_canary: bool,
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
  pub(crate) name: Option<String>,
  /// Source IP/CIDR allowed to connect; repeatable
  #[arg(long = "allowed-ip", value_name = "IP_OR_CIDR")]
  pub(crate) allowed_ips: Vec<String>,
  /// Lifetime: 30m, 2h, 1d (default 1h, max 7d)
  #[arg(long, value_name = "DURATION")]
  pub(crate) expire: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum MaintenanceCmd {
  /// List the hostnames currently in maintenance mode
  List,
  /// Turn maintenance mode on for a hostname (`*` = every hostname)
  On {
    /// Hostname, `*.example.com` for every subdomain of it, or `*`
    hostname: String,
    /// Why, in one line. Shown on the 503 page and in the dashboard.
    #[arg(long, value_name = "TEXT")]
    reason: Option<String>,
    /// Seconds until it lifts by itself; omitted = until turned off
    #[arg(long, value_name = "SECONDS")]
    ttl: Option<u64>,
  },
  /// Turn maintenance mode off for a hostname (`*` = every hostname)
  Off {
    /// Hostname, `*.example.com`, or `*`
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
  /// The configuration a connected client resolved, as it is running it
  Config {
    /// Client connection id
    id: String,
  },
}

#[derive(Args)]
pub(crate) struct ClientOverrideArgs {
  /// Client connection id
  pub(crate) id: String,
  /// Clear both overrides (otherwise --hostname / --path set them)
  #[arg(long)]
  pub(crate) clear: bool,
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
  pub(crate) path_prefix: Option<String>,
  /// Only entries tagged with this backend Surrogate-Key
  #[arg(long = "surrogate-key", value_name = "KEY")]
  pub(crate) surrogate_key: Option<String>,
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
  /// Fire one synthetic event through the real delivery path and report what
  /// the receiver answered
  Test {
    /// Webhook id
    id: String,
  },
}

#[derive(Args)]
pub(crate) struct WebhookCreateArgs {
  /// Label for the webhook
  #[arg(long, value_name = "NAME")]
  pub(crate) name: String,
  /// Destination URL
  #[arg(long, value_name = "URL")]
  pub(crate) url: String,
  /// Event to subscribe to; repeatable, omitted = all events
  #[arg(long = "event", value_name = "EVENT")]
  pub(crate) events: Vec<String>,
  /// HMAC signing secret for the delivery signature header
  #[arg(long, value_name = "SECRET")]
  pub(crate) secret: Option<String>,
  /// Payload format: generic (default), slack, discord, or teams
  #[arg(long, value_name = "FORMAT")]
  pub(crate) format: Option<String>,
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
  pub(crate) username: String,
  /// Password, at least 8 characters; `-` reads it from stdin
  #[arg(long, value_name = "PASSWORD")]
  pub(crate) password: String,
  /// Role: viewer, operator, or admin
  #[arg(long, value_name = "ROLE")]
  pub(crate) role: String,
}

#[derive(Args)]
pub(crate) struct UserUpdateArgs {
  /// User record id
  pub(crate) id: String,
  /// New role: viewer, operator, or admin
  #[arg(long, value_name = "ROLE")]
  pub(crate) role: Option<String>,
  /// Enable the account
  #[arg(long)]
  pub(crate) enable: bool,
  /// Disable the account
  #[arg(long, conflicts_with = "enable")]
  pub(crate) disable: bool,
  /// New password, at least 8 characters; `-` reads it from stdin
  #[arg(long, value_name = "PASSWORD")]
  pub(crate) password: Option<String>,
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
  /// Set an organization's display name (omit --name to go back to the handle)
  #[command(name = "custom-name")]
  CustomName {
    /// Organization id
    id: String,
    /// What to call it on screen
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
  },
  /// Set or clear a child organization's OIDC override (no --issuer clears it)
  Oidc(OrgOidcArgs),
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
pub(crate) struct OrgOidcArgs {
  /// Organization id
  pub(crate) id: String,
  /// Issuer URL; omit to clear the override
  #[arg(long, value_name = "URL")]
  pub(crate) issuer: Option<String>,
  /// OIDC client id
  #[arg(long = "client-id", value_name = "ID")]
  pub(crate) client_id: Option<String>,
  /// OIDC client secret (write-only; never echoed back)
  #[arg(long = "client-secret", value_name = "SECRET")]
  pub(crate) client_secret: Option<String>,
  /// Email address allowed to sign in; repeat for several
  #[arg(long = "allowed-email", value_name = "EMAIL")]
  pub(crate) allowed_emails: Vec<String>,
}

#[derive(Args)]
pub(crate) struct OrgQuotaArgs {
  /// Organization id
  pub(crate) id: String,
  /// Maximum connected clients; 0 clears the quota
  #[arg(long = "max-clients", value_name = "N")]
  pub(crate) max_clients: Option<u64>,
  /// Maximum tokens; 0 clears the quota
  #[arg(long = "max-tokens", value_name = "N")]
  pub(crate) max_tokens: Option<u64>,
  /// Maximum users; 0 clears the quota
  #[arg(long = "max-users", value_name = "N")]
  pub(crate) max_users: Option<u64>,
  /// Monthly byte budget; 0 clears the quota
  #[arg(long = "max-bytes-month", value_name = "BYTES")]
  pub(crate) max_bytes_month: Option<u64>,
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
  pub(crate) name: String,
  /// Role the key authenticates as: viewer, operator, or admin
  #[arg(long, value_name = "ROLE")]
  pub(crate) role: String,
  /// Organization the key acts within; omitted = master
  #[arg(long = "org", value_name = "ORG_ID")]
  pub(crate) org_id: Option<String>,
  /// Lifetime: 30m, 2h, 1d, or never (default)
  #[arg(long, value_name = "DURATION")]
  pub(crate) expire: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum ScalingCmd {
  /// List armed records with their live pool capacity and utilization
  List,
  /// Disarm a record (a running client re-arms it on its next heartbeat)
  Disarm {
    /// Record id, as shown by `api scaling list`
    id: String,
  },
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
  pub(crate) file: String,
}

#[derive(Args)]
pub(crate) struct ExportArgs {
  /// Sections to include, comma separated. Omitted: the configuration that
  /// rebuilds a deployment (tokens, webhooks, users, organizations, scaling,
  /// settings_overrides). Also available: statistics, uptime, inbox,
  /// admin_keys. Without `organizations`, only master's rows travel.
  #[arg(long, value_name = "SECTIONS")]
  pub(crate) include: Option<String>,
}

#[derive(Args)]
pub(crate) struct HistoryArgs {
  /// Bucket unit: day (default), week, month, or year
  #[arg(long, value_name = "UNIT")]
  pub(crate) unit: Option<String>,
  /// Number of buckets, newest last
  #[arg(long, value_name = "N")]
  pub(crate) count: Option<usize>,
  /// Custom range start, YYYY-MM-DD (day buckets)
  #[arg(long, value_name = "DATE")]
  pub(crate) from: Option<String>,
  /// Custom range end, YYYY-MM-DD
  #[arg(long, value_name = "DATE")]
  pub(crate) to: Option<String>,
}

#[derive(Args)]
pub(crate) struct BandwidthArgs {
  /// Bucket granularity: day (default) or month
  #[arg(long, value_name = "UNIT")]
  pub(crate) unit: Option<String>,
  /// Buckets to return (max 62)
  #[arg(long, value_name = "N")]
  pub(crate) count: Option<usize>,
}

#[derive(Args)]
pub(crate) struct PurgeArgs {
  /// Token label whose aggregate records should be erased
  #[arg(long = "token-name", value_name = "NAME")]
  pub(crate) token_name: Option<String>,
  /// Visitor IP whose inspector captures should be erased
  #[arg(long, value_name = "IP")]
  pub(crate) ip: Option<String>,
}
