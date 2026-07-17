# Resilience (multi-service, serve-stale)

`resilience:` is a per-entry opt-in on top of `cache:`: while the client is away (redeploy, dead uplink), the server keeps answering the marketing site from its cache — even past the entries' lifetime, marked `x-aperio-stale: true` — while the dynamic API correctly fails instead of returning stale data. See [s_resilience](../s_resilience/) for the serve-stale mechanics.
