use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::*;

/// One `rate_limits:` entry: an aggregate requests-per-second ceiling for a
/// hostname and/or path prefix, independent of the per-visitor IP limit.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RateLimitRule {
  /// Hostname the rule applies to (omit for any host).
  #[schemars(extend("examples" = ["api.example.com"]))]
  pub hostname: Option<String>,
  /// Path prefix the rule applies to (omit for any path).
  #[schemars(extend("examples" = ["/api"]))]
  pub path: Option<String>,
  /// Sustained requests per second allowed to the route.
  #[schemars(extend("examples" = [50.0]))]
  pub rps: f64,
  /// Burst capacity. Default: the `rps` value (one second's worth).
  #[schemars(extend("examples" = [100.0]))]
  pub burst: Option<f64>,
  /// HTTP methods the rule applies to (omit for every method). Lets a write
  /// path be limited without throttling reads of the same route.
  #[schemars(extend("examples" = [["POST", "PUT", "DELETE"]]))]
  pub methods: Option<Vec<String>>,
}

/// One `fallbacks:` entry: where to send visitors of a hostname no client
/// currently serves, instead of answering with a gateway error.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FallbackRule {
  /// Hostname to catch, or `*` for every unserved hostname.
  #[schemars(extend("examples" = ["app.example.com", "*"]))]
  pub hostname: String,
  /// URL visitors are redirected to.
  #[schemars(extend("examples" = ["https://status.example.com"]))]
  pub url: String,
  /// Answer `308` instead of `307`, i.e. let clients cache the redirect.
  /// Default: `false`.
  #[serde(default)]
  pub permanent: bool,
  /// Append the requested path to the target URL. Default: `false`.
  #[serde(default)]
  pub preserve_path: bool,
}

/// A header match inside a `waf:` rule.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WafHeaderMatch {
  /// Header name to inspect (case-insensitive).
  #[schemars(extend("examples" = ["user-agent"]))]
  pub name: String,
  /// Regular expression the header value must match for the rule to fire.
  #[schemars(extend("examples" = ["(?i)sqlmap|nikto"]))]
  pub regex: String,
}

/// One `waf:` entry: a request is denied with `403` when every set condition
/// matches, or answered `413` when `max_body` is exceeded.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WafRule {
  /// Regular expression matched against the request path.
  #[schemars(extend("examples" = ["^/wp-admin"]))]
  pub path: Option<String>,
  /// HTTP methods the rule applies to (omit for any).
  #[schemars(extend("examples" = [["POST", "PUT"]]))]
  pub methods: Option<Vec<String>>,
  /// Header condition the request must match.
  #[schemars(extend("examples" = [{"name": "user-agent", "regex": "(?i)sqlmap|nikto"}]))]
  pub header: Option<WafHeaderMatch>,
  /// Body-size ceiling in bytes for the matched route; makes this a `413`
  /// size rule rather than a `403` deny rule.
  #[schemars(extend("examples" = [1048576]))]
  pub max_body: Option<usize>,
}

/// The fixed response of a client-less `respond` route.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
pub struct RespondRule {
  /// HTTP status to answer with. Default: `200`.
  #[schemars(extend("examples" = [503]))]
  pub status: Option<u16>,
  /// `Content-Type` of the response body.
  #[schemars(extend("examples" = ["text/html; charset=utf-8"]))]
  pub content_type: Option<String>,
  /// Response body.
  #[schemars(extend("examples" = ["<h1>Coming soon</h1>"]))]
  pub body: Option<String>,
}

