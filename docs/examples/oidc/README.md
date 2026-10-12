# Single sign-on

`access: {method: sso}` on the server sends visitors to an identity provider. Register `https://app.example.com/aperio/sso/callback` with the provider and replace the issuer and credentials in `server-init.yaml`. The dashboard's own OIDC sign-in, for operators, is separate and commented out. See [`sso`](../../access.md#sso).
