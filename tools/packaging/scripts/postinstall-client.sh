#!/bin/sh
# Runs on install and on upgrade. Starts nothing: a client instance is a
# config file the operator writes, and until one exists there is no instance
# to start.
set -e

systemd-sysusers /usr/lib/sysusers.d/aperio-client.conf >/dev/null 2>&1 || true
systemd-tmpfiles --create /usr/lib/tmpfiles.d/aperio-client.conf >/dev/null 2>&1 || true
systemctl daemon-reload >/dev/null 2>&1 || true

# Upgrade: restart every instance that was running, by name.
restarted=""
for unit in $(systemctl list-units --state=active --no-legend 'aperio-client@*' 2>/dev/null | awk '{print $1}'); do
  systemctl restart "$unit" >/dev/null 2>&1 || true
  restarted="yes"
done

if [ -z "$restarted" ]; then
  cat <<'MSG'

Aperio client installed. To bring up an instance:

  cp /etc/aperio/aperio-client.yaml.example /etc/aperio/myapp.yaml
  $EDITOR /etc/aperio/myapp.yaml
  systemctl enable --now aperio-client@myapp

MSG
fi
