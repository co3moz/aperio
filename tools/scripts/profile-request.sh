#!/usr/bin/env bash
# Profiles ONE request through the tunnel and prints where its time went.
#
# Sends a single request to the given URL, locates its capture on the server
# (the inspector records a microsecond RequestTimeline for every proxied
# request), and renders the stage waterfall next to what curl saw from the
# visitor's side. Useful for answering "what does one request cost, and in
# which hop" without running a load test.
#
# Usage:
#   scripts/profile-request.sh <url> [extra curl args...]
#
# Environment:
#   APERIO_ADMIN_TOKEN  The master token; the script signs in to the dashboard
#                       with it (aperio:<token>) for the admin API (falls back
#                       to APERIO_SERVER_TOKEN). Required.
#   APERIO_ADMIN_URL    Admin API base. Default: the scheme://host:port of <url>.
#   APERIO_PROFILE_NO_MARKER=1
#                       Do not append the ?_aperio_profile=<id> query marker
#                       (use when the backend rejects unknown query keys); the
#                       capture is then matched by path + recency, which can
#                       misattribute under concurrent traffic.
#
# Requirements: curl, python3. The server must run with the inspector enabled
# (the default) and the service must not set capture: false.
#
# Notes on reading the output:
#   - Client-side stages are anchored estimates when marked (est): the client
#     measures its own durations and the server splits the unaccounted tunnel
#     transit evenly between the two directions; clocks are never mixed.
#   - A streamed response reports its stages at the head, where the body is
#     still arriving, so "backend body read" is replaced by one span from the
#     backend's first byte to the head reaching the tunnel. The rest of the
#     body streams afterwards, inside the visitor delivery gap.
#   - "visitor delivery gap" is curl's total minus the server's finished
#     timestamp: kernel buffers and the last write to the visitor socket.

set -euo pipefail

if [ $# -lt 1 ] || [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
  sed -n '2,33p' "$0" | sed 's/^# \{0,1\}//'
  exit 1
fi

URL="$1"
shift

TOKEN="${APERIO_ADMIN_TOKEN:-${APERIO_SERVER_TOKEN:-}}"
if [ -z "$TOKEN" ]; then
  echo "error: set APERIO_ADMIN_TOKEN (or APERIO_SERVER_TOKEN)" >&2
  exit 1
fi

# Admin base defaults to the origin of the target URL.
ADMIN="${APERIO_ADMIN_URL:-$(python3 -c 'import sys,urllib.parse as u; p=u.urlsplit(sys.argv[1]); print(f"{p.scheme}://{p.netloc}")' "$URL")}"

# The admin API authenticates by dashboard session; sign in once with the
# master token and carry the cookie jar through the two lookups below.
JAR="$(mktemp)"
trap 'rm -f "$JAR"' EXIT
LOGIN_CODE=$(curl -sS -o /dev/null -w '%{http_code}' -c "$JAR" -X POST -u "aperio:$TOKEN" "$ADMIN/aperio/auth")
if [ "$LOGIN_CODE" != "200" ]; then
  echo "error: dashboard login at $ADMIN/aperio/auth failed (HTTP $LOGIN_CODE); wrong token?" >&2
  exit 1
fi

# A unique query marker makes the capture identifiable even under concurrent
# traffic; the backend sees one extra harmless query parameter.
MARKER=""
REQ_URL="$URL"
if [ "${APERIO_PROFILE_NO_MARKER:-0}" != "1" ]; then
  MARKER="profile-$(date +%s)-$$"
  case "$URL" in
    *\?*) REQ_URL="${URL}&_aperio_profile=${MARKER}" ;;
    *) REQ_URL="${URL}?_aperio_profile=${MARKER}" ;;
  esac
fi

# 1. The one request, timed from the visitor's chair.
CURL_STATS=$(curl -sS -o /dev/null \
  -w '{"status":%{http_code},"bytes":%{size_download},"ttfb_s":%{time_starttransfer},"total_s":%{time_total}}' \
  "$@" "$REQ_URL")

