# Access

Who may reach a proxied route. It is written as `access`, in one grammar wherever it appears: on a client's service, on a server [site](configuration.md#sites), and as the server's own default.

```yaml
access: public                                     # anyone
access: { method: basic, users: "admin:s3cret" }   # one method
access:                                            # several: any one admits
  - method: basic
    users: [admin:s3cret, ops:hunter2]
  - method: bearer
    secret: ${API_SECRET}
```

A list is **any-of**: the first method that admits the visitor wins, which is how one route lets a person sign in and a script present a key. `public` is a word, never a password.

| `method:` | What it checks | Written by |
| --- | --- | --- |
| `none` | Nothing: deliberately open. The long form of `public`. | client or server |
| `basic` | A `user:password` login. `users:` takes one or a list. | client or server |
| `bearer` | A secret in `Authorization: Bearer`. `secret:` takes one or a list, so a key rotates by adding the new one first. | client or server |
| `jwt` | A token the visitor already holds, verified against `jwks_url:` or `hmac_secret:`. | client or server |
| `forward` | An endpoint you run: `2xx` admits, anything else is the answer the visitor gets. | client (`via: client`) or server |
| `sso` | A sign-in at an identity provider, for this hostname only. | server |
| `aperio` | This server's own dashboard sign-in, reaching the hostname's organization. | client or server |

A `method:` this build does not know stops the start, so a gate is never silently absent. Anything the set leaves out (LDAP, SAML) goes behind `forward`.

## Who has the last word

1. **A site's `access`** on the server decides for its hostname, whatever the clients serving it declare. A client overruled this way is told so in its own log.
2. **A client's `access:`** decides for its own service. Declaring any gate, `public` included, needs the token's *may publish public services* permission; without it the declaration is ignored and the client is told.
3. **The server's `access`** decides for a route nothing above speaks for.

**One route, one policy.** Clients serving the same hostname and path have to declare the same `access`, `allowed_ips` and `denied`. A client that disagrees with one already serving the route is kept out of it, and both logs say which setting differs; it joins as soon as they agree.

**Closed by default.** The server's `access` is `deny` unless set: a route is served because something said so. An undeclared route answers like an unclaimed hostname, and like a visitor `allowed_ips` turns away, `504 Nothing is published at this address`, so its existence does not leak; the client's log says which service was refused and what to write. Two exceptions: the server's own signed-in users reach it, and a service with `cache.offline` answers from its cache. Set the server's `access` to `public` to serve undeclared routes to everyone, or to a method to gate them.

`allowed_ips:` on a service limits its visitors to IPs or CIDRs, checked by the server before dispatch.

## The methods

### `basic`

A login form. A session from the server's own `basic` gate covers every hostname that gate covers; one from a client's gate covers that hostname only. Neither opens anything of Aperio.

### `bearer`

```yaml
access: { method: bearer, secret: [new-key-xxxxxxxxxxxx, old-key-xxxxxxxxxxxx] }
```

The secret is compared verbatim and must be at least 16 characters. `query: true` also accepts it as `?aperio_token=<secret>`, for callers that cannot set a header; it is off by default because a query string reaches logs and `Referer`. The parameter is stripped before the backend, and a page load carrying one is redirected to the clean URL with a short-lived cookie.

### `jwt`

```yaml
access:
  - method: jwt
    jwks_url: https://accounts.example.com/.well-known/jwks.json
    issuer: https://accounts.example.com
    audience: aperio
    claims: { groups: engineering }
    cookie: CF_Authorization     # where the token is, if not Authorization: Bearer
```

One method covers Cloudflare Access, an ALB doing OIDC, and your own issuer. Keys are fetched and cached for an hour and refetched when a token names an unknown key id (at most every 30 seconds). `hmac_secret:` verifies `HS256` instead. `exp` is always required; `iss` and `aud` are checked when written, and then a token without them is refused.

### `forward`

