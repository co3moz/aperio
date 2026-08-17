//! The process-wide things a reload has to be able to change: the local
//! message faces, the subscription runners, the OTLP bridge, and the pid file.
//!
//! Kept apart from the services because they are started once and *swapped*
//! rather than respawned: a face whose address did not change is left alone,
//! since rebinding drops the connections it is serving.

use std::time::Duration;
use tracing::{error, info, warn};

use crate::config::ClientSettings;
use crate::service::Shared;
use crate::*;

/// Starts the OTel bridge's receivers, if configured, and returns the queue
/// the tunnel transport drains.
///
/// `https` needs no queue on this side: the forwarder owns the receiver and
/// posts to the server itself, so nothing has to reach into a service task.
/// One running local face: its address, the switch that ends it, and the
/// acceptor's handle.
///
/// The switch reaches the accepted sessions as well as the acceptor. Ending
/// only the acceptor left every connection the face had already taken serving
/// a face the configuration no longer asks for, for as long as its client
/// cared to hold it open.
pub(crate) struct Face {
  addr: String,
  cancel: tokio::sync::watch::Sender<bool>,
  task: tokio::task::JoinHandle<()>,
}

impl Face {
  async fn stop(self, what: &str) {
    let _ = self.cancel.send(true);
    // Bounded: a face that will not wind up must not hold a reload, let
    // alone a shutdown, and its listener is released by the drop either way.
    let _ = tokio::time::timeout(Duration::from_secs(2), self.task).await;
    info!("{what} on {} stopped", self.addr);
  }
}

/// The process-wide facilities a reload has to be able to change: the two
/// local message faces and the subscription runners.
///
/// They were started once, before the supervisor loop, from the settings of
/// the first load. A reload rebuilt the services and nothing else, so a face
/// the operator removed from the file kept listening, one whose address moved
/// kept the old port, and an edited `subscribe:` block needed a restart, all
/// while `docs/configuration.md` promised that every setting applies.
///
/// A face whose address is unchanged is deliberately left alone rather than
/// rebound: rebinding drops the connections it is serving, and a reload that
/// changed something else entirely has no business doing that.
#[derive(Default)]
pub(crate) struct ProcessFacilities {
  pub(crate) http_face: Option<Face>,
  pub(crate) mqtt_face: Option<Face>,
  pub(crate) runners: Option<tokio::task::JoinHandle<()>>,
  /// What `otel_bridge:` said at startup, to notice a reload that changes the
  /// part of it that cannot be applied.
  pub(crate) otel_bridge: Option<aperio_config::OtelBridge>,
  /// The server and token the https transport posts with, when that transport
  /// is in use. Updated on reload: they are read per export, so this is one
  /// part of the bridge that does follow the file.
  pub(crate) otel_credentials: Option<OtelCredentials>,
}

