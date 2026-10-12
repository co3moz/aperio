# Deployment

Installing the server and its clients, putting them into production, and upgrading them.

## Installing

**Containers.** `ghcr.io/co3moz/aperio-server` and `ghcr.io/co3moz/aperio-client`, as in [Getting Started](getting-started.md). Mount a volume at the server's data directory (`/app/data` in the image).

**Packages.** Every release attaches `.deb` and `.rpm` packages for both binaries on `amd64` and `arm64`, a Homebrew formula and a Scoop manifest:

```bash
sudo dpkg -i aperio-server_<version>_amd64.deb      # or: sudo rpm -i aperio-server-<version>-1.x86_64.rpm
brew install --formula https://github.com/co3moz/aperio/releases/latest/download/aperio-client.rb
scoop install https://github.com/co3moz/aperio/releases/latest/download/aperio-client.json
```

| Path | What it is |
| --- | --- |
| `/usr/bin/aperio-server`, `/usr/bin/aperio-client` | the binaries |
| `/usr/lib/systemd/system/aperio-server.service` | the server unit |
| `/usr/lib/systemd/system/aperio-client@.service` | the client unit, one instance per config file |
| `/etc/aperio/aperio-server.env` | the server's boot settings, `0640 root:aperio` |
| `/etc/aperio/server-init.yaml` | the settings a new server starts with |
| `/etc/aperio/aperio-client.yaml.example` | the template a client instance is made from |
| `/var/lib/aperio`, `/var/lib/aperio-client/<instance>` | the server's and each client's state |

Installing starts nothing, because the shipped environment file holds a placeholder token:

```bash
sudoedit /etc/aperio/aperio-server.env          # set APERIO_SERVER_TOKEN
sudo systemctl enable --now aperio-server

sudo install -o root -g aperio -m 0640 /etc/aperio/aperio-client.yaml.example /etc/aperio/myapp.yaml
sudoedit /etc/aperio/myapp.yaml
sudo systemctl enable --now aperio-client@myapp
```

Both units run as the `aperio` account under a strict sandbox (`ProtectSystem=strict`, `NoNewPrivileges`, a system-call filter and the rest). Moving the data directory therefore takes an override:

```ini
# sudo systemctl edit aperio-server
[Service]
Environment=APERIO_DATA_DIR=/srv/aperio
ReadWritePaths=/srv/aperio
```

An upgrade never overwrites the files under `/etc/aperio`, and restarts only what was running.

**Kubernetes.** The Helm chart in [`tools/charts/aperio-server`](../tools/charts/aperio-server/) runs the server as a single-replica StatefulSet, since the SQLite store takes one writer:

```bash
kubectl create secret generic aperio-token --from-literal=token="$(openssl rand -hex 32)"
helm install aperio ./tools/charts/aperio-server --set existingSecret=aperio-token \
  --set image.tag=<version> --set ingress.enabled=true --set ingress.hosts[0].host=tunnel.example.com
```

`initSettings:` in the values becomes the first-start document; changing it on an installed release changes nothing, so roll settings out with an import. Other boot settings go through `extraEnv`. `/aperio/healthz` is the liveness probe and `/aperio/readyz` turns `503` the moment a shutdown starts, so traffic leaves the pod before it stops.

The client belongs in your workload's pod as a sidecar, where `localhost` is the app and nothing needs a Service or an Ingress. Scaling the workload adds clients behind the same hostname:

```yaml
- name: aperio-client
  image: ghcr.io/co3moz/aperio-client:<version>
  env:
    - { name: APERIO_SERVER_URL, value: https://tunnel.example.com }
    - { name: APERIO_TARGET, value: http://127.0.0.1:3000 }
    - { name: APERIO_HOSTNAME, value: app.example.com }
    - { name: APERIO_ACCESS, value: public }
    - name: APERIO_SERVER_TOKEN
      valueFrom: { secretKeyRef: { name: aperio-client-token, key: token } }
```

