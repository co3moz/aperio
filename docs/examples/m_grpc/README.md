# gRPC (multi-service)

Protocols mix freely across `services:` entries: a gRPC backend on an `h2c://` target (dialed over HTTP/2, trailers relayed) next to an ordinary HTTP web app, from one client. The HTTP/2 requirement on the visitor leg still applies to the gRPC hostname (see [s_grpc](../s_grpc/)).
