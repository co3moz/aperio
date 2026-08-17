use axum::extract::ws::Message;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Semaphore, mpsc};
use tracing::warn;

// Split by which question each part answers about a request: what a service
// binds on, which service serves it, what that route says about itself, and
// who the visitor is.
pub(crate) mod binds;
pub(crate) mod client_ip;
pub(crate) mod route;
pub(crate) mod select;

pub(crate) use binds::*;
pub(crate) use client_ip::*;
pub(crate) use route::*;
pub(crate) use select::*;

use crate::settings::LbStrategy;
use crate::state::{ClientHandle, RouteGroupKey, ServiceState};

#[cfg(test)]
#[path = "routing_tests.rs"]
mod tests;
