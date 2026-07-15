#!/usr/bin/env bash
# Phase B: auth. Sourced by tests/e2e.sh after the harness.
PHASE="auth"

step "Starting aperio-server with a visitor password"
start_server APERIO_SERVER_AUTH='demo:secret123'
start_client main "$BACKEND_PORT" APERIO_HOSTNAME_BIND="$HOSTNAME_BIND"
retry 20 curl -s -o /dev/null "$BASE/aperio/health" || true
# Wait until the client is registered (login page would mask wait_routable).
retry 20 sh -c "curl -s '$BASE/aperio/health' | grep -q '\"connected_clients\":1'" \
  || fail "client did not connect in the auth phase"

step "Visitor password gate"
RESP="$(curl -s -D - -o /dev/null -H "Host: ${HOSTNAME_BIND}" "$BASE/hello")"
assert_contains "$RESP" "302" "unauthenticated visitors are redirected"
assert_contains "$RESP" "/aperio/auth" "redirect goes to the login page"

step "Share link flow"
COOKIES="$LOG_DIR/cookies-auth.txt"
dashboard_login "$COOKIES"
SHARE="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"hostname\":\"$HOSTNAME_BIND\",\"ttl_seconds\":300}" "$BASE/aperio/api/share")" \
  || fail "share link creation failed"
SHARE_TOKEN="$(echo "$SHARE" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
[ -n "$SHARE_TOKEN" ] || fail "could not parse the share response: $SHARE"

RESP="$(curl -s -D - -o /dev/null -H "Host: ${HOSTNAME_BIND}" "$BASE/hello?aperio_share=${SHARE_TOKEN}")"
assert_contains "$RESP" "302" "share link answers with a redirect"
assert_contains "$RESP" "aperio_share=" "share link sets the cookie"
assert_contains "$RESP" "location: /hello" "share link redirects to the clean URL"

# The hostname bind arrives with the client's first heartbeat (~5s); the
# share cookie skips the login page, so it doubles as the routability probe.
retry 20 sh -c "curl -s -H 'Host: ${HOSTNAME_BIND}' -H 'Cookie: aperio_share=${SHARE_TOKEN}' '$BASE/hello' \
  | grep -q 'backend ${BACKEND_PORT} GET /hello'" \
  || fail "share cookie did not grant access in time"
echo "  ok: share cookie grants access"

RESP="$(curl -s -D - -o /dev/null -H "Host: other.e2e.local" -H "Cookie: aperio_share=${SHARE_TOKEN}" "$BASE/hello")"
assert_contains "$RESP" "302" "share cookie does not cover other hostnames"

RESP="$(curl -s -D - -o /dev/null -H "Host: ${HOSTNAME_BIND}" "$BASE/hello?aperio_share=tampered.token")"
assert_contains "$RESP" "/aperio/auth" "a tampered share token is rejected"

step "Public service opt-out of the visitor gate"
# Master token always may publish public services: this hostname bypasses auth.
start_client public "$BACKEND_PORT" APERIO_HOSTNAME_BIND=pub.e2e.local APERIO_PUBLIC=1
retry 20 sh -c "curl -s -H 'Host: pub.e2e.local' '$BASE/hello' | grep -q 'backend ${BACKEND_PORT} GET /hello'" \
  || fail "public client did not bypass the visitor gate"
echo "  ok: public client serves without login"
# The protected hostname keeps its gate.
RESP="$(curl -s -D - -o /dev/null -H "Host: ${HOSTNAME_BIND}" "$BASE/hello")"
assert_contains "$RESP" "/aperio/auth" "protected hostname still redirects to login"
# A dynamic token WITHOUT the allow_public permission cannot bypass the gate.
NP="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"nopublic","hostnames":["priv.e2e.local"]}' "$BASE/aperio/api/tokens")" \
  || fail "token creation failed"
NP_TOKEN="$(echo "$NP" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
[ -n "$NP_TOKEN" ] || fail "could not parse the token response: $NP"
env APERIO_SERVER_URL="$BASE" APERIO_SERVER_TOKEN="$NP_TOKEN" \
  APERIO_CLIENT_TARGET="http://127.0.0.1:${BACKEND_PORT}" APERIO_PUBLIC=1 \
  "$CLIENT_BIN" >"$LOG_DIR/client-auth-nopublic.log" 2>&1 &
CLIENT_PIDS+=($!)
retry 20 sh -c "grep -q 'does not permit publishing public' '$LOG_DIR/server-auth.log'" \
  || fail "server did not log the denied public declaration"
RESP="$(curl -s -D - -o /dev/null -H "Host: priv.e2e.local" "$BASE/hello")"
assert_contains "$RESP" "/aperio/auth" "unpermitted public declaration keeps the gate"

stop_server
