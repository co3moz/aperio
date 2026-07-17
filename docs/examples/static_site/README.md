# Static Site

Publish a local directory of static files without any backend process: `serve:` replaces `target:` and the client answers requests from the directory itself (directories serve their `index.html`).

Useful for putting a `dist/` build online in one command — the yaml below is the config-file equivalent of `aperio-client --serve ./dist`.

Note: `serve:` is a **single-service** setting. It is mutually exclusive with `target:` and cannot be combined with a `services:` list — the client refuses to start on either combination. To publish a static site next to proxied backends today, run a second client process for the static site.
