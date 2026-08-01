//! Unit tests for the OTLP/telemetry helpers. These cover the pure env-driven
//! resolvers, the W3C header extractor/injector adapters, span construction and
//! status recording, plus the (process-global) subscriber `init` in all three
//! branches. `init` installs a global default subscriber, so it can succeed at
//! most once per test binary; the disabled and error branches are exercised via
//! `catch_unwind` (their bodies run right up to the terminal `.init()` call,
//! which then panics because the global default is already set).

use super::*;
use tracing_subscriber::EnvFilter;

/// Serializes every test that mutates process-global environment variables or
/// the global tracing/propagator state.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The full set of env vars the resolvers read, saved/restored around a test so
/// tests never leak state into one another.
const KEYS: &[&str] = &[
  "APERIO_OTEL",
  "APERIO_OTEL_ENDPOINT",
  "OTEL_EXPORTER_OTLP_ENDPOINT",
  "APERIO_OTEL_PROTOCOL",
  "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
  "OTEL_EXPORTER_OTLP_PROTOCOL",
  "APERIO_OTEL_SERVICE_NAME",
  "OTEL_SERVICE_NAME",
  "APERIO_OTEL_SAMPLE_RATE",
];

/// Snapshot of the telemetry env vars; restores them (or removes them) on drop.
struct EnvSnapshot {
  saved: Vec<(&'static str, Option<String>)>,
}

impl EnvSnapshot {
  fn take() -> Self {
    let saved = KEYS.iter().map(|k| (*k, std::env::var(k).ok())).collect();
    // Start every env-driven test from a clean slate.
    for k in KEYS {
      unsafe { std::env::remove_var(k) };
    }
    Self { saved }
  }
}

impl Drop for EnvSnapshot {
  fn drop(&mut self) {
    for (k, v) in &self.saved {
      match v {
        Some(val) => unsafe { std::env::set_var(k, val) },
        None => unsafe { std::env::remove_var(k) },
      }
    }
  }
}

fn set(key: &str, val: &str) {
  unsafe { std::env::set_var(key, val) };
}

// --------------------------------------------------------------------------
// env_flag
// --------------------------------------------------------------------------

#[test]
fn env_flag_recognizes_truthy_and_falsy_values() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();

  assert!(!env_flag("APERIO_OTEL"), "unset is false");

  set("APERIO_OTEL", "1");
  assert!(env_flag("APERIO_OTEL"), "\"1\" is true");

  set("APERIO_OTEL", "true");
  assert!(env_flag("APERIO_OTEL"), "\"true\" is true");

  set("APERIO_OTEL", "TRUE");
  assert!(env_flag("APERIO_OTEL"), "case-insensitive true");

  set("APERIO_OTEL", "0");
  assert!(!env_flag("APERIO_OTEL"), "\"0\" is false");

  set("APERIO_OTEL", "yes");
  assert!(!env_flag("APERIO_OTEL"), "arbitrary strings are false");

  set("APERIO_OTEL", "");
  assert!(!env_flag("APERIO_OTEL"), "empty is false");
}

// --------------------------------------------------------------------------
// resolve_endpoint
// --------------------------------------------------------------------------

#[test]
fn resolve_endpoint_defaults_when_unset() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  assert_eq!(resolve_target().endpoint, "http://localhost:4318/v1/traces");
}

#[test]
fn resolve_endpoint_appends_signal_path_to_base_url() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_ENDPOINT", "http://collector:4318");
  assert_eq!(resolve_target().endpoint, "http://collector:4318/v1/traces");
}

#[test]
fn resolve_endpoint_strips_trailing_slash_before_appending() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_ENDPOINT", "http://collector:4318/");
  assert_eq!(resolve_target().endpoint, "http://collector:4318/v1/traces");
}

#[test]
fn resolve_endpoint_keeps_existing_signal_path() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_ENDPOINT", "http://collector:4318/v1/traces");
  assert_eq!(resolve_target().endpoint, "http://collector:4318/v1/traces");
}

#[test]
fn resolve_endpoint_trims_and_falls_back_to_conventional_when_aperio_unset() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  // Aperio var unset -> conventional var wins; surrounding whitespace trimmed.
  set("OTEL_EXPORTER_OTLP_ENDPOINT", "  http://conv:4318  ");
  assert_eq!(resolve_target().endpoint, "http://conv:4318/v1/traces");
}

