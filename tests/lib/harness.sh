#!/usr/bin/env bash
# Shared harness for the end-to-end suite: configuration, process
# lifecycle, assertion helpers, and mock backends. Sourced by tests/e2e.sh
# and by each tests/phases/*.sh file (never run directly).

SERVER_PORT=18100
BACKEND_PORT=18101
BACKEND2_PORT=18102
CACHE_BACKEND_PORT=18108
BASE="http://127.0.0.1:${SERVER_PORT}"
TOKEN="e2e-master-token-$(date +%s)"
HOSTNAME_BIND="app.e2e.local"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# The target dir may be relocated via config.toml; ask cargo when available.
TARGET_DIR="${CARGO_TARGET_DIR:-$(cd "$ROOT" && cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' | sed 's/\\\\/\//g')}"
TARGET_DIR="${TARGET_DIR:-$ROOT/target}"
SERVER_BIN="${APERIO_SERVER_BIN:-$TARGET_DIR/debug/aperio-server}"
CLIENT_BIN="${APERIO_CLIENT_BIN:-$TARGET_DIR/debug/aperio-client}"
# Windows (Git Bash) compatibility.
[ ! -e "$SERVER_BIN" ] && [ -e "$SERVER_BIN.exe" ] && SERVER_BIN="$SERVER_BIN.exe"
[ ! -e "$CLIENT_BIN" ] && [ -e "$CLIENT_BIN.exe" ] && CLIENT_BIN="$CLIENT_BIN.exe"

LOG_DIR="$(mktemp -d)"
DATA_DIR=""
PHASE="init"
SERVER_PID=""
CLIENT_PIDS=()
BACKEND_PIDS=()
FAILED=0

# Pick a python that actually runs (the Windows Store ships a python3 stub
# that only prints an install hint).
PYTHON=""
for candidate in python3 python; do
  if "$candidate" -c 'pass' >/dev/null 2>&1; then
    PYTHON="$candidate"
    break
  fi
done
[ -n "$PYTHON" ] || { echo "FAIL: no working python interpreter found" >&2; exit 1; }

