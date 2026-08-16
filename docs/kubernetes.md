# Kubernetes

A Helm chart for the server, and a sidecar for the client. Both are in
[`charts/`](../tools/charts/).

```bash
helm install aperio ./charts/aperio-server \
  --set existingSecret=aperio-token \
  --set ingress.enabled=true \
  --set ingress.hosts[0].host=tunnel.example.com
```

## The values file is the server's own configuration

Everything under `config:` is written out as `aperio-server.yaml` and handed
to the server as `APERIO_SERVER_CONFIG`, unchanged. The chart does not read
it, validate it or know what is in it.

```yaml
config:
  host: 0.0.0.0
  port: 8080
  trusted_proxies: ["10.0.0.0/8"]
  cache:
    enabled: true
    max_bytes: 268435456
  backup:
    interval: 86400
    dir: /var/lib/aperio/backups
```

So the reference is [configuration.md](configuration.md) and the JSON Schema
published with each release, not a second vocabulary invented by the chart,
and **a setting added to Aperio works here the day it ships** without the
chart being touched.

This is the trap the chart is built to avoid, and it is the usual one: a chart
that mirrors a subset of the application's settings as values of its own,
which then drift, cover less with every release, and leave an operator
learning two names for one thing. The chart's own values are only the things
Kubernetes needs and Aperio has no opinion about: the image, the volume, the
Service, the Ingress, the probes.

You can check a values file against the real thing before installing it:

```bash
helm template aperio ./charts/aperio-server -f my-values.yaml \
  | yq 'select(.kind == "ConfigMap") | .data["aperio-server.yaml"]' > /tmp/aperio-server.yaml
APERIO_SERVER_TOKEN=x APERIO_SERVER_CONFIG=/tmp/aperio-server.yaml aperio-server --check-config
```

## The master token

It is not in `config:`, and it should not be in a values file either. The
chart takes either a token to put in a Secret it creates, or the name of a
Secret you already manage:

```bash
kubectl create secret generic aperio-token --from-literal=token="$(openssl rand -hex 32)"
helm install aperio ./charts/aperio-server --set existingSecret=aperio-token
```

It arrives as `APERIO_SERVER_TOKEN`, which wins over the file. Everything else
that comes from a secret manager goes through `extraEnv`, since every setting
is reachable as an `APERIO_*` variable:

```yaml
extraEnv:
  - name: APERIO_OIDC_CLIENT_SECRET
    valueFrom:
      secretKeyRef: { name: aperio-oidc, key: client-secret }
```

## One replica, and why it is not a placeholder

The store is SQLite on a `ReadWriteOnce` volume, and two servers sharing it
would corrupt it. Scaling out means a second deployment with its own volume
and its own hostnames, which is a decision to make deliberately rather than by
raising a number.

That is also why it is a StatefulSet. A Deployment with a PVC hands the same
volume to a new pod while the old one may still hold it, so the rolling update
either deadlocks or briefly gives two servers one SQLite file.
`volumeClaimTemplates` bind the volume to an ordinal and the old pod stops
before the new one starts, which is what a single-writer store needs.

## The probes

`/aperio/healthz` is bodiless and takes no locks, so it is cheap enough for a
liveness probe every few seconds. `/aperio/readyz` is the interesting one: it
answers 503 from the moment a shutdown signal arrives while the process is
still serving, so the load balancer stops routing to the pod while the drain
finishes what is already in flight. Pair it with a `terminationGracePeriod`
that gives the drain room; the chart defaults to 60 seconds.

## The client is a sidecar

The server belongs in a chart because it is a deployment of its own. The
client usually does not: its job is to reach one workload, so it belongs in
that workload's pod, where `localhost` is the backend and no Service, no
NetworkPolicy and no cluster DNS entry has to exist for the two to talk.

```yaml
      containers:
        - name: app
          image: your/app:1.0
          ports:
            - containerPort: 3000

        - name: aperio-client
          image: ghcr.io/co3moz/aperio-client:0.9.0
          env:
            - name: APERIO_SERVER_URL
              value: https://tunnel.example.com
            - name: APERIO_TARGET
              value: http://127.0.0.1:3000
            - name: APERIO_HOSTNAME
              value: app.example.com
            - name: APERIO_SERVER_TOKEN
              valueFrom:
                secretKeyRef: { name: aperio-client-token, key: token }
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            runAsNonRoot: true
            capabilities: { drop: ["ALL"] }
```

Two things follow from this shape and are worth saying out loud:

- **The app does not need a Service or an Ingress.** The tunnel is outbound,
  so nothing in the cluster has to be reachable from outside, which is the
  reason to use Aperio here at all.
- **Scaling the workload scales the tunnel.** Every replica dials in and the
  server load-balances across them, so `kubectl scale` is also how you add
  capacity behind a hostname. Give each one the same token and the same
  hostname; the server treats them as what they are, several clients serving
  one name.

For a client that fronts something it is *not* deployed with, a network
appliance, a database on another network, run it as its own small Deployment
with the same environment. There is no chart for that case because there is
nothing in it a chart would add beyond what is above.
