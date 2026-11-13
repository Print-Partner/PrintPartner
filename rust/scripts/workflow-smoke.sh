#!/usr/bin/env bash
set -euo pipefail

usage='Usage: bash rust/scripts/workflow-smoke.sh SERVER_BINARY NODE_BINARY'
server="${1:?$usage}"
node="${2:?$usage}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
commit="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["commit"])' "$root/rust/bundle-manifest.json")"
fixture="$(mktemp -d "${TMPDIR:-/tmp}/pp-workflow-smoke.XXXXXX")"
data="$fixture/data"
credentials="$fixture/credentials.json"
server_pid=''
ws_pid=''

log() { printf '[smoke] %s\n' "$*"; }
fail() { printf '[smoke] FAIL: %s\n' "$*" >&2; exit 1; }
alive() { kill -0 "$1" 2>/dev/null; }
# pgrep status 1 means no match; every other nonzero status is a probe error.
check_no_leftovers() {
  local leftover probe_status
  if leftover="$(pgrep "$@")"; then
    probe_status=0
  else
    probe_status=$?
  fi
  case "$probe_status" in
    0) fail "leftover processes after stop (pgrep $*): $leftover" ;;
    1) return 0 ;;
    *) fail "leftover process probe failed (pgrep $*, exit $probe_status)" ;;
  esac
}
# Wait up to $2 seconds for pid $1 to exit; status 1 on timeout.
wait_exit() {
  local deadline=$((SECONDS + $2))
  while alive "$1"; do
    ((SECONDS < deadline)) || return 1
    sleep 0.2
  done
}

cleanup() {
  result=$?
  trap - EXIT
  if test -n "$ws_pid" && alive "$ws_pid"; then kill "$ws_pid" 2>/dev/null || true; fi
  if test -n "$server_pid" && alive "$server_pid"; then
    kill -TERM "$server_pid" 2>/dev/null || true
    wait_exit "$server_pid" 20 || kill -KILL "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
    result=1
  fi
  rm -f "$credentials"
  if ((result != 0)); then
    log "server stderr:"
    cat "$fixture/server.err" >&2 2>/dev/null || true
    cat "$fixture/ws.err" >&2 2>/dev/null || true
  fi
  log "fixture retained at $fixture"
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Prints "free" or "held" for the OS lock on the data directory; any other
# error (missing file, unexpected errno) makes the probe itself fail.
lock_state() {
  python3 - "$data/printpartner.lock" <<'PY'
import fcntl, sys
with open(sys.argv[1], 'rb') as lock:
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        print('held')
        sys.exit(0)
    fcntl.flock(lock, fcntl.LOCK_UN)
    print('free')
PY
}

log "boot: $server (commit $commit)"
"$server" --data "$data" --web "$root/web" --node "$node" --commit "$commit" \
  --test-credential-file "$credentials" >"$fixture/server.out" 2>"$fixture/server.err" &
server_pid=$!
SECONDS=0
until test -s "$fixture/server.out"; do
  alive "$server_pid" || fail 'server exited before readiness'
  ((SECONDS < 60)) || fail 'server readiness timed out'
  sleep 0.2
done
origin="$(head -n 1 "$fixture/server.out")"
case "$origin" in
  http://127.0.0.1:*) ;;
  *) fail "server did not print a loopback origin: $origin" ;;
esac
log "ready: $origin (pid $server_pid)"

# Data lock taken at boot.
test -f "$data/.desktop-lease" || fail 'compatibility reservation missing after boot'
runtime_dir="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["runtime_dir"])' "$credentials")"
lock="$(lock_state)" || fail 'data lock probe failed at boot'
test "$lock" = held || fail "data lock is $lock while running"
log 'lock: held at boot (flock EWOULDBLOCK, compatibility reservation present)'

# Authenticate with the one-use bootstrap credential.
test "$(python3 -c 'import os,sys; print(oct(os.stat(sys.argv[1]).st_mode & 0o777))' "$credentials")" = 0o600 \
  || fail 'credential file is not 0600'
