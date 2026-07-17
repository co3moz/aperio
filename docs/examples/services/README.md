# Services (multiple targets from one client)

One client process can expose several backends at once: replace the single `target:` with a `services:` list. The client opens one tunnel connection per entry, and each entry carries its own binds, health probe, and tuning knobs — unset knobs fall back to the top-level values.

Here a single machine publishes three things:

- `app.example.com` → a web app on port 3000 (with a health probe),
- `api.example.com` → an API on port 4000 (with a concurrency cap),
- `/docs` on any hostname → a docs server on port 5000.

The `services:` list only comes from the config file — a positional CLI target overrides it entirely. Config hot-reload re-resolves the whole list, so adding or removing a service does not need a restart.