cleanup() {
  stop_server || true
  for pid in "${BACKEND_PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
  done
  if [ "$FAILED" -ne 0 ]; then
    for f in "$LOG_DIR"/*.log; do
      echo "--- $f ----------------------------------------------------------"
      cat "$f" 2>/dev/null || true
    done
  fi
  rm -rf "$LOG_DIR"
}
trap cleanup EXIT

step() { echo; echo "==> [$PHASE] $*"; }

fail() {
  echo "FAIL: [$PHASE] $*" >&2
  FAILED=1
  exit 1
}

assert_contains() { # <haystack> <needle> <label>
  case "$1" in
    *"$2"*) echo "  ok: $3" ;;
    *) fail "$3 — expected to contain '$2', got: $1" ;;
  esac
}

assert_status() { # <expected> <actual> <label>
  if [ "$1" != "$2" ]; then
    fail "$3 — expected HTTP $1, got HTTP $2"
  fi
  echo "  ok: $3"
}

retry() { # <seconds> <command...>
  local deadline=$(( $(date +%s) + $1 )); shift
  until "$@" >/dev/null 2>&1; do
    if [ "$(date +%s)" -ge "$deadline" ]; then
      return 1
    fi
    sleep 1
  done
}

# Starts a mock backend on the given port. Responses embed the port so tests
# can tell which backend served them; /slow sleeps 5 seconds first.
start_backend() { # <port>
  "$PYTHON" - "$1" <<'PYEOF' >"$LOG_DIR/backend-$1.log" 2>&1 &
import http.server, sys, time

class Handler(http.server.BaseHTTPRequestHandler):
    def _respond(self, body):
        data = body.encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/plain')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path.startswith('/slow'):
            time.sleep(5)
        port = self.server.server_address[1]
        self._respond(f'backend {port} GET {self.path}')

    def do_POST(self):
        length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(length).decode()
        port = self.server.server_address[1]
        self._respond(f'backend {port} POST {self.path} body={body}')

    def log_message(self, *args):
        pass

http.server.HTTPServer(('127.0.0.1', int(sys.argv[1])), Handler).serve_forever()
PYEOF
  BACKEND_PIDS+=($!)
  retry 10 curl -sf "http://127.0.0.1:$1/ping" || fail "mock backend :$1 did not come up"
}

# Starts aperio-server with a fresh data dir; extra env pairs come as args.
start_server() { # [KEY=VAL ...]
  DATA_DIR="$(mktemp -d)"
  env PORT="$SERVER_PORT" \
    APERIO_SERVER_TOKEN="$TOKEN" \
    APERIO_DATA_DIR="$DATA_DIR" \
    APERIO_RANDOM_SUBDOMAIN='*.e2e.local' \
    APERIO_GATEWAY_TIMEOUT=3 \
    APERIO_UPTIME_TICK_SECS=1 \
    APERIO_WEBHOOK_RETRY_SCHEDULE=0 \
    "$@" \
    "$SERVER_BIN" >"$LOG_DIR/server-$PHASE.log" 2>&1 &
  SERVER_PID=$!
  retry 15 curl -sf "$BASE/aperio/health" || fail "server did not come up"
}

stop_server() {
  for pid in "${CLIENT_PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
  done
  CLIENT_PIDS=()
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    SERVER_PID=""
    sleep 1
  fi
}

# Starts aperio-client against a backend; extra env pairs come as args.
start_client() { # <name> <backend_port> [KEY=VAL ...]
  local name="$1" backend_port="$2"
  shift 2
  # Pin one connection per service so per-client-count assertions stay
  # deterministic regardless of the client's default; a phase may override by
  # passing APERIO_CONNECTIONS=N as an extra pair.
  env APERIO_CONNECTIONS=1 \
    APERIO_SERVER_URL="$BASE" \
    APERIO_SERVER_TOKEN="$TOKEN" \
    APERIO_TARGET="http://127.0.0.1:${backend_port}" \
    "$@" \
    "$CLIENT_BIN" >"$LOG_DIR/client-$PHASE-$name.log" 2>&1 &
  CLIENT_PIDS+=($!)
}

# Logs into the dashboard and stores the session cookie jar at $1.
dashboard_login() { # <cookie-jar>
  local code
  code="$(curl -s -o /dev/null -w '%{http_code}' -c "$1" -X POST -u "aperio:${TOKEN}" "$BASE/aperio/auth")"
  assert_status 200 "$code" "dashboard login"
}

wait_routable() { # <host> [path]
  retry 20 curl -sf -H "Host: $1" "$BASE${2:-/hello}" \
    || fail "tunnel for $1 did not become routable in time"
}

# --- Phase-specific helpers (relocated from their original phases) ---

only_main_connected() {
  test "$(curl -s -b "$COOKIES" "$BASE/aperio/api/stats" \
    | "$PYTHON" -c 'import sys,json; print(len(json.load(sys.stdin).get("active_clients", [])))')" -eq 1
}

totp_code() { # <base32-secret> [offset-steps]
  python3 - "$1" "${2:-0}" <<'PYEOF'
import base64, hashlib, hmac, struct, sys, time
secret = sys.argv[1]
pad = "=" * (-len(secret) % 8)
key = base64.b32decode(secret + pad, casefold=True)
step = int(time.time()) // 30 + int(sys.argv[2])
digest = hmac.new(key, struct.pack(">Q", step), hashlib.sha1).digest()
o = digest[19] & 0x0F
code = (struct.unpack(">I", digest[o:o+4])[0] & 0x7FFFFFFF) % 1000000
print(f"{code:06d}")
PYEOF
}

start_redirect_backend() { # <port> <target_port>
  "$PYTHON" - "$1" "$2" <<'PYEOF' >"$LOG_DIR/backend-redirect.log" 2>&1 &
import http.server, sys

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith('/ext'):
            location = 'http://unrelated.invalid/'
        else:
            location = f'http://127.0.0.1:{sys.argv[2]}/hello'
        self.send_response(301)
        self.send_header('Location', location)
        self.send_header('Content-Length', '0')
        self.end_headers()

    def log_message(self, *args):
        pass

http.server.HTTPServer(('127.0.0.1', int(sys.argv[1])), Handler).serve_forever()
PYEOF
  BACKEND_PIDS+=($!)
  retry 10 sh -c "curl -s -o /dev/null 'http://127.0.0.1:$1/ping'" \
    || fail "redirect backend :$1 did not come up"
}

start_ws_backend() { # <port>
  "$PYTHON" - "$1" <<'PYEOF' >"$LOG_DIR/backend-ws.log" 2>&1 &
import base64, hashlib, socket, sys, threading

MAGIC = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

def recvn(c, n):
    buf = b""
    while len(buf) < n:
        d = c.recv(n - len(buf))
        if not d:
            return None
        buf += d
    return buf

def handle(c):
    data = b""
    while b"\r\n\r\n" not in data:
        d = c.recv(4096)
        if not d:
            return
        data += d
    key = ""
    for line in data.decode("latin1").split("\r\n"):
        if line.lower().startswith("sec-websocket-key:"):
            key = line.split(":", 1)[1].strip()
    if not key:
        c.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
        c.close()
        return
    accept = base64.b64encode(hashlib.sha1((key + MAGIC).encode()).digest()).decode()
    c.sendall(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
               "Connection: Upgrade\r\nSec-WebSocket-Accept: %s\r\n\r\n" % accept).encode())
    while True:
        hdr = recvn(c, 2)
        if hdr is None:
            return
        opcode = hdr[0] & 0x0F
        ln = hdr[1] & 0x7F
        masked = hdr[1] & 0x80
        if ln == 126:
            ln = int.from_bytes(recvn(c, 2), "big")
        elif ln == 127:
            ln = int.from_bytes(recvn(c, 8), "big")
        mask = recvn(c, 4) if masked else b""
        payload = recvn(c, ln) if ln else b""
        if payload is None:
            return
        if masked and payload:
            payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        if opcode == 8:
            c.sendall(b"\x88\x00")
            c.close()
            return
        if opcode in (1, 2):
            resp = b"echo:" + payload
            c.sendall(bytes([0x80 | opcode, len(resp)]) + resp)

srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", int(sys.argv[1])))
srv.listen(8)
while True:
    conn, _ = srv.accept()
    threading.Thread(target=handle, args=(conn,), daemon=True).start()
PYEOF
  BACKEND_PIDS+=($!)
  retry 10 curl -sf "http://127.0.0.1:$1/ping" || fail "ws backend :$1 did not come up"
}

start_tcp_echo() { # <port>
  "$PYTHON" - "$1" <<'PYEOF' >"$LOG_DIR/backend-tcpecho.log" 2>&1 &
import socket, sys, threading

def go(c):
    while True:
        d = c.recv(4096)
        if not d:
            break
        c.sendall(b"echo:" + d)
    c.close()

srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", int(sys.argv[1])))
srv.listen(8)
while True:
    c, _ = srv.accept()
    threading.Thread(target=go, args=(c,), daemon=True).start()
PYEOF
  BACKEND_PIDS+=($!)
}

backend_health() {
  curl -s -b "$HJAR" "$BASE/aperio/api/stats" | "$PYTHON" -c \
    "import sys,json; cs=[c for c in json.load(sys.stdin)['active_clients']]; print(cs[0]['backend_healthy'] if cs else 'none')"
}
