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
METRICS_TOKEN="e2e-metrics-token"
start_server APERIO_ACCESS_LOG="$ACCESS_LOG" APERIO_METRICS=1 APERIO_METRICS_TOKEN="$METRICS_TOKEN"
BASE_DATA_DIR="$DATA_DIR"

step "Health endpoint"
HEALTH="$(curl -s "$BASE/aperio/health")"
assert_contains "$HEALTH" '"status":"healthy"' "health reports healthy"
assert_contains "$HEALTH" '"protocol":' "health reports the tunnel protocol version"
assert_contains "$HEALTH" '"ui_language"' "health reports the default UI language"

step "First-run redirect and 504 when no client is connected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/")"
assert_status 307 "$CODE" "fresh install redirects the bare root to the dashboard"
LOCATION="$(curl -s -o /dev/null -w '%{redirect_url}' "$BASE/")"
assert_contains "$LOCATION" '/aperio' "redirect points at /aperio"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/hello")"
assert_status 504 "$CODE" "proxying without a client returns 504"

step "Tunnel proxying through a connected client"
start_client main "$BACKEND_PORT" APERIO_HOSTNAME_BIND="$HOSTNAME_BIND"
wait_routable "$HOSTNAME_BIND"

BODY="$(curl -s -H "Host: ${HOSTNAME_BIND}" "$BASE/hello?x=1")"
assert_contains "$BODY" "backend ${BACKEND_PORT} GET /hello?x=1" "GET is proxied to the backend"

BODY="$(curl -s -X POST -H "Host: ${HOSTNAME_BIND}" -H 'Content-Type: text/plain' \
  --data 'payload-123' "$BASE/submit")"
assert_contains "$BODY" "backend ${BACKEND_PORT} POST /submit body=payload-123" "POST body is proxied"

step "Large upload/download streaming (protocol v2)"
BIG="$LOG_DIR/big.bin"
"$PYTHON" -c "import sys; sys.stdout.write('A'*600000)" > "$BIG"
SIZE_OUT="$(curl -s -X POST -H "Host: ${HOSTNAME_BIND}" -H 'Content-Type: application/octet-stream' \
  --data-binary @"$BIG" "$BASE/big" | wc -c)"
[ "$SIZE_OUT" -ge 600000 ] || fail "streamed upload/download returned only $SIZE_OUT bytes"
echo "  ok: 600 KB body streamed both ways ($SIZE_OUT bytes echoed)"

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
assert_contains "$SETTINGS" '"environment"' "settings expose the env-only flag report"
assert_contains "$SETTINGS" 'APERIO_TRUST_PROXY' "env report lists the proxy trust flag"
assert_contains "$SETTINGS" '"cache_enabled"' "settings expose the response cache toggle"
assert_contains "$SETTINGS" '"ui_language":"en"' "settings expose the default UI language"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"cache_enabled":true,"max_concurrent_requests":64,"login_lockout_threshold":7}' "$BASE/aperio/api/settings")"
assert_status 200 "$CODE" "new runtime settings can be updated"
SETTINGS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/settings")"
assert_contains "$SETTINGS" '"max_concurrent_requests":64' "concurrency limit applied live"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"lb_strategy":"bogus"}' "$BASE/aperio/api/settings")"
assert_status 400 "$CODE" "invalid settings are rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"cache_max_bytes":0}' "$BASE/aperio/api/settings")"
assert_status 400 "$CODE" "zero cache budget is rejected"
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
EPHEMERAL_PID=$!
CLIENT_PIDS+=($EPHEMERAL_PID)
wait_routable "$EPHEMERAL_HOST" "/preview"
BODY="$(curl -s -H "Host: ${EPHEMERAL_HOST}" "$BASE/preview")"
assert_contains "$BODY" "backend ${BACKEND_PORT} GET /preview" "ephemeral tunnel proxies to the backend"

CODE="$(curl -s -o /dev/null -w '%{http_code}' -X DELETE \
  -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/api/tunnels/${TUNNEL_ID}")"
assert_status 200 "$CODE" "tunnel revocation succeeds"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X DELETE \
  -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/api/tunnels/${TUNNEL_ID}")"
