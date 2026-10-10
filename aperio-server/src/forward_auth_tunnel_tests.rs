//! An ask over the tunnel: it goes to the connection that would serve the
//! request, carries exactly what the server composed, and every way the
//! answer can fail to come back refuses the visitor.

use super::*;
use crate::test_support::*;
use axum::extract::ws::Message;
use std::sync::Arc;

const VISITOR: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 40));

fn cfg(cache_secs: u64) -> ForwardConfig {
  ForwardConfig {
    url: "http://127.0.0.1:7070/check".to_string(),
    request_headers: Vec::new(),
    response_headers: vec!["x-auth-user".to_string()],
    timeout: Duration::from_millis(300),
    cache: Duration::from_secs(cache_secs),
    via_client: true,
  }
}

/// A state with one connection serving everything, whose frames the test can
/// read and answer.
async fn state_with_client() -> (Arc<AppState>, tokio::sync::mpsc::Receiver<Message>) {
  let state = Arc::new(test_state());
  let (tx, rx) = tokio::sync::mpsc::channel::<Message>(8);
  let mut c = mock_client(None, None, None, None);
  c.tx = tx;
  state.clients.write().await.insert("c1".to_string(), c);
  (state, rx)
}

/// Reads the next frame as the ask it is, returning its id and the
/// question.
async fn next_ask(rx: &mut tokio::sync::mpsc::Receiver<Message>) -> (String, serde_json::Value) {
  let frame = tokio::time::timeout(Duration::from_secs(2), rx.recv())
    .await
    .expect("an ask within two seconds")
    .expect("the connection is open");
  let Message::Text(text) = frame else {
    panic!("an ask travels as text");
  };
  let value: serde_json::Value = serde_json::from_str(&text).unwrap();
  assert_eq!(value["type"], "AuthAsk");
  (value["id"].as_str().unwrap().to_string(), value)
}

async fn next_ask_from_either(
  first: &mut tokio::sync::mpsc::Receiver<Message>,
  second: &mut tokio::sync::mpsc::Receiver<Message>,
) -> (&'static str, String) {
  let (client, frame) = tokio::time::timeout(Duration::from_secs(2), async {
    tokio::select! {
      frame = first.recv() => ("a", frame),
      frame = second.recv() => ("b", frame),
    }
  })
  .await
  .expect("a client-local check must cross a tunnel");
  let Message::Text(text) = frame.expect("the connection is open") else {
    panic!("an ask travels as text");
  };
  let value: serde_json::Value = serde_json::from_str(&text).unwrap();
  (client, value["id"].as_str().unwrap().to_string())
}

fn headers_with(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
  let mut h = HeaderMap::new();
  for (k, v) in pairs {
    h.insert(*k, v.parse().unwrap());
  }
  h
}

async fn ask(state: &AppState, cfg: &ForwardConfig, headers: &HeaderMap) -> Verdict {
  let uri: axum::http::Uri = "/private/page?x=1".parse().unwrap();
  let mut asked_client = None;
  ask_over_tunnel(
    state,
    cfg,
    &axum::http::Method::GET,
    headers,
    &uri,
    Some("app.test"),
    VISITOR,
    "/private/page",
    &mut asked_client,
  )
  .await
}

