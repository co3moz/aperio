# OIDC / SSO

Put an identity-provider login (Google, Keycloak, Authentik, …) in front of everything the tunnel serves. Unauthenticated visitors are redirected to the provider; after login, the verified email is checked against the allowlist — exact addresses, `*@domain`, or `*`. Sessions last 24 h, and OIDC logins act as dashboard admins.

Register an OAuth client at your issuer with redirect URI `https://tunnel.example.com/aperio/oidc/callback`. Discovery is fetched from `<issuer>/.well-known/openid-configuration` at startup, and a misconfigured SSO setup is a **fatal error** — the server refuses to start rather than silently serving an unprotected proxy.

Services a client declares `public: true` still bypass the gate (token-permitting), which is handy for webhooks and status pages.
