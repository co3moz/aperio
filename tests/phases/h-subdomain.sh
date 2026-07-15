#!/usr/bin/env bash
# Phase H: subdomain. Sourced by tests/e2e.sh after the harness.
PHASE="subdomain"

step "Same-level random subdomain pattern"
start_server APERIO_RANDOM_SUBDOMAIN='*-pi.e2e.local' APERIO_WEBAUTHN_ORIGIN='https://tunnel.e2e.local'
TUNNEL="$(curl -sf -X POST -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
  --data '{"name":"pattern","ttl_seconds":300}' "$BASE/aperio/api/tunnels")" \
  || fail "tunnel provisioning under the pattern failed"
PATTERN_HOST="$(echo "$TUNNEL" | sed -n 's/.*"hostname":"\([^"]*\)".*/\1/p')"
case "$PATTERN_HOST" in
  *-pi.e2e.local) echo "  ok: pattern hostname generated: $PATTERN_HOST" ;;
  *) fail "expected a *-pi.e2e.local hostname, got: $PATTERN_HOST" ;;
esac
case "$PATTERN_HOST" in
  *'*'*) fail "generated hostname still contains the placeholder: $PATTERN_HOST" ;;
  *) echo "  ok: placeholder fully substituted" ;;
esac

step "Passkey (WebAuthn) enabled surface"
AVAIL="$(curl -s "$BASE/aperio/auth/passkey")"
assert_contains "$AVAIL" '"available":true' "passkey probe reports configured"
# Unknown users and users without passkeys get a uniform 401 (no username oracle).
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
  --data '{"username":"nobody"}' "$BASE/aperio/auth/passkey/start")"
assert_status 401 "$CODE" "passkey login start refuses unknown users uniformly"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
  --data '{"ceremony_id":"bogus","credential":{}}' "$BASE/aperio/auth/passkey/finish")"
[ "$CODE" = "400" ] || [ "$CODE" = "422" ] || fail "bogus passkey finish should be rejected (got $CODE)"

stop_server
