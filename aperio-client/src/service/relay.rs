use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::warn;

use super::*;

/// Longest the tunnel's read loop will wait for one stream's consumer, whether
/// that is an upload's backend request, a proxied WebSocket or a TCP relay.
///
/// The loop is shared by every request, every stream and the heartbeat on this
/// connection, so a blocking send here does not slow one stream down, it stops
/// the whole tunnel: no Pong goes out, and fifteen seconds later the liveness
/// check tears the connection down and every in-flight request with it. Two
/// seconds is generous for a backend that is merely slow and short enough that
/// two of them in a row still leave the heartbeat alive.
const STREAM_STALL_BUDGET: Duration = Duration::from_secs(2);

/// Hands one frame to a relay's consumer, waiting only as long as the tunnel
/// can afford. `false` means this stream is finished and its entry should go.
///
/// The alternative that was here, `try_send` and drop the stream the moment
/// its buffer is full, protected the read loop and turned *transient*
/// backpressure into stream death. WebSockets and TCP relays are lossless, so
/// a healthy consumer that is merely slower than a burst, a large file over a
/// tunneled socket whose peer applies flow control, was being killed for
/// keeping the tunnel waiting a few milliseconds. Waiting a bounded two
/// seconds first covers that; a consumer still not ready after it is stalled
/// rather than slow, and loses its own stream instead of the connection.
async fn deliver_to_relay<T>(tx: &mpsc::Sender<T>, kind: &str, stream_id: &str, item: T) -> bool {
  match tx.try_send(item) {
    Ok(()) => true,
    Err(mpsc::error::TrySendError::Closed(_)) => false,
    Err(mpsc::error::TrySendError::Full(item)) => {
      match tokio::time::timeout(STREAM_STALL_BUDGET, tx.send(item)).await {
        Ok(Ok(())) => true,
        _ => {
          warn!(
            "{} relay {} stalled: its consumer took no data for {}s, dropping that stream rather than the tunnel",
            kind,
            stream_id,
            STREAM_STALL_BUDGET.as_secs()
          );
          false
        }
      }
    }
  }
}

/// Delivers one relayed TCP chunk to its backend stream, however it arrived
/// (base64 in JSON from an older server, or a v7 binary frame).
pub(crate) async fn deliver_tcp_bytes(
  streams: &Arc<Mutex<HashMap<String, TcpStreamHandle>>>,
  stream_id: &str,
  bytes: bytes::Bytes,
) {
  let tx = {
    let map = streams.lock().await;
    map.get(stream_id).map(|h| h.tx.clone())
  };
  if let Some(tx) = tx
    && !deliver_to_relay(&tx, "TCP", stream_id, bytes).await
  {
    streams.lock().await.remove(stream_id);
  }
}

/// Delivers one relayed datagram. Best-effort by contract, unlike the WS and
/// TCP paths: a datagram relay that waits for a congested consumer is no
/// longer a datagram relay, so a full channel drops it and keeps the stream.
pub(crate) async fn deliver_udp_bytes(
  streams: &Arc<Mutex<HashMap<String, UdpStreamHandle>>>,
  stream_id: &str,
  bytes: bytes::Bytes,
) {
  let streams = streams.lock().await;
  if let Some(handle) = streams.get(stream_id) {
    let _ = handle.tx.try_send(bytes);
  }
}

/// Delivers one frame of a passed-through WebSocket to its backend stream.
pub(crate) async fn deliver_ws_frame(
  streams: &Arc<Mutex<HashMap<String, WsStreamHandle>>>,
  stream_id: &str,
  msg: Message,
) {
  let tx = {
    let map = streams.lock().await;
    map.get(stream_id).map(|h| h.tx.clone())
  };
  if let Some(tx) = tx
    && !deliver_to_relay(&tx, "WebSocket", stream_id, msg).await
  {
    streams.lock().await.remove(stream_id);
  }
}

/// Hands one chunk of a streamed request body to the backend request it
/// belongs to, without letting a slow consumer stall the tunnel.
///
/// The lock is released before the send, and the send is bounded. A consumer
/// that cannot take the chunk in time has its upload *failed* rather than
/// silently truncated: the error travels down the same channel as the body, so
/// the backend request ends with an error instead of a body that looks
/// complete and is not.
pub(crate) async fn feed_request_chunk(
  streams: &Arc<Mutex<HashMap<String, RequestBodyFeeder>>>,
  id: &str,
  bytes: bytes::Bytes,
) {
  let feeder = {
    let map = streams.lock().await;
    match map.get(id) {
      Some(feeder) => feeder.clone(),
      None => return,
    }
  };
  // Fast path: room in the buffer, nothing to wait for.
  match feeder.try_send(Ok(bytes)) {
    Ok(()) => return,
    Err(mpsc::error::TrySendError::Closed(_)) => {
      streams.lock().await.remove(id);
      return;
    }
    Err(mpsc::error::TrySendError::Full(chunk)) => {
      if tokio::time::timeout(STREAM_STALL_BUDGET, feeder.send(chunk))
        .await
        .is_ok()
      {
        return;
      }
    }
  }
  warn!(
    "Upload {} stalled: the backend did not read it for {}s, failing that request rather than the tunnel",
    id,
    STREAM_STALL_BUDGET.as_secs()
  );
  // Best effort: the channel is full by definition here, so this only lands
  // once the consumer takes one more chunk. When it never does, dropping the
  // feeder below ends the body anyway, and the request fails on its own
  // content-length check.
  let _ = feeder.try_send(Err(std::io::Error::other(
    "upload abandoned: the backend stopped reading the request body",
  )));
  streams.lock().await.remove(id);
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
