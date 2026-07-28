#!/usr/bin/env bash
# Phase G: tunnels. Sourced by tests/e2e.sh after the harness.
PHASE="tunnels"

ECHO_PORT=18105
BIND_PORT=18106
BRIDGE_PORT=18107
NAMED_PORT=18121
BOTH_PORT=18122

# Raw TCP echo backend: echoes every received chunk prefixed with "echo:".

# TCP probe: sends argv[2] to 127.0.0.1:argv[1] and prints the response.
cat >"$LOG_DIR/tcp_probe.py" <<'PYEOF'
import socket, sys, time

s = socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=15)
msg = sys.argv[2].encode()
s.sendall(msg)
want = len(b"echo:") + len(msg)
buf = b""
deadline = time.time() + 15
while len(buf) < want and time.time() < deadline:
    d = s.recv(4096)
    if not d:
        break
    buf += d
print(buf.decode("latin1"))
PYEOF

step "Starting aperio-server (emergency tunnels configuration)"
start_server
start_tcp_echo "$ECHO_PORT"

DECL_ID="$("$PYTHON" -c 'import uuid; print(uuid.uuid4())')"
DECL_CFG="$LOG_DIR/decl.yaml"
cat >"$DECL_CFG" <<YAML
server:
  url: ${BASE}
  token: ${TOKEN}
client_id: ${DECL_ID}
target: http://127.0.0.1:${BACKEND_PORT}
hostname: decl.e2e.local
tcp_target: 127.0.0.1:${ECHO_PORT}
tunnels:
  - name: echo-main
    target: 127.0.0.1:${ECHO_PORT}
    protocol: tcp
  - name: echo-both
    target: 127.0.0.1:${ECHO_PORT}
    protocol: tcp/udp
YAML
"$CLIENT_BIN" --config "$DECL_CFG" >"$LOG_DIR/client-tunnels-decl.log" 2>&1 &
CLIENT_PIDS+=($!)
wait_routable decl.e2e.local

step "Tunnel discovery endpoint"
retry 20 sh -c "curl -sf -H 'Authorization: Bearer ${TOKEN}' '$BASE/aperio/tunnels/${DECL_ID}' \
  | grep -q '127.0.0.1:${ECHO_PORT}'" \
  || fail "declared tunnels did not become discoverable in time"
TUNNELS="$(curl -s -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/tunnels/${DECL_ID}")"
assert_contains "$TUNNELS" "\"127.0.0.1:${ECHO_PORT}\"" "discovery lists the declared target"
assert_contains "$TUNNELS" '"protocol":"tcp"' "discovery lists the protocol"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/tunnels/no-such-client")"
assert_status 404 "$CODE" "unknown client ids answer 404"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/aperio/tunnels/${DECL_ID}")"
assert_status 401 "$CODE" "discovery without a token is rejected"
COOKIES="$LOG_DIR/cookies-tunnels.txt"
dashboard_login "$COOKIES"
OTHER="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"other-token"}' "$BASE/aperio/api/tokens")" || fail "token creation failed"
OTHER_TOKEN="$(echo "$OTHER" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
[ -n "$OTHER_TOKEN" ] || fail "could not parse the token response: $OTHER"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${OTHER_TOKEN}" "$BASE/aperio/tunnels/${DECL_ID}")"
assert_status 403 "$CODE" "a different token without allow_bind is rejected"

step "Tunnel listing and the allow_bind capability"
LISTED="$(curl -s -H "Authorization: Bearer ${TOKEN}" "$BASE/aperio/tunnels")"
assert_contains "$LISTED" '"name":"echo-main"' "the listing names the tunnel"
assert_contains "$LISTED" '"available":true' "the listing says whether it can be served"
assert_contains "$LISTED" '"protocol":"tcp/udp"' "a combined declaration is listed as one tunnel"
# A token without the capability sees nothing, rather than being told what it
# cannot have.
EMPTY="$(curl -s -H "Authorization: Bearer ${OTHER_TOKEN}" "$BASE/aperio/tunnels")"
assert_contains "$EMPTY" "[]" "a token without allow_bind lists nothing"
# Granting it opens both the listing and the per-client endpoint, without ever
# handing over the declaring client's own token.
BIND_TOK="$(curl -sf -b "$COOKIES" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"binder-token","allow_bind":true}' "$BASE/aperio/api/tokens")" \
  || fail "allow_bind token creation failed"
