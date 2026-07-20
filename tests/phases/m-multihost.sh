#!/usr/bin/env bash
# Phase M: multihost. Sourced by tests/e2e.sh after the harness.
PHASE="multihost"

step "One service, several hostnames"
start_server APERIO_RANDOM_SUBDOMAIN=
start_backend "$BACKEND_PORT"
# A comma-separated hostname bind claims both names for the one client.
start_client multi "$BACKEND_PORT" APERIO_HOSTNAME="one.e2e.local,two.e2e.local"
wait_routable one.e2e.local /hello
BODY_ONE="$(curl -s -H "Host: one.e2e.local" "$BASE/hello")"
assert_contains "$BODY_ONE" "backend ${BACKEND_PORT}" "the first hostname routes to the service"
BODY_TWO="$(curl -s -H "Host: two.e2e.local" "$BASE/hello")"
assert_contains "$BODY_TWO" "backend ${BACKEND_PORT}" "the second hostname routes to the same service"
# An unrelated hostname is not served (no random subdomain, no bind).
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Host: nope.e2e.local" "$BASE/hello")"
assert_status 504 "$CODE" "an unclaimed hostname is not routed"

step "Per-service static serve: one client, two hostnames, two directories"
SERVE_ROOT="$(mktemp -d)"
mkdir -p "$SERVE_ROOT/site-a" "$SERVE_ROOT/site-b"
echo '<h1>site a</h1>' > "$SERVE_ROOT/site-a/index.html"
echo '<h1>site b</h1>' > "$SERVE_ROOT/site-b/index.html"
SERVE_CFG="$SERVE_ROOT/aperio.yaml"
cat > "$SERVE_CFG" <<EOF
server:
  url: $BASE
  token: $TOKEN
services:
  - name: site-a
    serve: $SERVE_ROOT/site-a
    hostname: site-a.e2e.local
  - name: site-b
    serve: $SERVE_ROOT/site-b
    hostname: site-b.e2e.local
EOF
"$CLIENT_BIN" --config "$SERVE_CFG" >"$LOG_DIR/client-$PHASE-serve.log" 2>&1 &
CLIENT_PIDS+=($!)
wait_routable site-a.e2e.local /
BODY_A="$(curl -s -H "Host: site-a.e2e.local" "$BASE/")"
assert_contains "$BODY_A" "site a" "the first hostname serves its own directory"
wait_routable site-b.e2e.local /
BODY_B="$(curl -s -H "Host: site-b.e2e.local" "$BASE/")"
assert_contains "$BODY_B" "site b" "the second hostname serves its own directory"
rm -rf "$SERVE_ROOT"
stop_server
