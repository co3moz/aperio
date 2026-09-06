# Organizations (Multi-Tenancy)

Organizations turn one Aperio server into several isolated tenants. Each organization has its own tunnel clients, API tokens, dashboard users, traffic, statistics, webhooks, audit trail, and maintenance flags, and the members of one organization never see or touch another's. A named admin who signs in lands in a self-contained view of their own organization and nothing else.

## The master organization

There is always exactly one implicit **master** organization. Everything created without an organization belongs to it: the built-in `aperio` admin (who signs in with the master token), the master token's own tunnel clients, and any tokens or users minted while master is selected. Master is not a stored record, internally it is simply "no organization" (`org_id: null`); the API refers to it by the reserved id `master`.

## Child organizations

Child organizations are created from master by the super-admin. Once a child organization is selected, everything you create, tokens, users, and (through those tokens) tunnel clients, belongs to it. Its members see only its resources; a token minted under a child organization only appears there, and the clients that authenticate with it are attributed to it everywhere (traffic log, live view, stats, uptime, topology).

Child organizations are managed through the dashboard's **Organizations** page or the API:

```bash
# From anywhere, against the server's admin API.
# List organizations (master + children, with per-org user/token counts)
curl -b cookies.txt https://tunnel.example.com/aperio/api/orgs

# Create a child organization (optionally fenced to its own hostnames)
curl -b cookies.txt -X POST -H 'Content-Type: application/json' \
  --data '{"name":"acme","custom_name":"Acme Inc.","hostnames":["acme.com","*.acme.example.com"]}' \
  https://tunnel.example.com/aperio/api/orgs

# Delete an (empty) child organization, refused while it still has users or tokens
curl -b cookies.txt -X DELETE https://tunnel.example.com/aperio/api/orgs/<id>
```

An organization's `name` is a handle, `a-z`, `0-9` and `_`, unique and with `master` reserved; anything a person should read goes in `custom_name`, which can be changed at any time (see [Names](configuration.md#names)). A child organization can only be deleted once all of its users and tokens are removed, so nothing is silently orphaned. All of these endpoints require Admin in the master organization (below).

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
aperio-client api org create --name acme --hostname acme.com,*.acme.example.com
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

## Panel hostname

`/aperio` answers on every hostname the server serves, and stays that way. A **panel hostname** is a second door: a name whose root *is* the dashboard, so nobody types `/aperio`. The server's own is `dashboard.hostname` (`APERIO_DASHBOARD_HOSTNAME`), `panel.example.com` for the super-admin. An organization picks its own, a name **inside its hostname allowlist**, and gets its own dashboard at the root of it:

```bash
# An Admin of the organization (the super-admin reaches it through `*`)
curl -b cookies.txt -X PUT -H 'Content-Type: application/json' \
  --data '{"hostname":"aperio.acme.example.com"}' \
  https://tunnel.example.com/aperio/api/orgs/<id>/panel

aperio-client api org panel <id> --hostname aperio.acme.example.com   # omit --hostname to clear
aperio-client api org create --name acme --hostname '*.acme.example.com' --panel-hostname aperio.acme.example.com
```

- **It is fenced like a bind.** A panel is a name the tenant claims, so it has to be inside the organization's allowlist, and an unfenced organization cannot set one: there is nothing to check it against. A fence that stops covering the panel takes it away.
- **It serves the panel and nothing else.** No token, client declaration or dashboard override may bind a panel hostname, whoever asks, master included; a client serving the name when it becomes a panel is dropped, as when a fence changes. Two organizations cannot share one, and none can take the server's own.
- **Its login is the organization's.** On an organization's panel the login form, a passkey and an OIDC login admit an account whose grants reach that organization, and the master super-admin, nobody else. The refusal is byte for byte the wrong-password answer, so the panel does not say which names exist elsewhere. The login page shows the organization's display name. The server's own panel admits everyone, like any other hostname.
- **`/aperio/...` keeps resolving on the panel too**, which is what `aperio-client api` needs, and the browser lands on `/` after signing in.
- **`aperio.<domain>` is a suggestion, not a rule.** When the fence holds exactly one `*.<domain>` entry, the Organizations page offers `aperio.<domain>` for the panel, one click to accept and editable; the server routes only on the stored value, so a fence edit never opens a login door nobody wrote down.

What an operator hits in the first ten minutes: the panel name needs a certificate like any bind; a passkey is bound to the origin it was registered on, so `APERIO_WEBAUTHN_RP_ID` has to be a parent domain covering both names, which an organization's own domain cannot share with the server's; and an OIDC provider's registered callback has to include the panel's.

## Fenced login

