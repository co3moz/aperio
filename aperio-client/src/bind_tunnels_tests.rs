//! How `aperio-client bind` resolves what it was given: which tunnel a name
//! reaches, what a short-form entry means, which port is chosen, and that one
//! entry that cannot resolve does not take its siblings down with it.

use super::*;
use aperio_config::{BindEntry, BindValue, Named};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn settings_with(token: Option<&str>, binds: Named<BindValue>) -> ClientSettings {
  let mut settings = crate::tests::settings("");
  settings.token = token.map(|t| t.to_string());
  settings.binds = binds;
  settings
}

/// One tunnel as discovery would report it.
fn view(name: &str, target: &str, protocol: &str) -> TunnelView {
  TunnelView {
    custom_name: None,
    name: name.to_string(),
    protocol: protocol.to_string(),
    target: target.to_string(),
    available: true,
    encrypt: false,
    idle_timeout: None,
    org: None,
    discovered_with: None,
  }
}

/// The same, owned by a named organization.
fn view_in(org: &str, name: &str, target: &str) -> TunnelView {
  TunnelView {
    org: Some(org.to_string()),
    ..view(name, target, "tcp")
  }
}

fn entry_with(port: Option<u16>) -> BindValue {
  BindValue::Entry(BindEntry {
    port,
    ..BindEntry::default()
  })
}

// ---------------------------------------------------------------------------
// plan: turning configuration into bindings
// ---------------------------------------------------------------------------

#[test]
fn a_name_resolves_against_what_the_server_lists() {
  let mut map = Named::new();
  map.insert("pg_main".to_string(), entry_with(Some(15432)));
  let visible = vec![view("pg_main", "127.0.0.1:5432", "tcp")];
  let planned = plan(
    &settings_with(Some("apr_t"), map),
    &[],
    &visible,
    &Some("apr_t".to_string()),
  )
  .unwrap();
  assert_eq!(planned.len(), 1);
  assert_eq!(planned[0].port, 15432);
  assert_eq!(planned[0].name, "pg_main");
}

#[test]
fn a_short_form_entry_is_just_the_local_port() {
  let mut map = Named::new();
  map.insert("dns".to_string(), BindValue::Port(5300));
  let visible = vec![view("dns", "192.168.3.100:53", "udp")];
  let planned = plan(
    &settings_with(Some("apr_t"), map),
    &[],
    &visible,
    &Some("apr_t".to_string()),
  )
  .unwrap();
  assert_eq!(planned[0].port, 5300);
}

#[test]
fn an_empty_section_binds_everything_the_token_may_reach() {
  let visible = vec![
    view("pg_main", "127.0.0.1:5432", "tcp"),
    view("dns", "192.168.3.100:53", "udp"),
  ];
  let planned = plan(
    &settings_with(Some("apr_t"), Named::new()),
    &[],
    &visible,
    &Some("apr_t".to_string()),
  )
  .unwrap();
  assert_eq!(planned.len(), 2);
}

#[test]
fn an_unresolvable_entry_does_not_take_down_the_others() {
  // A break-glass tool: three of four tunnels up during an incident beats
  // none, so an unknown name is reported and skipped, not fatal.
  let mut map = Named::new();
  map.insert("pg_main".to_string(), entry_with(None));
  map.insert("does-not-exist".to_string(), entry_with(None));
  let visible = vec![view("pg_main", "127.0.0.1:5432", "tcp")];
  let planned = plan(
    &settings_with(Some("apr_t"), map),
    &[],
    &visible,
    &Some("apr_t".to_string()),
  )
  .unwrap();
  assert_eq!(planned.len(), 1);
  assert!(planned[0].label.contains("pg_main"));
}

#[test]
fn an_explicit_selection_that_cannot_resolve_is_an_error() {
  let visible = vec![view("pg_main", "127.0.0.1:5432", "tcp")];
  let err = plan(
    &settings_with(Some("apr_t"), Named::new()),
    &["nope".to_string()],
    &visible,
    &Some("apr_t".to_string()),
  )
  .unwrap_err();
  assert!(err.contains("nope"), "got: {err}");
  // The error names what *can* be bound, which is the thing the operator
  // needs and could not otherwise find out.
  assert!(err.contains("pg_main"), "got: {err}");
}

