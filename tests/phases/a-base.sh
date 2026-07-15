#!/usr/bin/env bash
# Phase A: base. Sourced by tests/e2e.sh after the harness.
PHASE="base"

start_backend "$BACKEND_PORT"

step "Starting aperio-server (base configuration)"
ACCESS_LOG="$LOG_DIR/access.jsonl"
METRICS_TOKEN="e2e-metrics-token"
start_server APERIO_ACCESS_LOG="$ACCESS_LOG" APERIO_METRICS=1 APERIO_METRICS_TOKEN="$METRICS_TOKEN"
BASE_DATA_DIR="$DATA_DIR"

step "Health endpoint"
HEALTH="$(curl -s "$BASE/aperio/health")"
assert_contains "$HEALTH" '"status":"healthy"' "health reports healthy"
assert_contains "$HEALTH" '"protocol":' "health reports the tunnel protocol version"
assert_contains "$HEALTH" '"ui_language"' "health reports the default UI language"

step "First-run redirect and 504 when no client is connected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/")"
assert_status 307 "$CODE" "fresh install redirects the bare root to the dashboard"
LOCATION="$(curl -s -o /dev/null -w '%{redirect_url}' "$BASE/")"
assert_contains "$LOCATION" '/aperio' "redirect points at /aperio"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/hello")"
assert_status 504 "$CODE" "proxying without a client returns 504"

step "Tunnel proxying through a connected client"
start_client main "$BACKEND_PORT" APERIO_HOSTNAME_BIND="$HOSTNAME_BIND"
wait_routable "$HOSTNAME_BIND"

BODY="$(curl -s -H "Host: ${HOSTNAME_BIND}" "$BASE/hello?x=1")"
assert_contains "$BODY" "backend ${BACKEND_PORT} GET /hello?x=1" "GET is proxied to the backend"

BODY="$(curl -s -X POST -H "Host: ${HOSTNAME_BIND}" -H 'Content-Type: text/plain' \
  --data 'payload-123' "$BASE/submit")"
assert_contains "$BODY" "backend ${BACKEND_PORT} POST /submit body=payload-123" "POST body is proxied"

step "Large upload/download streaming (protocol v2)"
BIG="$LOG_DIR/big.bin"
"$PYTHON" -c "import sys; sys.stdout.write('A'*600000)" > "$BIG"
SIZE_OUT="$(curl -s -X POST -H "Host: ${HOSTNAME_BIND}" -H 'Content-Type: application/octet-stream' \
  --data-binary @"$BIG" "$BASE/big" | wc -c)"
[ "$SIZE_OUT" -ge 600000 ] || fail "streamed upload/download returned only $SIZE_OUT bytes"
echo "  ok: 600 KB body streamed both ways ($SIZE_OUT bytes echoed)"

step "Dashboard login and APIs"
COOKIES="$LOG_DIR/cookies.txt"
dashboard_login "$COOKIES"

CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -u 'aperio:wrong-password' "$BASE/aperio/auth")"
assert_status 401 "$CODE" "login with a bad password is rejected"

STATS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/stats")"
assert_contains "$STATS" '"connected_clients_count":1' "stats show the connected client"
assert_contains "$STATS" "\"$HOSTNAME_BIND\"" "stats show the hostname bind"
assert_contains "$STATS" '"by_hostname"' "stats include the traffic breakdown"
HIST="$(curl -s -b "$COOKIES" "$BASE/aperio/api/stats/history?unit=day&count=7")"
assert_contains "$HIST" '"period"' "stats history returns period buckets"
HIST_N="$(echo "$HIST" | grep -o '"period"' | wc -l | tr -d ' ')"
[ "$HIST_N" -eq 7 ] || fail "stats history should return 7 day buckets, got $HIST_N"
assert_contains "$HIST" '"requests":' "stats history buckets carry request counts"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" "$BASE/aperio/api/stats/history?unit=fortnight")"
assert_status 400 "$CODE" "unknown history units are rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" "$BASE/aperio/api/stats/history?from=2026-02-02&to=2026-01-01")"
assert_status 400 "$CODE" "reversed history date ranges are rejected"
retry 10 sh -c "curl -s -b '$COOKIES' '$BASE/aperio/api/uptime' | grep -q '\"status\":\"up\"'" \
  || fail "uptime history did not report the connected client as up"
UPTIME="$(curl -s -b "$COOKIES" "$BASE/aperio/api/uptime")"
assert_contains "$UPTIME" '"pct_today":' "uptime entries carry percentages"
assert_contains "$UPTIME" '"days":' "uptime entries carry daily buckets"

LOGS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/logs")"
assert_contains "$LOGS" '/submit' "request log captured the proxied POST"

CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/aperio/api/stats")"
assert_status 302 "$CODE" "stats without a session redirect to login"

