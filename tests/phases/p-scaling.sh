#!/usr/bin/env bash
# Phase P: autoscaling (cold start from zero). Sourced by tests/e2e.sh after
# the harness.
PHASE="scaling"

# 18120: phases I and K still hold 18110-18111 (backends live until the suite
# ends), so the mock endpoint needs a port no other phase touches.
SCALE_HOOK_PORT=18120
SCALE_HOST="scale.e2e.local"

# Stand-in for the provider's scale endpoint. It records every call and, on
# the first one, starts the very client Aperio is waiting for, which is
# exactly what a real cold start does.
cat >"$LOG_DIR/scale_hook.py" <<'PYEOF'
import http.server, json, os, subprocess, sys, threading

PORT = int(sys.argv[1])
CALLS = sys.argv[2]
CLIENT_BIN = sys.argv[3]
BASE = sys.argv[4]
TOKEN = sys.argv[5]
BACKEND = sys.argv[6]
HOSTNAME = sys.argv[7]
started = threading.Lock()
has_started = []

class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(length).decode()
        with open(CALLS, 'a') as f:
            f.write(body + "\n")
        # Only the first call starts an instance; a correct server makes
        # exactly one anyway, and this keeps the test honest if it does not.
        with started:
            if not has_started:
                has_started.append(True)
                env = dict(os.environ)
                env.update({
                    'APERIO_CONNECTIONS': '1',
                    'APERIO_SERVER_URL': BASE,
                    'APERIO_SERVER_TOKEN': TOKEN,
                    'APERIO_TARGET': BACKEND,
                    'APERIO_HOSTNAME': HOSTNAME,
                })
                subprocess.Popen([CLIENT_BIN], env=env,
                                 stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL)
        self.send_response(200)
        self.send_header('Content-Length', '2')
        self.end_headers()
        self.wfile.write(b'ok')

    def do_GET(self):
        # Readiness probe only: it must never start an instance, or the test
        # would be observing a client it started itself.
        self.send_response(200)
        self.send_header('Content-Length', '2')
        self.end_headers()
        self.wfile.write(b'up')

    def log_message(self, *args):
        pass

http.server.HTTPServer(('127.0.0.1', PORT), Handler).serve_forever()
PYEOF

step "Starting aperio-server with autoscaling enabled"
# The mock endpoint is on loopback, which the SSRF fence refuses by default;
# APERIO_SCALING_ALLOW_PRIVATE is the opt-in for an internal provider API. The
# strict default is verified in its own server below.
start_server APERIO_SCALING=1 APERIO_SCALING_ALLOW_HTTP=1 APERIO_SCALING_ALLOW_PRIVATE=1 \
  APERIO_GATEWAY_TIMEOUT=2
start_backend "$BACKEND_PORT"

CALLS_FILE="$LOG_DIR/scale-calls.jsonl"
: >"$CALLS_FILE"
"$PYTHON" "$LOG_DIR/scale_hook.py" "$SCALE_HOOK_PORT" "$CALLS_FILE" "$CLIENT_BIN" \
  "$BASE" "$TOKEN" "http://127.0.0.1:${BACKEND_PORT}" "$SCALE_HOST" \
  >"$LOG_DIR/scale-hook.log" 2>&1 &
BACKEND_PIDS+=($!)
retry 10 sh -c "curl -sf -o /dev/null http://127.0.0.1:${SCALE_HOOK_PORT}/ready" \
  || fail "the mock scale endpoint did not come up"

COOKIES="$LOG_DIR/scaling-cookies.txt"
dashboard_login "$COOKIES"

# The client is gone once it no longer appears in the live client list. This
# must never be probed with a real request: that request would itself trigger
# the cold start under test.
client_gone() { # <hostname>
  # Only the live client list counts: the persistent per-host statistics keep
  # the hostname around long after the client is gone.
  curl -s -b "$COOKIES" "$BASE/aperio/api/stats" \
    | "$PYTHON" -c 'import sys, json
host = sys.argv[1]
clients = json.load(sys.stdin).get("active_clients", [])
serving = any(host in (c.get("hostname_binds") or []) for c in clients)
sys.exit(1 if serving else 0)' "$1"
}

step "A client arms a record, then goes away"
SCALE_CFG="$LOG_DIR/scaling.yaml"
cat >"$SCALE_CFG" <<YAML
server:
  url: ${BASE}
  token: ${TOKEN}
