#!/usr/bin/env bash
# Phase E: features. Sourced by tests/e2e.sh after the harness.
PHASE="features"

REDIR_PORT=18103

# Mock backend answering /r with a same-host redirect to the main backend and
# /ext with a redirect to an unrelated domain.

start_redirect_backend "$REDIR_PORT" "$BACKEND_PORT"

step "Starting aperio-server (features configuration)"
# The features phase accumulates many concurrent clients across its steps
# (multi-service, unix socket, per-candidate allowlist union, ...); lift the
# default 10-tunnel cap so later clients still connect.
start_server APERIO_MAX_TUNNELS=30

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
start_client redirect "$REDIR_PORT" APERIO_HOSTNAME=redir.e2e.local
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
    webhook_inbox: true
  - name: upload
    target: http://127.0.0.1:${BACKEND_PORT}
    hostname: upload.e2e.local
    max_request_body: 64
    security_headers: true
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

step "Right-to-erasure selective purge"
LOGS_BEFORE="$(curl -s -b "$COOKIES" "$BASE/aperio/api/logs")"
assert_contains "$LOGS_BEFORE" '"host":"web.e2e.local"' "the traffic log records the request hostname"
PURGE="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"hostname":"web.e2e.local"}' "$BASE/aperio/api/purge")" \
  || fail "purge request failed"
assert_contains "$PURGE" '"status":"ok"' "the purge reports per-surface removal counts"
LOGS_AFTER="$(curl -s -b "$COOKIES" "$BASE/aperio/api/logs")"
case "$LOGS_AFTER" in
  *'"host":"web.e2e.local"'*) fail "purged hostname still present in the traffic log" ;;
  *) echo "  ok: the purged hostname is gone from the traffic log" ;;
esac
assert_contains "$LOGS_AFTER" '"host":"api.e2e.local"' "other hostnames keep their log entries"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST \
  -H 'Content-Type: application/json' --data '{}' "$BASE/aperio/api/purge")"
assert_status 400 "$CODE" "a purge without selectors is rejected"

step "Webhook inbox (webhook_inbox)"
curl -s -o /dev/null -X POST -H 'Host: api.e2e.local' \
  -H 'Content-Type: application/json' --data '{"event":"invoice.paid"}' \
  "$BASE/hooks/stripe" || fail "webhook POST failed"
INBOX="$(curl -s -b "$COOKIES" "$BASE/aperio/api/inbox")"
assert_contains "$INBOX" '"/hooks/stripe"' "the inbound POST landed in the inbox"
assert_contains "$INBOX" '"api.e2e.local"' "the inbox records the hostname"
HOOK_ID="$(echo "$INBOX" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | head -1)"
[ -n "$HOOK_ID" ] || fail "could not parse an inbox entry id: $INBOX"
DETAIL="$(curl -s -b "$COOKIES" "$BASE/aperio/api/inbox/$HOOK_ID")"
assert_contains "$DETAIL" '"headers"' "the entry detail carries the headers"
REFIRE="$(curl -sf -b "$COOKIES" -X POST "$BASE/aperio/api/inbox/$HOOK_ID/refire")" \
  || fail "re-fire failed"
assert_contains "$REFIRE" '"status":200' "the re-fire reached the backend"
curl -sf -b "$COOKIES" -X DELETE "$BASE/aperio/api/inbox/$HOOK_ID" >/dev/null \
  || fail "inbox delete failed"
INBOX2="$(curl -s -b "$COOKIES" "$BASE/aperio/api/inbox")"
case "$INBOX2" in
  *"$HOOK_ID"*) fail "deleted inbox entry still listed" ;;
  *) echo "  ok: the entry is gone after delete" ;;
esac

step "Per-service request body limit (max_request_body)"
wait_routable upload.e2e.local
SMALL_CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -d 'ok' \
  -H 'Host: upload.e2e.local' "$BASE/hello")"
assert_status 200 "$SMALL_CODE" "a body under the per-service cap passes"
BIG_BODY="$(head -c 200 /dev/zero | tr '\0' 'x')"
BIG_CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -d "$BIG_BODY" \
  -H 'Host: upload.e2e.local' "$BASE/hello")"
assert_status 413 "$BIG_CODE" "a body over the per-service cap is rejected with an early 413"
OTHER_CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -d "$BIG_BODY" \
  -H 'Host: web.e2e.local' "$BASE/hello")"
assert_status 200 "$OTHER_CODE" "services without a declared cap keep the global limit"

