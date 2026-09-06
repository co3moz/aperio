# Organizations (Multi-Tenancy)

Organizations turn one Aperio server into several isolated tenants. Each organization has its own tunnel clients, API tokens, dashboard users, traffic, statistics, webhooks, audit trail, and maintenance flags, and the members of one organization never see or touch another's. A named admin who signs in lands in a self-contained view of their own organization and nothing else.

## The master organization

There is always exactly one implicit **master** organization. Everything created without an organization belongs to it: the built-in `aperio` admin (who signs in with the master token or the dashboard password), the master token's own tunnel clients, and any tokens or users minted while master is selected. Master is not a stored record, internally it is simply "no organization" (`org_id: null`); the API refers to it by the reserved id `master`.

## Child organizations

Child organizations are created from master by the super-admin. Once a child organization is selected, everything you create, tokens, users, and (through those tokens) tunnel clients, belongs to it. Its members see only its resources; a token minted under a child organization only appears there, and the clients that authenticate with it are attributed to it everywhere (traffic log, live view, stats, uptime, topology).

Child organizations are managed through the dashboard's **Organizations** page or the API:

```bash
# From anywhere, against the server's admin API.
# List organizations (master + children, with per-org user/token counts)
curl -b cookies.txt https://tunnel.example.com/aperio/api/orgs

# Create a child organization (optionally fenced to its own hostnames)
curl -b cookies.txt -X POST -H 'Content-Type: application/json' \
  --data '{"name":"Acme","hostnames":["acme.com","*.acme.example.com"]}' \
  https://tunnel.example.com/aperio/api/orgs

# Delete an (empty) child organization, refused while it still has users or tokens
curl -b cookies.txt -X DELETE https://tunnel.example.com/aperio/api/orgs/<id>
```

Organization names are unique (case-insensitive); `master` is reserved. A child organization can only be deleted once all of its users and tokens are removed, so nothing is silently orphaned. All of these endpoints require the master super-admin (below).

## The super-admin and switching organizations

The built-in `aperio` admin holds every organization at Admin, `*` in the grant spelling below. Managing organizations and reaching the server-global surfaces takes **Admin in the master organization**, which the built-in account has and a named user can be granted:

- create, list, and delete child organizations,
- **switch** which organization the session is acting in, from the organization picker in the dashboard sidebar (or `POST /aperio/api/orgs/select` with `{"id": "<org-id>"}`; `master` or `null` selects master). The picker is not master's alone: any session whose grants reach more than one organization gets it, for exactly those organizations,
- reach the **server-global** surfaces (see below).

The selection is stored on the session, so every subsequent listing, action, and statistic is scoped to the organization currently selected. Switching to Acme and creating a token puts that token in Acme; switching back to master hides it again.