bootstrap="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["bootstrap_url"])' "$credentials")"
rm -f "$credentials"
status="$(curl --silent --max-time 10 --output /dev/null --write-out '%{http_code}' "$origin/__runtime")"
test "$status" = 401 || fail "unauthenticated request returned $status, expected 401"
cookie="$(curl --silent --show-error --max-time 10 --dump-header - --output /dev/null "$bootstrap" \
  | tr -d '\r' | sed -n 's/^[Ss]et-[Cc]ookie: \([^;]*\).*/\1/p' | head -n 1)"
test -n "$cookie" || fail 'bootstrap did not set a session cookie'
status="$(curl --silent --max-time 10 --output /dev/null --write-out '%{http_code}' "$bootstrap")"
test "$status" = 401 || fail "bootstrap reuse returned $status, expected 401"

# Authenticated HTTP: runtime state from Rust, then a round trip into Node.
runtime_json="$(curl --fail --silent --show-error --max-time 10 -H "Cookie: $cookie" "$origin/__runtime")"
child_pid="$(printf '%s' "$runtime_json" | python3 -c 'import json,sys; s=json.load(sys.stdin)["compat"]; assert s["state"]=="ready", s; print(s["pid"])')"
alive "$child_pid" || fail "compat child $child_pid is not running"
plan="$(curl --fail --silent --show-error --max-time 30 -H "Cookie: $cookie" -H "Origin: $origin" \
  -H 'Content-Type: application/json' -d '{"name":"Workflow smoke"}' "$origin/plans")"
plan_id="$(printf '%s' "$plan" | python3 -c 'import json,sys; p=json.load(sys.stdin); assert p["name"]=="Workflow smoke", p; print(p["id"])')"
curl --fail --silent --show-error --max-time 30 -H "Cookie: $cookie" -H "Origin: $origin" "$origin/plans" \
  | python3 -c 'import json,sys; assert any(p["id"]==int(sys.argv[1]) for p in json.load(sys.stdin)["profiles"])' "$plan_id" \
  || fail 'created plan not listed'
log "http: 401 without session, bootstrap single-use, /__runtime ready (child $child_pid), POST+GET /plans id=$plan_id"

# WebSocket: hold an admitted upgrade open until shutdown closes it.
printf '{"origin":"%s","cookie":"%s","mode":"hold"}' "$origin" "$cookie" \
  | "$node" "$root/rust/tests/gateway_ws.mjs" >"$fixture/ws.out" 2>"$fixture/ws.err" &
ws_pid=$!
SECONDS=0
until grep -q '"ready":true' "$fixture/ws.out" 2>/dev/null; do
  alive "$ws_pid" || fail 'websocket client exited before open'
  ((SECONDS < 15)) || fail 'websocket open timed out'
  sleep 0.1
done
log 'ws: /ws/profile-sync upgrade admitted and open'

# MCP: authenticated JSON-RPC reaches the gateway, which refuses it fail-closed
# until API-key authorization is verified (see rust/README.md).
mcp_body='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"pp-workflow-smoke","version":"1"}}}'
status="$(curl --silent --max-time 10 --output /dev/null --write-out '%{http_code}' \
  -H "Origin: $origin" -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
  -d "$mcp_body" "$origin/api/v1/mcp")"
test "$status" = 401 || fail "unauthenticated MCP returned $status, expected 401"
mcp="$(curl --silent --max-time 10 --write-out '\n%{http_code}' -H "Cookie: $cookie" -H "Origin: $origin" \
  -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
  -d "$mcp_body" "$origin/api/v1/mcp")"
test "$(printf '%s' "$mcp" | tail -n 1)" = 403 || fail "authenticated MCP returned: $mcp"
printf '%s' "$mcp" | grep -q 'MCP is disabled in this foundation' || fail "unexpected MCP refusal: $mcp"
log 'mcp: initialize -> 401 without session, 403 fail-closed with session'

