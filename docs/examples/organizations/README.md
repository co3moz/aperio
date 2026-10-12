# Organizations

A client whose token was minted in a child organization, so it and its traffic belong to that tenant. Create the organization and the token first:

```bash
aperio-client api org create --name acme
aperio-client api org select <acme-id>
aperio-client api token create --name acme_app --hostname app.acme.example.com --allow-public
```

Put the token in `aperio.yaml`. See [Organizations](../../tokens-and-organizations.md#organizations).