#[test]
fn a_binding_without_any_token_is_an_error() {
  let visible = vec![view("pg_main", "127.0.0.1:5432", "tcp")];
  let err = plan(
    &settings_with(None, Named::new()),
    &["pg_main".to_string()],
    &visible,
    &None,
  )
  .unwrap_err();
  assert!(err.contains("token"), "got: {err}");
}

// ---------------------------------------------------------------------------
// local port policy
// ---------------------------------------------------------------------------

#[test]
fn the_configured_port_wins() {
  assert_eq!(local_port(&view("t", "10.0.0.1:5432", "tcp"), Some(1)), 1);
}

#[test]
fn the_declared_port_is_the_default() {
  // Unchanged from before named tunnels: binders in the field depend on it.
  assert_eq!(local_port(&view("t", "10.0.0.1:5432", "tcp"), None), 5432);
}

#[test]
fn a_privileged_or_missing_port_falls_back_to_a_derived_one() {
  // Port 53 cannot be bound without privileges, and a target with no port at
  // all used to be skipped outright.
  let dns = local_port(&view("dns", "192.168.3.100:53", "udp"), None);
  assert!((DERIVED_PORT_BASE..DERIVED_PORT_BASE + DERIVED_PORT_SPAN).contains(&dns));
  let portless = local_port(&view("odd", "no-port-here", "tcp"), None);
  assert!((DERIVED_PORT_BASE..DERIVED_PORT_BASE + DERIVED_PORT_SPAN).contains(&portless));
}

#[test]
fn a_derived_port_is_stable_and_name_dependent() {
  // Stability is the point: the port must survive a restart, or whatever
  // connects to it needs reconfiguring every time.
  assert_eq!(derived_port("pg_main"), derived_port("pg_main"));
  assert_ne!(derived_port("pg_main"), derived_port("redis"));
}

// ---------------------------------------------------------------------------
// URL building
// ---------------------------------------------------------------------------

#[test]
fn a_binding_addresses_the_tunnel_by_name() {
  // Every binding resolves to a name, including one keyed by a client id, so
  // there is a single address form on the wire.
  let url = tunnel_ws_url("https://tunnel.example.com", "/aperio/tcp", "pg_main").unwrap();
  assert!(
    url.starts_with("wss://tunnel.example.com/aperio/tcp?"),
    "{url}"
  );
  assert!(url.contains("tunnel=pg_main"), "{url}");
  assert!(!url.contains("client="), "{url}");
}

#[test]
fn an_unusable_server_url_is_reported() {
  let err = tunnel_ws_url("not a url", "/aperio/tcp", "x");
  assert!(err.is_err());
}

// ---------------------------------------------------------------------------
// discovery
// ---------------------------------------------------------------------------

fn init_tracing() {
  use std::sync::Once;
  static ONCE: Once = Once::new();
  ONCE.call_once(|| {
    let _ = tracing_subscriber::fmt()
      .with_max_level(tracing::Level::TRACE)
      .with_test_writer()
      .try_init();
  });
}

