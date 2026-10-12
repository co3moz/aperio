# Primary and standby

`routing.lb_strategy: primary-standby` on the server, and a `priority` per service: this machine is primary for the web app and standby for the API. Run a copy on a second machine with the two priorities swapped, and each takes over the other's role when it goes. See [Routing](../../traffic.md#routing).