assert_status 404 "$CODE" "revoking the same tunnel twice returns 404"

# Stop the ephemeral client and wait for the server to drop it, so only `main`
# stays connected. Otherwise the client-control test below reads
# active_clients[0] non-deterministically and may disable the ephemeral client
# (bound to its own random subdomain) rather than the one serving app.e2e.local.
kill "$EPHEMERAL_PID" 2>/dev/null || true
wait "$EPHEMERAL_PID" 2>/dev/null || true
only_main_connected() {
  test "$(curl -s -b "$COOKIES" "$BASE/aperio/api/stats" \
    | "$PYTHON" -c 'import sys,json; print(len(json.load(sys.stdin).get("active_clients", [])))')" -eq 1
}
retry 15 only_main_connected \
  || fail "ephemeral client did not disconnect before the client-control test"

step "Prometheus metrics"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/aperio/metrics")"
assert_status 401 "$CODE" "metrics without a token are rejected"
METRICS="$(curl -s "$BASE/aperio/metrics?token=${METRICS_TOKEN}")"
assert_contains "$METRICS" 'aperio_requests_total' "metrics expose the request counter"
assert_contains "$METRICS" 'aperio_connected_clients' "metrics expose the client gauge"
assert_contains "$METRICS" 'aperio_request_duration_seconds_bucket' "metrics expose the duration histogram"
assert_contains "$METRICS" 'aperio_token_requests_total{token="' "metrics expose per-token counters"
assert_contains "$METRICS" 'aperio_hostname_requests_total{hostname="' "metrics expose per-hostname counters"
METRICS="$(curl -s -H "Authorization: Bearer ${METRICS_TOKEN}" "$BASE/aperio/metrics")"
assert_contains "$METRICS" 'aperio_requests_total' "metrics accept the Bearer form too"

step "Request inspector & replay"
curl -s -H "Host: ${HOSTNAME_BIND}" "$BASE/inspect-me" >/dev/null
REQ_ID="$(curl -s -b "$COOKIES" "$BASE/aperio/api/logs" | "$PYTHON" -c \
  "import sys,json; print(next(l['id'] for l in json.load(sys.stdin) if l['uri'].startswith('/inspect-me')))")" \
  || fail "could not find the /inspect-me request in the logs"
DETAIL="$(curl -s -b "$COOKIES" "$BASE/aperio/api/requests/${REQ_ID}")"
assert_contains "$DETAIL" '"method":"GET"' "inspector captures the request method"
assert_contains "$DETAIL" '/inspect-me' "inspector captures the request uri"
REPLAY="$(curl -s -b "$COOKIES" -X POST "$BASE/aperio/api/requests/${REQ_ID}/replay")"
assert_contains "$REPLAY" '"status":200' "replay reaches the backend again"
assert_contains "$REPLAY" '"replayed_id"' "replay reports the replayed id"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" "$BASE/aperio/api/requests/no-such-id")"
assert_status 404 "$CODE" "unknown capture ids answer 404"

step "Webhooks API"
HOOK="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"name\":\"e2e-hook\",\"url\":\"http://127.0.0.1:${BACKEND_PORT}/hook\",\"events\":[\"client_connected\"]}" \
  "$BASE/aperio/api/webhooks")" || fail "webhook creation failed"
HOOK_ID="$(echo "$HOOK" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
[ -n "$HOOK_ID" ] || fail "could not parse the webhook response: $HOOK"
LIST="$(curl -s -b "$COOKIES" "$BASE/aperio/api/webhooks")"
assert_contains "$LIST" 'e2e-hook' "webhook list contains the created hook"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"bad","url":"ftp://nope"}' "$BASE/aperio/api/webhooks")"
assert_status 400 "$CODE" "non-http webhook URLs are rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/webhooks/${HOOK_ID}")"
assert_status 200 "$CODE" "webhook deletion succeeds"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/webhooks/${HOOK_ID}")"
assert_status 404 "$CODE" "deleting the same webhook twice returns 404"

step "Audit API"
AUDIT="$(curl -s -b "$COOKIES" "$BASE/aperio/api/audit")"
assert_contains "$AUDIT" 'client_connected' "audit log records the client connection"
assert_contains "$AUDIT" 'webhook_created' "audit log records the webhook creation"

