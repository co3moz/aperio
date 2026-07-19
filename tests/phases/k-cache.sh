#!/usr/bin/env bash
# Phase K: cache. Sourced by tests/e2e.sh after the harness.
PHASE="cache"

step "Response cache + serve-stale for resilient services"
start_server APERIO_CACHE=1 APERIO_CACHE_MAX_STALE=60
# Backend that allows shared caching for 1 second.
"$PYTHON" - "$CACHE_BACKEND_PORT" <<'PYEOF' >"$LOG_DIR/backend-cache.log" 2>&1 &
import http.server, sys

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        data = f'cacheable {self.path}'.encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/plain')
        self.send_header('Cache-Control', 'max-age=1')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass

http.server.HTTPServer(('127.0.0.1', int(sys.argv[1])), Handler).serve_forever()
PYEOF
retry 15 curl -sf "http://127.0.0.1:${CACHE_BACKEND_PORT}/warm" || fail "cache backend did not come up"
WARM="$(curl -s -D - "http://127.0.0.1:${CACHE_BACKEND_PORT}/warm")"
assert_contains "$WARM" "max-age=1" "the cache backend is the one answering its port"

start_client resilient "$CACHE_BACKEND_PORT" APERIO_HOSTNAME_BIND=cache.e2e.local APERIO_CACHE=1 APERIO_RESILIENCE=1
RESILIENT_PID="${CLIENT_PIDS[${#CLIENT_PIDS[@]}-1]}"
start_client plain "$CACHE_BACKEND_PORT" APERIO_HOSTNAME_BIND=plain.e2e.local APERIO_CACHE=1
PLAIN_PID="${CLIENT_PIDS[${#CLIENT_PIDS[@]}-1]}"
wait_routable cache.e2e.local /data
wait_routable plain.e2e.local /data

# Warm both entries and confirm the second GET is a cache hit.
curl -sf -H "Host: cache.e2e.local" "$BASE/data" >/dev/null || fail "resilient warm-up failed"
HDRS="$(curl -s -D - -o /dev/null -H "Host: cache.e2e.local" "$BASE/data")"
assert_contains "$HDRS" "x-aperio-cache: hit" "second GET is served from the cache"

# Conditional GET: the cached entry carries a validator (synthesized when the
# backend sends none) and a matching If-None-Match is answered 304 edge-side.
ETAG="$(printf '%s' "$HDRS" | tr -d '\r' | awk 'tolower($1)=="etag:"{print $2}')"
[ -n "$ETAG" ] || fail "cached response carries an ETag"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Host: cache.e2e.local" -H "If-None-Match: $ETAG" "$BASE/data")"
assert_status 304 "$CODE" "matching If-None-Match is answered 304 from the cache"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Host: cache.e2e.local" -H 'If-None-Match: "other"' "$BASE/data")"
assert_status 200 "$CODE" "non-matching If-None-Match still gets the full body"

curl -sf -H "Host: plain.e2e.local" "$BASE/data" >/dev/null || fail "plain warm-up failed"

# Non-resilient route: killing its client means 504 even with a cached entry.
kill "$PLAIN_PID" 2>/dev/null || true
sleep 1
CODE="$(curl -s -o /dev/null -w '%{http_code}' -H "Host: plain.e2e.local" "$BASE/data")"
assert_status 504 "$CODE" "non-resilient route fails closed while offline"

# Resilient route: after the entry expires, it is still served — marked stale.
kill "$RESILIENT_PID" 2>/dev/null || true
sleep 2
BODY_AND_HDRS="$(curl -s -D - -H "Host: cache.e2e.local" "$BASE/data")"
assert_contains "$BODY_AND_HDRS" "x-aperio-stale: true" "expired entry is marked stale during the outage"
assert_contains "$BODY_AND_HDRS" "cacheable /data" "stale body is the cached response"

# A reconnecting client takes over immediately: fresh answer, no stale marker.
start_client resilient2 "$CACHE_BACKEND_PORT" APERIO_HOSTNAME_BIND=cache.e2e.local APERIO_CACHE=1 APERIO_RESILIENCE=1
wait_routable cache.e2e.local /data
HDRS="$(curl -s -D - -o /dev/null -H "Host: cache.e2e.local" "$BASE/fresh-after-reconnect")"
if printf '%s' "$HDRS" | grep -qi "x-aperio-stale"; then
  fail "reconnected client must serve fresh responses, not stale ones"
fi
echo "  ok: reconnected client serves fresh responses again"

step "Single-flight coalescing on cache miss"
SF_BACKEND_PORT=18110
"$PYTHON" - "$SF_BACKEND_PORT" <<'PYEOF' >"$LOG_DIR/backend-singleflight.log" 2>&1 &
import http.server, sys, threading, time

hits = {"n": 0}
lock = threading.Lock()

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/count':
            data = str(hits["n"]).encode()
            self.send_response(200)
            self.send_header('Cache-Control', 'no-store')
        else:
            with lock:
                hits["n"] += 1
            time.sleep(1)
            data = f'slow {self.path}'.encode()
            self.send_response(200)
            self.send_header('Cache-Control', 'max-age=60')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass

http.server.ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), Handler).serve_forever()
PYEOF
BACKEND_PIDS+=($!)
retry 15 curl -sf "http://127.0.0.1:${SF_BACKEND_PORT}/count" || fail "single-flight backend did not come up"
start_client singleflight "$SF_BACKEND_PORT" APERIO_HOSTNAME_BIND=sf.e2e.local APERIO_CACHE=1
wait_routable sf.e2e.local /count
# Five concurrent identical cacheable misses must reach the backend once.
for _ in 1 2 3 4 5; do
  curl -s -o /dev/null -H "Host: sf.e2e.local" "$BASE/coalesce-me" &
done
wait
COUNT="$(curl -s "http://127.0.0.1:${SF_BACKEND_PORT}/count")"
[ "$COUNT" = "1" ] || fail "expected 1 backend fetch for 5 concurrent identical misses, got $COUNT"
echo "  ok: 5 concurrent identical misses collapsed into 1 upstream fetch"
HDRS="$(curl -s -D - -o /dev/null -H "Host: sf.e2e.local" "$BASE/coalesce-me")"
assert_contains "$HDRS" "x-aperio-cache: hit" "followers left a warm cache behind"

step "stale-while-revalidate"
SWR_BACKEND_PORT=18111
"$PYTHON" - "$SWR_BACKEND_PORT" <<'PYEOF' >"$LOG_DIR/backend-swr.log" 2>&1 &
import http.server, sys, threading

hits = {"n": 0}
lock = threading.Lock()

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/count':
            data = str(hits["n"]).encode()
            self.send_response(200)
            self.send_header('Cache-Control', 'no-store')
        else:
            with lock:
                hits["n"] += 1
            data = f'swr v{hits["n"]}'.encode()
            self.send_response(200)
            self.send_header('Cache-Control', 'max-age=1, stale-while-revalidate=60')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass

http.server.ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), Handler).serve_forever()
PYEOF
BACKEND_PIDS+=($!)
retry 15 curl -sf "http://127.0.0.1:${SWR_BACKEND_PORT}/count" || fail "SWR backend did not come up"
start_client swr "$SWR_BACKEND_PORT" APERIO_HOSTNAME_BIND=swr.e2e.local APERIO_CACHE=1
wait_routable swr.e2e.local /count
curl -sf -H 'Host: swr.e2e.local' "$BASE/page" >/dev/null || fail "SWR warm-up failed"
sleep 2  # past max-age=1, inside the 60s SWR window
SWR_HDRS="$(curl -s -D - -H 'Host: swr.e2e.local' "$BASE/page")"
assert_contains "$SWR_HDRS" "x-aperio-cache: hit" "the expired entry is still answered from cache"
assert_contains "$SWR_HDRS" "x-aperio-stale: true" "the SWR answer is marked stale"
assert_contains "$SWR_HDRS" "swr v1" "the stale body is the previously cached response"
# The background revalidation refreshed the entry: a later hit is fresh v2.
retry 10 sh -c "curl -s -H 'Host: swr.e2e.local' '$BASE/page' | grep -q 'swr v2'" \
  || fail "the background revalidation did not refresh the entry"
FRESH_HDRS="$(curl -s -D - -o /dev/null -H 'Host: swr.e2e.local' "$BASE/page")"
if printf '%s' "$FRESH_HDRS" | grep -qi "x-aperio-stale"; then
  fail "the revalidated entry must serve fresh, not stale"
fi
echo "  ok: the stale window served instantly while the entry refreshed in the background"

step "Range requests served from cache"
# Fresh entry for the range test (SWR backend serves v3 now — re-warm).
curl -sf -H 'Host: swr.e2e.local' "$BASE/rangefile" >/dev/null || fail "range warm-up failed"
RANGE_RESP="$(curl -s -D - -r 0-3 -H 'Host: swr.e2e.local' "$BASE/rangefile")"
assert_contains "$RANGE_RESP" "206" "a ranged GET on a cached entry answers 206"
assert_contains "$RANGE_RESP" "x-aperio-cache: hit" "the partial answer comes from the cache"
assert_contains "$RANGE_RESP" "content-range: bytes 0-3/" "the 206 carries Content-Range"
CODE="$(curl -s -o /dev/null -w '%{http_code}' -r 9999- -H 'Host: swr.e2e.local' "$BASE/rangefile")"
assert_status 416 "$CODE" "an out-of-range request answers 416"

step "Cache purge API"
COOKIES_CACHE="$LOG_DIR/cookies-cache.txt"
dashboard_login "$COOKIES_CACHE"
PURGED="$(curl -sf -b "$COOKIES_CACHE" -X POST -H 'Content-Type: application/json' \
  --data '{"hostname":"swr.e2e.local"}' "$BASE/aperio/api/cache/purge")" \
  || fail "cache purge failed"
assert_contains "$PURGED" '"status":"ok"' "the purge reports its removal count"
MISS_HDRS="$(curl -s -D - -o /dev/null -H 'Host: swr.e2e.local' "$BASE/page")"
if printf '%s' "$MISS_HDRS" | grep -qi "x-aperio-cache: hit"; then
  fail "a purged entry must not answer from cache"
fi
echo "  ok: the purged entry is re-fetched from the backend"

stop_server
