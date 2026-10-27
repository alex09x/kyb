#!/usr/bin/env bash
# Copyright (c) 2026 Alexander Panasenko
# Contact: alex@prod.codes
# Author: https://prod.codes/about/
# Project: https://github.com/alex09x/kyb
# SPDX-License-Identifier: MIT
#
# Verify CLI attribution under nested agents, process tree walking,
# KYB_AGENT / KYB_SESSION overrides, and environment variable fallbacks.
#
#   scripts/test-cli-attribution.sh
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cli="$repo_root/skills/kyb/bin/kyb"
tmpdir="$(mktemp -d -t kyb-attribution-test.XXXXXX)"

cleanup() {
  if [ -n "${stub_pid:-}" ]; then
    kill "$stub_pid" 2>/dev/null || true
    wait "$stub_pid" 2>/dev/null || true
  fi
  rm -rf "$tmpdir"
}
trap cleanup EXIT

# 1. Tiny Python HTTP stub server echoing headers as JSON
stub_py="$tmpdir/stub.py"
port_file="$tmpdir/port"

cat > "$stub_py" <<'PY'
import http.server, json, sys

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({
            "agent": self.headers.get("x-kyb-agent"),
            "session": self.headers.get("x-kyb-session"),
            "nested_in": self.headers.get("x-kyb-nested-in"),
            "workspace": self.headers.get("x-kyb-workspace"),
            "via": self.headers.get("x-kyb-via"),
        }).encode("utf-8")
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass

server = http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H)
port = server.server_address[1]
with open(sys.argv[2], "w") as f:
    f.write(str(port))
server.serve_forever()
PY

base_port="${KYB_ATTRIBUTION_PORT:-0}"
python3 "$stub_py" "$base_port" "$port_file" &
stub_pid=$!

port=""
for _ in $(seq 1 40); do
  if [ -s "$port_file" ]; then
    port="$(cat "$port_file")"
    if curl -s -o /dev/null -m 1 "http://127.0.0.1:$port/healthz" 2>/dev/null; then
      break
    fi
  fi
  sleep 0.1
done

if [ -z "$port" ]; then
  echo "FAIL: HTTP stub server failed to start" >&2
  exit 1
fi

# 2. Fake ps command that reads a mock process table file
mkdir -p "$tmpdir/bin"
cat > "$tmpdir/bin/ps" <<'EOF'
#!/usr/bin/env bash
target_pid=""
want_comm=0
want_args=0

while [ $# -gt 0 ]; do
  case "$1" in
    -p)
      shift
      target_pid="$1"
      ;;
    -o)
      shift
      case "$1" in
        *comm*) want_comm=1 ;;
        *args*) want_args=1 ;;
      esac
      ;;
  esac
  shift
done

[ -z "$target_pid" ] && exit 1

table_file="${KYB_TEST_PROC_TABLE_FILE:-}"
if [ -n "$table_file" ] && [ -f "$table_file" ]; then
  while IFS=':' read -r pid ppid comm args; do
    [ -z "$pid" ] && continue
    case "$pid" in \#*) continue ;; esac
    if [ "$pid" = "$target_pid" ]; then
      if [ "$want_args" -eq 1 ]; then
        printf '%s\n' "${args:-$comm}"
      else
        printf ' %s %s\n' "$ppid" "$comm"
      fi
      exit 0
    fi
  done < "$table_file"

  # Fallback for unknown PID (e.g. caller subshell PID):
  # Map unknown PID to the first entry in table_file so the walk enters the fake tree
  if IFS=':' read -r first_pid first_ppid first_comm first_args < "$table_file"; then
    if [ "$want_args" -eq 1 ]; then
      printf '%s\n' "/bin/bash"
    else
      printf ' %s %s\n' "$first_pid" "/bin/bash"
    fi
    exit 0
  fi
fi

exit 1
EOF
chmod +x "$tmpdir/bin/ps"

failures=0