step "OpenAPI spec"
SPEC="$(curl -s -b "$COOKIES" "$BASE/aperio/api/openapi.json")"
assert_contains "$SPEC" '"openapi"' "openapi document is served"
assert_contains "$SPEC" '/aperio/api/tokens/refresh' "openapi document covers the token refresh endpoint"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/aperio/api/openapi.json")"
assert_status 302 "$CODE" "openapi document requires a dashboard session"

step "Structured access log"
[ -f "$ACCESS_LOG" ] || fail "access log file was not created"
assert_contains "$(cat "$ACCESS_LOG")" '"uri":"/hello' "access log records proxied requests"
assert_contains "$(cat "$ACCESS_LOG")" '"token":"master"' "access log attributes the token"

step "Token API lifecycle (list, edit, revoke)"
TOK="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"e2e-edit","hostnames":["edit.e2e.local"]}' "$BASE/aperio/api/tokens")" \
  || fail "token creation failed"
EDIT_ID="$(echo "$TOK" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
[ -n "$EDIT_ID" ] || fail "could not parse the token id: $TOK"
LIST="$(curl -s -b "$COOKIES" "$BASE/aperio/api/tokens")"
assert_contains "$LIST" '"name":"e2e-edit"' "token list includes the created token"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"hostnames":["edited.e2e.local"],"max_rps":5,"ttl_seconds":600,"allow_public":true}' \
  "$BASE/aperio/api/tokens/${EDIT_ID}")"
assert_status 200 "$CODE" "token scope can be edited"
LIST="$(curl -s -b "$COOKIES" "$BASE/aperio/api/tokens")"
assert_contains "$LIST" 'edited.e2e.local' "the edit updated the hostname scope"
assert_contains "$LIST" '"allow_public":true' "the edit set the public-publish flag"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"hostnames":["bad host"]}' "$BASE/aperio/api/tokens/${EDIT_ID}")"
assert_status 400 "$CODE" "an invalid hostname permission is rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"name":"x"}' "$BASE/aperio/api/tokens/no-such-token")"
assert_status 404 "$CODE" "editing an unknown token returns 404"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/tokens/${EDIT_ID}")"
assert_status 200 "$CODE" "token revocation succeeds"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/tokens/${EDIT_ID}")"
assert_status 404 "$CODE" "revoking the same token twice returns 404"

step "Client control API (overrule + kill switch)"
CLIENT_ID="$(curl -s -b "$COOKIES" "$BASE/aperio/api/stats" | "$PYTHON" -c \
  "import sys,json; print(json.load(sys.stdin)['active_clients'][0]['id'])")" \
  || fail "could not read the client id from stats"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"hostname_bind":"bad host"}' "$BASE/aperio/api/clients/${CLIENT_ID}/override")"
assert_status 400 "$CODE" "an invalid override hostname is rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"path_bind":"/x"}' "$BASE/aperio/api/clients/no-such-client/override")"
assert_status 404 "$CODE" "overruling an unknown client returns 404"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"path_bind":"/ov"}' "$BASE/aperio/api/clients/${CLIENT_ID}/override")"
assert_status 200 "$CODE" "a path override can be applied"
STATS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/stats")"
assert_contains "$STATS" '"override_path_bind":"/ov"' "stats reflect the applied override"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"path_bind":"","hostname_bind":""}' "$BASE/aperio/api/clients/${CLIENT_ID}/override")"
assert_status 200 "$CODE" "the override can be cleared"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"enabled":false}' "$BASE/aperio/api/clients/${CLIENT_ID}/enabled")"
assert_status 200 "$CODE" "the client can be disabled (kill switch)"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Host: ${HOSTNAME_BIND}" "$BASE/hello")"
assert_status 504 "$CODE" "a disabled client no longer receives traffic"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"enabled":true}' "$BASE/aperio/api/clients/no-such-client/enabled")"
assert_status 404 "$CODE" "toggling an unknown client returns 404"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"enabled":true}' "$BASE/aperio/api/clients/${CLIENT_ID}/enabled")"
assert_status 200 "$CODE" "the client can be re-enabled"
retry 10 sh -c "curl -s -H 'Host: ${HOSTNAME_BIND}' '$BASE/hello' | grep -q 'backend ${BACKEND_PORT} GET /hello'" \
  || fail "traffic did not resume after re-enabling the client"
