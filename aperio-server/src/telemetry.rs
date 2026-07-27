//! OpenTelemetry (OTLP) trace export, opt-in via `APERIO_OTEL`.
//!
//! When enabled, every proxied request becomes a `proxy.request` span exported
//! to a collector over OTLP — protobuf over HTTP by default, or gRPC when
//! `otel.protocol` says so or the endpoint sits on the gRPC port. The incoming
//! W3C `traceparent` header (if present) is adopted as the span's parent, and
//! the span's own context is injected back into the headers forwarded through
//! the tunnel, so a visitor → aperio → backend request shows up as one
//! distributed trace.
//!
//! Disabled by default: [`init`] then installs only the JSON stdout subscriber
//! and the propagation helpers become cheap no-ops (the global propagator stays
//! the default noop, so nothing is extracted or injected).

use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::propagation::{Extractor, Injector};
use opentelemetry::trace::{Span, SpanKind, TraceContextExt, Tracer, TracerProvider as _};
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

/// Set once at startup when OTLP export is installed. Lets hot-path code skip
/// the (otherwise no-op) child-span synthesis when tracing is off.
static OTEL_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True when OTLP trace export is active for this process.
pub(crate) fn otel_enabled() -> bool {
  OTEL_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// True for `1`/`true` (case-insensitive) environment values.
fn env_flag(key: &str) -> bool {
  std::env::var(key)
    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    .unwrap_or(false)
}

/// The OTLP transport carrying the spans. Both are the same protobuf payload;
/// they differ in framing, in the port collectors listen on, and in whether the
/// `/v1/traces` signal path belongs in the URL.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OtlpProtocol {
  /// Protobuf over HTTP, port 4318 by convention. The endpoint carries the
  /// `/v1/traces` path.
  Http,
  /// gRPC, port 4317 by convention. The endpoint is the bare base URL.
  Grpc,
}

impl OtlpProtocol {
  fn as_str(self) -> &'static str {
    match self {
      Self::Http => "http",
      Self::Grpc => "grpc",
    }
  }
}

/// Where and how spans are shipped, resolved once from the environment.
struct OtlpTarget {
  protocol: OtlpProtocol,
  endpoint: String,
  /// Why the protocol ended up as it did, when that is worth saying out loud:
  /// an unreadable setting, or a port that contradicts the chosen transport.
  /// Logged by `init` after the subscriber exists — resolution happens before
  /// it, so a `warn!` raised here would go nowhere.
  note: Option<String>,
}

/// The conventional OTLP ports. A collector answering the wrong protocol drops
/// every span without a word, so when nothing is configured the port is the
/// best evidence available about which transport the other side speaks.
const OTLP_GRPC_PORT: u16 = 4317;
const OTLP_HTTP_PORT: u16 = 4318;

/// Reads the explicitly configured transport: `APERIO_OTEL_PROTOCOL` first,
/// then the conventional `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` /
/// `OTEL_EXPORTER_OTLP_PROTOCOL`. Returns the parse failure as an
/// `Err(message)` so the caller can fall back *and* say why.
fn configured_protocol() -> Result<Option<OtlpProtocol>, String> {
  let Some(raw) = [
    "APERIO_OTEL_PROTOCOL",
    "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
    "OTEL_EXPORTER_OTLP_PROTOCOL",
  ]
  .iter()
  .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
  .map(|v| v.trim().to_ascii_lowercase()) else {
    return Ok(None);
  };
  match raw.as_str() {
    "grpc" => Ok(Some(OtlpProtocol::Grpc)),
    // `http/protobuf` is the spec spelling; `http` is the short one operators
    // reach for. `http/json` is a distinct encoding this build does not carry,
    // so it is a failure rather than a silent downgrade to protobuf.
    "http" | "http/protobuf" => Ok(Some(OtlpProtocol::Http)),
    other => Err(format!(
      "OTLP protocol \"{other}\" is not supported (use \"http\" or \"grpc\")"
    )),
  }
}