step "Maintenance mode"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"hostname\":\"$HOSTNAME_BIND\",\"enabled\":true}" "$BASE/aperio/api/maintenance")"
assert_status 200 "$CODE" "maintenance can be enabled"
RESP="$(curl -s -D - -o /dev/null -H "Host: ${HOSTNAME_BIND}" "$BASE/hello")"
assert_contains "$RESP" "503" "maintenance answers 503"
assert_contains "$RESP" "retry-after" "maintenance sets Retry-After"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"hostname\":\"$HOSTNAME_BIND\",\"enabled\":false}" "$BASE/aperio/api/maintenance")"
assert_status 200 "$CODE" "maintenance can be disabled"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Host: ${HOSTNAME_BIND}" "$BASE/hello")"
assert_status 200 "$CODE" "traffic resumes after maintenance"

step "Settings API"
SETTINGS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/settings")"
assert_contains "$SETTINGS" '"effective"' "settings expose effective values"
assert_contains "$SETTINGS" '"lb_strategy":"round-robin"' "settings show the default lb strategy"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"gateway_timeout_secs":5,"lb_strategy":"sticky"}' "$BASE/aperio/api/settings")"
assert_status 200 "$CODE" "settings can be updated"
SETTINGS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/settings")"
assert_contains "$SETTINGS" '"lb_strategy":"sticky"' "settings update applied live"
[ -f "$BASE_DATA_DIR/settings.json" ] || fail "settings.json was not persisted"
assert_contains "$(cat "$BASE_DATA_DIR/settings.json")" '"gateway_timeout_secs": 5' "settings.json persists overrides"
assert_contains "$SETTINGS" '"environment"' "settings expose the env-only flag report"
assert_contains "$SETTINGS" 'APERIO_TRUST_PROXY' "env report lists the proxy trust flag"
assert_contains "$SETTINGS" '"cache_enabled"' "settings expose the response cache toggle"
assert_contains "$SETTINGS" '"ui_language":"en"' "settings expose the default UI language"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"cache_enabled":true,"max_concurrent_requests":64,"login_lockout_threshold":7}' "$BASE/aperio/api/settings")"
assert_status 200 "$CODE" "new runtime settings can be updated"
SETTINGS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/settings")"
assert_contains "$SETTINGS" '"max_concurrent_requests":64' "concurrency limit applied live"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"lb_strategy":"bogus"}' "$BASE/aperio/api/settings")"
assert_status 400 "$CODE" "invalid settings are rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"cache_max_bytes":0}' "$BASE/aperio/api/settings")"
assert_status 400 "$CODE" "zero cache budget is rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{}' "$BASE/aperio/api/settings")"
assert_status 200 "$CODE" "settings overrides can be reset"

step "Programmatic tunnels API"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
  --data '{}' "$BASE/aperio/api/tunnels")"
assert_status 401 "$CODE" "tunnel provisioning without credentials is rejected"

TUNNEL="$(curl -sf -X POST -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
  --data '{"name":"e2e-preview","ttl_seconds":300}' "$BASE/aperio/api/tunnels")" \
  || fail "tunnel provisioning with the master token failed"
assert_contains "$TUNNEL" '"token":"apr_' "tunnel response contains an ephemeral token"
assert_contains "$TUNNEL" '.e2e.local' "tunnel response contains a random subdomain"

