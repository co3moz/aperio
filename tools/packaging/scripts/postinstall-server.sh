#!/bin/sh
# Runs on install and on upgrade. Deliberately does *not* start the service:
# the shipped config carries a placeholder master token, and a tunnel server
# that comes up on `apt install` with a token from a package is worse than one
# that does not come up at all.
set -e

systemd-sysusers /usr/lib/sysusers.d/aperio-server.conf >/dev/null 2>&1 || true
systemd-tmpfiles --create /usr/lib/tmpfiles.d/aperio-server.conf >/dev/null 2>&1 || true
systemctl daemon-reload >/dev/null 2>&1 || true

# On upgrade, restart what was already running; on first install, say what to do.
if systemctl is-active --quiet aperio-server 2>/dev/null; then
  systemctl restart aperio-server >/dev/null 2>&1 || true
else
  cat <<'MSG'

Aperio server installed. Before starting it:

  1. Set a master token in /etc/aperio/aperio-server.yaml
  2. systemctl enable --now aperio-server

It listens on 127.0.0.1:8080 and does not terminate TLS; put a reverse proxy
in front of it. See https://github.com/co3moz/aperio/blob/master/docs/edge-proxy.md

MSG
fi
