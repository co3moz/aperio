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
//! - [`spec`] is what a service is once the config has been resolved, plus the
//!   process-wide state every service task shares and the lifecycle gates
//!   (`depends_on`, drain, idle retirement) that run around it.
//! - [`connect`] is what has to be settled per service before a connection can
//!   serve it: its backend health, the visitor gate this server will accept,
//!   the connection ceiling, and how a request reaches its backend.
//! - [`relay`] is the read loop's delivery side, the four helpers that hand a
//!   frame to a TCP, UDP or WebSocket stream without blocking the loop.
//! - [`device_key`] is the trust-on-first-use key this client identifies with.
//!
//! Everything is re-exported, so `crate::service::Thing` resolves where it did.

pub(crate) mod connect;
pub(crate) mod device_key;
pub(crate) mod relay;
pub(crate) mod spec;

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
use tokio::sync::{Mutex, Semaphore, mpsc, watch};
use tokio_tungstenite::tungstenite::{
  client::IntoClientRequest,
  http::HeaderValue,
  protocol::{Message, WebSocketConfig},
};
use tracing::{debug, error, info, warn};

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

/// Drains the outgoing queue onto the tunnel socket until the socket fails or
/// the connection is asked to finish.
///
/// Extracted from the connection loop so the one decision it makes can be
/// tested: what happens to messages that are already queued when the
/// connection ends. It used to be aborted, and a response reaches this queue
/// *before* the request task decrements the in-flight counter that a drain
/// waits on. So a configuration reload could pass its drain, abort the
/// writer, and drop a response the visitor was owed, which is precisely what
/// the drain was added to prevent.
///
/// `finish` asks for "send what is queued, then stop", not "stop now": the
/// select below is biased so a queued message always wins the race with it.
pub(crate) async fn run_writer<S>(
  mut sink: S,
  mut queue: mpsc::Receiver<Message>,
  finish: tokio::sync::oneshot::Receiver<()>,
  compress_out: Arc<AtomicBool>,
) where
  S: futures_util::SinkExt<Message> + Unpin,
  <S as futures_util::Sink<Message>>::Error: std::fmt::Debug,
{
  let mut finish = finish;
  let transform = |msg: Message| match msg {
    Message::Text(t) if compress_out.load(Ordering::SeqCst) => {
      Message::Binary(compress_frame(&t).into())
    }
    // A full-response frame carries a body that used to travel inside a text
    // frame and be compressed with it. Compressed here rather than where it
    // is built, so the negotiated flag stays in one place, and only when
    // deflating wins: for an already-compressed body it does not, and the
    // frame goes out as it is.
    Message::Binary(b)
      if compress_out.load(Ordering::SeqCst) && b.first() == Some(&FRAME_RESPONSE_FULL) =>
    {
      match decode_binary_frame(&b) {
        Some((_, id, payload)) => match crate::protocol::deflate_payload(payload) {
          Some(deflated) => match encode_binary_frame(FRAME_RESPONSE_FULL_ZLIB, id, &deflated) {
            Some(frame) => Message::Binary(frame.into()),
            None => Message::Binary(b),
          },
          None => Message::Binary(b),
        },
        None => Message::Binary(b),
      }
    }
    other => other,
  };
  // Everything already queued behind a message rides the same flush: at bulk
  // throughput each message used to pay its own (a syscall per frame), and
  // the messages are already whole frames, so batching them costs no latency.
  'writer: loop {
    let next_msg = tokio::select! {
      biased;
      msg = queue.recv() => msg,
      _ = &mut finish => None,
    };
    let Some(msg) = next_msg else {
      break 'writer;
    };
    let mut msg = transform(msg);
    while let Ok(next) = queue.try_recv() {
      if let Err(e) = sink.feed(msg).await {
        error!("Error writing to server socket: {:?}", e);
        break 'writer;
      }
      msg = transform(next);
    }
    if let Err(e) = sink.send(msg).await {
      error!("Error writing to server socket: {:?}", e);
      break 'writer;
    }
  }
  // Whatever the loop stopped for, the socket's own buffer may still hold
  // bytes that were fed but never flushed.
  let _ = sink.flush().await;
}

