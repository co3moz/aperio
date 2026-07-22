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
  "APERIO_OTEL_SERVICE_NAME",
  "OTEL_SERVICE_NAME",
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
  assert_eq!(resolve_endpoint(), "http://localhost:4318/v1/traces");
}

#[test]
fn resolve_endpoint_appends_signal_path_to_base_url() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_ENDPOINT", "http://collector:4318");
  assert_eq!(resolve_endpoint(), "http://collector:4318/v1/traces");
}

#[test]
fn resolve_endpoint_strips_trailing_slash_before_appending() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_ENDPOINT", "http://collector:4318/");
  assert_eq!(resolve_endpoint(), "http://collector:4318/v1/traces");
}

#[test]
fn resolve_endpoint_keeps_existing_signal_path() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_ENDPOINT", "http://collector:4318/v1/traces");
  assert_eq!(resolve_endpoint(), "http://collector:4318/v1/traces");
}

#[test]
fn resolve_endpoint_trims_and_falls_back_to_conventional_when_aperio_unset() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  // Aperio var unset -> conventional var wins; surrounding whitespace trimmed.
  set("OTEL_EXPORTER_OTLP_ENDPOINT", "  http://conv:4318  ");
  assert_eq!(resolve_endpoint(), "http://conv:4318/v1/traces");
}

#[test]
fn resolve_endpoint_blank_value_falls_through_to_default() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  // A present-but-blank value trims to empty -> filtered out -> default.
  set("APERIO_OTEL_ENDPOINT", "   ");
  assert_eq!(resolve_endpoint(), "http://localhost:4318/v1/traces");
}

#[test]
fn resolve_endpoint_prefers_aperio_var_over_conventional() {
  let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let _env = EnvSnapshot::take();
  set("APERIO_OTEL_ENDPOINT", "http://aperio:4318");
  set("OTEL_EXPORTER_OTLP_ENDPOINT", "http://conv:4318");
  assert_eq!(resolve_endpoint(), "http://aperio:4318/v1/traces");
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
  let provider = build_provider().expect("provider should build without a live collector");
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
  let err = build_provider().expect_err("invalid endpoint must fail to build");
  assert!(
    err.contains("OTLP span exporter build failed"),
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
  let provider = build_provider().expect("provider builds");
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
  let provider = build_provider().expect("provider builds");
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
