#!/usr/bin/env bash
# End-to-end integration test: real aperio-server + aperio-client + mock
# backend, exercised over HTTP with curl.
#
# Covers:
#   1. health endpoint
#   2. 504 when no tunnel client is connected
#   3. GET/POST proxying through the tunnel (hostname routing)
#   4. dashboard login + stats/logs APIs
#   5. programmatic tunnels API (provision → connect → proxy → revoke)
#
# Usage: bash tests/e2e.sh
# Expects target/debug binaries (override with APERIO_SERVER_BIN/APERIO_CLIENT_BIN).
set -euo pipefail

SERVER_PORT=18100
BACKEND_PORT=18101
BASE="http://127.0.0.1:${SERVER_PORT}"
TOKEN="e2e-master-token-$(date +%s)"
HOSTNAME_BIND="app.e2e.local"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# The target dir may be relocated via config.toml; ask cargo when available.
TARGET_DIR="${CARGO_TARGET_DIR:-$(cd "$ROOT" && cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' | sed 's/\\\\/\//g')}"
TARGET_DIR="${TARGET_DIR:-$ROOT/target}"
SERVER_BIN="${APERIO_SERVER_BIN:-$TARGET_DIR/debug/aperio-server}"
CLIENT_BIN="${APERIO_CLIENT_BIN:-$TARGET_DIR/debug/aperio-client}"
# Windows (Git Bash) compatibility.
[ ! -e "$SERVER_BIN" ] && [ -e "$SERVER_BIN.exe" ] && SERVER_BIN="$SERVER_BIN.exe"
[ ! -e "$CLIENT_BIN" ] && [ -e "$CLIENT_BIN.exe" ] && CLIENT_BIN="$CLIENT_BIN.exe"
DATA_DIR="$(mktemp -d)"
LOG_DIR="$(mktemp -d)"

# Pick a python that actually runs (the Windows Store ships a python3 stub
# that only prints an install hint).
PYTHON=""
for candidate in python3 python; do
  if "$candidate" -c 'pass' >/dev/null 2>&1; then
    PYTHON="$candidate"
    break
  fi
done
[ -n "$PYTHON" ] || { echo "FAIL: no working python interpreter found" >&2; exit 1; }

PIDS=()
FAILED=0

cleanup() {
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
  done
  if [ "$FAILED" -ne 0 ]; then
    echo "--- server log ----------------------------------------------------"
    cat "$LOG_DIR/server.log" 2>/dev/null || true
    echo "--- client log ----------------------------------------------------"
    cat "$LOG_DIR/client.log" 2>/dev/null || true
    echo "--- ephemeral client log -------------------------------------------"
    cat "$LOG_DIR/client-ephemeral.log" 2>/dev/null || true
  fi
  rm -rf "$DATA_DIR" "$LOG_DIR"
}
trap cleanup EXIT

step() { echo; echo "==> $*"; }

fail() {
  echo "FAIL: $*" >&2
  FAILED=1
  exit 1
}

# assert_contains <haystack> <needle> <label>
assert_contains() {
  case "$1" in
    *"$2"*) echo "  ok: $3" ;;
    *) fail "$3 — expected to contain '$2', got: $1" ;;
  esac
}

# assert_status <expected> <actual> <label>
assert_status() {
  if [ "$1" != "$2" ]; then
    fail "$3 — expected HTTP $1, got HTTP $2"
  fi
  echo "  ok: $3"
}

# retry <seconds> <command...> — retries the command once per second.
retry() {
  local deadline=$(( $(date +%s) + $1 )); shift
  until "$@" >/dev/null 2>&1; do
    if [ "$(date +%s)" -ge "$deadline" ]; then
      return 1
    fi
    sleep 1
  done
}

step "Starting mock backend on :${BACKEND_PORT}"
"$PYTHON" - "$BACKEND_PORT" <<'PYEOF' >"$LOG_DIR/backend.log" 2>&1 &
import http.server, sys

class Handler(http.server.BaseHTTPRequestHandler):
    def _respond(self, body):
        data = body.encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/plain')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self._respond(f'backend GET {self.path}')

    def do_POST(self):
        length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(length).decode()
        self._respond(f'backend POST {self.path} body={body}')

    def log_message(self, *args):
        pass

http.server.HTTPServer(('127.0.0.1', int(sys.argv[1])), Handler).serve_forever()
PYEOF
PIDS+=($!)
retry 10 curl -sf "http://127.0.0.1:${BACKEND_PORT}/ping" || fail "mock backend did not come up"

step "Starting aperio-server on :${SERVER_PORT}"
PORT="$SERVER_PORT" \
  APERIO_SERVER_TOKEN="$TOKEN" \
  APERIO_DATA_DIR="$DATA_DIR" \
  APERIO_RANDOM_SUBDOMAIN='*.e2e.local' \
  APERIO_SERVER_GATEWAY_TIMEOUT=3 \
  "$SERVER_BIN" >"$LOG_DIR/server.log" 2>&1 &
