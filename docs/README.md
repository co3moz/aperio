# Aperio Documentation

Short, focused articles on each part of the product. For the project overview and quick start, see the [main README](../README.md).

## Getting Started

- [Getting Started](getting-started.md) — expose your first local service in five minutes, with Docker or the CLI.
- [Configuration Reference](configuration.md) — every setting on both sides: the env/CLI/yaml naming standard, precedence layers, full tables, and the HTTP endpoint list.
- [Configuration Examples](examples/README.md) — ready-to-adapt `aperio.yaml` + `aperio-server.yaml` pairs for common scenarios, from the minimal setup to load balancing, tunnels, and SSO.

## Core Features

- [Routing & Load Balancing](routing-and-load-balancing.md) — hostname/path binds, round-robin, primary-standby tiers, sticky sessions, random subdomains.
- [In-Flight Failover](failover.md) — what happens when a tunnel dies mid-request, and how to make it invisible to visitors.
- [Tokens & Authentication](tokens-and-auth.md) — the master token, scoped dynamic tokens with rate limits and quotas, visitor passwords, and OIDC/SSO.
- [Organizations (Multi-Tenancy)](organizations.md) — isolate one server into separate tenants: per-org clients, tokens, users, traffic, and stats, with a super-admin who can switch between them.
- [Share Links](share-links.md) — hand out temporary access to a protected site without creating accounts.
- [Ephemeral Tunnels](ephemeral-tunnels.md) — per-PR preview environments via the API and the GitHub Action.
- [Emergency Tunnels](emergency-tunnels.md) — declare unexposed TCP services and bind them from a peer client with `--bind-tunnels` when everything else is down.

## Operating Aperio

- [The Dashboard](dashboard.md) — live traffic, request inspector & replay, kill switch, maintenance mode, live server settings.
- [Observability](observability.md) — Prometheus metrics, OpenTelemetry tracing, structured access log, audit trail, webhooks, persistent statistics.
- [Client Resilience](client-resilience.md) — reconnect backoff, backend health probing, config hot-reload, graceful drain, bandwidth pacing.

## Under the Hood

- [Tunnel Protocol & Advanced Features](tunnel-protocol.md) — WebSocket pass-through, chunked body streaming, binary frames, compression, the response cache, custom error pages.
- [Development & Releases](development.md) — building from source, tests & coverage, the release process, project conventions.