/// Runs one tunnel connection, carrying every service in `specs`, until the
/// process shuts down or `cancel` fires.
///
/// `specs` is one service in the ordinary shape and several under `multiplex:
/// true`; `healths` is that same list's backend-health state, index for index,
/// created by the supervisor so a service's parallel connections share it.
/// `run_probe` is true only for the connection that owns those probes, the
/// others just report what they write.
pub(crate) async fn run_service(
  specs: Vec<ServiceSpec>,
  shared: Shared,
  mut cancel: watch::Receiver<bool>,
  healths: Vec<BackendHealth>,
  run_probe: bool,
  connection_index: u32,
  ceiling: ConnectionCeiling,
) {
  // The connection's own view. Everything about the socket, the dial, the
  // client id and the heartbeat is the first service's, because a connection
  // carrying several is still one connection and one of its services has to
  // stand for it. What a *request* is about is resolved per request, from the
  // service the server names in the frame.
  // The two lists are one list written as two arguments, and every index below
  // reads them together. Checked once at the door, because the alternative is
  // an out-of-range panic six hundred lines in, at whichever index happens to
  // be reached first, with nothing at the crash site saying what the caller
  // got wrong.
  if specs.is_empty() || healths.len() != specs.len() {
    error!(
      "Refusing to open a connection: it was given {} service(s) and {} health state(s), which have to be the same non-empty list",
      specs.len(),
      healths.len()
    );
    return;
  }
  let spec = specs[0].clone();
  // A connection carrying several services is labelled by how many, not by the
  // first of them: every line about the connection would otherwise read as
  // being about one service, and the other services' own lines already carry
  // their own labels.
  let multiplexed = specs.len() > 1;
  let label = if multiplexed {
    format!("{} services", specs.len())
  } else {
    spec.label()
  };

  // Lifecycle gates, before anything is dialed. Only the first connection of a
  // pool waits: the others are the same service, and making each of them sit
  // through the same delay would turn a five-second stagger into a
  // five-second-per-connection one.
  //
  // A multiplexed connection waits for every one of its services: they are
  // opening one socket together, so the last one that is ready is when it can
  // open. The waits run in sequence and the delays do not add up in a way that
  // matters, `depends_on` is a shared grace period rather than a per-service
  // one, and `startup_delay` is taken as the longest rather than the sum.
  if connection_index == 1 {
    let depends_on: Vec<String> = {
      let mut all: Vec<String> = specs.iter().flat_map(|s| s.depends_on.clone()).collect();
      // A service of this connection cannot wait for a service of this
      // connection: nothing would ever come up. Dropped rather than refused,
      // because the file is not wrong, it is describing an order that
      // multiplexing has made moot by putting both on one socket.
      all.retain(|d| !specs.iter().any(|s| s.name.as_deref() == Some(d.as_str())));
      all.sort();
      all.dedup();
      all
    };
    let startup_delay = specs.iter().map(|s| s.startup_delay).max().unwrap_or(0);
    if !depends_on.is_empty() {
      let missing = await_dependencies(&shared, &depends_on).await;
      if !missing.is_empty() {
        warn!(
          "[{}] depends_on: {} did not come up within {}s; starting anyway",
          label,
          missing.join(", "),
          DEPENDS_ON_GRACE.as_secs()
        );
      }
    }
    if startup_delay > 0 {
      info!(
        "[{}] startup_delay: waiting {}s before opening the tunnel",
        label, startup_delay
      );
      tokio::time::sleep(Duration::from_secs(startup_delay)).await;
    }
  }

  // Connections beyond the first wait for the server's announced ceiling
  // before opening a socket. Five seconds is the whole budget: past that the
  // server is either old (no announcement) or slow, and in both cases trying
  // is better than a connection that never happens.
  if connection_index > 1
    && let Some(permitted) = ceiling.learned(Duration::from_secs(5)).await
    && connection_index > permitted
  {
    warn!(
      "[{}] The server permits {} parallel connection(s) for this service; \
       connection {} stands down. Raise max_connections_per_service on the server \
       (or the token's max_connections) to use more.",
      label, permitted, connection_index
    );
    return;
  }

  // Backend health is per service and shared across a service's parallel
  // connections (created once by the supervisor, one per spec). This connection
  // reports every one of them in its heartbeat and, when it owns the probes,
  // drives the probe/gate that updates each.
  let probe_tasks: Vec<tokio::task::JoinHandle<()>> = if run_probe {
    specs
      .iter()
      .zip(&healths)
      .flat_map(|(s, h)| [spawn_health_probe(s, h), spawn_backend_wait(s, h)])
      .flatten()
      .collect()
  } else {
    Vec::new()
  };
  // Local concurrency guard, one per service and shared across reconnects.
  //
  // Per service rather than per connection because `max_concurrent:` is what a
  // *backend* will take: a connection carrying several would otherwise make one
  // service's slow backend hold up permits another service's requests are
  // waiting for, which is neither what the file says nor a number the server
  // can be told.
  let local_limiters: Vec<Option<Arc<Semaphore>>> = specs
    .iter()
    .map(|s| {
      s.max_concurrent
        .map(|n| Arc::new(Semaphore::new(n as usize)))
    })
    .collect();

  // Adaptive concurrency (#65): the announced number follows backend
  // pressure. It needs the local limiter, because the evidence is how long
  // requests wait for one of its permits, and it is that number being moved.
  // One per service for the same reason the limiter is: the evidence is one
  // backend's, and the number it moves is announced for one service.
  let adaptives: Vec<Option<Arc<crate::adaptive::Adaptive>>> = specs
    .iter()
    .zip(&local_limiters)
    .map(
      |(s, limiter)| match (s.adaptive_concurrency, limiter, s.max_concurrent) {
        (true, Some(limiter), Some(configured)) => {
          let adaptive = Arc::new(crate::adaptive::Adaptive::new(configured, limiter.clone()));
          crate::adaptive::spawn(adaptive.clone(), s.label());
          Some(adaptive)
        }
        (true, _, _) => {
          warn!(
            "[{}] adaptive_concurrency needs max_concurrent to be set; there is no number to move",
            s.label()
          );
          None
        }
        _ => None,
      },
    )
    .collect();

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
  // Cloned once for the whole reconnect loop: the policy is what the file
  // said, and each connection decides separately whether this server accepts
  // it (planned_features #111). One per service, because two services on one
  // connection can be written with different gates and a single negotiation
  // would run one of them under a policy nobody wrote for it.
  let visitor_auth_policies: Vec<Option<aperio_config::AuthSetting>> = specs
    .iter()
    .map(|s| s.visitor_auth_policy.clone())
    .collect();
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
                  specs.len(),
                  MIN_MULTIPLEX_PROTOCOL,
                  specs.len()
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
              Vec::with_capacity(specs.len());
            for (i, policy) in visitor_auth_policies.iter().enumerate() {
              let service_label = specs[i].label();
              let gate = match negotiate_visitor_gate(announced_methods, policy.as_ref()) {
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
            if withheld.len() == specs.len() {
              warn!(
                "[{}] No service on this connection can be served by this server. Retrying.",
                label
              );
              break 'connection;
            }
            let announced_services: Vec<usize> =
              (0..specs.len()).filter(|i| !withheld.contains(i)).collect();
            if !withheld.is_empty() {
              warn!(
                "[{}] Serving {} of this connection's {} services; the rest are held back for the reasons above",
                label,
                announced_services.len(),
                specs.len()
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
            let announced_ready: Vec<String> = specs
              .iter()
              .filter_map(|s| s.name.clone())
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
            let mut writer_task = tokio::spawn(run_writer(
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
            let drain_secs = specs
              .iter()
              .map(|s| s.reload_drain_secs)
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
                let s = &specs[i];
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
                health: healths[i].clone(),
                adaptive: adaptives[i].clone(),
                pool: specs[i].pool_load.clone(),
                connections_configured: specs[i].connections,
              })
              .collect();
            // Any service's health flipping is worth a heartbeat now rather
            // than up to 5s later, so the wait below listens to all of them.
            let health_changed_ping: Vec<Arc<tokio::sync::Notify>> = announced_services
              .iter()
              .map(|&i| healths[i].changed.clone())
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
            let forward_ctxs: Vec<Arc<ForwardContext>> = specs
              .iter()
              .map(|s| Arc::new(forward_context(s, &tx_write, &stream_pauses)))
              .collect();

            // Protocol version the server announced via Pong; v2 enables
            // binary chunk frames and streamed request bodies.
            let server_protocol = Arc::new(std::sync::atomic::AtomicU32::new(1));

            // Streamed request bodies in flight: request id → chunk feeder.
            let active_request_streams: Arc<Mutex<HashMap<String, RequestBodyFeeder>>> =
              Arc::new(Mutex::new(HashMap::new()));

            // Read messages from Server
            let mut version_skew_warned = false;
            let mut server_announced_shutdown = false;
            // Set when this connection is ended deliberately, so the line
            // below the loop reports a close rather than a loss.
            let mut closed_on_request = false;
            loop {
              tokio::select! {
                  reason = abort_rx.recv() => {
                      match reason {
                          Some(AbortReason::Liveness) => {
                              warn!("Liveness timeout triggered. Aborting socket loop.");
                          }
                          // A reload, a shutdown or an elastic pool giving
                          // this connection back. Nothing failed.
                          _ => {
                              closed_on_request = true;
                              debug!("[{}] Closing the socket loop on request.", label);
                          }
                      }
                      break;
                  }
                  _ = shutdown_requested(&shared) => {
                      // Announce drain, let in-flight requests finish, then exit.
                      if let Ok(json) = serde_json::to_string(&TunnelMessage::Draining {}) {
                          let _ = tx_write.send(Message::Text(json.into())).await;
                      }
                      drain_inflight(&shared).await;
                      // Give the Draining frame a moment to flush before closing.
                      tokio::time::sleep(Duration::from_millis(200)).await;
                      crate::remove_pid_file();
                      std::process::exit(0);
                  }
                  msg_res = ws_receiver.next() => {
                      match msg_res {
                          Some(Ok(msg)) => {
                              // A frame yields the envelope text and, for a v6
                              // full-request frame, the body that travelled with it
                              // as bytes rather than base64.
                              let mut frame_body: Option<Vec<u8>> = None;
                              let text_opt = match msg {
                                  Message::Text(t) => Some(t.to_string()),
                                  Message::Binary(b) => {
                                      // v2 binary chunk frames carry a tag byte that never
                                      // collides with zlib streams (0x78).
                                      // Payloads are the tail of the frame and
                                      // the frame is refcounted, so each of
                                      // these is a slice rather than a copy
                                      // (planned_features #42).
                                      match decode_binary_frame(&b) {
                                          Some((FRAME_REQUEST_CHUNK, fid, payload)) => {
                                              let payload = b.slice(b.len() - payload.len()..);
                                              feed_request_chunk(&active_request_streams, fid, payload).await;
                                              None
                                          }
                                          // v7: relay payloads as raw bytes, the same
                                          // deliveries their JSON shapes make below.
                                          Some((crate::protocol::FRAME_TCP_DATA, sid, payload)) => {
                                              deliver_tcp_bytes(&active_tcp_streams, sid, b.slice(b.len() - payload.len()..)).await;
                                              None
                                          }
                                          Some((crate::protocol::FRAME_UDP_DATAGRAM, sid, payload)) => {
                                              deliver_udp_bytes(&active_udp_streams, sid, b.slice(b.len() - payload.len()..)).await;
                                              None
                                          }
                                          Some((crate::protocol::FRAME_WS_DATA_BIN, sid, payload)) => {
                                              deliver_ws_frame(&active_ws_streams, sid, Message::Binary(b.slice(b.len() - payload.len()..))).await;
                                              None
                                          }
                                          // v6: envelope and buffered body in one frame,
                                          // deflated by the server's writer when this
                                          // connection negotiated compression.
                                          Some((tag @ (FRAME_REQUEST_FULL | FRAME_REQUEST_FULL_ZLIB), _, payload)) => {
                                              let max = spec.max_message_size.saturating_mul(4);
                                              let inflated = if tag == FRAME_REQUEST_FULL_ZLIB {
                                                  crate::protocol::inflate_payload(payload, max)
                                              } else {
                                                  None
                                              };
                                              let payload = inflated.as_deref().unwrap_or(payload);
                                              match split_full_response(payload) {
                                                  Some((json, body)) => {
                                                      frame_body = Some(body.to_vec());
                                                      Some(json.to_string())
                                                  }
                                                  None => {
                                                      warn!("Dropped a malformed full-request frame");
                                                      None
                                                  }
                                              }
                                          }
                                          _ => decompress_frame(&b, spec.max_message_size.saturating_mul(4)),
                                      }
                                  }
                                  _ => None,
                              };
                              if let Some(text) = text_opt
                                  && let Ok(tunnel_msg) = serde_json::from_str::<TunnelMessage>(&text)
                              {
                                  match tunnel_msg {
                                          TunnelMessage::Request {
                                              // Which service the server routed this to, by name.
                                              // Absent on a connection carrying one, where there
                                              // is nothing to choose.
                                              service: _service,
                                              id,
                                              method,
                                              uri,
                                              headers,
                                              body,
                                          } => {
                                              // The service the server named. With one, this is that one.
                                              let service_index = service_for(&specs, &announced_services, &_service);
                                              let spec = &specs[service_index];
                                              let ctx = forward_ctxs[service_index].clone();
                                              let limiter = local_limiters[service_index].clone();
                                              let inflight = shared.inflight_requests.clone();
                                              let proto = server_protocol.clone();
                                              let raw_body = frame_body.take();
                                              let pool = spec.pool_load.clone();
                                              inflight.fetch_add(1, Ordering::SeqCst);
                                              pool.enter();
                                              shared.mark_request_activity();

                                              // Handle incoming request concurrently
                                              let adaptive_for_task = adaptives[service_index].clone();
                                              tokio::spawn(async move {
                                                  // Local concurrency guard: even a misbehaving server
                                                  // cannot push more parallel work onto the backend.
                                                  // How long this waits is the evidence adaptive
                                                  // concurrency reads: a queue here means the backend
                                                  // is behind, whatever the host's CPU says.
                                                  let waiting = Instant::now();
                                                  let _permit = match limiter {
                                                      Some(sem) => sem.acquire_owned().await.ok(),
                                                      None => None,
                                                  };
                                                  if let Some(a) = &adaptive_for_task {
                                                      a.record_wait(waiting.elapsed());
                                                  }
                                                  let peer = proto.load(Ordering::Relaxed);
                                                  let binary = peer >= 2;
                                                  // v5: a buffered response goes out as one frame,
                                                  // envelope and body, instead of base64 in JSON.
                                                  let full_body = peer >= 5;
                                                  let response = handle_incoming_request(
                                                      &ctx,
                                                      ForwardRequest { id, method, uri, headers, body, raw_body },
                                                      None,
                                                      binary,
                                                      full_body,
                                                  )
                                                  .await;

                                                  // None = the response was streamed through the tunnel already.
                                                  if let Some(response) = response
                                                      && let Ok(resp_str) = serde_json::to_string(&response)
                                                  {
                                                      let _ = ctx.tunnel_tx.send(Message::Text(resp_str.into())).await;
                                                  }
                                                  inflight.fetch_sub(1, Ordering::SeqCst);
                                                  pool.leave();
                                              });
                                          }
                                          TunnelMessage::RequestStart {
                                              // Which service the server routed this to, by name.
                                              // Absent on a connection carrying one, where there
                                              // is nothing to choose.
                                              service: _service,
                                              id,
                                              method,
                                              uri,
                                              headers,
                                          } => {
                                              // The service the server named. With one, this is that one.
                                              let service_index = service_for(&specs, &announced_services, &_service);
                                              let spec = &specs[service_index];
                                              shared.mark_request_activity();
                                              // Streamed request body (protocol v2): the backend
                                              // request starts immediately and is fed chunk-by-chunk
                                              // as RequestChunk frames arrive.
                                              let (body_tx, body_rx) =
                                                  mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);
                                              active_request_streams.lock().await.insert(id.clone(), body_tx);
                                              let ctx = forward_ctxs[service_index].clone();
                                              let limiter = local_limiters[service_index].clone();
                                              let inflight = shared.inflight_requests.clone();
                                              let streams = active_request_streams.clone();
                                              let proto = server_protocol.clone();
                                              let pool = spec.pool_load.clone();
                                              inflight.fetch_add(1, Ordering::SeqCst);
                                              pool.enter();
                                              let adaptive_for_task = adaptives[service_index].clone();
                                              tokio::spawn(async move {
                                                  let waiting = Instant::now();
                                                  let _permit = match limiter {
                                                      Some(sem) => sem.acquire_owned().await.ok(),
                                                      None => None,
                                                  };
                                                  if let Some(a) = &adaptive_for_task {
                                                      a.record_wait(waiting.elapsed());
                                                  }
                                                  let peer = proto.load(Ordering::Relaxed);
                                                  let binary = peer >= 2;
                                                  // v5: a buffered response goes out as one frame,
                                                  // envelope and body, instead of base64 in JSON.
                                                  let full_body = peer >= 5;
                                                  let response = handle_incoming_request(
                                                      &ctx,
                                                      ForwardRequest {
                                                          id: id.clone(),
                                                          method,
                                                          uri,
                                                          headers,
                                                          body: None,
                                                          raw_body: None,
                                                      },
                                                      Some(body_rx),
                                                      binary,
                                                      full_body,
                                                  )
                                                  .await;
                                                  streams.lock().await.remove(&id);
                                                  if let Some(response) = response
                                                      && let Ok(resp_str) = serde_json::to_string(&response)
                                                  {
                                                      let _ = ctx.tunnel_tx.send(Message::Text(resp_str.into())).await;
                                                  }
                                                  inflight.fetch_sub(1, Ordering::SeqCst);
                                                  pool.leave();
                                              });
                                          }
                                          TunnelMessage::RequestChunk { id, data } => {
                                              // Base64 fallback path; v2 servers send binary frames.
                                              match BASE64_STANDARD.decode(&data) {
                                                  Ok(bytes) => {
                                                      feed_request_chunk(&active_request_streams, &id, bytes.into()).await;
                                                  }
                                                  Err(_) => warn!(
                                                      "Failed to decode Base64 RequestChunk for {}",
                                                      id
                                                  ),
                                              }
                                          }
                                          TunnelMessage::RequestEnd { id } => {
                                              // Dropping the feeder ends the streamed body.
                                              active_request_streams.lock().await.remove(&id);
                                          }
                                          TunnelMessage::UpgradeRequest {
                                              // Which service the server routed this to, by name.
                                              // Absent on a connection carrying one, where there
                                              // is nothing to choose.
                                              service: _service,
                                              id,
                                              method,
                                              uri,
                                              headers,
                                          } => {
                                              // The service the server named. With one, this is that one.
                                              let service_index = service_for(&specs, &announced_services, &_service);
                                              let spec = &specs[service_index];
                                              shared.mark_request_activity();
                                              let tx_resp = tx_write.clone();
                                              let target_url = spec.target.clone();
                                              let path_bind_val = spec.path.clone();
                                              let trim_bind_val = spec.trim_bind;
                                              let active_streams = active_ws_streams.clone();
                                              let client_timeout = spec.timeout_secs;
                                              let activity = shared.activity_clock();
                                              let pauses = stream_pauses.clone();
                                              // The peer's version decides how this stream's binary
                                              // frames travel back.
                                              let peer = server_protocol.load(Ordering::Relaxed);

                                              tokio::spawn(async move {
                                                  handle_upgrade_request(
                                                      id,
                                                      method,
                                                      uri,
                                                      headers,
                                                      &target_url,
                                                      path_bind_val,
                                                      trim_bind_val,
                                                      tx_resp,
                                                      active_streams,
                                                      client_timeout,
                                                      activity,
                                                      pauses,
                                                      peer,
                                                  )
                                                  .await;
                                              });
                                          }
                                          TunnelMessage::WsData {
                                              stream_id,
                                              data,
                                              is_text,
                                          } => {
                                              // Forward from tunnel → backend WS with the bounded
                                              // hand-off: the map is released first, and a consumer
                                              // that cannot take the frame within the budget loses its
                                              // own stream. Awaiting it without a bound would let one
                                              // backend that stopped reading wedge the read loop, which
                                              // also carries Pong, and take every stream on this
                                              // connection down with it.
                                              let ws_msg = if is_text {
                                                  Message::Text(data.into())
                                              } else {
                                                  // Base64 fallback; a v7 server sends
                                                  // FRAME_WS_DATA_BIN frames.
                                                  match BASE64_STANDARD.decode(&data) {
                                                      Ok(bytes) => Message::Binary(bytes.into()),
                                                      Err(_) => {
                                                          warn!("Failed to decode Base64 WsData for stream {}", stream_id);
                                                          continue;
                                                      }
                                                  }
                                              };
                                              deliver_ws_frame(&active_ws_streams, &stream_id, ws_msg).await;
                                          }
                                          TunnelMessage::WsClose {
                                              stream_id,
                                              code: _,
                                              reason: _,
                                          } => {
                                              // Close the backend WS stream
                                              let mut streams = active_ws_streams.lock().await;
                                              if let Some(handle) = streams.remove(&stream_id) {
                                                  let _ = handle.abort_tx.send(()).await;
                                                  debug!("Closed WebSocket stream {}", stream_id);
                                              }
                                          }
                                          TunnelMessage::TcpOpen { stream_id, target, visitor, service: _service } => {
                                              // The service the server named. With one, this is that one.
                                              let service_index = service_for(&specs, &announced_services, &_service);
                                              let spec = &specs[service_index];
                                              shared.mark_request_activity();
                                              // SSRF guard: only addresses this client itself
                                              // declared are ever dialed, a named target must be
                                              // in the tunnels: list, no target means the legacy
                                              // tcp_target.
                                              let resolved = match &target {
                                                  Some(t) => spec
                                                      .tunnels
                                                      .iter()
                                                      .find(|d| {
                                                          d.target == *t
                                                              && aperio_config::protocol_serves(&d.protocol, "tcp")
                                                      })
                                                      .map(|d| (d.target.clone(), d.encrypt, d.psk.clone(), d.proxy_protocol)),
                                                  None => spec.tcp_target.clone().map(|t| (t, false, None, false)),
                                              };
                                              match resolved {
                                                  Some((target_addr, encrypt, psk, proxy_protocol)) => {
                                                      // Register the stream handle synchronously, BEFORE
                                                      // spawning: TcpData for this stream can arrive on the
                                                      // very next tunnel frame and would be dropped if the
                                                      // spawned task had not registered yet. The channel
                                                      // buffers bytes until the backend connect completes.
                                                      let (bytes_tx, bytes_rx) = mpsc::channel::<bytes::Bytes>(64);
                                                      let (abort_tx, abort_rx) = mpsc::channel::<()>(1);
                                                      active_tcp_streams.lock().await.insert(
                                                          stream_id.clone(),
                                                          TcpStreamHandle { tx: bytes_tx, abort_tx },
                                                      );
                                                      let tx = tx_write.clone();
                                                      let streams = active_tcp_streams.clone();
                                                      let activity = shared.activity_clock();
                                                      let pauses = stream_pauses.clone();
                                                      // The peer's version, read when the stream opens:
                                                      // it decides whether this relay's payloads travel
                                                      // as v7 binary frames or base64 in JSON.
                                                      let peer = server_protocol.load(Ordering::Relaxed);
                                                      tokio::spawn(async move {
                                                          let e2e = encrypt.then_some(crate::e2e::E2eParams { psk });
                                                          let announce = proxy_protocol.then_some(visitor).flatten();
                                                          handle_tcp_open(stream_id, target_addr, tx, streams, bytes_rx, abort_rx, e2e, activity, pauses, peer, announce).await;
                                                      });
                                                  }
                                                  None => {
                                                      match target {
                                                          Some(t) => warn!("TcpOpen for undeclared target {}; refusing", t),
                                                          None => warn!("TcpOpen received but no TCP target is configured; refusing"),
                                                      }
                                                      let close = TunnelMessage::TcpClose { stream_id };
                                                      if let Ok(json) = serde_json::to_string(&close) {
                                                          let _ = tx_write.send(Message::Text(json.into())).await;
                                                      }
                                                  }
                                              }
                                          }
                                          TunnelMessage::UdpOpen { stream_id, target, service: _service } => {
                                              // The service the server named. With one, this is that one.
                                              let service_index = service_for(&specs, &announced_services, &_service);
                                              let spec = &specs[service_index];
                                              shared.mark_request_activity();
                                              // SSRF guard: only declared protocol: udp targets
                                              // are ever dialed, mirroring TcpOpen.
                                              let resolved = spec
                                                  .tunnels
                                                  .iter()
                                                  .find(|d| {
                                                      d.target == target
                                                          && aperio_config::protocol_serves(&d.protocol, "udp")
                                                  })
                                                  .map(|d| (d.target.clone(), crate::udp::effective_idle_timeout(d.idle_timeout)));
                                              match resolved {
                                                  Some((target_addr, idle_timeout)) => {
                                                      // Register synchronously, like TcpOpen: datagrams
                                                      // can arrive on the very next tunnel frame.
                                                      let (dg_tx, dg_rx) = mpsc::channel::<bytes::Bytes>(64);
                                                      let (abort_tx, abort_rx) = mpsc::channel::<()>(1);
                                                      active_udp_streams.lock().await.insert(
                                                          stream_id.clone(),
                                                          UdpStreamHandle { tx: dg_tx, abort_tx },
                                                      );
                                                      let tx = tx_write.clone();
                                                      let streams = active_udp_streams.clone();
                                                      let activity = shared.activity_clock();
                                                      let peer = server_protocol.load(Ordering::Relaxed);
                                                      tokio::spawn(async move {
                                                          handle_udp_open(stream_id, target_addr, tx, streams, dg_rx, abort_rx, idle_timeout, activity, peer).await;
                                                      });
                                                  }
                                                  None => {
                                                      warn!("UdpOpen for undeclared target {}; refusing", target);
                                                      let close = TunnelMessage::UdpClose { stream_id };
                                                      if let Ok(json) = serde_json::to_string(&close) {
                                                          let _ = tx_write.send(Message::Text(json.into())).await;
                                                      }
                                                  }
                                              }
                                          }
                                          TunnelMessage::UdpDatagram { stream_id, data } => {
                                              // Base64 fallback; a v7 server sends
                                              // FRAME_UDP_DATAGRAM frames.
                                              match BASE64_STANDARD.decode(&data) {
                                                  Ok(bytes) => deliver_udp_bytes(&active_udp_streams, &stream_id, bytes.into()).await,
                                                  Err(_) => warn!("Failed to decode Base64 UdpDatagram for stream {}", stream_id),
                                              }
                                          }
                                          TunnelMessage::UdpClose { stream_id } => {
                                              let mut streams = active_udp_streams.lock().await;
                                              if let Some(handle) = streams.remove(&stream_id) {
                                                  let _ = handle.abort_tx.send(()).await;
                                                  debug!("Closed UDP relay {}", stream_id);
                                              }
                                          }
                                          TunnelMessage::TcpData { stream_id, data } => {
                                              // The bounded hand-off (see the WsData arm): a backend
                                              // that accepts the connection and then stops reading must
                                              // never wedge the tunnel read loop and starve the liveness
                                              // watchdog, but a merely slow one keeps its stream.
                                              // Base64 fallback; a v7 server sends
                                              // FRAME_TCP_DATA frames.
                                              match BASE64_STANDARD.decode(&data) {
                                                  Ok(bytes) => deliver_tcp_bytes(&active_tcp_streams, &stream_id, bytes.into()).await,
                                                  Err(_) => warn!("Failed to decode Base64 TcpData for stream {}", stream_id),
                                              }
                                          }
                                          TunnelMessage::TcpClose { stream_id } => {
                                              let mut streams = active_tcp_streams.lock().await;
                                              if let Some(handle) = streams.remove(&stream_id) {
                                                  let _ = handle.abort_tx.send(()).await;
                                                  debug!("Closed TCP stream {}", stream_id);
                                              }
                                          }
                                          TunnelMessage::SubscribeRefused { topic, reason } => {
                                              warn!(
                                                  "[{}] Not subscribed to '{}': {}",
                                                  label, topic, reason
                                              );
                                          }
                                          TunnelMessage::PublishRefused { topic, reason } => {
                                              warn!(
                                                  "[{}] The message published on '{}' went nowhere: {}",
                                                  label, topic, reason
                                              );
                                          }
                                          TunnelMessage::Publish { topic, payload, id, qos } => {
                                              use base64::prelude::*;
                                              match BASE64_STANDARD.decode(&payload) {
                                                  Ok(bytes) => {
                                                      // Acknowledged before anything else, and
                                                      // whether or not this is a duplicate: the
                                                      // server resends until it hears back, and a
                                                      // redelivery it already sent needs answering
                                                      // too or it comes round again.
                                                      if qos >= 1 && let Some(id) = &id {
                                                          shared.messages.acknowledge(id).await;
                                                      }
                                                      // At-least-once means the same message can
                                                      // arrive twice when an acknowledgement is
                                                      // lost. Acting on a deploy trigger twice is
                                                      // worse than acting on it late.
                                                      let duplicate = match &id {
                                                          Some(id) => shared.messages.is_duplicate(id).await,
                                                          None => false,
                                                      };
                                                      // A filter removed since the server was told
                                                      // still delivers for a moment; dropping here
                                                      // keeps a local subscriber from seeing a
                                                      // topic it no longer asked for.
                                                      if !duplicate && shared.messages.wants(&topic).await {
                                                          shared.messages.deliver(crate::pubsub::Delivery {
                                                              topic,
                                                              payload: bytes,
                                                              id,
                                                          });
                                                      }
                                                  }
                                                  Err(e) => warn!("Undecodable message payload on '{}': {}", topic, e),
                                              }
                                          }
                                          TunnelMessage::StreamPause { id } => {
                                              // Server flow control (v3): the visitor of this
                                              // stream reads slower than we produce. An unknown
                                              // id (stream already finished) is a no-op.
                                              stream_pauses.pause(&id);
                                          }
                                          TunnelMessage::StreamResume { id } => {
                                              stream_pauses.resume(&id);
                                          }
                                          TunnelMessage::CompressionStart {} => {
                                              info!("Server offered tunnel compression; enabling zlib frames");
                                              if let Ok(json) = serde_json::to_string(&TunnelMessage::CompressionAck {}) {
                                                  let _ = tx_write.send(Message::Text(json.into())).await;
                                              }
                                              compress_out.store(true, Ordering::SeqCst);
                                          }
                                          TunnelMessage::HostnameAssigned { hostname } => {
                                              info!("[{}] Server assigned hostname to this client: {}", label, hostname);
                                          }
                                          TunnelMessage::ServerShutdown {} => {
                                              // The server is restarting: skip the reconnect backoff
                                              // once the socket drops so downtime stays minimal.
                                              info!("[{}] Server announced a graceful shutdown; will reconnect aggressively.", label);
                                              server_announced_shutdown = true;
                                          }
                                          TunnelMessage::Pong { timestamp, version, protocol } => {
                                              debug!("Pong received: {}", timestamp);
                                              health_report.pong_received();
                                              if let Some(p) = protocol {
                                                  server_protocol.store(p, Ordering::Relaxed);
                                              }
                                              // Log version skew once per connection, not per heartbeat.
                                              if !version_skew_warned
                                                && let Some(p) = protocol
                                                && p != PROTOCOL_VERSION
                                              {
                                                  version_skew_warned = true;
                                                  warn!(
                                                      "Server speaks tunnel protocol v{} (server version {}) but this client speaks v{}; update the older side",
                                                      p,
                                                      version.as_deref().unwrap_or("unknown"),
                                                      PROTOCOL_VERSION
                                                  );
                                              }
                                              let mut lock = last_pong_time.lock().await;
                                              *lock = Instant::now();
                                          }
                                          _ => {}
                                      }
                                  }
                              }
                           Some(Err(e)) => {
                              error!("Error reading from server socket: {:?}", e);
                              break;
                          }
                          None => {
                              warn!("WebSocket stream closed by server.");
                              break;
                          }
                      }
                  }
              }
            }

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
