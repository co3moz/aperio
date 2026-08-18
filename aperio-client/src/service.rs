//! One tunnel connection, from the dial to the read loop: the reconnect loop,
//! the heartbeat, the backend health probes and the forwarding state. The
//! supervisor in `main` spawns one task per connection and respawns them (with
//! freshly resolved settings) when the configuration file changes, which is how
//! every setting, not just a subset, takes effect on hot-reload.
//!
//! Split by what each piece is about, leaving [`run_service`] itself here
//! because it *is* the connection: the reconnect loop, the handshake, and the
//! dispatch loop that reads a frame and finds the service it belongs to.
//!
//! - [`startup`] is the four gates a connection passes before its first dial.
//! - [`writer`] is the write half of an established connection, the mirror of
//!   [`dispatch`].
//! - [`spec`] is what a service is once the config has been resolved, plus the
//!   process-wide state every service task shares and the lifecycle gates
//!   (`depends_on`, drain, idle retirement) that run around it.
//! - [`connect`] is what has to be settled per service before a connection can
//!   serve it: its backend health, the visitor gate this server will accept,
//!   the connection ceiling, and how a request reaches its backend.
//! - [`dispatch`] is the read loop itself: one frame in, the work it names
//!   dispatched, and the two things the reconnect loop needs when it ends.
//! - [`relay`] is that loop's delivery side, the four helpers that hand a
//!   frame to a TCP, UDP or WebSocket stream without blocking it.
//! - [`device_key`] is the trust-on-first-use key this client identifies with.
//!
//! Everything is re-exported, so `crate::service::Thing` resolves where it did.

pub(crate) mod connect;
pub(crate) mod device_key;
pub(crate) mod dispatch;
pub(crate) mod relay;
pub(crate) mod spec;
pub(crate) mod startup;
pub(crate) mod writer;

pub(crate) use connect::*;
pub(crate) use device_key::*;
pub(crate) use relay::*;
pub(crate) use spec::*;

use base64::prelude::*;
use futures_util::stream::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, watch};
use tokio_tungstenite::tungstenite::{
  client::IntoClientRequest,
  http::HeaderValue,
  protocol::{Message, WebSocketConfig},
};
use tracing::{error, info, warn};

use crate::protocol::{
  FRAME_REQUEST_CHUNK, FRAME_REQUEST_FULL, FRAME_REQUEST_FULL_ZLIB, FRAME_RESPONSE_FULL,
  FRAME_RESPONSE_FULL_ZLIB, PROTOCOL_VERSION, RequestBodyFeeder, TunnelDecl, TunnelMessage,
  compress_frame, decode_binary_frame, decompress_frame, encode_binary_frame, split_full_response,
};
use crate::proxy::http::{
  ForwardContext, ForwardRequest, HeaderTransform, handle_incoming_request,
};
use crate::proxy::ws::{WsStreamHandle, handle_upgrade_request};
use crate::tcp::{TcpStreamHandle, handle_tcp_open};
use crate::udp::{UdpStreamHandle, handle_udp_open};

