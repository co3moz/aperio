//! The first integration tests: the server composed and served in-process,
//! spoken to over a real socket, no subprocess and no shell harness. What
//! belongs here is what the unit tests structurally cannot see (the composed
//! middleware stack end to end, over TCP) and what e2e sees only as a black
//! box (that a login mints a session the API then accepts).
//!
//! One `#[test]` on purpose: the environment is process-global and this is
//! the only test binary that touches it, so a single test holds it for its
//! whole run instead of inventing a lock.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// One HTTP/1.1 request over a fresh connection; returns the raw response.
async fn http(addr: std::net::SocketAddr, raw: String) -> String {
  let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
  stream.write_all(raw.as_bytes()).await.unwrap();
  let mut buf = Vec::new();
  tokio::time::timeout(
    std::time::Duration::from_secs(5),
    stream.read_to_end(&mut buf),
  )
  .await
  .expect("the server answers within the deadline")
  .unwrap();
  String::from_utf8_lossy(&buf).to_string()
}

fn get(path: &str, extra: &str) -> String {
  format!("GET {path} HTTP/1.1\r\nhost: test.local\r\n{extra}connection: close\r\n\r\n")
}

#[test]
fn the_server_composes_serves_and_authenticates_over_a_real_socket() {
  let dir = std::env::temp_dir().join(format!("aperio-composed-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  // SAFETY: single-threaded still; the runtime is built below.
  unsafe {
    std::env::set_var("APERIO_SERVER_TOKEN", TOKEN);
    std::env::set_var("APERIO_DATA_DIR", dir.to_str().unwrap());
  }

  let rt = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(2)
    .enable_all()
    .build()
    .unwrap();
  rt.block_on(async {
    let composed = aperio_server::testkit::compose()
      .await
      .expect("a clean environment composes");
    assert_eq!(composed.connected_clients().await, 0);
    let (addr, server) = composed.serve_ephemeral().await;

    // Liveness, over the wire.
    let health = http(addr, get("/aperio/health", "")).await;
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");
    assert!(health.contains("\"connected_clients\":0"), "{health}");

    // The admin API refuses a session-less caller.
    let refused = http(addr, get("/aperio/api/stats", "")).await;
    assert!(
      refused.starts_with("HTTP/1.1 401") || refused.starts_with("HTTP/1.1 30"),
      "{refused}"
    );

    // Basic-auth login as aperio:<master token> mints a session cookie...
    use base64::prelude::*;
    let credential = BASE64_STANDARD.encode(format!("aperio:{TOKEN}"));
    let login = http(
      addr,
      format!(
        "POST /aperio/auth HTTP/1.1\r\nhost: test.local\r\nauthorization: Basic {credential}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
      ),
    )
    .await;
    assert!(login.starts_with("HTTP/1.1 200"), "{login}");
    let cookie = login
      .lines()
      .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
      .and_then(|l| l.split_once(':'))
      .map(|(_, v)| v.trim().split(';').next().unwrap_or_default().to_string())
      .expect("the login answers with a session cookie");

    // ...which the API then accepts: the full middleware stack, end to end.
    let stats = http(addr, get("/aperio/api/stats", &format!("cookie: {cookie}\r\n"))).await;
    assert!(stats.starts_with("HTTP/1.1 200"), "{stats}");
    assert!(stats.contains("\"total_requests\""), "{stats}");

    // And the fence unit tests assert on answers here over TCP too.
    let fenced = http(addr, get("/aperio/api/definitely-not-a-route", "")).await;
    assert!(fenced.starts_with("HTTP/1.1 404"), "{fenced}");

    server.abort();
  });
}
