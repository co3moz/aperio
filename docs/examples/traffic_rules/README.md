# Traffic rules

Per-site rate limits, firewall rules, a fallback for an unserved hostname, and a hostname's own error pages, all in `server-init.yaml`. `/api/login` gets 5 requests a second while the rest of the API shares 200. See [Sites](../../configuration.md#sites).