impl ProcessFacilities {
  /// Brings the facilities in line with `settings`. On the first call a
  /// listener that cannot bind is fatal, as it always was; on a reload it is
  /// reported and the previous configuration for that face is kept, which is
  /// what the rest of the reload path does.
  pub(crate) async fn apply(
    &mut self,
    settings: &ClientSettings,
    shared: &Shared,
    first: bool,
  ) -> Result<(), String> {
    // The two faces. The address the file now asks for is bound *before* the
    // running one is stopped, so a bind that fails leaves the old face
    // serving: stopping first and then failing left the process with no face
    // at all, under a log line that claimed the previous configuration had
    // been kept.
    let bus = shared.messages.clone();
    self.http_face = swap_face(
      self.http_face.take(),
      settings.messages_listen.clone(),
      "Message face",
      first,
      |addr, cancel| {
        let bus = bus.clone();
        Box::pin(async move { crate::messages_http::serve(&addr, bus, cancel).await })
      },
    )
    .await?;

    let bus = shared.messages.clone();
    self.mqtt_face = swap_face(
      self.mqtt_face.take(),
      settings.messages_mqtt_listen.clone(),
      "MQTT face",
      first,
      |addr, cancel| {
        let bus = bus.clone();
        Box::pin(async move { crate::messages_mqtt::serve(&addr, bus, cancel).await })
      },
    )
    .await?;

    // Subscription filters. Replaced wholesale; a filter a local subscriber
    // is still holding survives, because that subscriber is still there.
    let topics: Vec<String> = settings.subscribe.iter().map(|e| e.topic.clone()).collect();
    if shared.messages.set_filters(topics).await && !first {
      shared.messages.resubscribe_all().await;
      info!("Subscriptions reloaded");
    }

    // The runners. Restarted rather than diffed: a Runner owns its
    // concurrency counter, and carrying one over from a command that changed
    // would mean the new command inherits the old one's in-flight count.
    let runners: Vec<crate::messages_run::Runner> = settings
      .subscribe
      .iter()
      .filter_map(|entry| {
        entry.run.as_deref().map(|command| {
          crate::messages_run::Runner::new(
            entry.topic.clone(),
            command.to_string(),
            entry.timeout,
            entry.max_concurrent,
            entry
              .env
              .iter()
              .map(|(k, v)| (k.clone(), v.clone()))
              .collect(),
          )
        })
      })
      .collect();
    // Subscribe the replacement before stopping the incumbent, so nothing
    // delivered in between falls between the two: a broadcast receiver only
    // sees what is sent after it exists, and `spawn` takes its receiver on
    // the calling thread for exactly this reason. The overlap is the safe
    // direction, since a message the old dispatcher is already handling is
    // not re-delivered to the new one.
    let replacement = crate::messages_run::spawn(shared.messages.clone(), runners);
    if let Some(task) = self.runners.take() {
      task.abort();
    }
    self.runners = replacement;

    // The server and token the https transport posts with are read per
    // export, so a reload that moves the server or rotates the token reaches
    // them. Without this the tunnel followed the change and the telemetry did
    // not: exports kept going to the old address, or were refused by a token
    // that had been replaced, and the earlier warning did not fire either,
    // because it only watched the `otel_bridge:` block.
    if let Some(credentials) = &self.otel_credentials
      && let (Some(server), Some(token)) = (settings.server.clone(), settings.token.clone())
    {
      credentials.send_if_modified(|current| {
        let next = (server, token);
        if *current == next {
          return false;
        }
        if !first {
          info!("OTel bridge: exports will now be posted to {}", next.0);
        }
        *current = next;
        true
      });
    }

    // The rest of the bridge is the one facility a reload cannot rebuild: the
    // receiving end of its queue is held by whichever tunnel connection is
    // live, and moving that would mean handing every service a different
    // queue mid-flight. Saying so is better than ignoring the edit.
    let unappliable = |cfg: &Option<aperio_config::OtelBridge>| {
      cfg.as_ref().map(|c| {
        (
          c.listen.clone(),
          c.listen_grpc.clone(),
          c.queue,
          c.transport.clone(),
        )
      })
    };
    if !first && unappliable(&self.otel_bridge) != unappliable(&settings.otel_bridge) {
      warn!(
        "otel_bridge: the listeners, queue or transport changed, and those cannot be rebuilt \
         while the client runs; restart to apply them"
      );
    }
    self.otel_bridge = settings.otel_bridge.clone();
    Ok(())
  }
}

