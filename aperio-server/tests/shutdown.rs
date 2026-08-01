//! The graceful-shutdown path, driven by a real SIGTERM to this very process.
//!
//! Its own integration binary on purpose, and a single test on purpose:
//! `shutdown_signal` installs process-global signal handlers and, once
//! triggered, arms a ten-second force-exit fallback. Inside the unit-test
//! binary either would be sabotage (the fallback would `exit(0)` a suite that
//! runs longer than ten seconds); in a one-test process that finishes in
//! under a second, both are contained, the process is about to exit anyway.

#[test]
fn sigterm_drains_notifies_clients_and_returns() {
  let dir = std::env::temp_dir().join(format!("aperio-shutdown-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir).unwrap();
  // SAFETY: single-threaded still; the runtime is built below.
  unsafe {
    std::env::set_var("APERIO_SERVER_TOKEN", "0123456789abcdef0123456789abcdef");
    std::env::set_var("APERIO_DATA_DIR", dir.to_str().unwrap());
    std::env::set_var("HOST", "127.0.0.1");
    // Port 0: the OS picks a free one; nothing here dials it, so nothing
    // needs to know it. REUSEPORT covers bind_listener's socket2 path, the
    // zero-downtime-restart bind the plain path never touches.
    std::env::set_var("PORT", "0");
    std::env::set_var("APERIO_REUSEPORT", "1");
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
    let mut probe = composed.insert_probe_client().await;
    let serving = tokio::spawn(composed.serve_until_shutdown());

    // Give the serve loop a moment to bind and install its signal handlers,
    // then deliver the same signal a deploy sends.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    unsafe {
      libc::kill(std::process::id() as i32, libc::SIGTERM);
    }

    // The serve future returns on its own: graceful shutdown completed, well
    // inside the ten-second force-exit fallback.
    tokio::time::timeout(std::time::Duration::from_secs(8), serving)
      .await
      .expect("graceful shutdown completes before the force-exit fallback")
      .unwrap();

    // The connected client was told, so it stops reconnect-backing-off.
    let notice = tokio::time::timeout(std::time::Duration::from_secs(2), probe.recv())
      .await
      .expect("the shutdown notice arrives")
      .expect("the channel is still open");
    let axum::extract::ws::Message::Text(text) = notice else {
      panic!("the shutdown notice is a text frame");
    };
    assert!(text.contains("ServerShutdown"), "{text}");
  });

  // Reaching this line is itself the last assertion: the process survived
  // the whole graceful path, so the force-exit fallback never fired.
}
