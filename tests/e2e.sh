#!/usr/bin/env bash
# End-to-end integration test: real aperio-server + aperio-client + mock
# backends, exercised over HTTP with curl. Organized in phases, each with its
# own server configuration:
#
#   A. base        — health, 504, proxying, dashboard APIs, tunnels API,
#                    maintenance mode, settings API, access log
#   B. auth        — visitor password: login redirect + share-link flow
#   C. failover    — retry-wait re-dispatch after a mid-request client kill
#   D. lb          — primary-standby tiers, then sticky sessions
#
# Usage: bash tests/e2e.sh
# Expects target/debug binaries (override with APERIO_SERVER_BIN/APERIO_CLIENT_BIN).
set -euo pipefail

SERVER_PORT=18100
BACKEND_PORT=18101
BACKEND2_PORT=18102
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

LOG_DIR="$(mktemp -d)"
DATA_DIR=""
PHASE="init"
SERVER_PID=""
CLIENT_PIDS=()
BACKEND_PIDS=()
FAILED=0

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

cleanup() {
  stop_server || true
  for pid in "${BACKEND_PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
  done
  if [ "$FAILED" -ne 0 ]; then
    for f in "$LOG_DIR"/*.log; do
      echo "--- $f ----------------------------------------------------------"
      cat "$f" 2>/dev/null || true
    done
  fi
  rm -rf "$LOG_DIR"
}
trap cleanup EXIT

step() { echo; echo "==> [$PHASE] $*"; }

fail() {
  echo "FAIL: [$PHASE] $*" >&2
  FAILED=1
  exit 1
}

assert_contains() { # <haystack> <needle> <label>
  case "$1" in
    *"$2"*) echo "  ok: $3" ;;
    *) fail "$3 — expected to contain '$2', got: $1" ;;
  esac
}

assert_status() { # <expected> <actual> <label>
  if [ "$1" != "$2" ]; then
    fail "$3 — expected HTTP $1, got HTTP $2"
  fi
  echo "  ok: $3"
}

retry() { # <seconds> <command...>
  local deadline=$(( $(date +%s) + $1 )); shift
  until "$@" >/dev/null 2>&1; do
    if [ "$(date +%s)" -ge "$deadline" ]; then
      return 1
    fi
    sleep 1
  done
}

# Starts a mock backend on the given port. Responses embed the port so tests
# can tell which backend served them; /slow sleeps 5 seconds first.
start_backend() { # <port>
  "$PYTHON" - "$1" <<'PYEOF' >"$LOG_DIR/backend-$1.log" 2>&1 &
import http.server, sys, time

class Handler(http.server.BaseHTTPRequestHandler):
    def _respond(self, body):
        data = body.encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/plain')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path.startswith('/slow'):
            time.sleep(5)
        port = self.server.server_address[1]
        self._respond(f'backend {port} GET {self.path}')

    def do_POST(self):
        length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(length).decode()
        port = self.server.server_address[1]
        self._respond(f'backend {port} POST {self.path} body={body}')

    def log_message(self, *args):
        pass

http.server.HTTPServer(('127.0.0.1', int(sys.argv[1])), Handler).serve_forever()
PYEOF
  BACKEND_PIDS+=($!)
  retry 10 curl -sf "http://127.0.0.1:$1/ping" || fail "mock backend :$1 did not come up"
}

# Starts aperio-server with a fresh data dir; extra env pairs come as args.
start_server() { # [KEY=VAL ...]
  DATA_DIR="$(mktemp -d)"
  env PORT="$SERVER_PORT" \
    APERIO_SERVER_TOKEN="$TOKEN" \
    APERIO_DATA_DIR="$DATA_DIR" \
    APERIO_RANDOM_SUBDOMAIN='*.e2e.local' \
    APERIO_SERVER_GATEWAY_TIMEOUT=3 \
    "$@" \
    "$SERVER_BIN" >"$LOG_DIR/server-$PHASE.log" 2>&1 &
  SERVER_PID=$!
  retry 15 curl -sf "$BASE/aperio/health" || fail "server did not come up"
}

stop_server() {
  for pid in "${CLIENT_PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
  done
  CLIENT_PIDS=()
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    SERVER_PID=""
    sleep 1
  fi
}

# Starts aperio-client against a backend; extra env pairs come as args.
start_client() { # <name> <backend_port> [KEY=VAL ...]
  local name="$1" backend_port="$2"
  shift 2
  env APERIO_SERVER_URL="$BASE" \
    APERIO_SERVER_TOKEN="$TOKEN" \
    APERIO_CLIENT_TARGET="http://127.0.0.1:${backend_port}" \
    "$@" \
    "$CLIENT_BIN" >"$LOG_DIR/client-$PHASE-$name.log" 2>&1 &
  CLIENT_PIDS+=($!)
}

# Logs into the dashboard and stores the session cookie jar at $1.
dashboard_login() { # <cookie-jar>
  local code
  code="$(curl -s -o /dev/null -w '%{http_code}' -c "$1" -X POST -u "aperio:${TOKEN}" "$BASE/aperio/auth")"
  assert_status 200 "$code" "dashboard login"
}

wait_routable() { # <host> [path]
  retry 20 curl -sf -H "Host: $1" "$BASE${2:-/hello}" \
    || fail "tunnel for $1 did not become routable in time"
}

##############################################################################
PHASE="base"
##############################################################################

start_backend "$BACKEND_PORT"

step "Starting aperio-server (base configuration)"
ACCESS_LOG="$LOG_DIR/access.jsonl"
start_server APERIO_ACCESS_LOG="$ACCESS_LOG"
BASE_DATA_DIR="$DATA_DIR"

step "Health endpoint"
HEALTH="$(curl -s "$BASE/aperio/health")"
assert_contains "$HEALTH" '"status":"healthy"' "health reports healthy"
assert_contains "$HEALTH" '"protocol":' "health reports the tunnel protocol version"

step "504 when no client is connected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/")"
assert_status 504 "$CODE" "proxying without a client returns 504"

step "Tunnel proxying through a connected client"
start_client main "$BACKEND_PORT" APERIO_HOSTNAME_BIND="$HOSTNAME_BIND"
wait_routable "$HOSTNAME_BIND"

BODY="$(curl -s -H "Host: ${HOSTNAME_BIND}" "$BASE/hello?x=1")"
assert_contains "$BODY" "backend ${BACKEND_PORT} GET /hello?x=1" "GET is proxied to the backend"

BODY="$(curl -s -X POST -H "Host: ${HOSTNAME_BIND}" -H 'Content-Type: text/plain' \
  --data 'payload-123' "$BASE/submit")"
assert_contains "$BODY" "backend ${BACKEND_PORT} POST /submit body=payload-123" "POST body is proxied"

step "Dashboard login and APIs"
COOKIES="$LOG_DIR/cookies.txt"
dashboard_login "$COOKIES"

CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -u 'aperio:wrong-password' "$BASE/aperio/auth")"
assert_status 401 "$CODE" "login with a bad password is rejected"

STATS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/stats")"
assert_contains "$STATS" '"connected_clients_count":1' "stats show the connected client"
assert_contains "$STATS" "\"$HOSTNAME_BIND\"" "stats show the hostname bind"
assert_contains "$STATS" '"by_hostname"' "stats include the traffic breakdown"

LOGS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/logs")"
assert_contains "$LOGS" '/submit' "request log captured the proxied POST"

CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/aperio/api/stats")"
assert_status 302 "$CODE" "stats without a session redirect to login"

step "Maintenance mode"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"hostname\":\"$HOSTNAME_BIND\",\"enabled\":true}" "$BASE/aperio/api/maintenance")"
assert_status 200 "$CODE" "maintenance can be enabled"
RESP="$(curl -s -D - -o /dev/null -H "Host: ${HOSTNAME_BIND}" "$BASE/hello")"
assert_contains "$RESP" "503" "maintenance answers 503"
assert_contains "$RESP" "retry-after" "maintenance sets Retry-After"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"hostname\":\"$HOSTNAME_BIND\",\"enabled\":false}" "$BASE/aperio/api/maintenance")"
assert_status 200 "$CODE" "maintenance can be disabled"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Host: ${HOSTNAME_BIND}" "$BASE/hello")"
assert_status 200 "$CODE" "traffic resumes after maintenance"

step "Settings API"
SETTINGS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/settings")"
assert_contains "$SETTINGS" '"effective"' "settings expose effective values"
assert_contains "$SETTINGS" '"lb_strategy":"round-robin"' "settings show the default lb strategy"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"gateway_timeout_secs":5,"lb_strategy":"sticky"}' "$BASE/aperio/api/settings")"
assert_status 200 "$CODE" "settings can be updated"
SETTINGS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/settings")"
assert_contains "$SETTINGS" '"lb_strategy":"sticky"' "settings update applied live"
[ -f "$BASE_DATA_DIR/settings.json" ] || fail "settings.json was not persisted"
assert_contains "$(cat "$BASE_DATA_DIR/settings.json")" '"gateway_timeout_secs": 5' "settings.json persists overrides"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"lb_strategy":"bogus"}' "$BASE/aperio/api/settings")"
assert_status 400 "$CODE" "invalid settings are rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{}' "$BASE/aperio/api/settings")"
assert_status 200 "$CODE" "settings overrides can be reset"

step "Programmatic tunnels API"
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

env APERIO_SERVER_URL="$BASE" APERIO_SERVER_TOKEN="$EPHEMERAL_TOKEN" \
  APERIO_CLIENT_TARGET="http://127.0.0.1:${BACKEND_PORT}" \
  "$CLIENT_BIN" >"$LOG_DIR/client-base-ephemeral.log" 2>&1 &
CLIENT_PIDS+=($!)
wait_routable "$EPHEMERAL_HOST" "/preview"
BODY="$(curl -s -H "Host: ${EPHEMERAL_HOST}" "$BASE/preview")"
assert_contains "$BODY" "backend ${BACKEND_PORT} GET /preview" "ephemeral tunnel proxies to the backend"

CODE="$(curl -s -o /dev/null -w '%{http_code}' -X DELETE \
  -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/api/tunnels/${TUNNEL_ID}")"
assert_status 200 "$CODE" "tunnel revocation succeeds"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X DELETE \
  -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/api/tunnels/${TUNNEL_ID}")"
assert_status 404 "$CODE" "revoking the same tunnel twice returns 404"

step "Structured access log"
[ -f "$ACCESS_LOG" ] || fail "access log file was not created"
assert_contains "$(cat "$ACCESS_LOG")" '"uri":"/hello' "access log records proxied requests"
assert_contains "$(cat "$ACCESS_LOG")" '"token":"master"' "access log attributes the token"

stop_server

##############################################################################
PHASE="auth"
##############################################################################

step "Starting aperio-server with a visitor password"
start_server APERIO_SERVER_AUTH='demo:secret123'
start_client main "$BACKEND_PORT" APERIO_HOSTNAME_BIND="$HOSTNAME_BIND"
retry 20 curl -s -o /dev/null "$BASE/aperio/health" || true
# Wait until the client is registered (login page would mask wait_routable).
retry 20 sh -c "curl -s '$BASE/aperio/health' | grep -q '\"connected_clients\":1'" \
  || fail "client did not connect in the auth phase"

step "Visitor password gate"
RESP="$(curl -s -D - -o /dev/null -H "Host: ${HOSTNAME_BIND}" "$BASE/hello")"
assert_contains "$RESP" "302" "unauthenticated visitors are redirected"
assert_contains "$RESP" "/aperio/auth" "redirect goes to the login page"

step "Share link flow"
COOKIES="$LOG_DIR/cookies-auth.txt"
dashboard_login "$COOKIES"
SHARE="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"hostname\":\"$HOSTNAME_BIND\",\"ttl_seconds\":300}" "$BASE/aperio/api/share")" \
  || fail "share link creation failed"
SHARE_TOKEN="$(echo "$SHARE" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
[ -n "$SHARE_TOKEN" ] || fail "could not parse the share response: $SHARE"

RESP="$(curl -s -D - -o /dev/null -H "Host: ${HOSTNAME_BIND}" "$BASE/hello?aperio_share=${SHARE_TOKEN}")"
assert_contains "$RESP" "302" "share link answers with a redirect"
assert_contains "$RESP" "aperio_share=" "share link sets the cookie"
assert_contains "$RESP" "location: /hello" "share link redirects to the clean URL"

# The hostname bind arrives with the client's first heartbeat (~5s); the
# share cookie skips the login page, so it doubles as the routability probe.
retry 20 sh -c "curl -s -H 'Host: ${HOSTNAME_BIND}' -H 'Cookie: aperio_share=${SHARE_TOKEN}' '$BASE/hello' \
  | grep -q 'backend ${BACKEND_PORT} GET /hello'" \
  || fail "share cookie did not grant access in time"
echo "  ok: share cookie grants access"

RESP="$(curl -s -D - -o /dev/null -H "Host: other.e2e.local" -H "Cookie: aperio_share=${SHARE_TOKEN}" "$BASE/hello")"
assert_contains "$RESP" "302" "share cookie does not cover other hostnames"

RESP="$(curl -s -D - -o /dev/null -H "Host: ${HOSTNAME_BIND}" "$BASE/hello?aperio_share=tampered.token")"
assert_contains "$RESP" "/aperio/auth" "a tampered share token is rejected"

stop_server

##############################################################################
PHASE="failover"
##############################################################################

step "Starting aperio-server with retry-wait failover"
start_server APERIO_FAILOVER='retry-wait' APERIO_FAILOVER_WINDOW=20
start_client one "$BACKEND_PORT" APERIO_HOSTNAME_BIND="$HOSTNAME_BIND"
wait_routable "$HOSTNAME_BIND"

step "In-flight failover after a mid-request client kill"
FIRST_CLIENT_PID="${CLIENT_PIDS[0]}"
curl -s -H "Host: ${HOSTNAME_BIND}" "$BASE/slow" >"$LOG_DIR/failover-response.txt" &
CURL_PID=$!
sleep 1
kill "$FIRST_CLIENT_PID" 2>/dev/null || true
start_client two "$BACKEND_PORT" APERIO_HOSTNAME_BIND="$HOSTNAME_BIND"
wait "$CURL_PID" || fail "in-flight request did not complete"
assert_contains "$(cat "$LOG_DIR/failover-response.txt")" "backend ${BACKEND_PORT} GET /slow" \
  "request survived the client kill via failover"
assert_contains "$(cat "$LOG_DIR/server-failover.log")" "In-flight failover" \
  "server logged the failover jump"

stop_server

##############################################################################
PHASE="lb"
##############################################################################

start_backend "$BACKEND2_PORT"

step "Primary-standby strategy"
start_server APERIO_LB_STRATEGY='primary-standby'
start_client primary "$BACKEND_PORT" APERIO_HOSTNAME_BIND="$HOSTNAME_BIND"
start_client standby "$BACKEND2_PORT" APERIO_HOSTNAME_BIND="$HOSTNAME_BIND" APERIO_CLIENT_PRIORITY=1
wait_routable "$HOSTNAME_BIND"
# Give the standby's first heartbeat (priority announcement) time to land.
retry 20 sh -c "curl -s '$BASE/aperio/health' | grep -q '\"connected_clients\":2'" \
  || fail "both clients did not connect"
sleep 6

for i in 1 2 3 4; do
  BODY="$(curl -s -H "Host: ${HOSTNAME_BIND}" "$BASE/tier")"
  assert_contains "$BODY" "backend ${BACKEND_PORT} " "request $i goes to the primary"
done

PRIMARY_PID="${CLIENT_PIDS[0]}"
kill "$PRIMARY_PID" 2>/dev/null || true
retry 20 sh -c "curl -s -H 'Host: ${HOSTNAME_BIND}' '$BASE/tier' | grep -q 'backend ${BACKEND2_PORT} '" \
  || fail "standby did not take over after the primary died"
echo "  ok: standby takes over when the primary dies"

stop_server

step "Sticky sessions"
start_server APERIO_LB_STRATEGY='sticky'
start_client a "$BACKEND_PORT" APERIO_HOSTNAME_BIND="$HOSTNAME_BIND"
start_client b "$BACKEND2_PORT" APERIO_HOSTNAME_BIND="$HOSTNAME_BIND"
wait_routable "$HOSTNAME_BIND"
retry 20 sh -c "curl -s '$BASE/aperio/health' | grep -q '\"connected_clients\":2'" \
  || fail "both clients did not connect"

JAR="$LOG_DIR/sticky-jar.txt"
FIRST="$(curl -s -c "$JAR" -H "Host: ${HOSTNAME_BIND}" "$BASE/pin" | sed -n 's/backend \([0-9]*\) .*/\1/p')"
[ -n "$FIRST" ] || fail "could not parse the first sticky response"
grep -q 'aperio_affinity' "$JAR" || fail "sticky response did not set the affinity cookie"
for i in 1 2 3 4 5; do
  PORT_SEEN="$(curl -s -b "$JAR" -H "Host: ${HOSTNAME_BIND}" "$BASE/pin" | sed -n 's/backend \([0-9]*\) .*/\1/p')"
  [ "$PORT_SEEN" = "$FIRST" ] || fail "sticky request $i landed on backend $PORT_SEEN instead of $FIRST"
done
echo "  ok: five follow-up requests stuck to backend $FIRST"

stop_server

echo
echo "All E2E tests passed ✔"
