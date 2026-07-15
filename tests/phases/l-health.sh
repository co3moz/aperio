#!/usr/bin/env bash
# Phase L: health. Sourced by tests/e2e.sh after the harness.
PHASE="health"

step "Backend health probes drive routing"
start_server
HEALTH_BACKEND_PORT=18109
start_backend "$HEALTH_BACKEND_PORT"
HEALTH_BACKEND_PID=$!
start_client health "$HEALTH_BACKEND_PORT" APERIO_HOSTNAME_BIND=health.e2e.local \
  APERIO_TARGET_HEALTH=/health APERIO_HEALTH_INTERVAL=1 APERIO_HEALTH_TIMEOUT=1 APERIO_HEALTH_THRESHOLD=2
HEALTH_CLIENT_PID="${CLIENT_PIDS[${#CLIENT_PIDS[@]}-1]}"
wait_routable health.e2e.local /hello
HJAR="$LOG_DIR/cookies-health.txt"
dashboard_login "$HJAR"

[ "$(backend_health)" = "True" ] || fail "a live backend must become healthy after the first probe"

# Kill the backend: the verdict flips and the route fails closed.
kill "$HEALTH_BACKEND_PID" 2>/dev/null || true
FLIPPED=""
for _ in $(seq 1 15); do
  if [ "$(backend_health)" = "False" ]; then FLIPPED=1; break; fi
  sleep 1
done
[ -n "$FLIPPED" ] || fail "backend_healthy never turned false after the backend died"
echo "  ok: dead backend is reported unhealthy within the probe window"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Host: health.e2e.local" "$BASE/hello")"
assert_status 504 "$CODE" "an unhealthy backend is excluded from routing"

# Bring the backend back: the verdict recovers and traffic flows again.
start_backend "$HEALTH_BACKEND_PORT"
HEALTH_BACKEND_PID=$!
RECOVERED=""
for _ in $(seq 1 15); do
  if [ "$(backend_health)" = "True" ]; then RECOVERED=1; break; fi
  sleep 1
done
[ -n "$RECOVERED" ] || fail "backend_healthy never recovered after the backend returned"
echo "  ok: a returning backend is reported healthy again"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Host: health.e2e.local" "$BASE/hello")"
assert_status 200 "$CODE" "traffic flows again after recovery"

step "First probe fires immediately on a dead backend"
kill "$HEALTH_CLIENT_PID" 2>/dev/null || true
kill "$HEALTH_BACKEND_PID" 2>/dev/null || true
sleep 1
start_client health-dead "$HEALTH_BACKEND_PORT" APERIO_HOSTNAME_BIND=dead.e2e.local \
  APERIO_TARGET_HEALTH=/health APERIO_HEALTH_INTERVAL=5 APERIO_HEALTH_TIMEOUT=1 APERIO_HEALTH_THRESHOLD=1
# threshold 1 + immediate first probe: unhealthy well before one 5s interval.
EARLY=""
for _ in $(seq 1 4); do
  V="$(backend_health)"
  if [ "$V" = "False" ]; then EARLY=1; break; fi
  sleep 1
done
[ -n "$EARLY" ] || fail "the first probe did not run immediately (still healthy after 4s with a 5s interval)"
echo "  ok: a dead backend is caught by the immediate first probe"

step "A health-checked client starts unhealthy and becomes routable via the first probe"
# Long interval: if the client waited a full interval (or stayed stuck
# unhealthy), it would never become routable within wait_routable's window.
# The immediate first probe + immediate re-ping make it routable in ~1s.
lsof -tiTCP:"$HEALTH_BACKEND_PORT" -sTCP:LISTEN 2>/dev/null | xargs kill 2>/dev/null || true
start_backend "$HEALTH_BACKEND_PORT"
HEALTH_BACKEND_PID=$!
start_client health-slow "$HEALTH_BACKEND_PORT" APERIO_HOSTNAME_BIND=slow.e2e.local \
  APERIO_TARGET_HEALTH=/health APERIO_HEALTH_INTERVAL=30 APERIO_HEALTH_TIMEOUT=1 APERIO_HEALTH_THRESHOLD=1
wait_routable slow.e2e.local /hello
echo "  ok: routable well within one 30s probe interval"

stop_server