#[test]
fn resolve_endpoint_blank_value_falls_through_to_default() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  // A present-but-blank value trims to empty -> filtered out -> default.
  set("APERIO_OTEL_ENDPOINT", "   ");
  assert_eq!(resolve_target().endpoint, "http://localhost:4318/v1/traces");
}

#[test]
fn resolve_endpoint_prefers_aperio_var_over_conventional() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_ENDPOINT", "http://aperio:4318");
  set("OTEL_EXPORTER_OTLP_ENDPOINT", "http://conv:4318");
  assert_eq!(resolve_target().endpoint, "http://aperio:4318/v1/traces");
}

// --------------------------------------------------------------------------
// resolve_target (transport selection)
// --------------------------------------------------------------------------

#[test]
fn resolve_target_defaults_to_http() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  let target = resolve_target();
  assert_eq!(target.protocol, OtlpProtocol::Http);
  assert_eq!(target.note, None);
}

#[test]
fn resolve_target_infers_grpc_from_the_grpc_port() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_ENDPOINT", "http://collector:4317");
  let target = resolve_target();
  assert_eq!(target.protocol, OtlpProtocol::Grpc);
  // gRPC takes the bare base URL, never the HTTP signal path.
  assert_eq!(target.endpoint, "http://collector:4317");
  // Inference agreeing with the port is the expected case, not a surprise.
  assert_eq!(target.note, None);
}

#[test]
fn resolve_target_defaults_the_endpoint_to_the_chosen_transports_port() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_PROTOCOL", "grpc");
  let target = resolve_target();
  assert_eq!(target.protocol, OtlpProtocol::Grpc);
  assert_eq!(target.endpoint, "http://localhost:4317");
}

#[test]
fn resolve_target_strips_the_signal_path_for_grpc() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_PROTOCOL", "grpc");
  set("APERIO_OTEL_ENDPOINT", "http://collector:4317/v1/traces");
  assert_eq!(resolve_target().endpoint, "http://collector:4317");
}

#[test]
fn resolve_target_accepts_the_spec_spelling_and_prefers_the_aperio_var() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf");
  assert_eq!(resolve_target().protocol, OtlpProtocol::Http);

  set("APERIO_OTEL_PROTOCOL", "grpc");
  assert_eq!(resolve_target().protocol, OtlpProtocol::Grpc);
}

#[test]
fn resolve_target_notes_a_protocol_pinned_against_the_port() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  // Explicit wins over inference, but the mismatch is worth saying out loud:
  // the failure mode it usually precedes is every span silently dropped.
  set("APERIO_OTEL_PROTOCOL", "http");
  set("APERIO_OTEL_ENDPOINT", "http://collector:4317");
  let target = resolve_target();
  assert_eq!(target.protocol, OtlpProtocol::Http);
  assert_eq!(target.endpoint, "http://collector:4317/v1/traces");
  assert!(
    target.note.as_deref().unwrap_or_default().contains("4317"),
    "unexpected note: {:?}",
    target.note
  );
}

#[test]
fn resolve_target_falls_back_and_explains_an_unsupported_protocol() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  // `http/json` is a real OTLP encoding, just not one this build carries, it
  // must not be quietly treated as protobuf.
  set("APERIO_OTEL_PROTOCOL", "http/json");
  set("APERIO_OTEL_ENDPOINT", "http://collector:4317");
  let target = resolve_target();
  assert_eq!(
    target.protocol,
    OtlpProtocol::Grpc,
    "an unreadable setting falls back to the port, not to the default"
  );
  assert!(
    target
      .note
      .as_deref()
      .unwrap_or_default()
      .contains("http/json"),
    "unexpected note: {:?}",
    target.note
  );
}

// --------------------------------------------------------------------------
// speaks_http2 (gRPC startup probe)
// --------------------------------------------------------------------------

/// Serves one connection: reads a little, then writes `reply`.
fn serve_once(reply: &'static [u8]) -> u16 {
  use std::io::{Read, Write};
  let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
  let port = listener.local_addr().expect("addr").port();
  std::thread::spawn(move || {
    if let Ok((mut sock, _)) = listener.accept() {
      let mut buf = [0u8; 64];
      let _ = sock.read(&mut buf);
      let _ = sock.write_all(reply);
    }
  });
  port
}

