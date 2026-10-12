# Examples

Each folder is one scenario: a client `aperio.yaml`, and a `server-init.yaml` when the scenario needs server settings. Replace the placeholder tokens (`apr_<scenario>_change_me`), hostnames and addresses first.

```bash
APERIO_SERVER_TOKEN=apr_simple_change_me APERIO_INIT_FILE=server-init.yaml aperio-server
aperio-client            # in the folder, reads ./aperio.yaml
```

`server-init.yaml` is read once, into an empty data directory; after that the settings are changed from the dashboard. Routes are closed by default, so most examples say `access: public`. `aperio-server check server-init.yaml` and `aperio-client check` test a folder before you run it.

| Folder | Scenario |
| --- | --- |
| [simple](simple/) | Simple |
| [multiple_services](multiple_services/) | Multiple services |
| [static_site](static_site/) | Static sites |
| [grpc](grpc/) | gRPC |
| [headers](headers/) | Headers |
| [health_check](health_check/) | Health checks |
| [load_balancing](load_balancing/) | Primary and standby |
| [sticky_sessions](sticky_sessions/) | Sticky sessions |
| [failover](failover/) | Failover |
| [cache](cache/) | Response cache |
| [resilience](resilience/) | Serving from the cache while offline |
| [tuning](tuning/) | Tuning |
| [routes](routes/) | Sites that answer by themselves |
| [traffic_rules](traffic_rules/) | Traffic rules |
| [random_subdomain](random_subdomain/) | Random subdomains |
| [visitor_auth](visitor_auth/) | Visitor gates |
| [allowed_ips](allowed_ips/) | Allowed IPs |
| [oidc](oidc/) | Single sign-on |
| [share_links](share_links/) | Share links |
| [organizations](organizations/) | Organizations |
| [emergency_tunnels](emergency_tunnels/) | Tunnels |
| [encrypted_tunnels](encrypted_tunnels/) | Encrypted tunnels |
| [mqtt](mqtt/) | An MQTT broker between clients |
| [public_expose](public_expose/) | Public listener |
| [messaging](messaging/) | Messages between clients |
| [autoscaling](autoscaling/) | Autoscaling |
| [behind_proxy](behind_proxy/) | Behind a proxy or CDN |
| [observability](observability/) | Observability |
| [dashboard](dashboard/) | The dashboard |
