# Organizations (multi-tenancy)

> **Concept:** [Organizations](../../organizations.md).

One Aperio server, several isolated tenants. Each organization has its own tunnel clients, API tokens, dashboard users, traffic, statistics, and audit trail, and one tenant never sees another's. A tunnel client belongs to the organization of the **token** it authenticates with, so making a client "belong to Acme" is entirely a matter of which token it uses.

Nothing tenant-specific lives in the config files: organizations are created at runtime by the built-in `aperio` super-admin. The server file here is a normal master setup; the client file simply uses a token that was minted under a child organization.

## Set it up

Sign in as the super-admin (master token) to get a session cookie, then:

```bash
# 1. Create the child organization.
curl -b cookies.txt -X POST -H 'Content-Type: application/json' \
  --data '{"name":"Acme"}' https://tunnel.example.com/aperio/api/orgs

# 2. Select it, so what you create next belongs to Acme.
curl -b cookies.txt -X POST -H 'Content-Type: application/json' \
  --data '{"id":"<acme-id>"}' https://tunnel.example.com/aperio/api/orgs/select

# 3. Mint a scoped token in Acme and hand it to the client (this file's server.token).
curl -b cookies.txt -X POST -H 'Content-Type: application/json' \
  --data '{"name":"acme-app","hostnames":["app.acme.example.com"]}' \
  https://tunnel.example.com/aperio/api/tokens
```

The client connects with that token, and everywhere (live view, traffic log, stats, uptime, topology) it shows under Acme. A named admin you create in Acme signs in to a self-contained view of Acme alone, never master.

## Quotas

A child organization can carry quotas (max connected clients, tokens, users, and proxied bytes per calendar month), set from the dashboard (Organizations, the gauge icon) or `PUT /aperio/api/orgs/{id}/quota`. They are enforced at creation and, for the byte cap, on each proxied request. `GET /aperio/api/orgs/{id}/usage` reports current-month usage and emits an `org_usage` webhook a billing system can consume.
