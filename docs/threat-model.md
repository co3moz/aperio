# Threat Model

Aperio sits between the public internet and a service you did not want to
expose directly. This document names the trust boundaries in that path, states
what each side is trusted to do, and lists the controls that defend each
boundary. It is deliberately explicit about the project's guiding assumption:
**the client does not trust the server, and the server does not trust the
visitor.**

## The actors

```
   visitor  ──HTTP──▶  server  ◀──outbound WS──  client  ──local──▶  backend
  (untrusted)        (semi-trusted)            (trusted)          (trusted)
```

- **Visitor**, anyone on the internet sending requests to a published
  hostname. Fully untrusted.
- **Server**, the Aperio server: the public front door, router, and admin
  surface. Trusted to route and to enforce policy, but treated by the client as
  a potentially-hostile relay (it never receives the client's local
  credentials, only proxied traffic).
- **Client**, the `aperio-client` process running next to a service. Trusted;
  it dials **outbound** to the server, so nothing on the client's side accepts
  inbound connections.
- **Backend**, the local service the client forwards to. Trusted; reached only
  over the loopback/private address the client was pointed at.

## Trust boundaries

### 1. Visitor → Server (the public edge)

The primary attack surface. A visitor is assumed to be malicious: probing for
open hostnames, spoofing headers, flooding requests, and attacking the login.

Controls:

- Per-IP token-bucket rate limiting, a global concurrency cap, and a request
  body-size limit.
- Client-IP resolution that does **not** trust `X-Forwarded-For` unless a proxy
  is configured (`APERIO_TRUSTED_PROXIES` (yaml `trusted_proxies`)), so a visitor cannot spoof its IP to
  dodge rate limits.
- Visitor authentication (server password / share links / OIDC) in front of
  protected services; unauthenticated hostnames answer only what routing
  allows.
- Escalating login lockout against brute force.

### 2. Visitor / anyone → Admin surface

The `/aperio` dashboard and `/aperio/api/*` endpoints are the highest-value
target: they manage tokens, users, and settings.

Controls:

- Session authentication with role-based access (viewer / operator / admin) and
  optional TOTP / passkey second factor.
- Optional network fence: `APERIO_ADMIN_ALLOWED_IPS` (yaml `admin_allowed_ips`) restricts the dashboard and
  its API to operator CIDRs, answering `403` otherwise, while leaving the login
  page and visitor-auth endpoints reachable so password-gated proxied services
  keep working.
- The metrics endpoint always requires a token.

### 3. Server → Client (the tunnel)

The client treats the server as an untrusted relay. A compromised or malicious
server should not be able to reach the client's backend beyond the request path
the client already agreed to serve, nor replay a leaked token from elsewhere.

Controls:

- The client dials outbound; the server can never initiate a connection to the
  client or its network.
- Token scoping: a dynamic token only binds the hostnames/paths it was granted,
  with optional source-IP allowlists, TTLs, rate limits, and quotas.
- Leak detection: `token_new_ip` fires when a token connects from an unseen
  address, and canary tokens fire `canary_tripped` on any use, so a token
  lifted from a CI log or a dump surfaces quickly.

### 4. Client → Backend

The narrowest boundary. The client forwards proxied requests to exactly the
local address it was configured with; the backend is assumed trusted and is
never exposed to the internet directly.

## What Aperio does *not* defend against

- A compromised **backend** or **client host**: if the machine running the
  client is owned, the attacker already has what the tunnel would reach.
- The **master token** leaking: it is unrestricted by design. Rotate it, keep it
  out of clients (use scoped tokens), and fence the admin surface.
- **Application-layer bugs in the backend**: Aperio proxies requests; it is not
  a substitute for securing the service behind it.
- **Denial of service at the network layer**: absorb volumetric floods at the
  CDN/reverse-proxy tier in front of Aperio.

## Tamper-evidence

The audit log is an append-only, hash-chained record: each line commits to the
previous one, so a deleted or altered event breaks the chain. Verify it with
`aperio-server --verify-audit` or `GET /aperio/api/audit/verify`, and ship the
log off-box so an attacker who reaches the server cannot quietly rewrite history.

See the [Production Hardening Checklist](production-hardening.md) to put these
controls in place.