#[tokio::test]
async fn the_question_carries_what_the_server_composed_and_a_2xx_admits() {
  let (state, mut rx) = state_with_client().await;
  let headers = headers_with(&[("cookie", "sid=1"), ("x-other", "not-copied")]);
  let asking = {
    let state = state.clone();
    tokio::spawn(async move { ask(&state, &cfg(0), &headers).await })
  };
  let (id, question) = next_ask(&mut rx).await;
  assert_eq!(question["url"], "http://127.0.0.1:7070/check");
  assert_eq!(question["method"], "GET");
  assert_eq!(question["host"], "app.test");
  assert_eq!(question["visitor_ip"], VISITOR.to_string());
  assert_eq!(question["response_headers"][0], "x-auth-user");
  let sent: Vec<(String, String)> =
    serde_json::from_value(question["request_headers"].clone()).unwrap();
  let name = |n: &str| sent.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
  assert_eq!(
    name("x-forwarded-uri").as_deref(),
    Some("/private/page?x=1")
  );
  assert_eq!(
    name("x-forwarded-for").as_deref(),
    Some(&*VISITOR.to_string())
  );
  assert_eq!(name("cookie").as_deref(), Some("sid=1"));
  assert_eq!(
    name("x-other"),
    None,
    "only the allow-listed headers travel"
  );

  resolve(
    &state,
    &id,
    AskAnswer {
      status: 200,
      headers: vec![
        ("x-auth-user".to_string(), "alice".to_string()),
        ("x-injected".to_string(), "nope".to_string()),
      ],
      error: None,
    },
  )
  .await;
  match asking.await.unwrap() {
    Verdict::Allow(carried) => assert_eq!(
      carried,
      vec![("x-auth-user".to_string(), "alice".to_string())],
      "only the names the operator listed come back"
    ),
    Verdict::Deny(_) => panic!("a 2xx admits"),
  }
  assert!(state.pending_auth_asks.lock().await.is_empty());
}

#[tokio::test]
async fn a_refusal_is_the_endpoints_own_with_the_headers_a_browser_needs() {
  let (state, mut rx) = state_with_client().await;
  let asking = {
    let state = state.clone();
    tokio::spawn(async move { ask(&state, &cfg(0), &HeaderMap::new()).await })
  };
  let (id, _) = next_ask(&mut rx).await;
  resolve(
    &state,
    &id,
    AskAnswer {
      status: 302,
      headers: vec![
        ("location".to_string(), "https://login.test/".to_string()),
        ("set-cookie".to_string(), "nonce=1".to_string()),
        ("x-secret".to_string(), "dropped".to_string()),
      ],
      error: None,
    },
  )
  .await;
  match asking.await.unwrap() {
    Verdict::Deny(resp) => {
      assert_eq!(resp.status(), StatusCode::FOUND);
      assert_eq!(
        resp.headers().get("location").unwrap(),
        "https://login.test/"
      );
      assert_eq!(resp.headers().get("set-cookie").unwrap(), "nonce=1");
      assert!(resp.headers().get("x-secret").is_none());
    }
    Verdict::Allow(_) => panic!("a 302 refuses"),
  }
}

#[tokio::test]
async fn every_way_the_answer_fails_to_come_back_refuses() {
  // The endpoint could not be asked.
  let (state, mut rx) = state_with_client().await;
  let asking = {
    let state = state.clone();
    tokio::spawn(async move { ask(&state, &cfg(0), &HeaderMap::new()).await })
  };
  let (id, _) = next_ask(&mut rx).await;
  resolve(
    &state,
    &id,
    AskAnswer {
      status: 0,
      headers: Vec::new(),
      error: Some("connection refused".to_string()),
    },
  )
  .await;
  assert!(matches!(asking.await.unwrap(), Verdict::Deny(_)));

  // No answer at all: the timeout refuses and the ask is forgotten.
  let asking = {
    let state = state.clone();
    tokio::spawn(async move { ask(&state, &cfg(0), &HeaderMap::new()).await })
  };
  let _ = next_ask(&mut rx).await;
  assert!(matches!(asking.await.unwrap(), Verdict::Deny(_)));
  assert!(state.pending_auth_asks.lock().await.is_empty());

  // The connection went away while the ask was open.
  let asking = {
    let state = state.clone();
    tokio::spawn(async move { ask(&state, &cfg(0), &HeaderMap::new()).await })
  };
  let _ = next_ask(&mut rx).await;
  assert_eq!(forget_client(&state, "c1").await, 1);
  assert!(matches!(asking.await.unwrap(), Verdict::Deny(_)));

  // Nothing serves the route.
  let empty = Arc::new(test_state());
  assert!(matches!(
    ask(&empty, &cfg(0), &HeaderMap::new()).await,
    Verdict::Deny(_)
  ));
}

