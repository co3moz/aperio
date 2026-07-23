# Client-less Routes

A `routes:` list binds a hostname and/or path prefix directly to a server-produced answer, no tunnel client involved. Each rule matches on an exact `hostname` and/or a `path` prefix (bind semantics; first match wins, in file order) and carries exactly one action:

- `redirect`, 302, or 301 with `permanent: true`; `preserve_path: true` appends the request path and query.
- `respond`, a fixed response with optional `status`, `content_type`, `body`.

Typical uses: vanity redirects, a "coming soon" page for a hostname whose client is not deployed yet, or a fixed `/robots.txt`. Routes match before client routing, maintenance mode still wins, and the visitor gate does not apply (they serve operator-authored content). The client file here is an ordinary service, routes work with or without connected clients.
