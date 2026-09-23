#!/usr/bin/env bash
# The CLI refuses to send a version-dependent flag to a server that cannot
# honour it. That guard is only worth having if it fails CLOSED in every way it
# can fail: a check that silently passes is worse than no check, because nobody
# looks again and the original bug returns wearing a badge.
#
# So this exercises the failure modes rather than the happy path, which the Rust
# suite already covers. Each stub stands for a real situation:
#
#   nothing listening   the server is down, or KYB_ADDR is wrong
#   502 + HTML          a proxy or gateway answering instead of the server
#   empty 200           a load balancer with nothing behind it
#   foreign JSON        something else entirely on that port
#   hangs               a wedged server, caught by the curl timeout
#   old KYB             the case this exists for: 0.2.1, no capabilities field
#
#   scripts/test-cli-preflight.sh
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cli="$repo_root/skills/kyb/bin/kyb"
stub_py="$(mktemp -t kyb-preflight-stubs.XXXXXX).py"
base_port="${KYB_PREFLIGHT_PORT:-9380}"

cleanup() {
  if [ -n "${stub_pid:-}" ]; then
    kill "$stub_pid" 2>/dev/null
    wait "$stub_pid" 2>/dev/null
  fi
  rm -f "$stub_py"
}
trap cleanup EXIT

cat > "$stub_py" <<'PY'
import http.server, json, sys, threading, time

base = int(sys.argv[1])

def serve(offset, handler):
    class H(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            handler(self)

        def log_message(self, *a):
            pass

    http.server.HTTPServer(("127.0.0.1", base + offset), H).serve_forever()

def send(h, code, ctype, body):
    h.send_response(code)
    h.send_header("content-type", ctype)
    h.send_header("content-length", str(len(body)))
    h.end_headers()
    h.wfile.write(body)

handlers = {
    1: lambda h: send(h, 502, "text/html", b"<html>502 Bad Gateway</html>"),
    2: lambda h: send(h, 200, "application/json", b""),
    3: lambda h: send(h, 200, "application/json", json.dumps({"status": "ok"}).encode()),
    4: lambda h: time.sleep(30),
    # the released 0.2.1 shape: healthy, informative, and silent about capabilities
    5: lambda h: send(h, 200, "application/json", json.dumps(
        {"ok": True, "entries": 565, "open_incidents": 69, "open_tasks": 83}).encode()),
}
for offset, fn in handlers.items():
    threading.Thread(target=serve, args=(offset, fn), daemon=True).start()
print("ready", flush=True)
time.sleep(900)
PY

python3 "$stub_py" "$base_port" &
stub_pid=$!
for _ in $(seq 1 40); do
  curl -s -o /dev/null -m 1 "http://127.0.0.1:$((base_port + 1))/healthz" && break
  sleep 0.25
done

failures=0

refuses() { # refuses <label> <port-offset> <argv...>
  local label="$1" offset="$2"
  shift 2
  local addr="127.0.0.1:$((base_port + offset))" out rc
  out="$(KYB_ADDR="$addr" bash "$cli" "$@" 2>&1)"
  rc=$?
  if [ "$rc" -eq 0 ]; then
    printf 'FAIL  %-34s the command SUCCEEDED; the preflight failed open\n' "$label"
    printf '        %s\n' "$out"
    failures=$((failures + 1))
    return
  fi
  case "$out" in
    *"NOT sent"* | *"predates"* | *"does not support"*)
      printf 'ok    %-34s refused (exit %s)\n' "$label" "$rc" ;;
    *)
      printf 'FAIL  %-34s refused, but without saying why\n' "$label"
      printf '        %s\n' "$out"
      failures=$((failures + 1)) ;;
  esac
}

# offset 0 is deliberately never bound: nothing is listening there
refuses "nothing listening"        0 query x --as-of 2026-08-01
refuses "502 from a proxy"         1 query x --as-of 2026-08-01
refuses "empty 200 body"           2 query x --as-of 2026-08-01
refuses "foreign JSON service"     3 query x --as-of 2026-08-01
refuses "wedged server (timeout)"  4 query x --as-of 2026-08-01
refuses "released 0.2.1 server"    5 query x --as-of 2026-08-01
refuses "released 0.2.1, window"   5 query x --changed-between 2026-08-01,2026-08-02
refuses "released 0.2.1, diff"     5 diff some-key

echo "---"
if [ "$failures" -ne 0 ]; then
  echo "$failures preflight case(s) did not fail closed"
  exit 1
fi
echo "CLI preflight fails closed in all 8 cases"
