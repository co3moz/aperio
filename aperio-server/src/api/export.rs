//! Dump export/import (`GET /aperio/api/export`, `POST /aperio/api/import`).
//!
//! The dump is a single JSON document rebuilding a server's persisted state
//! on another instance or after an upgrade gone wrong. Being a *logical*
//! dump, it survives schema changes that a raw `aperio.db` copy would not:
//! unknown fields are dropped, missing ones take their defaults.
//!
//! **The caller chooses what travels.** `?include=` names the sections;
//! omitted, it means the configuration that rebuilds a deployment (tokens,
//! webhooks, users, organizations, autoscaling, settings overrides), which
//! is what this endpoint always dumped. The rest of what the store holds,
//! the statistics, the uptime history, the recent activity rings, the webhook
//! inbox and the admin keys,
//! is there for the asking: it is a migration's history, and refusing to
//! carry it only means someone copies `aperio.db` and loses the schema
//! tolerance that is the point of this format.
//!
//! **Organizations gate the rest.** Every record here carries an `org_id`,
//! and without the `organizations` section those rows would land on a server
//! where their organization does not exist. Leave it out and only master's
//! rows travel, statistics included, rather than orphans.
//!
//! Never included, in either direction: sessions (everyone signs in again),
//! and the audit log, which is a tamper-evident chain in its own file and
//! would stop being evidence the moment it could be imported.
//! Admin-only in both directions; an import *replaces* the stores.

use axum::{
  Json,
  extract::{ConnectInfo, Query, State},
  http::{HeaderMap, StatusCode},
  response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::routing::extract_client_ip;
use crate::settings::SettingsOverrides;
use crate::state::AppState;
use crate::store::orgs::Organization;
use crate::store::tokens::ApiToken;
use crate::store::users::User;
use crate::store::webhooks::Webhook;

/// The dump format version this build writes and accepts.
const FORMAT_VERSION: u32 = 1;

/// One selectable part of a dump. The name is the JSON key it writes, which
/// is also what `?include=` takes, so there is one spelling to know.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Section {
  Tokens,
  Webhooks,
  Users,
  Organizations,
  Scaling,
  SettingsOverrides,
  Statistics,
  Uptime,
  Inbox,
  AdminKeys,
  Activity,
}

impl Section {
  pub(crate) fn key(self) -> &'static str {
    match self {
      Section::Tokens => "tokens",
      Section::Webhooks => "webhooks",
      Section::Users => "users",
      Section::Organizations => "organizations",
      Section::Scaling => "scaling",
      Section::SettingsOverrides => "settings_overrides",
      Section::Statistics => "statistics",
      Section::Uptime => "uptime",
      Section::Inbox => "inbox",
      Section::AdminKeys => "admin_keys",
      Section::Activity => "activity",
    }
  }

  /// In the dump when `include` is not given. The six that rebuild a
  /// deployment, which is what this endpoint wrote before it could be asked
  /// for anything else: an existing script must keep getting them.
  fn on_by_default(self) -> bool {
    !matches!(
      self,
      Section::Statistics
        | Section::Uptime
        | Section::Inbox
        | Section::AdminKeys
        | Section::Activity
    )
  }
}

pub(crate) const ALL_SECTIONS: [Section; 11] = [
  Section::Tokens,
  Section::Webhooks,
  Section::Users,
  Section::Organizations,
  Section::Scaling,
  Section::SettingsOverrides,
  Section::Statistics,
  Section::Uptime,
  Section::Inbox,
  Section::AdminKeys,
  Section::Activity,
];

/// `?include=tokens,users`. Absent means the default set; an empty value
/// means the same, since a dump of nothing is never what was meant.
#[derive(Deserialize, Default)]
pub(crate) struct ExportQuery {
  include: Option<String>,
}

/// The sections `include` asks for, and the names it got wrong.
fn requested(include: Option<&str>) -> (Vec<Section>, Vec<String>) {
  let Some(raw) = include.map(str::trim).filter(|s| !s.is_empty()) else {
    return (
      ALL_SECTIONS
        .into_iter()
        .filter(|s| s.on_by_default())
        .collect(),
      Vec::new(),
    );
  };
  let mut chosen = Vec::new();
  let mut unknown = Vec::new();
  for name in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
    match ALL_SECTIONS
      .into_iter()
      .find(|s| s.key().eq_ignore_ascii_case(name))
    {
      Some(section) if !chosen.contains(&section) => chosen.push(section),
      Some(_) => {}
      None => unknown.push(name.to_string()),
    }
  }
  (chosen, unknown)
}

