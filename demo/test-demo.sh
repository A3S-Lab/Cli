#!/usr/bin/env bash
# test-demo.sh — build, start, and integration-test the a3s demo stack.
#
# Usage:
#   ./demo/test-demo.sh            # build debug + run all tests
#   ./demo/test-demo.sh --release  # build release binary
#
# Prerequisites: cargo, python3, a3s-gateway on PATH
set -euo pipefail

# ── paths ──────────────────────────────────────────────────────────────────
FILE="$(cd "$(dirname "$0")" && pwd)/A3sfile.hcl"
DIR="$(dirname "$FILE")"
CRATE_DIR="$(dirname "$DIR")"

STORE_PORT=6380
API_PORT=8001
WORKER_PORT=8002
WEB_PORT=3000
GW_PORT=8080

STORE_KEY="demo-store-secret"

PASS=0; FAIL=0
RELEASE=0

# ── helpers ────────────────────────────────────────────────────────────────
green()  { printf '\033[32m%s\033[0m\n' "$*"; }
red()    { printf '\033[31m%s\033[0m\n' "$*"; }
yellow() { printf '\033[33m%s\033[0m\n' "$*"; }

ok()   { green  "  ✓ $*"; PASS=$((PASS+1)); }
fail() { red    "  ✗ $*"; FAIL=$((FAIL+1)); }
info() { yellow "  · $*"; }

assert_contains() {
    local desc="$1" haystack="$2" needle="$3"
    if echo "$haystack" | grep -q "$needle"; then ok "$desc"; else fail "$desc (missing '$needle' in: $haystack)"; fi
}

assert_empty() {
    local desc="$1" val="$2"
    if [ -z "$val" ]; then ok "$desc"; else fail "$desc (expected empty, got: $val)"; fi
}

_curl() { curl -sf --max-time 3 "$@" 2>/dev/null || true; }

wait_http() {
    local url="$1" label="$2" tries=0
    while [ $tries -lt 30 ]; do
        if _curl "$url" >/dev/null; then return 0; fi
        tries=$((tries+1)); sleep 0.5
    done
    fail "timeout waiting for $label ($url)"; return 1
}

cleanup() {
    info "cleaning up..."
    "$A3S" -f "$FILE" down 2>/dev/null || true
    rm -rf "$DIR/logs"
}

# ── parse args ─────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --release) RELEASE=1; shift ;;
        *) echo "unknown arg: $1"; exit 1 ;;
    esac
done

echo ""
yellow "═══════════════════════════════════════════════════"
yellow "  a3s demo integration test"
yellow "═══════════════════════════════════════════════════"
echo ""

# ── 0. build ───────────────────────────────────────────────────────────────
yellow "── 0. build ─────────────────────────────────────────"
command -v cargo    >/dev/null || { red "cargo not found — install Rust"; exit 1; }
command -v python3  >/dev/null || { red "python3 not found";              exit 1; }
command -v a3s-gateway >/dev/null || { red "a3s-gateway not found on PATH"; exit 1; }

if [ "$RELEASE" -eq 1 ]; then
    info "cargo build --release"
    cargo build --release --manifest-path "$CRATE_DIR/Cargo.toml" 2>&1 \
        | sed 's/^/    /'
    A3S="$CRATE_DIR/target/release/a3s"
else
    info "cargo build"
    cargo build --manifest-path "$CRATE_DIR/Cargo.toml" 2>&1 \
        | sed 's/^/    /'
    A3S="$CRATE_DIR/target/debug/a3s"
fi
ok "built: $A3S"

trap cleanup EXIT

mkdir -p "$DIR/logs"

# ── 1. validate ────────────────────────────────────────────────────────────
echo ""
yellow "── 1. validate ──────────────────────────────────────"
out=$("$A3S" -f "$FILE" validate 2>&1)
if echo "$out" | grep -qi "error\|invalid"; then
    fail "validate: $out"
else
    ok "a3s validate passed"
fi

# ── 2. start services ─────────────────────────────────────────────────────
echo ""
yellow "── 2. a3s up --detach ───────────────────────────────"
"$A3S" -f "$FILE" up --detach --no-ui
ok "daemon started"