/// One `alerts.rules` entry: a quantity the server measures, a bound, and how
/// long the condition must hold.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AlertRule {
  /// Names the alert. It becomes the event's `kind`, which is what a webhook
  /// receiver switches on, so it has to be unique.
  #[schemars(extend("examples" = ["disk-filling"]))]
  pub name: String,
  /// What to watch: `connected_clients`, `pending_requests`, `store_bytes`
  /// (the SQLite store and its sidecars on disk), or `rss_bytes` (the server
  /// process's resident memory, Linux only).
  #[schemars(extend("examples" = ["store_bytes"]))]
  pub metric: String,
  /// Fire while the value is strictly above this. Set this or `below`.
  #[schemars(extend("examples" = [536870912]))]
  pub above: Option<f64>,
  /// Fire while the value is strictly below this. Set this or `above`.
  #[schemars(extend("examples" = [1]))]
  pub below: Option<f64>,
  /// Seconds the condition must hold before firing, and hold clear before
  /// resolving. Default `0` = react on the first observation. Both directions
  /// use it, so a value sitting on its threshold cannot alert every tick.
  #[serde(rename = "for")]
  #[schemars(extend("examples" = [300]))]
  pub r#for: Option<u64>,
}

/// One `maintenance_windows:` entry: a recurring window during which matching
/// hostnames answer the maintenance page by themselves.
///
/// A list of mappings, so it has no environment-variable equivalent, like
/// `routes:` and `rate_limits:`.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceWindow {
  /// Hostname or pattern the window applies to, in the same shapes the
  /// maintenance API takes (`app.example.com`, `*.example.com`,
  /// `*-pi.example.com`). Omitted or `*` = every hostname on the server.
  #[schemars(extend("examples" = ["*.example.com"]))]
  pub hostname: Option<String>,
  /// Local start time, `HH:MM`.
  #[schemars(extend("examples" = ["02:00"]))]
  pub from: String,
  /// Local end time, `HH:MM`, exclusive. Earlier than `from` means the window
  /// wraps past midnight, and `days` then names the day it *starts*.
  #[schemars(extend("examples" = ["04:00"]))]
  pub to: String,
  /// Weekdays the window runs on (`mon`..`sun`, long names accepted).
  /// Omitted = every day.
  #[schemars(extend("examples" = [["sat", "sun"]]))]
  pub days: Option<Vec<String>>,
  /// IANA time zone the times are local to. Default: `UTC`. Use a named zone
  /// rather than a fixed offset so the window stays put across a
  /// daylight-saving change.
  #[schemars(extend("examples" = ["Europe/Istanbul"]))]
  pub tz: Option<String>,
  /// Shown on the maintenance page and in the dashboard while the window is
  /// running.
  #[schemars(extend("examples" = ["weekly patching"]))]
  pub reason: Option<String>,
}

/// One `routes` entry of the server's settings. Either an *answer* rule, a
/// hostname/path match paired with exactly one action (`redirect` or
/// `respond`) served without a client, or a *policy* rule, which has neither
/// action and instead carries settings (`timeout`, `headers`, `rate_limit`)
/// that apply to proxied traffic matching it. The two kinds are matched
/// independently, each first-match in file order.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
pub struct RouteRule {
  /// Hostname to match exactly (unset = any hostname).
  #[schemars(extend("examples" = ["old.example.com"]))]
  pub hostname: Option<String>,
  /// Path prefix to match, with bind semantics (unset = any path).
  #[schemars(extend("examples" = ["/robots.txt"]))]
  pub path: Option<String>,
  /// Redirect target; answers 302 (or 301 with `permanent: true`).
  #[schemars(extend("examples" = ["https://new.example.com"]))]
  pub redirect: Option<String>,
  /// Use a permanent 301 instead of the default 302. Default: `false`.
  #[serde(default)]
  pub permanent: bool,
  /// Append the request's path and query to the redirect target.
  /// Default: `false`.
  #[serde(default)]
  pub preserve_path: bool,
  /// Serve a fixed response instead of redirecting.
  #[schemars(extend("examples" = [{"status": 503, "body": "Be right back", "content_type": "text/plain"}]))]
  pub respond: Option<RespondRule>,
  /// Seconds to wait for the serving client's answer on this route, overriding
  /// `gateway.response_timeout` and any per-service `response_timeout` the
  /// client declared. Policy field: only valid on an entry that has neither
  /// `redirect` nor `respond`, since those never reach a backend.
  #[schemars(extend("examples" = [120]))]
  pub timeout: Option<u64>,
  /// Header edits for this route only, applied after the server-wide
  /// `headers:` rules so a route can override them. Policy field.
  #[schemars(extend("examples" = [{"response": {"add": {"cache-control": "public, max-age=3600"}}}]))]
  pub headers: Option<HeaderRules>,
  /// Rate limit for this route only, so the hostname and path are written
  /// once instead of repeated under `rate_limits:`. Wins over any
  /// `rate_limits:` entry matching the same request. Policy field.
  #[schemars(extend("examples" = [{"rps": 10, "burst": 20, "methods": ["POST"]}]))]
  pub rate_limit: Option<RouteRateLimit>,
  /// Split this route's traffic between two versions of a service. Policy
  /// field.
  #[schemars(extend("examples" = [{"service": "web-v2", "weight": 20, "header": "x-canary"}]))]
  pub canary: Option<RouteCanary>,
}

