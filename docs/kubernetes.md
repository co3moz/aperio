# Kubernetes

A Helm chart deploys the server; the client can run beside your app in its pod. The chart is in [`tools/charts/aperio-server`](../tools/charts/aperio-server/).

Create the `aperio-token` Secret shown [below](#the-master-token) before installing. Replace the hostname, configure DNS and TLS on the Ingress, and set `proxy.trusted` to your actual proxy ranges. The chart currently defaults to a 0.9.0 server image, so set the image tag to the release you intend to run.

```bash
helm install aperio ./tools/charts/aperio-server \
  --set existingSecret=aperio-token \
  --set image.tag=0.13.0 \
  --set ingress.enabled=true \
  --set ingress.hosts[0].host=tunnel.example.com
```

## The values file seeds the server's settings

The server keeps its settings in its database, on the volume, and changes them
from the dashboard without a restart. What the chart can say is what a new
server starts with: everything under `initSettings:` is written out as the
`settings:` section of a document handed to the server as `APERIO_INIT_FILE`,
unchanged. The chart does not read it, validate it or know what is in it, and
the server reads it once, into an empty database.

```yaml
initSettings:
  proxy:
    trusted: ["10.0.0.0/8"]
  cache:
    enabled: true
    size: 256MB
  backup:
    interval: 1d
    dir: /var/lib/aperio/backups
```

Use [Configuration](configuration.md) or the release JSON Schema for the settings. Changing `initSettings:` on an installed release changes nothing on the running server, deliberately: to roll a settings document out to it, `POST /aperio/api/import` it. The chart's other values cover Kubernetes resources: the image, volume, Service, Ingress, and probes.

You can check a values file against the real thing before installing it:

```bash
helm template aperio ./tools/charts/aperio-server -f my-values.yaml \
  | yq 'select(.kind == "ConfigMap") | .data["server-init.yaml"]' > /tmp/server-init.yaml
aperio-server check /tmp/server-init.yaml
```

## The master token

It should not be in a values file. The chart takes either a token to put in a
Secret it creates, or the name of a Secret you already manage:

```bash
kubectl create secret generic aperio-token --from-literal=token="$(openssl rand -hex 32)"
helm install aperio ./tools/charts/aperio-server --set existingSecret=aperio-token
```

It arrives as `APERIO_SERVER_TOKEN`. The other boot settings a secret manager
holds, a backup encryption key for instance, go through `extraEnv`. A
credential among the settings (the OIDC client secret, say) is set from the
dashboard after the first start, where it is stored and never read back.

```yaml
extraEnv:
  - name: APERIO_BACKUP_KEY
    valueFrom:
      secretKeyRef: { name: aperio-backup, key: key }
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
finishes what is already in flight. Pair it with a
`terminationGracePeriodSeconds` that gives the drain room; the chart defaults
to 60 seconds.

## The client is a sidecar

The server belongs in a chart because it is a deployment of its own. The
client usually does not: its job is to reach one workload, so it belongs in
that workload's pod, where `localhost` is the backend and no Service, no
NetworkPolicy and no cluster DNS entry has to exist for the two to talk.

```yaml
# Fragment of a Pod template spec (spec.template.spec in a Deployment).
containers:
  - name: app
    image: your/app:1.0
    ports:
      - containerPort: 3000

  - name: aperio-client
    image: ghcr.io/co3moz/aperio-client:0.13.0
    env:
      - name: APERIO_SERVER_URL
        value: https://tunnel.example.com
      - name: APERIO_TARGET
        value: http://127.0.0.1:3000
      - name: APERIO_HOSTNAME
        value: app.example.com
      - name: APERIO_PUBLIC
        value: "1"
      - name: APERIO_SERVER_TOKEN
        valueFrom:
          secretKeyRef: { name: aperio-client-token, key: token }
    securityContext:
      allowPrivilegeEscalation: false
      readOnlyRootFilesystem: true
      runAsNonRoot: true
      capabilities: { drop: ["ALL"] }
```

> **Note:** Create `aperio-client-token` with a token allowed to publish `app.example.com` and public services. Replace the hostnames and app image, and point DNS at the Ingress. `APERIO_PUBLIC=1` is needed for a public route on servers using the default closed access policy. See [Tokens & Authentication](tokens-and-auth.md) and [Configuration](configuration.md#closed-by-default).

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
