# Build Stage
FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev

WORKDIR /usr/src/aperio
COPY Cargo.toml Cargo.lock ./
COPY aperio-client/Cargo.toml aperio-client/build.rs ./aperio-client/
COPY aperio-server/Cargo.toml ./aperio-server/
# aperio-config is a real dependency of the client and its build script, so its
# full (tiny, rarely-changing) source is needed to compile — not just the manifest.
COPY aperio-config/Cargo.toml ./aperio-config/
COPY aperio-config/src/ ./aperio-config/src/
# tests/mock-h2 is a workspace member (e2e-only helper), so its manifest must
# exist for the workspace to resolve — its code is never built here.
COPY tests/mock-h2/Cargo.toml ./tests/mock-h2/

RUN mkdir -p aperio-client/src && echo "fn main() {}" > aperio-client/src/main.rs
RUN mkdir -p aperio-server/src && echo "fn main() {}" > aperio-server/src/main.rs
RUN mkdir -p tests/mock-h2/src && echo "fn main() {}" > tests/mock-h2/src/main.rs
RUN cargo build --locked --release -p aperio-client
RUN rm -rf aperio-client/src

COPY aperio-client/src/ ./aperio-client/src/
RUN touch aperio-client/src/main.rs
RUN cargo build --locked --release -p aperio-client

# Runtime Stage
FROM alpine:latest
RUN apk add --no-cache ca-certificates

WORKDIR /app
COPY --from=builder /usr/src/aperio/target/release/aperio-client /app/aperio-client

ENV APERIO_SERVER_URL=http://localhost:8080
ENV APERIO_CLIENT_TARGET=http://127.0.0.1:8000
ENV APERIO_CLIENT_PASS_HOSTNAME=0

CMD ["/app/aperio-client"]
