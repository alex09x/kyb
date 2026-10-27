#!/usr/bin/env bash
# Copyright (c) 2026 Alexander Panasenko
# Contact: alex@prod.codes
# Author: https://prod.codes/about/
# Project: https://github.com/alex09x/kyb
# SPDX-License-Identifier: MIT
# CLI exit codes and error output against a real server: 0 for success,
# 1 for server rejections (4xx) and validation errors, 2 for server errors (5xx),
# and 3 when the server cannot be reached. Non-zero exits print the error
# body or message on stderr and leave stdout empty.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cli="$repo_root/skills/kyb/bin/kyb"
server="${KYB_SERVER:-$repo_root/target/release/kyb-server}"
port="${KYB_EXIT_CODES_PORT:-9395}"
work="$(mktemp -d -t kyb-exit-codes-test.XXXXXX)"

cleanup() {
  if [ -n "${server_pid:-}" ]; then kill "$server_pid" 2>/dev/null; wait "$server_pid" 2>/dev/null || true; fi
  if [ -n "${stub_pid:-}" ]; then kill "$stub_pid" 2>/dev/null; wait "$stub_pid" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT

[ -x "$server" ] || { echo "no server binary at $server" >&2; exit 2; }
[ -x "$cli" ] || { echo "no CLI at $cli" >&2; exit 2; }

KYB_DATA="$work/data" KYB_INDEX="$work/index" KYB_AUDIT="$work/audit.jsonl" KYB_MODEL="$work/no-model" \
  KYB_ADDR="127.0.0.1:$port" KYB_AUTH=off "$server" > "$work/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do curl -sf "http://127.0.0.1:$port/healthz" >/dev/null && break; sleep 0.1; done

export KYB_URL="http://127.0.0.1:$port" HOME="$work/home" KYB_TODAY=2026-03-10
mkdir -p "$HOME"
fail() { echo "FAIL: $*" >&2; exit 1; }
json_has() {
  python3 -c 'import json,sys; j=json.load(open(sys.argv[1])); sys.exit(0 if eval(sys.argv[2]) else 1)' "$1" "$2"
}

# 1. 404: kyb get on a missing key -> exit 1, error body on stderr, stdout empty
rc=0
"$cli" get missing-key > "$work/out.txt" 2> "$work/err.txt" || rc=$?
[ "$rc" -eq 1 ] || fail "kyb get missing-key: expected exit 1, got $rc"
[ ! -s "$work/out.txt" ] || fail "kyb get missing-key: expected empty stdout, got: $(cat "$work/out.txt")"
[ -s "$work/err.txt" ] || fail "kyb get missing-key: expected error on stderr, got nothing"
grep -q "no entry 'missing-key'" "$work/err.txt" || fail "kyb get missing-key: unexpected stderr: $(cat "$work/err.txt")"

# 2. Write refused by validation: task with invalid due date -> exit 1, error on stderr, stdout empty
rc=0
echo "task body" | "$cli" task --key task-bad-due --title "Bad due" --due 2026-02-30 > "$work/out.txt" 2> "$work/err.txt" || rc=$?
[ "$rc" -eq 1 ] || fail "kyb task --due 2026-02-30: expected exit 1, got $rc"
[ ! -s "$work/out.txt" ] || fail "kyb task --due 2026-02-30: expected empty stdout, got: $(cat "$work/out.txt")"
[ -s "$work/err.txt" ] || fail "kyb task --due 2026-02-30: expected error on stderr, got nothing"
grep -q "due must be YYYY-MM-DD" "$work/err.txt" || fail "kyb task --due 2026-02-30: unexpected stderr: $(cat "$work/err.txt")"

# 3. Write refused by validation: task-status with invalid status -> exit 1, error on stderr, stdout empty
echo "initial task" | "$cli" task --key task-seed --title "Seed task" >/dev/null
rc=0
"$cli" task-status task-seed --status bogus > "$work/out.txt" 2> "$work/err.txt" || rc=$?
[ "$rc" -eq 1 ] || fail "kyb task-status --status bogus: expected exit 1, got $rc"
[ ! -s "$work/out.txt" ] || fail "kyb task-status --status bogus: expected empty stdout, got: $(cat "$work/out.txt")"
[ -s "$work/err.txt" ] || fail "kyb task-status --status bogus: expected error on stderr, got nothing"
grep -q "status must be one of" "$work/err.txt" || fail "kyb task-status --status bogus: unexpected stderr: $(cat "$work/err.txt")"

# 4. Unreachable server -> exit 3, error on stderr, stdout empty
unreachable_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); p=s.getsockname()[1]; s.close(); print(p)')
rc=0
KYB_URL="http://127.0.0.1:$unreachable_port" "$cli" get task-seed > "$work/out.txt" 2> "$work/err.txt" || rc=$?
[ "$rc" -eq 3 ] || fail "unreachable server: expected exit 3, got $rc"
[ ! -s "$work/out.txt" ] || fail "unreachable server: expected empty stdout, got: $(cat "$work/out.txt")"
[ -s "$work/err.txt" ] || fail "unreachable server: expected error on stderr, got nothing"
grep -q "cannot reach 127.0.0.1:$unreachable_port" "$work/err.txt" || fail "unreachable server: unexpected stderr: $(cat "$work/err.txt")"