echo "  ok: traffic resumes after re-enabling the client"

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

step "Public service opt-out of the visitor gate"
# Master token always may publish public services: this hostname bypasses auth.
start_client public "$BACKEND_PORT" APERIO_HOSTNAME_BIND=pub.e2e.local APERIO_PUBLIC=1
retry 20 sh -c "curl -s -H 'Host: pub.e2e.local' '$BASE/hello' | grep -q 'backend ${BACKEND_PORT} GET /hello'" \
  || fail "public client did not bypass the visitor gate"
echo "  ok: public client serves without login"
# The protected hostname keeps its gate.
RESP="$(curl -s -D - -o /dev/null -H "Host: ${HOSTNAME_BIND}" "$BASE/hello")"
assert_contains "$RESP" "/aperio/auth" "protected hostname still redirects to login"
# A dynamic token WITHOUT the allow_public permission cannot bypass the gate.
NP="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"nopublic","hostnames":["priv.e2e.local"]}' "$BASE/aperio/api/tokens")" \
  || fail "token creation failed"
NP_TOKEN="$(echo "$NP" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
[ -n "$NP_TOKEN" ] || fail "could not parse the token response: $NP"
env APERIO_SERVER_URL="$BASE" APERIO_SERVER_TOKEN="$NP_TOKEN" \
  APERIO_CLIENT_TARGET="http://127.0.0.1:${BACKEND_PORT}" APERIO_PUBLIC=1 \
  "$CLIENT_BIN" >"$LOG_DIR/client-auth-nopublic.log" 2>&1 &
CLIENT_PIDS+=($!)
retry 20 sh -c "grep -q 'does not permit publishing public' '$LOG_DIR/server-auth.log'" \
  || fail "server did not log the denied public declaration"
RESP="$(curl -s -D - -o /dev/null -H "Host: priv.e2e.local" "$BASE/hello")"
assert_contains "$RESP" "/aperio/auth" "unpermitted public declaration keeps the gate"

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

##############################################################################
PHASE="features"
##############################################################################

REDIR_PORT=18103

# Mock backend answering /r with a same-host redirect to the main backend and
# /ext with a redirect to an unrelated domain.
start_redirect_backend() { # <port> <target_port>
  "$PYTHON" - "$1" "$2" <<'PYEOF' >"$LOG_DIR/backend-redirect.log" 2>&1 &
import http.server, sys

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith('/ext'):
            location = 'http://unrelated.invalid/'
        else:
            location = f'http://127.0.0.1:{sys.argv[2]}/hello'
        self.send_response(301)
        self.send_header('Location', location)
        self.send_header('Content-Length', '0')
        self.end_headers()

    def log_message(self, *args):
        pass

http.server.HTTPServer(('127.0.0.1', int(sys.argv[1])), Handler).serve_forever()
PYEOF
  BACKEND_PIDS+=($!)
  retry 10 sh -c "curl -s -o /dev/null 'http://127.0.0.1:$1/ping'" \
    || fail "redirect backend :$1 did not come up"
}

start_redirect_backend "$REDIR_PORT" "$BACKEND_PORT"

step "Starting aperio-server (features configuration)"
start_server

step "Positional-target CLI form"
"$CLIENT_BIN" "127.0.0.1:${BACKEND_PORT}" \
  --server-url "$BASE" --server-token "$TOKEN" --hostname cli.e2e.local \
  >"$LOG_DIR/client-features-cli.log" 2>&1 &
CLIENT_PIDS+=($!)
wait_routable cli.e2e.local
BODY="$(curl -s -H 'Host: cli.e2e.local' "$BASE/hello")"
assert_contains "$BODY" "backend ${BACKEND_PORT} GET /hello" "positional target is proxied"

step "check reports value provenance"
CHECK_OUT="$(env APERIO_TARGET="http://127.0.0.1:${BACKEND_PORT}" \
  "$CLIENT_BIN" check --server-url "$BASE" --server-token "$TOKEN")" \
  || fail "check exited non-zero: $CHECK_OUT"