```yaml
access:
  - method: forward
    url: http://127.0.0.1:7070/_authcheck
    request_headers: [cookie, authorization]   # the default
    response_headers: [x-auth-user]            # empty by default
    timeout: 5
    cache: 30
    via: client
```

What nginx calls `auth_request`. The endpoint gets a `GET` describing the request (`X-Forwarded-Method`, `-Proto`, `-Host`, `-Uri`, `-For`) plus the request headers you name. `2xx` admits and copies the named response headers onto the request; a visitor's own copy of those names is always dropped. Anything else is relayed to the visitor as the answer, so the endpoint can redirect to a login of its own. A timeout refuses. `cache:` remembers an admission, never a refusal, keyed on everything the check carried.

**`via: client`** asks over the tunnel, so the endpoint lives on the client's network (`localhost` is the client's). A client's `forward` must say so, because the server never dials a URL a client chose. A client answers only the checks its own `access:` declares, unless `allow_server_forward: true` lets the server write one. A connection holds at most 32 open checks, each spending from the visitor's rate limit.

Without `via: client`, the URL is called by the server and goes through its [outbound policy](reference.md#outbound).

### `sso`

```yaml
# a site, or the server's access
access:
  method: sso
  issuer: https://accounts.google.com
  client_id: xxxx.apps.googleusercontent.com
  client_secret: ${SSO_CLIENT_SECRET}
  allowed_emails: [me@corp.com, "*@team.example.com"]
```

A visitor signs in at your provider and gets a session for that hostname alone. The email is checked against `allowed_emails` on every request, so narrowing the list applies at once. Register `https://<hostname>/aperio/sso/callback` at the provider for each hostname. Sign-ins are audited as `sso_login_success` and `sso_login_denied`. This is not the dashboard's OIDC sign-in, which is for operators (see [Tokens and organizations](tokens-and-organizations.md#signing-in-to-the-dashboard)).

### `aperio`

The dashboard sign-in: a master token, a named user, a passkey or OIDC, reaching the hostname's organization. Alone it is answered `401` without a challenge for scripts; beside `bearer`, a person signs in and a script presents a key.

## How a refusal looks

A browser navigation is sent to the login page. A request with no browser is answered `401` with `WWW-Authenticate: Bearer` when the gate has `bearer` or `jwt`, and `401` without a challenge for `aperio`. A `forward` refusal is whatever the endpoint answered.

## Two kinds of session

A session either administers Aperio or views a site, and the credential that created it decides which:

| Created by | Opens |
| --- | --- |
| Master token, named user, passkey, OIDC | the dashboard and its API, and, fenced by organization, the sites |
| The server's or a site's `basic` | the hostnames that gate covers |
| A client's `access:` | that hostname |
| `sso` | that hostname, for that gate |

Credentials addressed to the gate never reach the backend: Aperio's cookies and an `Authorization` header that opened the gate are stripped.

## Telling the backend who came in

`identity_headers.visitor: true` adds two headers to a gated request: `x-aperio-visitor-how` (`session`, `bearer`, `jwt`, `forward` or `share`) and `x-aperio-visitor-id` (the email or username, or the `x-auth-user` a `forward` endpoint answered). Inbound `x-aperio-*` headers are always stripped, so a visitor cannot forge them.

## Share links

A share link gives temporary access to a gated site without an account:

```
https://app.example.com/docs?aperio_share=eyJob3N0IjoiYXBw...9f2c
```

Mint one from the dashboard's *Share Links* page or with `aperio-client api share --hostname app.example.com --path /docs --expire 1d`. It is signed with a key derived from the master token and carries the hostname, an optional path prefix and an expiry (3 days by default). The first visit sets a cookie and redirects to the clean URL. Nothing is stored, so a link cannot be revoked by itself; it expires, or every link dies when the master token changes. Each link is audited as `share_created`. Scope links to a path and keep them short.

## Examples

[`visitor_auth`](examples/visitor_auth/), [`oidc`](examples/oidc/), [`share_links`](examples/share_links/), [`allowed_ips`](examples/allowed_ips/).
