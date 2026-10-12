# Visitor Authentication

> **Concept:** [Tokens & Authentication](../../tokens-and-auth.md).


Three ways to control who may reach proxied services, shown together:

- **The server's own gate**, `access: {method: basic, users: user:password}` in the server's settings puts a login form in front of every route nothing else declares.
- **A client's gate**, a service's `access:` (here `{method: basic, users: ...}`) supersedes the server's own for that service (only the client's credentials work there; master and dashboard passwords always do). A successful login is scoped to that hostname. Needs a token that permits it (master always does).
- **Public opt-out**, `access: public` skips the visitor gate entirely for routes served exclusively by this client (same token permission).

Above all three sits a **site's `access`** in the server's settings: the operator's word on a hostname, which wins over what any client serving it declares, so the server can keep sole control of a hostname without switching off every client's gate.

The client below exposes a staff app behind its own login and a status page with no login, while everything else stays behind the server-wide password. See [Tokens & Authentication](../../tokens-and-auth.md).