assert_contains "$CHECK_OUT" "All checks passed" "check passes end to end"
assert_contains "$CHECK_OUT" "WS handshake" "check reports the token handshake round-trip"
assert_contains "$CHECK_OUT" "(from CLI argument)" "check shows the CLI layer"
assert_contains "$CHECK_OUT" "(from environment)" "check shows the environment layer"

step "check reports failures against an unreachable server/target"
if CHECK_FAIL="$(env APERIO_TARGET='http://127.0.0.1:19191' APERIO_TARGET_HEALTH='/health' \
  "$CLIENT_BIN" check --server-url 'http://127.0.0.1:19191' --server-token bogus 2>&1)"; then
  CHECK_RC=0
else
  CHECK_RC=$?
fi
assert_contains "$CHECK_FAIL" "FAIL  server health" "check flags the unreachable server"
assert_contains "$CHECK_FAIL" "FAIL  target" "check flags the unreachable target"
assert_contains "$CHECK_FAIL" "check(s) failed" "check summarizes the failures"
[ "$CHECK_RC" -eq 1 ] || fail "check should exit 1 on failures (got $CHECK_RC)"

step "Same-site redirect following"
start_client redirect "$REDIR_PORT" APERIO_HOSTNAME_BIND=redir.e2e.local
wait_routable redir.e2e.local "/r"
BODY="$(curl -s -H 'Host: redir.e2e.local' "$BASE/r")"
assert_contains "$BODY" "backend ${BACKEND_PORT} GET /hello" "same-host redirect is followed transparently"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: redir.e2e.local' "$BASE/ext")"
assert_status 301 "$CODE" "cross-site redirect passes through to the visitor"

step "Multi-service client (services: list)"
MS_CFG="$LOG_DIR/multi-service.yaml"
cat >"$MS_CFG" <<YAML
server:
  url: ${BASE}
  token: ${TOKEN}
services:
  - name: web
    target: http://127.0.0.1:${BACKEND_PORT}
    hostname: web.e2e.local
  - name: api
    target: http://127.0.0.1:${BACKEND2_PORT}
    hostname: api.e2e.local
YAML
"$CLIENT_BIN" --config "$MS_CFG" >"$LOG_DIR/client-features-multi.log" 2>&1 &
CLIENT_PIDS+=($!)
wait_routable web.e2e.local
wait_routable api.e2e.local
BODY="$(curl -s -H 'Host: web.e2e.local' "$BASE/hello")"
assert_contains "$BODY" "backend ${BACKEND_PORT} " "service 'web' routes to its backend"
BODY="$(curl -s -H 'Host: api.e2e.local' "$BASE/hello")"
assert_contains "$BODY" "backend ${BACKEND2_PORT} " "service 'api' routes to its backend"
COOKIES="$LOG_DIR/cookies-features.txt"
dashboard_login "$COOKIES"
STATS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/stats")"
assert_contains "$STATS" '"service":"web"' "stats show the announced service name"

step "~/.aperio.yaml user-level layer"
HOME_DIR="$(mktemp -d)"
cat >"$HOME_DIR/.aperio.yaml" <<YAML
server:
  url: ${BASE}
  token: ${TOKEN}
YAML
env HOME="$HOME_DIR" USERPROFILE="$HOME_DIR" \
  APERIO_TARGET="http://127.0.0.1:${BACKEND_PORT}" APERIO_HOSTNAME=home.e2e.local \
  "$CLIENT_BIN" >"$LOG_DIR/client-features-home.log" 2>&1 &
CLIENT_PIDS+=($!)
wait_routable home.e2e.local
BODY="$(curl -s -H 'Host: home.e2e.local' "$BASE/hello")"
assert_contains "$BODY" "backend ${BACKEND_PORT} " "server url/token came from ~/.aperio.yaml"

step "Per-token rate limit"
RL="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"rl","hostnames":["rl.e2e.local"],"max_rps":1}' "$BASE/aperio/api/tokens")" \
  || fail "rate-limited token creation failed"
