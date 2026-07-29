# Headers

> **Concept:** [Configuration](../../configuration.md).


Header add/remove rules exist on both sides of the tunnel and compose:

- **Client `headers:`**, `request` rules edit what the local backend receives, `response` rules what the visitor receives. Also available per `services:` entry (the entry replaces the top-level section entirely when set).
- **Server `headers:`**, the server-wide counterpart, applied to every proxied request across all services. Response edits happen before the response cache and the request inspector see the response, so all views agree.

`add` sets a header (replacing any existing value of the same name); `remove` strips names case-insensitively. Hop-by-hop and tunnel-critical headers (`Connection`, `Upgrade`, `Sec-WebSocket-*`, …) stay managed by Aperio regardless, and WebSocket upgrades pass through untouched. Config file only (no CLI/env form); hot-reload applies edits within ~5 s.

Per entry, an entry's `headers:` section **replaces** the top-level one entirely (no merging), so each service controls its own edits — below, the web app takes the shared defaults while the API strips different headers and tags its responses.

The server-side `headers:` still applies on top of everything, across all services.
