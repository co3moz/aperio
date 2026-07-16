# syntax=docker/dockerfile:1
#
# Runtime image for aperio-client. Like the server image, the binary is prebuilt
# (musl-static) by the Release workflow's binary matrix and copied in per
# platform — no compilation happens in this image. See aperio-server/Dockerfile
# for the full rationale.
#
# The build context must contain the prebuilt binaries at
# `dist-docker/aperio-client-<arch>` (arch = amd64 | arm64); the release
# workflow stages them there. `TARGETARCH` is set per platform by buildx.
#
# For local development, `cargo build -p aperio-client` and run the binary.
FROM alpine:latest
RUN apk add --no-cache ca-certificates

WORKDIR /app
ARG TARGETARCH
COPY --chmod=0755 dist-docker/aperio-client-${TARGETARCH} /app/aperio-client

ENV APERIO_SERVER_URL=http://localhost:8080
ENV APERIO_CLIENT_TARGET=http://127.0.0.1:8000
ENV APERIO_CLIENT_PASS_HOSTNAME=0

CMD ["/app/aperio-client"]
