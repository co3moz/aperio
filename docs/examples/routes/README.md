# Client-less Routes

> **Concept:** [Routing & Load Balancing](../../routing-and-load-balancing.md).


A site in the server's `sites` setting can answer by itself, no tunnel client involved. A site matches a `hostname` (exact, a pattern like `*.example.com`, or `*` for every hostname) and optionally a `path` prefix, and gives exactly one answer:

- `redirect`, with a `url`: 302, or 301 with `permanent: true`; `preserve_path: true` appends the request path and query.
- `respond`, a fixed response with optional `status`, `content_type`, `body`.

Typical uses: vanity redirects, a "coming soon" page for a hostname whose client is not deployed yet, or a fixed `/robots.txt`. When several sites match, the most specific one that answers does. A site that answers comes before client routing, maintenance mode still wins over it, and the visitor gate does not apply (it serves operator-authored content). The client file here is an ordinary service; the answers work with or without connected clients.
