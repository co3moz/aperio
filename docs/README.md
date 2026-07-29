# Aperio Documentation

Short, focused articles on each part of the product. For the project overview and quick start, see the [main README](../README.md).

Prefer one long read? [**The Complete Guide**](book/aperio.tex) is a single-file LaTeX book covering everything below in one narrative, plus generated reference tables for every setting, endpoint and protocol message. Build it with `pdflatex aperio.tex` (twice, so the table of contents resolves).

## Getting Started

- [Getting Started](getting-started.md), expose your first local service in five minutes, with Docker or the CLI.
- [Configuration Reference](configuration.md), every setting on both sides: the env/CLI/yaml naming standard, precedence layers, full tables, and the HTTP endpoint list.
- [Configuration Examples](examples/README.md), ready-to-adapt `aperio.yaml` + `aperio-server.yaml` pairs for common scenarios, from the minimal setup to load balancing, tunnels, and SSO.

## Core Features

- [Routing & Load Balancing](routing-and-load-balancing.md), hostname/path binds, round-robin, primary-standby tiers, sticky sessions, random subdomains.
- [In-Flight Failover](failover.md), what happens when a tunnel dies mid-request, and how to make it invisible to visitors.
- [Tokens & Authentication](tokens-and-auth.md), the master token, scoped dynamic tokens with rate limits and quotas, visitor passwords, and OIDC/SSO.
- [Organizations (Multi-Tenancy)](organizations.md), isolate one server into separate tenants: per-org clients, tokens, users, traffic, and stats, with a super-admin who can switch between them.
- [Share Links](share-links.md), hand out temporary access to a protected site without creating accounts.
- [Static File Serving](static-serving.md), publish a directory with no backend at all: SPA fallback, custom 404, streamed files, `Range` requests, and the path-safety rules.
- [Ephemeral Tunnels](ephemeral-tunnels.md), per-PR preview environments via the API and the GitHub Action.
- [Tunnels](emergency-tunnels.md), declare unexposed TCP/UDP services, bind them by name from another client with `--bind-tunnels`, or open one on a public port with `expose:`.
- [Messages](messaging.md), publish a topic and every client of the organization that subscribed to it hears about it, over the tunnel they already hold. The server's own events are on `$aperio/`.

## Operating Aperio

- [The Dashboard](dashboard.md), live traffic, request inspector & replay, kill switch, maintenance mode, live server settings.
- [Admin API from the CLI](cli-api.md), run the dashboard's operations from scripts with `aperio-client api ...`: share links, tokens, tunnels, maintenance, users, reports.
- [Observability](observability.md), Prometheus metrics, OpenTelemetry tracing, structured access log, audit trail, webhooks, persistent statistics.
- [Autoscaling](autoscaling.md), let the server ask your provider for capacity: cold start from zero on the first request, scale out when the pool saturates, and clients that retire themselves when idle.
- [Client Resilience](client-resilience.md), reconnect backoff, backend health probing, config hot-reload, graceful drain, bandwidth pacing.
- [Behind a Dynamic Edge Proxy](edge-proxy.md), run Traefik or Caddy in front and let them learn Aperio's runtime hostnames: wildcard routing, Caddy on-demand TLS, and the Traefik HTTP provider.
- [Response Caching](caching.md), the server-side GET cache: the two-key opt-in, what gets cached, edge 304 / single-flight / stale-while-revalidate / range hits, and serve-stale resilience.

## Security

- [Production Hardening Checklist](production-hardening.md), a pre-flight checklist for going live: TLS, token hygiene, admin IP fencing, lockout, retention, backups, alerting, and fencing outbound callbacks.
- [Threat Model](threat-model.md), the trust boundaries (visitor ↔ server ↔ client ↔ backend), what each side is trusted to do, and the controls that defend each boundary.

## Under the Hood

- [Architecture Deep-Dive](architecture.md), the tunnel protocol, the request lifecycle, the concurrency model, and where state lives.
- [Tunnel Protocol & Advanced Features](tunnel-protocol.md), WebSocket pass-through, chunked body streaming, binary frames, per-stream flow control, compression, the response cache, custom error pages.
- [Performance Tuning](performance-tuning.md), the throughput/latency knobs and their trade-offs: parallelism, limits, caching, compression, failover.
- [Upgrade Guide & Compatibility](upgrade-guide.md), safe upgrades and what client ↔ server version skew means.
- [Development & Releases](development.md), building from source, tests & coverage, benchmarks & fuzzing, the release process, project conventions.