RL_TOKEN="$(echo "$RL" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
[ -n "$RL_TOKEN" ] || fail "could not parse the token response: $RL"
env APERIO_SERVER_URL="$BASE" APERIO_SERVER_TOKEN="$RL_TOKEN" \
  APERIO_CLIENT_TARGET="http://127.0.0.1:${BACKEND_PORT}" \
  "$CLIENT_BIN" >"$LOG_DIR/client-features-rl.log" 2>&1 &
CLIENT_PIDS+=($!)
wait_routable rl.e2e.local
# Let the bucket refill after the routability probe consumed its one token,
# so the burst below sees both a success and 429s.
sleep 2
RL_CODES=""
for i in 1 2 3 4 5 6 7 8; do
  RL_CODES="$RL_CODES $(curl -s -o /dev/null -w '%{http_code}' -H 'Host: rl.e2e.local' "$BASE/limited")"
done
assert_contains "$RL_CODES" "200" "some requests pass within the rate limit"
assert_contains "$RL_CODES" "429" "excess requests are rejected with 429"

stop_server

##############################################################################
PHASE="ws"
##############################################################################

WS_PORT=18104

# Minimal RFC6455 WebSocket echo backend (stdlib only): answers plain HTTP
# with 200/ok (so wait_routable works), upgrades WS handshakes, and echoes
# every text/binary frame back prefixed with "echo:".
start_ws_backend() { # <port>
  "$PYTHON" - "$1" <<'PYEOF' >"$LOG_DIR/backend-ws.log" 2>&1 &
import base64, hashlib, socket, sys, threading

MAGIC = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

def recvn(c, n):
    buf = b""
    while len(buf) < n:
        d = c.recv(n - len(buf))
        if not d:
            return None
        buf += d
    return buf

def handle(c):
    data = b""
    while b"\r\n\r\n" not in data:
        d = c.recv(4096)
        if not d:
            return
        data += d
    key = ""
    for line in data.decode("latin1").split("\r\n"):
        if line.lower().startswith("sec-websocket-key:"):
            key = line.split(":", 1)[1].strip()
    if not key:
        c.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
        c.close()
        return
    accept = base64.b64encode(hashlib.sha1((key + MAGIC).encode()).digest()).decode()
    c.sendall(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
               "Connection: Upgrade\r\nSec-WebSocket-Accept: %s\r\n\r\n" % accept).encode())
    while True:
        hdr = recvn(c, 2)
        if hdr is None:
            return
        opcode = hdr[0] & 0x0F
        ln = hdr[1] & 0x7F
        masked = hdr[1] & 0x80
        if ln == 126:
            ln = int.from_bytes(recvn(c, 2), "big")
        elif ln == 127:
            ln = int.from_bytes(recvn(c, 8), "big")
        mask = recvn(c, 4) if masked else b""
        payload = recvn(c, ln) if ln else b""
        if payload is None:
            return
        if masked and payload:
            payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        if opcode == 8:
            c.sendall(b"\x88\x00")
            c.close()
            return
        if opcode in (1, 2):
            resp = b"echo:" + payload
            c.sendall(bytes([0x80 | opcode, len(resp)]) + resp)

srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", int(sys.argv[1])))
srv.listen(8)
while True:
    conn, _ = srv.accept()
    threading.Thread(target=handle, args=(conn,), daemon=True).start()
PYEOF
  BACKEND_PIDS+=($!)
  retry 10 curl -sf "http://127.0.0.1:$1/ping" || fail "ws backend :$1 did not come up"
}

# WebSocket probe: connects THROUGH the aperio server (Host header routing),
# performs the upgrade, sends one masked text frame, prints the echoed frame,
# and closes cleanly.
cat >"$LOG_DIR/ws_probe.py" <<'PYEOF'
import base64, os, socket, sys

port, host = int(sys.argv[1]), sys.argv[2]
s = socket.create_connection(("127.0.0.1", port), timeout=15)
key = base64.b64encode(os.urandom(16)).decode()
s.sendall(("GET /ws-echo HTTP/1.1\r\nHost: %s\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
           "Sec-WebSocket-Key: %s\r\nSec-WebSocket-Version: 13\r\n\r\n" % (host, key)).encode())
