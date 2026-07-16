# syntax=docker/dockerfile:1
#
# Runtime image for aperio-server. The binary is NOT compiled here: the Release
# workflow's binary matrix already produces a musl-static aperio-server (with
# the dashboard embedded) for both linux/amd64 and linux/arm64, and this image
# simply copies the right one in. That keeps the release from building the same
# Rust code twice, and it removes the whole class of in-container build failures
# (cross-compiling OpenSSL, workspace manifests, ...).
#
# The build context must contain the prebuilt binaries at
# `dist-docker/aperio-server-<arch>` (arch = amd64 | arm64); the release
# workflow stages them there. `TARGETARCH` is set per platform by buildx.
#
# There is no from-source Docker build — for local development just
# `cargo build -p aperio-server` and run the binary directly.
FROM alpine:latest
RUN apk add --no-cache ca-certificates

WORKDIR /app
ARG TARGETARCH
COPY --chmod=0755 dist-docker/aperio-server-${TARGETARCH} /app/aperio-server

EXPOSE 8080
ENV PORT=8080
ENV APERIO_SERVER_GATEWAY_TIMEOUT=10
ENV APERIO_SERVER_GATEWAY_RESPONSE_TIMEOUT=30

CMD ["/app/aperio-server"]
