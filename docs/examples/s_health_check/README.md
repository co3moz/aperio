# Health Check

The client probes its local backend independently and reports the result to the server. A failing backend takes the client **out of routing without dropping the tunnel**; it rejoins automatically when the probe recovers, and the dashboard shows a `BACKEND DOWN` badge meanwhile.

The service starts *unhealthy* until the first probe succeeds (shown as `CHECKING` in the dashboard), the client never claims a backend is up before it has checked it. The first probe runs immediately at startup, so a healthy backend becomes routable within one probe, not one interval. Probes never follow redirects.

Multi-service variant: [m_health_check](../m_health_check/).
