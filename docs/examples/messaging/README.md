# Messages between clients

`aperio.yaml` reacts: it subscribes to `deploy/#`, runs `./deploy.sh` for `deploy/web`, and opens both local faces. `aperio-publisher.yaml` only publishes, with a token scoped to `deploy/#`. Run the reacting side from this folder, then:

```bash
curl -X POST 'http://127.0.0.1:1888/publish?topic=deploy%2Fweb' --data 'v1.9.2'
```

See [Messages between clients](../../integrations.md#messages-between-clients).
