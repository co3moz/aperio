#!/usr/bin/env bash
# Phase J: sessions. Sourced by tests/e2e.sh after the harness.
PHASE="sessions"

step "Dashboard sessions survive a server restart"
start_server
SESS_JAR="$LOG_DIR/cookies-restart.txt"
dashboard_login "$SESS_JAR"
USR="$(curl -sf -b "$SESS_JAR" -X POST -H 'Content-Type: application/json' \
  --data '{"username":"e2e-restart","password":"restart-password","role":"operator"}' "$BASE/aperio/api/users")" \
  || fail "user creation failed"
UJAR="$LOG_DIR/cookies-restart-user.txt"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -c "$UJAR" -X POST -u 'e2e-restart:restart-password' "$BASE/aperio/auth")"
assert_status 200 "$CODE" "the user signs in before the restart"
# Restart the server on the SAME data dir (stop_server would also kill
# clients and we need the mktemp dir preserved).
kill "$SERVER_PID" 2>/dev/null || true
sleep 1
env PORT="$SERVER_PORT" \
  APERIO_SERVER_TOKEN="$TOKEN" \
  APERIO_DATA_DIR="$DATA_DIR" \
  APERIO_RANDOM_SUBDOMAIN='*.e2e.local' \
  APERIO_GATEWAY_TIMEOUT=3 \
  APERIO_UPTIME_TICK_SECS=1 \
  APERIO_WEBAUTHN_ORIGIN=http://localhost:18100 \
  "$SERVER_BIN" >"$LOG_DIR/server-sessions-restarted.log" 2>&1 &
SERVER_PID=$!
retry 15 curl -sf "$BASE/aperio/health" || fail "server did not come back up"
SESSION="$(curl -s -b "$UJAR" "$BASE/aperio/api/session")"
assert_contains "$SESSION" '"username":"e2e-restart"' "the user's session survived the restart"
assert_contains "$SESSION" '"role":"operator"' "the restored session kept its role"
SESSION="$(curl -s -b "$SESS_JAR" "$BASE/aperio/api/session")"
assert_contains "$SESSION" '"username":"aperio"' "the admin session survived the restart"
step "Active session management"
# Admin lists sessions: both the admin's and the user's appear with metadata.
SESSIONS="$(curl -s -b "$SESS_JAR" "$BASE/aperio/api/sessions")"
assert_contains "$SESSIONS" '"username":"e2e-restart"' "sessions list shows the named user"
assert_contains "$SESSIONS" '"username":"aperio"' "sessions list shows the admin"
assert_contains "$SESSIONS" '"current":true' "the caller's own session is marked"
assert_contains "$SESSIONS" '"ip":"127.0.0.1"' "sessions record the sign-in IP"
# Non-admins may not see the list.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$UJAR" "$BASE/aperio/api/sessions")"
assert_status 403 "$CODE" "the sessions list is admin-only"
# Revoke the user's session: their cookie stops working immediately.
USER_SID="$(echo "$SESSIONS" | "$PYTHON" -c \
  "import sys,json; print(next(s['id'] for s in json.load(sys.stdin) if s['username']=='e2e-restart'))")"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$SESS_JAR" -X DELETE "$BASE/aperio/api/sessions/${USER_SID}")"
assert_status 200 "$CODE" "revoking a session succeeds"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$UJAR" "$BASE/aperio/api/stats")"
assert_status 302 "$CODE" "the revoked session stops working immediately"
# Sign back in and clear everything else: the admin's own session survives.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -c "$UJAR" -X POST -u 'e2e-restart:restart-password' "$BASE/aperio/auth")"
assert_status 200 "$CODE" "the user signs back in"
CLEARED="$(curl -s -b "$SESS_JAR" -X DELETE "$BASE/aperio/api/sessions")"
assert_contains "$CLEARED" '"ended":' "sign-out-everywhere reports a count"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$UJAR" "$BASE/aperio/api/session")"
assert_status 302 "$CODE" "other sessions are gone after sign-out-everywhere"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$SESS_JAR" "$BASE/aperio/api/stats")"
assert_status 200 "$CODE" "the caller's own session survives sign-out-everywhere"
# Re-create the user session for the logout checks below.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -c "$UJAR" -X POST -u 'e2e-restart:restart-password' "$BASE/aperio/auth")"
assert_status 200 "$CODE" "the user signs in once more"

step "Usernameless passkey endpoints"
PK="$(curl -s "$BASE/aperio/auth/passkey")"
assert_contains "$PK" '"available":true' "passkey support is on for this phase"
DISC="$(curl -s -X POST "$BASE/aperio/auth/passkey/discoverable/start")"
assert_contains "$DISC" '"ceremony_id"' "discoverable start returns a ceremony"
assert_contains "$DISC" '"challenge"' "discoverable start returns a challenge"
DISC_ID="$(echo "$DISC" | sed -n 's/.*"ceremony_id":"\([^"]*\)".*/\1/p')"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
  --data "{\"ceremony_id\":\"${DISC_ID}\",\"credential\":{}}" "$BASE/aperio/auth/passkey/discoverable/finish")"
case "$CODE" in 400|401|422) echo "  ok: garbage discoverable credentials are rejected ($CODE)" ;; *) fail "expected 4xx for a garbage credential, got $CODE" ;; esac
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
  --data '{"ceremony_id":"nope","credential":{}}' "$BASE/aperio/auth/passkey/discoverable/finish")"
case "$CODE" in 400|422) echo "  ok: unknown ceremonies are rejected ($CODE)" ;; *) fail "expected 400/422 for an unknown ceremony, got $CODE" ;; esac

# Logout still removes it durably.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$UJAR" -X POST "$BASE/aperio/auth/logout")"
assert_status 200 "$CODE" "logout succeeds"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$UJAR" -o /dev/null "$BASE/aperio/api/stats")"
assert_status 302 "$CODE" "the logged-out session no longer works"

stop_server