/// Returns the selected sections of the dump as a downloadable JSON document.
#[utoipa::path(get, path = "/aperio/api/export", tag = "dashboard",
  description = "Downloads a logical dump. ?include= names the sections (tokens, webhooks, users, organizations, scaling, settings_overrides, statistics, uptime, activity, inbox, admin_keys); omitted, the six configuration sections. Admin only.",
  params(("include" = Option<String>, Query, description = "Comma-separated section names; omitted = the configuration sections")),
  responses((status = 200, description = "The dump document", body = serde_json::Value), (status = 400, description = "Unknown section name")))]
pub(crate) async fn export_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Query(query): Query<ExportQuery>,
) -> Response {
  // The dump spans every organization's tokens, users, webhooks, and orgs,
  // a whole-server backup, restricted to the master super-admin.
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  let (sections, unknown) = requested(query.include.as_deref());
  if !unknown.is_empty() {
    // Silently dropping a misspelled section would hand back a dump missing
    // exactly what was asked for, which is the one failure a backup must not
    // have.
    return (
      StatusCode::BAD_REQUEST,
      format!(
        "Unknown section(s): {}. Known: {}",
        unknown.join(", "),
        ALL_SECTIONS
          .iter()
          .map(|s| s.key())
          .collect::<Vec<_>>()
          .join(", ")
      ),
    )
      .into_response();
  }
  // Charged here rather than at the top of the handler: this reads every
  // table in the store, so it is priced well above a page view, and a request
  // that was never going to be served should not pay for it. Auth and the
  // argument checks come first and answer 401 or 400 for free.
  let caller_ip = crate::routing::extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  if !state
    .check_rate_limit_cost(caller_ip, crate::state::RateCost::Expensive)
    .await
  {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let wants = |section: Section| sections.contains(&section);
  // Without the organizations themselves, a child org's rows would arrive
  // pointing at an organization that does not exist on the target. Only
  // master's travel.
  let orgs_included = wants(Section::Organizations);
  let keep = |org_id: &Option<String>| orgs_included || org_id.is_none();

  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();

  let mut dump = serde_json::Map::new();
  dump.insert("format_version".into(), FORMAT_VERSION.into());
  dump.insert(
    "exported_at".into(),
    chrono::Local::now().to_rfc3339().into(),
  );
  dump.insert("server_version".into(), env!("CARGO_PKG_VERSION").into());
  dump.insert(
    "sections".into(),
    sections
      .iter()
      .map(|s| serde_json::Value::from(s.key()))
      .collect::<Vec<_>>()
      .into(),
  );

  let mut put = |section: Section, value: serde_json::Value| {
    dump.insert(section.key().to_string(), value);
  };
  let mut counts: Vec<String> = Vec::new();

  if wants(Section::Tokens) {
    let rows: Vec<_> = state
      .token_store
      .lock()
      .await
      .list()
      .iter()
      .filter(|t| keep(&t.org_id))
      .cloned()
      .collect();
    counts.push(format!("tokens={}", rows.len()));
    put(
      Section::Tokens,
      serde_json::to_value(rows).unwrap_or_default(),
    );
  }
  if wants(Section::Webhooks) {
    let rows: Vec<_> = state
      .webhook_store
      .lock()
      .await
      .list()
      .iter()
      .filter(|w| keep(&w.org_id))
      .cloned()
      .collect();
    counts.push(format!("webhooks={}", rows.len()));
    put(
      Section::Webhooks,
      serde_json::to_value(rows).unwrap_or_default(),
    );
  }
  if wants(Section::Users) {
    let rows: Vec<_> = state
      .users
      .lock()
      .await
      .list()
      .iter()
      .filter(|u| keep(&u.org_id))
      .cloned()
      .collect();
    counts.push(format!("users={}", rows.len()));
    put(
      Section::Users,
      serde_json::to_value(rows).unwrap_or_default(),
    );
  }
  if wants(Section::Organizations) {
    let rows = state.org_store.lock().await.list().to_vec();
    counts.push(format!("organizations={}", rows.len()));
    put(
      Section::Organizations,
      serde_json::to_value(rows).unwrap_or_default(),
    );
  }
  if wants(Section::Scaling) {
    let rows: Vec<_> = state
      .scaling_store
      .lock()
      .await
      .list()
      .iter()
      .filter(|r| keep(&r.org_id))
      .cloned()
      .collect();
    counts.push(format!("scaling={}", rows.len()));
    put(
      Section::Scaling,
      serde_json::to_value(rows).unwrap_or_default(),
    );
  }
  if wants(Section::SettingsOverrides) {
    let overrides = state.settings_overrides.lock().await.clone();
    put(
      Section::SettingsOverrides,
      serde_json::to_value(overrides).unwrap_or_default(),
    );
  }
  if wants(Section::Statistics) {
    let mut stats = state.persistent_stats.lock().await.export();
    if !orgs_included {
      // The global aggregate stays: it is this server's own total, not any
      // organization's. Only the per-org slices are dropped.
      stats
        .by_org
        .retain(|id, _| id == crate::store::stats::MASTER_ORG_KEY);
    }
    counts.push(format!("statistics_orgs={}", stats.by_org.len()));
    put(
      Section::Statistics,
      serde_json::to_value(stats).unwrap_or_default(),
    );
  }
  if wants(Section::Uptime) {
    let rows: std::collections::HashMap<_, _> = state
      .uptime
      .lock()
      .await
      .snapshot()
      .into_iter()
      .filter(|(_, e)| keep(&e.org_id))
      .collect();
    counts.push(format!("uptime={}", rows.len()));
    put(
      Section::Uptime,
      serde_json::to_value(rows).unwrap_or_default(),
    );
  }
  if wants(Section::Activity) {
    // The two coarse rings behind the chart's long views. Kept out of the
    // default set for the same reason the statistics are: it is history, not
    // what rebuilds a deployment. Carrying it means a restored server's
    // two-hour and one-day charts still show what the source served rather
    // than starting blank.
    let mut rings = state.activity.lock().await.export();
    if !orgs_included {
      rings.retain_master_only();
    }
    counts.push(format!("activity={}", rings.len()));
    put(
      Section::Activity,
      serde_json::to_value(rings).unwrap_or_default(),
    );
  }
  if wants(Section::Inbox) {
    let rows: Vec<_> = state
      .inbox_store
      .lock()
      .await
      .list_all()
      .into_iter()
      .filter(|e| keep(&e.org_id))
      .cloned()
      .collect();
    counts.push(format!("inbox={}", rows.len()));
    put(
      Section::Inbox,
      serde_json::to_value(rows).unwrap_or_default(),
    );
  }
  if wants(Section::AdminKeys) {
    let rows: Vec<_> = state
      .admin_key_store
      .lock()
      .await
      .list()
      .iter()
      .filter(|k| keep(&k.org_id))
      .cloned()
      .collect();
    counts.push(format!("admin_keys={}", rows.len()));
    put(
      Section::AdminKeys,
      serde_json::to_value(rows).unwrap_or_default(),
    );
  }

  state
    .audit(
      "export_created",
      &state.session_actor(&headers).await,
      &actor_ip,
      &counts.join(" "),
    )
    .await;

  (
    StatusCode::OK,
    [
      ("content-type", "application/json".to_string()),
      (
        "content-disposition",
        format!(
          "attachment; filename=\"aperio-export-{}.json\"",
          chrono::Local::now().format("%Y%m%d-%H%M%S")
        ),
      ),
    ],
    serde_json::to_string_pretty(&serde_json::Value::Object(dump)).unwrap_or_default(),
  )
    .into_response()
}

/// A dump document accepted by the import endpoint. Sections are optional:
/// a missing section leaves that store untouched.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct ImportDump {
  format_version: u32,
  tokens: Option<Vec<ApiToken>>,
  webhooks: Option<Vec<Webhook>>,
  /// Dashboard user records; the full stored shape (hashes, TOTP, passkeys).
  #[schema(value_type = Option<Vec<serde_json::Value>>)]
  users: Option<Vec<User>>,
  settings_overrides: Option<SettingsOverrides>,
  organizations: Option<Vec<Organization>>,
  /// Autoscaling records, keyed by the bind they protect.
  scaling: Option<Vec<crate::store::scaling::ScalingRecord>>,
  /// Counters and per-period buckets, global plus one slice per organization.
  #[schema(value_type = Option<serde_json::Value>)]
  statistics: Option<crate::store::stats::PersistedStats>,
  /// Availability history, keyed by the entity it tracks.
  #[schema(value_type = Option<serde_json::Value>)]
  uptime: Option<std::collections::HashMap<String, crate::store::uptime::EntityUptime>>,
  /// The coarse activity rings, the two-hour and one-day request volume.
  #[schema(value_type = Option<serde_json::Value>)]
  activity: Option<crate::state::PersistedActivity>,
  /// Captured inbound webhooks.
  #[schema(value_type = Option<Vec<serde_json::Value>>)]
  inbox: Option<Vec<crate::store::inbox::InboxEntry>>,
  /// Programmatic admin keys (hashes only, like every credential here).
  #[schema(value_type = Option<Vec<serde_json::Value>>)]
  admin_keys: Option<Vec<crate::store::admin_keys::AdminKey>>,
}

