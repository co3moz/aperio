# Behind a proxy or CDN

`proxy.trusted` lists the proxies in front, so visitor addresses are read correctly from `X-Forwarded-For`. The server listens on loopback behind them:

```bash
HOST=127.0.0.1 APERIO_SERVER_TOKEN=apr_behind_proxy_change_me APERIO_INIT_FILE=server-init.yaml aperio-server
```

See [In front of the server](../../deployment.md#in-front-of-the-server).
