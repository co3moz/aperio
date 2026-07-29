# Health Check

> **Concept:** [Client Resilience](../../client-resilience.md).


The client probes its local backend independently and reports the result to the server. A failing backend takes the client **out of routing without dropping the tunnel**; it rejoins automatically when the probe recovers, and the dashboard shows a `BACKEND DOWN` badge meanwhile.

The service starts *unhealthy* until the first probe succeeds (shown as `CHECKING` in the dashboard), the client never claims a backend is up before it has checked it. The first probe runs immediately at startup, so a healthy backend becomes routable within one probe, not one interval. Probes never follow redirects.

Every `services:` entry probes its **own** backend, which is what the pair below shows: each has its own `health.endpoint` and tuning, and each leaves rotation independently when its probe fails, the web app going down does not touch the API's routing, and neither drops the shared tunnel process.

Unset probe knobs (`health.interval`, `health.timeout`, `health.threshold`) fall back to the top-level `health:` block, so shared tuning is written once.
