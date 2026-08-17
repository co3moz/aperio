use axum::{
  Json,
  extract::ConnectInfo,
  response::sse::{Event, KeepAlive, Sse},
};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use tracing::info;

// Split by what each endpoint is for: the numbers, the streams that keep
// arriving, the controls an operator has, and the config view. `numbers`
// rather than `stats` because the crate already has a `store::stats` these
// files reach for by that name.
pub(crate) mod config_view;
pub(crate) mod control;
pub(crate) mod live;
pub(crate) mod numbers;

pub(crate) use config_view::*;
pub(crate) use control::*;
pub(crate) use live::*;
pub(crate) use numbers::*;

use aperio_config::format_bandwidth;

use crate::protocol::PROTOCOL_VERSION;
use crate::routing::{extract_client_ip, normalize_hostname_bind, normalize_path_bind};
use crate::state::{ClientDetail, EnhancedServerStats, RequestLog};
use crate::store::stats::{self};

#[cfg(test)]
#[path = "clients_tests.rs"]
pub(crate) mod clients_tests;
