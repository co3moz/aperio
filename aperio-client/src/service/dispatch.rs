//! The read loop of an established connection: one frame in, the work it names
//! dispatched, and the two things the reconnect loop needs to know when it ends.
//!
//! Extracted on the measurement `run_service` was put through. Twenty-one
//! values crossed into this loop, the highest count of any cut made in this
//! codebase, and all but two of them are the *same* thing said fifteen ways:
//! the live state of one socket. [`Dispatch`] is that state named once, and
//! [`Ended`] is the two flags that genuinely travel back out.
//!
//! `version_skew_warned` is in neither: it is warn-once state for the life of
//! this loop, and nothing above reads it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};

use super::*;

/// One established connection's live state, as its read loop needs it.
///
/// The borrows are the connection's own and outlive the loop; the `Arc`s are
/// shared with the writer and the ping task, which is why they are clones
/// rather than references.
pub(crate) struct Dispatch<'a> {
  pub(crate) services: &'a [ServiceRuntime],
  pub(crate) shared: &'a Shared,
  pub(crate) label: &'a str,
  pub(crate) forward_ctxs: &'a [Arc<ForwardContext>],
  pub(crate) announced_services: &'a [usize],
  pub(crate) health_report: &'a Arc<crate::health_report::HealthReport>,
  pub(crate) tx_write: mpsc::Sender<Message>,
  pub(crate) last_pong_time: Arc<Mutex<std::time::Instant>>,
  pub(crate) server_protocol: Arc<AtomicU32>,
  pub(crate) compress_out: Arc<AtomicBool>,
  pub(crate) stream_pauses: crate::flow::PauseRegistry,
  /// The socket's frame cap. Top-level rather than per service, so it is one
  /// number rather than something to look up per declaration.
  pub(crate) max_message_size: usize,
  pub(crate) active_request_streams: Arc<Mutex<HashMap<String, RequestBodyFeeder>>>,
  pub(crate) active_ws_streams: Arc<Mutex<HashMap<String, WsStreamHandle>>>,
  pub(crate) active_tcp_streams: Arc<Mutex<HashMap<String, TcpStreamHandle>>>,
  pub(crate) active_udp_streams: Arc<Mutex<HashMap<String, UdpStreamHandle>>>,
}

/// Why the read loop ended, in the only two terms the reconnect loop acts on.
#[derive(Default)]
pub(crate) struct Ended {
  /// The connection was ended deliberately, so the line below reports a close
  /// rather than a loss.
  pub(crate) closed_on_request: bool,
  /// The server said it was restarting, so the next reconnect skips the
  /// exponential backoff.
  pub(crate) server_announced_shutdown: bool,
}

impl Dispatch<'_> {
  /// Reads until the socket ends, the peer goes away, or something asks this
  /// connection to stop.
  pub(crate) async fn run(
    &self,
    abort_rx: &mut mpsc::Receiver<AbortReason>,
    ws_receiver: &mut futures_util::stream::SplitStream<
      tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    >,
  ) -> Ended {
    let Dispatch {
      services,
      shared,
      label,
      forward_ctxs,
      announced_services,
      health_report,
      tx_write,
      last_pong_time,
      server_protocol,
      compress_out,
      stream_pauses,
      max_message_size: _,
      active_request_streams,
      active_ws_streams,
      active_tcp_streams,
      active_udp_streams,
    } = self;
    let mut version_skew_warned = false;
    let mut ended = Ended::default();
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
                      ended.closed_on_request = true;
                      debug!("[{}] Closing the socket loop on request.", label);
                  }
              }
              break;
          }
          _ = shutdown_requested(shared) => {
              // Announce drain, let in-flight requests finish, then exit.
              if let Ok(json) = serde_json::to_string(&TunnelMessage::Draining {}) {
                  let _ = tx_write.send(Message::Text(json.into())).await;
              }
              drain_inflight(shared).await;
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
                                      feed_request_chunk(active_request_streams, fid, payload).await;
                                      None
                                  }
                                  // v7: relay payloads as raw bytes, the same
                                  // deliveries their JSON shapes make below.
                                  Some((crate::protocol::FRAME_TCP_DATA, sid, payload)) => {
                                      deliver_tcp_bytes(active_tcp_streams, sid, b.slice(b.len() - payload.len()..)).await;
                                      None
                                  }
                                  Some((crate::protocol::FRAME_UDP_DATAGRAM, sid, payload)) => {
                                      deliver_udp_bytes(active_udp_streams, sid, b.slice(b.len() - payload.len()..)).await;
                                      None
                                  }
                                  Some((crate::protocol::FRAME_WS_DATA_BIN, sid, payload)) => {
                                      deliver_ws_frame(active_ws_streams, sid, Message::Binary(b.slice(b.len() - payload.len()..))).await;
                                      None
                                  }
                                  // v6: envelope and buffered body in one frame,
                                  // deflated by the server's writer when this
                                  // connection negotiated compression.
                                  Some((tag @ (FRAME_REQUEST_FULL | FRAME_REQUEST_FULL_ZLIB), _, payload)) => {
                                      let max = self.max_message_size.saturating_mul(4);
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
                                  _ => decompress_frame(&b, self.max_message_size.saturating_mul(4)),
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
                                      let service_index = service_for(services, announced_services, &_service);
                                      let spec = &services[service_index].spec;
                                      let ctx = forward_ctxs[service_index].clone();
                                      let limiter = services[service_index].limiter.clone();
                                      let inflight = shared.inflight_requests.clone();
                                      let proto = server_protocol.clone();
                                      let raw_body = frame_body.take();
                                      let pool = spec.pool_load.clone();
                                      inflight.fetch_add(1, Ordering::SeqCst);
                                      pool.enter();
                                      shared.mark_request_activity();

                                      // Handle incoming request concurrently
                                      let adaptive_for_task = services[service_index].adaptive.clone();
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
                                      let service_index = service_for(services, announced_services, &_service);
                                      let spec = &services[service_index].spec;
                                      shared.mark_request_activity();
                                      // Streamed request body (protocol v2): the backend
                                      // request starts immediately and is fed chunk-by-chunk
                                      // as RequestChunk frames arrive.
                                      let (body_tx, body_rx) =
                                          mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);
                                      active_request_streams.lock().await.insert(id.clone(), body_tx);
                                      let ctx = forward_ctxs[service_index].clone();
                                      let limiter = services[service_index].limiter.clone();
                                      let inflight = shared.inflight_requests.clone();
                                      let streams = active_request_streams.clone();
                                      let proto = server_protocol.clone();
                                      let pool = spec.pool_load.clone();
                                      inflight.fetch_add(1, Ordering::SeqCst);
                                      pool.enter();
                                      let adaptive_for_task = services[service_index].adaptive.clone();
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
                                              feed_request_chunk(active_request_streams, &id, bytes.into()).await;
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
                                      let service_index = service_for(services, announced_services, &_service);
                                      let spec = &services[service_index].spec;
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
                                      deliver_ws_frame(active_ws_streams, &stream_id, ws_msg).await;
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
                                      let service_index = service_for(services, announced_services, &_service);
                                      let spec = &services[service_index].spec;
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
                                      let service_index = service_for(services, announced_services, &_service);
                                      let spec = &services[service_index].spec;
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
                                          Ok(bytes) => deliver_udp_bytes(active_udp_streams, &stream_id, bytes.into()).await,
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
                                          Ok(bytes) => deliver_tcp_bytes(active_tcp_streams, &stream_id, bytes.into()).await,
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
                                      ended.server_announced_shutdown = true;
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
    ended
  }
}