# 2. Find its capture id. The traffic log strips the query string, so path
# recency only narrows the candidates; the marker is then matched against the
# capture detail, whose uri keeps the query. The capture is written right
# after the response completes; give the collector a moment.
REQ_PATH="$(python3 -c 'import sys,urllib.parse as u; print(u.urlsplit(sys.argv[1]).path or "/")' "$URL")"
REQ_ID=""
for _ in $(seq 1 20); do
  CANDIDATES=$(curl -sS -b "$JAR" "$ADMIN/aperio/api/logs" \
    | python3 -c '
import json, sys
path = sys.argv[1]
logs = json.load(sys.stdin)
ids = [l["id"] for l in logs if l.get("uri") == path]
# Newest first, and only the recent tail: the request just happened.
print("\n".join(reversed(ids[-10:])))
' "$REQ_PATH")
  for CAND in $CANDIDATES; do
    if [ -z "$MARKER" ]; then
      # No marker: take the newest capture of this path.
      REQ_ID="$CAND"
      break
    fi
    if curl -sS -b "$JAR" "$ADMIN/aperio/api/requests/$CAND" \
      | python3 -c '
import json, sys
cap = json.load(sys.stdin)
sys.exit(0 if sys.argv[1] in cap.get("uri", "") else 1)
' "$MARKER" 2>/dev/null; then
      REQ_ID="$CAND"
      break
    fi
  done
  [ -n "$REQ_ID" ] && break
  sleep 0.1
done
if [ -z "$REQ_ID" ]; then
  echo "error: request not found in $ADMIN/aperio/api/logs." >&2
  echo "  Is the inspector on (APERIO_INSPECTOR, default on), the service" >&2
  echo "  without capture: false, and the token allowed to see this org?" >&2
  exit 1
fi

# 3. Fetch the capture and render the waterfall.
CAP_FILE="$(mktemp)"
trap 'rm -f "$JAR" "$CAP_FILE"' EXIT
curl -sS -b "$JAR" "$ADMIN/aperio/api/requests/$REQ_ID" > "$CAP_FILE"

CAP_FILE="$CAP_FILE" CURL_STATS="$CURL_STATS" REQ_ID="$REQ_ID" python3 <<'PY'
import json, os, sys

cap = json.load(open(os.environ["CAP_FILE"]))
curl = json.loads(os.environ["CURL_STATS"])
tl = cap.get("timeline")

kind = "streamed" if cap.get("resp_streamed") else "buffered"
print(f"request  {cap.get('method', '?')} {cap.get('uri', '?')}")
print(f"capture  {os.environ['REQ_ID']}  status {cap.get('status')}  {kind}  "
      f"server duration {cap.get('duration_ms')} ms")
print(f"visitor  ttfb {curl['ttfb_s']*1000:.1f} ms  total {curl['total_s']*1000:.1f} ms  "
      f"body {curl['bytes']} bytes")
if not tl:
    print("no timeline on this capture (older server, or capture evicted)")
    sys.exit(0)

est = tl.get("estimated_anchor", False)
total = tl["finished_us"]

b = tl.get
stages = []
def add(label, start, end, estimated=False):
    # A None boundary means the stage was not recorded; it collapses away.
    if start is None or end is None:
        return
    stages.append((label, start, end, estimated))

add("wait for a client",       0,                    b("client_ready_us"))
add("admission (concurrency)", b("client_ready_us"), b("admitted_us"))
add("routing",                 b("admitted_us"),     b("selected_us"))
add("dispatch prep",           b("selected_us"),     b("dispatched_us"))
if b("client_received_us") is not None:
    add("tunnel -> client",   b("dispatched_us"),         b("client_received_us"), est)
    add("client prep",        b("client_received_us"),    b("backend_sent_us"), est)
    add("backend ttfb",       b("backend_sent_us"),       b("backend_first_byte_us"), est)
    if b("backend_done_us") is not None:
        add("backend body read",  b("backend_first_byte_us"), b("backend_done_us"), est)
        add("client hand-off",    b("backend_done_us"),       b("client_responded_us"), est)
    else:
        # Streamed: the head was reported before the body finished, so these
        # two are one span and there is no end of body to draw.
        add("head hand-off",      b("backend_first_byte_us"), b("client_responded_us"), est)
    add("tunnel -> server",   b("client_responded_us"),   b("response_received_us"), est)
else:
    add("tunnel round trip", b("dispatched_us"), b("response_received_us"))
add("serve to visitor",        b("response_received_us"), b("finished_us"))

def fmt(us):
    # The timeline is microsecond-resolution; show short stages in it rather
    # than as a wall of 0.00x ms.
    if us >= 1000:
        return f"{us / 1000:>9.3f} ms"
    return f"{us:>9.1f} us"

width = max(len(s[0]) for s in stages)
print()
for label, start, end, estimated in stages:
    dur = max(0, end - start)
    pct = 100.0 * dur / total if total else 0.0
    bar = "#" * max(1, round(pct / 2)) if dur else ""
    mark = " (est)" if estimated else ""
    print(f"  {label:<{width}}  {fmt(dur)}  {pct:>5.1f}%  {bar}{mark}")

total_label = "total (server-side)"
gap_label = "visitor delivery gap"
gap_us = curl["total_s"] * 1_000_000 - total
print(f"  {total_label:<{width}}  {fmt(total)}")
print(f"  {gap_label:<{width}}  {fmt(gap_us)}  (curl total - server finished)")
if cap.get("resp_streamed"):
    print()
    print("  streamed response: the stages above end at the response head; the")
    print("  body streams afterwards, inside the visitor delivery gap.")
PY
