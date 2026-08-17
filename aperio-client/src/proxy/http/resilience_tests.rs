//! What happens when a backend misbehaves: which methods a retry covers, the
//! breaker opening on a run of failures and letting exactly one request probe
//! afterwards, and a pooled connection the backend had already closed being
//! dialed again rather than counted as a failure.

use super::super::http_tests::*;
use super::*;

#[test]
fn retry_is_limited_to_idempotent_methods_unless_opted_in() {
  let cautious = BackendResilience::new(3, 10, false, 0, 30);
  assert!(cautious.may_retry_method("GET"));
  assert!(cautious.may_retry_method("head"), "the check ignores case");
  assert!(cautious.may_retry_method("DELETE"));
  // A retried write may reach the backend twice, so it is opt-in.
  assert!(!cautious.may_retry_method("POST"));
  assert!(!cautious.may_retry_method("PATCH"));

  let eager = BackendResilience::new(3, 10, true, 0, 30);
  assert!(eager.may_retry_method("POST"));
}

#[test]
fn a_disabled_breaker_never_opens() {
  let r = BackendResilience::new(1, 10, false, 0, 30);
  for _ in 0..100 {
    assert!(!r.record_failure(), "failures are not counted when off");
    assert!(matches!(r.check(), BreakerVerdict::Proceed));
  }
}

#[test]
fn the_breaker_opens_on_consecutive_failures_and_reports_it_once() {
  let r = BackendResilience::new(1, 10, false, 3, 30);
  assert!(!r.record_failure());
  assert!(!r.record_failure());
  assert!(matches!(r.check(), BreakerVerdict::Proceed), "still closed");
  assert!(
    r.record_failure(),
    "the third failure opens it, and says so"
  );
  // Further failures keep it open but do not re-announce it, so a flood
  // produces one line rather than one per request.
  assert!(!r.record_failure());
  match r.check() {
    BreakerVerdict::Open(left) => assert!(left.as_secs() <= 30),
    BreakerVerdict::Proceed => panic!("expected the breaker to be open"),
  }
}

#[test]
fn a_success_resets_the_failure_run() {
  let r = BackendResilience::new(1, 10, false, 3, 30);
  r.record_failure();
  r.record_failure();
  r.record_success();
  // The count restarted, so two more failures are not enough to open it.
  assert!(!r.record_failure());
  assert!(!r.record_failure());
  assert!(matches!(r.check(), BreakerVerdict::Proceed));
}

#[test]
fn the_open_window_lets_exactly_one_request_probe_the_backend() {
  // A one-second window, so the test can wait it out without being slow.
  let r = BackendResilience::new(1, 10, false, 1, 1);
  assert!(r.record_failure(), "one failure is the threshold here");
  assert!(matches!(r.check(), BreakerVerdict::Open(_)));
  std::thread::sleep(std::time::Duration::from_millis(1100));
  // The first caller after the window is the probe...
  assert!(matches!(r.check(), BreakerVerdict::Proceed));
  // ...and until it reports back, everyone else is let through too, because
  // the window was cleared. What keeps a dead backend from being hammered is
  // that the probe's failure opens a fresh window.
  assert!(r.record_failure());
  assert!(matches!(r.check(), BreakerVerdict::Open(_)));
}

#[tokio::test]
async fn a_connection_the_backend_had_already_closed_is_dialed_again_once() {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  // An HTTP client keeps connections alive and reuses them, and the backend
  // closes idle ones on its own schedule, so there is a window where the
  // request goes onto a socket the backend has just finished with. hyper
  // reports that as IncompleteMessage, with no response head, and it used to
  // reach the visitor as a 502 even though nothing was wrong with the
  // backend. Under load that window is hit constantly, which is why the same
  // backend behind any mainstream proxy does not produce these.
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  let target_url = format!("http://127.0.0.1:{}", port);

  tokio::spawn(async move {
    // First connection: answer once, keeping it alive, then close the moment
    // the next request arrives on it without answering. That is exactly the
    // shape of an idle timeout that fires as a request is being written.
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0u8; 2048];
      let _ = socket.read(&mut buf).await;
      let _ = socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nfirst")
        .await;
      let _ = socket.read(&mut buf).await;
      drop(socket);
    }
    // Second connection: the re-dial, answered normally.
    if let Ok((mut socket, _)) = listener.accept().await {
      let mut buf = [0u8; 2048];
      let _ = socket.read(&mut buf).await;
      let _ = socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecond")
        .await;
      let mut sink = [0u8; 64];
      while matches!(socket.read(&mut sink).await, Ok(n) if n > 0) {}
    }
  });

  let (tx, _rx) = mpsc::channel::<Message>(64);
  // Retrying is off, as it is by default: this is not the retry policy, which
  // is about a backend that failed, it is the client's own pool racing the
  // backend's idle timeout.
  let ctx = test_ctx(&target_url, tx);
  assert_eq!(ctx.resilience.attempts, 1);

  let call = |n: u32| {
    handle_incoming_request(
      &ctx,
      ForwardRequest {
        id: format!("req-pool-{n}"),
        method: "GET".to_string(),
        uri: "/".to_string(),
        headers: vec![],
        body: None,
        raw_body: None,
      },
      None,
      false,
      false,
    )
  };

  let Some(TunnelMessage::Response { status, .. }) = call(1).await else {
    panic!("the first request should be answered");
  };
  assert_eq!(status, 200);

  // The second goes onto the pooled connection the backend is about to drop.
  let Some(TunnelMessage::Response { status, body, .. }) = call(2).await else {
    panic!("the second request should be answered");
  };
  assert_eq!(
    status, 200,
    "a connection the backend had already closed must be dialed again, not reported as a 502"
  );
  use base64::prelude::*;
  let decoded = BASE64_STANDARD.decode(body.unwrap()).unwrap();
  assert_eq!(String::from_utf8(decoded).unwrap(), "second");
}
