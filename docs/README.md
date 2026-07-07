# Aperio Documentation

Short, focused articles on each part of the product. For the project overview and quick start, see the [main README](../README.md).

## Getting Started

- [Getting Started](getting-started.md) — expose your first local service in five minutes, with Docker or the CLI.
- [Configuration Reference](configuration.md) — every setting on both sides: the env/CLI/yaml naming standard, precedence layers, full tables, and the HTTP endpoint list.

## Core Features

- [Routing & Load Balancing](routing-and-load-balancing.md) — hostname/path binds, round-robin, primary-standby tiers, sticky sessions, random subdomains.
- [In-Flight Failover](failover.md) — what happens when a tunnel dies mid-request, and how to make it invisible to visitors.
- [Tokens & Authentication](tokens-and-auth.md) — the master token, scoped dynamic tokens, visitor passwords, and OIDC/SSO.
- [Share Links](share-links.md) — hand out temporary access to a protected site without creating accounts.
- [Ephemeral Tunnels](ephemeral-tunnels.md) — per-PR preview environments via the API and the GitHub Action.
- [Emergency Tunnels](emergency-tunnels.md) — declare unexposed TCP services and bind them from a peer client with `--bind-tunnels` when everything else is down.

## Operating Aperio

- [The Dashboard](dashboard.md) — live traffic, request inspector & replay, kill switch, maintenance mode, live server settings.
- [Observability](observability.md) — Prometheus metrics, structured access log, audit trail, webhooks, persistent statistics.
- [Client Resilience](client-resilience.md) — reconnect backoff, backend health probing, config hot-reload, graceful drain, bandwidth pacing.

## Under the Hood

- [Tunnel Protocol & Advanced Features](tunnel-protocol.md) — WebSocket pass-through, chunked body streaming, binary frames, compression.
- [Development & Releases](development.md) — building from source, tests & coverage, the release process, project conventions.
