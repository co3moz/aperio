# Load Balancing

> **Concept:** [Routing & Load Balancing](../../routing-and-load-balancing.md).


With `lb_strategy: primary-standby`, only the clients announcing the **lowest** priority tier receive traffic (`priority: 0` = primary). Standby tiers take over automatically when every more-primary client is unhealthy, draining, disabled, or gone, and hand back when a primary returns. The dashboard marks standby clients with a `standby N` badge.

With the default `round-robin` strategy instead, clients with identical binds simply share traffic evenly, no priority needed.

`priority` is announced **per service**, so one client can be the primary for some routes and a standby for others, which is what the pair below shows. Here machine A is the primary for the web app but only the standby for the API, machine B runs the mirror image, so each machine has an active role and takes over the other's when it dies.

`aperio.yaml` below is machine A; copy it to machine B and swap the two `priority` values (comments mark them). Both connect with the same token; the server's `lb_strategy: primary-standby` routes each hostname to its lowest healthy tier.
