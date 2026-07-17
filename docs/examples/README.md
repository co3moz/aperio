# Configuration Examples

Ready-to-adapt configuration pairs for common Aperio scenarios. Every folder contains a client `aperio.yaml` and a server `aperio-server.yaml` that work **together**: the tunnel token in the client file matches the `server_token` in the server file, so you can copy a folder, replace the token and hostnames, and run both sides as-is.

Conventions used throughout:

- `https://tunnel.example.com` — the public URL of your Aperio server.
- `apr_<scenario>_change_me` — a placeholder token; replace it with a long random string of your own.
- Topics that need more than one example use numbered folders (`load_balancing`, `load_balancing_2`, …).

| Folder | Scenario |
| --- | --- |
| [simple](simple/) | The minimal pair: one client, one target, one token. |
| [static_site](static_site/) | Publish a local directory of static files (`serve:`) — no backend. |
| [services](services/) | One client exposing several backends via a `services:` list. |
| [health_check](health_check/) | Backend health probes: leave rotation when the backend is down, without dropping the tunnel. |
| [headers](headers/) | Header add/remove rules on both the client and the server side. |
| [load_balancing](load_balancing/) | Primary/standby failover tiers via client `priority`. |
| [load_balancing_2](load_balancing_2/) | Sticky sessions — pin each visitor to the client that first served them. |
| [failover](failover/) | In-flight failover: re-dispatch requests when a client dies mid-request. |
| [cache](cache/) | Server-side GET response cache, opted in per service. |
| [resilience](resilience/) | Serve cached (even stale) responses while no healthy client is connected. |
| [emergency_tunnels](emergency_tunnels/) | Break-glass TCP/UDP tunnels to private services (`tunnels:` / `bind-tunnels:`). |
| [emergency_tunnels_2](emergency_tunnels_2/) | End-to-end encrypted tunnels with a pre-shared key. |
| [public_expose](public_expose/) | Expose a declared tunnel on a raw public server port (experimental). |
| [routes](routes/) | Client-less routes: redirects and fixed responses served by the server alone. |
| [visitor_auth](visitor_auth/) | Visitor login gates: server-wide password, client-set override, and `public:`. |
| [allowed_ips](allowed_ips/) | Restrict a service to specific visitor IPs/CIDRs. |
| [random_subdomain](random_subdomain/) | Preview environments on random subdomains, kept out of search engines. |
| [grpc](grpc/) | Expose a gRPC backend over an HTTP/2 (`h2c://`) target. |
| [behind_proxy](behind_proxy/) | Run the server behind a reverse proxy / CDN with correct client IPs. |
| [observability](observability/) | Prometheus metrics, access log, OpenTelemetry traces, and alerting. |
| [oidc](oidc/) | Put an identity-provider (SSO) login in front of everything the tunnel serves. |
| [tuning](tuning/) | Capacity knobs: concurrency, parallel connections, bandwidth, timeouts. |

Tip: point your editor at the generated JSON Schemas for completion and validation while editing these files — see [Configuration → Editor autocompletion](../configuration.md#editor-autocompletion-json-schema).