/// Runs one tunnel connection, carrying every service in `services`, until the
/// process shuts down or `cancel` fires.
///
/// `services` is one entry in the ordinary shape and several under `multiplex:
/// true`. Each carries its own backend health, which the supervisor created so
/// a service's parallel connections share it.
/// `run_probe` is true only for the connection that owns those probes, the
/// others just report what they write.
pub(crate) async fn run_service(
  services: Vec<ServiceRuntime>,
  shared: Shared,
  mut cancel: watch::Receiver<bool>,
  run_probe: bool,
  connection_index: u32,
  ceiling: ConnectionCeiling,
) {
  let startup::Prepared {
    spec,
    multiplexed,
    label,
    probe_tasks,
  } = match startup::prepare(&services, &shared, run_probe, connection_index, &ceiling).await {
    Some(p) => p,
    None => return,
  };

  // Reconnection Loop. Retries use exponential backoff with jitter so that a
  // fleet of clients does not stampede the server after a restart; the
  // counter resets once a connection proves stable.
  let mut reconnect_attempt: u32 = 0;
  // Set when the server announces a graceful shutdown: the next reconnect
  // skips the exponential backoff (one short jittered delay instead).
  let mut fast_reconnect = false;
  // Index into `spec.ws_urls` for cross-server failover: advanced after each
  // failed/dropped connection so the client rotates across the server fleet.
  let mut server_idx = 0usize;
  // This connection's candidate servers: the configured list, plus whatever
  // the servers on it announce. Owned here rather than on the spec because it
  // grows at runtime and a config reload rebuilds the spec, which is the right
  // moment to forget what was learned.
  let mut ws_urls: Vec<String> = spec.ws_urls.clone();
  // Self-reported health for this connection: the ping task fills it in, the
  // read loop times the pongs, and the reconnect counter lives across
  // attempts, which is the point of it.
  let health_report = Arc::new(crate::health_report::HealthReport::default());
  let mut connected_once = false;
  'outer: loop {
    if *cancel.borrow() {
      break;
    }
    exit_if_shutting_down(&shared).await;

    let current_ws = ws_urls
      .get(server_idx % ws_urls.len().max(1))
      .cloned()
      .unwrap_or_else(|| spec.ws_url.clone());
    info!(
      "[{}] Connecting to Aperio Server at: {}...",
      label, current_ws
    );

    let ws_req_result = current_ws.into_client_request();
    let ws_req = match ws_req_result {
      Ok(mut req) => {
        // Set Authorization Token Header securely (avoids leaking token in query params / logs)
        match HeaderValue::from_str(&format!("Bearer {}", spec.token)) {
          Ok(val) => {
            req.headers_mut().insert("Authorization", val);
            // Announce the process-wide instance group so the server can group
            // this process's connections and share one random hostname across
            // them. Non-secret; safe as a plain header.
            if let Ok(g) = HeaderValue::from_str(&spec.instance_group) {
              req.headers_mut().insert("x-aperio-instance", g);
            }
            // The release this binary is, so the server can refuse a pairing
            // it does not support at connect time rather than letting the
            // connection come up and misbehave somewhere deeper (#113).
            // Non-secret, and a server too old to read it simply ignores it.
            if let Ok(v) = HeaderValue::from_str(env!("CARGO_PKG_VERSION")) {
              req
                .headers_mut()
                .insert(aperio_config::pairing::CLIENT_RELEASE_HEADER, v);
            }
            Ok(req)
          }
          Err(e) => Err(format!("Invalid token header format: {:?}", e)),
        }
      }
      Err(e) => Err(format!("Failed to construct connection request: {:?}", e)),
    };

    match ws_req {
      Ok(req) => {
        // Built from the default rather than as a literal: the config struct
        // is non-exhaustive, so its future fields keep their own defaults.
        let mut ws_config = WebSocketConfig::default();
        ws_config.max_message_size = Some(spec.max_message_size);
        ws_config.max_frame_size = Some(spec.max_message_size);
        // Dial under the cancel signal so a shutdown aborts an in-progress
        // connect/handshake immediately instead of waiting for it to finish
        // (a half-open server can otherwise stall the handshake with no
        // timeout, keeping the service alive past cancel).
        let connect_fut = crate::dial::connect_ws(req, Some(ws_config));
        tokio::pin!(connect_fut);
        let connect_result = tokio::select! {
          _ = cancel.changed() => break 'outer,
          r = &mut connect_fut => r,
        };
        match connect_result {
          // Labelled so a refusal below can give this connection up without
          // giving up the *loop*. `continue` reads like the right word for
          // "retry", and it is the wrong one here: the backoff, the jitter and
          // the failover to the next server all live at the tail of the loop,
          // and skipping them turned a refused connection into a dial as fast
          // as the network allows, forever, with an error line per attempt.
          Ok((ws_stream, response)) => 'connection: {
            info!("[{}] Successfully connected to Aperio Server!", label);
            // The half of the window only this side can judge (#113). A server
            // cannot know it is too old for something a future client wants,
            // so the client compares what the server announced against its own
            // floor. Held back rather than served: a service that comes up
            // against a server it does not support is the connection that
            // establishes and then misbehaves, which is what the gate exists
            // to prevent. A server that announces nothing is admitted, since
            // silence predates the header.
            if let Some(refused) = aperio_config::pairing::check(
              response
                .headers()
                .get(aperio_config::pairing::SERVER_RELEASE_HEADER)
                .and_then(|v| v.to_str().ok()),
              aperio_config::pairing::MIN_SUPPORTED_SERVER,
              aperio_config::pairing::Side::Server,
            ) {
              error!("[{}] Refusing to serve: {}", label, refused.message());
              break 'outer;
            }
            // Multiplexing is negotiated, not assumed. A server too old to
            // serve a list of services would read the Ping's singular fields
            // instead, bring up the first service and silently drop the rest:
            // a connection that establishes and then serves less than it was
            // told to, which is the failure the connect-time gate exists to
            // prevent. So the services are held back until a server that says
            // it can carry them answers, and the log line says which side has
            // to move.
            //
            // Absent means old. The header was added with the ability, so a
            // server that does not send it cannot have it, and reading silence
            // as consent is the one mistake that produces the quiet half-serve.
            if multiplexed {
              let announced = response
                .headers()
                .get(crate::protocol::PROTOCOL_HEADER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u32>().ok());
              if announced.is_none_or(|p| p < MIN_MULTIPLEX_PROTOCOL) {
                error!(
                  "[{}] This server speaks tunnel protocol {}, and carrying {} services on one connection (multiplex: true) needs {}. Not serving these {} service(s): upgrade the server to 0.10.0 or newer, or set multiplex: false to give each its own connection. Retrying.",
                  label,
                  announced
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "an older version".to_string()),
                  services.len(),
                  MIN_MULTIPLEX_PROTOCOL,
                  services.len()
                );
                break 'connection;
              }
            }
            // The server announces what this token may open for one service.
            // Published for the siblings waiting on it above; refreshed on
            // every reconnect, so raising the number on the server reaches a
            // running client without restarting it.
            if let Some(permitted) = response
              .headers()
              .get("x-aperio-max-connections")
              .and_then(|v| v.to_str().ok())
              .and_then(|v| v.trim().parse::<u32>().ok())
              .filter(|v| *v > 0)
            {
              ceiling.tx.send_replace(Some(permitted));
            }
            // Servers this one says a client may fall back to
            // (planned_features #52). Appended after the ones this client's
            // own config names, never replacing them: the operator's list
            // decides the order, and this is advice from one of the servers on
            // it. Learned per connection, so a migration set up on the server
            // reaches a running client without a restart.
            //
            // The rotation is round-robin and wraps, so an alternate is never
            // a one-way door: a client that failed over keeps coming back to
            // try the primary, and a server that was briefly restarting gets
            // its clients back on the next pass.
            // What this server accepts as a client-declared visitor gate,
            // read from the handshake response because that is the only
            // moment where the answer is known and nothing has been declared
            // yet. The reasoning lives on `negotiate_visitor_gate`.
            //
            // Asked once per service, because the answer depends on the policy
            // each one was written with. A service whose gate this server
            // cannot carry is withheld and the rest are served: on a connection
            // of its own that means the connection is retried, which is what it
            // always meant, and on a shared one it means the sibling services
            // are not taken down over a gate that is not theirs.
            let announced_methods = response
              .headers()
              .get("x-aperio-visitor-auth-methods")
              .and_then(|v| v.to_str().ok());
            let mut withheld: Vec<usize> = Vec::new();
            let mut negotiated_gates: Vec<Option<Vec<aperio_config::AuthMethodSpec>>> =
              Vec::with_capacity(services.len());
            for (i, service) in services.iter().enumerate() {
              let service_label = services[i].spec.label();
              let gate = match negotiate_visitor_gate(
                announced_methods,
                service.visitor_auth_policy.as_ref(),
              ) {
                GateNegotiation::Scalar => None,
                GateNegotiation::Methods(methods) => Some(methods),
                GateNegotiation::Unsupported { wanted, accepted } => {
                  // Withholding it is the only safe answer: this client cannot
                  // serve the route under the gate that was written, and
                  // serving it without one would be worse than being absent.
                  if accepted.is_empty() {
                    // The server named no method at all, which it does for a
                    // connection that may not declare a gate rather than for one
                    // whose method it does not know. Its own log says which
                    // token and why; from here the honest thing is to name the
                    // usual cause without asserting it.
                    error!(
                      "[{}] This server accepts no client-declared visitor gate on this connection, which is what it answers when the token may not control the visitor gate. Not serving this service: grant the token that permission, or write the gate on the server.",
                      service_label
                    );
                  } else {
                    error!(
                      "[{}] This server does not accept `{}` as a client-declared visitor gate (it accepts: {}). Not serving this service: upgrade the server, or write a gate it understands.",
                      service_label,
                      wanted.join(", "),
                      accepted.join(", ")
                    );
                  }
                  withheld.push(i);
                  None
                }
                GateNegotiation::TooOldForPolicy { wanted } => {
                  // Same refusal, different reason: the server is old enough
                  // that it never says what it accepts, and a gate of this shape
                  // can only be sent in a field it does not read. It would
                  // ignore that field, see no gate, and serve the route open.
                  error!(
                    "[{}] This server is too old to be told an `auth:` of this shape (`{}`): it can only be given a single `user:password`. Not serving this service: upgrade the server, or write the gate as one credential.",
                    service_label,
                    wanted.join(", ")
                  );
                  withheld.push(i);
                  None
                }
              };
              negotiated_gates.push(gate);
            }
            // Nothing left to serve, so there is no connection to hold open.
            // Retried rather than abandoned, for the reason each refusal above
            // gives: every one of them is about what *this* server accepts, and
            // the next reconnect may reach a different one.
            if withheld.len() == services.len() {
              warn!(
                "[{}] No service on this connection can be served by this server. Retrying.",
                label
              );
              break 'connection;
            }
            let announced_services: Vec<usize> = (0..services.len())
              .filter(|i| !withheld.contains(i))
              .collect();
            if !withheld.is_empty() {
              warn!(
                "[{}] Serving {} of this connection's {} services; the rest are held back for the reasons above",
                label,
                announced_services.len(),
                services.len()
              );
            }
            if let Some(learned) = response
              .headers()
              .get("x-aperio-alternate-servers")
              .and_then(|v| v.to_str().ok())
            {
              for url in learned.split(',').map(str::trim) {
                if (url.starts_with("ws://") || url.starts_with("wss://"))
                  && !ws_urls.iter().any(|u| u == url)
                  && ws_urls.len() < MAX_SERVER_URLS
                {
                  info!("[{}] Server announced an alternate: {}", label, url);
                  ws_urls.push(url.to_string());
                }
              }
            }
            let connected_at = Instant::now();
            // Announce every service this connection carries to anything
            // waiting on one via `depends_on`. Keyed by service name, so every
            // connection of a parallel pool announces the same name and the
            // first one to connect is enough; a connection carrying several
            // announces each, since a service that is up is up whether it has
            // a socket to itself or shares one.
            let announced_ready: Vec<String> = services
              .iter()
              .filter_map(|s| s.spec.name.clone())
              .inspect(|name| {
                shared.ready_services.send_modify(|live| {
                  *live.entry(name.clone()).or_insert(0) += 1;
                });
              })
              .collect();
            // Every established connection after the first is a reconnect,
            // and the count is what tells a flapping link from a quiet one:
            // two clients both answering pings look identical otherwise.
            if connected_once {
              health_report.reconnected();
            }
            connected_once = true;
            let (ws_sender, mut ws_receiver) = ws_stream.split();

            // Channel to write messages to the WebSocket
            let (tx_write, rx_write) = mpsc::channel::<Message>(100);

            // OTel bridge, tunnel transport: one task per connection drains the
            // process-wide queue onto this socket. The queue is behind a mutex,
            // so exactly one live connection holds it; when this one ends the
            // lock is released and the next connection picks the queue up where
            // it was left, which is what makes exports survive a reconnect.
            let otel_task = shared.otel_exports.clone().map(|queue| {
              let tx = tx_write.clone();
              tokio::spawn(async move {
                use base64::Engine;
                let mut rx = queue.lock().await;
                while let Some(export) = rx.recv().await {
                  let msg = TunnelMessage::OtlpExport {
                    signal: export.signal.to_string(),
                    data: base64::engine::general_purpose::STANDARD.encode(&export.payload),
                  };
                  let Ok(json) = serde_json::to_string(&msg) else {
                    continue;
                  };
                  if tx.send(Message::Text(json.into())).await.is_err() {
                    break;
                  }
                }
              })
            });

            // Ends the socket loop from outside it, saying why.
            let (abort_tx, mut abort_rx) = mpsc::channel::<AbortReason>(1);

            // Track connection liveness via Pong response time
            let last_pong_time = Arc::new(Mutex::new(Instant::now()));

            // Active WebSocket proxy streams: stream_id → handle
            let active_ws_streams: Arc<Mutex<HashMap<String, WsStreamHandle>>> =
              Arc::new(Mutex::new(HashMap::new()));

            // Active raw TCP tunnel streams: stream_id → handle
            let active_tcp_streams: Arc<Mutex<HashMap<String, TcpStreamHandle>>> =
              Arc::new(Mutex::new(HashMap::new()));

            // Active UDP relay streams: stream_id → handle
            let active_udp_streams: Arc<Mutex<HashMap<String, UdpStreamHandle>>> =
              Arc::new(Mutex::new(HashMap::new()));

            // Outgoing compression is enabled after the server's offer is Acked.
            let compress_out = Arc::new(AtomicBool::new(false));

            // Spawn task to handle WebSocket writes.
            let compress_out_writer = compress_out.clone();
            let (finish_tx, finish_rx) = tokio::sync::oneshot::channel::<()>();
            let mut writer_task = tokio::spawn(writer::run_writer(
              ws_sender,
              rx_write,
              finish_rx,
              compress_out_writer,
            ));

            // Spawn task for heartbeat (Ping every 5 seconds & liveness check)
            // Every connection subscribes with the full filter set. The
            // server collapses the copies to one per client process, and a
            // connection that never subscribed would leave the process deaf
            // the moment its sibling dropped.
            {
              let bus = shared.messages.clone();
              let tx = tx_write.clone();
              // This connection's own id, not the service label: a service
              // with `connections: N` shares one label across N connections,
              // and the bus keys its writers by connection.
              let connection_id = spec.client_id.clone();
              tokio::spawn(async move {
                bus.attach(&connection_id, tx.clone()).await;
                bus.subscribe_on(&tx).await;
              });
            }
            let tx_ping = tx_write.clone();
            let client_id_ping = spec.client_id.clone();
            let last_pong_time_ping = last_pong_time.clone();
            let abort_tx_ping = abort_tx.clone();
            let cancel_ping = cancel.clone();
            let self_health_ping = health_report.clone();
            let shared_ping = shared.clone();
            // The connection drains once, so the window is the longest any of
            // its services asked for: cutting one short to honour another's
            // shorter number would kill in-flight requests the file promised
            // to let finish, and the drain is bounded by what is actually in
            // flight rather than by running out the clock.
            let drain_secs = services
              .iter()
              .map(|s| s.spec.reload_drain_secs)
              .max()
              .unwrap_or_default();
            let reload_drain_ping = Duration::from_secs(drain_secs);
            let client_key_ping = device_key::device_key();
            let drain_secs_ping = Some(drain_secs);
            // Everything the heartbeat says about a service, built once per
            // service and per connection: these are the values the config
            // settled, and a config change respawns the connection rather than
            // editing them underneath it.
            //
            // Built as `ServiceDecl` values rather than as loose locals because
            // that is the shape the wire wants, and because there is now more
            // than one of them. Only three fields move while the connection is
            // up, and the loop below is the one place that patches them.
            let decl_templates: Vec<crate::protocol::ServiceDecl> = announced_services
              .iter()
              .map(|&i| {
                let s = &services[i].spec;
                crate::protocol::ServiceDecl {
                  service: s.name.clone(),
                  service_custom_name: s.custom_name.clone(),
                  path_bind: s.path.clone(),
                  hostname_bind: s.hostnames.first().cloned(),
                  hostname_binds: s.hostnames.clone(),
                  // Patched per heartbeat from this service's adaptive
                  // controller; the configured number is what it starts at.
                  max_concurrent: s.max_concurrent,
                  tcp: s.tcp_target.is_some(),
                  // Patched per heartbeat: this is the pair the probe writes.
                  backend_healthy: false,
                  backend_probed: false,
                  priority: s.priority,
                  bandwidth_bps: s.bandwidth_bps,
                  public: s.public,
                  server_side_target: s.server_side_target.clone(),
                  visitor_auth: s.visitor_auth.clone(),
                  visitor_auth_methods: negotiated_gates[i].clone(),
                  allowed_ips: s.allowed_ips.clone(),
                  tunnels: s.tunnels.clone(),
                  cache: s.cache,
                  resilience: s.resilience,
                  no_capture: !s.capture,
                  max_request_body: s.max_request_body,
                  response_timeout: s.response_timeout,
                  webhook_inbox: s.webhook_inbox,
                  denied: s.denied.clone(),
                  scaling: s.scaling.clone(),
                  // Patched per heartbeat: what the pool is running right now,
                  // not what it may grow to. The dashboard reads this as "this
                  // service has N connections", and an elastic pool sitting at
                  // its floor would otherwise claim its ceiling and look like
                  // connections had gone missing. Read inside the heartbeat
                  // because an elastic pool moves: taken once here it would
                  // report the size the pool happened to be when this
                  // connection opened, for as long as the connection lived.
                  connections: None,
                  // Only meaningful as a range; a fixed `connections: N`
                  // announces nothing rather than a min and max that are the
                  // same number.
                  connections_min: (s.connections_min < s.connections).then_some(s.connections_min),
                  connections_max: (s.connections_min < s.connections).then_some(s.connections),
                  config_notes: s.config_notes.clone(),
                  metrics_labels: s.metrics_labels.clone(),
                }
              })
              .collect();
            // The moving parts, in the same order as the templates.
            let live_ping: Vec<LiveDecl> = announced_services
              .iter()
              .map(|&i| LiveDecl {
                health: services[i].health.clone(),
                adaptive: services[i].adaptive.clone(),
                pool: services[i].spec.pool_load.clone(),
                connections_configured: services[i].spec.connections,
              })
              .collect();
            // Any service's health flipping is worth a heartbeat now rather
            // than up to 5s later, so the wait below listens to all of them.
            let health_changed_ping: Vec<Arc<tokio::sync::Notify>> = announced_services
              .iter()
              .map(|&i| services[i].health.changed.clone())
              .collect();

            let ping_task = tokio::spawn(async move {
              // The first Ping goes out immediately: it announces the binds,
              // version/protocol, and health before any traffic is routed.
              loop {
                // The supervisor asked for this connection to end: a config
                // reload, a shutdown, or an elastic pool giving it back. The
                // cancel signal does not say which, and neither do these
                // lines: whichever of the three it was has already logged its
                // own reason, and guessing here is how a pool retirement came
                // to announce a configuration change that never happened.
                if *cancel_ping.borrow() {
                  // Announce the drain before dropping the socket. Without
                  // this, ending a connection killed whatever was in flight:
                  // the visitor saw a failure caused by a change that was
                  // meant to be invisible to them. `Draining` stops the
                  // server dispatching anything new here, which is what makes
                  // the wait below terminate rather than chase a moving
                  // target.
                  if reload_drain_ping.is_zero() {
                    info!("Closing this connection...");
                  } else {
                    info!("Draining before closing this connection...");
                    if let Ok(json) = serde_json::to_string(&TunnelMessage::Draining {}) {
                      let _ = tx_ping.send(Message::Text(json.into())).await;
                    }
                    drain_inflight_for(&shared_ping, reload_drain_ping).await;
                  }
                  let _ = abort_tx_ping.send(AbortReason::Requested).await;
                  break;
                }

                // Check last Pong receipt time (max 15s limit)
                let elapsed = {
                  let lock = last_pong_time_ping.lock().await;
                  lock.elapsed()
                };
                if elapsed > Duration::from_secs(15) {
                  warn!(
                    "Liveness check failed: no Pong received for {} seconds. Resetting connection.",
                    elapsed.as_secs()
                  );
                  let _ = abort_tx_ping.send(AbortReason::Liveness).await;
                  break;
                }

                // This heartbeat's description of every service on the
                // connection: the settled values, with the three that move
                // read now so everything in the frame describes one moment.
                let mut decls = decl_templates.clone();
                for (decl, live) in decls.iter_mut().zip(&live_ping) {
                  // One read, so the pair in this heartbeat is one observation.
                  let reported = live.health.report();
                  decl.backend_healthy = reported.0;
                  decl.backend_probed = reported.1;
                  // Not the configured number: what this service will take
                  // right now, which adaptive concurrency may have lowered.
                  decl.max_concurrent = live
                    .adaptive
                    .as_ref()
                    .map(|a| a.announced())
                    .or(decl.max_concurrent);
                  decl.connections = Some(live.pool.open().unwrap_or(live.connections_configured));
                }
                // Likewise for the self-reported figures: everything in this
                // heartbeat describes the same moment.
                let (rtt_ms, jitter_ms, reconnects) = self_health_ping.link();
                // The first service stands for the connection in the singular
                // fields, which is what it has always done and what every
                // server before v8 reads. The list is sent alongside only when
                // there is more than one service to describe, because a server
                // that reads it treats it as authoritative and a one-entry list
                // says nothing the singular fields do not: what it would
                // change is which servers can read the Ping at all.
                let first = &decls[0];
                let ping_msg = TunnelMessage::Ping {
                  services: (decls.len() > 1).then(|| decls.clone()),
                  cpu_percent: self_health_ping.cpu_percent(),
                  rss_bytes: crate::health_report::rss_bytes(),
                  rtt_ms,
                  jitter_ms,
                  reconnects: Some(reconnects),
                  client_id: client_id_ping.clone(),
                  timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                  path_bind: first.path_bind.clone(),
                  hostname_bind: first.hostname_bind.clone(),
                  hostname_binds: first.hostname_binds.clone(),
                  max_concurrent: first.max_concurrent,
                  tcp: first.tcp,
                  version: Some(env!("CARGO_PKG_VERSION").to_string()),
                  protocol: Some(PROTOCOL_VERSION),
                  backend_healthy: first.backend_healthy,
                  backend_probed: first.backend_probed,
                  priority: first.priority,
                  bandwidth_bps: first.bandwidth_bps,
                  service: first.service.clone(),
                  service_custom_name: first.service_custom_name.clone(),
                  // No `server_side_target` here on purpose. These singular
                  // fields are the shim an older server reads, and an older
                  // server cannot honour this: it would relay instead, which
                  // is the silent fallback the feature exists to avoid. The
                  // ask travels only in the `services` list, where a server
                  // that understands it will find it.
                  public: first.public,
                  visitor_auth: first.visitor_auth.clone(),
                  visitor_auth_methods: first.visitor_auth_methods.clone(),
                  allowed_ips: first.allowed_ips.clone(),
                  tunnels: first.tunnels.clone(),
                  cache: first.cache,
                  resilience: first.resilience,
                  no_capture: first.no_capture,
                  max_request_body: first.max_request_body,
                  response_timeout: first.response_timeout,
                  client_key: client_key_ping.clone(),
                  webhook_inbox: first.webhook_inbox,
                  denied: first.denied.clone(),
                  scaling: first.scaling.clone(),
                  connections: first.connections,
                  connections_min: first.connections_min,
                  connections_max: first.connections_max,
                  metrics_labels: first.metrics_labels.clone(),
                  drain_secs: drain_secs_ping,
                  config_notes: first.config_notes.clone(),
                };
                if let Ok(ping_str) = serde_json::to_string(&ping_msg) {
                  // Timed from the moment it is queued, which is the same
                  // queue every other frame waits in: a round trip that
                  // excluded the writer's backlog would report the link as
                  // healthy while the connection was the thing falling behind.
                  self_health_ping.ping_sent();
                  if tx_ping.send(Message::Text(ping_str.into())).await.is_err() {
                    break;
                  }
                }
                // Wake early when any service's backend health verdict flips,
                // so a change is reported at once rather than up to 5s later.
                // The futures are built together and raced as one, which is
                // what makes a notify on the last service as prompt as one on
                // the first.
                let flipped = futures_util::future::select_all(
                  health_changed_ping
                    .iter()
                    .map(|n| Box::pin(n.notified()))
                    .collect::<Vec<_>>(),
                );
                tokio::select! {
                  _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                  _ = flipped => {}
                }
              }
            });

            // Pause switches for the streams this connection produces
            // (server flow control, protocol v3). Per connection: stream ids
            // do not survive a reconnect.
            let stream_pauses = crate::flow::PauseRegistry::default();

            // How a request is forwarded, one per service. Everything in it
            // is the service's own, the backend URL and its TLS floor, the
            // timeouts, the path bind, the header rules and the circuit
            // breaker, so a connection carrying several needs one each: built
            // once for the connection, every service on it would have been
            // proxied to the first one's backend under the first one's rules.
            let forward_ctxs: Vec<Arc<ForwardContext>> = services
              .iter()
              .map(|s| Arc::new(forward_context(&s.spec, &tx_write, &stream_pauses)))
              .collect();

            // Protocol version the server announced via Pong; v2 enables
            // binary chunk frames and streamed request bodies.
            let server_protocol = Arc::new(std::sync::atomic::AtomicU32::new(1));

            // Streamed request bodies in flight: request id → chunk feeder.
            let active_request_streams: Arc<Mutex<HashMap<String, RequestBodyFeeder>>> =
              Arc::new(Mutex::new(HashMap::new()));

            // Read messages from Server. The loop's own state is `Dispatch`;
            // what comes back is the two things the reconnect loop below acts
            // on.
            let dispatch = dispatch::Dispatch {
              services: &services,
              shared: &shared,
              label: &label,
              forward_ctxs: &forward_ctxs,
              announced_services: &announced_services,
              health_report: &health_report,
              tx_write: tx_write.clone(),
              last_pong_time: last_pong_time.clone(),
              server_protocol: server_protocol.clone(),
              compress_out: compress_out.clone(),
              stream_pauses: stream_pauses.clone(),
              max_message_size: spec.max_message_size,
              active_request_streams: active_request_streams.clone(),
              active_ws_streams: active_ws_streams.clone(),
              active_tcp_streams: active_tcp_streams.clone(),
              active_udp_streams: active_udp_streams.clone(),
            };
            let dispatch::Ended {
              closed_on_request,
              server_announced_shutdown,
            } = dispatch.run(&mut abort_rx, &mut ws_receiver).await;

            // Cleanup tasks on connection loss.
            //
            // Asked to finish rather than aborted, so anything already queued
            // reaches the socket. Bounded, because a connection that is gone
            // will never accept the writes and this must not become the thing
            // that holds a shutdown open.
            let _ = finish_tx.send(());
            if tokio::time::timeout(Duration::from_secs(2), &mut writer_task)
              .await
              .is_err()
            {
              writer_task.abort();
            }
            // Releases the export queue for the next connection to pick up.
            if let Some(task) = otel_task {
              task.abort();
            }
            ping_task.abort();
            // This connection is no longer live, so it no longer counts
            // towards the service being up. Nothing used to take a name back
            // out, which made `depends_on` a claim about the past: a service
            // that connected once and then went away was still reported ready
            // to anything that started later.
            for name in &announced_ready {
              shared.ready_services.send_modify(|live| {
                if let Some(count) = live.get_mut(name) {
                  *count -= 1;
                  if *count == 0 {
                    live.remove(name);
                  }
                }
              });
            }
            if closed_on_request {
              info!("[{}] Connection closed.", label);
            } else {
              warn!("[{}] Connection to server lost.", label);
            }

            // A connection that survived for a while counts as healthy:
            // start the next retry sequence from the base delay again.
            if connected_at.elapsed() >= Duration::from_secs(RECONNECT_STABLE_SECS) {
              reconnect_attempt = 0;
            }
            fast_reconnect = server_announced_shutdown;
          }
          Err(e) => {
            use tokio_tungstenite::tungstenite::Error as WsError;
            if let WsError::Http(resp) = &e {
              let code = resp.status().as_u16();
              if code == 401 || code == 403 {
                error!(
                  "[{}] Authentication failed (HTTP {}): the server rejected the tunnel token. Check --server-token / APERIO_SERVER_TOKEN / yaml server.token, it may be wrong, expired, or revoked.",
                  label, code
                );
              } else if code == 426 {
                // The pairing gate (#113). Its whole value is the sentence in
                // the body, which names both versions and which side to
                // upgrade, so reporting the bare status would throw away the
                // answer and leave a retry loop with no visible cause.
                let detail = resp
                  .body()
                  .as_ref()
                  .and_then(|b| std::str::from_utf8(b).ok())
                  .map(str::trim)
                  .filter(|d| !d.is_empty())
                  .unwrap_or("this client and this server are not a supported pairing");
                error!("[{}] Refused by the server: {}", label, detail);
              } else {
                error!(
                  "[{}] Server rejected the connection with HTTP {}.",
                  label, code
                );
              }
            } else {
              error!("[{}] Failed to connect to server: {}.", label, e);
            }
          }
        }
      }
      Err(e) => {
        error!("WebSocket configuration request building error: {}", e);
      }
    }

    // This connection's writer is gone: take it out of the bus so a publish
    // is not handed to a dead channel, and so "no tunnel connection is up"
    // stays a true statement when every one of them has dropped.
    shared.messages.detach(&spec.client_id).await;

    exit_if_shutting_down(&shared).await;
    if *cancel.borrow() {
      break 'outer;
    }
    let delay = if fast_reconnect {
      // The server told us it is restarting: come back right away (with a
      // little jitter so a fleet does not stampede), and reset the backoff
      // so a slow restart falls back to the normal schedule from the start.
      fast_reconnect = false;
      reconnect_attempt = 0;
      let d = fast_reconnect_delay();
      info!(
        "[{}] Server shutdown announced; reconnecting in {:.2} seconds...",
        label,
        d.as_secs_f64()
      );
      d
    } else {
      reconnect_attempt = reconnect_attempt.saturating_add(1);
      let d = reconnect_delay(reconnect_attempt);
      info!(
        "[{}] Retrying connection in {:.1} seconds (attempt {})...",
        label,
        d.as_secs_f64(),
        reconnect_attempt
      );
      d
    };
    // Cross-server failover: after a failed/dropped connection, try the next
    // server on the next attempt (no-op with a single server).
    if ws_urls.len() > 1 {
      server_idx = server_idx.wrapping_add(1);
    }
    tokio::select! {
      _ = tokio::time::sleep(delay) => {}
      _ = cancel.changed() => break 'outer,
      // The loop head does the exiting; this arm only cuts the wait short.
      _ = shutdown_requested(&shared) => {}
    }
  }

  for t in probe_tasks {
    t.abort();
  }
  info!("[{}] Service stopped.", label);
}