/// A `canary:` block inside a `routes:` entry: which service gets the new
/// version's traffic, how much of it, and how somebody opts in by hand.
///
/// Weighted routing and a header-based canary are the same mechanism seen from
/// two angles, which is why they are one block rather than two settings.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RouteCanary {
  /// Name of the `services:` entry serving the new version. Clients that
  /// announce this service name are the canary side; every other client
  /// serving the route is the stable side.
  #[schemars(extend("examples" = ["web-v2"]))]
  pub service: String,
  /// Percentage of visitors sent there without asking, `0` to `100`. Default:
  /// `0`, which with a `header` set is the opt-in-only shape.
  ///
  /// The split is decided per **visitor**, by hashing their address, not per
  /// request: a per-request coin flip would send one page load's twenty assets
  /// to both versions, which is a mixture rather than a canary and breaks the
  /// thing being tested first. The cost is that the split is only as even as
  /// the addresses are spread, so at low traffic or behind one large NAT
  /// twenty percent may not look like twenty percent.
  #[schemars(extend("examples" = [20]))]
  pub weight: Option<u8>,
  /// Request header that sends this visitor to the canary whatever the weight
  /// says, so a developer can reach the new version on demand.
  #[schemars(extend("examples" = ["x-canary"]))]
  pub header: Option<String>,
  /// Value that header must carry. Unset = any non-empty value.
  #[schemars(extend("examples" = ["on"]))]
  pub value: Option<String>,
}

/// A `rate_limit:` block inside a `routes:` entry: the same token bucket as a
/// `rate_limits:` rule, without repeating the hostname and path.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RouteRateLimit {
  /// Sustained requests per second allowed to the route.
  #[schemars(extend("examples" = [50.0]))]
  pub rps: f64,
  /// Burst capacity. Default: the `rps` value (one second's worth).
  #[schemars(extend("examples" = [100.0]))]
  pub burst: Option<f64>,
  /// HTTP methods the limit applies to (omit for every method). Lets a write
  /// path be limited without throttling reads of the same route.
  #[schemars(extend("examples" = [["POST", "PUT", "DELETE"]]))]
  pub methods: Option<Vec<String>>,
}

/// One `error_pages` entry of the server's settings: per-hostname custom
/// 504/503 pages overriding the global `504_page`/`503_page` for that host.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
pub struct ErrorPageRule {
  /// Hostname the pages apply to (matched exactly, case-insensitive).
  #[schemars(extend("examples" = ["app.example.com"]))]
  pub hostname: String,
  /// Path of the HTML file served on 504 gateway-timeout responses.
  #[serde(rename = "504_page")]
  #[schemars(extend("examples" = ["./pages/app-504.html"]))]
  pub page_504: Option<String>,
  /// Path of the HTML file served on 503 maintenance responses.
  #[serde(rename = "503_page")]
  #[schemars(extend("examples" = ["./pages/app-503.html"]))]
  pub page_503: Option<String>,
}