# 5. Server error (5xx) -> exit 2, error on stderr, stdout empty
stub_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); p=s.getsockname()[1]; s.close(); print(p)')
python3 -c '
import http.server, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(500)
        self.send_header("content-type", "application/json")
        body = b"{\"error\":\"simulated internal server error\"}"
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
' "$stub_port" &
stub_pid=$!
for _ in $(seq 1 40); do curl -s "http://127.0.0.1:$stub_port/healthz" >/dev/null && break; sleep 0.1; done

rc=0
KYB_URL="http://127.0.0.1:$stub_port" "$cli" get task-seed > "$work/out.txt" 2> "$work/err.txt" || rc=$?
[ "$rc" -eq 2 ] || fail "server 500 error: expected exit 2, got $rc"
[ ! -s "$work/out.txt" ] || fail "server 500 error: expected empty stdout, got: $(cat "$work/out.txt")"
[ -s "$work/err.txt" ] || fail "server 500 error: expected error on stderr, got nothing"
grep -q "simulated internal server error" "$work/err.txt" || fail "server 500 error: unexpected stderr: $(cat "$work/err.txt")"

# 6. Successful write -> exit 0, JSON on stdout
rc=0
echo "healthy task body" | "$cli" task --key task-healthy --title "Healthy task" > "$work/out.txt" 2> "$work/err.txt" || rc=$?
[ "$rc" -eq 0 ] || fail "successful task write: expected exit 0, got $rc (stderr: $(cat "$work/err.txt"))"
[ -s "$work/out.txt" ] || fail "successful task write: expected stdout with JSON, got nothing"
json_has "$work/out.txt" 'j["key"] == "task-healthy" and j["action"] == "created"' || fail "successful task write: JSON invalid: $(cat "$work/out.txt")"

# 7. Successful get -> exit 0, JSON on stdout
rc=0
"$cli" get task-healthy --no-deadlines > "$work/out.txt" 2> "$work/err.txt" || rc=$?
[ "$rc" -eq 0 ] || fail "successful get: expected exit 0, got $rc (stderr: $(cat "$work/err.txt"))"
[ -s "$work/out.txt" ] || fail "successful get: expected stdout with JSON, got nothing"
json_has "$work/out.txt" 'j["key"] == "task-healthy" and j["title"] == "Healthy task"' || fail "successful get: JSON invalid: $(cat "$work/out.txt")"

# 8. A key that is not a slug reaches the server and is refused there -> exit 1, not 3:
#    unescaped, curl rejects the URL before any request and it looks like an unreachable server
refused_not_unreachable() {
  local rc=0
  "$cli" "$@" > "$work/out.txt" 2> "$work/err.txt" || rc=$?
  [ "$rc" -eq 1 ] || fail "kyb $*: expected exit 1, got $rc (stderr: $(cat "$work/err.txt"))"
  [ ! -s "$work/out.txt" ] || fail "kyb $*: expected empty stdout, got: $(cat "$work/out.txt")"
  ! grep -q "cannot reach" "$work/err.txt" || fail "kyb $*: reported unreachable: $(cat "$work/err.txt")"
}
refused_not_unreachable get "Bad Key"
refused_not_unreachable history "Bad Key"
refused_not_unreachable diff "Bad Key"
refused_not_unreachable get "a/b"

echo "all CLI exit code checks passed"