check_attribution() {
  local label="$1"
  local expected_agent="$2"
  local expected_session="$3"
  local expected_nested="${4:-}"
  local table_content="$5"
  shift 5

  local table_file="$tmpdir/table.txt"
  printf '%s\n' "$table_content" > "$table_file"

  local out rc=0
  out="$(
    env -u KYB_AGENT \
        -u KYB_SESSION \
        -u CLAUDECODE \
        -u CLAUDE_CODE_SESSION_ID \
        -u CLAUDE_PID \
        -u CODEX_THREAD_ID \
        -u CODEX_SESSION_ID \
        -u ANTIGRAVITY_CONVERSATION_ID \
        -u ANTIGRAVITY_AGENT \
        -u ANTIGRAVITY_AGENTAPI_EXE \
        -u PROD_PROVIDER \
        -u PROD_PRIMARY_SESSION_KEY \
        PATH="$tmpdir/bin:$PATH" \
        KYB_ADDR="127.0.0.1:$port" \
        KYB_TEST_PROC_TABLE_FILE="$table_file" \
        "$@" \
        bash "$cli" whoami 2>&1
  )" || rc=$?

  if [ "$rc" -ne 0 ]; then
    printf 'FAIL  %-48s (command failed with exit %s)\n' "$label" "$rc"
    printf '        %s\n' "$out"
    failures=$((failures + 1))
    return
  fi

  local actual_agent actual_session actual_nested
  actual_agent="$(printf '%s' "$out" | python3 -c 'import sys, json; d=json.load(sys.stdin); print(d.get("agent") or "")')"
  actual_session="$(printf '%s' "$out" | python3 -c 'import sys, json; d=json.load(sys.stdin); print(d.get("session") or "")')"
  actual_nested="$(printf '%s' "$out" | python3 -c 'import sys, json; d=json.load(sys.stdin); print(d.get("nested_in") or "")')"

  if [ "$actual_agent" = "$expected_agent" ] && \
     [ "$actual_session" = "$expected_session" ] && \
     [ "$actual_nested" = "$expected_nested" ]; then
    printf 'ok    %-48s (agent=%s session=%s nested=%s)\n' \
      "$label" "$actual_agent" "$actual_session" "${actual_nested:-none}"
  else
    printf 'FAIL  %-48s\n' "$label"
    printf '        expected: agent=%s session=%s nested=%s\n' \
      "$expected_agent" "$expected_session" "${expected_nested:-none}"
    printf '        actual:   agent=%s session=%s nested=%s\n' \
      "$actual_agent" "$actual_session" "${actual_nested:-none}"
    printf '        response: %s\n' "$out"
    failures=$((failures + 1))
  fi
}

# Case 1: Claude Code inside Antigravity -> claude with Claude session, nested in antigravity
check_attribution \
  "Claude Code inside Antigravity" \
  "claude" "claude-sess-111" "antigravity" \
  "1001:1002:/Users/alex/.local/bin/claude:claude
1002:1:/Applications/Antigravity.app/Contents/MacOS/Antigravity:Antigravity" \
  ANTIGRAVITY_CONVERSATION_ID=anti-sess-000 \
  CLAUDE_CODE_SESSION_ID=claude-sess-111

# Case 2: Codex inside Claude Code -> codex with Codex session, nested in claude
check_attribution \
  "Codex inside Claude Code" \
  "codex" "codex-sess-222" "claude" \
  "2001:2002:/Applications/CodexCLI.app/Contents/MacOS/codex:codex
2002:1:/Users/alex/.local/bin/claude:claude" \
  CLAUDE_CODE_SESSION_ID=claude-sess-111 \
  CODEX_THREAD_ID=codex-sess-222

# Case 3: Antigravity inside Claude Code -> antigravity with Antigravity session, nested in claude
check_attribution \
  "Antigravity inside Claude Code" \
  "antigravity" "anti-sess-333" "claude" \
  "3001:3002:/Applications/Antigravity.app/Contents/MacOS/Antigravity Helper:Antigravity Helper
3002:1:/Users/alex/.local/bin/claude:claude" \
  CLAUDE_CODE_SESSION_ID=claude-sess-111 \
  ANTIGRAVITY_CONVERSATION_ID=anti-sess-333

# Case 4: KYB_AGENT overrides discovered inner agent
check_attribution \
  "KYB_AGENT override over inner agent" \
  "antigravity" "anti-sess-999" "" \
  "2001:2002:/Applications/CodexCLI.app/Contents/MacOS/codex:codex