#[tokio::test]
async fn the_ask_is_priced_from_the_visitors_bucket_and_capped_per_connection() {
  let mut config = test_config();
  // One credential attempt's worth, then nothing.
  config.ip_limit_max = 2.0;
  config.ip_limit_refill = 0.0;
  let state = Arc::new(test_state_with(config));
  let (tx, mut rx) = tokio::sync::mpsc::channel::<Message>(64);
  let mut c = mock_client(None, None, None, None);
  c.tx = tx;
  state.clients.write().await.insert("c1".to_string(), c);
  let first = {
    let state = state.clone();
    tokio::spawn(async move { ask(&state, &cfg(0), &HeaderMap::new()).await })
  };
  let (id, _) = next_ask(&mut rx).await;
  resolve(
    &state,
    &id,
    AskAnswer {
      status: 200,
      headers: Vec::new(),
      error: None,
    },
  )
  .await;
  assert!(matches!(first.await.unwrap(), Verdict::Allow(_)));
  // The bucket is spent: refused before anything crosses the tunnel.
  match ask(&state, &cfg(0), &HeaderMap::new()).await {
    Verdict::Deny(resp) => assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS),
    Verdict::Allow(_) => panic!("a spent bucket refuses"),
  }
  assert!(
    tokio::time::timeout(Duration::from_millis(100), rx.recv())
      .await
      .is_err(),
    "nothing was asked"
  );

  // And a connection holds at most MAX_ASKS_PER_CONNECTION open asks.
  {
    let mut pending = state.pending_auth_asks.lock().await;
    for i in 0..MAX_ASKS_PER_CONNECTION {
      let (tx, _rx) = oneshot::channel();
      pending.insert(
        format!("open-{i}"),
        PendingAsk {
          client_id: "c1".to_string(),
          tx,
        },
      );
    }
  }
  let mut config = test_config();
  config.ip_limit_max = 100.0;
  let roomy = Arc::new(test_state_with(config));
  let (tx, mut rx2) = tokio::sync::mpsc::channel::<Message>(64);
  let mut c = mock_client(None, None, None, None);
  c.tx = tx;
  roomy.clients.write().await.insert("c1".to_string(), c);
  {
    let mut pending = roomy.pending_auth_asks.lock().await;
    for i in 0..MAX_ASKS_PER_CONNECTION {
      let (tx, _rx) = oneshot::channel();
      pending.insert(
        format!("open-{i}"),
        PendingAsk {
          client_id: "c1".to_string(),
          tx,
        },
      );
    }
  }
  assert!(matches!(
    ask(&roomy, &cfg(0), &HeaderMap::new()).await,
    Verdict::Deny(_)
  ));
  assert!(
    tokio::time::timeout(Duration::from_millis(100), rx2.recv())
      .await
      .is_err(),
    "nothing was asked past the ceiling"
  );
}

#[tokio::test]
async fn an_admission_is_remembered_for_the_cache_span() {
  let (state, mut rx) = state_with_client().await;
  let headers = headers_with(&[("cookie", "sid=remembered")]);
  let asking = {
    let state = state.clone();
    let headers = headers.clone();
    tokio::spawn(async move { ask(&state, &cfg(30), &headers).await })
  };
  let (id, _) = next_ask(&mut rx).await;
  resolve(
    &state,
    &id,
    AskAnswer {
      status: 200,
      headers: vec![("x-auth-user".to_string(), "bob".to_string())],
      error: None,
    },
  )
  .await;
  assert!(matches!(asking.await.unwrap(), Verdict::Allow(_)));
  // The same credential again: answered from the cache, nothing crosses.
  match ask(&state, &cfg(30), &headers).await {
    Verdict::Allow(carried) => assert_eq!(carried[0].1, "bob"),
    Verdict::Deny(_) => panic!("remembered"),
  }
  assert!(
    tokio::time::timeout(Duration::from_millis(100), rx.recv())
      .await
      .is_err(),
    "the cache answered"
  );
}

