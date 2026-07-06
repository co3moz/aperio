# Share Links

When a proxied site sits behind a visitor password or OIDC, share links let you hand out **temporary access without creating accounts** — send a URL, and the recipient is in until it expires.

## How it works

The dashboard's *Share Links* section generates a URL like:

```
https://app.example.com/docs?aperio_share=eyJob3N0IjoiYXBwLuKApiJ9.9f2c…
```

The token is JWT-style — `base64url(claims).base64url(HMAC-SHA256)` — carrying the hostname, an optional path prefix, and an expiry. The default lifetime is 3 days; the dashboard offers presets from 30 minutes up to 1 month, plus a never-expires option.

Opening the link validates the token, redirects to the clean URL, and sets an `aperio_share` cookie (`HttpOnly`, `SameSite=Lax`, expiring with the token) that authorizes subsequent requests — including the page's WebSockets. Paths outside the granted scope still redirect to the login page, and the internal cookie is stripped before requests reach your backends.

## Stateless by design

Nothing is stored server-side. The signing key is derived from the master token, so:

- Links survive server restarts — there is no table of issued links to lose.
- Links cannot be revoked individually — they simply expire.
- Rotating `APERIO_SERVER_TOKEN` invalidates **all** outstanding links at once.

Because anyone holding a link has access until it expires, scope links tightly: restrict them to a path prefix where possible and pick the shortest lifetime that does the job.

## Auditing

Every link creation is recorded in the audit log as `share_created` and emitted to webhooks, so you always know what was shared, when, and by whom.
