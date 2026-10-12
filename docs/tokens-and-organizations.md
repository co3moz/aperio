# Tokens and organizations

Who may open a tunnel, who may administer the server, and how one server is shared between tenants. Who may *visit* a site is [Access](access.md).

## The master token

`APERIO_SERVER_TOKEN` is root. It authenticates clients, signs in to the dashboard as the built-in admin `aperio`, and opens every API. Keep it behind TLS, and give clients scoped tokens instead.

## Tunnel tokens

![The API Tokens page: scoped credentials with their permissions, limits and expiry](images/dashboard-tokens.png)

Minted on the dashboard's *API Tokens* page or with `aperio-client api token create`, a token is scoped and revocable. The secret is shown once and stored as a SHA-256 hash.

| Field | Effect |
| --- | --- |
| Hostnames | Hostnames the token may bind (`*` = any). A specific entry is bound on connect without the client naming it. |
| Paths | Path binds it may claim. |
| Allowed IPs | Addresses that may connect with it. |
| Lifetime | Expiry. A `token_expiring` event fires when less than `security.token_expiry_warning` is left. |
| Rate limit, daily quota | Requests per second and bytes per day served through it; past either, `429`. |
| `max_connections` | Lowers `limits.connections_per_service` for clients using it. |
| May publish public services | Lets a client declare `access:`, `public` included. |
| `allow_bind` | May bind the [tunnels](tunnels.md) of other clients in its organization. |
| `allow_server_side` | May ask for [`server_side:`](configuration.md#reaching-the-target-from-the-server-server_side). |
| `allow_otel` | May send OpenTelemetry exports through the server's bridge. |

A bind the token does not permit is ignored and the client is told. Editing a token keeps its secret; revoking it drops its connections at once.

**Refresh.** A token with a lifetime can extend itself while in use: `POST /aperio/api/tokens/refresh` with the token as `Authorization: Bearer` resets its expiry to now plus its TTL.

**Rotation.** `POST /aperio/api/tokens/{id}/rotate` with `{"grace_seconds": 86400}` issues a new secret and keeps the old one working for the grace period; `0` cuts it off at once. Identity, scope and live connections are unchanged.

**Pinning.** `security.token_pinning: true` binds each token to the first device that uses it. A client announces a device key, generated on its first run and kept beside its id when it reads a config file; a container gives one with `APERIO_DEVICE_KEY`. A later connection with a different or missing key is refused and raises `token_pin_mismatch`. To move a token to another machine, carry the key or rotate the token.

## Signing in to the dashboard

**Users.** Named users are created on the *Users* page, each with a role: **viewer** reads, **operator** runs day-to-day operations (clients, tokens, webhooks, maintenance, share links), **admin** also manages users and public listeners. The server's settings take the server admin, [below](#grants-and-the-server-admin). Passwords are Argon2id hashes. Roles are enforced by the server, not just hidden in the UI. Sessions last 24 hours and survive a restart.

**Two-factor.** A named user enrolls an authenticator from the sidebar and keeps eight single-use recovery codes. An admin can reset a user's second factor. The built-in `aperio` account has no user record and cannot enroll.

**Passkeys.** With `dashboard.webauthn.origin` set to the dashboard's public URL, users register passkeys and sign in without a password. Set `dashboard.webauthn.rp_id` to a parent domain to make one passkey cover several hostnames.

**OIDC.** `oidc.issuer`, `oidc.client_id` and `oidc.client_secret` add a sign-in through your identity provider, for operators. A login is matched to a user record by email and gets that record's grants; an email with no record gets `oidc.default_grants`, which is empty, so it is refused until someone grants it a role. A directory can own the grants: `oidc.groups_claim` names the claim and `oidc.group_grants` maps each group to `<org>:<role>`, applied at every login. Removing someone from a group takes effect at their next login; disable the record to end their sessions at once.

`dashboard.enabled: false` turns the dashboard off; the API stays.

## Admin keys

Scripts use admin keys instead of sessions: `aperio-client login` mints one for the machine, and `aperio-server admin create-key` mints one on the server. A key has one organization (`*`, `master`, or a child) and one role, and is sent as `Authorization: Bearer`. Only the server admin mints keys.

## Organizations

Organizations turn one server into isolated tenants. Each has its own clients, tokens, users, traffic, statistics, webhooks, audit trail and maintenance flags, and one never sees another's.

- **Master** is the organization everything belongs to unless created elsewhere, including the master token's clients.
- **A child** is created by the server admin on the *Organizations* page or with `aperio-client api org create --name acme`. Its name is a [handle](configuration.md#names); `custom_name` is the display name. It can be deleted once it has no users or tokens.
- **A client belongs to its token's organization.** Mint a token with the organization selected and hand it to the client.

### Grants and the server admin

A user holds grants, `{org, role}` pairs, where `org` is a child, `master`, or `*` for every organization. A specific entry beats `*`: `{*: viewer, acme: operator}` is Operator in Acme and Viewer elsewhere.

**The server admin is `*: admin`**, and only that grant reaches the server-wide surfaces: settings, organizations, admin keys, port allocations, export and import. The built-in `aperio` account always is one. `master: admin` administers master and nothing more.

A user with grants in several organizations is created from master and switches between them with the sidebar's organization picker. Giving or removing a role in an organization takes Admin there; giving `*` takes the server admin. Every change is audited (`user_grant_added`, `user_grant_removed`).

```bash
aperio-client api user create --username carol --password - \
  --grant <acme-id>:admin --grant <beta-id>:viewer
```

A dashboard session also carries its holder past the visitor gate, on the hostnames their organizations serve.

### Fences, panels and quotas

**A hostname fence** limits the hostnames an organization may claim: `acme.com` (exact), `*.acme.example.com` (any subdomain, not the name itself), or `*-pi.acme.com` (one label). It is checked at token creation, at every client connect, for ephemeral tunnels, maintenance and share links. Without one, a `*` token in the organization may bind any hostname on the server. Master is never fenced; a random subdomain is exempt.

```bash
aperio-client api org hostnames <id> --hostname "acme.com,*.acme.example.com"
```

**A panel hostname** is a name inside the fence whose root is the organization's own dashboard, with a login that admits only people whose grants reach it: `aperio-client api org panel <id> --hostname aperio.acme.example.com`. The server's own is `dashboard.hostname`. A panel serves the dashboard and nothing else.

**`dashboard.fenced_login: true`** makes the login on a fenced hostname admit only that organization's people, and a session work only on the hostname it was made on. Turn it on after giving each fenced organization a panel, or its people cannot sign in at the server's own name.

**Quotas** cap an organization's connected clients, tokens, users and monthly bytes: `PUT /aperio/api/orgs/{id}/quota`. `GET /aperio/api/orgs/{id}/usage` reports usage and emits an `org_usage` webhook for billing.

**Its own identity provider.** `aperio-client api org oidc <id> --issuer ... --client-id ... --client-secret ... --allowed-email '*@acme.com'` gives an organization its own OIDC sign-in at `/aperio/oidc/login?org=<id>`. Its users live in that organization and act only there. `--default-role` (default `admin`) and `--group-grant <group>=<role>` set what they get.

**Public listeners** follow the role: viewers read them, operators enable, disable and drain them, admins create and delete them, within the port allocation the server admin gives the organization. See [Public listeners](tunnels.md#public-listeners).

## Examples

[`organizations`](examples/organizations/).
