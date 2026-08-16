#!/bin/sh
# Stop on removal, not on upgrade. `$1` is the remaining version count on rpm
# and the word `upgrade` on deb, which is why both are checked.
set -e
if [ "$1" = "upgrade" ] || [ "$1" = "1" ]; then
  exit 0
fi
systemctl disable --now aperio-server >/dev/null 2>&1 || true