target: http://127.0.0.1:${BACKEND_PORT}
hostname: ${SCALE_HOST}
max_concurrent: 10
scaling:
  url: http://127.0.0.1:${SCALE_HOOK_PORT}/scale
  min: 0
  max: 4
  cold_start: 30s
  cooldown: 1s
YAML
"$CLIENT_BIN" --config "$SCALE_CFG" >"$LOG_DIR/client-scaling-arm.log" 2>&1 &
ARM_PID=$!
wait_routable "$SCALE_HOST"
# The declaration is persisted against the hostname, so it survives the client.
kill "$ARM_PID" 2>/dev/null || true
retry 20 client_gone "$SCALE_HOST" || fail "the client did not disappear from routing"
echo "  ok: the armed service is gone, its record is not"

step "A request cold starts the service instead of failing"
# No client serves the hostname now: the server must call the endpoint and hold
# this request until the instance it starts is routable.
BODY="$(curl -s --max-time 40 -H "Host: ${SCALE_HOST}" "$BASE/hello")"
assert_contains "$BODY" "backend ${BACKEND_PORT}" "the held request was served after a cold start"
assert_contains "$(cat "$CALLS_FILE")" '"reason":"cold_start"' "the endpoint was called with reason=cold_start"
assert_contains "$(cat "$CALLS_FILE")" "\"hostname\":\"${SCALE_HOST}\"" "the call names the bind that needed capacity"
assert_contains "$(cat "$CALLS_FILE")" '"desired":1' "a cold start asks for one instance"
CALL_COUNT="$(wc -l <"$CALLS_FILE" | tr -d ' ')"
if [ "$CALL_COUNT" -ne 1 ]; then
  fail "expected exactly one scaling call, got $CALL_COUNT"
fi
echo "  ok: a burst produces exactly one call (single flight)"

step "The scaling API reports the armed record"
RECORDS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/scaling")"
assert_contains "$RECORDS" "\"hostname\":\"${SCALE_HOST}\"" "the record is listed"
assert_contains "$RECORDS" '"authenticated":false' "no secret was declared"
assert_contains "$RECORDS" '"instances":1' "the live pool is reported"
if echo "$RECORDS" | grep -q '"url":"http://127.0.0.1'; then
  echo "  ok: the endpoint URL is visible to the operator"
fi
RECORD_ID="$(echo "$RECORDS" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
[ -n "$RECORD_ID" ] || fail "could not parse the record id"

step "Disarming a record removes it"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE \
  "$BASE/aperio/api/scaling/$(printf '%s' "$RECORD_ID" | sed 's/|/%7C/g')")"
assert_status 200 "$CODE" "the record is disarmed"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE \
  "$BASE/aperio/api/scaling/no-such-record")"
assert_status 404 "$CODE" "an unknown record answers 404"

step "The SSRF fence refuses an internal endpoint by default"
# A fresh server with the strict defaults: the same declaration must now be
# refused before any request leaves the process.
stop_server
start_server APERIO_SCALING=1 APERIO_GATEWAY_TIMEOUT=2
dashboard_login "$COOKIES"
SSRF_CFG="$LOG_DIR/scaling-ssrf.yaml"
sed "s|${SCALE_HOST}|ssrf.e2e.local|" "$SCALE_CFG" >"$SSRF_CFG"
"$CLIENT_BIN" --config "$SSRF_CFG" >"$LOG_DIR/client-scaling-ssrf.log" 2>&1 &
SSRF_PID=$!
wait_routable ssrf.e2e.local
kill "$SSRF_PID" 2>/dev/null || true
retry 20 client_gone ssrf.e2e.local || fail "the ssrf client did not disappear from routing"
# A refused call means nothing is starting, so the request must fail fast
# rather than sit out the whole cold-start budget.
START="$(date +%s)"
CODE="$(curl -s -o /dev/null -w '%{http_code}' --max-time 45 -H 'Host: ssrf.e2e.local' "$BASE/hello")"
ELAPSED=$(( $(date +%s) - START ))
assert_status 504 "$CODE" "a refused endpoint falls through to the normal 504"
if [ "$ELAPSED" -ge 25 ]; then
  fail "a refused call held the request for ${ELAPSED}s instead of failing fast"
fi
echo "  ok: a refused call does not hold the visitor (${ELAPSED}s)"
assert_contains "$(cat "$LOG_DIR/server-$PHASE.log")" \
  "Scaling: call for ssrf.e2e.local failed" "the internal destination was refused before any request left the server"

stop_server
