# Build Stage
FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev

WORKDIR /usr/src/aperio
COPY Cargo.toml Cargo.lock ./
COPY aperio-client/Cargo.toml ./aperio-client/
COPY aperio-server/Cargo.toml ./aperio-server/

RUN mkdir -p aperio-client/src && echo "fn main() {}" > aperio-client/src/main.rs
RUN mkdir -p aperio-server/src && echo "fn main() {}" > aperio-server/src/main.rs
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
