# Health Check (multi-service)

> **Concept:** [Client Resilience](../../client-resilience.md).


Every `services:` entry probes its **own** backend: each has its own `target_health` endpoint and tuning, and each leaves rotation independently when its probe fails, the web app going down does not touch the API's routing, and neither drops the shared tunnel process.

Unset probe knobs (`health_interval`, `health_timeout`, `health_threshold`) fall back to the top-level values, so shared tuning is written once.
