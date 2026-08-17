//! Raw relay frames arriving on a tunnel connection: TCP bytes and UDP
//! datagrams reaching the stream that owns them, and nothing reaching a stream
//! that does not.

use super::super::tests::*;
use crate::protocol::TunnelMessage;
use crate::state::*;
use crate::test_support::*;
use base64::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

// --- TCP / UDP data frames --------------------------------------------------

#[tokio::test]
async fn tcp_data_and_close_owned() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, mut rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.tcp_streams.lock().await.insert(
    "t1".into(),
    TcpStreamHandle {
      tx: crate::state::test_pump(tx),
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::TcpData {
      stream_id: "t1".into(),
      data: BASE64_STANDARD.encode([4u8, 5]),
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Data(d) => assert_eq!(d, vec![4, 5]),
    _ => panic!("expected data"),
  }

  // Bad base64: ignored, stream kept.
  send(
    &mut ws,
    &TunnelMessage::TcpData {
      stream_id: "t1".into(),
      data: "###".into(),
    },
  )
  .await;
  send(
    &mut ws,
    &TunnelMessage::TcpClose {
      stream_id: "t1".into(),
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Close => {}
    _ => panic!("expected close"),
  }
  assert!(!state.tcp_streams.lock().await.contains_key("t1"));

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn tcp_data_and_close_not_owned() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let _cid = wait_client_id(&state).await;

  let (tx, mut rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.tcp_streams.lock().await.insert(
    "t1".into(),
    TcpStreamHandle {
      tx: crate::state::test_pump(tx),
      client_id: "foreign".into(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::TcpData {
      stream_id: "t1".into(),
      data: BASE64_STANDARD.encode([1u8]),
    },
  )
  .await;
  send(
    &mut ws,
    &TunnelMessage::TcpClose {
      stream_id: "t1".into(),
    },
  )
  .await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(rx.try_recv().is_err());
  // Not owned: TcpClose reinserts it.
  assert!(state.tcp_streams.lock().await.contains_key("t1"));
}

#[tokio::test]
async fn udp_datagram_and_close_owned() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let cid = wait_client_id(&state).await;

  let (tx, mut rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.udp_streams.lock().await.insert(
    "u1".into(),
    crate::state::UdpStreamHandle {
      tx,
      client_id: cid.clone(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::UdpDatagram {
      stream_id: "u1".into(),
      data: BASE64_STANDARD.encode([6u8]),
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Data(d) => assert_eq!(d, vec![6]),
    _ => panic!("expected data"),
  }
  // Bad base64 for udp is ignored.
  send(
    &mut ws,
    &TunnelMessage::UdpDatagram {
      stream_id: "u1".into(),
      data: "%%%".into(),
    },
  )
  .await;
  send(
    &mut ws,
    &TunnelMessage::UdpClose {
      stream_id: "u1".into(),
    },
  )
  .await;
  match tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .unwrap()
    .unwrap()
  {
    TcpConsumerMsg::Close => {}
    _ => panic!("expected close"),
  }
  assert!(!state.udp_streams.lock().await.contains_key("u1"));

  ws.close(None).await.unwrap();
  wait_no_clients(&state).await;
}

#[tokio::test]
async fn udp_not_owned_rejected() {
  let state = Arc::new(test_state());
  let url = start_server(state.clone()).await;
  let mut ws = connect(&url, "test").await;
  let _cid = wait_client_id(&state).await;

  let (tx, mut rx) = mpsc::channel::<TcpConsumerMsg>(8);
  state.udp_streams.lock().await.insert(
    "u1".into(),
    crate::state::UdpStreamHandle {
      tx,
      client_id: "foreign".into(),
    },
  );
  send(
    &mut ws,
    &TunnelMessage::UdpDatagram {
      stream_id: "u1".into(),
      data: BASE64_STANDARD.encode([1u8]),
    },
  )
  .await;
  send(
    &mut ws,
    &TunnelMessage::UdpClose {
      stream_id: "u1".into(),
    },
  )
  .await;
  send(&mut ws, &base_ping()).await;
  read_until_pong(&mut ws).await;
  assert!(rx.try_recv().is_err());
  assert!(state.udp_streams.lock().await.contains_key("u1"));
}