/// A tiny loopback HTTP server. The handler receives the request path and
/// returns `(status_code, json_body)`. Returns the server's base URL.
async fn spawn_http<F>(handler: F) -> String
where
  F: Fn(&str) -> (u16, String) + Send + Sync + 'static,
{
  let handler = Arc::new(handler);
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    loop {
      let Ok((mut sock, _)) = listener.accept().await else {
        return;
      };
      let handler = handler.clone();
      tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req
          .lines()
          .next()
          .and_then(|l| l.split_whitespace().nth(1))
          .unwrap_or("/")
          .to_string();
        let (status, body) = handler(&path);
        let resp = format!(
          "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.flush().await;
      });
    }
  });
  format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn discovery_reads_the_listing() {
  init_tracing();
  let body = serde_json::json!([
    {
      "name": "pg_main",
      "protocol": "tcp",
      "target": "127.0.0.1:5432",
      "client_id": "peer-1",
      "paths": 2,
      "available": true,
      "encrypt": false,
      "idle_timeout": null,
      "token_name": "ops"
    }
  ])
  .to_string();
  let server = spawn_http(move |path| {
    assert_eq!(path, "/aperio/tunnels", "the listing endpoint is called");
    (200, body.clone())
  })
  .await;
  let got = discover(&server, &["apr_test".to_string()]).await;
  assert_eq!(got.len(), 1);
  assert_eq!(got[0].name, "pg_main");
  assert!(got[0].available);
}

#[tokio::test]
async fn discovery_retries_while_the_listing_is_empty() {
  init_tracing();
  // A binder may legitimately start before the clients it binds, so an empty
  // listing is a wait rather than a failure. Cut the wait short by timing out.
  let server = spawn_http(|_| (200, "[]".to_string())).await;
  let waited = tokio::time::timeout(
    Duration::from_millis(300),
    discover(&server, &["t".to_string()]),
  )
  .await;
  assert!(waited.is_err(), "an empty listing must keep retrying");
}

// ---------------------------------------------------------------------------
// end to end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn binding_opens_listeners_for_the_discovered_tunnels() {
  init_tracing();
  let body = serde_json::json!([
    {
      "name": "pg_main",
      "protocol": "tcp",
      "target": "127.0.0.1:5432",
      "client_id": "peer-1",
      "paths": 1,
      "available": true,
      "encrypt": false,
      "idle_timeout": null,
      "token_name": null
    },
    {
      "name": "dns",
      "protocol": "udp",
      "target": "192.168.3.100:53",
      "client_id": "peer-1",
      "paths": 1,
      "available": false,
      "encrypt": false,
      "idle_timeout": 30,
      "token_name": null
    },
    {
      "name": "vault",
      "protocol": "tcp",
      "target": "127.0.0.1:8200",
      "client_id": "peer-1",
      "paths": 1,
      "available": true,
      "encrypt": true,
      "idle_timeout": null,
      "token_name": null
    }
  ])
  .to_string();
  let server = spawn_http(move |_| (200, body.clone())).await;

  let mut map = Named::new();
  map.insert("pg_main".to_string(), BindValue::Port(39210));
  map.insert("dns".to_string(), BindValue::Port(39211));
  // An encrypted tunnel with no psk: warns, still binds.
  map.insert("vault".to_string(), BindValue::Port(39212));
  let settings = settings_with(Some("apr_test"), map);

  // run_bind_tunnels never returns, so run it in the background and then
  // connect to one of the listeners it opened.
  let server2 = server.clone();
  tokio::spawn(async move {
    run_bind_tunnels(&settings, &server2, &[]).await;
  });
  tokio::time::sleep(Duration::from_millis(500)).await;
  let connected = tokio::net::TcpStream::connect("127.0.0.1:39210").await;
  assert!(
    connected.is_ok(),
    "the tcp tunnel should be listening locally"
  );
  if let Ok(mut c) = connected {
    let _ = c.write_all(b"hello").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

#[tokio::test]
async fn discovery_asks_every_configured_credential() {
  init_tracing();
  // The older spelling puts the token on the entry and has no server token at
  // all, so asking only the layered one would find nothing and the binder
  // would exit before binding anything.
  let body = serde_json::json!([
    {
      "name": "pg_main",
      "protocol": "tcp",
      "target": "127.0.0.1:5432",
      "client_id": "peer-1",
      "paths": 1,
      "available": true,
      "encrypt": false,
      "idle_timeout": null,
      "token_name": null
    }
  ])
  .to_string();
  let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
  let recorded = seen.clone();
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    loop {
      let Ok((mut sock, _)) = listener.accept().await else {
        return;
      };
      let (body, recorded) = (body.clone(), recorded.clone());
      tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        let bearer = req
          .lines()
          .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
          .and_then(|l| l.split_whitespace().last())
          .unwrap_or("")
          .to_string();
        recorded.lock().unwrap().push(bearer.clone());
        // Only the entry's own token may see anything.
        let payload = if bearer == "apr_entry" {
          body
        } else {
          "[]".to_string()
        };
        let resp = format!(
          "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
          payload.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.flush().await;
      });
    }
  });

  let found = discover(
    &format!("http://127.0.0.1:{port}"),
    &["apr_layered".to_string(), "apr_entry".to_string()],
  )
  .await;
  assert_eq!(found.len(), 1);
  assert_eq!(
    found[0].discovered_with.as_deref(),
    Some("apr_entry"),
    "a binding must use the credential that could actually see it"
  );
  let asked = seen.lock().unwrap().clone();
  assert!(asked.contains(&"apr_layered".to_string()));
  assert!(asked.contains(&"apr_entry".to_string()));
}