/// Resolves the endpoint and the transport together, because neither is
/// meaningful alone: the default endpoint depends on the protocol, and the
/// default protocol depends on the endpoint's port.
///
/// Honors `APERIO_OTEL_ENDPOINT` first, then the conventional
/// `OTEL_EXPORTER_OTLP_ENDPOINT`. With no protocol configured, port 4317 means
/// gRPC and anything else means HTTP.
fn resolve_target() -> OtlpTarget {
  let (explicit, mut note) = match configured_protocol() {
    Ok(protocol) => (protocol, None),
    Err(message) => (None, Some(format!("{message}; falling back to the port"))),
  };
  let raw = std::env::var("APERIO_OTEL_ENDPOINT")
    .ok()
    .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok())
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty());
  let port = raw
    .as_deref()
    .and_then(endpoint_host_port)
    .map(|(_, port)| port);
  let protocol = explicit.unwrap_or(match port {
    Some(OTLP_GRPC_PORT) => OtlpProtocol::Grpc,
    _ => OtlpProtocol::Http,
  });

  // An explicit protocol on the other transport's conventional port is the one
  // combination worth flagging: it is legal (collectors can listen anywhere)
  // but it is far more often a typo, and the symptom is silence.
  if explicit.is_some() {
    let contradicts = match protocol {
      OtlpProtocol::Http => port == Some(OTLP_GRPC_PORT),
      OtlpProtocol::Grpc => port == Some(OTLP_HTTP_PORT),
    };
    if contradicts && note.is_none() {
      note = Some(format!(
        "protocol is pinned to {} but the endpoint uses port {}, the conventional port of the other transport",
        protocol.as_str(),
        port.unwrap_or_default()
      ));
    }
  }

  let endpoint = match (raw, protocol) {
    (None, OtlpProtocol::Http) => format!("http://localhost:{OTLP_HTTP_PORT}/v1/traces"),
    (None, OtlpProtocol::Grpc) => format!("http://localhost:{OTLP_GRPC_PORT}"),
    (Some(raw), protocol) => {
      let trimmed = raw.trim_end_matches('/');
      match protocol {
        // The signal path belongs to the HTTP transport only.
        OtlpProtocol::Http if trimmed.ends_with("/v1/traces") => trimmed.to_string(),
        OtlpProtocol::Http => format!("{trimmed}/v1/traces"),
        // Tolerate a URL carried over from the HTTP transport rather than
        // sending gRPC to a path no collector routes.
        OtlpProtocol::Grpc => trimmed
          .strip_suffix("/v1/traces")
          .unwrap_or(trimmed)
          .to_string(),
      }
    }
  };
  OtlpTarget {
    protocol,
    endpoint,
    note,
  }
}

/// Extracts `(host, port)` from a resolved OTLP endpoint URL for the startup
/// reachability probe. Handles an explicit port, a scheme default (https ->
/// 443, else 80), and a bracketed IPv6 literal. Returns `None` when the URL has
/// no `scheme://` authority to probe.
fn endpoint_host_port(endpoint: &str) -> Option<(String, u16)> {
  let (scheme, rest) = endpoint.split_once("://")?;
  let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
  // Drop any userinfo (`user@host`), unusual for OTLP but cheap to handle.
  let authority = authority.rsplit('@').next().unwrap_or(authority);
  if authority.is_empty() {
    return None;
  }
  let default_port = if scheme.eq_ignore_ascii_case("https") {
    443
  } else {
    80
  };
  // Bracketed IPv6 literal, e.g. `[::1]:4318`.
  if let Some(inner) = authority.strip_prefix('[') {
    let (host, after) = inner.split_once(']')?;
    let port = after
      .strip_prefix(':')
      .and_then(|p| p.parse().ok())
      .unwrap_or(default_port);
    return Some((host.to_string(), port));
  }
  match authority.rsplit_once(':') {
    Some((host, port)) if !host.is_empty() => {
      Some((host.to_string(), port.parse().unwrap_or(default_port)))
    }
    _ => Some((authority.to_string(), default_port)),
  }
}

/// How long the startup probe waits for the collector, in total.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Best-effort startup check of the OTLP endpoint. Spans are exported
/// asynchronously and a broken endpoint (wrong host/port, wrong protocol, DNS,
/// collector down) is otherwise silent — every span is just dropped.
///
/// Each transport is probed the way it is actually spoken, because a check
/// that is merely *near* the real path is what produced the bug this replaced:
/// a plain TCP connect cannot tell a collector apart from anything else
/// listening on that port, so pointed at the gRPC port it reported "reachable"
/// while every real export failed.
///
/// Runs synchronously with blocking IO so it is independent of any Tokio
/// runtime (`init` may be called before/without one); callers run it on a
/// detached thread so startup never blocks.
fn probe_target(protocol: OtlpProtocol, endpoint: String) {
  match protocol {
    OtlpProtocol::Http => probe_http(endpoint),
    OtlpProtocol::Grpc => probe_grpc(endpoint),
  }
}

