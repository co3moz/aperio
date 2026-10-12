//! The settings document in force right now.
//!
//! Most settings reach the request path through [`crate::settings::ServerConfig`],
//! which is rebuilt from this document whenever it changes. A few modules
//! read their own section directly, the way they once read the environment:
//! a background task that starts once (backups, retention), a client built on
//! first use (the OTLP bridge), a cache tunable read per insert. Those read
//! [`current`], which is set before anything starts and replaced, whole, by
//! every successful settings change.
//!
//! A test build keeps the document per thread instead. Every test runs on its
//! own thread, and so does the current-thread runtime of a `#[tokio::test]`,
//! so one test's settings never reach a test running beside it, which a
//! process-wide value would do the moment two of them ran at once.

use std::sync::Arc;

use aperio_config::server_settings::ServerSettings;

#[cfg(not(test))]
static CURRENT: std::sync::RwLock<Option<Arc<ServerSettings>>> = std::sync::RwLock::new(None);

#[cfg(test)]
thread_local! {
  static CURRENT: std::cell::RefCell<Option<Arc<ServerSettings>>> =
    const { std::cell::RefCell::new(None) };
}

/// The document in force. The defaults until one has been set.
#[cfg(not(test))]
pub(crate) fn current() -> Arc<ServerSettings> {
  CURRENT
    .read()
    .expect("live settings lock poisoned")
    .clone()
    .unwrap_or_default()
}

/// Puts `settings` in force for every reader of [`current`].
#[cfg(not(test))]
pub(crate) fn set(settings: ServerSettings) {
  *CURRENT.write().expect("live settings lock poisoned") = Some(Arc::new(settings));
}

#[cfg(test)]
pub(crate) fn current() -> Arc<ServerSettings> {
  CURRENT.with(|c| c.borrow().clone().unwrap_or_default())
}

#[cfg(test)]
pub(crate) fn set(settings: ServerSettings) {
  CURRENT.with(|c| *c.borrow_mut() = Some(Arc::new(settings)));
}

/// Forgets this thread's document, so the next reader sees the defaults.
#[cfg(test)]
pub(crate) fn reset() {
  CURRENT.with(|c| *c.borrow_mut() = None);
}

/// Puts the document `yaml` describes in force on this thread.
#[cfg(test)]
pub(crate) fn set_yaml(yaml: &str) {
  set(ServerSettings::from_yaml(yaml).unwrap_or_else(|e| panic!("{yaml}: {e}")));
}
