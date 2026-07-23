# Production Hardening Checklist

A pre-flight checklist for taking an Aperio server to production. It is ordered
roughly by blast radius: the first items keep an attacker off the box, the
later ones limit the damage and make an incident visible. Nothing here is
exotic — every item maps to an existing setting — but going live with the
secure defaults in place is the difference between a tunnel and a liability.

> **Config surfaces.** Settings below are named by their `APERIO_*` environment variable; each also has an equivalent `aperio-server.yaml` key — the same name lowercased, without the `APERIO_` prefix (e.g. `APERIO_TRUST_PROXY` → `trust_proxy`, `APERIO_ADMIN_ALLOWED_IPS` → `admin_allowed_ips`). YAML is the primary surface. See [Configuration](configuration.md) for the full mapping.

Run `aperio-server --check-config` after wiring these up: it validates the
layered configuration (env + `aperio-server.yaml`) without binding a port.

## Transport & network

- [ ] **Terminate TLS in front of the server.** Aperio speaks plain HTTP; put
      it behind a TLS-terminating reverse proxy (or a CDN) and never expose the
      HTTP port directly to the internet.
- [ ] **Set `APERIO_TRUST_PROXY` / `APERIO_TRUSTED_PROXIES` correctly.** Only
      enable proxy trust when you actually run behind one, and prefer the
      CIDR-based `APERIO_TRUSTED_PROXIES` so client IPs are resolved by walking
      the `X-Forwarded-For` chain. Trusting the header without a proxy lets
      visitors spoof their IP and bypass rate limiting.
- [ ] **Enable `APERIO_SECURE_COOKIES`.** Session cookies then carry the
      `Secure` flag (defaults on when proxy trust is on).
- [ ] **Fence the admin surface with `APERIO_ADMIN_ALLOWED_IPS`.** Restrict the
      `/aperio` dashboard and `/aperio/api/*` endpoints to your operator
      network (office/VPN CIDRs). The login page and visitor-auth endpoints stay
      reachable so password-gated proxied sites keep working.

## Credentials & authentication

- [ ] **Use a long, random `APERIO_SERVER_TOKEN`.** This is the master
      credential; treat it like a root password. `--check-config` warns on
      tokens shorter than 16 characters.
- [ ] **Prefer scoped dynamic tokens over the master token** for clients, CI,
      and automation. Scope each to the hostnames/paths it needs, set a TTL, and
      add per-token rate limits and daily byte quotas where relevant. See
      [Tokens & Authentication](tokens-and-auth.md).
- [ ] **Give the dashboard its own password (`APERIO_DASHBOARD_AUTH`) or OIDC**,
      and create per-person accounts with the least role that works
      (viewer/operator/admin) instead of sharing the master login.
- [ ] **Turn on a second factor** (TOTP or a passkey) for dashboard admins.
- [ ] **Seed canary tokens.** Mint one or more decoy tokens flagged as canary
      and leave them where a leak would surface them (a stale config, a repo).
      Any authentication with one fires a `canary_tripped` alert — a
      high-signal breach indicator.

## Abuse & brute-force protection

- [ ] **Keep the login lockout enabled** (`APERIO_LOGIN_LOCKOUT_THRESHOLD` /
      `APERIO_LOGIN_LOCKOUT_SECS`); the defaults escalate per repeat offender.
- [ ] **Set per-IP rate limits** (`APERIO_IP_LIMIT_MAX` / `APERIO_IP_LIMIT_REFILL`)
      sized to your traffic, plus `APERIO_MAX_CONCURRENT_REQUESTS` and
      `APERIO_MAX_BODY_SIZE` so a single visitor cannot exhaust the server.

## Data lifecycle & durability

- [ ] **Configure retention** (`APERIO_RETENTION_*`) so captures, access logs,
      audit events, and stats do not grow without bound, and cap the store with
      `APERIO_DB_MAX_BYTES`.
- [ ] **Schedule physical backups** (`APERIO_BACKUP_INTERVAL` /
      `APERIO_BACKUP_DIR` / `APERIO_BACKUP_KEEP`) and store snapshots off-box.
      Complement them with periodic logical exports (`/aperio/api/export`).
- [ ] **Keep secret redaction on** (`APERIO_INSPECTOR_REDACT`, on by default) so
      the request inspector never shows credentials to a dashboard viewer.

## Observability & incident response

- [ ] **Point a webhook at the security events** — `canary_tripped`,
      `token_new_ip`, `alert_triggered`, `disk_usage_warning` — so they page
      someone. See [Observability](observability.md).
- [ ] **Enable threshold alerting** (`APERIO_ALERT_ERROR_RATE` /
      `APERIO_ALERT_CLIENT_DOWN`).
- [ ] **Ship the audit log** off-box and verify it periodically. The audit log
      is a tamper-evident hash chain; `aperio-server --verify-audit` (or
      `GET /aperio/api/audit/verify`) reports any broken line.
- [ ] **Scrape `/aperio/metrics`** with an authenticated token and alert on the
      request-duration and error trends.

## Before you flip the switch

- [ ] `aperio-server --check-config` is clean (no `FAIL`, warnings reviewed).
- [ ] `aperio-server --verify-audit` passes on a fresh install.
- [ ] A backup snapshot restores into a working server in a staging test.
- [ ] The dashboard is reachable **only** from your operator network.

See the [Threat Model](threat-model.md) for the trust boundaries these controls
defend.