# ── 3. wait for all services ──────────────────────────────────────────────
echo ""
yellow "── 3. waiting for services to become healthy ────────"
wait_http "http://localhost:$STORE_PORT/health"      "store"   && ok "store  :$STORE_PORT"
wait_http "http://localhost:$API_PORT/health"        "api"     && ok "api    :$API_PORT"
wait_http "http://localhost:$WORKER_PORT/health"     "worker"  && ok "worker :$WORKER_PORT"
wait_http "http://localhost:$WEB_PORT/health"        "web"     && ok "web    :$WEB_PORT"
wait_http "http://localhost:$GW_PORT/api/gateway/health" "gateway" && ok "gateway:$GW_PORT"

# ── 4. a3s status ─────────────────────────────────────────────────────────
echo ""
yellow "── 4. a3s status ────────────────────────────────────"
status_json=$("$A3S" -f "$FILE" status --json 2>/dev/null)
for svc in store api worker web gateway; do
    assert_contains "status: $svc present" "$status_json" "\"$svc\""
done

# ── 5. direct service tests ───────────────────────────────────────────────
echo ""
yellow "── 5. direct service endpoints ──────────────────────"

# store
_curl -X POST "http://localhost:$STORE_PORT/set" \
    -H "Content-Type: application/json" -d '{"key":"direct","value":"ok"}' >/dev/null
get=$(_curl "http://localhost:$STORE_PORT/get?key=direct")
assert_contains "store direct SET/GET" "$get" '"ok"'
_curl -X DELETE "http://localhost:$STORE_PORT/del?key=direct" >/dev/null

