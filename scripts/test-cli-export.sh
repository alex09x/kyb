#!/usr/bin/env bash
# `kyb export` against a real server: every state in the output, pages of one
# snapshot stitched without gaps or repeats, and an export pinned to an older
# commit unchanged by the writes made after it.
#
#   cargo build --release --bin kyb-server && scripts/test-cli-export.sh
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cli="$repo_root/skills/kyb/bin/kyb"
server="${KYB_SERVER:-$repo_root/target/release/kyb-server}"
port="${KYB_EXPORT_PORT:-9391}"
work="$(mktemp -d -t kyb-export-test.XXXXXX)"

cleanup() {
  if [ -n "${server_pid:-}" ]; then kill "$server_pid" 2>/dev/null; wait "$server_pid" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT

KYB_DATA="$work/data" KYB_INDEX="$work/index" KYB_AUDIT="$work/audit.jsonl" KYB_MODEL="$work/no-model" \
  KYB_ADDR="127.0.0.1:$port" KYB_AUTH=off "$server" > "$work/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do curl -sf "http://127.0.0.1:$port/healthz" >/dev/null && break; sleep 0.1; done

export KYB_URL="http://127.0.0.1:$port" HOME="$work/home"
mkdir -p "$HOME"
fail() { echo "FAIL: $*" >&2; exit 1; }
for k in alpha bravo charlie delta echo; do echo "body of $k" | "$cli" add --key "$k" --title "$k" >/dev/null; done
"$cli" rm delta >/dev/null
echo "what broke" | "$cli" incident --key inc-2026-01-01-a --title A --service s --severity low >/dev/null
"$cli" resolve inc-2026-01-01-a --resolution "fixed" >/dev/null
echo "to do" | "$cli" task --key task-a --title A >/dev/null
"$cli" done task-a --resolution "done" >/dev/null
echo "to do" | "$cli" task --key task-b --title B >/dev/null

"$cli" export --page 2 --out "$work/all.jsonl" > "$work/summary.json" || fail "export did not verify: $(cat "$work/summary.json")"
python3 - "$work/summary.json" "$work/all.jsonl" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
rows = [json.loads(l) for l in open(sys.argv[2])]
assert s["complete"] and s["entries"] == 8 and s["unique_keys"] == 8 and s["pages"] == 4, s
assert s["counts"] == {"incident": {"archived": 1}, "knowledge": {"active": 4, "retracted": 1},
                       "task": {"active": 1, "archived": 1}}, s["counts"]
state = {r["key"]: r["state"] for r in rows}
assert state["delta"] == "retracted" and state["task-a"] == "archived" and state["task-b"] == "active", state
PY
snap="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["snapshot"])' "$work/summary.json")"

# later writes: a new key, a rewrite, a retraction
echo "new" | "$cli" add --key aardvark --title aardvark >/dev/null
echo "rewritten" | "$cli" add --key bravo --title bravo >/dev/null
"$cli" rm charlie >/dev/null

"$cli" export --at "$snap" --page 3 --out "$work/pinned.jsonl" > /dev/null || fail "pinned export did not verify"
cmp -s "$work/all.jsonl" "$work/pinned.jsonl" || fail "an export pinned to $snap changed after later writes"
"$cli" export --out "$work/now.jsonl" > "$work/now.json" || fail "current export did not verify"
python3 -c 'import json,sys; s=json.load(open(sys.argv[1])); assert s["entries"]==9 and s["counts"]["knowledge"]=={"active":4,"retracted":2}, s' "$work/now.json"

echo "kyb export: 3 exports verified (paged, pinned after later writes, current)"
