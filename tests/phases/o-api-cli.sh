#!/usr/bin/env bash
# Phase O: the `aperio-client api ...` admin API commands. Sourced by
# tests/e2e.sh after the harness.
PHASE="api-cli"

step "Starting aperio-server and one client for the api commands"
start_server
start_backend "$BACKEND_PORT"
start_client main "$BACKEND_PORT" APERIO_HOSTNAME="$HOSTNAME_BIND"
wait_routable "$HOSTNAME_BIND"

# The api commands authenticate with a programmatic admin key, so mint one
# through the dashboard session first, exactly what an operator would do.
COOKIES="$LOG_DIR/api-cli-cookies.txt"
dashboard_login "$COOKIES"
ADMIN_KEY="$(curl -s -b "$COOKIES" -X POST "$BASE/aperio/api/admin-keys" \
  -H 'Content-Type: application/json' \
  -d '{"name":"e2e-cli","role":"admin"}' \
  | "$PYTHON" -c 'import sys,json; print(json.load(sys.stdin)["key"])')"
[ -n "$ADMIN_KEY" ] || fail "could not mint an admin key for the api commands"

# Every command below talks to this server with that key.
api() { # <args...>
  env APERIO_SERVER_URL="$BASE" APERIO_API_KEY="$ADMIN_KEY" "$CLIENT_BIN" api "$@"
}

step "Read-only reports"
OUT="$(api stats)"
assert_contains "$OUT" '"active_clients"' "api stats returns the stats snapshot"
OUT="$(api health)"
assert_contains "$OUT" '"status": "healthy"' "api health reports the server as healthy"
OUT="$(api topology)"
assert_contains "$OUT" "$HOSTNAME_BIND" "api topology lists the connected client's bind"
OUT="$(api traffic-csv --count 2)"
assert_contains "$OUT" "period,requests" "api traffic-csv prints raw CSV, not JSON"

step "Share links"
OUT="$(api share --hostname "$HOSTNAME_BIND" --path /test --expire 1d)"
assert_contains "$OUT" '"url"' "api share mints a link"
assert_contains "$OUT" "aperio_share=" "the share link carries the signed token"
# `never` is a real value here (0 = no expiry), not an omitted field.
OUT="$(api share --hostname "$HOSTNAME_BIND" --expire never)"
assert_contains "$OUT" '"expires_at": null' "api share --expire never yields a permanent link"
if api share --hostname "$HOSTNAME_BIND" --expire tomorrow >/dev/null 2>&1; then
  fail "api share accepted an invalid duration"
fi
echo "  ok: an invalid --expire value is rejected before any request"

step "Token lifecycle"
OUT="$(api token create --name e2e-cli-token --hostname "$HOSTNAME_BIND" --expire 1d)"
assert_contains "$OUT" '"token": "apr_' "api token create returns the secret once"
TOKEN_ID="$(printf '%s' "$OUT" | "$PYTHON" -c 'import sys,json; print(json.load(sys.stdin)["id"])')"
OUT="$(api token list)"
assert_contains "$OUT" "e2e-cli-token" "api token list shows the new token"
OUT="$(api token update "$TOKEN_ID" --name e2e-cli-renamed)"
assert_contains "$(api token list)" "e2e-cli-renamed" "api token update renames it"
OUT="$(api token rotate "$TOKEN_ID" --grace 1h)"
assert_contains "$OUT" '"token": "apr_' "api token rotate returns a fresh secret"
api token revoke "$TOKEN_ID" >/dev/null
case "$(api token list)" in
  *e2e-cli-renamed*) fail "api token revoke did not remove the token" ;;
  *) echo "  ok: api token revoke removes the token" ;;
esac

step "Maintenance mode"
api maintenance on "$HOSTNAME_BIND" >/dev/null
assert_contains "$(api maintenance list)" "$HOSTNAME_BIND" "api maintenance on flags the host"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Host: $HOSTNAME_BIND" "$BASE/hello")"
assert_status 503 "$CODE" "the flagged host serves the maintenance page"
api maintenance off "$HOSTNAME_BIND" >/dev/null
retry 10 sh -c "curl -sf -H 'Host: $HOSTNAME_BIND' '$BASE/hello' >/dev/null" \
  || fail "api maintenance off did not restore routing"
echo "  ok: api maintenance off restores routing"

step "Ephemeral tunnels"
OUT="$(api tunnel create --name e2e-cli-tunnel --hostname cli.e2e.local --expire 30m)"
assert_contains "$OUT" '"token": "apr_' "api tunnel create mints a scoped token"
TUNNEL_ID="$(printf '%s' "$OUT" | "$PYTHON" -c 'import sys,json; print(json.load(sys.stdin)["id"])')"
api tunnel delete "$TUNNEL_ID" >/dev/null
echo "  ok: api tunnel delete removes it"

step "Users, webhooks, and the cache"
OUT="$(api user create --username e2e-cli-user --password e2e-cli-password --role operator)"
assert_contains "$OUT" '"role": "operator"' "api user create makes an operator"
USER_ID="$(printf '%s' "$OUT" | "$PYTHON" -c 'import sys,json; print(json.load(sys.stdin)["id"])')"
api user update "$USER_ID" --role viewer >/dev/null
assert_contains "$(api user list)" '"role": "viewer"' "api user update changes the role"
api user delete "$USER_ID" >/dev/null
OUT="$(api webhook create --name e2e-cli-hook --url http://127.0.0.1:1/none --event client_connected)"
assert_contains "$OUT" '"status": "ok"' "api webhook create registers a hook"
HOOK_ID="$(printf '%s' "$OUT" | "$PYTHON" -c 'import sys,json; print(json.load(sys.stdin)["id"])')"
api webhook delete "$HOOK_ID" >/dev/null
assert_contains "$(api cache purge)" '"removed"' "api cache purge reports what it dropped"

step "Authentication failures"
if env APERIO_SERVER_URL="$BASE" "$CLIENT_BIN" api stats >/dev/null 2>&1; then
  fail "api stats succeeded without a credential"
fi
echo "  ok: a call without an admin key fails instead of returning the login page"

stop_server
