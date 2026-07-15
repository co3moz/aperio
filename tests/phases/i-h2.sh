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
start_client h2 "$H2_PORT" APERIO_TARGET="h2c://127.0.0.1:${H2_PORT}"
retry 30 sh -c "'$MOCK_H2_BIN' client 'http://127.0.0.1:$SERVER_PORT/echo' ping 2>/dev/null | grep -q status=200" \
  || fail "h2c tunnel did not become routable in time"
H2_OUT="$("$MOCK_H2_BIN" client "http://127.0.0.1:${SERVER_PORT}/echo" grpc-payload-123)"
assert_contains "$H2_OUT" 'status=200' "h2c request round-trips through the tunnel"
assert_contains "$H2_OUT" 'body=h2-echo:grpc-payload-123' "request body reached the HTTP/2 backend"
assert_contains "$H2_OUT" 'trailer grpc-status=0' "grpc-status trailer is relayed to the visitor"
assert_contains "$H2_OUT" 'trailer grpc-message=ok' "grpc-message trailer is relayed to the visitor"

stop_server
