# Build Stage
FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev

WORKDIR /usr/src/aperio-server
COPY Cargo.toml ./
# Build dummy main to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release

# Copy actual source code and asset
COPY src/ ./src/
RUN touch src/main.rs
RUN cargo build --release

# Runtime Stage
FROM alpine:latest
RUN apk add --no-cache ca-certificates

WORKDIR /app
COPY --from=builder /usr/src/aperio-server/target/release/aperio-server /app/aperio-server

EXPOSE 8080
ENV PORT=8080
ENV APERIO_SERVER_GATEWAY_TIMEOUT=10
ENV APERIO_SERVER_GATEWAY_RESPONSE_TIMEOUT=30

CMD ["/app/aperio-server"]
