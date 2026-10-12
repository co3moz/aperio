# Single sign-on

> **Concept:** [Tokens & Authentication](../../tokens-and-auth.md).


Put an identity-provider sign-in in front of proxied services with `access: {method: sso}` in the server's settings. Aperio checks the verified email against `allowed_emails`, on every request, and lets the visitor in to the hostname they signed in on. It is the visitor's sign-in alone: it opens the sites and nothing of Aperio, and no dashboard user or grant is involved.

Register an OAuth client at your issuer with the redirect URI `https://<hostname>/aperio/sso/callback` for each hostname the gate covers, here `https://app.example.com/aperio/sso/callback`. Discovery is fetched from `<issuer>/.well-known/openid-configuration` on the first sign-in, through the server's outbound fence. To gate only some hostnames, write the same `access` on a [site](../../configuration.md#sites) instead of on the server.

The dashboard's own OIDC sign-in (`oidc:`, commented out in `server-init.yaml`) is a separate thing: it lets operators in with the roles their records give them, see [OIDC / SSO](../../configuration.md#oidc--sso-oidc).

> **Before running:** Replace the issuer, client ID, secret and allowed emails in `server-init.yaml`, and start the server once with `APERIO_INIT_FILE=server-init.yaml` (or set the server's `access` under **Settings** in the dashboard). Register the callback URL with your provider. The sample Google issuer cannot use the placeholder client credentials.

Services a client declares `access: public` still bypass the gate (token-permitting), which is handy for webhooks and status pages.