#[test]
fn speaks_http2_accepts_a_settings_frame() {
  // An empty SETTINGS frame, which is what a real HTTP/2 server answers with.
  let port = serve_once(&[0, 0, 0, 4, 0, 0, 0, 0, 0]);
  assert!(speaks_http2("127.0.0.1", port).expect("probe runs"));
}

#[test]
fn speaks_http2_rejects_an_http1_answer() {
  // What an OTLP/HTTP listener says when spoken to like this: HTTP/1, whose
  // first bytes are not a valid frame header.
  let port = serve_once(b"HTTP/1.1 400 Bad Request\r\n\r\n");
  assert!(!speaks_http2("127.0.0.1", port).expect("probe runs"));
}

#[test]
fn speaks_http2_errors_when_nothing_answers() {
  // Bind and drop, so the port is (almost certainly) closed.
  let port = {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("addr").port()
  };
  assert!(speaks_http2("127.0.0.1", port).is_err());
}

// --------------------------------------------------------------------------
// endpoint_host_port (startup probe target parsing)
// --------------------------------------------------------------------------

#[test]
fn endpoint_host_port_reads_explicit_port() {
  assert_eq!(
    endpoint_host_port("http://trace:4318/v1/traces"),
    Some(("trace".to_string(), 4318))
  );
}

#[test]
fn endpoint_host_port_defaults_by_scheme_when_no_port() {
  assert_eq!(
    endpoint_host_port("http://collector/v1/traces"),
    Some(("collector".to_string(), 80))
  );
  assert_eq!(
    endpoint_host_port("https://collector/v1/traces"),
    Some(("collector".to_string(), 443))
  );
}

#[test]
fn endpoint_host_port_handles_ipv6_literal() {
  assert_eq!(
    endpoint_host_port("http://[::1]:4318/v1/traces"),
    Some(("::1".to_string(), 4318))
  );
  assert_eq!(
    endpoint_host_port("https://[2606:4700::1]/v1/traces"),
    Some(("2606:4700::1".to_string(), 443))
  );
}

#[test]
fn endpoint_host_port_rejects_missing_authority() {
  assert_eq!(endpoint_host_port("not-a-url"), None);
  assert_eq!(endpoint_host_port("http:///v1/traces"), None);
}

// --------------------------------------------------------------------------
// resolve_service_name
// --------------------------------------------------------------------------

#[test]
fn resolve_service_name_defaults_when_unset() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  assert_eq!(resolve_service_name(), "aperio-server");
}

#[test]
fn resolve_service_name_prefers_aperio_var() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_SERVICE_NAME", "my-svc");
  set("OTEL_SERVICE_NAME", "other");
  assert_eq!(resolve_service_name(), "my-svc");
}

#[test]
fn resolve_service_name_falls_back_to_conventional_when_aperio_unset() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  // Aperio var unset -> conventional var wins; surrounding whitespace trimmed.
  set("OTEL_SERVICE_NAME", "  conv-svc  ");
  assert_eq!(resolve_service_name(), "conv-svc");
}

#[test]
fn resolve_service_name_blank_value_falls_through_to_default() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_SERVICE_NAME", "   ");
  assert_eq!(resolve_service_name(), "aperio-server");
}

// --------------------------------------------------------------------------
// build_provider
// --------------------------------------------------------------------------

#[test]
fn build_provider_succeeds_with_default_endpoint() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  let provider =
    build_provider(&resolve_target()).expect("provider should build without a live collector");
  // Shutting the freshly-built provider down should not error.
  let _ = provider.shutdown();
}

/// A syntactically invalid endpoint (embedded space) fails URI parsing, so the
/// exporter build errors and `build_provider` returns Err.
const BAD_ENDPOINT: &str = "http://exa mple:4318";

#[test]
fn build_provider_errors_on_unparseable_endpoint() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_ENDPOINT", BAD_ENDPOINT);
  let err = build_provider(&resolve_target()).expect_err("invalid endpoint must fail to build");
  assert!(
    err.contains("OTLP/HTTP span exporter build failed"),
    "unexpected error: {err}"
  );
}

// --------------------------------------------------------------------------
// HeaderExtractor
// --------------------------------------------------------------------------

