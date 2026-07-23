# Share Links

> **Concept:** [Share Links](../../share-links.md).

Hand out **temporary, scoped access to a gated site without creating an account**: send a URL, and the recipient is in until it expires. Share links only make sense in front of a gated site, so this server puts a visitor login on everything proxied (`server_auth`); an already-open site needs no link.

## Mint one

From the dashboard's *Share Links* section, or over the API with an operator/admin session:

```bash
curl -b cookies.txt -X POST -H 'Content-Type: application/json' \
  --data '{"hostname":"app.example.com","path":"/docs","ttl_seconds":86400}' \
  https://tunnel.example.com/aperio/api/share
```

You get back a URL like `https://app.example.com/docs?aperio_share=...`. Opening it validates the signed token, redirects to the clean URL, and drops an `aperio_share` cookie (HttpOnly, expiring with the token) that lets the recipient through, including the page's WebSockets. Paths outside the granted scope still hit the login page, and the internal cookie is stripped before requests reach your backend.

## Notes

- **Stateless.** The signing key is derived from the master token, so links survive restarts and there is no table to lose. They cannot be revoked one by one; they expire. Rotating `server_token` invalidates every outstanding link at once.
- **Scope tightly.** Anyone holding a link is in until it expires, so prefer a narrow `path` and the shortest `ttl_seconds` that does the job (presets in the dashboard run from 30 minutes up to a month, plus never-expires).
- Every mint is recorded in the audit log as `share_created` and emitted to webhooks.