/// Brings one face in line with what the configuration now asks for.
///
/// The order is the whole point: bind first, stop second. Only an address
/// that actually changed gets here, so the two never contend for the same
/// port, and a failure to bind the new one leaves the old one serving rather
/// than leaving the process with nothing.
pub(crate) async fn swap_face<F>(
  running: Option<Face>,
  want: Option<String>,
  what: &str,
  first: bool,
  start: F,
) -> Result<Option<Face>, String>
where
  F: FnOnce(
    String,
    tokio::sync::watch::Receiver<bool>,
  ) -> std::pin::Pin<
    Box<dyn Future<Output = Result<tokio::task::JoinHandle<()>, String>> + Send>,
  >,
{
  if running.as_ref().map(|f| f.addr.clone()) == want {
    return Ok(running);
  }
  let Some(addr) = want else {
    if let Some(face) = running {
      face
        .stop(&format!("{what} (the configuration no longer asks for it)"))
        .await;
    }
    return Ok(None);
  };
  let (cancel, cancel_rx) = tokio::sync::watch::channel(false);
  match start(addr.clone(), cancel_rx).await {
    Ok(task) => {
      if let Some(face) = running {
        face.stop(&format!("{what} (moved to {addr})")).await;
      }
      Ok(Some(Face { addr, cancel, task }))
    }
    Err(e) if first => Err(e),
    Err(e) => {
      match &running {
        Some(face) => warn!("{e}; the {what} on {} keeps serving", face.addr),
        None => warn!("{e}; no {what} is running"),
      }
      Ok(running)
    }
  }
}

/// What the bridge's https transport needs kept current, when it is in use.
pub(crate) type OtelCredentials = tokio::sync::watch::Sender<(String, String)>;

pub(crate) async fn start_otel_bridge(
  settings: &ClientSettings,
) -> (Option<otel_bridge::Queue>, Option<OtelCredentials>) {
  let Some(cfg) = settings.otel_bridge.as_ref() else {
    return (None, None);
  };
  let http = cfg
    .listen
    .clone()
    .or_else(|| Some("127.0.0.1:4318".to_string()));
  let grpc = cfg.listen_grpc.clone();
  let (tx, rx) = otel_bridge::channel(cfg.queue.unwrap_or(256));
  tokio::spawn(otel_bridge::run(http, grpc, tx));
  tokio::spawn(otel_bridge::report_drops());

  let over_tunnel = cfg
    .transport
    .as_deref()
    .map(str::trim)
    .map(|t| !t.eq_ignore_ascii_case("https"))
    .unwrap_or(true);
  if over_tunnel {
    info!("OTel bridge: exports will travel on the tunnel");
    return (Some(std::sync::Arc::new(tokio::sync::Mutex::new(rx))), None);
  }
  match (settings.server.clone(), settings.token.clone()) {
    (Some(server), Some(token)) => {
      info!("OTel bridge: exports will be posted to the server over https");
      let (credentials, rx_credentials) = tokio::sync::watch::channel((server, token));
      tokio::spawn(otel_bridge::run_https_forwarder(rx, rx_credentials));
      (None, Some(credentials))
    }
    _ => {
      error!(
        "OTel bridge: transport https needs a server URL and a tunnel token; exports will be dropped"
      );
      (None, None)
    }
  }
}

/// Writes the process id where an init system can find it.
///
/// Best effort by design: a pid file it cannot write is worth a warning, not a
/// refusal to start. The tunnel is the job, and a supervisor that wanted the
/// file will notice its absence long before a visitor does.
pub(crate) fn write_pid_file(path: &str) {
  match std::fs::write(path, std::process::id().to_string()) {
    Ok(()) => {
      info!("Wrote pid {} to {}", std::process::id(), path);
      let _ = PID_FILE.set(path.to_string());
    }
    Err(e) => warn!("Could not write the pid file {path}: {e}"),
  }
}

/// The pid file this process wrote, if any.
///
/// Recorded process-wide because the shutdown path does not come back here:
/// a service that has finished draining ends the process where it stands, so
/// the removal at the end of `async_main` was only ever reached by a run that
/// ended some other way. A clean SIGTERM left a stale pid file behind, and a
/// stale pid file is a number an init system will signal, whatever process
/// now holds it.
static PID_FILE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Removes the pid file, if this process wrote one. Called on every path that
/// ends the process deliberately.
pub(crate) fn remove_pid_file() {
  if let Some(path) = PID_FILE.get()
    && let Err(e) = std::fs::remove_file(path)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    warn!("Could not remove the pid file {path}: {e}");
  }
}