2002:1:/Users/alex/.local/bin/claude:claude" \
  CODEX_THREAD_ID=codex-sess-222 \
  CLAUDE_CODE_SESSION_ID=claude-sess-111 \
  ANTIGRAVITY_CONVERSATION_ID=anti-sess-999 \
  KYB_AGENT=antigravity

# Case 5: KYB_AGENT and KYB_SESSION override everything
check_attribution \
  "KYB_AGENT and KYB_SESSION override everything" \
  "custom-agent" "custom-session-888" "" \
  "1001:1002:/Users/alex/.local/bin/claude:claude
1002:1:/Applications/Antigravity.app/Contents/MacOS/Antigravity:Antigravity" \
  CLAUDE_CODE_SESSION_ID=claude-sess-111 \
  ANTIGRAVITY_CONVERSATION_ID=anti-sess-000 \
  KYB_AGENT=custom-agent \
  KYB_SESSION=custom-session-888

# Case 6: No-ancestor fallback: env precedence picks Antigravity over Claude
check_attribution \
  "No-ancestor fallback (Antigravity over Claude)" \
  "antigravity" "anti-fallback-444" "" \
  "5001:5002:/bin/bash:/bin/bash
5002:1:/bin/bash:/bin/bash" \
  ANTIGRAVITY_CONVERSATION_ID=anti-fallback-444 \
  CLAUDE_CODE_SESSION_ID=claude-fallback-555

# Case 7: No-ancestor fallback: env precedence picks Codex over Antigravity and Claude
check_attribution \
  "No-ancestor fallback (Codex over others)" \
  "codex" "codex-fallback-666" "" \
  "5001:5002:/bin/bash:/bin/bash
5002:1:/bin/bash:/bin/bash" \
  CODEX_THREAD_ID=codex-fallback-666 \
  ANTIGRAVITY_CONVERSATION_ID=anti-fallback-444 \
  CLAUDE_CODE_SESSION_ID=claude-fallback-555

# Case 8: No-ancestor fallback with only Claude in environment
check_attribution \
  "No-ancestor fallback (Claude only)" \
  "claude" "claude-fallback-555" "" \
  "5001:5002:/bin/bash:/bin/bash
5002:1:/bin/bash:/bin/bash" \
  CLAUDE_CODE_SESSION_ID=claude-fallback-555

# Case 9: Explicit KYB_PS_COMMAND hook
check_attribution \
  "KYB_PS_COMMAND hook with single agent" \
  "claude" "claude-hook-777" "" \
  "6001:1:/Users/alex/.local/bin/claude:claude" \
  CLAUDE_CODE_SESSION_ID=claude-hook-777 \
  KYB_PS_COMMAND="$tmpdir/bin/ps"

# Case 10: Fallback to args for Linux-style node wrapper
check_attribution \
  "Fallback to ps args for node wrapper" \
  "claude" "claude-node-888" "" \
  "7001:1:node:node --max-old-space-size=4096 /Users/alex/.local/bin/claude" \
  CLAUDE_CODE_SESSION_ID=claude-node-888

# Case 11: Extra signal from CLAUDE_PID
check_attribution \
  "CLAUDE_PID extra ancestor signal" \
  "claude" "claude-pid-999" "" \
  "8001:1:sh:sh /some/wrapper/script.sh" \
  CLAUDE_PID=8001 \
  CLAUDE_CODE_SESSION_ID=claude-pid-999

# Case 12: the shell an agent runs its tools in names other agents in its command
# line; it is not one (review of #37)
check_attribution \
  "A shell command line that mentions agents" \
  "claude" "claude-shell-121" "" \
  "9101:9102:/bin/zsh:/bin/zsh -c claude --version && kyb add --body /Applications/Antigravity.app/x
9102:1:/Users/alex/.local/bin/claude:claude" \
  CLAUDE_CODE_SESSION_ID=claude-shell-121

echo "---"
if [ "$failures" -ne 0 ]; then
  echo "$failures attribution case(s) failed" >&2
  exit 1
fi
echo "CLI attribution tests passed in all cases"
