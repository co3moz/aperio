# Build Stage
FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev

WORKDIR /usr/src/aperio-client
COPY Cargo.toml ./
# Build dummy main to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release

# Copy actual source code
COPY src/ ./src/
RUN touch src/main.rs
RUN cargo build --release

# Runtime Stage
FROM alpine:latest
RUN apk add --no-cache ca-certificates

WORKDIR /app
COPY --from=builder /usr/src/aperio-client/target/release/aperio-client /app/aperio-client

ENV APERIO_SERVER=http://localhost:8080
ENV APERIO_CLIENT_TARGET=http://127.0.0.1:8000
ENV APERIO_CLIENT_PASS_HOSTNAME=0

CMD ["/app/aperio-client"]
