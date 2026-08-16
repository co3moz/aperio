#!/bin/sh
# Stop every instance on removal, not on upgrade. See preremove-server.sh.
set -e
if [ "$1" = "upgrade" ] || [ "$1" = "1" ]; then
  exit 0
fi
for unit in $(systemctl list-units --state=active --no-legend 'aperio-client@*' 2>/dev/null | awk '{print $1}'); do
  systemctl disable --now "$unit" >/dev/null 2>&1 || true
done
