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

step "A token may only use the topics it carries"
# A dynamic token scoped to one subtree: it may subscribe there and nowhere
# else, and the refusal is named rather than silent.
SCOPED_TOKEN="$(curl -s -b "$MJAR" -X POST "$BASE/aperio/api/tokens" \
  -H 'content-type: application/json' \
  -d '{"name":"e2e-scoped","hostnames":["*"],"paths":["*"],"topics":["deploy/#"]}' \
  | "$PYTHON" -c 'import sys,json; print(json.load(sys.stdin)["token"])')"
[ -n "$SCOPED_TOKEN" ] || fail "could not mint a scoped token"

SCOPED_FACE_PORT=18892
start_client msgscoped "$MSG_BACKEND_PORT" \
  APERIO_SERVER_TOKEN="$SCOPED_TOKEN" \
  APERIO_HOSTNAME=msgscoped.e2e.local \
  APERIO_SUBSCRIBE='deploy/#,secrets/#' \
  APERIO_MESSAGES_LISTEN="127.0.0.1:${SCOPED_FACE_PORT}"
wait_routable msgscoped.e2e.local /hello
retry 20 curl -sf -o /dev/null "http://127.0.0.1:${SCOPED_FACE_PORT}/" \
  || fail "the scoped client's message face did not come up"

SCOPED_OUT="$LOG_DIR/messages-scoped.txt"
curl -sN --max-time 10 "http://127.0.0.1:${SCOPED_FACE_PORT}/subscribe?topic=deploy%2F%23" \
  >"$SCOPED_OUT" 2>&1 &
SCOPED_PID=$!
sleep 1

curl -s -o /dev/null -b "$MJAR" -X POST "$BASE/aperio/api/publish" \
  -H 'content-type: application/json' -d '{"topic":"deploy/scoped","payload":"yes"}'
curl -s -o /dev/null -b "$MJAR" -X POST "$BASE/aperio/api/publish" \
  -H 'content-type: application/json' -d '{"topic":"secrets/rotate","payload":"no"}'

retry 20 grep -q "event: deploy/scoped" "$SCOPED_OUT" \
  || fail "the scoped token did not receive the topic it carries"
grep -q "event: secrets/rotate" "$SCOPED_OUT" \
  && fail "a token received a topic outside its scope"
retry 20 grep -q "Not subscribed to 'secrets/#'" "$LOG_DIR/client-$PHASE-msgscoped.log" \
  || fail "the client should be told which filter was refused, and why"
echo "  ok: a scoped token is fenced to the topics it carries"

kill "$SCOPED_PID" 2>/dev/null || true


step "An ordinary MQTT client talks to the client's MQTT face"
# The probe is a hand-rolled MQTT client: the face encodes with `mqttbytes`,
# so a test using the same crate would agree with it about any misreading of
# the spec. This is an independent second opinion on the wire format.
MQTT_PORT_A=18841
MQTT_PORT_B=18842
start_client mqtta "$MSG_BACKEND_PORT" \
  APERIO_HOSTNAME=mqtta.e2e.local \
  APERIO_MESSAGES_MQTT_LISTEN="127.0.0.1:${MQTT_PORT_A}"
start_client mqttb "$MSG_BACKEND_PORT" \
  APERIO_HOSTNAME=mqttb.e2e.local \
  APERIO_MESSAGES_MQTT_LISTEN="127.0.0.1:${MQTT_PORT_B}"
wait_routable mqtta.e2e.local /hello
wait_routable mqttb.e2e.local /hello

MQTT_OUT="$LOG_DIR/messages-mqtt.txt"
"$PYTHON" "$HERE/lib/mqtt_probe.py" subscribe 127.0.0.1 "$MQTT_PORT_A" 'deploy/#' 8 \
  >"$MQTT_OUT" 2>&1 &
MQTT_PID=$!
retry 20 grep -q "suback granted=0" "$MQTT_OUT" \
  || fail "the MQTT face did not answer SUBSCRIBE"

# Published from the *other* machine's MQTT face, so the message crosses the
# server rather than staying inside one process.
"$PYTHON" "$HERE/lib/mqtt_probe.py" publish 127.0.0.1 "$MQTT_PORT_B" 'deploy/mqtt' 'over-the-tunnel'
retry 20 grep -q "message topic=deploy/mqtt payload=over-the-tunnel" "$MQTT_OUT" \
  || fail "an MQTT publish on one client did not reach an MQTT subscriber on another"
echo "  ok: an MQTT client publishes on one machine and another receives it"
kill "$MQTT_PID" 2>/dev/null || true

step "A QoS 1 message is acknowledged, so it arrives once and stops"
# The resend logic itself is pinned deterministically in the unit tests by
# ageing the timestamps. What only an end-to-end run can show is that the
# acknowledgement actually comes back: if it did not, the server would resend
# every few seconds and the subscriber would see the message again and again.
QOS_OUT="$LOG_DIR/messages-qos1.txt"
curl -sN --max-time 12 "http://127.0.0.1:${MSG_FACE_PORT}/subscribe?topic=deploy%2F%23" \
  >"$QOS_OUT" 2>&1 &
QOS_PID=$!
sleep 1

QOS_PUBLISHED="$(curl -s -b "$MJAR" -X POST "$BASE/aperio/api/publish" \
  -H 'content-type: application/json' \
  -d '{"topic":"deploy/once","payload":"exactly","qos":1}')"
echo "$QOS_PUBLISHED" | grep -q '"qos":1' \
  || fail "the publish should report the qos it was accepted at, got: $QOS_PUBLISHED"

retry 20 grep -q "event: deploy/once" "$QOS_OUT" || fail "the QoS 1 message never arrived"
# Well past two retry timeouts: a missing acknowledgement would have produced
# more copies by now.
sleep 8
COPIES="$(grep -c 'event: deploy/once' "$QOS_OUT")"
[ "$COPIES" = "1" ] \
  || fail "acknowledged once, so it should have arrived once; got $COPIES copies"
echo "  ok: a QoS 1 message is acknowledged and stops being resent"
kill "$QOS_PID" 2>/dev/null || true

kill "$MSG_BACKEND_PID" 2>/dev/null || true
