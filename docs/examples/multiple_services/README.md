# Services (multiple targets from one client)

> **Concept:** [Configuration](../../configuration.md).


One client process can expose several backends through a `services:` list. Client config files use this list even for one backend. Each entry has its own binds, health probe, and tuning; unset values use supported top-level defaults. See [Multiple services](../../configuration.md#multiple-services).

Here a single machine publishes three things:

- `app.example.com` → a web app on port 3000 (with a health probe),
- `api.example.com` → an API on port 4000 (with a concurrency cap),
- `/docs` on any hostname → a docs server on port 5000.

The `services:` list only comes from the config file, a positional CLI target overrides it entirely. Config hot-reload re-resolves the whole list, so adding or removing a service does not need a restart.

An entry may also carry `serve: <dir>` instead of `target:` to serve a static directory as that service, see [static_site](../static_site/).
