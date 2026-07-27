#!/usr/bin/env bash
# Phase N: config. Sourced by tests/e2e.sh after the harness.
PHASE="config"

step "aperio-server.yaml hot-reload"
CFG="$LOG_DIR/aperio-server.yaml"
# The initial file uses the grouped form of both settings; the reloaded one
# below uses the flat spelling, so this step covers each of them reaching the
# running server.
cat > "$CFG" <<YAML
cache:
  enabled: false
login_lockout:
  threshold: 5
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
[ "$(reload_setting cache_enabled)" = "False" ] || fail "a grouped cache.enabled reaches the server"
[ "$(reload_setting login_lockout_threshold)" = "5" ] || fail "a grouped login_lockout.threshold reaches the server"
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
[ "$(reload_setting login_lockout_threshold)" = "9" ] || fail "the flat spelling still reloads to 9"
BODY="$(curl -s -H "Host: probe.e2e.local" "$BASE/reload-probe")"
assert_contains "$BODY" "v2-reloaded" "the structured route reloaded to its new body"
# The port change is structural: the server stays on its original port.
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/aperio/health")"
assert_status 200 "$CODE" "a structural port change is ignored live (no restart)"

step "Edge endpoints without an edge token"
# This phase's server runs without APERIO_EDGE_TOKEN. The endpoints must still
# own their paths and say the feature is off: when they were registered only
# with the token, the request fell through to the visitor proxy and came back
# as a 504 "no client connected", which reads as a tunnel fault.
for EDGE_PATH in "edge/traefik" "edge/ask?domain=probe.e2e.local"; do
  BODY="$(curl -s -w '\n%{http_code}' "$BASE/aperio/api/${EDGE_PATH}")"
  assert_status 404 "$(echo "$BODY" | tail -1)" "${EDGE_PATH%%\?*} answers 404 while the feature is off"
  assert_contains "$BODY" "edge integration is not enabled" \
    "${EDGE_PATH%%\?*} says why, instead of a gateway error"
done

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

step "Config version declaration (upgrade safety)"
VER_CFG="$LOG_DIR/version.yaml"
CURRENT_VERSION="$("$SERVER_BIN" --version | awk '{print $NF}')"
# A file declaring this build's own version is, by definition, current: the
# lint says so and nothing is warned about.
cat > "$VER_CFG" <<YAML
version: $CURRENT_VERSION
server_token: e2e-version-token-long-enough
YAML
VER_OUT="$(env APERIO_SERVER_CONFIG="$VER_CFG" "$SERVER_BIN" --check-config)" \
  || fail "--check-config rejected a config declaring the current version: $VER_OUT"
assert_contains "$VER_OUT" "matches this build" "a current version: is reported as up to date"

# An old declaration is accepted too: no config-format change is recorded yet,
# so the upgrade is silent rather than noisy. This is the guarantee that a
# clean upgrade stays quiet.
cat > "$VER_CFG" <<YAML
version: 0.1.0
server_token: e2e-version-token-long-enough
YAML
VER_OLD="$(env APERIO_SERVER_CONFIG="$VER_CFG" "$SERVER_BIN" --check-config)" \
  || fail "--check-config rejected an older version declaration: $VER_OLD"

# A misspelled version is an error, not a silent skip: a typo must never look
# like a clean upgrade.
cat > "$VER_CFG" <<YAML
version: not-a-version
server_token: e2e-version-token-long-enough
YAML
if VER_BAD="$(env APERIO_SERVER_CONFIG="$VER_CFG" "$SERVER_BIN" --check-config 2>&1)"; then
  fail "--check-config should reject an unparseable version:"
fi
assert_contains "$VER_BAD" "not a version" "a malformed version: is reported"

# Omitting it keeps the old behaviour, with a note that the check is off.
cat > "$VER_CFG" <<YAML
server_token: e2e-version-token-long-enough
YAML
VER_NONE="$(env APERIO_SERVER_CONFIG="$VER_CFG" "$SERVER_BIN" --check-config)" \
  || fail "--check-config rejected a config without version:: $VER_NONE"
assert_contains "$VER_NONE" "no \`version:\` declared" "an absent version: is noted, not fatal"
