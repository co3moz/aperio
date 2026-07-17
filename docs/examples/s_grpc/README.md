# gRPC (HTTP/2 backends)

An `h2c://` (cleartext prior knowledge) or `h2://` (TLS) target is dialed over HTTP/2: `te: trailers` is forwarded and response trailers (`grpc-status`) are relayed to the visitor — everything gRPC needs on the backend leg.

The **visitor leg** must also be HTTP/2 for trailers to survive: aperio-server accepts h2c, so have your fronting proxy forward gRPC traffic as HTTP/2 (e.g. nginx `grpc_pass`, or an h2c-capable load balancer) rather than downgrading it to HTTP/1.1.