#[tokio::test]
async fn a_combined_tunnel_opens_both_listeners_on_one_port() {
  init_tracing();
  // The whole point of `tcp/udp`: one name, one entry, one local port, and
  // both transports answering on it. TCP and UDP are separate port spaces, so
  // sharing the number is not a conflict.
  let body = serde_json::json!([
    {
      "name": "dns",
      "protocol": "tcp/udp",
      "target": "192.168.3.100:53",
      "client_id": "peer-1",
      "paths": 1,
      "available": true,
      "encrypt": false,
      "idle_timeout": 30,
      "token_name": null
    }
  ])
  .to_string();
  let server = spawn_http(move |_| (200, body.clone())).await;

  let mut map = Named::new();
  map.insert("dns".to_string(), BindValue::Port(39230));
  let settings = settings_with(Some("apr_test"), map);

  let server2 = server.clone();
  tokio::spawn(async move {
    run_bind_tunnels(&settings, &server2, &[]).await;
  });
  tokio::time::sleep(Duration::from_millis(500)).await;

  assert!(
    tokio::net::TcpStream::connect("127.0.0.1:39230")
      .await
      .is_ok(),
    "the tcp half should be listening"
  );
  // The udp half holds the same port number: binding it ourselves must fail.
  assert!(
    tokio::net::UdpSocket::bind("127.0.0.1:39230")
      .await
      .is_err(),
    "the udp half should hold the same local port"
  );
}

#[test]
fn a_client_id_is_not_a_name() {
  // Addressing a peer by its id bound every tunnel it declared, under an id
  // that changed with every run that did not pin one. A binding names one
  // tunnel, the way a file names it.
  let mut map = Named::new();
  map.insert(
    "3f2504e0-4f89-41d3-9a0c-0305e82c3301".to_string(),
    entry_with(None),
  );
  let visible = vec![view("pg_main", "127.0.0.1:5432", "tcp")];
  let planned = plan(
    &settings_with(Some("apr_t"), map),
    &[],
    &visible,
    &Some("apr_t".to_string()),
  )
  .unwrap();
  assert!(planned.is_empty(), "an id binds nothing");
}

#[test]
fn an_org_qualified_key_binds_that_organizations_tunnel() {
  // A binder that can see two organizations sees the same name twice; the
  // qualifier is how a file says which one it means, and it is the spelling
  // the dashboard shows and the server's `expose:` accepts.
  let visible = vec![
    view_in("payments", "postgres", "127.0.0.1:5432"),
    view_in("billing", "postgres", "127.0.0.1:5433"),
  ];
  let mut map = Named::new();
  map.insert("billing@postgres".to_string(), BindValue::Port(15433));
  let planned = plan(
    &settings_with(Some("apr_t"), map),
    &[],
    &visible,
    &Some("apr_t".to_string()),
  )
  .expect("the qualified key resolves");
  assert_eq!(planned.len(), 1);
  assert_eq!(planned[0].decl.target, "127.0.0.1:5433");
  assert_eq!(planned[0].port, 15433);
}

