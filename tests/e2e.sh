#!/usr/bin/env bash
# End-to-end integration test: real aperio-server + aperio-client + mock
# backends, exercised over HTTP with curl. Organized in phases, each with its
# own server configuration:
#
#   A. base        — health, 504, proxying, dashboard APIs, tunnels API,
#                    maintenance mode, settings API, access log, metrics,
#                    request inspector & replay, webhooks API, audit API,
#                    token API lifecycle (list/edit/revoke), client control
#                    API (overrule + kill switch)
#   B. auth        — visitor password: login redirect + share-link flow
#   C. failover    — retry-wait re-dispatch after a mid-request client kill
#   D. lb          — primary-standby tiers, then sticky sessions
#   E. features    — positional-target CLI, check provenance & failure modes,
#                    redirect following, multi-service client, ~/.aperio.yaml
#                    layer, per-token rate limit
#   F. ws          — WebSocket pass-through (upgrade + frame echo + close)
#   G. tunnels     — emergency tunnels (tunnels: + --bind-tunnels) and the
#                    legacy tcp bridge
#   H. subdomain   — same-level random subdomain pattern (*-suffix)
#   M. multihost   — one service claiming several hostnames
#   I. h2          — h2c:// backend (HTTP/2 prior knowledge) with gRPC-style
#                    response trailers relayed end to end
#   J. sessions    — dashboard sessions survive a server restart
#   K. cache       — response cache hits, ETag/304 conditional answers, and
#                    serve-stale for resilient services while offline
#   L. health      — target_health probes: reporting, routing exclusion,
#                    recovery, and immediate first probe on a dead backend
#
# Usage: bash tests/e2e.sh
# Expects target/debug binaries (override with APERIO_SERVER_BIN/APERIO_CLIENT_BIN).
#
# Structure: this runner sources tests/lib/harness.sh (config + helpers) and
# then each tests/phases/<letter>-<name>.sh in order. Run everything with
# `bash tests/e2e.sh`, or a subset by phase letter or name:
#   bash tests/e2e.sh cache health      # only those phases
#   bash tests/e2e.sh k l               # same, by letter
# Expects target/debug binaries (override with APERIO_SERVER_BIN/APERIO_CLIENT_BIN).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib/harness.sh
source "$HERE/lib/harness.sh"

run_phase() { # <path>
  # shellcheck source=/dev/null
  source "$1"
}

# Map a requested selector (phase letter or name) to its file.
phase_file() { # <selector>
  case "$1" in
    "a"|"base") echo "$HERE/phases/a-base.sh" ;;
    "b"|"auth") echo "$HERE/phases/b-auth.sh" ;;
    "c"|"failover") echo "$HERE/phases/c-failover.sh" ;;
    "d"|"lb") echo "$HERE/phases/d-lb.sh" ;;
    "e"|"features") echo "$HERE/phases/e-features.sh" ;;
    "f"|"ws") echo "$HERE/phases/f-ws.sh" ;;
    "g"|"tunnels") echo "$HERE/phases/g-tunnels.sh" ;;
    "h"|"subdomain") echo "$HERE/phases/h-subdomain.sh" ;;
    "i"|"h2") echo "$HERE/phases/i-h2.sh" ;;
    "j"|"sessions") echo "$HERE/phases/j-sessions.sh" ;;
    "k"|"cache") echo "$HERE/phases/k-cache.sh" ;;
    "l"|"health") echo "$HERE/phases/l-health.sh" ;;
    "m"|"multihost") echo "$HERE/phases/m-multihost.sh" ;;
    *) echo ""; return 1 ;;
  esac
}

ALL_PHASES=("a-base.sh" "b-auth.sh" "c-failover.sh" "d-lb.sh" "e-features.sh" "f-ws.sh" "g-tunnels.sh" "h-subdomain.sh" "i-h2.sh" "j-sessions.sh" "k-cache.sh" "l-health.sh" "m-multihost.sh")

if [ "$#" -gt 0 ]; then
  SELECTED=()
  for sel in "$@"; do
    f="$(phase_file "$sel")" || { echo "FAIL: unknown phase '$sel'" >&2; exit 1; }
    SELECTED+=("$HERE/phases/$(basename "$f")")
  done
  for f in "${SELECTED[@]}"; do run_phase "$f"; done
else
  for f in "${ALL_PHASES[@]}"; do run_phase "$HERE/phases/$f"; done
fi

echo
echo "All E2E tests passed ✔"
