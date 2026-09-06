//! What crosses to a client-side auth endpoint and what comes back: the
//! composed headers as given, the status, only the headers a browser or the
//! allowlist needs, and a refusal when the endpoint cannot be asked.

use super::*;

/// A one-shot endpoint answering `status` with `headers`, that records the
/// request line and headers it received.
async fn endpoint(
  status: u16,
  headers: Vec<(&'static str, &'static str)>,
) -> (String, tokio::sync::oneshot::Receiver<String>) {
  let _ = rustls::crypto::ring::default_provider().install_default();
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
  tokio::spawn(async move {
    let (mut sock, _) = listener.accept().await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 8192];
    let n = sock.read(&mut buf).await.unwrap_or(0);
    let _ = seen_tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
    let mut resp = format!("HTTP/1.1 {status} X\r\n");
    for (k, v) in headers {
      resp.push_str(&format!("{k}: {v}\r\n"));
    }
    resp.push_str("content-length: 2\r\nconnection: close\r\n\r\nok");
    let _ = sock.write_all(resp.as_bytes()).await;
  });
  (format!("http://{addr}/check"), seen_rx)
}

#[tokio::test]
async fn the_question_is_sent_as_composed_and_the_answer_is_filtered() {
  let (url, seen) = endpoint(
    200,
    vec![
      ("x-auth-user", "alice"),
      ("x-private", "nope"),
      ("set-cookie", "a=1"),
    ],
  )
  .await;
  let out = answer(
    &url,
    &[
      ("x-forwarded-uri".to_string(), "/p?x=1".to_string()),
      ("cookie".to_string(), "sid=1".to_string()),
    ],
    &["x-auth-user".to_string()],
    Duration::from_secs(2),
  )
  .await;
  assert_eq!(out.status, 200);
  assert_eq!(out.error, None);
  assert!(
    out
      .headers
      .contains(&("x-auth-user".to_string(), "alice".to_string()))
  );
  assert!(
    out
      .headers
      .contains(&("set-cookie".to_string(), "a=1".to_string()))
  );
  assert!(!out.headers.iter().any(|(k, _)| k == "x-private"));
  let request = seen.await.unwrap();
  assert!(request.starts_with("GET /check HTTP/1.1"), "{request}");
  assert!(
    request
      .to_ascii_lowercase()
      .contains("x-forwarded-uri: /p?x=1")
  );
  assert!(request.to_ascii_lowercase().contains("cookie: sid=1"));
}

#[tokio::test]
async fn a_redirect_comes_back_as_itself_and_an_unreachable_endpoint_is_an_error() {
  let (url, _seen) = endpoint(302, vec![("location", "https://login.test/")]).await;
  let out = answer(&url, &[], &[], Duration::from_secs(2)).await;
  assert_eq!(out.status, 302);
  assert_eq!(
    out.headers,
    vec![("location".to_string(), "https://login.test/".to_string())]
  );
  // Nothing listens here.
  let out = answer("http://127.0.0.1:1/check", &[], &[], Duration::from_secs(1)).await;
  assert_eq!(out.status, 0);
  assert!(out.error.is_some());
}
