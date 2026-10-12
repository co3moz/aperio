//! The server's sites: a hostname, or a path under one, and everything the
//! server itself does for it.
//!
//! A site is where every server-side rule about a route lives: an answer
//! given without a client (`redirect`, `respond`), the policy on proxied
//! traffic (`timeout`, `headers`, `rate_limit`, `canary`, `waf`), and what a
//! hostname says when nothing serves it (`fallback`, `pages`, `maintenance`).
//! It replaces seven sections that each matched a hostname in a way of its
//! own: exactly, exactly or `*`, by pattern, or not at all.
//!
//! Every site is matched the same way. `hostname` is exact
//! (`app.example.com`), a wildcard (`*.example.com`, `*-pi.example.com`), or
//! `*` for every hostname; `path` is a prefix on a segment boundary. A
//! request takes each setting from the most specific site that writes it: an
//! exact hostname before a pattern before `*`, then the longest `path`.
//! `headers` is the one exception and layers instead, every matching site's
//! edits applied broadest first, so the narrowest site has the last word.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::HeaderRules;
use crate::server_settings::Pages;
use crate::units::{ByteSize, Span};

/// One hostname, or a path under one, and what the server does for it.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Site {
  /// The hostname: exact (`app.example.com`), a pattern (`*.example.com`,
  /// `*-pi.example.com`), or `*` for every hostname.
  #[schemars(extend("examples" = ["app.example.com", "*.example.com", "*"]))]
  pub hostname: String,
  /// A path prefix under the hostname, on a segment boundary (`/api` covers
  /// `/api/v1` but not `/apix`). Unset = the whole hostname.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["/api"]))]
  pub path: Option<String>,
  /// Answer every request here with a redirect, without a client.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"url": "https://new.example.com", "permanent": true}]))]
  pub redirect: Option<Redirect>,
  /// Answer every request here with a fixed response, without a client.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"status": 503, "body": "Be right back", "content_type": "text/plain"}]))]
  pub respond: Option<RespondRule>,
  /// How long to wait for the serving client's answer, over
  /// `gateway.response_timeout` and any `timeout` the client declared.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["2m"]))]
  pub timeout: Option<Span>,
  /// Header edits on proxied traffic. Applied after the edits of every
  /// broader site that matches, so `*` sets what every hostname gets and a
  /// narrower site can override it.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"response": {"add": {"cache-control": "public, max-age=3600"}}}]))]
  pub headers: Option<HeaderRules>,
  /// A ceiling on the requests per second this site takes from every visitor
  /// together, beside the per-visitor limit.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"rps": 5, "burst": 10, "methods": ["POST"]}]))]
  pub rate_limit: Option<SiteRateLimit>,
  /// Split the traffic between two versions of a service.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"service": "web-v2", "weight": 20, "header": "x-canary"}]))]
  pub canary: Option<SiteCanary>,
  /// Requests refused before they reach a client: `403` for a deny rule,
  /// `413` for a `max_body` rule.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  #[schemars(extend("examples" = [[{"path": "^/wp-admin"}, {"header": {"name": "user-agent", "regex": "(?i)sqlmap|nikto"}}]]))]
  pub waf: Vec<WafRule>,
  /// Where to send visitors when no client serves this hostname, instead of
  /// a `504`. A site with a `path` cannot have one: it is about the hostname.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"url": "https://status.example.com", "preserve_path": true}]))]
  pub fallback: Option<Redirect>,
  /// This hostname's own `504` and `503` pages, over the ones a broader site
  /// sets. A site with a `path` cannot have them.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"unavailable": "<h1>Nothing is serving this right now</h1>"}]))]
  pub pages: Option<Pages>,
  /// Recurring windows during which this hostname answers the maintenance
  /// page by itself. A site with a `path` cannot have them.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  #[schemars(extend("examples" = [[{"from": "02:00", "to": "04:00", "days": ["sat"], "tz": "Europe/Istanbul"}]]))]
  pub maintenance: Vec<MaintenanceSchedule>,
}

/// A redirect: a site's own `redirect`, or its `fallback`.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Redirect {
  /// Where to send the visitor, an absolute `http(s)` URL.
  #[schemars(extend("examples" = ["https://new.example.com"]))]
  pub url: String,
  /// Answer `301` rather than `302`, which lets a browser remember it.
  /// Default `false`.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  #[schemars(extend("examples" = [true]))]
  pub permanent: bool,
  /// Append the requested path and query to `url`. Default `false`.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  #[schemars(extend("examples" = [true]))]
  pub preserve_path: bool,
}

