#!/usr/bin/env bash
# Phase D: lb. Sourced by tests/e2e.sh after the harness.
PHASE="lb"

start_backend "$BACKEND2_PORT"

step "Primary-standby strategy"
start_server APERIO_LB_STRATEGY='primary-standby'
start_client primary "$BACKEND_PORT" APERIO_HOSTNAME="$HOSTNAME_BIND"
start_client standby "$BACKEND2_PORT" APERIO_HOSTNAME="$HOSTNAME_BIND" APERIO_PRIORITY=1
wait_routable "$HOSTNAME_BIND"
# Wait for the standby's first heartbeat, which is what carries the priority:
# until it lands both clients look like tier 0 and the tier assertions below
# would be testing round-robin. Waiting for the announcement itself rather
# than for six seconds, which was the old way of spelling the same thing.
retry 20 sh -c "curl -s '$BASE/aperio/health' | grep -q '\"connected_clients\":2'" \
  || fail "both clients did not connect"
LB_COOKIES="$LOG_DIR/cookies-$PHASE.txt"
dashboard_login "$LB_COOKIES"
retry 20 sh -c "curl -s -b '$LB_COOKIES' '$BASE/aperio/api/stats' | grep -q '\"priority\":1'" \
  || fail "the standby never announced its priority"

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
start_client a "$BACKEND_PORT" APERIO_HOSTNAME="$HOSTNAME_BIND"
start_client b "$BACKEND2_PORT" APERIO_HOSTNAME="$HOSTNAME_BIND"
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