step "Per-service security header preset (security_headers)"
SEC_HDRS="$(curl -s -D - -o /dev/null -H 'Host: upload.e2e.local' "$BASE/hello")"
assert_contains "$SEC_HDRS" "x-frame-options: DENY" "the preset injects X-Frame-Options"
assert_contains "$SEC_HDRS" "x-content-type-options: nosniff" "the preset injects nosniff"
assert_contains "$SEC_HDRS" "strict-transport-security: max-age=" "the preset injects HSTS"
PLAIN_HDRS="$(curl -s -D - -o /dev/null -H 'Host: web.e2e.local' "$BASE/hello")"
if echo "$PLAIN_HDRS" | grep -qi "x-frame-options"; then
  fail "services without the preset must not gain security headers"
fi

step "Unix socket target (unix://)"
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    echo "  ok: skipped on Windows (unix sockets unsupported)"
    ;;
  *)
    UDS_SOCK="$LOG_DIR/uds-backend.sock"
    "$PYTHON" - "$UDS_SOCK" <<'PY' >"$LOG_DIR/uds-backend.log" 2>&1 &
import http.server, os, socketserver, sys
sock = sys.argv[1]
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = ("uds backend GET %s" % self.path).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass
    def address_string(self): return "uds"
if os.path.exists(sock):
    os.unlink(sock)
socketserver.UnixStreamServer(sock, H).serve_forever()
PY
    BACKEND_PIDS+=($!)
    retry 15 test -S "$UDS_SOCK" || fail "the unix socket backend did not come up"
    env APERIO_SERVER_URL="$BASE" APERIO_SERVER_TOKEN="$TOKEN" \
      APERIO_TARGET="unix://$UDS_SOCK" APERIO_HOSTNAME=uds.e2e.local \
      "$CLIENT_BIN" >"$LOG_DIR/client-features-uds.log" 2>&1 &
    CLIENT_PIDS+=($!)
    wait_routable uds.e2e.local
    BODY="$(curl -s -H 'Host: uds.e2e.local' "$BASE/uds-hello")"
    assert_contains "$BODY" "uds backend GET /uds-hello" "requests are proxied over the unix socket"
    ;;
esac

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
  APERIO_TARGET="http://127.0.0.1:${BACKEND_PORT}" \
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

step "Per-candidate visitor IP allowlist (APERIO_ALLOWED_IPS)"
# A client that only admits a TEST-NET address: the local visitor is fully
# rejected and gets the stealth answer — indistinguishable from an
# unclaimed route (504), not a route-revealing 403.
start_client ipdeny "$BACKEND_PORT" APERIO_HOSTNAME=ipdeny.e2e.local APERIO_ALLOWED_IPS=203.0.113.7
retry 20 sh -c "curl -s -o /dev/null -m 10 -w '%{http_code}' -H 'Host: ipdeny.e2e.local' '$BASE/hello' | grep -q 504" \
  || fail "allowlist did not start rejecting in time"
CODE="$(curl -s -o /dev/null -m 10 -w '%{http_code}' -H 'Host: ipdeny.e2e.local' "$BASE/hello")"
assert_status 504 "$CODE" "a fully rejected visitor gets the stealth unclaimed-route answer"
# With a denied: redirect declared, the rejected visitor is redirected.
start_client ipredir "$BACKEND_PORT" APERIO_HOSTNAME=ipredir.e2e.local \
  APERIO_ALLOWED_IPS=203.0.113.7 APERIO_DENIED=https://example.com/denied
retry 20 sh -c "curl -s -o /dev/null -w '%{http_code}' -H 'Host: ipredir.e2e.local' '$BASE/hello' | grep -q 302" \
  || fail "the denied redirect did not take effect in time"
LOC="$(curl -s -D - -o /dev/null -H 'Host: ipredir.e2e.local' "$BASE/hello" | tr -d '\r' | awk 'tolower($1)=="location:"{print $2}')"
[ "$LOC" = "https://example.com/denied" ] || fail "expected the denied redirect, got Location: $LOC"
echo "  ok: a rejected visitor is redirected to the declared denied page"
# Per-candidate union: an unrestricted client joining the same route admits
# the visitor (route-wide lockdown belongs to the token-level IP allowlist).
start_client ipopen "$BACKEND_PORT" APERIO_HOSTNAME=ipdeny.e2e.local
wait_routable ipdeny.e2e.local
BODY="$(curl -s -H 'Host: ipdeny.e2e.local' "$BASE/hello")"
assert_contains "$BODY" "backend ${BACKEND_PORT} " "an unrestricted candidate admits the visitor (union semantics)"
# A client admitting the loopback CIDR: the local visitor passes.
start_client ipallow "$BACKEND_PORT" APERIO_HOSTNAME=ipallow.e2e.local APERIO_ALLOWED_IPS=127.0.0.0/8
wait_routable ipallow.e2e.local
BODY="$(curl -s -H 'Host: ipallow.e2e.local' "$BASE/hello")"
assert_contains "$BODY" "backend ${BACKEND_PORT} " "visitor inside the allowlist CIDR is served"

stop_server
