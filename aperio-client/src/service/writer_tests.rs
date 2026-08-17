//! The writer's drain: that it flushes what it was given before it ends.
//!
//! The connection used to end by aborting this task, and what was still queued
//! went with it.

use super::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[tokio::test]
async fn the_writer_sends_what_is_queued_before_it_stops() {
  use std::sync::atomic::AtomicBool;

  // The connection used to end by aborting this task, which drops whatever
  // is still queued. A response reaches the queue *before* the request task
  // decrements the in-flight counter a drain waits on, so a configuration
  // reload could pass its drain and then throw away a response the visitor
  // was owed: exactly what the drain was added to prevent.
  let (tx, rx) = mpsc::channel::<Message>(16);
  let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
  let sent = Arc::new(Mutex::new(Vec::<String>::new()));

  // A sink that records what reached it.
  let recorder = sent.clone();
  let sink = futures_util::sink::unfold((), move |(), msg: Message| {
    let recorder = recorder.clone();
    async move {
      if let Message::Text(t) = msg {
        recorder.lock().await.push(t.to_string());
      }
      Ok::<(), std::convert::Infallible>(())
    }
  });
  let sink = Box::pin(sink);

  let writer = tokio::spawn(run_writer(
    sink,
    rx,
    finish_rx,
    Arc::new(AtomicBool::new(false)),
  ));

  // Queued and then immediately asked to finish, with nothing awaited in
  // between: this is the shape of the race, where the queue is full at the
  // moment the connection is told to end.
  for n in 0..5 {
    tx.send(Message::Text(format!("response-{n}").into()))
      .await
      .unwrap();
  }
  let _ = finish_tx.send(());
  drop(tx);
  tokio::time::timeout(Duration::from_secs(2), writer)
    .await
    .expect("the writer should finish once the queue is empty")
    .unwrap();

  let sent = sent.lock().await.clone();
  assert_eq!(
    sent,
    (0..5).map(|n| format!("response-{n}")).collect::<Vec<_>>(),
    "every queued message must reach the socket, in order"
  );
}