#[test]
fn header_extractor_reads_present_and_absent_keys() {
  let mut headers = axum::http::HeaderMap::new();
  headers.insert("traceparent", axum::http::HeaderValue::from_static("abc"));
  headers.insert("x-other", axum::http::HeaderValue::from_static("v"));

  let ex = HeaderExtractor(&headers);
  assert_eq!(ex.get("traceparent"), Some("abc"));
  assert_eq!(ex.get("missing"), None);

  let mut keys = ex.keys();
  keys.sort();
  assert_eq!(keys, vec!["traceparent", "x-other"]);
}

#[test]
fn header_extractor_skips_non_ascii_values() {
  let mut headers = axum::http::HeaderMap::new();
  headers.insert(
    "traceparent",
    axum::http::HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
  );
  let ex = HeaderExtractor(&headers);
  // Non-UTF8 header value cannot be read as &str -> None.
  assert_eq!(ex.get("traceparent"), None);
}

// --------------------------------------------------------------------------
// HeaderInjector
// --------------------------------------------------------------------------

#[test]
fn header_injector_collects_key_value_pairs() {
  let mut inj = HeaderInjector(Vec::new());
  inj.set("traceparent", "00-abc-def-01".to_string());
  inj.set("tracestate", "vendor=1".to_string());
  assert_eq!(
    inj.0,
    vec![
      ("traceparent".to_string(), "00-abc-def-01".to_string()),
      ("tracestate".to_string(), "vendor=1".to_string()),
    ]
  );
}

// --------------------------------------------------------------------------
// request_span / trace_headers (real W3C propagation round-trip)
// --------------------------------------------------------------------------

#[test]
fn request_span_and_trace_headers_round_trip_with_propagator() {
  use opentelemetry::trace::TracerProvider as _;
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  // Install the real W3C propagator so extraction/injection actually run.
  global::set_text_map_propagator(TraceContextPropagator::new());

  // A scoped subscriber carrying the OTLP layer makes span<->OTel context
  // wiring deterministic regardless of whatever global subscriber is installed.
  let provider = build_provider(&resolve_target()).expect("provider builds");
  let otel_layer =
    tracing_opentelemetry::layer().with_tracer(provider.tracer("aperio-server-test"));
  let subscriber = tracing_subscriber::registry().with(otel_layer);

  let injected = tracing::subscriber::with_default(subscriber, || {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
      "traceparent",
      axum::http::HeaderValue::from_static(
        "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
      ),
    );
    let span = request_span(&headers, "GET", "/api/thing", Some("example.com"));
    let _guard = span.enter();
    // Inject the (adopted) context back out into forwardable headers.
    trace_headers(&span)
  });

  // The propagator must have produced a traceparent carrying the same trace id.
  let tp = injected
    .iter()
    .find(|(k, _)| k == "traceparent")
    .map(|(_, v)| v.clone())
    .expect("traceparent should be injected");
  assert!(
    tp.contains("0af7651916cd43dd8448eb211c80319c"),
    "trace id must be propagated through the span, got {tp}"
  );

  let _ = provider.shutdown();
}

#[test]
fn request_span_handles_missing_host_and_no_trace_context() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  global::set_text_map_propagator(TraceContextPropagator::new());
  // No traceparent header, no host -> both default branches.
  let headers = axum::http::HeaderMap::new();
  let span = request_span(&headers, "POST", "/", None);
  let _guard = span.enter();
  // Without an incoming/current context, nothing is injected.
  let injected = trace_headers(&span);
  assert!(injected.iter().all(|(k, _)| k != "traceparent") || injected.is_empty());
}

// --------------------------------------------------------------------------
// record_status
// --------------------------------------------------------------------------

#[test]
fn record_status_covers_ok_and_error_ranges() {
  let headers = axum::http::HeaderMap::new();
  let span = request_span(&headers, "GET", "/x", None);
  // Both the OK (<500) and ERROR (>=500) branches.
  record_status(&span, 200);
  record_status(&span, 503);
}

// --------------------------------------------------------------------------
// OtelGuard
// --------------------------------------------------------------------------

#[test]
fn otel_guard_shutdown_is_a_noop_when_disabled() {
  // Guard holding no provider shuts down cleanly.
  OtelGuard(None).shutdown();
}

