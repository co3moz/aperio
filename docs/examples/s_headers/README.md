# Headers

> **Concept:** [Configuration](../../configuration.md).


Header add/remove rules exist on both sides of the tunnel and compose:

- **Client `headers:`**, `request` rules edit what the local backend receives, `response` rules what the visitor receives. Also available per `services:` entry (the entry replaces the top-level section entirely when set).
- **Server `headers:`**, the server-wide counterpart, applied to every proxied request across all services. Response edits happen before the response cache and the request inspector see the response, so all views agree.

`add` sets a header (replacing any existing value of the same name); `remove` strips names case-insensitively. Hop-by-hop and tunnel-critical headers (`Connection`, `Upgrade`, `Sec-WebSocket-*`, …) stay managed by Aperio regardless, and WebSocket upgrades pass through untouched. Config file only (no CLI/env form); hot-reload applies edits within ~5 s.

Multi-service variant: [m_headers](../m_headers/).
