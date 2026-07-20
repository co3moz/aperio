#!/usr/bin/env bash
# Phase C: failover. Sourced by tests/e2e.sh after the harness.
PHASE="failover"

step "Starting aperio-server with retry-wait failover"
start_server APERIO_FAILOVER='retry-wait' APERIO_FAILOVER_WINDOW=20
start_client one "$BACKEND_PORT" APERIO_HOSTNAME="$HOSTNAME_BIND"
wait_routable "$HOSTNAME_BIND"

step "In-flight failover after a mid-request client kill"
FIRST_CLIENT_PID="${CLIENT_PIDS[0]}"
curl -s -H "Host: ${HOSTNAME_BIND}" "$BASE/slow" >"$LOG_DIR/failover-response.txt" &
CURL_PID=$!
sleep 1
kill "$FIRST_CLIENT_PID" 2>/dev/null || true
start_client two "$BACKEND_PORT" APERIO_HOSTNAME="$HOSTNAME_BIND"
wait "$CURL_PID" || fail "in-flight request did not complete"
assert_contains "$(cat "$LOG_DIR/failover-response.txt")" "backend ${BACKEND_PORT} GET /slow" \
  "request survived the client kill via failover"
assert_contains "$(cat "$LOG_DIR/server-failover.log")" "In-flight failover" \
  "server logged the failover jump"

stop_server
