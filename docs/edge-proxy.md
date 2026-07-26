# Behind a Dynamic Edge Proxy

Aperio is usually not the outermost thing on the box. Traefik, Caddy, or nginx terminates TLS and Aperio sits behind it. That works fine for a fixed set of hostnames, but Aperio's hostnames are **born at runtime**: a client connects, claims `app.example.com`, and now the edge needs a route and a certificate for a name it has never heard of. Container labels cannot express that (they are fixed when the container is created), and hand-written config defeats the point of a tunnel server.

There are three ways to solve it. Start at the top: the first one costs nothing and covers most deployments.

> **Config surfaces.** Settings below are named by their `APERIO_*` environment variable; each also has an equivalent `aperio-server.yaml` key, the same name lowercased, without the `APERIO_` prefix (e.g. `APERIO_EDGE_TOKEN` → `edge_token`). YAML is the primary surface, the file is loaded into the environment at startup and wins over it: put server keys in `aperio-server.yaml`, client keys in `aperio.yaml`. See [Configuration](configuration.md) for the full mapping.

## 1. One wildcard domain: nothing to integrate

If every tunnel hostname lives under a single domain you control, the edge never needs to know individual names. Give it one wildcard route and one wildcard certificate (DNS-01), and you are done.

```yaml
# docker-compose.yml, on the aperio service
labels:
  - "traefik.enable=true"
  - "traefik.http.routers.aperio.rule=HostRegexp(`^.+\\.example\\.com$`)"
  - "traefik.http.routers.aperio.entrypoints=websecure"
  - "traefik.http.routers.aperio.tls.certresolver=dns"
  - "traefik.http.routers.aperio.tls.domains[0].main=example.com"
  - "traefik.http.routers.aperio.tls.domains[0].sans=*.example.com"
  - "traefik.http.services.aperio.loadbalancer.server.port=8080"
```

This is the natural fit for `APERIO_RANDOM_SUBDOMAIN` (yaml `random_subdomain`) preview environments, where every hostname is `<random>.example.com` by construction. No polling, no extra endpoint, no coupling.

The rest of this article is for the case this cannot cover: **hostnames that are not under one wildcard**, typically customer domains pointed at your server by CNAME. Those need a per-host route and a per-host certificate.

## Enabling the edge endpoints

Both remaining options are served by two endpoints on the Aperio server, and both are **off until you set a token** there:

```yaml
# aperio-server.yaml
edge_token: a-long-random-secret        # env: APERIO_EDGE_TOKEN, required, enables the endpoints
edge_service_url: http://aperio:8080    # env: APERIO_EDGE_SERVICE_URL, only for the Traefik document
```

| Env variable | yaml key | Meaning |
| --- | --- | --- |
| `APERIO_EDGE_TOKEN` | `edge_token` | Credential the edge presents. Unset = the endpoints answer `404 edge integration is not enabled`. |
| `APERIO_EDGE_SERVICE_URL` | `edge_service_url` | The URL the edge should forward matched traffic to, i.e. this server as the edge sees it. |
| `APERIO_EDGE_ENTRYPOINTS` | `edge_entrypoints` | Comma-separated Traefik entry points for the generated routers (e.g. `websecure`). |
| `APERIO_EDGE_CERT_RESOLVER` | `edge_cert_resolver` | Traefik certificate resolver named on the generated routers. |
| `APERIO_EDGE_INCLUDE_OFFLINE` | `edge_include_offline` | Also publish hostnames a token permits but no client is serving. Off by default, see the warning below. |

The token is accepted as `Authorization: Bearer <token>` or as a `?token=` query parameter, because Caddy's `ask` cannot send headers.

## 2. Caddy: on-demand TLS with no configuration at all

Caddy can ask before it issues a certificate. Point it at Aperio and every new tunnel hostname gets a certificate the moment it appears:

```
# Caddyfile, on the edge proxy host
{
  on_demand_tls {
    ask http://aperio:8080/aperio/api/edge/ask?token=YOUR_EDGE_TOKEN
  }
}

https:// {
  tls {
    on_demand
  }
  reverse_proxy aperio:8080
}
```