/// One connect-probe of the wait-for-backend gate: true when the backend
/// accepts a TCP (or unix-socket) connection. Deliberately connection-level
/// only, the gate answers "is anything listening yet", not "is it healthy"
/// (that is `target_health`'s job).
async fn backend_accepts_connections(target: &str) -> bool {
  let attempt = async {
    #[cfg(unix)]
    if let Some(path) = crate::proxy::unix::unix_socket_path(target) {
      return tokio::net::UnixStream::connect(path).await.is_ok();
    }
    let wire = target
      .replacen("h2c://", "http://", 1)
      .replacen("h2://", "https://", 1);
    let Ok(url) = url::Url::parse(&wire) else {
      return false;
    };
    let Some(host) = url.host_str() else {
      return false;
    };
    let Some(port) = url.port_or_known_default() else {
      return false;
    };
    tokio::net::TcpStream::connect((host, port)).await.is_ok()
  };
  tokio::time::timeout(Duration::from_secs(3), attempt)
    .await
    .unwrap_or(false)
}

/// First retry delay of the reconnect backoff.
const RECONNECT_BASE_DELAY_MS: u64 = 1_000;
/// Upper bound for the reconnect backoff.
const RECONNECT_MAX_DELAY_MS: u64 = 60_000;
/// A connection lasting at least this long resets the backoff counter.
const RECONNECT_STABLE_SECS: u64 = 30;

/// Exponential reconnect backoff with jitter: the deterministic delay doubles
/// per attempt (1s, 2s, 4s, ... capped at 60s) and the returned value is
/// drawn from [cap/2, cap] so simultaneously disconnected clients spread out
/// instead of reconnecting in lockstep. The jitter is derived from the clock
/// to avoid pulling in a RNG dependency.
fn reconnect_delay(attempt: u32) -> Duration {
  let doublings = attempt.saturating_sub(1).min(6); // 2^6 * 1s covers the 60s cap
  let cap = (RECONNECT_BASE_DELAY_MS << doublings).min(RECONNECT_MAX_DELAY_MS);
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .subsec_nanos() as u64;
  let jitter = nanos % (cap / 2 + 1);
  Duration::from_millis(cap / 2 + jitter)
}

/// Reconnect delay used after the server announces a graceful shutdown:
/// 100–500 ms of clock-derived jitter, no exponential backoff. Short enough
/// that a rolling restart is barely visible, jittered enough that a fleet of
/// clients does not stampede the returning server.
fn fast_reconnect_delay() -> Duration {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .subsec_nanos() as u64;
  Duration::from_millis(100 + nanos % 401)
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
