# gRPC

> **Concept:** [Tunnel Protocol](../../tunnel-protocol.md).


An `h2c://` (cleartext prior knowledge) or `h2://` (TLS) target is dialed over HTTP/2: `te: trailers` is forwarded and response trailers (`grpc-status`) are relayed to the visitor, everything gRPC needs on the backend leg.

The **visitor leg** must also be HTTP/2 for trailers to survive: aperio-server accepts h2c, so have your fronting proxy forward gRPC traffic as HTTP/2 (e.g. nginx `grpc_pass`, or an h2c-capable load balancer) rather than downgrading it to HTTP/1.1.

Protocols mix freely across `services:` entries, which is what the pair below shows: a gRPC backend on an `h2c://` target next to an ordinary HTTP web app, from one client. The HTTP/2 requirement on the visitor leg applies to the gRPC hostname only.
