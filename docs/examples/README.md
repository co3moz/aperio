# Configuration Examples

Each folder contains a client `aperio.yaml` and a server `aperio-server.yaml` for one scenario. Replace example tokens, hostnames, and backend addresses before running them. Binder, publisher, and organization examples need separate tokens with the permissions described in their READMEs.

Since 0.10.0 the server is **closed by default**. Most examples set `public: true` so visitors can reach the service. The `oidc` and `share_links` examples keep the visitor gate enabled; `visitor_auth` shows both protected and public services. Remove `public: true` and configure `auth:` when a service should require a login. See [Tokens & Authentication](../tokens-and-auth.md) for the access rules.

> **Note:** These are configuration templates. OIDC needs an identity provider, autoscaling needs a scaling endpoint, and MQTT needs a broker. Check each scenario's README and linked guide for its prerequisites. `aperio-server --check-config` validates server settings; `aperio-client check` also tests connectivity and therefore needs the real server and backend.

## Conventions

- Client config files put exposed backends under `services:`, including single-service examples. Top-level `target:` and related service keys are no longer accepted in config files. CLI one-liners and `APERIO_TARGET` still support one target. See [Configuration](../configuration.md#multiple-services).
- `https://tunnel.example.com` is a placeholder server URL. Replace it with your server's public URL.
- `apr_<scenario>_change_me` is a placeholder token. For a basic example, replace it with the same long random master token in both files. Organization, binder, and publisher examples use separately issued tokens; follow their READMEs. Do not commit real tokens; use environment-variable expansion such as `${APERIO_SERVER_TOKEN}` for secrets.
- Each folder covers one scenario. Some include multiple services to show how the settings differ.
- `version:` records the Aperio release the file targets. Keep it current so the client can report configuration changes during upgrades. See the [Upgrade Guide](../upgrade-guide.md).

| Folder | Scenario |
| --- | --- |
| [simple](simple/) | The minimal pair: one client, one backend, one token. |
| [multiple_services](multiple_services/) | One client exposing several backends, each with its own binds and tuning. |
| [static_site](static_site/) | Publish local directories of static files (`serve:`), no backend, one site, or several on their own hostnames. |
| [health_check](health_check/) | Backend health probes: a failing backend leaves rotation without dropping the tunnel, independently per service. |
| [headers](headers/) | Header add/remove rules on the client and the server side, and per service. |
| [load_balancing](load_balancing/) | Primary/standby failover tiers via `priority`, including a machine that is primary for some routes and standby for others. |
| [sticky_sessions](sticky_sessions/) | Pin each visitor to the client that first served them. |
| [failover](failover/) | In-flight failover: re-dispatch requests when a client dies mid-request. |
| [autoscaling](autoscaling/) | Scale out through an endpoint you control, scale in by idling out, and cold-start from zero. |
| [cache](cache/) | Server-side GET response cache, opted in per service. |
| [resilience](resilience/) | Serve cached (even stale) responses while no healthy client is connected. |
| [messaging](messaging/) | Clients signalling each other over the tunnel they already hold: subscribe, publish, and run a command on receipt. |
| [emergency_tunnels](emergency_tunnels/) | Break-glass TCP/UDP tunnels to private services (`tunnels:` / `bind-tunnels:`). |
| [encrypted_tunnels](encrypted_tunnels/) | End-to-end encrypted tunnels with a pre-shared key. |
| [mqtt](mqtt/) | An MQTT broker reachable by every client of an organization, and by nothing else. |
| [public_expose](public_expose/) | Expose a declared tunnel on a raw public server port, owned by a named token. |
| [routes](routes/) | Client-less routes: redirects and fixed responses served by the server alone. |
| [traffic_rules](traffic_rules/) | Server-side request rules: per-route rate limits, WAF-lite, fallbacks, per-hostname error pages. |
| [visitor_auth](visitor_auth/) | Visitor login gates: server-wide password, client-set override, and `public:`. |
| [allowed_ips](allowed_ips/) | Restrict a service to specific visitor IPs/CIDRs, per service. |
| [random_subdomain](random_subdomain/) | Preview environments on random subdomains, kept out of search engines. |
| [grpc](grpc/) | Expose a gRPC backend over an HTTP/2 (`h2c://`) target, alongside ordinary HTTP. |
| [behind_proxy](behind_proxy/) | Run the server behind a reverse proxy / CDN with correct client IPs. |
| [observability](observability/) | Prometheus metrics, access log, OpenTelemetry traces, and alerting. |
| [oidc](oidc/) | Put an identity-provider (SSO) login in front of everything the tunnel serves. |
| [share_links](share_links/) | Temporary, scoped visitor access to a gated site, no accounts. |
| [organizations](organizations/) | Multi-tenancy: isolate one server into separate organizations. |
| [dashboard](dashboard/) | The admin dashboard: signing in with the master token or a named user, IP fencing, headless off. |
| [tuning](tuning/) | Capacity knobs: concurrency, parallel connections, bandwidth, timeouts, per service and shared. |

Tip: point your editor at the generated JSON Schemas for completion and validation while editing these files, see [Configuration → Editor autocompletion](../configuration.md#editor-autocompletion-json-schema).
