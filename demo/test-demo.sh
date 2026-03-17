#!/usr/bin/env bash
# test-demo.sh — integration test for `a3s up` with the demo services.
#
# Usage:
#   cd demo/
#   bash test-demo.sh [--binary /path/to/a3s]
#
# Prerequisites: python3, a3s binary on PATH (or pass --binary)
set -euo pipefail

# ── config ─────────────────────────────────────────────────────────────────
A3S="${A3S_BIN:-a3s}"
FILE="$(cd "$(dirname "$0")" && pwd)/A3sfile.hcl"
DIR="$(dirname "$FILE")"

STORE_PORT=6380
API_PORT=8001
WORKER_PORT=8002
WEB_PORT=3000

PASS=0; FAIL=0

# ── helpers ────────────────────────────────────────────────────────────────
green()  { printf '\033[32m%s\033[0m\n' "$*"; }
red()    { printf '\033[31m%s\033[0m\n' "$*"; }
yellow() { printf '\033[33m%s\033[0m\n' "$*"; }

ok()   { green  "  ✓ $*"; PASS=$((PASS+1)); }
fail() { red    "  ✗ $*"; FAIL=$((FAIL+1)); }
info() { yellow "  · $*"; }

assert_eq() {
    local desc="$1" got="$2" want="$3"
    if [ "$got" = "$want" ]; then ok "$desc"; else fail "$desc (got='$got' want='$want')"; fi
}

assert_contains() {
    local desc="$1" haystack="$2" needle="$3"
    if echo "$haystack" | grep -q "$needle"; then ok "$desc"; else fail "$desc (missing '$needle')"; fi
}

# curl wrapper — returns empty string on error instead of failing
_curl() { curl -sf --max-time 3 "$@" 2>/dev/null || true; }

wait_http() {
    local url="$1" label="$2" tries=0
    while [ $tries -lt 20 ]; do
        if _curl "$url" >/dev/null 2>&1; then return 0; fi
        tries=$((tries+1)); sleep 0.5
    done
    fail "timeout waiting for $label ($url)"
    return 1
}

cleanup() {
    info "cleaning up..."
    "$A3S" -f "$FILE" down 2>/dev/null || true
    rm -rf "$DIR/logs"
}
trap cleanup EXIT

# ── parse args ─────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary) A3S="$2"; shift 2 ;;
        *) echo "unknown arg: $1"; exit 1 ;;
    esac
done

echo ""
yellow "═══════════════════════════════════════════════════"
yellow "  a3s demo integration test"
yellow "═══════════════════════════════════════════════════"
echo ""

# ── 0. prerequisites ───────────────────────────────────────────────────────
info "checking prerequisites"
if ! command -v python3 &>/dev/null; then fail "python3 not found"; exit 1; fi
if ! command -v "$A3S"  &>/dev/null; then fail "a3s binary not found (set A3S_BIN or --binary)"; exit 1; fi
ok "python3 and a3s found"

mkdir -p "$DIR/logs"

# ── 1. validate ────────────────────────────────────────────────────────────
echo ""
yellow "── 1. validate ──────────────────────────────────────"
out=$("$A3S" -f "$FILE" validate 2>&1)
if echo "$out" | grep -qi "error\|invalid"; then
    fail "validate reported errors: $out"
else
    ok "a3s validate passed"
fi

# ── 2. start services ─────────────────────────────────────────────────────
echo ""
yellow "── 2. a3s up --detach ───────────────────────────────"
"$A3S" -f "$FILE" up --detach --no-ui
ok "daemon started"

# ── 3. wait for each service ──────────────────────────────────────────────
echo ""
yellow "── 3. waiting for services to become healthy ────────"
(cd "$DIR" && wait_http "http://localhost:$STORE_PORT/health"  "store")  && ok "store reachable"
(cd "$DIR" && wait_http "http://localhost:$API_PORT/health"    "api")    && ok "api reachable"
(cd "$DIR" && wait_http "http://localhost:$WORKER_PORT/health" "worker") && ok "worker reachable"
(cd "$DIR" && wait_http "http://localhost:$WEB_PORT/health"    "web")    && ok "web reachable"

# ── 4. a3s status ─────────────────────────────────────────────────────────
echo ""
yellow "── 4. a3s status ────────────────────────────────────"
status_json=$("$A3S" -f "$FILE" status --json 2>/dev/null)
for svc in store api worker web; do
    assert_contains "status: $svc present" "$status_json" "\"$svc\""
done

# ── 5. store endpoints ────────────────────────────────────────────────────
echo ""
yellow "── 5. store endpoints ───────────────────────────────"

health=$(_curl "http://localhost:$STORE_PORT/health")
assert_contains "store /health ok"   "$health" '"ok"'

_curl -X POST "http://localhost:$STORE_PORT/set" \
    -H "Content-Type: application/json" \
    -d '{"key":"demo","value":"hello"}' >/dev/null
ok "store SET demo=hello"

get=$(_curl "http://localhost:$STORE_PORT/get?key=demo")
assert_contains "store GET demo" "$get" '"hello"'

keys=$(_curl "http://localhost:$STORE_PORT/keys")
assert_contains "store /keys lists demo" "$keys" '"demo"'