/// Probes the HTTP transport with a real OTLP export of *zero* spans: an empty
/// body is a valid, empty `ExportTraceServiceRequest`, so a working collector
/// accepts it and records nothing.
fn probe_http(endpoint: String) {
  let port = endpoint_host_port(&endpoint).map(|(_, port)| port);
  let hint = if port == Some(OTLP_GRPC_PORT) {
    concat!(
      " — port 4317 is the OTLP/gRPC port and this endpoint is being spoken to ",
      "over OTLP/HTTP; set `otel.protocol: grpc`, or use the HTTP port 4318"
    )
  } else {
    ""
  };

  let client = match reqwest_otlp::blocking::Client::builder()
    .timeout(PROBE_TIMEOUT)
    .build()
  {
    Ok(client) => client,
    Err(e) => {
      tracing::warn!("OTLP endpoint {} could not be probed ({})", endpoint, e);
      return;
    }
  };
  match client
    .post(&endpoint)
    .header("content-type", "application/x-protobuf")
    .body(Vec::new())
    .send()
  {
    Ok(res) if res.status().is_success() => {
      tracing::info!("OTLP endpoint {} accepted a test export", endpoint);
    }
    // An HTTP answer of any status proves the other side speaks HTTP, so the
    // transport is right and something else (path, auth, collector config) is
    // not. Worth a warning, but a different one.
    Ok(res) => tracing::warn!(
      "OTLP endpoint {} answered {} to a test export — traces may be dropped{}",
      endpoint,
      res.status(),
      hint
    ),
    Err(e) => tracing::warn!(
      "OTLP endpoint {} did not accept a test export ({}) — trace spans will be dropped{}",
      endpoint,
      e,
      hint
    ),
  }
}

/// Probes the gRPC transport by completing the HTTP/2 connection preface.
///
/// gRPC has no request that is safe to send blind and free of side effects the
/// way an empty OTLP/HTTP body is, so the probe stops one layer lower: it
/// checks that the endpoint speaks HTTP/2, which gRPC is defined on top of and
/// which an OTLP/HTTP or plain HTTP/1 listener does not. That is genuinely
/// weaker evidence than the HTTP probe's end-to-end export, and the log says
/// so rather than claiming an export succeeded.
///
/// TLS endpoints are left alone: verifying them means an ALPN handshake, and a
/// probe that quietly downgraded to a bare TCP connect would be the lie this
/// whole design exists to avoid.
fn probe_grpc(endpoint: String) {
  let Some((host, port)) = endpoint_host_port(&endpoint) else {
    tracing::warn!("OTLP endpoint {} could not be parsed to probe it", endpoint);
    return;
  };
  if endpoint
    .split("://")
    .next()
    .unwrap_or("")
    .eq_ignore_ascii_case("https")
  {
    tracing::info!(
      "OpenTelemetry OTLP/gRPC export targets {} (TLS endpoint, not probed at startup)",
      endpoint
    );
    return;
  }
  let hint = if port == OTLP_HTTP_PORT {
    concat!(
      " — port 4318 is the OTLP/HTTP port and this endpoint is being spoken to ",
      "over OTLP/gRPC; set `otel.protocol: http`, or use the gRPC port 4317"
    )
  } else {
    ""
  };
  match speaks_http2(&host, port) {
    Ok(true) => tracing::info!(
      "OTLP endpoint {} completed an HTTP/2 handshake, the transport OTLP/gRPC runs on",
      endpoint
    ),
    Ok(false) => tracing::warn!(
      "OTLP endpoint {} answered without HTTP/2 — trace spans will be dropped{}",
      endpoint,
      hint
    ),
    Err(e) => tracing::warn!(
      "OTLP endpoint {} did not complete an HTTP/2 handshake ({}) — trace spans will be dropped{}",
      endpoint,
      e,
      hint
    ),
  }
}

/// Opens the HTTP/2 connection preface against a cleartext endpoint and waits
/// for the peer's mandatory SETTINGS frame. `Ok(false)` means the peer answered
/// with something that is not an HTTP/2 frame.
fn speaks_http2(host: &str, port: u16) -> std::io::Result<bool> {
  use std::io::{Read, Write};
  use std::net::{TcpStream, ToSocketAddrs};

  /// RFC 9113 §3.4 client connection preface.
  const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
  /// An empty SETTINGS frame: 3-byte length, type 0x4, no flags, stream 0.
  const EMPTY_SETTINGS: [u8; 9] = [0, 0, 0, 4, 0, 0, 0, 0, 0];
  /// Frame type of SETTINGS, at byte 3 of a frame header.
  const FRAME_TYPE_SETTINGS: u8 = 4;

  let addr = (host, port)
    .to_socket_addrs()?
    .next()
    .ok_or_else(|| std::io::Error::other(format!("no address for {host}:{port}")))?;
  let mut stream = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT)?;
  stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
  stream.set_write_timeout(Some(PROBE_TIMEOUT))?;
  stream.write_all(PREFACE)?;
  stream.write_all(&EMPTY_SETTINGS)?;
  stream.flush()?;
  // A server must answer the preface with its own SETTINGS frame first.
  let mut header = [0u8; 9];
  stream.read_exact(&mut header)?;
  Ok(header[3] == FRAME_TYPE_SETTINGS)
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

