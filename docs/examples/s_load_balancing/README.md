# Load Balancing — primary/standby

With `lb_strategy: primary-standby`, only the clients announcing the **lowest** priority tier receive traffic (`priority: 0` = primary). Standby tiers take over automatically when every more-primary client is unhealthy, draining, disabled, or gone — and hand back when a primary returns. The dashboard marks standby clients with a `standby N` badge.

This folder's `aperio.yaml` is the **primary** client; run a second client on the standby machine with the same config but `priority: 1` (see the comment in the file). With the default `round-robin` strategy instead, clients with identical binds simply share traffic evenly — no priority needed.

Multi-service variant: [m_load_balancing](../m_load_balancing/).
