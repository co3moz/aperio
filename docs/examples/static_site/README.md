# Static Sites

> **Concept:** [Static File Serving](../../static-serving.md).


Publish local directories of static files without any backend process: `serve:` replaces `target:` on a `services:` entry and the client answers requests from the directory itself (directories serve their `index.html`). Useful for putting a `dist/` build online in one step, the yaml here is the config-file equivalent of `aperio-client --serve ./dist`.

The pair below publishes **two directories on two hostnames**, the work of two clients in a single process:

- `a.example.com` → the files under `./sites/a`
- `b.example.com` → the files under `./sites/b`

The client runs one loopback file server per distinct directory and tunnels each under its own binds. All the usual per-entry knobs (auth, cache, headers, …) apply unchanged, and one entry is just as valid as two. Mixing is fine too: a `serve:` entry can sit next to ordinary `target:` entries, so a static landing page and a proxied API can share one client.

`serve:` and `target:` are mutually exclusive within an entry, the served directory *is* the backend.
