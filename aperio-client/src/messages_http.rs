//! The local face of client-to-client messaging.
//!
//! An application on this machine subscribes with `GET /subscribe?topic=...`
//! and publishes with `POST /publish?topic=...`, over plain HTTP on loopback.
//! No dependency, no codec, no protocol to learn: `curl -N` is a working
//! subscriber, and every language's standard library can do both halves.
//!
//! This is the first of two faces. A local MQTT listener is the other, for
//! applications that would rather use the client library they already have;
//! it will attach to the same bus, which is why the bus is not shaped around
//! HTTP.
//!
//! Loopback by default, and no authentication of its own: anything that can
//! open a socket on this host is already running as a local process next to
//! the client's credentials. Binding it anywhere else is the operator's
//! decision and is warned about, the same way `bind-tunnels` warns.

use std::net::SocketAddr;
use std::sync::Arc;

use aperio_config::topic_matches;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info, warn};

use crate::pubsub::MessageBus;

/// Largest request line plus headers accepted, and the ceiling on a published
/// body. Small on purpose: this is a signalling channel, not a way to move
/// data, which is what tunnels are for.
const MAX_REQUEST_BYTES: usize = 256 * 1024;

/// Starts the local listener. Returns once it is bound, so a failure is
/// reported at startup rather than on the first request.
pub(crate) async fn serve(addr: &str, bus: Arc<MessageBus>) -> Result<(), String> {
  let listener = TcpListener::bind(addr)
    .await
    .map_err(|e| format!("cannot listen on {addr} for the message face: {e}"))?;
  let local = listener
    .local_addr()
    .map(|a| a.to_string())
    .unwrap_or_else(|_| addr.to_string());
  if !is_loopback(&listener) {
    warn!(
      "The message face is listening on {local}, which is not loopback: anything that can reach \
       that address can publish as this client and read everything it subscribes to"
    );
  }
  info!("Message face listening on http://{local} (GET /subscribe?topic=…, POST /publish?topic=…)");

  tokio::spawn(async move {
    loop {
      match listener.accept().await {
        Ok((stream, peer)) => {
          let bus = bus.clone();
          tokio::spawn(async move {
            if let Err(e) = handle(stream, bus).await {
              debug!("Message face connection from {peer} ended: {e}");
            }
          });
        }
        Err(e) => {
          error!("Message face stopped accepting connections: {e}");
          return;
        }
      }
    }
  });
  Ok(())
}

fn is_loopback(listener: &TcpListener) -> bool {
  listener
    .local_addr()
    .map(|a: SocketAddr| a.ip().is_loopback())
    .unwrap_or(false)
}

/// Reads one request and answers it. Deliberately a hand-rolled subset of
/// HTTP/1.1 rather than a server framework: two routes, one of which is an
/// endless stream, and the client already carries enough dependencies.
async fn handle(mut stream: TcpStream, bus: Arc<MessageBus>) -> Result<(), String> {
  let mut buf = Vec::new();
  let mut chunk = [0u8; 8192];
  // Headers first: read until the blank line that ends them.
  let head_end = loop {
    let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
    if n == 0 {
      return Ok(());
    }
    buf.extend_from_slice(&chunk[..n]);
    if let Some(i) = find_head_end(&buf) {
      break i;
    }
    if buf.len() > MAX_REQUEST_BYTES {
      return respond(&mut stream, 431, "headers too large").await;
    }
  };

  let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
  let mut lines = head.lines();
  let Some(request_line) = lines.next() else {
    return respond(&mut stream, 400, "empty request").await;
  };
  let mut parts = request_line.split_whitespace();
  let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
  let (path, query) = target.split_once('?').unwrap_or((target, ""));
  let topic = query_param(query, "topic");

  match (method, path) {
    ("GET", "/subscribe") => {
      let Some(filter) = topic else {
        return respond(&mut stream, 400, "give a ?topic= filter").await;
      };
      if let Err(why) = aperio_config::validate_topic_filter(&filter) {
        return respond(&mut stream, 400, &why).await;
      }
      subscribe(stream, bus, filter).await
    }
    ("POST", "/publish") => {
      let Some(topic) = topic else {
        return respond(&mut stream, 400, "give a ?topic=").await;
      };
      let body = read_body(&mut stream, &head, buf.split_off(head_end + 4)).await?;
      match bus.publish(&topic, &body).await {
        Ok(()) => respond(&mut stream, 202, "accepted").await,
        Err(why) => respond(&mut stream, 400, &why).await,
      }
    }
    ("GET", "/") => {
      respond(
        &mut stream,
        200,
        "aperio message face\nGET  /subscribe?topic=<filter>  server-sent events\nPOST /publish?topic=<topic>   body is the message\n",
      )
      .await
    }
    _ => respond(&mut stream, 404, "no such route").await,
  }
}

