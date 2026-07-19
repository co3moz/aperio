#!/usr/bin/env bash
# Phase N: config. Sourced by tests/e2e.sh after the harness.
PHASE="config"

step "aperio-server.yaml hot-reload"
CFG="$LOG_DIR/aperio-server.yaml"
cat > "$CFG" <<YAML
cache: false
login_lockout_threshold: 5
routes:
  - path: /reload-probe
    respond:
      status: 200
      body: "v1"
YAML
start_server APERIO_SERVER_CONFIG="$CFG"
RJAR="$LOG_DIR/cookies-reload.txt"
dashboard_login "$RJAR"

reload_setting() { # <json-key>
  curl -s -b "$RJAR" "$BASE/aperio/api/settings" \
    | "$PYTHON" -c "import sys,json; print(json.load(sys.stdin)['effective']['$1'])"
}

# Initial file values are in effect.
[ "$(reload_setting cache_enabled)" = "False" ] || fail "initial cache_enabled should be false"
[ "$(reload_setting login_lockout_threshold)" = "5" ] || fail "initial lockout threshold should be 5"
BODY="$(curl -s -H "Host: probe.e2e.local" "$BASE/reload-probe")"
assert_contains "$BODY" "v1" "the client-less route serves its initial body"

# Edit the file: a live setting, a structured route, and a structural key
# (port) that must NOT take effect live.
cat > "$CFG" <<YAML
cache: true
login_lockout_threshold: 9
port: 9999
routes:
  - path: /reload-probe
    respond:
      status: 200
      body: "v2-reloaded"
YAML
APPLIED=""
for _ in $(seq 1 10); do
  if [ "$(reload_setting cache_enabled)" = "True" ]; then APPLIED=1; break; fi
  sleep 1
done
[ -n "$APPLIED" ] || fail "the edited config was not hot-reloaded within 10s"
echo "  ok: a live setting is re-applied on file change"
[ "$(reload_setting login_lockout_threshold)" = "9" ] || fail "lockout threshold did not reload to 9"
BODY="$(curl -s -H "Host: probe.e2e.local" "$BASE/reload-probe")"
assert_contains "$BODY" "v2-reloaded" "the structured route reloaded to its new body"
# The port change is structural: the server stays on its original port.
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/aperio/health")"
assert_status 200 "$CODE" "a structural port change is ignored live (no restart)"

step "Per-hostname custom error pages (error_pages:)"
ERR_PAGE="$LOG_DIR/custom-504.html"
echo "<h1>custom err.e2e.local 504</h1>" > "$ERR_PAGE"
cat > "$CFG" <<YAML
cache: true
error_pages:
  - hostname: err.e2e.local
    504_page: ${ERR_PAGE}
YAML
EP_APPLIED=""
for _ in $(seq 1 10); do
  BODY="$(curl -s -m 10 -H 'Host: err.e2e.local' "$BASE/nothing")"
  case "$BODY" in
    *"custom err.e2e.local 504"*) EP_APPLIED=1; break ;;
  esac
  sleep 1
done
[ -n "$EP_APPLIED" ] || fail "the per-hostname 504 page was not served after reload"
echo "  ok: the hostname's own 504 page is served"
BODY="$(curl -s -m 10 -H 'Host: other.e2e.local' "$BASE/nothing")"
assert_contains "$BODY" "504 Gateway Timeout" "other hostnames keep the default 504 text"

stop_server
