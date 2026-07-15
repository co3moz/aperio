#!/usr/bin/env bash
# Phase E: features. Sourced by tests/e2e.sh after the harness.
PHASE="features"

REDIR_PORT=18103

# Mock backend answering /r with a same-host redirect to the main backend and
# /ext with a redirect to an unrelated domain.

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

step "Visitor IP allowlist (APERIO_ALLOWED_IPS)"
# A client that only admits a TEST-NET address: the local visitor gets 403.
start_client ipdeny "$BACKEND_PORT" APERIO_HOSTNAME_BIND=ipdeny.e2e.local APERIO_ALLOWED_IPS=203.0.113.7
retry 20 sh -c "curl -s -o /dev/null -w '%{http_code}' -H 'Host: ipdeny.e2e.local' '$BASE/hello' | grep -q 403" \
  || fail "allowlist did not start rejecting in time"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: ipdeny.e2e.local' "$BASE/hello")"
assert_status 403 "$CODE" "visitor outside the allowlist is rejected with 403"
# A client admitting the loopback CIDR: the local visitor passes.
start_client ipallow "$BACKEND_PORT" APERIO_HOSTNAME_BIND=ipallow.e2e.local APERIO_ALLOWED_IPS=127.0.0.0/8
wait_routable ipallow.e2e.local
BODY="$(curl -s -H 'Host: ipallow.e2e.local' "$BASE/hello")"
assert_contains "$BODY" "backend ${BACKEND_PORT} " "visitor inside the allowlist CIDR is served"

stop_server