#[tokio::test]
async fn cached_admission_is_scoped_to_the_client_that_granted_it() {
  let state = Arc::new(test_state());
  let (tx_a, mut rx_a) = tokio::sync::mpsc::channel::<Message>(8);
  let (tx_b, mut rx_b) = tokio::sync::mpsc::channel::<Message>(8);
  for (id, tx) in [("a", tx_a), ("b", tx_b)] {
    let mut client = mock_client(Some("app.test"), None, None, None);
    client.tx = tx;
    state.clients.write().await.insert(id.to_string(), client);
  }
  let headers = headers_with(&[("cookie", "sid=same")]);
  let first = {
    let state = state.clone();
    let headers = headers.clone();
    tokio::spawn(async move { ask(&state, &cfg(30), &headers).await })
  };
  let (first_client, first_id) = next_ask_from_either(&mut rx_a, &mut rx_b).await;
  resolve(
    &state,
    &first_id,
    AskAnswer {
      status: 200,
      headers: Vec::new(),
      error: None,
    },
  )
  .await;
  assert!(matches!(first.await.unwrap(), Verdict::Allow(_)));

  let second = {
    let state = state.clone();
    let headers = headers.clone();
    tokio::spawn(async move { ask(&state, &cfg(30), &headers).await })
  };
  let (second_client, second_id) = next_ask_from_either(&mut rx_a, &mut rx_b).await;
  assert_ne!(first_client, second_client, "round robin chooses the peer");
  resolve(
    &state,
    &second_id,
    AskAnswer {
      status: 200,
      headers: Vec::new(),
      error: None,
    },
  )
  .await;
  assert!(matches!(second.await.unwrap(), Verdict::Allow(_)));
}

#[tokio::test]
async fn aperios_own_cookies_never_cross_the_tunnel() {
  // A dashboard session that did not open this gate, a share link and the
  // affinity cookie are Aperio's; the client's endpoint gets the rest.
  let (state, mut rx) = state_with_client().await;
  let headers = headers_with(&[(
    "cookie",
    "sid=1; __Host-aperio_session=0f0e0d0c-aaaa-bbbb-cccc-000000000000; aperio_session=x; \
     aperio_share=s; aperio_affinity=c1; theme=dark",
  )]);
  let asking = {
    let state = state.clone();
    tokio::spawn(async move { ask(&state, &cfg(0), &headers).await })
  };
  let (id, question) = next_ask(&mut rx).await;
  let sent: Vec<(String, String)> =
    serde_json::from_value(question["request_headers"].clone()).unwrap();
  let cookie = sent
    .iter()
    .find(|(k, _)| k == "cookie")
    .map(|(_, v)| v.clone());
  assert_eq!(cookie.as_deref(), Some("sid=1; theme=dark"));
  assert!(!question.to_string().contains("aperio_"), "{question}");
  resolve(
    &state,
    &id,
    AskAnswer {
      status: 403,
      headers: Vec::new(),
      error: None,
    },
  )
  .await;
  assert!(matches!(asking.await.unwrap(), Verdict::Deny(_)));

  // Nothing but Aperio's cookies: no cookie header at all.
  let headers = headers_with(&[("cookie", "aperio_session=x")]);
  let asking = {
    let state = state.clone();
    tokio::spawn(async move { ask(&state, &cfg(0), &headers).await })
  };
  let (id, question) = next_ask(&mut rx).await;
  let sent: Vec<(String, String)> =
    serde_json::from_value(question["request_headers"].clone()).unwrap();
  assert!(sent.iter().all(|(k, _)| k != "cookie"), "{sent:?}");
  resolve(
    &state,
    &id,
    AskAnswer {
      status: 403,
      headers: Vec::new(),
      error: None,
    },
  )
  .await;
  let _ = asking.await;
}