PIDS+=($!)
retry 15 curl -sf "$BASE/aperio/health" || fail "server did not come up"

step "1. Health endpoint"
HEALTH="$(curl -s "$BASE/aperio/health")"
assert_contains "$HEALTH" '"status":"healthy"' "health reports healthy"

step "2. 504 when no client is connected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/")"
assert_status 504 "$CODE" "proxying without a client returns 504"

step "3. Tunnel proxying through a connected client"
APERIO_SERVER_URL="$BASE" \
  APERIO_SERVER_TOKEN="$TOKEN" \
  APERIO_CLIENT_TARGET="http://127.0.0.1:${BACKEND_PORT}" \
  APERIO_HOSTNAME_BIND="$HOSTNAME_BIND" \
  "$CLIENT_BIN" >"$LOG_DIR/client.log" 2>&1 &
PIDS+=($!)
# The hostname bind is declared via the client's heartbeat; retry until routed.
retry 20 curl -sf -H "Host: ${HOSTNAME_BIND}" "$BASE/hello" \
  || fail "tunnel did not become routable in time"

BODY="$(curl -s -H "Host: ${HOSTNAME_BIND}" "$BASE/hello?x=1")"
assert_contains "$BODY" 'backend GET /hello?x=1' "GET is proxied to the backend"

BODY="$(curl -s -X POST -H "Host: ${HOSTNAME_BIND}" -H 'Content-Type: text/plain' \
  --data 'payload-123' "$BASE/submit")"
assert_contains "$BODY" 'backend POST /submit body=payload-123' "POST body is proxied"

step "4. Dashboard login and APIs"
COOKIES="$LOG_DIR/cookies.txt"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -c "$COOKIES" -X POST \
  -u "aperio:${TOKEN}" "$BASE/aperio/auth")"
assert_status 200 "$CODE" "login with master token succeeds"

CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -u 'aperio:wrong-password' "$BASE/aperio/auth")"
assert_status 401 "$CODE" "login with a bad password is rejected"

STATS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/stats")"
assert_contains "$STATS" '"connected_clients_count":1' "stats show the connected client"
assert_contains "$STATS" "\"$HOSTNAME_BIND\"" "stats show the hostname bind"

LOGS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/logs")"
assert_contains "$LOGS" '/submit' "request log captured the proxied POST"

CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/aperio/api/stats")"
assert_status 302 "$CODE" "stats without a session redirect to login"

step "5. Programmatic tunnels API"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
  --data '{}' "$BASE/aperio/api/tunnels")"
assert_status 401 "$CODE" "tunnel provisioning without credentials is rejected"

TUNNEL="$(curl -sf -X POST -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
  --data '{"name":"e2e-preview","ttl_seconds":300}' "$BASE/aperio/api/tunnels")" \
  || fail "tunnel provisioning with the master token failed"
assert_contains "$TUNNEL" '"token":"apr_' "tunnel response contains an ephemeral token"
assert_contains "$TUNNEL" '.e2e.local' "tunnel response contains a random subdomain"

EPHEMERAL_TOKEN="$(echo "$TUNNEL" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
EPHEMERAL_HOST="$(echo "$TUNNEL" | sed -n 's/.*"hostname":"\([^"]*\)".*/\1/p')"
TUNNEL_ID="$(echo "$TUNNEL" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
[ -n "$EPHEMERAL_TOKEN" ] && [ -n "$EPHEMERAL_HOST" ] && [ -n "$TUNNEL_ID" ] \
  || fail "could not parse the tunnel response: $TUNNEL"

APERIO_SERVER_URL="$BASE" \
  APERIO_SERVER_TOKEN="$EPHEMERAL_TOKEN" \
  APERIO_CLIENT_TARGET="http://127.0.0.1:${BACKEND_PORT}" \
  "$CLIENT_BIN" >"$LOG_DIR/client-ephemeral.log" 2>&1 &
PIDS+=($!)
# The token-granted hostname is auto-bound on connect.
retry 20 curl -sf -H "Host: ${EPHEMERAL_HOST}" "$BASE/preview" \
  || fail "ephemeral tunnel did not become routable in time"
BODY="$(curl -s -H "Host: ${EPHEMERAL_HOST}" "$BASE/preview")"
assert_contains "$BODY" 'backend GET /preview' "ephemeral tunnel proxies to the backend"

CODE="$(curl -s -o /dev/null -w '%{http_code}' -X DELETE \
  -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/api/tunnels/${TUNNEL_ID}")"
assert_status 200 "$CODE" "tunnel revocation succeeds"

CODE="$(curl -s -o /dev/null -w '%{http_code}' -X DELETE \
  -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/api/tunnels/${TUNNEL_ID}")"
assert_status 404 "$CODE" "revoking the same tunnel twice returns 404"

TOKENS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/tokens")"
assert_contains "$TOKENS" '[]' "revoked ephemeral token is gone from the store"

echo
echo "All E2E tests passed ✔"
