# Production Hardening Checklist

A pre-flight checklist for taking an Aperio server to production. It is ordered
roughly by blast radius: the first items keep an attacker off the box, the
later ones limit the damage and make an incident visible. Nothing here is
exotic, every item maps to an existing setting, but going live with the
secure defaults in place is the difference between a tunnel and a liability.

> **Config surfaces.** Settings below are named by their `APERIO_*` environment variable; each also has an equivalent `aperio-server.yaml` key, the same name lowercased, without the `APERIO_` prefix (e.g. `APERIO_TRUST_PROXY` → `trust_proxy`, `APERIO_ADMIN_ALLOWED_IPS` → `admin_allowed_ips`). YAML is the primary surface, the file is loaded into the environment at startup and wins over it: put server keys in `aperio-server.yaml`, client keys in `aperio.yaml`. See [Configuration](configuration.md) for the full mapping.

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
- [ ] **Give each operator their own dashboard user, or OIDC**,
      and create per-person accounts with the least role that works
      (viewer/operator/admin) instead of sharing the master login.
- [ ] **Turn on a second factor** (TOTP or a passkey) for dashboard admins.
- [ ] **Set `APERIO_OIDC_REDIRECT_URL` if you use OIDC.** Without it the
      callback URL is derived from each request's `Host`, so the hostname a
      login starts on decides where the provider sends the authorization code
      back to. One fixed URL, the one registered with your provider.
- [ ] **Seed canary tokens.** Mint one or more decoy tokens flagged as canary
      and leave them where a leak would surface them (a stale config, a repo).
      Any authentication with one fires a `canary_tripped` alert, a
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
      The two are for different emergencies; see *Restoring, and which restore
      you want* below for which one to reach for.
- [ ] **Keep secret redaction on** (`APERIO_INSPECTOR_REDACT`, on by default) so
      the request inspector never shows credentials to a dashboard viewer.

### Restoring, and which restore you want

There are two, for two different emergencies, and picking the wrong one costs
time you do not have.

**A logical restore** (`GET /aperio/api/export` to `POST /aperio/api/import`)
runs against a server that is up. Each section present in the dump replaces
the corresponding store, it is master super-admin only, and it carries a
format version, so a dump taken by a different release is either applied or
refused rather than half-read. This is the one for undoing a bad change, and
the one that crosses a version boundary safely.

**A physical restore** puts a snapshot back as the database. This is the one
for getting a machine back, and it is offline by nature. The documented step
gets you a plaintext file:

```
aperio-server --decrypt-backup /backups/aperio-1755500000.db.enc /tmp/restored.db
```

What that step does not say is what comes next, and the next part has a trap
in it. The store runs in WAL mode, so a live data directory holds three files:

```
$APERIO_DATA_DIR/aperio.db
$APERIO_DATA_DIR/aperio.db-wal
$APERIO_DATA_DIR/aperio.db-shm
```

A snapshot is one consolidated file with no sidecars, because it was written
with `VACUUM INTO`. So dropping the restored database over `aperio.db` and
leaving the other two in place pairs a fresh main database with a stale
write-ahead log describing a database that no longer exists. Do it with the
server running and you are also racing its own writes.

The whole sequence:

1. **Stop the server.** Not a reload, a stop. The sequence below is not safe
   against a process that still holds the database open.
2. **Keep what is there.** Move the existing `aperio.db`, `aperio.db-wal` and
   `aperio.db-shm` aside rather than deleting them. If the snapshot turns out
   to be older than you thought, this is the only copy of what you had.
3. **Put the restored file in place** as `$APERIO_DATA_DIR/aperio.db`, and make
   sure **no `-wal` or `-shm` remains beside it**. This is the step that goes
   wrong.
4. **Match the ownership** the server runs as, before starting it. A restore
   performed as root leaves a database the service user cannot write, which
   surfaces later as a write failure rather than at startup.
5. **Start the server and check what came back**: the client count and token
   list on the dashboard, and `aperio-server --check-config` for a clean
   startup. A snapshot restores the state as of when it was taken, so tokens
   issued after that moment are gone and the clients holding them will be
   refused.

Rehearse this in staging before you need it, which is what the checklist item
below is asking for. The rehearsal that matters is the whole sequence, not the
decrypt: the decrypt either works or says why, while steps 2 to 4 are the ones
where a real restore goes wrong quietly.


## Observability & incident response

- [ ] **Point a webhook at the security events**, `canary_tripped`,
      `token_new_ip`, `alert_triggered`, `disk_usage_warning`, so they page
      someone. See [Observability](observability.md).
- [ ] **Enable threshold alerting** (`APERIO_ALERT_ERROR_RATE` /
      `APERIO_ALERT_CLIENT_DOWN`).
- [ ] **Fence outbound callbacks** where webhook creators are not fully
      trusted. `APERIO_OUTBOUND_ALLOWLIST` names the destinations the server
      may call for webhook deliveries and autoscaling hooks; failing that,
      `APERIO_OUTBOUND_BLOCK_PRIVATE=1` at least keeps them off the internal
      network. Both are off by default, and without one of them the delivery
      log lets a tenant probe your private network one port at a time. See
      [Threat Model](threat-model.md).
- [ ] **Ship the audit log** off-box and verify it periodically. The audit log
      is a tamper-evident hash chain; `aperio-server --verify-audit` (or
      `GET /aperio/api/audit/verify`) reports any broken line.
- [ ] **Scrape `/aperio/metrics`** with an authenticated token and alert on the
      request-duration and error trends.

## Before you flip the switch

- [ ] `aperio-server --check-config` is clean (no `FAIL`, warnings reviewed).
- [ ] `aperio-server --verify-audit` passes on a fresh install.
- [ ] A backup snapshot restores into a working server in a staging test,
      following the whole sequence in *Restoring, and which restore you want*,
      not just the decrypt step.
- [ ] The dashboard is reachable **only** from your operator network.
- [ ] **The startup log is clean.** The server checks its own capacity settings against the machine once at startup and warns when they do not fit: `max_ws_connections` plus `max_tunnels` against the process's file-descriptor ceiling, and the response cache budget against the container's memory limit. It never changes a setting, only names both numbers, because a value that silently follows the host is the opposite of being able to say which value is in effect. The first of these is the one worth reading: past the descriptor ceiling `accept` fails with `EMFILE` and connections break at a number nobody configured. Raise it with `ulimit -n`, or `LimitNOFILE=` in a systemd unit.

See the [Threat Model](threat-model.md) for the trust boundaries these controls
defend.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`behind_proxy`](examples/behind_proxy/): behind a reverse proxy / CDN
- [`oidc`](examples/oidc/): SSO login in front
- [`allowed_ips`](examples/allowed_ips/): per-service visitor IP allowlists
- [`traffic_rules`](examples/traffic_rules/): per-route rate limits, WAF-lite, fallbacks, per-hostname error pages
