# The Dashboard

The admin dashboard lives at `/aperio` (login: `aperio` / master token, or a separate `APERIO_DASHBOARD_AUTH` password). It is a Vite + React app embedded into the server binary — no extra deployment.

## Live overview

Connected clients, a request-rate chart, lifetime average response time, and today's traffic — persisted across restarts. The whole live view is pushed over a single Server-Sent Events stream (`/aperio/api/stream`): `stats` events (the connections list and counters, every 2s) and `traffic` events (one per request) rather than polling. It falls back to polling only if the stream can't be established; the session-expiry check is the one thing still polled (once a minute).

## Clients table

Every connected client with its binds, health dot, last heartbeat, client version (with a warning badge on tunnel protocol mismatch), standby tier, announced concurrency limit, and a `BACKEND DOWN` badge when the client's own health probe is failing. Two controls act on live clients:

- **Enable/Disable kill switch** — a disabled client stays connected but receives no new traffic. Useful for taking a backend out of rotation without touching its machine.

Below the table, an **Uptime** panel tracks each service's availability history: current status (up / degraded / down), uptime percentages for today, the last 7 days, and the last 30 days, plus a per-day color strip. A background ticker (every `APERIO_UPTIME_TICK_SECS` seconds, default 10) accrues time as *up* (tunnel healthy and backend probe passing), *degraded* (connected but not serving — backend down, draining, or disabled), or *down* (no connection); history is persisted in `aperio.db` for 60 days. Percentages cover observed time only — time while the server itself was offline is not counted against a service. Also available as `GET /aperio/api/uptime`.
- **Overrule** — temporarily override a client's hostname/path binds, e.g. to redirect a hostname live. In-memory only; a reconnect or restart reverts it.

## Live traffic table

The traffic table is streamed live: the server pushes each proxied request over Server-Sent Events (`/aperio/api/stream`) as it completes, so rows appear the moment traffic flows instead of on a polling interval. If the stream can't be established (e.g. a proxy that buffers SSE) the table transparently falls back to periodic polling, and the **Live/Paused** toggle still freezes the view while you inspect. Latency percentiles (p50/p95/p99), a status-class mix bar, and method/status filters sit on top of the same feed.

## Request inspector & replay

Click any row in the traffic table to see full request/response headers and body previews (up to 64 KB per direction, last 50 requests) — then **replay** the request through the tunnel with one click while debugging a backend, copy it as an equivalent `curl` command, or download it as a devtools-importable HAR file.

**Secrets are masked before anything leaves the server**: credential headers (`Authorization`, `Cookie`/`Set-Cookie`, `X-Api-Key` and friends) and secret-looking body fields (`password`, `token`, `api_key`, `client_secret`, … in JSON or form bodies) show as `[REDACTED]` in the inspector — and therefore also in the cURL copy and the HAR download. The raw capture stays intact in server memory, so replay still re-sends the original bytes. Opt out with `APERIO_INSPECTOR_REDACT=0`.

## Add Client wizard

Pick a token strategy (placeholder, or mint a scoped token on the spot), describe the local service, and copy a ready-to-run `docker run` / CLI / `aperio.yaml` snippet.

## Active sessions

Admins see every live dashboard session on the Users page — who is signed in, from which IP and browser, since when; the caller's own session is marked. Any session can be ended individually (its cookie stops working on the next request), and **Sign out everywhere else** ends all sessions but your own. Both actions are audited (`session_revoked`, `sessions_cleared`). The session list and its controls are admin-only, `GET/DELETE /aperio/api/sessions[/{id}]`.

## Maintenance mode

Put a hostname (or `*` for everything) into maintenance: visitors get a 503 page (customizable via `APERIO_503_PAGE`, served with `Retry-After`) while tunnel clients stay connected. Like bind overrides it is in-memory and cleared on restart. Toggles are audited and emitted as `maintenance_on` / `maintenance_off` webhook events.

## Server settings

Almost every runtime setting — timeouts, limits, load-balancing strategy, failover, compression, random subdomains, visitor password, custom 503/504 HTML — can be edited live and takes effect immediately: changing the random-subdomain pattern re-issues connected clients' random hostnames on the spot, and enabling tunnel compression is offered to already-connected clients. Environment variables stay the defaults; edits become **persisted overrides** (`APERIO_DATA_DIR/settings.json`) that survive restarts and can be reset per field. The master token, `HOST`/`PORT`, proxy trust, and OIDC remain env-only. Changes are audited as `settings_updated`.

## Also here

- **API Tokens / Webhooks** — create, edit, revoke (see [Tokens & Authentication](tokens-and-auth.md), [Observability](observability.md)).
- **Share links** — generate signed visitor-access URLs (see [Share Links](share-links.md)).
- **Traffic breakdown** — top consumers per token and per hostname, plus a **traffic history** chart over the persisted statistics: last 7/30/60 days, 26 weeks, 24 months, or a custom date range, with successful/failed requests, transfer volume, and average latency per bucket (`GET /aperio/api/stats/history`).
- **Audit log** — the last 200 administrative/security events.

## The admin API

Everything the dashboard does goes through a REST API under `/aperio/api/`, and the whole surface is described by a generated OpenAPI 3.1 document at `GET /aperio/api/openapi.json` — point Swagger UI, Postman, or a client generator at it to script the server (mint tokens, read stats, toggle maintenance) with the same authentication as the dashboard. The endpoint list also lives in the [Configuration Reference](configuration.md).