/// Builds the batch exporter for the resolved transport and the tracer
/// provider around it.
///
/// Must be called from within a Tokio runtime when the transport is gRPC:
/// tonic's channel spawns the task that drives the connection onto the runtime
/// that is current at build time, and the batch processor's own thread then
/// only waits on it. `init` runs inside the server runtime, so this holds.
fn build_provider(target: &OtlpTarget) -> Result<SdkTracerProvider, String> {
  // Both exporters build a rustls client on a `no-provider` stack, which
  // requires a process-wide crypto provider to already be installed. `main()`
  // installs ring at startup, but guarantee it here too so the exporter never
  // depends on call ordering (and so unit tests that build a provider directly
  // work without a full server boot). Idempotent: a no-op once a default is set.
  let _ = rustls::crypto::ring::default_provider().install_default();
  let resource = Resource::builder()
    .with_service_name(resolve_service_name())
    .build();
  let builder = SdkTracerProvider::builder().with_resource(resource);
  // The two exporters are distinct types and `SpanExporter` is not dyn-safe
  // (its `export` returns an opaque future), so the branch has to wrap the
  // whole `with_batch_exporter` call rather than just the exporter.
  let builder = match target.protocol {
    OtlpProtocol::Http => builder.with_batch_exporter(
      SpanExporter::builder()
        .with_http()
        .with_endpoint(&target.endpoint)
        .with_protocol(Protocol::HttpBinary)
        .build()
        .map_err(|e| format!("OTLP/HTTP span exporter build failed: {e}"))?,
    ),
    OtlpProtocol::Grpc => builder.with_batch_exporter(
      SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&target.endpoint)
        .build()
        .map_err(|e| format!("OTLP/gRPC span exporter build failed: {e}"))?,
    ),
  };
  Ok(builder.build())
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

  let target = resolve_target();
  match build_provider(&target) {
    Ok(provider) => {
      global::set_text_map_propagator(TraceContextPropagator::new());
      // The tracing layer holds its own tracer, but `emit_phase_spans` builds
      // the per-request phase children through `global::tracer`, and the global
      // provider defaults to a noop that silently discards everything handed to
      // it. Without this line a trace arrives at the collector as a single
      // `proxy.request` span with none of its breakdown, and nothing anywhere
      // reports an error.
      global::set_tracer_provider(provider.clone());
      let tracer = provider.tracer("aperio-server");
      let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
      tracing_subscriber::registry()
        .with(log_filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();
      tracing::info!(
        "OpenTelemetry OTLP trace export enabled (protocol: {}, endpoint: {})",
        target.protocol.as_str(),
        target.endpoint
      );
      // Resolution happens before the subscriber exists, so anything odd about
      // it is reported here instead.
      if let Some(note) = &target.note {
        tracing::warn!("OTLP configuration: {}", note);
      }
      OTEL_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
      // Surface an unreachable collector immediately instead of silently
      // dropping every span. Detached thread with blocking IO — advisory only,
      // never blocks startup and needs no Tokio runtime (export still runs).
      let (protocol, endpoint) = (target.protocol, target.endpoint.clone());
      std::thread::spawn(move || probe_target(protocol, endpoint));
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

/// Emits the request's flow as child spans under the current `proxy.request`
/// span, so a trace shows what actually happened to the request. Three
/// top-level children mirror the real path, each with **measured** server-clock
/// timestamps:
///
/// ```text
/// proxy.request
/// ├─ queue & routing        arrival → dispatched      (server-side, before the request leaves)
/// │  ├─ await client        arrival → client ready    (wait for a connected client)
/// │  ├─ admission           → admitted                (server-wide concurrency slot)
/// │  ├─ routing             → selected                (WAF, route limit, client pick)
/// │  └─ dispatch prep       → dispatched              (token check, header/trace build, send)
/// ├─ tunnel round-trip      dispatched → response     (out over the tunnel to the client, and back)
/// │  ├─ tunnel → client     ┐
/// │  ├─ client → backend    │  buffered path only; nested detail of what the
/// │  ├─ backend (first byte)│  client/backend did. These are split-transit
/// │  ├─ backend body        │  ESTIMATES (client & server clocks never mixed),
/// │  ├─ client → tunnel     │  flagged `aperio.estimated = true`.
/// │  └─ tunnel → server     ┘
/// └─ server → visitor       response → finished       (response streamed back)
/// ```
///
/// The three top-level spans are real, observed boundaries; only the nested
/// client/backend breakdown is estimated (and present only when the client
/// reported its offsets — the buffered response path). `t0`'s wall clock is
/// recovered from the monotonic `start_time` so the children line up under the
/// parent. No-op unless OTLP export is enabled.
pub(crate) fn emit_phase_spans(
  start_time: std::time::Instant,
  timeline: &crate::state::RequestTimeline,
) {
  if !otel_enabled() {
    return;
  }
  use std::time::{Duration, SystemTime};
  let parent_cx = tracing::Span::current().context();
  let t0 = SystemTime::now()
    .checked_sub(start_time.elapsed())
    .unwrap_or_else(SystemTime::now);
  let tracer = global::tracer("aperio-server");
  let at = |us: u64| t0 + Duration::from_micros(us);

  // A leaf child span [from, to] under `cx`. `to` is clamped so a span can
  // never end before it starts (estimation/rounding).
  let leaf =
    |name: &'static str, from: u64, to: u64, cx: &opentelemetry::Context, estimated: bool| {
      let mut builder = tracer
        .span_builder(name)
        .with_kind(SpanKind::Internal)
        .with_start_time(at(from));
      if estimated {
        builder = builder.with_attributes(vec![KeyValue::new("aperio.estimated", true)]);
      }
      let mut span = tracer.build_with_context(builder, cx);
      span.end_with_timestamp(at(to.max(from)));
    };

  // 1. Real: the server received the request and processed it up to dispatch.
  //    Nest the measured server-side sub-phases when they were captured.
  if let (Some(ready), Some(admitted), Some(selected)) = (
    timeline.client_ready_us,
    timeline.admitted_us,
    timeline.selected_us,
  ) {
    let qb = tracer
      .span_builder("queue & routing")
      .with_kind(SpanKind::Internal)
      .with_start_time(at(0));
    let qspan = tracer.build_with_context(qb, &parent_cx);
    let qcx = parent_cx.with_span(qspan);
    leaf("await client", 0, ready, &qcx, false);
    leaf("admission", ready, admitted, &qcx, false);
    leaf("routing", admitted, selected, &qcx, false);
    leaf(
      "dispatch prep",
      selected,
      timeline.dispatched_us,
      &qcx,
      false,
    );
    qcx.span().end_with_timestamp(at(timeline.dispatched_us));
  } else {
    leaf(
      "queue & routing",
      0,
      timeline.dispatched_us,
      &parent_cx,
      false,
    );
  }

  // 2. Real: the request went out over the tunnel to the client and the server
  //    waited for the response. The estimated client/backend breakdown (only on
  //    the buffered path, where the client reports its offsets) nests under it.
  let tunnel_builder = tracer
    .span_builder("tunnel round-trip")
    .with_kind(SpanKind::Internal)
    .with_start_time(at(timeline.dispatched_us));
  let tunnel_span = tracer.build_with_context(tunnel_builder, &parent_cx);
  let tunnel_cx = parent_cx.with_span(tunnel_span);
  if let (Some(cr), Some(bs), Some(bf), Some(bd), Some(crd)) = (
    timeline.client_received_us,
    timeline.backend_sent_us,
    timeline.backend_first_byte_us,
    timeline.backend_done_us,
    timeline.client_responded_us,
  ) {
    leaf(
      "tunnel → client",
      timeline.dispatched_us,
      cr,
      &tunnel_cx,
      true,
    );
    leaf("client → backend", cr, bs, &tunnel_cx, true);
    leaf("backend (first byte)", bs, bf, &tunnel_cx, true);
    leaf("backend body", bf, bd, &tunnel_cx, true);
    leaf("client → tunnel", bd, crd, &tunnel_cx, true);
    leaf(
      "tunnel → server",
      crd,
      timeline.response_received_us,
      &tunnel_cx,
      true,
    );
  }
  tunnel_cx
    .span()
    .end_with_timestamp(at(timeline.response_received_us));

  // 3. Real: the response was served back to the visitor.
  leaf(
    "server → visitor",
    timeline.response_received_us,
    timeline.finished_us,
    &parent_cx,
    false,
  );
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
