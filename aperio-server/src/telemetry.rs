//! OpenTelemetry (OTLP) trace export, opt-in via `APERIO_OTEL`.
//!
//! When enabled, every proxied request becomes a `proxy.request` span exported
//! over OTLP/HTTP (protobuf) to a collector. The incoming W3C `traceparent`
//! header (if present) is adopted as the span's parent, and the span's own
//! context is injected back into the headers forwarded through the tunnel, so a
//! visitor → aperio → backend request shows up as one distributed trace.
//!
//! Disabled by default: [`init`] then installs only the JSON stdout subscriber
//! and the propagation helpers become cheap no-ops (the global propagator stays
//! the default noop, so nothing is extracted or injected).

use opentelemetry::global;
use opentelemetry::propagation::{Extractor, Injector};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::field::Empty;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

/// Held for the lifetime of the process; flushes any buffered spans on
/// [`OtelGuard::shutdown`] (called during graceful shutdown).
pub(crate) struct OtelGuard(Option<SdkTracerProvider>);

impl OtelGuard {
  /// Flushes and shuts the exporter down. Safe to call when OTLP is disabled.
  pub(crate) fn shutdown(self) {
    if let Some(provider) = self.0
      && let Err(e) = provider.shutdown()
    {
      // Nothing to log to anymore during shutdown; surface on stderr.
      eprintln!("OpenTelemetry provider shutdown error: {e}");
    }
  }
}

/// True for `1`/`true` (case-insensitive) environment values.
fn env_flag(key: &str) -> bool {
  std::env::var(key)
    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    .unwrap_or(false)
}

/// Resolves the OTLP traces endpoint, appending the standard `/v1/traces`
/// signal path when only a base URL is given. Honors `APERIO_OTEL_ENDPOINT`
/// first, then the conventional `OTEL_EXPORTER_OTLP_ENDPOINT`.
fn resolve_endpoint() -> String {
  let raw = std::env::var("APERIO_OTEL_ENDPOINT")
    .ok()
    .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok())
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty())
    .unwrap_or_else(|| "http://localhost:4318".to_string());
  let trimmed = raw.trim_end_matches('/');
  if trimmed.ends_with("/v1/traces") {
    trimmed.to_string()
  } else {
    format!("{trimmed}/v1/traces")
  }
}

/// Service name reported on every span (`APERIO_OTEL_SERVICE_NAME`, then
/// `OTEL_SERVICE_NAME`, defaulting to `aperio-server`).
fn resolve_service_name() -> String {
  std::env::var("APERIO_OTEL_SERVICE_NAME")
    .ok()
    .or_else(|| std::env::var("OTEL_SERVICE_NAME").ok())
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty())
    .unwrap_or_else(|| "aperio-server".to_string())
}

/// Builds the OTLP/HTTP batch exporter and tracer provider.
fn build_provider() -> Result<SdkTracerProvider, String> {
  // The OTLP HTTP exporter builds a reqwest Client on the `rustls-no-provider`
  // stack, which requires a process-wide crypto provider to already be
  // installed. `main()` installs ring at startup, but guarantee it here too so
  // the exporter never depends on call ordering (and so unit tests that build a
  // provider directly work without a full server boot). Idempotent: a no-op
  // once a default is set.
  let _ = rustls::crypto::ring::default_provider().install_default();
  let endpoint = resolve_endpoint();
  let exporter = SpanExporter::builder()
    .with_http()
    .with_endpoint(&endpoint)
    .with_protocol(Protocol::HttpBinary)
    .build()
    .map_err(|e| format!("OTLP span exporter build failed: {e}"))?;
  let resource = Resource::builder()
    .with_service_name(resolve_service_name())
    .build();
  Ok(
    SdkTracerProvider::builder()
      .with_batch_exporter(exporter)
      .with_resource(resource)
      .build(),
  )
}

/// Installs the tracing subscriber: the JSON stdout layer always, plus the
/// OTLP export layer when `APERIO_OTEL` is enabled. Returns a guard that flushes
/// the exporter on shutdown.
pub(crate) fn init(log_filter: EnvFilter) -> OtelGuard {
  let fmt_layer = tracing_subscriber::fmt::layer()
    .json()
    .with_current_span(false)
    .with_span_list(false)
    .flatten_event(true);

  if !env_flag("APERIO_OTEL") {
    tracing_subscriber::registry()
      .with(log_filter)
      .with(fmt_layer)
      .init();
    return OtelGuard(None);
  }

  match build_provider() {
    Ok(provider) => {
      global::set_text_map_propagator(TraceContextPropagator::new());
      let tracer = provider.tracer("aperio-server");
      let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
      tracing_subscriber::registry()
        .with(log_filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();
      tracing::info!(
        "OpenTelemetry OTLP trace export enabled (endpoint: {})",
        resolve_endpoint()
      );
      OtelGuard(Some(provider))
    }
    Err(e) => {
      tracing_subscriber::registry()
        .with(log_filter)
        .with(fmt_layer)
        .init();
      tracing::error!("APERIO_OTEL is set but tracing export could not start: {e}");
      OtelGuard(None)
    }
  }
}

/// Reads W3C trace headers from an axum `HeaderMap`.
struct HeaderExtractor<'a>(&'a axum::http::HeaderMap);

impl Extractor for HeaderExtractor<'_> {
  fn get(&self, key: &str) -> Option<&str> {
    self.0.get(key).and_then(|v| v.to_str().ok())
  }
  fn keys(&self) -> Vec<&str> {
    self.0.keys().map(|k| k.as_str()).collect()
  }
}

/// Collects injected trace headers into a `(name, value)` list.
struct HeaderInjector(Vec<(String, String)>);

impl Injector for HeaderInjector {
  fn set(&mut self, key: &str, value: String) {
    self.0.push((key.to_string(), value));
  }
}

/// Creates the per-request server span, adopting any incoming W3C trace context
/// as its parent. When OTLP is disabled this is a plain (cheap) tracing span
/// with no external effect.
pub(crate) fn request_span(
  headers: &axum::http::HeaderMap,
  method: &str,
  path: &str,
  host: Option<&str>,
) -> tracing::Span {
  let span = tracing::info_span!(
    "proxy.request",
    otel.kind = "server",
    otel.name = Empty,
    otel.status_code = Empty,
    { "http.request.method" } = method,
    { "url.path" } = path,
    { "server.address" } = host.unwrap_or(""),
    { "http.response.status_code" } = Empty,
    { "aperio.client.id" } = Empty,
  );
  span.record("otel.name", format!("{method} {path}").as_str());
  let parent = global::get_text_map_propagator(|prop| prop.extract(&HeaderExtractor(headers)));
  let _ = span.set_parent(parent);
  span
}

/// Serializes the given span's trace context into headers to forward through
/// the tunnel (so the backend continues the trace). Empty when OTLP is off.
pub(crate) fn trace_headers(span: &tracing::Span) -> Vec<(String, String)> {
  let cx = span.context();
  let mut injector = HeaderInjector(Vec::new());
  global::get_text_map_propagator(|prop| prop.inject_context(&cx, &mut injector));
  injector.0
}

/// Records the final response status on the current request span.
pub(crate) fn record_status(span: &tracing::Span, status: u16) {
  span.record("http.response.status_code", status as i64);
  span.record(
    "otel.status_code",
    if status >= 500 { "ERROR" } else { "OK" },
  );
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