/// The fixed response of a site that answers by itself.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RespondRule {
  /// HTTP status to answer with. Default `200`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [503]))]
  pub status: Option<u16>,
  /// `Content-Type` of the body. Default `text/html; charset=utf-8`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["text/plain"]))]
  pub content_type: Option<String>,
  /// The body.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["<h1>Coming soon</h1>"]))]
  pub body: Option<String>,
}

/// A site's `rate_limit`: one token bucket shared by every visitor.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SiteRateLimit {
  /// Sustained requests per second.
  #[schemars(extend("examples" = [50.0]))]
  pub rps: f64,
  /// Burst capacity. Default: the `rps` value, one second's worth.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [100.0]))]
  pub burst: Option<f64>,
  /// HTTP methods the limit counts (unset = every method), so a write path
  /// can be limited without throttling reads of it.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [["POST", "PUT", "DELETE"]]))]
  pub methods: Option<Vec<String>>,
}

/// A site's `canary`: which service gets the new version's traffic, how much
/// of it, and how somebody opts in by hand.
///
/// Weighted routing and a header-based canary are the same mechanism seen from
/// two angles, which is why they are one block rather than two settings.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SiteCanary {
  /// Name of the client service serving the new version. Clients announcing
  /// this service name are the canary side; every other client serving the
  /// site is the stable side.
  #[schemars(extend("examples" = ["web-v2"]))]
  pub service: String,
  /// Percentage of visitors sent there without asking, `0` to `100`. Default
  /// `0`, which with a `header` set is the opt-in-only shape.
  ///
  /// The split is decided per **visitor**, by hashing their address, not per
  /// request: a per-request coin flip would send one page load's twenty assets
  /// to both versions, which is a mixture rather than a canary and breaks the
  /// thing being tested first. The cost is that the split is only as even as
  /// the addresses are spread, so at low traffic or behind one large NAT
  /// twenty percent may not look like twenty percent.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [20]))]
  pub weight: Option<u8>,
  /// Request header that sends this visitor to the canary whatever the weight
  /// says, so a developer can reach the new version on demand.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["x-canary"]))]
  pub header: Option<String>,
  /// Value that header must carry. Unset = any non-empty value.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["on"]))]
  pub value: Option<String>,
}

/// A header condition inside a `waf` rule.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WafHeaderMatch {
  /// Header name to inspect (case-insensitive).
  #[schemars(extend("examples" = ["user-agent"]))]
  pub name: String,
  /// Regular expression the header value must match for the rule to fire.
  #[schemars(extend("examples" = ["(?i)sqlmap|nikto"]))]
  pub regex: String,
}

/// One `waf` rule of a site: a request is refused with `403` when every
/// condition it sets matches, or with `413` when it has a `max_body` and the
/// body is larger.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WafRule {
  /// Regular expression matched against the request path. It sees the whole
  /// path, not the part under the site's `path`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["^/wp-admin"]))]
  pub path: Option<String>,
  /// HTTP methods the rule applies to (unset = any).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [["POST", "PUT"]]))]
  pub methods: Option<Vec<String>>,
  /// A header the request must carry, matching a pattern.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [{"name": "user-agent", "regex": "(?i)sqlmap|nikto"}]))]
  pub header: Option<WafHeaderMatch>,
  /// Largest body the matched requests may carry, which makes this a `413`
  /// size rule rather than a `403` deny rule.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["1MB"]))]
  pub max_body: Option<ByteSize>,
}

/// One recurring maintenance window of a site.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceSchedule {
  /// Local start time, `HH:MM`.
  #[schemars(extend("examples" = ["02:00"]))]
  pub from: String,
  /// Local end time, `HH:MM`, exclusive. Earlier than `from` means the window
  /// wraps past midnight, and `days` then names the day it *starts*.
  #[schemars(extend("examples" = ["04:00"]))]
  pub to: String,
  /// Weekdays the window runs on (`mon`..`sun`, long names accepted). Unset =
  /// every day.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = [["sat", "sun"]]))]
  pub days: Option<Vec<String>>,
  /// IANA time zone the times are local to. Default `UTC`. A named zone
  /// rather than a fixed offset keeps the window in place across a
  /// daylight-saving change.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["Europe/Istanbul"]))]
  pub tz: Option<String>,
  /// Shown on the maintenance page and in the dashboard while the window
  /// runs.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(extend("examples" = ["weekly patching"]))]
  pub reason: Option<String>,
}
