# Tokens & Authentication

Aperio has several independent auth layers. They answer different questions — *who may open a tunnel*, *who may visit the proxied site*, and *who may administer the server* — and you enable only the ones you need.

## The master token

`APERIO_SERVER_TOKEN` is root: it authenticates tunnel clients, logs into the dashboard (user `aperio`), and authorizes the ephemeral-tunnel API. Treat it accordingly — front the server with TLS so it never travels in plaintext, and prefer dynamic tokens for day-to-day clients.

## Dynamic tokens

Minted from the dashboard's *API Tokens* section, each token is scoped and revocable:

- **Hostnames** — which hostname binds the token may claim (`*` = any). Specific entries are **auto-bound** on connect, so the client doesn't even need `--host`.
- **Paths** — which path binds it may claim.
- **Allowed IPs** — source IPs/CIDRs that may connect with this token.
- **Lifetime** — optional TTL; expired tokens are rejected at connect time. As an early-warning system, a `token_expiring` webhook/audit event fires once a token's remaining lifetime drops under `APERIO_TOKEN_EXPIRY_WARNING` (default 24 h), and the dashboard tokens table shows an "expiring soon" badge — refresh or re-issue before anything breaks.
- **Rate limit** — optional requests/second cap for the traffic served through this token; excess requests answer `429`.
- **Daily quota** — optional bytes/day cap (request + response payload), answering `429` once exhausted until local midnight (in-memory tracking; a restart resets the day's usage).

A client declaring a bind its token doesn't permit gets the declaration ignored (and logged). Tokens can be edited in place — scope, IPs, expiry change while the secret stays the same — or revoked, which immediately drops the tunnel connections using the token and rejects reconnects.

Secrets are stored as SHA-256 hashes in `APERIO_DATA_DIR/aperio.db` (SQLite) and shown exactly once at creation.

### Short-lived tokens & refresh

A token created with a lifetime can slide its own expiry forward — mint it with a short TTL and let the holder keep it alive only while it is actually in use:

```bash
curl -X POST -H "Authorization: Bearer $APERIO_TOKEN" https://tunnel.example.com/aperio/api/tokens/refresh
# → { "status": "ok", "id": "…", "expires_at": 1780000000 }
```

The endpoint authenticates with the token secret itself (no dashboard session needed), so a CI job or a long-running client can refresh on a timer. Each refresh resets the expiry to *now + the TTL the token was created with*. Never-expiring tokens are not refreshable, an expired token cannot resurrect itself, and each refresh writes a `token_refreshed` audit event.

## Protecting proxied traffic

Two options put a gate in front of everything the tunnel serves:

- **Visitor password** — `APERIO_SERVER_AUTH=user:password` shows a login form to every visitor.
- **OIDC / SSO** — redirect unauthenticated visitors to an identity provider (Google, Keycloak, Authentik, ...), Cloudflare-Access style:

  ```bash
  APERIO_OIDC_ISSUER=https://accounts.google.com
  APERIO_OIDC_CLIENT_ID=xxxx.apps.googleusercontent.com
  APERIO_OIDC_CLIENT_SECRET=xxxx
  APERIO_OIDC_ALLOWED_EMAILS=me@corp.com,*@team.example.com
  ```

  After login, the verified email (fetched from the issuer's `userinfo` endpoint over TLS) is checked against the allowlist — exact addresses, `*@domain`, or `*`. Sessions last 24 h. A misconfigured SSO setup is a **fatal error**: the server refuses to start rather than silently serving an unprotected proxy. Grants and denials are audit-logged.

A client can opt its own service out of the gate by declaring itself **public** (`--public`, yaml `public: true`, or `APERIO_PUBLIC=1`) — useful when one Aperio server fronts both protected internal tools and a public site. Two safety rules apply: the client's token must carry the *may publish public services* permission (off by default; master always may), and the gate is only skipped for routes served exclusively by public clients — if a protected and a public client share the same hostname, the gate stays.

### Client-set visitor password (per service)

Instead of opting out, a client can supply its **own** visitor login for its service — `--visitor-auth user:password`, env `APERIO_VISITOR_AUTH`, or per `services:` entry `auth: user:password`. The server then shows the normal login form for that service and accepts only these credentials, whether or not the server itself set `APERIO_SERVER_AUTH`:

- It reuses the same *may publish public services* token permission (master always may); a client without it has its `auth` ignored (and logged).
- When set, it **supersedes** the server's own visitor password *for that service*: the `APERIO_SERVER_AUTH` credentials no longer work there — only the client's credentials, plus the always-valid `aperio:<master token>` and the dashboard password.
- A successful login with client credentials yields a session **scoped to that hostname only** — it never unlocks the dashboard or another host. (If several path-bound services share one hostname with *different* `auth`, a login covers the whole hostname; give each its own hostname to isolate them. All clients serving one route must declare the same `auth`, mirroring the `public` rule.)
- The server operator can turn the whole feature off with **`APERIO_IGNORE_CLIENT_AUTH=1`**, which makes the server ignore every client-declared `auth` and keep sole control of the gate with its own `APERIO_SERVER_AUTH` / OIDC.

To let specific people through a protected site *without* an account, use [Share Links](share-links.md).

## Dashboard access

By default the dashboard password is the master token. Set `APERIO_DASHBOARD_AUTH` to give dashboard users a separate password without handing them root, or `APERIO_DASHBOARD=0` to disable the dashboard entirely. The Prometheus endpoint always requires its own token (`APERIO_METRICS_TOKEN`).

## Defense in depth

The client deliberately does not fully trust the server: it only connects to its configured HTTP/TCP targets (SSRF guards), caps tunnel message sizes, bounds decompression output, and enforces its own concurrency limit. All secret comparisons are constant-time; session cookies are `HttpOnly` + `SameSite=Lax`.
