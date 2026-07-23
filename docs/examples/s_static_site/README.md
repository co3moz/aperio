# Static Site

> **Concept:** [Configuration](../../configuration.md).


Publish a local directory of static files without any backend process: `serve:` replaces `target:` and the client answers requests from the directory itself (directories serve their `index.html`).

Useful for putting a `dist/` build online in one command, the yaml below is the config-file equivalent of `aperio-client --serve ./dist`.

The top-level `serve:` is single-service mode and is mutually exclusive with `target:` and a `services:` list. To serve **several** directories (or a static site next to proxied backends) from one client, put `serve:` on individual `services:` entries instead, see [m_static_site](../m_static_site/).