## In front of the server

The server speaks plain HTTP on `PORT`. Never put that port bare on the internet: either serve HTTPS from the server itself, or put a TLS-terminating proxy or a CDN in front of it.

**HTTPS from the server.** `tls.enabled` opens a second listener on `tls.port` (443), with certificates from PEM files, from an ACME authority (Let's Encrypt unless `tls.acme.directory` names another), or both:

```yaml
settings:
  tls:
    enabled: true
    redirect_http: true
    acme:
      enabled: true
      email: ops@example.com
      hostnames: [tunnel.example.com]
```

- **Turning it on.** `tls` is read at start, so save it and restart. The listener uses the boot `HOST`: a server that listened on loopback behind a proxy needs `HOST=0.0.0.0` or its public address. A container publishes the port (`-p 443:443`, and `-p 80:8080` for the redirect). The packaged unit may bind ports below 1024; anything else running the server unprivileged needs `CAP_NET_BIND_SERVICE`, or a forward from 443 to a higher `tls.port`.
- **The proof is TLS-ALPN-01, on 443 itself.** The authority connects to the hostname on port 443, so the listener has to be reachable there, directly or through a plain TCP forward; no port 80 and no DNS provider are involved. A wildcard needs a DNS-01 proof and comes from `tls.certificates` instead.
- **Which names get one.** Those in `tls.acme.hostnames` and `dashboard.hostname` are obtained at start. A panel's hostname, and with `tls.acme.on_demand` (on) a hostname a connected client serves, gets one the first time it is reached over HTTPS: that handshake waits for the certificate, up to a minute. Random subdomains are left out, and at most 20 are obtained on demand an hour. A name whose order fails is tried again after an hour, and both outcomes are audited (`tls_certificate_issued`, `tls_certificate_failed`).
- **Try the staging directory first** (`https://acme-staging-v02.api.letsencrypt.org/directory`): a mistake there costs nothing against the production rate limits. The account and the issued certificates are kept in `APERIO_DATA_DIR/tls/` and renewed 30 days before they end.
- **Your own files** in `tls.certificates` serve the names written in them, wildcards included, and are read again when they change, so a renewal by another tool needs no restart.
- **`tls.redirect_http`** answers plain HTTP with a redirect to HTTPS, except the health probes. Point clients at `https://`, since a WebSocket upgrade does not follow a redirect. With HTTPS on, `security.secure_cookies` defaults to on.

**Behind a proxy**, tell the server which proxies to believe: `proxy.trusted` lists their addresses and the visitor's is found by walking `X-Forwarded-For` back past them; `proxy.real_ip_header` reads a CDN's header (`CF-Connecting-IP`) instead. Trusting a header without a proxy lets anyone choose their address. For hostnames that appear at runtime, see [Behind a dynamic edge proxy](integrations.md#behind-a-dynamic-edge-proxy).

## Before going live

- **Credentials.** A master token of 32 random bytes (`openssl rand -hex 32`); scoped tokens for every client; a named user per operator with the least role, and a second factor for admins. Set `oidc.redirect_url` if you use OIDC. Seed a few canary tokens where a leak would expose them; using one raises `canary_tripped`.
- **HTTPS.** The plain port is not on the internet: the server serves HTTPS itself, or a proxy in front of it does (above).
- **The admin surface.** `security.admin_allowed_ips` fences the dashboard and the API to your operator network; `security.secure_cookies` is on behind a trusted proxy or with `tls.enabled`.
- **Limits.** Size `limits.ip_rate`, `limits.concurrent_requests` and `limits.request_body` to your traffic; keep `security.login_lockout` on.
- **Outbound calls.** Where webhook or autoscaling owners are not fully trusted, set `outbound.allowlist`, or at least `outbound.block_private`, so the server cannot be pointed at your internal network.
- **Data.** Set the `retention` periods and `retention.db_max_size`, schedule `backup` snapshots to somewhere off the box, and keep `inspector.redact` on.
- **Watching.** A webhook on `canary_tripped`, `token_new_ip`, `alert_triggered` and `disk_usage_warning`; `alerts.error_rate` and `alerts.client_down`; metrics scraped; the audit log shipped and verified (`aperio-server audit verify`).
- **The last look.** `aperio-server settings show` lists everything that differs from the defaults, and the startup log is clean: the server warns when its connection limits exceed the process's file-descriptor ceiling (`ulimit -n`, `LimitNOFILE=`) or its cache exceeds the container's memory.

The trust boundaries behind this list are in [Architecture](architecture.md#trust-boundaries).

## Restoring a backup

A logical dump (export and import) is for undoing a bad change on a running server. A snapshot is for getting a machine back:

1. Stop the server.
2. Move the existing `aperio.db`, `aperio.db-wal` and `aperio.db-shm` aside, keeping them.
3. `aperio-server backup decrypt <snapshot> $APERIO_DATA_DIR/aperio.db`, and make sure **no `-wal` or `-shm` file remains beside it**: a stale write-ahead log describes a database that no longer exists.
4. Give the file the ownership the server runs as.
5. Start the server and check the clients and tokens. Tokens issued after the snapshot are gone.

Rehearse the whole sequence in staging, not just the decrypt.

## Upgrading

**A client and a server pair only on the same minor release.** `0.13.0` and `0.13.4` work together; `0.13.x` and `0.12.x` do not, in either direction. The check happens at the WebSocket upgrade: a server on another minor answers `426` with a sentence naming both releases and the side to upgrade, and a client refuses a server on another minor the same way. A refused client keeps retrying, so it connects by itself once the other side is upgraded. `aperio-client check` shows the pairing. A patch never forces anything.

1. Read the [CHANGELOG](../CHANGELOG.md); every change that needs action is marked **Breaking**.
2. Export the settings and run `aperio-server check` on them with the new binary.
3. Back up the store.
4. Upgrade the server. With `APERIO_REUSEPORT=1` (Unix), start the new process on the same port and send the old one `SIGTERM`: it tells its clients to reconnect at once and drains before exiting.
5. On a minor release, upgrade the clients right after; each reconnects by itself.
6. Run `aperio-server audit verify` and check that every client is back.

The store only grows additively, so an older server can usually open a newer store. Across a minor release, downgrade the clients with it.

### To 0.13

The server reads no configuration file and only its [boot settings](configuration.md#boot-environment) from the environment; every other setting lives in the database. A 0.13 server that finds an old variable refuses to start and names the setting that replaced it. Moving a deployment over:

1. Write the old file, environment and dashboard overrides (`settings.json`) as one document under `settings:`, in the new names.
2. Check it: `aperio-server check server-init.yaml`.
3. Start 0.13 once with `APERIO_INIT_FILE=server-init.yaml` and without the old variables. Tokens, users and statistics are untouched.
4. Keep the file in git as a record; it is not read again.

| 0.12 | 0.13 |
| --- | --- |
| `trusted_proxies`, `trust_proxy`, `real_ip_header` | `proxy.trusted`, `proxy.trust`, `proxy.real_ip_header` |
| `server.auth`, `server_auth` | `access: {method: basic, users: "user:password"}` |
| `default_access: deny` / `allow` | `access: deny` / `access: public` |
| `ignore_client_auth: true` | `access` on a site for each hostname it should decide |
| `admin_allowed_ips`, `denied_ips`, `secure_cookies` | `security.admin_allowed_ips`, `security.denied_ips`, `security.secure_cookies` |
| `max_body_size`, `max_concurrent_requests`, `max_tunnels` | `limits.request_body`, `limits.concurrent_requests`, `limits.tunnels` |
| `gateway_timeout`, `gateway_response_timeout` | `gateway.timeout`, `gateway.response_timeout` |
| `lb_strategy`, `random_subdomain` | `routing.lb_strategy`, `routing.random_subdomain` |
| `cache: true`, `cache.max_bytes` | `cache.enabled: true`, `cache.size` |
| `oidc_*` | `oidc.*` |
| `alert_error_rate: 5` (percent), `alert_rules:` | `alerts.error_rate: 0.05`, `alerts.rules:` |
| `failover`, `outlier` | `resilience.failover`, `resilience.outlier`, defaulting to `resilience.profile` |
| `stream`, `tunnel.compression` | `advanced.stream`, `advanced.compression` |
| `504_page`, `503_page` (paths) | `pages.unavailable`, `pages.maintenance` of the `*` site (the HTML) |
| `routes:`, `rate_limits:`, `waf:`, `fallbacks:`, `error_pages:`, `maintenance_windows:`, `headers:` | one [`sites:`](configuration.md#sites) list |
| `expose:` | [public listeners](tunnels.md#public-listeners), from the dashboard or the API |
| `--check-config`, `--print-config`, `--print-schema`, `--verify-audit`, `--decrypt-backup` | `check`, `settings show`, `schema`, `audit verify`, `backup decrypt` |

A 0.13 client refuses a 0.12 file and names every key that moved:

| 0.12 | 0.13 |
| --- | --- |
| `server: URL`, `token:` | `server: {url, token}` |
| `services:`, `tunnels:` as lists with `name:` | maps keyed by name |
| a service setting at the top level | the same key under `defaults:` |
| `public: true`, `auth:` | `access: public`, `access:` with a method |
| `max_concurrent`, `adaptive_concurrency`, `max_request_body`, `max_response_body`, `max_redirects` | `limits.concurrent_requests`, `.adaptive_concurrency`, `.request_body`, `.response_body`, `.redirects` |
| `target_health`, `health_interval`, `health_timeout`, `health_threshold`, `wait_for_backend` | `health.endpoint`, `.interval`, `.timeout`, `.threshold`, `.wait_for_backend` |
| `capture` | `inspector` |
| `cache: true`, `resilience: true` | `cache: {enabled: true, offline: true}` |
| `retry.backoff: 100` (ms) | `retry.delay: 100ms` |
| `ip_family`, `egress_proxy`, `tls_min_version`, `tls_cipher_suites` | `server.ip_family`, `server.proxy`, `server.min_tls_version`, `server.tls_cipher_suites` |
| `messages_listen`, `messages_mqtt_listen` | `messages.listen`, `messages.mqtt_listen` |
| `bind-tunnels:`, `--bind-tunnels` | `binds:`, `aperio-client bind` |
| `--public`, `--server`, `--token`, `--host`, `--target` | `--access public`, `--server-url`, `--server-token`, `--hostname`, the positional target |

Variables follow the keys (`APERIO_PUBLIC=1` is `APERIO_ACCESS=public`), and **the environment now beats the file**, so a stale variable in a unit file wins where the file used to.

Behaviour that changed without a key to point at:
- **Resilience works harder by default.** Under `safe`, a request whose client dies is re-dispatched and failing clients are ejected. `resilience: {profile: off}` on the server and `resilience: off` under the client's `defaults:` restore 0.12.
- **`*: admin` is the server admin.** `master: admin` now administers master alone; grant `*: admin` to whoever runs the server.
- **Public listeners follow the role**, within the organization's port allocation. Review the allocations.
- **A service naming no hostname no longer serves every unclaimed one.** Write `hostname: "*"` where that was meant.
- **OIDC gates no visitor by being configured.** Write `access: {method: aperio}` or `{method: sso, ...}` where it did.
- **A client answers only the `forward` checks it declared.** A `forward` with `via: client` written in the server's own `access` needs `allow_server_forward: true` on every client that should answer it; set it before upgrading those clients.
- **Several services share one connection and a lone one grows a pool.** `multiplex: false` and `connections: 1` restore the old shape.