resp = b""
while b"\r\n\r\n" not in resp:
    d = s.recv(4096)
    if not d:
        print("no handshake response")
        sys.exit(1)
    resp += d
if b" 101" not in resp.split(b"\r\n", 1)[0]:
    print("handshake failed: %r" % resp[:200])
    sys.exit(1)

def recvn(n):
    buf = b""
    while len(buf) < n:
        d = s.recv(n - len(buf))
        if not d:
            print("connection closed early")
            sys.exit(1)
        buf += d
    return buf

payload = b"hello-ws"
mask = os.urandom(4)
s.sendall(bytes([0x81, 0x80 | len(payload)]) + mask +
          bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))
hdr = recvn(2)
data = recvn(hdr[1] & 0x7F)
print(data.decode("latin1"))
# Close cleanly so the WsClose path is exercised end to end.
mask = os.urandom(4)
s.sendall(bytes([0x88, 0x80]) + mask)
s.close()
sys.exit(0 if data == b"echo:hello-ws" else 1)
PYEOF

step "WebSocket pass-through"
start_server
start_ws_backend "$WS_PORT"
start_client ws "$WS_PORT" APERIO_HOSTNAME_BIND=ws.e2e.local
wait_routable ws.e2e.local "/ping"
WS_OUT="$("$PYTHON" "$LOG_DIR/ws_probe.py" "$SERVER_PORT" ws.e2e.local)" \
  || fail "ws probe failed: $WS_OUT"
assert_contains "$WS_OUT" "echo:hello-ws" "WS frame echoed through the tunnel"

stop_server

##############################################################################
PHASE="tunnels"
##############################################################################

ECHO_PORT=18105
BIND_PORT=18106
BRIDGE_PORT=18107

# Raw TCP echo backend: echoes every received chunk prefixed with "echo:".
start_tcp_echo() { # <port>
  "$PYTHON" - "$1" <<'PYEOF' >"$LOG_DIR/backend-tcpecho.log" 2>&1 &
import socket, sys, threading

def go(c):
    while True:
        d = c.recv(4096)
        if not d:
            break
        c.sendall(b"echo:" + d)
    c.close()

srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", int(sys.argv[1])))
srv.listen(8)
while True:
    c, _ = srv.accept()
    threading.Thread(target=go, args=(c,), daemon=True).start()
PYEOF
  BACKEND_PIDS+=($!)
}

# TCP probe: sends argv[2] to 127.0.0.1:argv[1] and prints the response.
cat >"$LOG_DIR/tcp_probe.py" <<'PYEOF'
import socket, sys, time

s = socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=15)
msg = sys.argv[2].encode()
s.sendall(msg)
want = len(b"echo:") + len(msg)
buf = b""
deadline = time.time() + 15
while len(buf) < want and time.time() < deadline:
    d = s.recv(4096)
    if not d:
        break
    buf += d
print(buf.decode("latin1"))
PYEOF

step "Starting aperio-server (emergency tunnels configuration)"
start_server
start_tcp_echo "$ECHO_PORT"

DECL_ID="$("$PYTHON" -c 'import uuid; print(uuid.uuid4())')"
DECL_CFG="$LOG_DIR/decl.yaml"
cat >"$DECL_CFG" <<YAML
server:
  url: ${BASE}
  token: ${TOKEN}
client_id: ${DECL_ID}
target: http://127.0.0.1:${BACKEND_PORT}
hostname: decl.e2e.local
tcp_target: 127.0.0.1:${ECHO_PORT}
tunnels:
  - target: 127.0.0.1:${ECHO_PORT}
    protocol: tcp
YAML
"$CLIENT_BIN" --config "$DECL_CFG" >"$LOG_DIR/client-tunnels-decl.log" 2>&1 &
CLIENT_PIDS+=($!)
wait_routable decl.e2e.local

step "Tunnel discovery endpoint"
retry 20 sh -c "curl -sf -H 'Authorization: Bearer ${TOKEN}' '$BASE/aperio/tunnels/${DECL_ID}' \
  | grep -q '127.0.0.1:${ECHO_PORT}'" \
  || fail "declared tunnels did not become discoverable in time"