`GET /aperio/api/edge/ask?domain=<host>` answers `200` when a client is currently serving that hostname and `404` when it is not, which is exactly what Caddy's `ask` expects. Nothing is generated, nothing is polled, and a hostname nobody serves never triggers an ACME request.

This is the simplest of the three for arbitrary customer domains, and it is the reason to prefer Caddy at the edge for that use case: Traefik has no equivalent of on-demand TLS.

## 3. Traefik: the HTTP provider

Traefik cannot ask, so it needs the route list up front. `GET /aperio/api/edge/traefik` returns a dynamic-configuration document with one router per served hostname:

```json
{
  "http": {
    "routers": {
      "aperio-app.example.com": {
        "rule": "Host(`app.example.com`)",
        "service": "aperio",
        "entryPoints": ["websecure"],
        "tls": { "certResolver": "letsencrypt" }
      }
    },
    "services": {
      "aperio": {
        "loadBalancer": { "passHostHeader": true, "servers": [{ "url": "http://aperio:8080" }] }
      }
    }
  }
}
```

Point Traefik's HTTP provider at it, in Traefik's **static** configuration:

```yaml
# traefik.yml, on the edge proxy host
providers:
  http:
    endpoint: "http://aperio:8080/aperio/api/edge/traefik"
    pollInterval: 10s
    headers:
      Authorization: "Bearer YOUR_EDGE_TOKEN"
```

### About that static block

Providers are part of Traefik's install (static) configuration; they cannot be declared with container labels, because the Docker provider has to exist before any label can be read. So adding Aperio to a Traefik that did not have it means one config block and one Traefik restart, once.

That is the only static part. **The routes themselves are fully dynamic**: what the endpoint returns is dynamic configuration, applied live on each poll. A tunnel that connects now is routable within one `pollInterval`, with no restart and no reload.

The chicken-and-egg this looks like mostly is not one. The endpoint is addressed by service name (`http://aperio:8080/...`), not by an IP that has to be known in advance, and Docker's embedded DNS resolves it. If Aperio is not up yet, Traefik logs a failed poll and retries on the next interval, so there is no startup ordering requirement in either direction.

### Polling notes

Traefik polls (it does not long-poll or subscribe), so the document is fetched every `pollInterval` regardless of whether anything changed, and there is no conditional-request support to lean on. The document is therefore kept small and **deterministically ordered**, and router keys are derived from the hostname rather than from a position, so an unchanged inventory produces a byte-identical document and Traefik never churns routers.

## What counts as "served"

Both endpoints report the hostnames of **connected** clients, including a client whose backend is currently failing its health probe. That is deliberate: such a client still owns its hostname, and withdrawing the route (or letting the certificate lapse) would turn a recoverable `504` into a TLS failure, or worse, hand the name to a wildcard route belonging to something else.

`APERIO_EDGE_INCLUDE_OFFLINE=1` additionally publishes hostnames a token *permits* but nobody is serving yet, so a certificate can exist before the first client ever connects. Weigh it carefully in multi-tenant setups: it lets a tenant provoke an ACME request for any hostname its token covers, which is a way to burn rate limits at your expense. It is off by default. Combine it with a [per-organization hostname allowlist](organizations.md) if you enable it.

## Security

- The endpoints do nothing without `APERIO_EDGE_TOKEN`: no token, `404 edge integration is not enabled`, no inventory in the answer. The two paths are reserved either way, so a request for them is never forwarded to a client's backend.
- They expose the hostname inventory, so treat the token as a real credential and prefer a private network between the edge and Aperio.
- They live outside the dashboard session middleware, so they keep working with the dashboard disabled, and they are never reachable with a dashboard session alone.
- No mutation is possible through them: both are read-only.

## Related

- [Configuration Reference](configuration.md), every setting and the endpoint list.
- [Routing & Load Balancing](routing-and-load-balancing.md), how hostname binds are claimed in the first place.
- [Organizations](organizations.md), fencing which hostnames a tenant may claim.
