# OIDC / SSO

> **Concept:** [Tokens & Authentication](../../tokens-and-auth.md).


Put an identity-provider login in front of proxied services. Aperio checks the verified email against `oidc_allowed_emails` and then applies the matching dashboard user's grants. A new email with no grants cannot sign in. Create a user with the required grant first, or configure `oidc_default_grants` deliberately; see [OIDC / SSO](../../configuration.md#oidc--sso).

Register an OAuth client at your issuer with redirect URI `https://tunnel.example.com/aperio/oidc/callback`. Discovery is fetched from `<issuer>/.well-known/openid-configuration` at startup, and a misconfigured SSO setup is a **fatal error**, the server refuses to start rather than silently serving an unprotected proxy.

> **Before running:** Replace the issuer, client ID, secret, allowed emails, and callback URL in `aperio-server.yaml`. Register that callback URL with your provider. The sample Google issuer cannot use the placeholder client credentials.

Services a client declares `public: true` still bypass the gate (token-permitting), which is handy for webhooks and status pages.