_curl -X DELETE "http://localhost:$STORE_PORT/del?key=demo" >/dev/null
get2=$(_curl "http://localhost:$STORE_PORT/get?key=demo" || true)
assert_contains "store DELETE demo" "$get2" '"error"'

# ── 6. api endpoints ──────────────────────────────────────────────────────
echo ""
yellow "── 6. api endpoints ─────────────────────────────────"

api_health=$(_curl "http://localhost:$API_PORT/health")
assert_contains "api /health ok"    "$api_health" '"ok"'
assert_contains "api /health store" "$api_health" '"store"'

# create items
id1=$(  _curl -X POST "http://localhost:$API_PORT/items" \
            -H "Content-Type: application/json" \
            -d '{"name":"apple","value":"red"}' | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
ok "api POST /items → id=$id1"

id2=$(  _curl -X POST "http://localhost:$API_PORT/items" \
            -H "Content-Type: application/json" \
            -d '{"name":"banana","value":"yellow"}' | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
ok "api POST /items → id=$id2"

items=$(_curl "http://localhost:$API_PORT/items")
assert_contains "api GET /items has apple"  "$items" '"apple"'
assert_contains "api GET /items has banana" "$items" '"banana"'

item1=$(_curl "http://localhost:$API_PORT/items/$id1")
assert_contains "api GET /items/$id1" "$item1" '"apple"'

_curl -X DELETE "http://localhost:$API_PORT/items/$id1" >/dev/null
del_check=$(_curl "http://localhost:$API_PORT/items/$id1" || true)
assert_contains "api DELETE /items/$id1" "$del_check" '"error"'

# ── 7. worker endpoints ───────────────────────────────────────────────────
echo ""
yellow "── 7. worker endpoints ──────────────────────────────"

wstatus=$(_curl "http://localhost:$WORKER_PORT/status")
assert_contains "worker /status has beats"    "$wstatus" '"beats"'
assert_contains "worker /status has interval" "$wstatus" '"interval"'

info "waiting 4s for at least one heartbeat..."
sleep 4

wstatus2=$(_curl "http://localhost:$WORKER_PORT/status")
beats=$(echo "$wstatus2" | python3 -c "import json,sys; print(json.load(sys.stdin).get('beats',0))")
if [ "$beats" -gt 0 ]; then
    ok "worker beat count=$beats"
else
    fail "worker beat count is 0 after 4s"
fi

beat_in_store=$(_curl "http://localhost:$STORE_PORT/get?key=worker%3Alast_beat")
assert_contains "worker heartbeat in store" "$beat_in_store" '"value"'

# ── 8. web frontend ───────────────────────────────────────────────────────
echo ""
yellow "── 8. web frontend ──────────────────────────────────"

html=$(_curl "http://localhost:$WEB_PORT/")
assert_contains "web / returns HTML"     "$html" "a3s demo"
assert_contains "web / has fetch script" "$html" "fetch"

web_api=$(_curl "http://localhost:$WEB_PORT/api/items")
assert_contains "web /api/items proxied" "$web_api" '"items"'

# ── 9. restart ────────────────────────────────────────────────────────────
echo ""
yellow "── 9. a3s restart worker ────────────────────────────"
"$A3S" -f "$FILE" restart worker
info "waiting 3s for worker to come back..."
sleep 3
(cd "$DIR" && wait_http "http://localhost:$WORKER_PORT/health" "worker after restart") && ok "worker healthy after restart"

wstatus3=$(_curl "http://localhost:$WORKER_PORT/status")
assert_contains "worker /status ok after restart" "$wstatus3" '"beats"'

# ── 10. log files ─────────────────────────────────────────────────────────
echo ""
yellow "── 10. log files ────────────────────────────────────"
for svc in store api worker web; do
    logfile="$DIR/logs/$svc.log"
    if [ -f "$logfile" ] && [ -s "$logfile" ]; then
        ok "logs/$svc.log has content"
    else
        fail "logs/$svc.log missing or empty"
    fi
done

# ── 11. label filter ──────────────────────────────────────────────────────
echo ""
yellow "── 11. a3s down --label backend ─────────────────────"
"$A3S" -f "$FILE" down --label backend
sleep 2

# api and worker should be gone; store and web still up
api_check=$(_curl "http://localhost:$API_PORT/health" || true)
if [ -z "$api_check" ]; then ok "api stopped (label=backend)"; else fail "api still running"; fi

worker_check=$(_curl "http://localhost:$WORKER_PORT/health" || true)
if [ -z "$worker_check" ]; then ok "worker stopped (label=backend)"; else fail "worker still running"; fi

store_check=$(_curl "http://localhost:$STORE_PORT/health" || true)
assert_contains "store still running" "$store_check" '"ok"'

# bring backend back before final down
"$A3S" -f "$FILE" up --detach --no-ui api worker
sleep 3

# ── 12. a3s down (all) ────────────────────────────────────────────────────
echo ""
yellow "── 12. a3s down ─────────────────────────────────────"
"$A3S" -f "$FILE" down
sleep 2

for port in $STORE_PORT $API_PORT $WORKER_PORT $WEB_PORT; do
    check=$(_curl "http://localhost:$port/health" || true)
    if [ -z "$check" ]; then ok "port $port down"; else fail "port $port still responding"; fi
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
