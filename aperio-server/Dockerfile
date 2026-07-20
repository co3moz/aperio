# Dashboard Build Stage
FROM node:22-alpine AS dashboard
WORKDIR /usr/src/aperio/aperio-dashboard
COPY aperio-dashboard/package.json aperio-dashboard/package-lock.json ./
RUN npm ci
COPY aperio-dashboard/ ./
RUN npm run build

# Build Stage
FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig

# The dashboard is built in the Node stage above; build.rs must not re-run npm.
ENV APERIO_SKIP_DASHBOARD_BUILD=1

WORKDIR /usr/src/aperio
COPY Cargo.toml Cargo.lock ./
COPY aperio-server/Cargo.toml aperio-server/build.rs ./aperio-server/
COPY aperio-client/Cargo.toml ./aperio-client/
# aperio-config and tests/mock-h2 are workspace members, so their manifests
# must exist for the workspace to resolve — even though aperio-server does
# not depend on either.
COPY aperio-config/Cargo.toml ./aperio-config/
COPY tests/mock-h2/Cargo.toml ./tests/mock-h2/

RUN mkdir -p aperio-server/src && echo "fn main() {}" > aperio-server/src/main.rs
RUN mkdir -p aperio-client/src && echo "fn main() {}" > aperio-client/src/main.rs
RUN mkdir -p aperio-config/src && echo "" > aperio-config/src/lib.rs
RUN mkdir -p tests/mock-h2/src && echo "fn main() {}" > tests/mock-h2/src/main.rs
RUN cargo build --locked --release -p aperio-server
RUN rm -rf aperio-server/src

COPY aperio-server/src/ ./aperio-server/src/
COPY --from=dashboard /usr/src/aperio/aperio-dashboard/dist/ ./aperio-dashboard/dist/
RUN touch aperio-server/src/main.rs
RUN cargo build --locked --release -p aperio-server

# Runtime Stage
FROM alpine:latest
RUN apk add --no-cache ca-certificates

WORKDIR /app
COPY --from=builder /usr/src/aperio/target/release/aperio-server /app/aperio-server

EXPOSE 8080
ENV PORT=8080
ENV APERIO_GATEWAY_TIMEOUT=10
ENV APERIO_GATEWAY_RESPONSE_TIMEOUT=30

CMD ["/app/aperio-server"]
