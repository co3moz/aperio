# The Dashboard

The admin dashboard lives at `/aperio` (login: `aperio` / master token, or a separate `APERIO_DASHBOARD_AUTH` password). It is a Vite + React app embedded into the server binary — no extra deployment.

## Live overview

Connected clients, a request-rate chart, lifetime average response time, and today's traffic — persisted across restarts.

## Clients table

Every connected client with its binds, health dot, last heartbeat, client version (with a warning badge on tunnel protocol mismatch), standby tier, announced concurrency limit, and a `BACKEND DOWN` badge when the client's own health probe is failing. Two controls act on live clients:

- **Enable/Disable kill switch** — a disabled client stays connected but receives no new traffic. Useful for taking a backend out of rotation without touching its machine.
- **Overrule** — temporarily override a client's hostname/path binds, e.g. to redirect a hostname live. In-memory only; a reconnect or restart reverts it.

## Request inspector & replay

Click any row in the traffic table to see full request/response headers and body previews (up to 64 KB per direction, last 50 requests) — then **replay** the request through the tunnel with one click while debugging a backend.

## Add Client wizard

Pick a token strategy (placeholder, or mint a scoped token on the spot), describe the local service, and copy a ready-to-run `docker run` / CLI / `aperio.yaml` snippet.

## Maintenance mode

Put a hostname (or `*` for everything) into maintenance: visitors get a 503 page (customizable via `APERIO_503_PAGE`, served with `Retry-After`) while tunnel clients stay connected. Like bind overrides it is in-memory and cleared on restart. Toggles are audited and emitted as `maintenance_on` / `maintenance_off` webhook events.

## Server settings

Almost every runtime setting — timeouts, limits, load-balancing strategy, failover, compression, random subdomains, visitor password, custom 503/504 HTML — can be edited live. Environment variables stay the defaults; edits become **persisted overrides** (`APERIO_DATA_DIR/settings.json`) that survive restarts and can be reset per field. The master token, `HOST`/`PORT`, proxy trust, and OIDC remain env-only. Changes are audited as `settings_updated`.

## Also here

- **API Tokens / Webhooks** — create, edit, revoke (see [Tokens & Authentication](tokens-and-auth.md), [Observability](observability.md)).
- **Share links** — generate signed visitor-access URLs (see [Share Links](share-links.md)).
- **Traffic breakdown** — top consumers per token and per hostname.
- **Audit log** — the last 200 administrative/security events.
