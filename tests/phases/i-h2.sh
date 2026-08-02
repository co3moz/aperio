#!/usr/bin/env bash
# Phase I: h2. Sourced by tests/e2e.sh after the harness.
PHASE="h2"

step "h2c:// backend with gRPC-style trailers"
MOCK_H2_BIN="${MOCK_H2_BIN:-$TARGET_DIR/debug/mock-h2}"
if [ ! -x "$MOCK_H2_BIN" ]; then
  (cd "$ROOT" && cargo build -p mock-h2 >/dev/null 2>&1) || fail "could not build the mock-h2 helper"
fi
H2_PORT=18110
"$MOCK_H2_BIN" server "$H2_PORT" >"$LOG_DIR/mock-h2.log" 2>&1 &
CLIENT_PIDS+=($!)
# Random subdomains off and no hostname bind: the phase's only client serves
# all traffic, so the h2c visitor below needs no Host override.
start_server APERIO_RANDOM_SUBDOMAIN=
# The health probe is on, and against an h2c target that means the standard
# grpc.health.v1.Health/Check RPC rather than a GET (which this backend, like
# any prior-knowledge HTTP/2 server, would refuse). `/` asks about the server
# as a whole. The client starts out of routing until its first probe passes,
# so the retry below succeeding is itself the assertion that it did.
start_client h2 "$H2_PORT" APERIO_TARGET="h2c://127.0.0.1:${H2_PORT}" \
  APERIO_TARGET_HEALTH=/
retry 30 sh -c "'$MOCK_H2_BIN' client 'http://127.0.0.1:$SERVER_PORT/echo' ping 2>/dev/null | grep -q status=200" \
  || fail "h2c tunnel did not become routable in time"
H2_OUT="$("$MOCK_H2_BIN" client "http://127.0.0.1:${SERVER_PORT}/echo" grpc-payload-123)"
assert_contains "$H2_OUT" 'status=200' "h2c request round-trips through the tunnel"
assert_contains "$H2_OUT" 'body=h2-echo:grpc-payload-123' "request body reached the HTTP/2 backend"
assert_contains "$H2_OUT" 'trailer grpc-status=0' "grpc-status trailer is relayed to the visitor"
assert_contains "$H2_OUT" 'trailer grpc-message=ok' "grpc-message trailer is relayed to the visitor"

# The probe itself: the client says what it is checking, and traffic flowing at
# all means the gRPC health check answered SERVING.
CLIENT_LOG="$(cat "$LOG_DIR/client-h2-h2.log" 2>/dev/null)"
assert_contains "$CLIENT_LOG" "gRPC health of h2c://127.0.0.1:${H2_PORT}" \
  "the h2c backend is probed over gRPC health checking, not with a GET"
# Which line announces the pass depends on whether the very first probe won
# the race with the backend's listener ("now routable") or a later one did
# ("health restored"); either proves the gRPC check answered SERVING.
if ! echo "$CLIENT_LOG" | grep -qE 'Backend healthy:|Backend health restored:'; then
  fail "the gRPC health probe never reported the backend healthy"
fi
echo "  ok: the gRPC health probe answered SERVING and opened routing"

stop_server