BIND_TOKEN="$(echo "$BIND_TOK" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
[ -n "$BIND_TOKEN" ] || fail "could not parse the allow_bind token response: $BIND_TOK"
LISTED2="$(curl -s -H "Authorization: Bearer ${BIND_TOKEN}" "$BASE/aperio/tunnels")"
assert_contains "$LISTED2" '"name":"echo-main"' "allow_bind sees its organization's tunnels"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${BIND_TOKEN}" "$BASE/aperio/tunnels/${DECL_ID}")"
assert_status 200 "$CODE" "allow_bind may read a peer's declarations"
# The raw client_id from the config file resolves, not just the suffixed form.
CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIES" "$BASE/aperio/api/tunnels")"
assert_status 200 "$CODE" "the dashboard lists declared tunnels"
DASH="$(curl -s -b "$COOKIES" "$BASE/aperio/api/tunnels")"
assert_contains "$DASH" '"name":"echo-main"' "the dashboard listing names the tunnel"

step "Binding tunnels with --bind-tunnels (port override)"
BT_CFG="$LOG_DIR/binder.yaml"
cat >"$BT_CFG" <<YAML
bind-tunnels:
  '${DECL_ID}':
    token: ${TOKEN}
    override:
      '127.0.0.1:${ECHO_PORT}': ${BIND_PORT}
YAML
"$CLIENT_BIN" --bind-tunnels "$DECL_ID" --config "$BT_CFG" --server-url "$BASE" \
  >"$LOG_DIR/client-tunnels-binder.log" 2>&1 &
CLIENT_PIDS+=($!)
retry 30 sh -c "\"$PYTHON\" '$LOG_DIR/tcp_probe.py' $BIND_PORT ping | grep -q 'echo:ping'" \
  || fail "bound tunnel did not become usable in time"
OUT="$("$PYTHON" "$LOG_DIR/tcp_probe.py" "$BIND_PORT" ping-123)"
assert_contains "$OUT" "echo:ping-123" "bytes relayed through the bound tunnel"
assert_contains "$(cat "$LOG_DIR/client-tunnels-binder.log")" "Tunnel bound: 127.0.0.1:${BIND_PORT}" \
  "binder honored the port override"

step "Binding by name, with a token that only carries allow_bind"
NAMED_CFG="$LOG_DIR/binder-named.yaml"
cat >"$NAMED_CFG" <<YAML
server:
  url: ${BASE}
  token: ${BIND_TOKEN}
bind-tunnels:
  echo-main: ${NAMED_PORT}
  echo-both: ${BOTH_PORT}
YAML
"$CLIENT_BIN" --bind-tunnels --config "$NAMED_CFG" \
  >"$LOG_DIR/client-tunnels-named.log" 2>&1 &
CLIENT_PIDS+=($!)
retry 30 sh -c "\"$PYTHON\" '$LOG_DIR/tcp_probe.py' $NAMED_PORT ping | grep -q 'echo:ping'" \
  || fail "the name-bound tunnel did not become usable in time"
OUT="$("$PYTHON" "$LOG_DIR/tcp_probe.py" "$NAMED_PORT" ping-named)"
assert_contains "$OUT" "echo:ping-named" "bytes relayed through a tunnel bound by name"
assert_contains "$(cat "$LOG_DIR/client-tunnels-named.log")" "tunnel echo-main" \
  "the binder logs the tunnel by name"
# One `tcp/udp` declaration opens both listeners on the same local port.
NAMED_LOG="$(cat "$LOG_DIR/client-tunnels-named.log")"
assert_contains "$NAMED_LOG" "127.0.0.1:${BOTH_PORT} -> tunnel echo-both -> 127.0.0.1:${ECHO_PORT} (tcp)" \
  "a tcp/udp tunnel opens its tcp half"
assert_contains "$NAMED_LOG" "127.0.0.1:${BOTH_PORT} -> tunnel echo-both -> 127.0.0.1:${ECHO_PORT} (udp)" \
  "a tcp/udp tunnel opens its udp half on the same port"
OUT="$("$PYTHON" "$LOG_DIR/tcp_probe.py" "$BOTH_PORT" ping-both)"
assert_contains "$OUT" "echo:ping-both" "bytes relayed through the tcp half of a tcp/udp tunnel"

step "Legacy tcp bridge"
"$CLIENT_BIN" tcp "$BRIDGE_PORT" --server-url "$BASE" --server-token "$TOKEN" \
  >"$LOG_DIR/client-tunnels-bridge.log" 2>&1 &
CLIENT_PIDS+=($!)
retry 30 sh -c "\"$PYTHON\" '$LOG_DIR/tcp_probe.py' $BRIDGE_PORT ping | grep -q 'echo:ping'" \
  || fail "legacy tcp bridge did not become usable in time"
OUT="$("$PYTHON" "$LOG_DIR/tcp_probe.py" "$BRIDGE_PORT" ping-legacy)"
assert_contains "$OUT" "echo:ping-legacy" "bytes relayed through the legacy bridge"

stop_server