#[test]
fn otel_guard_shutdown_flushes_a_provider() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  let provider = build_provider(&resolve_target()).expect("provider builds");
  OtelGuard(Some(provider)).shutdown();
}

// --------------------------------------------------------------------------
// init (all three branches, one process-global success + two panicking runs)
// --------------------------------------------------------------------------

#[test]
fn init_installs_subscriber_across_all_branches() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();

  // 1. Enabled + reachable-looking endpoint: build_provider() -> Ok, so the
  //    OTLP export layer is installed. This is the first init(), so its
  //    terminal .init() succeeds and sets the process-global subscriber.
  set("APERIO_OTEL", "1");
  set("APERIO_OTEL_ENDPOINT", "http://localhost:4318");
  let guard = init(EnvFilter::new("info"));

  // The global tracer provider must be the real one. `emit_phase_spans` builds
  // every phase child through `global::tracer`, and the default global provider
  // is a noop that drops them without a word, the symptom is a trace that
  // arrives with only its root `proxy.request` span and no breakdown at all.
  // A noop span carries the invalid (all-zero) span context; a real one does not.
  let probe_span = global::tracer("test").start("global-provider-probe");
  assert!(
    probe_span.span_context().is_valid(),
    "global tracer provider is still the noop: phase spans would be discarded"
  );
  drop(probe_span);

  guard.shutdown();

  // Silence the deliberate double-init panics below.
  let prev_hook = std::panic::take_hook();
  std::panic::set_hook(Box::new(|_| {}));

  // 2. Disabled branch: runs the fmt-only registry setup, then panics at the
  //    already-installed global .init(). catch_unwind lets the body's coverage
  //    counters settle.
  let disabled = std::panic::catch_unwind(|| {
    unsafe { std::env::remove_var("APERIO_OTEL") };
    let _ = init(EnvFilter::new("info"));
  });
  assert!(
    disabled.is_err(),
    "second init must panic on double-install"
  );

  // 3. Enabled-but-broken branch: force build_provider() to fail with an
  //    unparseable endpoint so init takes the Err arm, then panics at .init().
  let errored = std::panic::catch_unwind(|| {
    unsafe { std::env::set_var("APERIO_OTEL", "1") };
    unsafe { std::env::set_var("APERIO_OTEL_ENDPOINT", BAD_ENDPOINT) };
    let _ = init(EnvFilter::new("info"));
  });
  assert!(errored.is_err(), "third init must panic on double-install");

  std::panic::set_hook(prev_hook);
}

// ---------------------------------------------------------------------------
// probe_endpoint (does the collector actually speak OTLP/HTTP?)
// ---------------------------------------------------------------------------

/// A one-shot TCP listener that answers the given raw HTTP response, or hangs
/// up without answering at all when `response` is None (which is how a gRPC
/// port behaves when an HTTP request arrives).
fn one_shot_http(response: Option<&'static str>) -> u16 {
  use std::io::{Read, Write};
  let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
  let port = listener.local_addr().unwrap().port();
  std::thread::spawn(move || {
    if let Ok((mut sock, _)) = listener.accept() {
      let mut buf = [0u8; 1024];
      let _ = sock.read(&mut buf);
      if let Some(body) = response {
        let _ = sock.write_all(body.as_bytes());
      }
      // Dropping without writing is the "speaks something else" case.
    }
  });
  port
}

#[test]
fn probe_accepts_a_collector_that_answers_the_export() {
  let port = one_shot_http(Some("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"));
  // No assertion on the log line itself; what matters is that a collector
  // answering the POST is the success path and the probe returns promptly.
  probe_target(
    OtlpProtocol::Http,
    format!("http://127.0.0.1:{port}/v1/traces"),
  );
}

#[test]
fn probe_reports_a_port_that_accepts_tcp_but_does_not_speak_http() {
  // This is the case the old TCP-connect probe called "reachable": something
  // is listening, the connection succeeds, and no export will ever land.
  let port = one_shot_http(None);
  probe_target(
    OtlpProtocol::Http,
    format!("http://127.0.0.1:{port}/v1/traces"),
  );
}

