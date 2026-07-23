# Load Balancing (multi-service, primary/standby)

`priority` is announced **per service**, so one client can be the primary for some routes and a standby for others. Here machine A is the primary for the web app but only the standby for the API, machine B runs the mirror image, so each machine has an active role and takes over the other's when it dies.

`aperio.yaml` below is machine A; copy it to machine B and swap the two `priority` values (comments mark them). Both connect with the same token; the server's `lb_strategy: primary-standby` routes each hostname to its lowest healthy tier.