# api: create two items directly
id1=$(_curl -X POST "http://localhost:$API_PORT/items" \
    -H "Content-Type: application/json" -d '{"name":"apple","value":"red"}' \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
ok "api POST /items apple id=$id1"

id2=$(_curl -X POST "http://localhost:$API_PORT/items" \
    -H "Content-Type: application/json" -d '{"name":"banana","value":"yellow"}' \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
ok "api POST /items banana id=$id2"

items=$(_curl "http://localhost:$API_PORT/items")
assert_contains "api GET /items has apple"  "$items" '"apple"'
assert_contains "api GET /items has banana" "$items" '"banana"'

# ── 6. gateway routing tests ──────────────────────────────────────────────
echo ""
yellow "── 6. gateway routing ───────────────────────────────"

gw="http://localhost:$GW_PORT"

# 6a. gateway health (dashboard)
gw_health=$(_curl "$gw/api/gateway/health")
assert_contains "gateway /api/gateway/health" "$gw_health" '"'

# 6b. / → web frontend
html=$(_curl "$gw/")
assert_contains "gateway / → web HTML"     "$html" "a3s demo"
assert_contains "gateway / → has JS fetch" "$html" "fetch"

# 6c. /api → api (strip-prefix + rate-limit + cors)
gw_items=$(_curl "$gw/api/items")
assert_contains "gateway /api/items → api"  "$gw_items" '"items"'
assert_contains "gateway /api/items has apple"  "$gw_items" '"apple"'
assert_contains "gateway /api/items has banana" "$gw_items" '"banana"'

# create via gateway
gw_id=$(_curl -X POST "$gw/api/items" \
    -H "Content-Type: application/json" -d '{"name":"cherry","value":"dark-red"}' \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
ok "gateway POST /api/items cherry id=$gw_id"

gw_item=$(_curl "$gw/api/items/$gw_id")
assert_contains "gateway GET /api/items/$gw_id" "$gw_item" '"cherry"'

_curl -X DELETE "$gw/api/items/$gw_id" >/dev/null
del_check=$(_curl "$gw/api/items/$gw_id" || true)
assert_contains "gateway DELETE /api/items/$gw_id" "$del_check" '"error"'

# 6d. /worker → worker (strip-prefix)
wstatus=$(_curl "$gw/worker/status")
assert_contains "gateway /worker/status → worker" "$wstatus" '"beats"'
assert_contains "gateway /worker/status interval"  "$wstatus" '"interval"'

# 6e. /store → store (strip-prefix + api-key required)
# Without key → should be rejected (401 or 403)
no_key_resp=$(curl -s -o /dev/null -w "%{http_code}" --max-time 3 \
    "$gw/store/health" 2>/dev/null || true)
if [ "$no_key_resp" = "401" ] || [ "$no_key_resp" = "403" ]; then
    ok "gateway /store rejects missing api-key ($no_key_resp)"
else
    fail "gateway /store should reject missing api-key (got $no_key_resp)"
fi

# With correct key → should succeed
store_health=$(_curl -H "X-Store-Key: $STORE_KEY" "$gw/store/health")
assert_contains "gateway /store/health with api-key" "$store_health" '"ok"'

_curl -X POST -H "X-Store-Key: $STORE_KEY" \
    -H "Content-Type: application/json" \
    -d '{"key":"gw-test","value":"routed"}' \
    "$gw/store/set" >/dev/null
gw_get=$(_curl -H "X-Store-Key: $STORE_KEY" "$gw/store/get?key=gw-test")
assert_contains "gateway /store SET→GET via api-key" "$gw_get" '"routed"'
_curl -X DELETE -H "X-Store-Key: $STORE_KEY" "$gw/store/del?key=gw-test" >/dev/null

# 6f. CORS headers present on /api route
cors_header=$(curl -sf --max-time 3 -I "$gw/api/items" 2>/dev/null \
    | grep -i "access-control-allow-origin" || true)
if [ -n "$cors_header" ]; then
    ok "gateway /api has CORS header"
else
    info "CORS header not observed (may depend on gateway version)"
fi

# ── 7. worker heartbeat in store ──────────────────────────────────────────
echo ""
yellow "── 7. worker heartbeat ──────────────────────────────"
info "waiting 4s for heartbeat..."
sleep 4

beats=$(_curl "$gw/worker/status" \
    | python3 -c "import json,sys; print(json.load(sys.stdin).get('beats',0))")
if [ "$beats" -gt 0 ]; then
    ok "worker beat_count=$beats via gateway"
else
    fail "worker beat_count=0 after 4s"
fi

beat_val=$(_curl -H "X-Store-Key: $STORE_KEY" \
    "$gw/store/get?key=worker%3Alast_beat")
assert_contains "heartbeat written to store (via gateway)" "$beat_val" '"value"'

# ── 8. a3s restart gateway ────────────────────────────────────────────────
echo ""
yellow "── 8. a3s restart gateway ───────────────────────────"
"$A3S" -f "$FILE" restart gateway
info "waiting for gateway to come back..."
sleep 3
wait_http "$gw/api/gateway/health" "gateway after restart" && ok "gateway healthy after restart"

gw_items2=$(_curl "$gw/api/items")
assert_contains "gateway /api/items still works after restart" "$gw_items2" '"items"'

# ── 9. log files ──────────────────────────────────────────────────────────
echo ""
yellow "── 9. log files ─────────────────────────────────────"
for svc in store api worker web gateway; do
    logfile="$DIR/logs/$svc.log"
    if [ -f "$logfile" ] && [ -s "$logfile" ]; then
        ok "logs/$svc.log has content"
    else
        fail "logs/$svc.log missing or empty"
    fi
done

# ── 10. label filter down ─────────────────────────────────────────────────
echo ""
yellow "── 10. a3s down --label backend ─────────────────────"
"$A3S" -f "$FILE" down --label backend
sleep 2

# api + worker should be down; gateway will lose backends but still answer
api_gone=$(_curl "http://localhost:$API_PORT/health" || true)
assert_empty "api stopped (label=backend)" "$api_gone"

worker_gone=$(_curl "http://localhost:$WORKER_PORT/health" || true)
assert_empty "worker stopped (label=backend)" "$worker_gone"

store_still=$(_curl "http://localhost:$STORE_PORT/health")
assert_contains "store still running" "$store_still" '"ok"'

# bring backend back
"$A3S" -f "$FILE" up --detach --no-ui api worker
sleep 3

# ── 11. env() function — default values ──────────────────────────────────
#
# A3sfile.hcl uses:
#   log_level  = env("LOG_LEVEL",       "info")
#   INTERVAL   = env("WORKER_INTERVAL", "3")
#   APP_ENV    = env("APP_ENV",         "development")
#   STORE_API_KEY = env("STORE_API_KEY","demo-store-secret")
#
# With none of those OS env vars set the services must use the defaults.
echo ""
yellow "── 11. env() defaults (no OS env vars set) ──────────"

w_status=$(_curl "http://localhost:$WORKER_PORT/status")

w_interval=$(echo "$w_status" | python3 -c "import json,sys; print(json.load(sys.stdin).get('interval',''))")
if [ "$w_interval" = "3" ]; then
    ok "env(WORKER_INTERVAL) default=3"
else
    fail "env(WORKER_INTERVAL) expected default 3, got '$w_interval'"
fi

w_appenv=$(echo "$w_status" | python3 -c "import json,sys; print(json.load(sys.stdin).get('app_env',''))")
if [ "$w_appenv" = "development" ]; then
    ok "env(APP_ENV) default=development"
else
    fail "env(APP_ENV) expected default 'development', got '$w_appenv'"
fi

# STORE_API_KEY default: the gateway should still accept the default key
store_gw=$(_curl -H "X-Store-Key: $STORE_KEY" "$gw/store/health")
assert_contains "env(STORE_API_KEY) default key accepted by gateway" "$store_gw" '"ok"'

# ── 12. env() function — OS env var overrides ─────────────────────────────
echo ""
yellow "── 12. env() overrides (OS env vars set) ────────────"

# Stop everything, then restart with overriding env vars.
"$A3S" -f "$FILE" down
sleep 2

CUSTOM_KEY="my-custom-store-key"

info "restarting with WORKER_INTERVAL=2 APP_ENV=staging STORE_API_KEY=$CUSTOM_KEY"
WORKER_INTERVAL=2 APP_ENV=staging STORE_API_KEY="$CUSTOM_KEY" \
    "$A3S" -f "$FILE" up --detach --no-ui

wait_http "http://localhost:$WORKER_PORT/health" "worker (override run)"

w_status2=$(_curl "http://localhost:$WORKER_PORT/status")

w_interval2=$(echo "$w_status2" | python3 -c "import json,sys; print(json.load(sys.stdin).get('interval',''))")
if [ "$w_interval2" = "2" ]; then
    ok "env(WORKER_INTERVAL) override=2"
else
    fail "env(WORKER_INTERVAL) expected override 2, got '$w_interval2'"
fi

w_appenv2=$(echo "$w_status2" | python3 -c "import json,sys; print(json.load(sys.stdin).get('app_env',''))")
if [ "$w_appenv2" = "staging" ]; then
    ok "env(APP_ENV) override=staging"
else
    fail "env(APP_ENV) expected override 'staging', got '$w_appenv2'"
fi

# Verify the custom STORE_API_KEY is active in gateway
wait_http "$gw/api/gateway/health" "gateway (override run)"

# old default key must now be rejected
old_key_resp=$(curl -s -o /dev/null -w "%{http_code}" --max-time 3 \
    -H "X-Store-Key: $STORE_KEY" "$gw/store/health" 2>/dev/null || true)
if [ "$old_key_resp" = "401" ] || [ "$old_key_resp" = "403" ]; then
    ok "env(STORE_API_KEY) old default key rejected after override ($old_key_resp)"
else
    fail "env(STORE_API_KEY) old default key should be rejected (got $old_key_resp)"
fi

# new custom key must be accepted
new_key_resp=$(_curl -H "X-Store-Key: $CUSTOM_KEY" "$gw/store/health")
assert_contains "env(STORE_API_KEY) new custom key accepted" "$new_key_resp" '"ok"'

# restore STORE_KEY for cleanup helpers
STORE_KEY="$CUSTOM_KEY"

# ── 13. full down + port checks ───────────────────────────────────────────
echo ""
yellow "── 13. a3s down (all) ───────────────────────────────"
"$A3S" -f "$FILE" down
sleep 2

for port in $STORE_PORT $API_PORT $WORKER_PORT $WEB_PORT $GW_PORT; do
    check=$(_curl "http://localhost:$port/" || true)
    assert_empty "port $port down" "$check"
done

# ── summary ───────────────────────────────────────────────────────────────
echo ""
yellow "═══════════════════════════════════════════════════"
if [ "$FAIL" -eq 0 ]; then
    green "  ALL $PASS tests passed"
else
    red   "  $PASS passed, $FAIL FAILED"
fi
yellow "═══════════════════════════════════════════════════"
echo ""

[ "$FAIL" -eq 0 ]