/// Applies a dump: each present section *replaces* the corresponding store.
#[utoipa::path(post, path = "/aperio/api/import", tag = "dashboard",
  description = "Applies a dump created by /aperio/api/export; every section present in the document replaces its store, a missing one leaves it untouched (admin only).",
  request_body = ImportDump,
  responses((status = 200, description = "Import applied", body = serde_json::Value), (status = 400, description = "Invalid dump")))]
pub(crate) async fn import_handler(
  State(state): State<Arc<AppState>>,
  ConnectInfo(addr): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  Json(dump): Json<ImportDump>,
) -> Response {
  // Import replaces every organization's stores, master super-admin only.
  if let Err(resp) = crate::auth::require_master_admin(&state, &headers).await {
    return resp;
  }
  if dump.format_version != FORMAT_VERSION {
    return (
      StatusCode::BAD_REQUEST,
      format!(
        "Unsupported format_version {} (this server reads version {})",
        dump.format_version, FORMAT_VERSION
      ),
    )
      .into_response();
  }
  // Charged here rather than at the top of the handler: this rewrites every
  // table in the store, so it is priced well above a page view, and a request
  // that was never going to be served should not pay for it. Auth and the
  // argument checks come first and answer 401 or 400 for free.
  let caller_ip = crate::routing::extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  );
  if !state
    .check_rate_limit_cost(caller_ip, crate::state::RateCost::Expensive)
    .await
  {
    return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
  }
  let actor_ip = extract_client_ip(
    &headers,
    addr.ip(),
    state.config().trust_proxy,
    state.config().real_ip_header.as_deref(),
    &state.config().trusted_proxies,
  )
  .to_string();

  // Settings first: they can fail validation, and a rejected import should
  // change nothing at all.
  if let Some(overrides) = dump.settings_overrides
    && let Err(msg) = super::settings::apply_overrides_validated(&state, overrides).await
  {
    return (
      StatusCode::BAD_REQUEST,
      format!("settings_overrides rejected: {}", msg),
    )
      .into_response();
  }

  let mut counts = serde_json::Map::new();
  if let Some(tokens) = dump.tokens {
    let n = state.token_store.lock().await.import(tokens);
    counts.insert("tokens".into(), n.into());
  }
  if let Some(webhooks) = dump.webhooks {
    let n = state.webhook_store.lock().await.import(webhooks);
    counts.insert("webhooks".into(), n.into());
  }
  if let Some(users) = dump.users {
    let n = state.users.lock().await.import(users);
    counts.insert("users".into(), n.into());
  }
  if let Some(scaling) = dump.scaling {
    let n = state.scaling_store.lock().await.import(scaling);
    counts.insert("scaling".into(), n.into());
  }
  if let Some(organizations) = dump.organizations {
    let n = state.org_store.lock().await.import(organizations);
    counts.insert("organizations".into(), n.into());
  }
  if let Some(statistics) = dump.statistics {
    let orgs = statistics.by_org.len();
    state.persistent_stats.lock().await.import(statistics);
    counts.insert("statistics_orgs".into(), orgs.into());
  }
  if let Some(activity) = dump.activity {
    let now = crate::store::tokens::now_secs();
    let n = state.activity.lock().await.import(activity, now);
    counts.insert("activity".into(), n.into());
  }
  if let Some(uptime) = dump.uptime {
    let n = state.uptime.lock().await.import(uptime);
    counts.insert("uptime".into(), n.into());
  }
  if let Some(inbox) = dump.inbox {
    let n = state.inbox_store.lock().await.import(inbox);
    counts.insert("inbox".into(), n.into());
  }
  if let Some(admin_keys) = dump.admin_keys {
    let n = state.admin_key_store.lock().await.import(admin_keys);
    counts.insert("admin_keys".into(), n.into());
  }

  let summary = counts
    .iter()
    .map(|(k, v)| format!("{}={}", k, v))
    .collect::<Vec<_>>()
    .join(" ");
  info!("Dump imported ({})", summary);
  state
    .audit(
      "import_applied",
      &state.session_actor(&headers).await,
      &actor_ip,
      &summary,
    )
    .await;
  state
    .emit_event("import_applied", serde_json::Value::Object(counts.clone()))
    .await;

  (
    StatusCode::OK,
    Json(serde_json::json!({"imported": counts})),
  )
    .into_response()
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
