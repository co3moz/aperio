//! Relayed WebSocket streams and the two announcements that end a connection
//! cleanly: the answer to an upgrade, its frames, and `Draining`.

use super::super::tests::*;
use super::super::*;
use crate::protocol::TunnelMessage;
use crate::state::*;
use crate::test_support::*;
use base64::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

// --- WebSocket relay frames -------------------------------------------------

#[tokio::test]
async fn ws_data_text_and_binary_and_close() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, mut rx) = mpsc::channel::<WsStreamMessage>(8);
  state.ws_streams.lock().await.insert(
    "w1".into(),
    WsStreamHandle {
      tx: crate::state::test_pump(tx),
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::WsData {
      stream_id: "w1".into(),
      data: "hello".into(),
      is_text: true,
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    WsStreamMessage::Data(Message::Text(t)) => assert_eq!(t, "hello"),
    _ => panic!("expected text data"),
  }

  send(
    &mut ws,
    &TunnelMessage::WsData {
      stream_id: "w1".into(),
      data: BASE64_STANDARD.encode([1u8, 2, 3]),
      is_text: false,
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    WsStreamMessage::Data(Message::Binary(b)) => assert_eq!(b, vec![1, 2, 3]),
    _ => panic!("expected binary data"),
  }

  // Bad base64 binary: skipped without closing the stream.
  send(
    &mut ws,
    &TunnelMessage::WsData {
      stream_id: "w1".into(),
      data: "@@@".into(),
      is_text: false,
    },
  )
  .await;
  send(
    &mut ws,
    &TunnelMessage::WsClose {
      stream_id: "w1".into(),
      code: 1000,
      reason: "bye".into(),
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    WsStreamMessage::Close => {}
    _ => panic!("expected close"),
  }

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn ws_data_not_owned_ignored() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let _cid = wait_client_id(&state).await;

  let (tx, mut rx) = mpsc::channel::<WsStreamMessage>(8);
  state.ws_streams.lock().await.insert(
    "w1".into(),
    WsStreamHandle {
      tx: crate::state::test_pump(tx),
      client_id: "foreign".into(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::WsData {
      stream_id: "w1".into(),
      data: "hi".into(),
      is_text: true,
    },
  )
  .await;
  send(
    &mut ws,
    &TunnelMessage::WsClose {
      stream_id: "w1".into(),
      code: 1000,
      reason: String::new(),
    },
  )
  .await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(rx.try_recv().is_err());
}

// --- UpgradeResponse --------------------------------------------------------

#[tokio::test]
async fn upgrade_response_owned_and_dropped() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, rx) = oneshot::channel::<TunnelResponse>();
  state.pending_upgrades.lock().await.insert(
    "up1".into(),
    PendingRequest {
      tx,
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::UpgradeResponse {
      id: "up1".into(),
      status: 101,
      headers: vec![],
    },
  )
  .await;
  let resp = tokio::time::timeout(Duration::from_secs(2), rx)
    .await
    .expect("timeout")
    .expect("dropped");
  assert_eq!(resp.status, 101);

  // Not-owned variant is rejected and kept.
  let (tx2, _rx2) = oneshot::channel::<TunnelResponse>();
  state.pending_upgrades.lock().await.insert(
    "up2".into(),
    PendingRequest {
      tx: tx2,
      client_id: "foreign".into(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::UpgradeResponse {
      id: "up2".into(),
      status: 101,
      headers: vec![],
    },
  )
  .await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(state.pending_upgrades.lock().await.contains_key("up2"));

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

// --- Draining ---------------------------------------------------------------

#[tokio::test]
async fn draining_marks_client() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  send(&mut ws, &TunnelMessage::Draining {}).await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(state.clients.write().await.get(&cid).unwrap().draining);

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}
