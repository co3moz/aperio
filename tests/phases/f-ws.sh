#!/usr/bin/env bash
# Phase F: ws. Sourced by tests/e2e.sh after the harness.
PHASE="ws"

WS_PORT=18104

# Minimal RFC6455 WebSocket echo backend (stdlib only): answers plain HTTP
# with 200/ok (so wait_routable works), upgrades WS handshakes, and echoes
# every text/binary frame back prefixed with "echo:".

# WebSocket probe: connects THROUGH the aperio server (Host header routing),
# performs the upgrade, sends one masked text frame, prints the echoed frame,
# and closes cleanly.
cat >"$LOG_DIR/ws_probe.py" <<'PYEOF'
import base64, os, socket, sys

port, host = int(sys.argv[1]), sys.argv[2]
s = socket.create_connection(("127.0.0.1", port), timeout=15)
key = base64.b64encode(os.urandom(16)).decode()
s.sendall(("GET /ws-echo HTTP/1.1\r\nHost: %s\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
           "Sec-WebSocket-Key: %s\r\nSec-WebSocket-Version: 13\r\n\r\n" % (host, key)).encode())
resp = b""
while b"\r\n\r\n" not in resp:
    d = s.recv(4096)
    if not d:
        print("no handshake response")
        sys.exit(1)
    resp += d
if b" 101" not in resp.split(b"\r\n", 1)[0]:
    print("handshake failed: %r" % resp[:200])
    sys.exit(1)

def recvn(n):
    buf = b""
    while len(buf) < n:
        d = s.recv(n - len(buf))
        if not d:
            print("connection closed early")
            sys.exit(1)
        buf += d
    return buf

payload = b"hello-ws"
mask = os.urandom(4)
s.sendall(bytes([0x81, 0x80 | len(payload)]) + mask +
          bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))
hdr = recvn(2)
data = recvn(hdr[1] & 0x7F)
print(data.decode("latin1"))
# Close cleanly so the WsClose path is exercised end to end.
mask = os.urandom(4)
s.sendall(bytes([0x88, 0x80]) + mask)
s.close()
sys.exit(0 if data == b"echo:hello-ws" else 1)
PYEOF

step "WebSocket pass-through"
start_server
start_ws_backend "$WS_PORT"
start_client ws "$WS_PORT" APERIO_HOSTNAME_BIND=ws.e2e.local
wait_routable ws.e2e.local "/ping"
WS_OUT="$("$PYTHON" "$LOG_DIR/ws_probe.py" "$SERVER_PORT" ws.e2e.local)" \
  || fail "ws probe failed: $WS_OUT"
assert_contains "$WS_OUT" "echo:hello-ws" "WS frame echoed through the tunnel"

stop_server
