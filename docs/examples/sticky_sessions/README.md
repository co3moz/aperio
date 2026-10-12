# Sticky sessions

`routing.lb_strategy: sticky` pins each visitor to the client that first served them. Run the same file on each backend machine: each keeps an id of its own, which the affinity keys on. See [Routing](../../traffic.md#routing).
