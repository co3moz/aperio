# syntax=docker/dockerfile:1
#
# Runtime image for aperio-client. Like the server image, the binary is prebuilt
# (musl-static) by the Release workflow's binary matrix and copied in per
# platform, no compilation happens in this image. See aperio-server/Dockerfile
# for the full rationale.
#
# The build context must contain the prebuilt binaries at
# `dist-docker/aperio-client-<arch>` (arch = amd64 | arm64); the release
# workflow stages them there. `TARGETARCH` is set per platform by buildx.
#
# This is used only by the release workflow. For a from-source build, use the
# sibling `Dockerfile`, which compiles the crate itself.
FROM alpine:latest
RUN apk add --no-cache ca-certificates

WORKDIR /app
ARG TARGETARCH
COPY --chmod=0755 dist-docker/aperio-client-${TARGETARCH} /app/aperio-client

# No setting is baked into the image: the environment beats a mounted
# aperio.yaml, so a default here would override the file it is run with.

CMD ["/app/aperio-client"]
