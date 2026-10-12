# Autoscaling

The server asks your endpoint for capacity, and clients retire themselves when idle (`idle_timeout`). Replace the endpoint with one that accepts the scaling request and starts clients, and give every client the same secret:

```bash
export SCALE_SECRET="$(openssl rand -hex 32)"
```

See [Autoscaling](../../integrations.md#autoscaling).