TUNNELS="$(curl -s -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/tunnels/${DECL_ID}")"
assert_contains "$TUNNELS" "\"127.0.0.1:${ECHO_PORT}\"" "discovery lists the declared target"
assert_contains "$TUNNELS" '"protocol":"tcp"' "discovery lists the protocol"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/tunnels/no-such-client")"
assert_status 404 "$CODE" "unknown client ids answer 404"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/aperio/tunnels/${DECL_ID}")"
assert_status 401 "$CODE" "discovery without a token is rejected"
COOKIES="$LOG_DIR/cookies-tunnels.txt"
dashboard_login "$COOKIES"
OTHER="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"other-token"}' "$BASE/aperio/api/tokens")" || fail "token creation failed"
OTHER_TOKEN="$(echo "$OTHER" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
[ -n "$OTHER_TOKEN" ] || fail "could not parse the token response: $OTHER"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${OTHER_TOKEN}" "$BASE/aperio/tunnels/${DECL_ID}")"
assert_status 403 "$CODE" "a different valid token is rejected (same-token rule)"

step "Binding tunnels with --bind-tunnels (port override)"
BT_CFG="$LOG_DIR/binder.yaml"
cat >"$BT_CFG" <<YAML
bind-tunnels:
  '${DECL_ID}':
    token: ${TOKEN}
    override:
      '127.0.0.1:${ECHO_PORT}': ${BIND_PORT}
YAML
"$CLIENT_BIN" --bind-tunnels "$DECL_ID" --config "$BT_CFG" --server-url "$BASE" \
  >"$LOG_DIR/client-tunnels-binder.log" 2>&1 &
CLIENT_PIDS+=($!)
retry 30 sh -c "\"$PYTHON\" '$LOG_DIR/tcp_probe.py' $BIND_PORT ping | grep -q 'echo:ping'" \
  || fail "bound tunnel did not become usable in time"
OUT="$("$PYTHON" "$LOG_DIR/tcp_probe.py" "$BIND_PORT" ping-123)"
assert_contains "$OUT" "echo:ping-123" "bytes relayed through the bound tunnel"
assert_contains "$(cat "$LOG_DIR/client-tunnels-binder.log")" "Tunnel bound: 127.0.0.1:${BIND_PORT}" \
  "binder honored the port override"

step "Legacy tcp bridge"
"$CLIENT_BIN" tcp "$BRIDGE_PORT" --server-url "$BASE" --server-token "$TOKEN" \
  >"$LOG_DIR/client-tunnels-bridge.log" 2>&1 &
CLIENT_PIDS+=($!)
retry 30 sh -c "\"$PYTHON\" '$LOG_DIR/tcp_probe.py' $BRIDGE_PORT ping | grep -q 'echo:ping'" \
  || fail "legacy tcp bridge did not become usable in time"
OUT="$("$PYTHON" "$LOG_DIR/tcp_probe.py" "$BRIDGE_PORT" ping-legacy)"
assert_contains "$OUT" "echo:ping-legacy" "bytes relayed through the legacy bridge"

stop_server

##############################################################################
PHASE="subdomain"
##############################################################################

step "Same-level random subdomain pattern"
start_server APERIO_RANDOM_SUBDOMAIN='*-pi.e2e.local'
TUNNEL="$(curl -sf -X POST -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
  --data '{"name":"pattern","ttl_seconds":300}' "$BASE/aperio/api/tunnels")" \
  || fail "tunnel provisioning under the pattern failed"
PATTERN_HOST="$(echo "$TUNNEL" | sed -n 's/.*"hostname":"\([^"]*\)".*/\1/p')"
case "$PATTERN_HOST" in
  *-pi.e2e.local) echo "  ok: pattern hostname generated: $PATTERN_HOST" ;;
  *) fail "expected a *-pi.e2e.local hostname, got: $PATTERN_HOST" ;;
esac
case "$PATTERN_HOST" in
  *'*'*) fail "generated hostname still contains the placeholder: $PATTERN_HOST" ;;
  *) echo "  ok: placeholder fully substituted" ;;
esac

stop_server

echo
echo "All E2E tests passed ✔"
