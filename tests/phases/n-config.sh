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

step "aperio-server --print-schema"
SCHEMA="$("$SERVER_BIN" --print-schema)"
assert_contains "$SCHEMA" '"ServerFileConfig"' "--print-schema emits the server file-config schema"
echo "$SCHEMA" | "$PYTHON" -c "import sys,json; json.load(sys.stdin)" \
  || fail "--print-schema output is not valid JSON"
echo "  ok: --print-schema emits valid JSON schema"

step "aperio-server --print-config"
PCFG="$LOG_DIR/aperio-print.yaml"
cat > "$PCFG" <<YAML
max_body_size: 4242
trusted_proxies: [10.0.0.0/8]
headers:
  request:
    add:
      X-A: b
YAML
PC_OUT="$(APERIO_SERVER_CONFIG="$PCFG" APERIO_SERVER_TOKEN="print-secret-token" \
  APERIO_DATA_DIR="$LOG_DIR/print-data" "$SERVER_BIN" --print-config)"
assert_contains "$PC_OUT" "APERIO_MAX_BODY_SIZE" "--print-config lists a file-set variable"
assert_contains "$PC_OUT" "[aperio-server.yaml]" "--print-config attributes it to the file"
assert_contains "$PC_OUT" "Structured aperio-server.yaml sections: headers" \
  "--print-config lists structured sections"
case "$PC_OUT" in
  *print-secret-token*) fail "--print-config leaked the master token" ;;
  *) echo "  ok: --print-config masks the master token" ;;
esac

step "Server config lint (--check-config)"
LINT_CFG="$LOG_DIR/lint.yaml"
cat > "$LINT_CFG" <<YAML
server_token: e2e-lint-token-long-enough
lb_strategy: sticky
YAML
LINT_OUT="$(env APERIO_SERVER_CONFIG="$LINT_CFG" "$SERVER_BIN" --check-config)" \
  || fail "--check-config exited non-zero on a valid config: $LINT_OUT"
assert_contains "$LINT_OUT" "Configuration OK" "a valid config passes the lint"
cat > "$LINT_CFG" <<YAML
server_token: e2e-lint-token-long-enough
lb_strategy: bogus
max_body_size: not-a-number
YAML
if LINT_BAD="$(env APERIO_SERVER_CONFIG="$LINT_CFG" "$SERVER_BIN" --check-config 2>&1)"; then
  fail "--check-config should exit 1 on an invalid config"
fi
assert_contains "$LINT_BAD" "FAIL" "invalid values are reported as failures"
assert_contains "$LINT_BAD" "Configuration check FAILED" "the lint summarizes the errors"
