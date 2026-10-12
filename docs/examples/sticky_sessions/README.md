# Sticky sessions

`routing.lb_strategy: sticky` pins each visitor to the client that first served them. Run the same file on each backend machine, each with its own `client_id`. See [Routing](../../traffic.md#routing).
