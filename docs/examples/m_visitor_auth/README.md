# Visitor Authentication

Three ways to control who may reach proxied services, shown together:

- **Server-wide gate**, `server_auth: user:password` puts a login form in front of *all* proxied traffic.
- **Client-set override**, a service's `auth: user:password` supersedes the server's gate for that service (only the client's credentials work there; master and dashboard passwords always do). A successful login is scoped to that hostname. Needs a token that permits it (master always does); the server can veto all client-set gates with `ignore_client_auth: true`.
- **Public opt-out**, `public: true` skips the visitor gate entirely for routes served exclusively by this client (same token permission).

The client below exposes a staff app behind its own login and a status page with no login, while everything else stays behind the server-wide password. See [Tokens & Authentication](../../tokens-and-auth.md).