EPHEMERAL_TOKEN="$(echo "$TUNNEL" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
EPHEMERAL_HOST="$(echo "$TUNNEL" | sed -n 's/.*"hostname":"\([^"]*\)".*/\1/p')"
TUNNEL_ID="$(echo "$TUNNEL" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
[ -n "$EPHEMERAL_TOKEN" ] && [ -n "$EPHEMERAL_HOST" ] && [ -n "$TUNNEL_ID" ] \
  || fail "could not parse the tunnel response: $TUNNEL"

env APERIO_SERVER_URL="$BASE" APERIO_SERVER_TOKEN="$EPHEMERAL_TOKEN" \
  APERIO_CLIENT_TARGET="http://127.0.0.1:${BACKEND_PORT}" \
  "$CLIENT_BIN" >"$LOG_DIR/client-base-ephemeral.log" 2>&1 &
EPHEMERAL_PID=$!
CLIENT_PIDS+=($EPHEMERAL_PID)
wait_routable "$EPHEMERAL_HOST" "/preview"
BODY="$(curl -s -H "Host: ${EPHEMERAL_HOST}" "$BASE/preview")"
assert_contains "$BODY" "backend ${BACKEND_PORT} GET /preview" "ephemeral tunnel proxies to the backend"

CODE="$(curl -s -o /dev/null -w '%{http_code}' -X DELETE \
  -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/api/tunnels/${TUNNEL_ID}")"
assert_status 200 "$CODE" "tunnel revocation succeeds"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X DELETE \
  -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/api/tunnels/${TUNNEL_ID}")"
assert_status 404 "$CODE" "revoking the same tunnel twice returns 404"

# Stop the ephemeral client and wait for the server to drop it, so only `main`
# stays connected. Otherwise the client-control test below reads
# active_clients[0] non-deterministically and may disable the ephemeral client
# (bound to its own random subdomain) rather than the one serving app.e2e.local.
kill "$EPHEMERAL_PID" 2>/dev/null || true
wait "$EPHEMERAL_PID" 2>/dev/null || true
retry 15 only_main_connected \
  || fail "ephemeral client did not disconnect before the client-control test"

step "Prometheus metrics"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/aperio/metrics")"
assert_status 401 "$CODE" "metrics without a token are rejected"
METRICS="$(curl -s "$BASE/aperio/metrics?token=${METRICS_TOKEN}")"
assert_contains "$METRICS" 'aperio_requests_total' "metrics expose the request counter"
assert_contains "$METRICS" 'aperio_connected_clients' "metrics expose the client gauge"
assert_contains "$METRICS" 'aperio_request_duration_seconds_bucket' "metrics expose the duration histogram"
assert_contains "$METRICS" 'aperio_token_requests_total{token="' "metrics expose per-token counters"
assert_contains "$METRICS" 'aperio_hostname_requests_total{hostname="' "metrics expose per-hostname counters"
METRICS="$(curl -s -H "Authorization: Bearer ${METRICS_TOKEN}" "$BASE/aperio/metrics")"
assert_contains "$METRICS" 'aperio_requests_total' "metrics accept the Bearer form too"

step "Request inspector & replay"
curl -s -H "Host: ${HOSTNAME_BIND}" "$BASE/inspect-me" >/dev/null
REQ_ID="$(curl -s -b "$COOKIES" "$BASE/aperio/api/logs" | "$PYTHON" -c \
  "import sys,json; print(next(l['id'] for l in json.load(sys.stdin) if l['uri'].startswith('/inspect-me')))")" \
  || fail "could not find the /inspect-me request in the logs"
DETAIL="$(curl -s -b "$COOKIES" "$BASE/aperio/api/requests/${REQ_ID}")"
assert_contains "$DETAIL" '"method":"GET"' "inspector captures the request method"
assert_contains "$DETAIL" '/inspect-me' "inspector captures the request uri"
# High-resolution timeline: stage offsets exist and are ordered.
echo "$DETAIL" | "$PYTHON" -c "
import sys, json
tl = json.load(sys.stdin).get('timeline')
assert tl, 'timeline missing from the capture'
order = [0, tl['dispatched_us'], tl['client_received_us'], tl['backend_sent_us'],
         tl['backend_first_byte_us'], tl['backend_done_us'], tl['client_responded_us'],
         tl['response_received_us'], tl['finished_us']]
assert all(a <= b for a, b in zip(order, order[1:])), f'stages out of order: {order}'
assert tl['estimated_anchor'] is True
" || fail "captured timeline is missing or out of order"
echo "  ok: capture carries an ordered high-resolution timeline"

STAGES="$(curl -s -b "$COOKIES" "$BASE/aperio/api/stage-stats")"
assert_contains "$STAGES" '"stage":"backend_wait"' "stage statistics cover the backend-wait stage"
assert_contains "$STAGES" '"host":"'"$HOSTNAME_BIND"'"' "stage statistics are grouped by route"
echo "$STAGES" | "$PYTHON" -c "
import sys, json
routes = json.load(sys.stdin)
row = next(r for r in routes if r['host'] != '*')
counts = {s['stage']: s['count'] for s in row['stages']}
assert counts['queue'] > 0 and counts['backend_wait'] > 0, counts
" || fail "stage statistics carry no samples"
echo "  ok: stage statistics accumulate samples per stage"

REPLAY="$(curl -s -b "$COOKIES" -X POST "$BASE/aperio/api/requests/${REQ_ID}/replay")"
assert_contains "$REPLAY" '"status":200' "replay reaches the backend again"
assert_contains "$REPLAY" '"replayed_id"' "replay reports the replayed id"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" "$BASE/aperio/api/requests/no-such-id")"
assert_status 404 "$CODE" "unknown capture ids answer 404"

step "Inspector secret redaction"
curl -s -H "Host: ${HOSTNAME_BIND}" -H "Authorization: Bearer sk-live-e2e-token" \
  -H "Cookie: sid=e2e-cookie-secret" -H 'Content-Type: application/json' \
  --data '{"username":"doga","password":"e2e-hunter2"}' "$BASE/redact-me" >/dev/null
RED_ID="$(curl -s -b "$COOKIES" "$BASE/aperio/api/logs" | "$PYTHON" -c \
  "import sys,json; print(next(l['id'] for l in json.load(sys.stdin) if l['uri'].startswith('/redact-me')))")" \
  || fail "could not find the /redact-me request in the logs"
RED="$(curl -s -b "$COOKIES" "$BASE/aperio/api/requests/${RED_ID}")"
assert_contains "$RED" 'Bearer [REDACTED]' "authorization header is masked in the inspector"
case "$RED" in
  *sk-live-e2e-token*) fail "the bearer token leaked into the inspector detail" ;;
  *e2e-cookie-secret*) fail "the cookie value leaked into the inspector detail" ;;
esac
echo "  ok: header secrets never leave the server"
RED_BODY="$(echo "$RED" | "$PYTHON" -c \
  "import sys,json,base64; print(base64.b64decode(json.load(sys.stdin)['req_body']).decode())")"
assert_contains "$RED_BODY" '"username":"doga"' "non-secret body fields stay readable"
assert_contains "$RED_BODY" '"password":"[REDACTED]"' "secret body fields are masked"
case "$RED_BODY" in *e2e-hunter2*) fail "the password leaked into the captured body" ;; esac
echo "  ok: body secrets never leave the server"
# The raw capture is intact server-side: replaying still works.
REPLAY="$(curl -s -b "$COOKIES" -X POST "$BASE/aperio/api/requests/${RED_ID}/replay")"
assert_contains "$REPLAY" '"status":200' "redacted captures still replay with their original bytes"

step "Passkeys disabled by default"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/aperio/auth/passkey/discoverable/start")"
assert_status 501 "$CODE" "usernameless start answers 501 without APERIO_WEBAUTHN_ORIGIN"

step "Webhooks API"
HOOK="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"name\":\"e2e-hook\",\"url\":\"http://127.0.0.1:${BACKEND_PORT}/hook\",\"events\":[\"client_connected\"]}" \
  "$BASE/aperio/api/webhooks")" || fail "webhook creation failed"
HOOK_ID="$(echo "$HOOK" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
[ -n "$HOOK_ID" ] || fail "could not parse the webhook response: $HOOK"
LIST="$(curl -s -b "$COOKIES" "$BASE/aperio/api/webhooks")"
assert_contains "$LIST" 'e2e-hook' "webhook list contains the created hook"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"bad","url":"ftp://nope"}' "$BASE/aperio/api/webhooks")"
assert_status 400 "$CODE" "non-http webhook URLs are rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/webhooks/${HOOK_ID}")"
assert_status 200 "$CODE" "webhook deletion succeeds"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/webhooks/${HOOK_ID}")"
assert_status 404 "$CODE" "deleting the same webhook twice returns 404"
SLACK="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"name\":\"e2e-slack\",\"url\":\"http://127.0.0.1:${BACKEND_PORT}/hook\",\"events\":[\"*\"],\"format\":\"slack\"}" \
  "$BASE/aperio/api/webhooks")" || fail "slack-format webhook creation failed"
SLACK_ID="$(echo "$SLACK" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
LIST="$(curl -s -b "$COOKIES" "$BASE/aperio/api/webhooks")"
assert_contains "$LIST" '"format":"slack"' "webhook list reports the slack format"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"bad-format","url":"http://127.0.0.1:1/x","format":"telegram"}' "$BASE/aperio/api/webhooks")"
assert_status 400 "$CODE" "unknown webhook formats are rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/webhooks/${SLACK_ID}")"
assert_status 200 "$CODE" "slack webhook deletion succeeds"

step "Webhook delivery log & redelivery"
# A webhook the backend accepts: token_created events deliver successfully.
DLV="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"name\":\"e2e-deliveries\",\"url\":\"http://127.0.0.1:${BACKEND_PORT}/hook\",\"events\":[\"token_created\"]}" \
  "$BASE/aperio/api/webhooks")" || fail "delivery webhook creation failed"
DLV_ID="$(echo "$DLV" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
# And one nothing listens on: delivery must be retried, then recorded failed.
curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"e2e-dead","url":"http://127.0.0.1:9/hook","events":["token_created"]}' \
  "$BASE/aperio/api/webhooks" >/dev/null || fail "dead webhook creation failed"
curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"dlv-probe","hostnames":["dlv.e2e.local"]}' "$BASE/aperio/api/tokens" >/dev/null \
  || fail "token creation (delivery trigger) failed"
DELIVERIES=""
for _ in 1 2 3 4 5 6 7 8 9 10; do
  DELIVERIES="$(curl -s -b "$COOKIES" "$BASE/aperio/api/webhooks/deliveries")"
  case "$DELIVERIES" in
    *'"webhook_name":"e2e-deliveries"'*'"webhook_name":"e2e-dead"'*|*'"webhook_name":"e2e-dead"'*'"webhook_name":"e2e-deliveries"'*) break ;;
  esac
  sleep 1
done
assert_contains "$DELIVERIES" '"webhook_name":"e2e-deliveries"' "delivery log records the successful delivery"
assert_contains "$DELIVERIES" '"success":true' "the reachable webhook delivered"
assert_contains "$DELIVERIES" '"webhook_name":"e2e-dead"' "delivery log records the failed delivery"
assert_contains "$DELIVERIES" '"success":false' "the unreachable webhook is marked failed"
assert_contains "$DELIVERIES" '"attempts":2' "the failed delivery was retried per the schedule"
GOOD_DLV_ID="$(echo "$DELIVERIES" | tr '}' '\n' | grep '"webhook_name":"e2e-deliveries"' | sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | head -1)"
[ -n "$GOOD_DLV_ID" ] || fail "could not parse a delivery id"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST "$BASE/aperio/api/webhooks/deliveries/${GOOD_DLV_ID}/redeliver")"
assert_status 202 "$CODE" "redelivery is accepted"
REDELIVERED=""
for _ in 1 2 3 4 5; do
  REDELIVERED="$(curl -s -b "$COOKIES" "$BASE/aperio/api/webhooks/deliveries?webhook_id=${DLV_ID}")"
  COUNT="$(echo "$REDELIVERED" | grep -o '"webhook_name":"e2e-deliveries"' | wc -l | tr -d ' ')"
  [ "$COUNT" -ge 2 ] && break
  sleep 1
done
[ "${COUNT:-0}" -ge 2 ] || fail "redelivery did not appear in the log (got $COUNT)"
echo "  ok: redelivery lands in the log as a new row"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST "$BASE/aperio/api/webhooks/deliveries/no-such-id/redeliver")"
assert_status 404 "$CODE" "redelivering an unknown id answers 404"

step "Organizations API (master super-admin)"
ORG="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"Acme"}' "$BASE/aperio/api/orgs")" || fail "org creation failed"
ORG_ID="$(echo "$ORG" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
[ -n "$ORG_ID" ] || fail "could not parse the org id: $ORG"
ORGS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/orgs")"
assert_contains "$ORGS" '"id":"master"' "org list includes the implicit master org"
assert_contains "$ORGS" '"name":"Acme"' "org list includes the created child org"
assert_contains "$ORGS" '"master":true' "the master org is flagged"
# Duplicate name (case-insensitive) and the reserved name are rejected.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' --data '{"name":"acme"}' "$BASE/aperio/api/orgs")"
assert_status 400 "$CODE" "duplicate org names are rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' --data '{"name":"master"}' "$BASE/aperio/api/orgs")"
assert_status 400 "$CODE" "the reserved name master is rejected"
# The implicit master org cannot be deleted.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/orgs/master")"
assert_status 400 "$CODE" "the master org cannot be deleted"

step "Organization isolation (effective-org scoping)"
# Switch the super-admin into the child org: resources created now belong to it.
curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"id\":\"${ORG_ID}\"}" "$BASE/aperio/api/orgs/select" >/dev/null \
  || fail "selecting the child org failed"
ACME_TOK="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"acme-token","hostnames":["*"]}' "$BASE/aperio/api/tokens")" \
  || fail "creating a token in the child org failed"
ACME_TOK_ID="$(echo "$ACME_TOK" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
[ -n "$ACME_TOK_ID" ] || fail "could not parse the child-org token id"
# While the child org is selected, its token is visible.
TOKS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/tokens")"
assert_contains "$TOKS" 'acme-token' "the child-org token is visible in its own org"
# The audit log is org-scoped too: the child org sees its own token_created event.
AUD_CHILD="$(curl -s -b "$COOKIES" "$BASE/aperio/api/audit")"
assert_contains "$AUD_CHILD" 'name=acme-token' "the child org's audit shows its own token creation"
# Live sessions are org-scoped: the master admin's own session (which belongs
# to master) is hidden while the super-admin is viewing a child org.
SESS_CHILD="$(curl -s -b "$COOKIES" "$BASE/aperio/api/sessions")"
if echo "$SESS_CHILD" | grep -q '"current":true'; then
  fail "isolation breach: the master session appears while viewing a child org"
fi
echo "  ok: the master session is hidden while viewing a child org"
# Switch back to master: the child-org token must NOT be visible there.
curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"id":"master"}' "$BASE/aperio/api/orgs/select" >/dev/null \
  || fail "selecting master failed"
TOKS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/tokens")"
if echo "$TOKS" | grep -q 'acme-token'; then
  fail "isolation breach: the child-org token leaked into master's token list"
fi
echo "  ok: the child-org token is hidden from master's token list"
# And master's audit log must not surface the child org's events.
AUD_MASTER="$(curl -s -b "$COOKIES" "$BASE/aperio/api/audit")"
if echo "$AUD_MASTER" | grep -q 'name=acme-token'; then
  fail "isolation breach: the child-org token_created event leaked into master's audit log"
fi
echo "  ok: the child-org audit event is hidden from master's audit log"
# Back in master, the caller's own session is visible again.
SESS_MASTER="$(curl -s -b "$COOKIES" "$BASE/aperio/api/sessions")"
assert_contains "$SESS_MASTER" '"current":true' "the master session is visible in master"
# The org listing still counts the child org's token.
ORGS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/orgs")"
assert_contains "$ORGS" '"tokens":1' "the org listing counts the child org's token"
# A cross-org by-id revoke from master is refused (404, existence hidden).
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/tokens/${ACME_TOK_ID}")"
assert_status 404 "$CODE" "revoking a child-org token from master is refused"
# Selecting an unknown org id is a 404.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' --data '{"id":"nope"}' "$BASE/aperio/api/orgs/select")"
assert_status 404 "$CODE" "selecting an unknown org id returns 404"
# Deleting a non-empty child org is refused (409).
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/orgs/${ORG_ID}")"
assert_status 409 "$CODE" "a non-empty child org cannot be deleted"
# Clean up: revoke the token from inside the child org, then delete the org.
curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"id\":\"${ORG_ID}\"}" "$BASE/aperio/api/orgs/select" >/dev/null
curl -sf -b "$COOKIES" -X DELETE "$BASE/aperio/api/tokens/${ACME_TOK_ID}" >/dev/null \
  || fail "revoking the child-org token failed"
curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"id":"master"}' "$BASE/aperio/api/orgs/select" >/dev/null

# An empty child org deletes; a repeat is 404.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/orgs/${ORG_ID}")"
assert_status 200 "$CODE" "an empty child org is deleted"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/orgs/${ORG_ID}")"
assert_status 404 "$CODE" "deleting an unknown org returns 404"

step "Organization traffic isolation (per-org logs & stats)"
# A child org with its own token, bound to a dedicated hostname, then a real
# client that authenticates with that token and serves a request.
TORG="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"Traffico"}' "$BASE/aperio/api/orgs")" || fail "traffic-org creation failed"
TORG_ID="$(echo "$TORG" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"id\":\"${TORG_ID}\"}" "$BASE/aperio/api/orgs/select" >/dev/null
ORG_HOST="orgtraffic.example.com"
TOK="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"name\":\"org-client\",\"hostnames\":[\"${ORG_HOST}\"]}" "$BASE/aperio/api/tokens")" \
  || fail "org token creation failed"
TOK_SECRET="$(echo "$TOK" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
[ -n "$TOK_SECRET" ] || fail "could not parse the org token secret"
# Back to master for the baseline check.
curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"id":"master"}' "$BASE/aperio/api/orgs/select" >/dev/null
# A client authenticating with the child-org token, bound to ORG_HOST.
start_client orgtraffic "$BACKEND_PORT" APERIO_SERVER_TOKEN="$TOK_SECRET" APERIO_HOSTNAME_BIND="$ORG_HOST"
wait_routable "$ORG_HOST"
# Drive a uniquely-identifiable request through the org's client.
curl -s -H "Host: ${ORG_HOST}" "$BASE/orgtraffic-probe" >/dev/null
retry 5 sh -c "curl -s -b '$COOKIES' '$BASE/aperio/api/orgs' | grep -q '\"tokens\":1'"
# Master context: the child org's request must NOT appear in master's log.
LOGS_MASTER="$(curl -s -b "$COOKIES" "$BASE/aperio/api/logs")"
if echo "$LOGS_MASTER" | grep -q 'orgtraffic-probe'; then
  fail "isolation breach: the child org's traffic leaked into master's log"
fi
echo "  ok: the child org's request is absent from master's traffic log"
# Child context: the org's own log shows it, and its stats count it.
curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"id\":\"${TORG_ID}\"}" "$BASE/aperio/api/orgs/select" >/dev/null
retry 5 sh -c "curl -s -b '$COOKIES' '$BASE/aperio/api/logs' | grep -q 'orgtraffic-probe'"
LOGS_CHILD="$(curl -s -b "$COOKIES" "$BASE/aperio/api/logs")"
assert_contains "$LOGS_CHILD" 'orgtraffic-probe' "the child org's request shows in its own traffic log"
STATS_CHILD="$(curl -s -b "$COOKIES" "$BASE/aperio/api/stats")"
CHILD_REQS="$(echo "$STATS_CHILD" | "$PYTHON" -c 'import sys,json; print(json.load(sys.stdin)["total_requests"])')"
[ "$CHILD_REQS" -ge 1 ] || fail "the child org's stats should count its own request (got $CHILD_REQS)"
echo "  ok: the child org's stats count its own traffic ($CHILD_REQS request(s))"
# Back to master to leave the session in a known state for later steps.
curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"id":"master"}' "$BASE/aperio/api/orgs/select" >/dev/null

step "Audit API"
AUDIT="$(curl -s -b "$COOKIES" "$BASE/aperio/api/audit")"
assert_contains "$AUDIT" 'client_connected' "audit log records the client connection"
assert_contains "$AUDIT" 'webhook_created' "audit log records the webhook creation"
# Audit records the acting user: the dashboard admin's actions are attributed
# to "aperio", and client_connected is a system event.
assert_contains "$AUDIT" '"actor":"aperio"' "audit records the acting dashboard user"
assert_contains "$AUDIT" '"actor":"system"' "system events are attributed to system"

step "OpenAPI spec"
SPEC="$(curl -s -b "$COOKIES" "$BASE/aperio/api/openapi.json")"
assert_contains "$SPEC" '"openapi"' "openapi document is served"
assert_contains "$SPEC" '/aperio/api/tokens/refresh' "openapi document covers the token refresh endpoint"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/aperio/api/openapi.json")"
assert_status 302 "$CODE" "openapi document requires a dashboard session"

step "Dashboard users & role-based access"
# Master-token session is a built-in admin: it can create users.
USER="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"username":"e2e-viewer","password":"viewer-password","role":"viewer"}' "$BASE/aperio/api/users")" \
  || fail "user creation failed"
assert_contains "$USER" '"role":"viewer"' "created a viewer user"
assert_contains "$USER" '"username":"e2e-viewer"' "the created user carries its username"
USER_ID="$(echo "$USER" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
[ -n "$USER_ID" ] || fail "could not parse the user id: $USER"
# Short passwords and the reserved name are rejected.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"username":"e2e-x","password":"short","role":"viewer"}' "$BASE/aperio/api/users")"
assert_status 400 "$CODE" "a short password is rejected"
# The viewer can log in and read, but not mutate or reach admin-only routes.
VCOOKIES="$LOG_DIR/viewer-cookies.txt"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -c "$VCOOKIES" -X POST -u 'e2e-viewer:viewer-password' "$BASE/aperio/auth")"
assert_status 200 "$CODE" "viewer can sign in"
SESSION="$(curl -s -b "$VCOOKIES" "$BASE/aperio/api/session")"
assert_contains "$SESSION" '"role":"viewer"' "session reports the viewer role"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$VCOOKIES" "$BASE/aperio/api/stats")"
assert_status 200 "$CODE" "viewer can read stats"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$VCOOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"nope","hostnames":["*"]}' "$BASE/aperio/api/tokens")"
assert_status 403 "$CODE" "viewer cannot create a token (needs operator)"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$VCOOKIES" "$BASE/aperio/api/users")"
assert_status 403 "$CODE" "viewer cannot list users (admin only)"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$VCOOKIES" "$BASE/aperio/api/settings")"
assert_status 403 "$CODE" "viewer cannot read settings (admin only)"
# Delete the user; the admin session still can.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/users/${USER_ID}")"
assert_status 200 "$CODE" "admin can delete the user"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -c "$VCOOKIES" -X POST -u 'e2e-viewer:viewer-password' "$BASE/aperio/auth")"
assert_status 401 "$CODE" "the deleted user can no longer sign in"

step "TOTP two-factor authentication"
MFA="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"username":"e2e-mfa","password":"mfa-password","role":"operator"}' "$BASE/aperio/api/users")" \
  || fail "mfa user creation failed"
MFA_ID="$(echo "$MFA" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
MCOOKIES="$LOG_DIR/cookies-mfa.txt"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -c "$MCOOKIES" -X POST -u 'e2e-mfa:mfa-password' "$BASE/aperio/auth")"
assert_status 200 "$CODE" "the mfa user signs in with password only before enrollment"
SETUP="$(curl -sf -b "$MCOOKIES" -X POST "$BASE/aperio/api/me/totp/setup")" || fail "totp setup failed"
SECRET="$(echo "$SETUP" | sed -n 's/.*"secret":"\([^"]*\)".*/\1/p')"
[ -n "$SECRET" ] || fail "could not parse the totp secret: $SETUP"
assert_contains "$SETUP" 'otpauth://totp/Aperio:e2e-mfa' "setup returns the provisioning URL"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$MCOOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"code":"000000"}' "$BASE/aperio/api/me/totp/enable")"
assert_status 400 "$CODE" "a wrong code does not complete enrollment"
ENABLE="$(curl -sf -b "$MCOOKIES" -X POST -H 'Content-Type: application/json' \
  --data "{\"code\":\"$(totp_code "$SECRET")\"}" "$BASE/aperio/api/me/totp/enable")" \
  || fail "totp enable failed"
assert_contains "$ENABLE" '"recovery_codes"' "enrollment returns recovery codes"
RECOVERY="$(echo "$ENABLE" | sed -n 's/.*"recovery_codes":\["\([^"]*\)".*/\1/p')"
[ -n "$RECOVERY" ] || fail "could not parse a recovery code: $ENABLE"
LIST="$(curl -s -b "$COOKIES" "$BASE/aperio/api/users")"
assert_contains "$LIST" '"totp":true' "the users list reports totp as enabled"
# Password-only login now asks for the second factor without creating a session.
RESP_HEADERS="$(curl -s -D - -o /dev/null -X POST -u 'e2e-mfa:mfa-password' "$BASE/aperio/auth")"
assert_contains "$RESP_HEADERS" '401' "password-only login is refused once totp is on"
assert_contains "$RESP_HEADERS" 'x-aperio-totp: required' "the response asks for the totp code"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -u 'e2e-mfa:mfa-password' \
  -H 'X-Aperio-Totp: 000000' "$BASE/aperio/auth")"
assert_status 401 "$CODE" "a wrong totp code is refused"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -c "$MCOOKIES" -X POST -u 'e2e-mfa:mfa-password' \
  -H "X-Aperio-Totp: $(totp_code "$SECRET")" "$BASE/aperio/auth")"
assert_status 200 "$CODE" "password + totp code signs in"
# A recovery code works exactly once.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -u 'e2e-mfa:mfa-password' \
  -H "X-Aperio-Totp: $RECOVERY" "$BASE/aperio/auth")"
assert_status 200 "$CODE" "a recovery code signs in"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -u 'e2e-mfa:mfa-password' \
  -H "X-Aperio-Totp: $RECOVERY" "$BASE/aperio/auth")"
assert_status 401 "$CODE" "a spent recovery code is refused"
# Admin reset clears totp; password-only login works again.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/users/${MFA_ID}/totp")"
assert_status 200 "$CODE" "admin can reset a user's totp"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -u 'e2e-mfa:mfa-password' "$BASE/aperio/auth")"
assert_status 200 "$CODE" "password-only login works again after the reset"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/users/${MFA_ID}")"
assert_status 200 "$CODE" "the mfa user can be deleted"

step "Passkey (WebAuthn) API surface"
# Passkeys are disabled without APERIO_WEBAUTHN_ORIGIN: the probe says so and
# the ceremonies answer 501.
AVAIL="$(curl -s "$BASE/aperio/auth/passkey")"
assert_contains "$AVAIL" '"available":false' "passkey probe reports not configured"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
  --data '{"username":"nobody"}' "$BASE/aperio/auth/passkey/start")"
assert_status 501 "$CODE" "passkey login start answers 501 when not configured"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST "$BASE/aperio/api/me/passkeys/register/start")"
assert_status 501 "$CODE" "passkey registration answers 501 when not configured"

step "Structured access log"
[ -f "$ACCESS_LOG" ] || fail "access log file was not created"
assert_contains "$(cat "$ACCESS_LOG")" '"uri":"/hello' "access log records proxied requests"
assert_contains "$(cat "$ACCESS_LOG")" '"token":"master"' "access log attributes the token"

step "Token API lifecycle (list, edit, revoke)"
TOK="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"e2e-edit","hostnames":["edit.e2e.local"]}' "$BASE/aperio/api/tokens")" \
  || fail "token creation failed"
EDIT_ID="$(echo "$TOK" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
[ -n "$EDIT_ID" ] || fail "could not parse the token id: $TOK"
LIST="$(curl -s -b "$COOKIES" "$BASE/aperio/api/tokens")"
assert_contains "$LIST" '"name":"e2e-edit"' "token list includes the created token"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"hostnames":["edited.e2e.local"],"max_rps":5,"ttl_seconds":600,"allow_public":true}' \
  "$BASE/aperio/api/tokens/${EDIT_ID}")"
assert_status 200 "$CODE" "token scope can be edited"
LIST="$(curl -s -b "$COOKIES" "$BASE/aperio/api/tokens")"
assert_contains "$LIST" 'edited.e2e.local' "the edit updated the hostname scope"
assert_contains "$LIST" '"allow_public":true' "the edit set the public-publish flag"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"hostnames":["bad host"]}' "$BASE/aperio/api/tokens/${EDIT_ID}")"
assert_status 400 "$CODE" "an invalid hostname permission is rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X PUT -H 'Content-Type: application/json' \
  --data '{"name":"x"}' "$BASE/aperio/api/tokens/no-such-token")"
assert_status 404 "$CODE" "editing an unknown token returns 404"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/tokens/${EDIT_ID}")"
assert_status 200 "$CODE" "token revocation succeeds"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X DELETE "$BASE/aperio/api/tokens/${EDIT_ID}")"
assert_status 404 "$CODE" "revoking the same token twice returns 404"

step "Client control API (overrule + kill switch)"
CLIENT_ID="$(curl -s -b "$COOKIES" "$BASE/aperio/api/stats" | "$PYTHON" -c \
  "import sys,json; print(json.load(sys.stdin)['active_clients'][0]['id'])")" \
  || fail "could not read the client id from stats"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"hostname_bind":"bad host"}' "$BASE/aperio/api/clients/${CLIENT_ID}/override")"
assert_status 400 "$CODE" "an invalid override hostname is rejected"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"path_bind":"/x"}' "$BASE/aperio/api/clients/no-such-client/override")"
assert_status 404 "$CODE" "overruling an unknown client returns 404"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"path_bind":"/ov"}' "$BASE/aperio/api/clients/${CLIENT_ID}/override")"
assert_status 200 "$CODE" "a path override can be applied"
STATS="$(curl -s -b "$COOKIES" "$BASE/aperio/api/stats")"
assert_contains "$STATS" '"override_path_bind":"/ov"' "stats reflect the applied override"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"path_bind":"","hostname_bind":""}' "$BASE/aperio/api/clients/${CLIENT_ID}/override")"
assert_status 200 "$CODE" "the override can be cleared"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"enabled":false}' "$BASE/aperio/api/clients/${CLIENT_ID}/enabled")"
assert_status 200 "$CODE" "the client can be disabled (kill switch)"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Host: ${HOSTNAME_BIND}" "$BASE/hello")"
assert_status 504 "$CODE" "a disabled client no longer receives traffic"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"enabled":true}' "$BASE/aperio/api/clients/no-such-client/enabled")"
assert_status 404 "$CODE" "toggling an unknown client returns 404"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"enabled":true}' "$BASE/aperio/api/clients/${CLIENT_ID}/enabled")"
assert_status 200 "$CODE" "the client can be re-enabled"
retry 10 sh -c "curl -s -H 'Host: ${HOSTNAME_BIND}' '$BASE/hello' | grep -q 'backend ${BACKEND_PORT} GET /hello'" \
  || fail "traffic did not resume after re-enabling the client"
echo "  ok: traffic resumes after re-enabling the client"

stop_server
