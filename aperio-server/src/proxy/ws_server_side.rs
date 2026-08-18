//! Relaying a WebSocket between a visitor and a target this server dials.
//!
//! The socket half of `server_side:` (`planned_features` `#141`). The HTTP
//! half shipped first and this answered 501 in the meantime, which was honest
//! but left an operator with a service whose requests work from the server and
//! whose sockets do not.
//!
//! **Simpler than the tunnel's relay, and for a reason worth stating.** That
//! one carries frames through a third party: it registers a stream id, waits
//! for the client to confirm the open, encodes each frame in whichever shape
//! that client's protocol version speaks, and unwinds all of it on close.
//! Here there is no third party. Two sockets are spliced, and the only
//! questions left are the ones any splice has: who closes first, and what
//! happens to the other side when they do.
//!
//! What is deliberately *not* here is a second copy of the gates. Everything
//! before the upgrade, the per-IP limit, the visitor gate, the organization
//! fence, the allowlist check on the target, has already run in
//! [`super::ws::handle_ws_proxy`], which is where the two paths part company.

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::protocol::Message as TMessage;

/// Turns a target address into the `ws://`/`wss://` URL to dial.
///
/// The scheme follows the target's: a target the operator wrote as `https://`
/// gets a `wss://` socket, anything else a plain one. Only ever appends to the
/// target's path, for the same reason the HTTP half does: what
/// `server_side_targets:` approved is the target, so nothing a visitor sends
/// may change which host is reached.
pub(crate) fn socket_url(target: &str, path_and_query: &str) -> Option<url::Url> {
  let base = if target.contains("://") {
    target.to_string()
  } else {
    format!("http://{target}")
  };
  let mut url = url::Url::parse(&base).ok()?;
  let scheme = match url.scheme() {
    "https" | "wss" => "wss",
    _ => "ws",
  };
  let (path, query) = match path_and_query.split_once('?') {
    Some((p, q)) => (p, Some(q)),
    None => (path_and_query, None),
  };
  let base_path = url.path().trim_end_matches('/').to_string();
  url.set_path(&format!("{base_path}{path}"));
  url.set_query(query);
  url.set_scheme(scheme).ok()?;
  Some(url)
}

/// Splices the visitor's socket to one this server opens to `target`.
///
/// Ends when either side closes or errors, and closes the other when it does.
/// A relay that outlived one of its halves would be a socket held open against
/// a peer that is gone, which is the leak this shape exists to avoid: both
/// directions live in one `select!` rather than in two tasks that would each
/// have to learn about the other's death.
pub(crate) async fn relay(public_ws: WebSocket, target_ws: WsStream) {
  let (mut pub_tx, mut pub_rx) = public_ws.split();
  let (mut tgt_tx, mut tgt_rx) = target_ws.split();

  loop {
    tokio::select! {
      from_visitor = pub_rx.next() => {
        match from_visitor {
          Some(Ok(msg)) => {
            let out = match msg {
              Message::Text(t) => TMessage::Text(t.as_str().into()),
              Message::Binary(b) => TMessage::Binary(b),
              Message::Ping(p) => TMessage::Ping(p),
              Message::Pong(p) => TMessage::Pong(p),
              // The visitor hanging up is the ordinary end of a relay, not an
              // error: pass it on so the target sees a close rather than a
              // socket that simply stops.
              Message::Close(_) => {
                let _ = tgt_tx.send(TMessage::Close(None)).await;
                break;
              }
            };
            if tgt_tx.send(out).await.is_err() {
              break;
            }
          }
          _ => {
            let _ = tgt_tx.send(TMessage::Close(None)).await;
            break;
          }
        }
      }
      from_target = tgt_rx.next() => {
        match from_target {
          Some(Ok(msg)) => {
            let out = match msg {
              TMessage::Text(t) => Message::Text(t.as_str().into()),
              TMessage::Binary(b) => Message::Binary(b),
              TMessage::Ping(p) => Message::Ping(p),
              TMessage::Pong(p) => Message::Pong(p),
              TMessage::Close(_) => {
                let _ = pub_tx.send(Message::Close(None)).await;
                break;
              }
              // Raw frames are tungstenite's own plumbing and never reach a
              // reader of this stream.
              TMessage::Frame(_) => continue,
            };
            if pub_tx.send(out).await.is_err() {
              break;
            }
          }
          _ => {
            let _ = pub_tx.send(Message::Close(None)).await;
            break;
          }
        }
      }
    }
  }
}

/// The socket this server opens to the target.
pub(crate) type WsStream =
  tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[cfg(test)]
#[path = "ws_server_side_tests.rs"]
mod tests;
