# The Dashboard

> **Concept:** [The Dashboard](../../dashboard.md).

The admin dashboard is embedded in the server binary at `/aperio`, no extra deployment. It shows live traffic, the request inspector and replay, the topology map, the client kill switch, maintenance mode, tokens, and settings. This example shows the settings that shape access to it.

## Access

- **Default login:** user `aperio`, password = the master token.
- **Separate password:** set `dashboard_auth` (as here) so dashboard users do not need the master token. Named users with roles (viewer / operator / admin) can also be created from the *Users* page, and OIDC works too.
- **Network fence:** `admin_allowed_ips` restricts the dashboard and `/aperio/api/*` to your operator CIDRs. Proxied sites and their login pages stay reachable from anywhere, only the authenticated admin surface is fenced.
- **Headless:** set `dashboard: false` to disable the dashboard entirely; the proxy keeps working and you manage over the API.

Open `https://tunnel.example.com/aperio` and sign in. Connect the client in this folder to a local service, send it some traffic, then click a row in *Live Traffic* to open the inspector.