A **named user** (one created through the *Users* page) holds a list of **grants**, one `(organization, role)` pair each. Most hold exactly one: a user created inside a child organization is granted that organization and nothing else, cannot switch, and has no visibility outside it, even with the `admin` role, which grants full control *within* that organization only. A user who needs more than one is created from master, see [Grants](#grants-one-user-several-organizations).

## Grants: one user, several organizations

Every dashboard user carries `grants`, a list of `{org, role}` pairs where `org` is a child organization id, `master`, or `*` for every organization present and future. The role the dashboard enforces is the role in the organization the session is acting in, read from the record on every request, so a grant taken away is gone at the next request, the way a disabled account's sessions grant nothing.

- **A specific entry beats `*`.** `{*: viewer, acme: operator}` reads the way it looks: Operator in Acme, Viewer everywhere else. `*: viewer` on its own is an auditor, reading the whole server and changing nothing.
- **A grant is bounded by the granter's.** Giving or taking away a role in an organization takes Admin there; giving or taking away `*` takes `*` Admin. An organization's admin cannot grant themselves a second organization, and an Admin of master runs the server without being able to mint a credential wider than their own reach.
- **Server-global surfaces take Admin in master**: settings, import and export, organization create, delete, quota and fence. What Admin in master does *not* give is the children. `master: admin` is the server; `*: admin` is the server and every tenant, the nearest thing to the built-in account. It may create other `*` users, and it never touches the built-in account or the master token.
- **A user reaching several organizations lives in master.** It is created from master, with the grant editor on the Users page or `grants` on `POST /aperio/api/users`, and only master manages it: an Acme admin resetting such a user's password or TOTP would be resetting their Beta login too. Inside a child organization the form offers the role alone, and a user created there is granted that organization only.
- **The picker opens to everyone with more than one grant.** `GET /aperio/api/session` lists the organizations a session reaches (`orgs`) with the role in each, and `POST /aperio/api/orgs/select` accepts any of them. A user granted one organization sees no picker.
- **The visitor gate admits on any granted organization's hostnames**, not only the selected one's, and Viewer is enough.
- **Admin keys follow the same reading.** A key keeps one scope, since it is minted for one purpose: a child id, `master`, or `*`, which only a holder of `*` Admin may mint (`"org_id": "*"` on `POST /aperio/api/admin-keys`, `--org '*'` on the CLI).

```bash
# From master: one user, Admin in Acme and read-only in Beta
curl -b cookies.txt -X POST -H 'Content-Type: application/json' \
  --data '{"username":"carol","password":"...","grants":[{"org":"<acme-id>","role":"admin"},{"org":"<beta-id>","role":"viewer"}]}' \
  https://tunnel.example.com/aperio/api/users

# Replace the list later; every grant added or removed is an audit event
# (user_grant_added, user_grant_removed) naming who made the change
curl -b cookies.txt -X PUT -H 'Content-Type: application/json' \
  --data '{"grants":[{"org":"<acme-id>","role":"admin"}]}' \
  https://tunnel.example.com/aperio/api/users/<id>

# The same from the command line
aperio-client api user create --username carol --password - --grant <acme-id>:admin --grant <beta-id>:viewer
aperio-client api user update <id> --grant <acme-id>:admin
```

**Upgrading.** A user record from before grants existed is read as one grant in its home organization, except an Admin of master, which is read as `*: admin`: that is exactly what the record could do before, and narrowing it on an upgrade would lock out whoever runs the server. The same goes for an Admin key of master. Each one is written down in the audit log at the first start (`grants_widened_on_upgrade`), and the Users page shows a notice while any user still carries `*`, so the operator narrows them by hand and knows when they are done.

## What is isolated

Per **effective organization**, the one selected on the session out of those the caller's grants reach (a user granted one organization is simply in it), the following are scoped so one organization never sees another's:

- **Tunnel clients**, the live view, topology, and connected-client count.
- **API tokens**, listing, creation, editing, and revocation (a token id from another org is treated as not-found).
- **Dashboard users**, listing, creation, editing, deletion, and admin TOTP reset.
- **Live sessions**, the *Active sessions* list, per-session revoke, and "sign out everywhere else".
- **Traffic**, the recent-requests log, the live SSE stream, and the request inspector / replay.
- **Statistics**, the counters, "today", the activity and history charts, and the per-token / per-hostname breakdown all reflect the org's own traffic.
- **Uptime / SLA** and **per-stage latency**, only the org's own services.
- **Webhooks**, definitions, the delivery log, and redelivery; a webhook fires **only** for events in its own organization. Note that the *destination* is not org-scoped: an org operator's webhook URL is called by the server, from the server's network. Where tenants are not fully trusted, fence that with the server-wide [outbound policy](threat-model.md).
- **Maintenance mode**, a hostname can be put into maintenance only by the organization it belongs to, never one another organization's client is serving, and each flag is visible and clearable only within that org.
- **Share links**, can only be minted for a hostname the caller's own organization serves.
- **Audit log**, each event records the organization it belongs to; the log shows only the caller's org's events.
- **The visitor gate**, a dashboard session gets its holder past the visitor login only on hostnames their own organization serves, by the same rule as maintenance and share links: covered by the fence *and* not currently served by another organization's client. Master sessions are unfenced here as everywhere else.

## What stays server-global (master-only)

A few things are properties of the *server*, not of any one tenant, and are reserved for the master super-admin:

- **Server settings** (`/aperio/api/settings`), one runtime configuration for the whole process.
- **Export / import** (`/aperio/api/export`, `/aperio/api/import`), a whole-server backup that spans every organization. Exporting *without* the `organizations` section keeps only master's rows, since a token, user or statistics slice whose organization does not exist on the target server would be an orphan.
- **Prometheus metrics** (`/aperio/metrics`), the server-wide grand totals for operators (guarded by its own metrics token).

Where a server-global feature's data *can* be attributed to an organization, the request counters, for instance, each organization still sees its own slice; only the cross-organization grand total is master-only.

## How a client joins an organization

A tunnel client belongs to the organization of the **token** it authenticates with. Mint a token while a child organization is selected, hand that token to the client (`APERIO_SERVER_TOKEN` (yaml `server.token`)), and the client, and all of its traffic, is attributed to that organization. The master token always belongs to master. See [Tokens & Authentication](tokens-and-auth.md) for how tokens are scoped and issued.

## Per-organization quotas

Each child organization can carry quotas, max concurrently-connected clients, dynamic tokens, dashboard users, and proxied bytes per calendar month, set from the dashboard (Organizations → the gauge icon) or `PUT /aperio/api/orgs/{id}/quota`. They are enforced at the point of creation (token/user create, client connect) and, for the monthly byte cap, on each proxied request against the org's current-month usage. `GET /aperio/api/orgs/{id}/usage` returns current-month usage against the quota and emits an `org_usage` webhook a billing system can consume.

## Per-organization hostname allowlist

An organization can be fenced to the hostnames it actually owns. Without a fence, a token carrying `*` (or no hostname permission at all) may bind **any** hostname on the server, so an org admin minting a wildcard token for their own tenant could claim a hostname belonging to another one. Give the organization a hostname allowlist and that becomes impossible:

```bash
# From anywhere, against the server's admin API.
# At creation, or later on an existing organization
curl -b cookies.txt -X PUT -H 'Content-Type: application/json' \
  --data '{"hostnames":["acme.com","*.acme.example.com"]}' \
  https://tunnel.example.com/aperio/api/orgs/<id>/hostnames

# From the command line
aperio-client api org create --name Acme --hostname acme.com,*.acme.example.com
aperio-client api org hostnames <id> --hostname "*.acme.example.com"
```

Entries take three shapes:

| Entry | Matches |
|---|---|
| `acme.com` | that hostname, and nothing under it |
| `*.acme.example.com` | any depth of subdomain, **not** `acme.example.com` itself, so list both if you want both |
| `*-pi.acme.com`, `dev-*.acme.com` | one label, around the placeholder: `raspberry-pi.acme.com` but not `a.raspberry-pi.acme.com` |

The third is for a fleet naming convention: an organization that owns every `<something>-pi.acme.com` can say exactly that instead of being handed `*.acme.com`, which is the whole domain and rather more than they own. One placeholder, in the leftmost label only, the same shape `random_subdomain` accepts. An empty list, or a single `*`, means no restriction and is the default: existing deployments are unchanged.

The fence is enforced everywhere a hostname can be claimed, not just once:

- **Token creation and editing** refuse a hostname permission outside the allowlist (`403`). A wildcard permission (`*`) stays legal, it simply means "any hostname within this organization's fence".
- **Client connect** re-checks every declared and token-granted bind, so a token minted *before* the fence existed cannot bind past it either. Rejected binds are logged and dropped, exactly like a bind the token never permitted.
- **Ephemeral tunnels** (`POST /api/tunnels`) refuse an out-of-fence hostname.
- **Dashboard bind overrides** refuse one too, the one place a bind is set with no token behind it.
- **Maintenance mode and share links** are limited to hostnames the fence admits *and* that nobody else is currently serving, so one tenant cannot 503 another's site or hand out access to it. Both halves are needed. The fence alone is not enough, because the master token is never fenced, so a master client can be serving a name inside an organization's fence; and a live client of the caller's own is not required, because putting up a maintenance page is most wanted precisely when nothing is up. So: inside your fence, and not somebody else's right now. An organization with **no** fence has no allowlist to read, so those two fall back to the older rule, one of its own clients must currently serve the hostname. The master organization is fenced by the other organizations and by nothing else: whatever no tenant claims is master's, so the super-admin can put up a maintenance page for a hostname whose client is down, and still cannot 503 a tenant's site.

A server-assigned **random subdomain** is exempt: the tenant cannot influence which name it gets, so it can never collide with another organization's hostname. The master organization is never fenced.

Set it from the dashboard in Organizations → the gauge icon → *Allowed hostnames*, or in the create dialog.

## Per-organization OIDC (SSO)

An organization can bring its own identity provider. Configure its issuer, client id/secret, and allowed emails (`PUT /aperio/api/orgs/{id}/oidc`, or the OIDC panel in the org's quota dialog), then its members sign in at `/aperio/oidc/login?org=<id>`. The resulting session is **bound to that organization**, the user is an admin *within* their org (their tokens, users, and traffic) but never the master super-admin, and cannot switch to other orgs. It is a dashboard identity: it also carries its holder past the visitor gate, but only on hostnames their own organization serves. Organizations without an override fall back to the global `APERIO_OIDC_*` (yaml `oidc_*`) settings.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`organizations`](examples/organizations/): multi-tenancy: create, select, mint a scoped token
