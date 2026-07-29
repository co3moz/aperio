# Configuration Examples

Ready-to-adapt configuration pairs for common Aperio scenarios. Every folder contains a client `aperio.yaml` and a server `aperio-server.yaml` that work **together**: the tunnel token in the client file matches `server.token` in the server file, so you can copy a folder, replace the token and hostnames, and run both sides as-is.

## Conventions

- **Every client file describes its backends under `services:`**, even the ones that expose exactly one. That is the shape a config file has; naming a single backend at the top level (`target:`, `serve:`, `hostname:`, …) still works but is deprecated and goes away in 0.7.0. Single-service mode lives on in the CLI one-liner and the `APERIO_TARGET` family, where a one-liner is the point.
- `https://tunnel.example.com`, the public URL of your Aperio server.
- `apr_<scenario>_change_me`, a placeholder token; replace it with a long random string of your own.
- One folder per scenario. Where a feature reads differently with one service than with several, the folder shows both.

| Folder | Scenario |
| --- | --- |
| [simple](simple/) | The minimal pair: one client, one backend, one token. |
| [multiple_services](multiple_services/) | One client exposing several backends, each with its own binds and tuning. |
| [static_site](static_site/) | Publish local directories of static files (`serve:`), no backend — one site, or several on their own hostnames. |
| [health_check](health_check/) | Backend health probes: a failing backend leaves rotation without dropping the tunnel, independently per service. |
| [headers](headers/) | Header add/remove rules on the client and the server side, and per service. |
| [load_balancing](load_balancing/) | Primary/standby failover tiers via `priority`, including a machine that is primary for some routes and standby for others. |
| [sticky_sessions](sticky_sessions/) | Pin each visitor to the client that first served them. |
| [failover](failover/) | In-flight failover: re-dispatch requests when a client dies mid-request. |
| [cache](cache/) | Server-side GET response cache, opted in per service. |
| [resilience](resilience/) | Serve cached (even stale) responses while no healthy client is connected. |
| [emergency_tunnels](emergency_tunnels/) | Break-glass TCP/UDP tunnels to private services (`tunnels:` / `bind-tunnels:`). |
| [encrypted_tunnels](encrypted_tunnels/) | End-to-end encrypted tunnels with a pre-shared key. |
| [mqtt](mqtt/) | An MQTT broker reachable by every client of an organization, and by nothing else. |
| [public_expose](public_expose/) | Expose a declared tunnel on a raw public server port, owned by a named token. |
| [routes](routes/) | Client-less routes: redirects and fixed responses served by the server alone. |
| [visitor_auth](visitor_auth/) | Visitor login gates: server-wide password, client-set override, and `public:`. |
| [allowed_ips](allowed_ips/) | Restrict a service to specific visitor IPs/CIDRs, per service. |
| [random_subdomain](random_subdomain/) | Preview environments on random subdomains, kept out of search engines. |
| [grpc](grpc/) | Expose a gRPC backend over an HTTP/2 (`h2c://`) target, alongside ordinary HTTP. |
| [behind_proxy](behind_proxy/) | Run the server behind a reverse proxy / CDN with correct client IPs. |
| [observability](observability/) | Prometheus metrics, access log, OpenTelemetry traces, and alerting. |
| [oidc](oidc/) | Put an identity-provider (SSO) login in front of everything the tunnel serves. |
| [share_links](share_links/) | Temporary, scoped visitor access to a gated site, no accounts. |
| [organizations](organizations/) | Multi-tenancy: isolate one server into separate organizations. |
| [dashboard](dashboard/) | The admin dashboard: separate password, IP fencing, headless off. |
| [tuning](tuning/) | Capacity knobs: concurrency, parallel connections, bandwidth, timeouts, per service and shared. |

Tip: point your editor at the generated JSON Schemas for completion and validation while editing these files, see [Configuration → Editor autocompletion](../configuration.md#editor-autocompletion-json-schema).
