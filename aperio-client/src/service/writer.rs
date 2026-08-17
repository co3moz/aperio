//! The write half of an established connection: everything the tunnel sends
//! goes through this one task, so a slow socket backs up in one place instead
//! of stalling whichever task happened to produce the frame.
//!
//! The read half is [`super::dispatch`].

use super::*;

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

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
