#!/usr/bin/env sh
# Aperio installer: downloads a prebuilt release binary for this platform.
#
#   curl -sSf https://raw.githubusercontent.com/co3moz/aperio/master/install.sh | sh
#
# Options (environment variables):
#   APERIO_BIN=aperio-server     binary to install (default: aperio-client)
#   APERIO_VERSION=v0.2.0        release tag (default: latest)
#   APERIO_INSTALL_DIR=~/bin     install directory (default: ~/.local/bin)
set -eu

REPO="co3moz/aperio"
BIN="${APERIO_BIN:-aperio-client}"
VERSION="${APERIO_VERSION:-latest}"
INSTALL_DIR="${APERIO_INSTALL_DIR:-$HOME/.local/bin}"

case "$BIN" in
  aperio-client | aperio-server) ;;
  *)
    echo "error: APERIO_BIN must be 'aperio-client' or 'aperio-server'" >&2
    exit 1
    ;;
esac

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)
    case "$arch" in
      x86_64) target="x86_64-unknown-linux-musl" ;;
      aarch64 | arm64) target="aarch64-unknown-linux-musl" ;;
      *)
        echo "error: unsupported architecture: $arch" >&2
        exit 1
        ;;
    esac
    ;;
  Darwin)
    case "$arch" in
      x86_64) target="x86_64-apple-darwin" ;;
      arm64) target="aarch64-apple-darwin" ;;
      *)
        echo "error: unsupported architecture: $arch" >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "error: unsupported OS: $os" >&2
    echo "On Windows, download the zip from https://github.com/$REPO/releases" >&2
    exit 1
    ;;
esac

if [ "$VERSION" = "latest" ]; then
  url="https://github.com/$REPO/releases/latest/download/${BIN}-${target}.tar.gz"
else
  url="https://github.com/$REPO/releases/download/${VERSION}/${BIN}-${target}.tar.gz"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $url ..."
curl -sSfL "$url" -o "$tmp/pkg.tar.gz"
tar -xzf "$tmp/pkg.tar.gz" -C "$tmp"

mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/$BIN" "$INSTALL_DIR/$BIN"
echo "Installed: $INSTALL_DIR/$BIN ($("$INSTALL_DIR/$BIN" --version))"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo "note: $INSTALL_DIR is not on your PATH; add it with:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac
