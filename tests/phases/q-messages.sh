#!/usr/bin/env bash
# Phase Q: messages between the clients of an organization. Sourced by
# tests/e2e.sh after the harness.
PHASE="messages"

step "Publish reaches a subscriber, once per client process"
start_server
MSG_BACKEND_PORT=18113
start_backend "$MSG_BACKEND_PORT"
MSG_BACKEND_PID=$!

MSG_FACE_PORT=18891
# Two connections for one process, so the per-process delivery rule is under
# test rather than assumed: keyed on the connection this subscriber would
# receive every message twice.
start_client msgsub "$MSG_BACKEND_PORT" \
  APERIO_CONNECTIONS=2 \
  APERIO_HOSTNAME=msgsub.e2e.local \
  APERIO_SUBSCRIBE='deploy/#' \
  APERIO_MESSAGES_LISTEN="127.0.0.1:${MSG_FACE_PORT}"
wait_routable msgsub.e2e.local /hello
retry 20 curl -sf -o /dev/null "http://127.0.0.1:${MSG_FACE_PORT}/" \
  || fail "the message face did not come up"

MJAR="$LOG_DIR/cookies-messages.txt"
dashboard_login "$MJAR"

# Attach a subscriber and give it a moment to register.
SSE_OUT="$LOG_DIR/messages-sse.txt"
curl -sN --max-time 12 "http://127.0.0.1:${MSG_FACE_PORT}/subscribe?topic=deploy%2F%23" \
  >"$SSE_OUT" 2>&1 &
SSE_PID=$!
sleep 1

PUBLISHED="$(curl -s -b "$MJAR" -X POST "$BASE/aperio/api/publish" \
  -H 'content-type: application/json' \
  -d '{"topic":"deploy/web","payload":"ship-it"}')"
echo "$PUBLISHED" | grep -q '"clients":1' \
  || fail "one client process should have received it once, got: $PUBLISHED"

# A topic nobody asked for must not arrive, and must not be an error either.
curl -s -o /dev/null -b "$MJAR" -X POST "$BASE/aperio/api/publish" \
  -H 'content-type: application/json' \
  -d '{"topic":"metrics/cpu","payload":"99"}'

retry 20 grep -q "event: deploy/web" "$SSE_OUT" \
  || fail "the subscriber did not receive the message"
[ "$(grep -c 'event: deploy/web' "$SSE_OUT")" = "1" ] \
  || fail "one publish, one delivery: $(grep -c 'event: deploy/web' "$SSE_OUT") arrived"
grep -q "event: metrics/cpu" "$SSE_OUT" \
  && fail "a topic the client did not subscribe to was delivered"
# ship-it, Base64, because a payload is bytes and an SSE field is a line.
grep -q "data: c2hpcC1pdA==" "$SSE_OUT" || fail "the payload did not survive the trip"
echo "  ok: a published message reaches the subscribing process exactly once"

step "The local face publishes, and the server's own events are on \$aperio/"
# Publishing through the face needs no admin credential: the client's own
# token carries it.
FACE_PUB="$(curl -s -o /dev/null -w '%{http_code}' -X POST \
  "http://127.0.0.1:${MSG_FACE_PORT}/publish?topic=deploy%2Ffrom-face" --data 'local')"
assert_status 202 "$FACE_PUB" "publish through the local face"
retry 20 grep -q "event: deploy/from-face" "$SSE_OUT" \
  || fail "a message published through the face did not come back"
echo "  ok: the local face publishes over the client's own tunnel"

# `$aperio/` is the server's: a client may not publish into it.
FORGED="$(curl -s -o /dev/null -w '%{http_code}' -X POST \
  "http://127.0.0.1:${MSG_FACE_PORT}/publish?topic=%24aperio%2Fforged" --data 'x')"
assert_status 400 "$FORGED" "a client publishing into \$aperio/"

# But it may listen, and the events already feeding webhooks arrive there.
EVENTS_OUT="$LOG_DIR/messages-events.txt"
curl -sN --max-time 12 "http://127.0.0.1:${MSG_FACE_PORT}/subscribe?topic=%24aperio%2F%23" \
  >"$EVENTS_OUT" 2>&1 &
EVENTS_PID=$!
sleep 1
curl -s -o /dev/null -b "$MJAR" -X POST "$BASE/aperio/api/tokens" \
  -H 'content-type: application/json' -d '{"name":"e2e-message-token"}'
retry 20 grep -q "event: \$aperio/token/created" "$EVENTS_OUT" \
  || fail "a server event did not reach the subscribing client"
echo "  ok: server events are published on \$aperio/"

kill "$SSE_PID" "$EVENTS_PID" 2>/dev/null || true
kill "$MSG_BACKEND_PID" 2>/dev/null || true