#[test]
fn probe_does_not_panic_on_an_unusable_endpoint() {
  probe_target(
    OtlpProtocol::Http,
    "http://127.0.0.1:1/v1/traces".to_string(),
  );
  probe_target(OtlpProtocol::Http, "not a url".to_string());
  probe_target(OtlpProtocol::Grpc, "http://127.0.0.1:1".to_string());
  probe_target(OtlpProtocol::Grpc, "not a url".to_string());
  // TLS endpoints are reported, not probed.
  probe_target(
    OtlpProtocol::Grpc,
    "https://collector.invalid:4317".to_string(),
  );
}

#[test]
fn the_sample_rate_defaults_to_every_trace_and_refuses_nonsense() {
  let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
  let _env = EnvSnapshot::take();

  // Unset: what tracing did before the knob existed.
  assert_eq!(resolve_sample_rate(), 1.0);

  for (raw, expected) in [("0.01", 0.01), ("1", 1.0), ("0", 0.0), (" 0.5 ", 0.5)] {
    unsafe { std::env::set_var("APERIO_OTEL_SAMPLE_RATE", raw) };
    assert_eq!(resolve_sample_rate(), expected, "{raw}");
  }

  // Nonsense falls back to tracing everything rather than to tracing nothing:
  // a typo that silently turns off observability is the wrong way to fail.
  for raw in ["0.5%", "half", "-1", "2", "NaN", ""] {
    unsafe { std::env::set_var("APERIO_OTEL_SAMPLE_RATE", raw) };
    assert_eq!(resolve_sample_rate(), 1.0, "{raw}");
  }
}

// ---------------------------------------------------------------------------
// emit_phase_spans: the per-request trace breakdown.
// ---------------------------------------------------------------------------

/// Runs `f` inside a sampled `proxy.request` span wired to an in-memory
/// exporter, and returns every span exported when it ends. This is the whole
/// machinery `emit_phase_spans` needs to do anything at all: the enabled
/// flag, a global provider that samples, and a current span with a context.
fn with_recording_tracer(f: impl FnOnce()) -> Vec<opentelemetry_sdk::trace::SpanData> {
  use opentelemetry::trace::TracerProvider as _;
  use tracing_subscriber::layer::SubscriberExt;
  // ENV_LOCK, not the config lock: the global tracer provider is what needs
  // serializing here, and the init() test that also replaces it holds this
  // lock. Two different locks over one global is no lock at all, which is
  // exactly how these tests raced under a full parallel run.
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
  let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
    .with_simple_exporter(exporter.clone())
    .build();
  opentelemetry::global::set_tracer_provider(provider.clone());
  let was_enabled = OTEL_ENABLED.swap(true, std::sync::atomic::Ordering::Relaxed);
  let tracer = provider.tracer("aperio-server");
  let subscriber =
    tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
  tracing::subscriber::with_default(subscriber, || {
    let span = request_span(
      &axum::http::HeaderMap::new(),
      "GET",
      "/x",
      Some("app.example.com"),
    );
    let _entered = span.enter();
    f();
  });
  provider.force_flush().unwrap();
  OTEL_ENABLED.store(was_enabled, std::sync::atomic::Ordering::Relaxed);
  exporter.get_finished_spans().unwrap()
}

fn timeline_with(subphases: bool, client_offsets: bool) -> crate::state::RequestTimeline {
  crate::state::RequestTimeline {
    client_ready_us: subphases.then_some(10),
    admitted_us: subphases.then_some(20),
    selected_us: subphases.then_some(30),
    dispatched_us: 40,
    client_received_us: client_offsets.then_some(50),
    backend_sent_us: client_offsets.then_some(60),
    backend_first_byte_us: client_offsets.then_some(70),
    backend_done_us: client_offsets.then_some(80),
    client_responded_us: client_offsets.then_some(90),
    response_received_us: 100,
    finished_us: 110,
    estimated_anchor: client_offsets,
  }
}

#[test]
fn phase_spans_cover_the_full_breakdown_when_everything_was_measured() {
  let spans = with_recording_tracer(|| {
    emit_phase_spans(std::time::Instant::now(), &timeline_with(true, true));
  });
  let names: Vec<&str> = spans.iter().map(|s| s.name.as_ref()).collect();
  for expected in [
    "await client",
    "admission",
    "routing",
    "dispatch prep",
    "queue & routing",
    "tunnel → client",
    "client → backend",
    "backend (first byte)",
    "backend body",
    "client → tunnel",
    "tunnel → server",
    "tunnel round-trip",
    "server → visitor",
  ] {
    assert!(names.contains(&expected), "missing {expected}: {names:?}");
  }
  // The client/backend breakdown is an estimate and says so; the measured
  // server-side phases carry no such flag.
  let estimated = |name: &str| {
    spans
      .iter()
      .find(|s| s.name == name)
      .unwrap()
      .attributes
      .iter()
      .any(|kv| kv.key.as_str() == "aperio.estimated")
  };
  assert!(estimated("backend body"));
  assert!(!estimated("routing"));
}