#[test]
fn a_qualified_key_naming_the_wrong_organization_binds_nothing() {
  // An entry that cannot be resolved is reported and skipped rather than
  // taking the run down, as every unresolvable key is; what matters here is
  // that it does not quietly fall back to the same name in another
  // organization.
  let visible = vec![view_in("payments", "postgres", "127.0.0.1:5432")];
  let mut map = Named::new();
  map.insert("billing@postgres".to_string(), BindValue::Port(15433));
  let planned = plan(
    &settings_with(Some("apr_t"), map),
    &[],
    &visible,
    &Some("apr_t".to_string()),
  )
  .expect("an unresolvable entry is skipped, not fatal");
  assert!(planned.is_empty(), "{planned:?}");
}

#[test]
fn master_is_the_qualifier_for_the_built_in_organization() {
  let visible = vec![view("postgres", "127.0.0.1:5432", "tcp")];
  let mut map = Named::new();
  map.insert("master@postgres".to_string(), BindValue::Port(15432));
  let planned = plan(
    &settings_with(Some("apr_t"), map),
    &[],
    &visible,
    &Some("apr_t".to_string()),
  )
  .expect("master@ resolves the unowned tunnel");
  assert_eq!(planned.len(), 1);
}

#[test]
fn a_bare_name_that_two_organizations_carry_is_refused_rather_than_guessed() {
  // The whole point of the qualifier. Binding the first match would put a
  // local port on another organization's database and say nothing.
  let visible = vec![
    view_in("payments", "postgres", "127.0.0.1:5432"),
    view_in("billing", "postgres", "127.0.0.1:5433"),
  ];
  let mut map = Named::new();
  map.insert("postgres".to_string(), BindValue::Port(15432));
  let planned = plan(
    &settings_with(Some("apr_t"), map),
    &[],
    &visible,
    &Some("apr_t".to_string()),
  )
  .expect("an unresolvable entry is skipped, not fatal");
  assert!(planned.is_empty(), "{planned:?}");

  // The same key given explicitly on the command line reports why.
  let err = plan(
    &settings_with(Some("apr_t"), Named::new()),
    &["postgres".to_string()],
    &visible,
    &Some("apr_t".to_string()),
  )
  .expect_err("an explicit key cannot be guessed at either");
  assert!(err.contains("payments@postgres"), "{err}");
  assert!(err.contains("billing@postgres"), "{err}");
}

#[tokio::test]
async fn discovery_error_answers_are_nothing_not_a_crash() {
  init_tracing();
  let http = crate::test_http_client();
  // A rejected token, a garbage body, and a plain server error: each is
  // "nothing from this token", never a panic. (404 is deliberately absent
  // here: it exits the process by design, a server predating discovery.)
  let unauthorized = spawn_http(|_| (401, String::new())).await;
  assert!(
    list_for(&http, &format!("{unauthorized}/aperio/tunnels"), "t")
      .await
      .is_empty()
  );
  let garbage = spawn_http(|_| (200, "not json".to_string())).await;
  assert!(
    list_for(&http, &format!("{garbage}/aperio/tunnels"), "t")
      .await
      .is_empty()
  );
  let flaky = spawn_http(|_| (500, String::new())).await;
  assert!(
    list_for(&http, &format!("{flaky}/aperio/tunnels"), "t")
      .await
      .is_empty()
  );
  // A server that is not there at all.
  assert!(
    list_for(&http, "http://127.0.0.1:9/aperio/tunnels", "t")
      .await
      .is_empty()
  );
}

#[test]
fn the_ws_url_carries_the_tunnel_name() {
  let url = tunnel_ws_url(
    "https://tunnel.example.com",
    "/aperio/tunnel-stream",
    "pg_main",
  )
  .unwrap();
  assert!(url.starts_with("wss://tunnel.example.com"), "{url}");
  assert!(url.contains("tunnel=pg_main"), "{url}");
  assert!(tunnel_ws_url("not a url", "/x", "pg").is_err());
}
