# Configuration Examples

Ready-to-adapt configuration pairs for common Aperio scenarios. Every folder contains a client `aperio.yaml` and a server `aperio-server.yaml` that work **together**: the tunnel token in the client file matches the `server_token` in the server file, so you can copy a folder, replace the token and hostnames, and run both sides as-is.

## Naming conventions

- **`m_` (multi)**, the client is configured to do the work of several clients in one process, via a `services:` list. This is the recommended shape for anything beyond a quick one-off.
- **`s_` (single)**, the client exposes exactly one thing through the top-level `target:`/`serve:` keys. Single-service mode is fully supported (and is what the CLI one-liners produce), but for permanent setups prefer the `m_` form, every `s_` example translates mechanically into one `services:` entry.
- `https://tunnel.example.com`, the public URL of your Aperio server.
- `apr_<scenario>_change_me`, a placeholder token; replace it with a long random string of your own.
- Topics that need more than one example use numbered folders (`s_load_balancing`, `s_load_balancing_2`, …).

| Folder | Scenario |
| --- | --- |
| [s_simple](s_simple/) | The minimal pair: one client, one target, one token. |
| [s_static_site](s_static_site/) | Publish a local directory of static files (`serve:`), no backend. |
| [m_static_site](m_static_site/) | One client serving two static directories on two hostnames (per-service `serve:`). |
| [m_services](m_services/) | One client exposing several backends via a `services:` list. |
| [s_health_check](s_health_check/) | Backend health probes: leave rotation when the backend is down, without dropping the tunnel. |
| [m_health_check](m_health_check/) | Independent health probes per service, one backend going down doesn't touch the others. |
| [s_headers](s_headers/) | Header add/remove rules on both the client and the server side. |
| [m_headers](m_headers/) | Per-service header rules: an entry's `headers:` replaces the shared defaults. |
| [s_load_balancing](s_load_balancing/) | Primary/standby failover tiers via client `priority`. |
| [m_load_balancing](m_load_balancing/) | Per-service priority tiers: each machine primary for some routes, standby for others. |
| [s_load_balancing_2](s_load_balancing_2/) | Sticky sessions, pin each visitor to the client that first served them. |
| [s_failover](s_failover/) | In-flight failover: re-dispatch requests when a client dies mid-request. |
| [s_cache](s_cache/) | Server-side GET response cache, opted in per service. |
| [m_cache](m_cache/) | Per-service cache opt-in: a cached site next to a strictly-proxied API. |
| [s_resilience](s_resilience/) | Serve cached (even stale) responses while no healthy client is connected. |
| [m_resilience](m_resilience/) | Per-service serve-stale: the static site survives outages, the API fails honestly. |
| [s_emergency_tunnels](s_emergency_tunnels/) | Break-glass TCP/UDP tunnels to private services (`tunnels:` / `bind-tunnels:`). |
| [s_emergency_tunnels_2](s_emergency_tunnels_2/) | End-to-end encrypted tunnels with a pre-shared key. |
| [s_public_expose](s_public_expose/) | Expose a declared tunnel on a raw public server port (experimental). |
| [s_routes](s_routes/) | Client-less routes: redirects and fixed responses served by the server alone. |
| [m_visitor_auth](m_visitor_auth/) | Visitor login gates: server-wide password, client-set override, and `public:`. |
| [s_allowed_ips](s_allowed_ips/) | Restrict a service to specific visitor IPs/CIDRs. |
| [m_allowed_ips](m_allowed_ips/) | Per-service IP allowlists: a locked-down admin panel next to a public app. |
| [s_random_subdomain](s_random_subdomain/) | Preview environments on random subdomains, kept out of search engines. |
| [s_grpc](s_grpc/) | Expose a gRPC backend over an HTTP/2 (`h2c://`) target. |
| [m_grpc](m_grpc/) | A gRPC service and an HTTP web app mixed in one client. |
| [s_behind_proxy](s_behind_proxy/) | Run the server behind a reverse proxy / CDN with correct client IPs. |
| [s_observability](s_observability/) | Prometheus metrics, access log, OpenTelemetry traces, and alerting. |
| [s_oidc](s_oidc/) | Put an identity-provider (SSO) login in front of everything the tunnel serves. |
| [s_tuning](s_tuning/) | Capacity knobs: concurrency, parallel connections, bandwidth, timeouts. |
| [m_tuning](m_tuning/) | Per-service capacity knobs: hot API, slow reports, bandwidth-paced media. |

Tip: point your editor at the generated JSON Schemas for completion and validation while editing these files, see [Configuration → Editor autocompletion](../configuration.md#editor-autocompletion-json-schema).