#[test]
fn phase_spans_collapse_to_the_observed_boundaries_without_the_detail() {
  // No server sub-phases and no client offsets: the three real boundaries
  // are still emitted, nothing is invented.
  let spans = with_recording_tracer(|| {
    emit_phase_spans(std::time::Instant::now(), &timeline_with(false, false));
  });
  let names: Vec<&str> = spans.iter().map(|s| s.name.as_ref()).collect();
  for expected in ["queue & routing", "tunnel round-trip", "server → visitor"] {
    assert!(names.contains(&expected), "missing {expected}: {names:?}");
  }
  assert!(!names.contains(&"await client"), "{names:?}");
  assert!(!names.contains(&"backend body"), "{names:?}");
}

#[test]
fn phase_spans_are_free_when_otel_is_off_or_the_request_is_sampled_out() {
  // Off: the function returns before touching the timeline.
  emit_phase_spans(std::time::Instant::now(), &timeline_with(true, true));
  // On but with no sampled parent span: still nothing exported.
  let spans = with_recording_tracer(|| {});
  assert_eq!(spans.len(), 1, "only the request span itself: {spans:?}");
}

// ---------------------------------------------------------------------------
// build_provider and the startup probes.
// ---------------------------------------------------------------------------

#[test]
fn a_provider_builds_for_both_transports_and_both_sampling_modes() {
  // Building an exporter opens no connection, so both transports can be
  // proven constructible without a collector. The HTTP exporter's client
  // wants a runtime handle at hand, so one is entered, the way `init` runs
  // inside the server's own runtime.
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _g = EnvSnapshot::take();
  let rt = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(1)
    .enable_all()
    .build()
    .unwrap();
  let _enter = rt.enter();
  for (protocol, endpoint) in [
    (OtlpProtocol::Http, "http://127.0.0.1:4318/v1/traces"),
    (OtlpProtocol::Grpc, "http://127.0.0.1:4317"),
  ] {
    let target = OtlpTarget {
      protocol,
      endpoint: endpoint.to_string(),
      note: None,
    };
    let provider = build_provider(&target).expect(endpoint);
    // The Some branch of the shutdown guard, while a provider is at hand.
    OtelGuard(Some(provider)).shutdown();
  }
  // A fractional rate picks the ratio-based sampler branch.
  set("APERIO_OTEL_SAMPLE_RATE", "0.25");
  let target = OtlpTarget {
    protocol: OtlpProtocol::Http,
    endpoint: "http://127.0.0.1:4318/v1/traces".to_string(),
    note: None,
  };
  assert!(build_provider(&target).is_ok());
}

/// A local listener answering one canned HTTP response, for the probes.
fn canned_http_listener(
  response: &'static str,
) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
  let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
  let addr = listener.local_addr().unwrap();
  let handle = std::thread::spawn(move || {
    if let Ok((mut socket, _)) = listener.accept() {
      use std::io::{Read, Write};
      let mut buf = [0u8; 1024];
      let _ = socket.read(&mut buf);
      let _ = socket.write_all(response.as_bytes());
    }
  });
  (addr, handle)
}

#[test]
fn the_http_probe_accepts_a_200_and_warns_on_anything_else() {
  // A collector that accepts the empty export.
  let (addr, server) =
    canned_http_listener("HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
  probe_http(format!("http://{addr}/v1/traces"));
  server.join().unwrap();

  // One that answers, but not with success: the transport is proven right,
  // the warning is about everything else. Port 4317 adds the protocol hint.
  let (addr, server) = canned_http_listener(
    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
  );
  probe_http(format!("http://{addr}/v1/traces"));
  server.join().unwrap();

  // And one that is not there at all.
  probe_target(
    OtlpProtocol::Http,
    "http://127.0.0.1:9/v1/traces".to_string(),
  );
}
