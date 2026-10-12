# gRPC

A gRPC backend on an `h2c://` target beside an ordinary web app. The proxy in front of the server must forward gRPC as HTTP/2 (nginx `grpc_pass`) for trailers to survive. See [Kinds of service](../../configuration.md#kinds-of-service).