alive "$ws_pid" || fail 'websocket client exited before shutdown'
if grep -q '"closed"' "$fixture/ws.out"; then fail 'websocket closed before shutdown'; fi
SECONDS=0
kill -TERM "$server_pid"
wait_exit "$server_pid" 17 || fail 'server did not exit within 17s of SIGTERM'
shutdown_seconds=$SECONDS
set +e
wait "$server_pid"
server_exit=$?
set -e
server_pid_stopped="$server_pid"
server_pid=''
test "$server_exit" = 1 || fail "server exited $server_exit after stalled WebSocket drain"
((shutdown_seconds >= 5 && shutdown_seconds < 17)) || fail "shutdown took ${shutdown_seconds}s"
python3 - "$fixture/server.out" "$fixture/server.err" <<'PY'
import json, pathlib, sys
records = pathlib.Path(sys.argv[1]).read_text().splitlines()
assert len(records) == 2, records
receipt = json.loads(records[1])
assert receipt == {
    "proof_class": "headless_unsigned", "tracked_idle": True, "gateway_idle": False,
    "compat_exit": "graceful", "compat_reaped": True, "storage_released": True,
    "gateway_stopped": True, "marker_removed": True, "runtime_removed": True,
    "errors": ["gateway_drain_timed_out"],
}, receipt
def log_row(line):
    """pp-server's structured tracing rows (GRE-1457): JSON objects with a target."""
    try:
        row = json.loads(line)
    except ValueError:
        return False
    return isinstance(row, dict) and "target" in row
logged = pathlib.Path(sys.argv[2]).read_text()
stderr = "\n".join(line for line in logged.splitlines() if not log_row(line)).strip()
assert stderr == "Error: Desktop shutdown incomplete", logged
PY
wait_exit "$ws_pid" 10 || fail 'websocket client did not exit after shutdown'
set +e
wait "$ws_pid"
ws_exit=$?
set -e
ws_pid=''
test "$ws_exit" = 0 || fail "websocket client exited $ws_exit"
grep -q '"closed":true' "$fixture/ws.out" || fail 'websocket was not closed by shutdown'
if alive "$child_pid"; then fail "compat child $child_pid survived stop"; fi
check_no_leftovers -g "$child_pid"
check_no_leftovers -P "$server_pid_stopped"
log "stop: SIGTERM -> gateway drain timeout, exit 1 in ${shutdown_seconds}s, ws closed, child $child_pid gone, no process-group or child leftovers"

# Data lock released at stop.
test ! -e "$data/.desktop-lease" || fail 'compatibility reservation survived stop'
test ! -e "$runtime_dir" || fail 'runtime directory survived stop'
lock="$(lock_state)" || fail 'data lock probe failed after stop'
test "$lock" = free || fail "data lock is $lock after stop"
log 'lock: released at stop (flock acquired, compatibility reservation removed)'

python3 - "$commit" "$origin" "$child_pid" "$plan_id" "$fixture/server.out" "$shutdown_seconds" <<'PY'
import json, platform, sys
shutdown = json.loads(open(sys.argv[5]).read().splitlines()[1])
print(json.dumps({"proof_class": "headless_unsigned", "case": "stage1_server_smoke", "os": platform.system(),
                  "arch": platform.machine(), "commit": sys.argv[1], "origin": sys.argv[2], "child_pid": int(sys.argv[3]),
                  "plan_id": int(sys.argv[4]), "http_authenticated": True, "ws_open_and_closed_on_stop": True,
                  "mcp_fail_closed": True, "lock_held_at_boot": True, "lock_released_at_stop": True,
                  "server_exit": 1, "shutdown_seconds": int(sys.argv[6]), "shutdown": shutdown,
                  "runtime_removed": True, "leftover_processes": 0}))
PY
log PASS
