use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