`/aperio` answers on every hostname, and by default so does its login: Beta's user can sign in at `acme.com/aperio`. Nothing leaks, the session is Beta's, but Acme's hostname is accepting another tenant's credentials, and a password list can be run against every organization's users from any hostname. `dashboard.fenced_login: true` (`APERIO_DASHBOARD_FENCED_LOGIN=1`) closes that:

- **On a hostname inside an organization's allowlist**, the login form, a passkey and an OIDC login admit an account whose grants reach that organization, and anyone reaching master, since master is unfenced everywhere and `*` reaches it. Everyone else gets the wrong-password answer, byte for byte, so the hostname does not say which names exist elsewhere.
- **A hostname no fence claims is master's**: it admits master's people and the users of organizations that have no fence, who have no hostname to be sent to. An unfenced organization keeps today's behaviour everywhere.
- **A random subdomain** is in no fence, so it follows the organization currently serving it.
- **A session is good only on the hostname it was minted on.** The browser already keeps sessions apart per host through the `__Host-` cookie; this is the server doing the same, so a cookie value lifted from one hostname opens nothing elsewhere. A person who administers two tenants signs in on each, or on a [panel](#panel-hostname).
- **The login page names the organization** when the hostname is inside exactly one allowlist.

Off by default, and worth checking before turning on: a deployment whose tenants sign in at the server's own name, `tunnel.example.com/aperio`, has fenced organizations whose people would be refused there. Give those organizations a [panel hostname](#panel-hostname), or leave the fence off. An organization's panel is fenced whatever this setting says.

## OIDC: identity is the provider's, authorization is Aperio's

An OIDC login is matched to a **dashboard user record by email**. The identity provider says who this is; the record says what they may do, under exactly the [grant rules](#grants-one-user-several-organizations) above. The record is the same row a named user has, with no password: create it ahead of the first login from the Users page (*Signs in through the identity provider only*) or with `aperio-client api user create --username alice@example.com --grant <acme-id>:admin`, which is how a fleet admin wants it, the person exists with the grants already written before they sign in.

- **An email with no record** gets `oidc.default_grants` (`APERIO_OIDC_DEFAULT_GRANTS`, `<org>:<role>` entries, `*` and `master` allowed), empty by default. Empty means the login is refused with a message to ask an administrator, and a record is created anyway, with no grants, so the grant has a name to land on; it shows on the Users page as an SSO account. `master:admin` restores what every allowed email got before 0.12.0.
- **A directory that is the source of truth** maps its groups: `oidc.groups_claim` (`APERIO_OIDC_GROUPS_CLAIM`, default `groups`) names the claim, read from userinfo and then from the ID token, and `oidc.group_grants` (`APERIO_OIDC_GROUP_GRANTS`) says what each value means, `<group>=<org>:<role>`, for example `aperio-admins=master:admin, acme-ops=acme:operator, auditors=*:viewer`. At every login the map is applied to the record: a grant the map produces is written, one it produced before and no longer does is taken back, and a grant an admin wrote by hand is left alone unless the map names the same organization, where the directory wins. Every grant a login writes or removes is an audit event (`user_grant_added`, `user_grant_removed`) naming the group that caused it.
- **The honest limit:** a person removed from a group loses the access at their **next login**, not before, and a session lasts a day. A deployment that needs it sooner disables the record, which ends every session at once.
- **The allowed-emails list stays** as the gate in front of all of this: it is what keeps the provider's whole tenant from signing in at all. `*` through a group claim is still `*`, so the map is server configuration and a per-organization table cannot name it.

### Per-organization OIDC (SSO)

An organization can bring its own identity provider. Configure its issuer, client id/secret, and allowed emails (`PUT /aperio/api/orgs/{id}/oidc`, or the OIDC panel in the org's quota dialog), then its members sign in at `/aperio/oidc/login?org=<id>`. The record such a login matches or creates **lives in that organization**, so the organization's own admins manage it, and the session is **bound to that organization**: whatever else the record holds, it acts there and nowhere else, never as the master super-admin, and cannot switch. Two more fields say what the tenant's people get:

- `default_role`: what an email with no record is granted in the organization at its first login, `admin` (the default, what such a login always was), `operator`, `viewer`, or `none` for nothing until an admin grants it;
- `group_grants`: `<group>=<role>` entries, the same map as above with the organization fixed, so a tenant's directory hands out roles inside the tenant and nothing beyond.

```bash
aperio-client api org oidc <id> --issuer https://idp.acme.com --client-id ... --client-secret ... \
  --allowed-email '*@acme.com' --default-role none --group-grant acme-ops=operator --group-grant acme-admins=admin
```

It is a dashboard identity: it also carries its holder past the visitor gate, but only on hostnames their own organization serves. Organizations without an override fall back to the global `APERIO_OIDC_*` (yaml `oidc.*`) settings.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`organizations`](examples/organizations/): multi-tenancy: create, select, mint a scoped token
