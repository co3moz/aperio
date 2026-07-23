# Organizations (Multi-Tenancy)

Organizations turn one Aperio server into several isolated tenants. Each organization has its own tunnel clients, API tokens, dashboard users, traffic, statistics, webhooks, audit trail, and maintenance flags, and the members of one organization never see or touch another's. A named admin who signs in lands in a self-contained view of their own organization and nothing else.

## The master organization

There is always exactly one implicit **master** organization. Everything created without an organization belongs to it: the built-in `aperio` admin (who signs in with the master token or the dashboard password), the master token's own tunnel clients, and any tokens or users minted while master is selected. Master is not a stored record, internally it is simply "no organization" (`org_id: null`); the API refers to it by the reserved id `master`.

## Child organizations

Child organizations are created from master by the super-admin. Once a child organization is selected, everything you create, tokens, users, and (through those tokens) tunnel clients, belongs to it. Its members see only its resources; a token minted under a child organization only appears there, and the clients that authenticate with it are attributed to it everywhere (traffic log, live view, stats, uptime, topology).

Child organizations are managed through the dashboard's **Organizations** page or the API:

```bash
# List organizations (master + children, with per-org user/token counts)
curl -b cookies.txt https://tunnel.example.com/aperio/api/orgs

# Create a child organization
curl -b cookies.txt -X POST -H 'Content-Type: application/json' \
  --data '{"name":"Acme"}' https://tunnel.example.com/aperio/api/orgs

# Delete an (empty) child organization, refused while it still has users or tokens
curl -b cookies.txt -X DELETE https://tunnel.example.com/aperio/api/orgs/<id>
```

Organization names are unique (case-insensitive); `master` is reserved. A child organization can only be deleted once all of its users and tokens are removed, so nothing is silently orphaned. All of these endpoints require the master super-admin (below).

## The super-admin and switching organizations

The built-in `aperio` admin is the hidden super-admin of every organization. Only this account can:

- create, list, and delete child organizations,
- **switch** which organization it is acting in, from the organization picker in the dashboard sidebar (or `POST /aperio/api/orgs/select` with `{"id": "<org-id>"}`; `master` or `null` selects master),
- reach the **server-global** surfaces (see below).

The selection is stored on the super-admin's session, so every subsequent listing, action, and statistic is scoped to the organization currently selected. Switching to Acme and creating a token puts that token in Acme; switching back to master hides it again.

A **named user** (one created through the *Users* page) is *pinned* to the organization it was created in. It cannot switch organizations and has no visibility outside its own, even with the `admin` role, which grants full control *within* that organization only.

## What is isolated

Per **effective organization**, a named user's own org, or the org the super-admin has selected, the following are scoped so one organization never sees another's:

- **Tunnel clients**, the live view, topology, and connected-client count.
- **API tokens**, listing, creation, editing, and revocation (a token id from another org is treated as not-found).
- **Dashboard users**, listing, creation, editing, deletion, and admin TOTP reset.
- **Live sessions**, the *Active sessions* list, per-session revoke, and "sign out everywhere else".
- **Traffic**, the recent-requests log, the live SSE stream, and the request inspector / replay.
- **Statistics**, the counters, "today", the activity and history charts, and the per-token / per-hostname breakdown all reflect the org's own traffic.
- **Uptime / SLA** and **per-stage latency**, only the org's own services.
- **Webhooks**, definitions, the delivery log, and redelivery; a webhook fires **only** for events in its own organization.
- **Maintenance mode**, a hostname can be put into maintenance only by the organization whose clients serve it, and each flag is visible and clearable only within that org.
- **Share links**, can only be minted for a hostname the caller's own organization serves.
- **Audit log**, each event records the organization it belongs to; the log shows only the caller's org's events.

## What stays server-global (master-only)

A few things are properties of the *server*, not of any one tenant, and are reserved for the master super-admin:

- **Server settings** (`/aperio/api/settings`), one runtime configuration for the whole process.
- **Export / import** (`/aperio/api/export`, `/aperio/api/import`), a whole-server backup that spans every organization.
- **Prometheus metrics** (`/aperio/metrics`), the server-wide grand totals for operators (guarded by its own metrics token).

Where a server-global feature's data *can* be attributed to an organization, the request counters, for instance, each organization still sees its own slice; only the cross-organization grand total is master-only.

## How a client joins an organization

A tunnel client belongs to the organization of the **token** it authenticates with. Mint a token while a child organization is selected, hand that token to the client (`APERIO_SERVER_TOKEN` (yaml `server.token`)), and the client, and all of its traffic, is attributed to that organization. The master token always belongs to master. See [Tokens & Authentication](tokens-and-auth.md) for how tokens are scoped and issued.

## Per-organization quotas

Each child organization can carry quotas, max concurrently-connected clients, dynamic tokens, dashboard users, and proxied bytes per calendar month, set from the dashboard (Organizations → the gauge icon) or `PUT /aperio/api/orgs/{id}/quota`. They are enforced at the point of creation (token/user create, client connect) and, for the monthly byte cap, on each proxied request against the org's current-month usage. `GET /aperio/api/orgs/{id}/usage` returns current-month usage against the quota and emits an `org_usage` webhook a billing system can consume.

## Per-organization OIDC (SSO)

An organization can bring its own identity provider. Configure its issuer, client id/secret, and allowed emails (`PUT /aperio/api/orgs/{id}/oidc`, or the OIDC panel in the org's quota dialog), then its members sign in at `/aperio/oidc/login?org=<id>`. The resulting session is **bound to that organization**, the user is an admin *within* their org (their tokens, users, and traffic) but never the master super-admin, and cannot switch to other orgs. Organizations without an override fall back to the global `APERIO_OIDC_*` (yaml `oidc_*`) settings.
