#!/usr/bin/env bash
# Phase M: multihost. Sourced by tests/e2e.sh after the harness.
PHASE="multihost"

step "One service, several hostnames"
start_server APERIO_RANDOM_SUBDOMAIN=
start_backend "$BACKEND_PORT"
# A comma-separated hostname bind claims both names for the one client.
start_client multi "$BACKEND_PORT" APERIO_HOSTNAME_BIND="one.e2e.local,two.e2e.local"
wait_routable one.e2e.local /hello
BODY_ONE="$(curl -s -H "Host: one.e2e.local" "$BASE/hello")"
assert_contains "$BODY_ONE" "backend ${BACKEND_PORT}" "the first hostname routes to the service"
BODY_TWO="$(curl -s -H "Host: two.e2e.local" "$BASE/hello")"
assert_contains "$BODY_TWO" "backend ${BACKEND_PORT}" "the second hostname routes to the same service"
# An unrelated hostname is not served (no random subdomain, no bind).
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Host: nope.e2e.local" "$BASE/hello")"
assert_status 504 "$CODE" "an unclaimed hostname is not routed"
stop_server
