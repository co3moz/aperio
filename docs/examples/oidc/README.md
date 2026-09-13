# OIDC / SSO

> **Concept:** [Tokens & Authentication](../../tokens-and-auth.md).


Put an identity-provider login (Google, Keycloak, Authentik, …) in front of everything the tunnel serves. Unauthenticated visitors are redirected to the provider; after login, the verified email is checked against the allowlist, exact addresses, `*@domain`, or `*`. Sessions last 24 h. An OIDC login is matched to a dashboard user record by email and takes the grants on it, so the first login of an email nothing reaches is refused; either set `oidc_default_grants: master:admin` in the server config for the "every allowed email is an admin" behaviour, or pre-create the user and grant it a role.

Register an OAuth client at your issuer with redirect URI `https://tunnel.example.com/aperio/oidc/callback`. Discovery is fetched from `<issuer>/.well-known/openid-configuration` at startup, and a misconfigured SSO setup is a **fatal error**, the server refuses to start rather than silently serving an unprotected proxy.

Services a client declares `public: true` still bypass the gate (token-permitting), which is handy for webhooks and status pages.
