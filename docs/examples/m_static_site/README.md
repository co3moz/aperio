# Static Sites (multi-service)

> **Concept:** [Configuration](../../configuration.md).


One client publishing **two static directories on two hostnames**, the work of two clients in a single process:

- `a.example.com` → the files under `./sites/a`
- `b.example.com` → the files under `./sites/b`

Each `services:` entry carries `serve:` instead of `target:`; the client runs one loopback file server per distinct directory and tunnels each under its own binds. All the usual per-entry knobs (auth, cache, headers, …) apply unchanged. Mixing is fine too, a `serve:` entry can sit next to ordinary `target:` entries, so a static landing page and a proxied API can share one client.
