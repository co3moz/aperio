# Build Stage
FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig

WORKDIR /usr/src/aperio
COPY Cargo.toml Cargo.lock ./
COPY aperio-server/Cargo.toml ./aperio-server/
COPY aperio-client/Cargo.toml ./aperio-client/

RUN mkdir -p aperio-server/src && echo "fn main() {}" > aperio-server/src/main.rs
RUN mkdir -p aperio-client/src && echo "fn main() {}" > aperio-client/src/main.rs
RUN cargo build --locked --release -p aperio-server
RUN rm -rf aperio-server/src

COPY aperio-server/src/ ./aperio-server/src/
RUN touch aperio-server/src/main.rs
RUN cargo build --locked --release -p aperio-server

# Runtime Stage
FROM alpine:latest
RUN apk add --no-cache ca-certificates

WORKDIR /app
COPY --from=builder /usr/src/aperio/target/release/aperio-server /app/aperio-server

EXPOSE 8080
ENV PORT=8080
ENV APERIO_SERVER_GATEWAY_TIMEOUT=10
ENV APERIO_SERVER_GATEWAY_RESPONSE_TIMEOUT=30

CMD ["/app/aperio-server"]
