use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::*;

/// `shutdown_drain:` as a number of seconds, or the word `auto`.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum ShutdownDrain {
  /// Wait this many seconds for in-flight requests.
  Seconds(u64),
  /// `auto`: take the longest drain budget connected clients announce,
  /// capped, so the number follows the deployment instead of being a constant
  /// nobody revisits.
  Named(String),
}

/// `cache:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too. Default: `false`
/// (disabled).
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum CacheSetting {
  /// `true` turns the cache on with the defaults.
  Enabled(bool),
  /// The full block.
  Group(CacheGroup),
}

/// `dashboard:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too. Default: `true`
/// (enabled).
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum DashboardSetting {
  /// `false` serves no dashboard at all.
  Enabled(bool),
  /// The full block.
  Group(DashboardGroup),
}

/// `metrics:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too. Default: `false`
/// (disabled).
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum MetricsSetting {
  /// `true` exposes the Prometheus endpoint.
  Enabled(bool),
  /// The full block.
  Group(MetricsGroup),
}

/// `otel:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too. Default: `false`
/// (disabled).
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum OtelSetting {
  /// `true` exports traces with the defaults.
  Enabled(bool),
  /// The full block.
  Group(OtelGroup),
}

/// `scaling:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too. Default: `false`
/// (disabled).
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum ScalingSetting {
  /// `true` honors the clients' scaling declarations.
  Enabled(bool),
  /// The full block.
  Group(ScalingGroup),
}

/// `failover:` written either as the bare value it has always accepted, or as
/// the block that carries its companion settings too. Default: `fail`.
#[derive(Deserialize, Serialize, Debug, Clone, JsonSchema)]
#[serde(untagged)]
pub enum FailoverSetting {
  /// The mode alone, e.g. `retry`.
  Mode(String),
  /// The full block.
  Group(FailoverGroup),
}