/// Streams matching messages as server-sent events until the client goes away.
async fn subscribe(
  mut stream: TcpStream,
  bus: Arc<MessageBus>,
  filter: String,
) -> Result<(), String> {
  // Tell the server about the filter if this process was not already asking
  // for it, so `curl -N` works without the filter being in the config file.
  if bus.add_filter(&filter).await {
    bus.resubscribe_all().await;
  }
  let mut rx = bus.listen();
  stream
    .write_all(
      b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
    )
    .await
    .map_err(|e| e.to_string())?;
  debug!("Local subscriber attached to '{filter}'");

  loop {
    let delivery = match rx.recv().await {
      Ok(d) => d,
      // Lagged: this subscriber stopped reading long enough to fall out of
      // the buffer. Saying so is better than a silent gap.
      Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
        let note = format!(": missed {n} message(s)\n\n");
        stream
          .write_all(note.as_bytes())
          .await
          .map_err(|e| e.to_string())?;
        continue;
      }
      Err(_) => return Ok(()),
    };
    if !topic_matches(&filter, &delivery.topic) {
      continue;
    }
    // The payload is arbitrary bytes and an SSE field is a line, so it goes
    // out Base64-encoded rather than mangled or truncated at the first
    // newline. `id:` is the server's message id, which is what lets a
    // subscriber recognize a redelivery.
    use base64::prelude::*;
    let mut frame = String::new();
    if let Some(id) = &delivery.id {
      frame.push_str(&format!("id: {id}\n"));
    }
    frame.push_str(&format!("event: {}\n", delivery.topic));
    frame.push_str(&format!(
      "data: {}\n\n",
      BASE64_STANDARD.encode(&delivery.payload)
    ));
    if stream.write_all(frame.as_bytes()).await.is_err() {
      debug!("Local subscriber for '{filter}' went away");
      return Ok(());
    }
  }
}

/// Reads the request body, honouring `Content-Length`. Bodies without one are
/// taken as whatever arrived with the headers, which is what a small `curl
/// --data` looks like.
async fn read_body(
  stream: &mut TcpStream,
  head: &str,
  mut already: Vec<u8>,
) -> Result<Vec<u8>, String> {
  let want = head
    .lines()
    .find_map(|l| {
      let (name, value) = l.split_once(':')?;
      name
        .trim()
        .eq_ignore_ascii_case("content-length")
        .then(|| value.trim().parse::<usize>().ok())?
    })
    .unwrap_or(already.len());
  if want > MAX_REQUEST_BYTES {
    return Err(format!("body of {want} bytes is too large"));
  }
  let mut chunk = [0u8; 8192];
  while already.len() < want {
    let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
    if n == 0 {
      break;
    }
    already.extend_from_slice(&chunk[..n]);
  }
  already.truncate(want);
  Ok(already)
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
  buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn query_param(query: &str, name: &str) -> Option<String> {
  query.split('&').find_map(|pair| {
    let (k, v) = pair.split_once('=')?;
    (k == name).then(|| percent_decode(v))
  })
}

/// Enough percent-decoding for a topic: `%2F` and `+` are the two that show
/// up when a filter is put in a query string.
fn percent_decode(raw: &str) -> String {
  let bytes = raw.replace('+', " ").into_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'%'
      && i + 2 < bytes.len()
      && let Ok(byte) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16)
    {
      out.push(byte);
      i += 3;
      continue;
    }
    out.push(bytes[i]);
    i += 1;
  }
  String::from_utf8_lossy(&out).to_string()
}

async fn respond(stream: &mut TcpStream, status: u16, body: &str) -> Result<(), String> {
  let reason = match status {
    200 => "OK",
    202 => "Accepted",
    400 => "Bad Request",
    404 => "Not Found",
    431 => "Request Header Fields Too Large",
    _ => "Error",
  };
  let response = format!(
    "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
    body.len()
  );
  stream
    .write_all(response.as_bytes())
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "messages_http_tests.rs"]
mod tests;
