# Ephemeral Tunnels (CI & Preview Environments)

Ephemeral tunnels turn Aperio into a preview-environment backend: one API call mints a **short-lived, hostname-scoped token**, a client connects with it, and the hostname goes live, ideal for per-PR previews.

## The API

`POST /aperio/api/tunnels` authenticates with the master token in a header (no browser login) and works even when the dashboard is disabled:

```bash
curl -X POST https://tunnel.example.com/aperio/api/tunnels \
  -H "Authorization: Bearer $APERIO_SERVER_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name": "pr-123", "hostname": "pr-123.example.com", "ttl_seconds": 3600}'
# → {"id": "…", "hostname": "pr-123.example.com", "url": "https://pr-123.example.com",
#    "token": "apr_…", "expires_at": 1700000000}
```

- Omit `hostname` to get a **random subdomain** (requires `APERIO_RANDOM_SUBDOMAIN` (yaml `random_subdomain`) on the server).
- `ttl_seconds` defaults to 1 hour and is capped at 7 days, the TTL is the safety net if cleanup never runs.
- `allowed_ips` restricts which source IPs may connect with the minted token.
- The token's hostname is **auto-bound** on connect: run the client with just the server URL, the token, and the target.
- `DELETE /aperio/api/tunnels/:id` revokes the token (same auth), call it from your CI cleanup step.

Provisioning appears in the audit log and is delivered to webhooks as `tunnel_created` / `tunnel_deleted`.

## GitHub Action

[`aperio-tunnel-action`](../aperio-tunnel-action/) wraps the whole flow: it provisions a tunnel, runs the `aperio-client` container for the rest of the job, and revokes the token when the job finishes.

```yaml
- name: Open tunnel
  id: tunnel
  uses: co3moz/aperio/aperio-tunnel-action@master
  with:
    server-url: https://tunnel.example.com
    server-token: ${{ secrets.APERIO_SERVER_TOKEN }}
    port: 3000
    hostname: pr-${{ github.event.number }}.example.com

- run: echo "Preview at ${{ steps.tunnel.outputs.url }}"
```

See the [action's README](../aperio-tunnel-action/README.md) for all inputs and outputs.

## Keeping previews out of search engines

Preview URLs are public by default, and crawlers do find them. With `APERIO_PREVIEW_NOINDEX=1` (yaml `preview_noindex`) (or the *Noindex preview hosts* toggle in the dashboard settings) every service reached through its **random subdomain** answers with `X-Robots-Tag: noindex, nofollow` and a disallow-all `/robots.txt` served by the server itself. Explicitly named hostnames (like the `pr-123.example.com` above) are considered deliberate and are not marked, protect those with the visitor password or OIDC if they should stay private.

## Runnable examples

Copy-and-adapt config pairs for this topic:

- [`s_random_subdomain`](examples/s_random_subdomain/): preview subdomains
