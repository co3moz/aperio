//! Getting a response body back through the tunnel: the thresholds that decide
//! buffered from streamed, the coalescer that stops a chatty backend turning
//! every few bytes into a frame, and the two senders that put one on the wire.

use super::*;

/// Response bodies larger than this are streamed through the tunnel in
/// chunks instead of being buffered and sent as one message. Used with a peer
/// that cannot take binary frames, where streaming means base64 chunks and is
/// therefore only worth it to bound memory.
pub(crate) const STREAM_THRESHOLD: usize = 256 * 1024;

/// The same threshold for a peer that takes binary frames, where streaming
/// also means the body stops being base64-encoded and JSON-escaped.
///
/// Measured on loopback, median of three interleaved runs, buffered against
/// streamed at the same body size:
///
/// | body   | buffered | streamed |
/// |--------|----------|----------|
/// | 8 KB   | **+18%** |          |
/// | 16 KB  | **+14%** |          |
/// | 32 KB  | tie      | tie      |
/// | 64 KB  |          | **+23%** |
/// | 128 KB |          | **+36%** |
///
/// Streaming has a fixed cost per response (a head message, a frame per
/// chunk, a tail, and a pause registration) that a small body cannot repay,
/// and base64 has a per-byte cost that a large one cannot escape. They cross
/// at 32 KB, so that is where the switch goes: everything above it is the
/// side that wins, everything below keeps the single message it wanted.
pub(crate) const BINARY_STREAM_THRESHOLD: usize = 32 * 1024;

/// Size of individual streamed body chunks.
pub(crate) const STREAM_CHUNK_SIZE: usize = 128 * 1024;

impl ChunkCoalescer {
  pub(crate) fn new() -> Self {
    ChunkCoalescer { buf: Vec::new() }
  }

  pub(crate) fn is_empty(&self) -> bool {
    self.buf.is_empty()
  }

  /// Appends one backend chunk.
  pub(crate) fn add(&mut self, data: &[u8]) {
    self.buf.extend_from_slice(data);
  }

  /// A full frame's worth, when one has accumulated.
  pub(crate) fn pop_full(&mut self) -> Option<Vec<u8>> {
    if self.buf.len() < STREAM_CHUNK_SIZE {
      return None;
    }
    let rest = self.buf.split_off(STREAM_CHUNK_SIZE);
    Some(std::mem::replace(&mut self.buf, rest))
  }

  /// Whatever is left, for the backend-quiet and end-of-stream flushes.
  pub(crate) fn take(&mut self) -> Option<Vec<u8>> {
    if self.buf.is_empty() {
      None
    } else {
      Some(std::mem::take(&mut self.buf))
    }
  }
}

/// Sends a whole buffered response as one v5 binary frame: the envelope and
/// the body in a single message, with the body as bytes.
///
/// The alternative is what every version before v5 did, base64 the body into
/// the JSON: a third more bytes on the wire, an encode pass here and a decode
/// pass on the server, and a String the size of the response held on both
/// sides. Returns whether it went out; a peer that cannot take the frame is
/// never offered one, so a failure here is a dead connection rather than a
/// version problem.
pub(crate) async fn send_full_response(
  tunnel_tx: &mpsc::Sender<Message>,
  id: &str,
  message: &TunnelMessage,
  body: &[u8],
) -> bool {
  let Ok(json) = serde_json::to_string(message) else {
    return false;
  };
  let Some(frame) = crate::protocol::encode_full_response_frame(id, &json, body) else {
    return false;
  };
  tunnel_tx.send(Message::Binary(frame.into())).await.is_ok()
}

/// Sends one streamed response chunk: a raw binary frame for v2 servers, or
/// the legacy base64+JSON message otherwise. Honors the stream's pause
/// switch first, so a `StreamPause` from the server stops the body read
/// loop right here and TCP backpressure reaches the backend.
pub(crate) async fn send_response_chunk(
  tunnel_tx: &mpsc::Sender<Message>,
  id: &str,
  part: &[u8],
  binary: bool,
  pause: &crate::flow::PauseSignal,
) -> Result<(), ()> {
  pause.wait_while_paused().await;
  if binary {
    // An id too long to frame is a broken peer, not a chunk to guess at.
    let Some(frame) = encode_binary_frame(FRAME_RESPONSE_CHUNK, id, part) else {
      tracing::warn!("Refusing to frame a response chunk: request id is too long to encode");
      return Err(());
    };
    tunnel_tx
      .send(Message::Binary(frame.into()))
      .await
      .map_err(|_| ())
  } else {
    let msg = TunnelMessage::ResponseChunk {
      id: id.to_string(),
      data: BASE64_STANDARD.encode(part),
    };
    send_tunnel_msg(tunnel_tx, &msg).await
  }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
