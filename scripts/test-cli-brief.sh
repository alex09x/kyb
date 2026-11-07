#!/usr/bin/env bash
# Copyright (c) 2026 Alexander Panasenko
# Contact: alex@prod.codes
# Author: https://prod.codes/about/
# Project: https://github.com/alex09x/kyb
# SPDX-License-Identifier: MIT
# `kyb brief` through the CLI against a real server holding a large base: every
# section capped with "+N more", the whole brief under 40 lines, --json equal to
# GET /brief, filters, a retraction, and a closed incident gone at once.
#
#   mitts build --release --bin kyb-server && scripts/test-cli-brief.sh
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cli="$repo_root/skills/kyb/bin/kyb"
server="${KYB_SERVER:-$repo_root/target/release/kyb-server}"
port="${KYB_BRIEF_PORT:-9394}"
work="$(mktemp -d -t kyb-brief-test.XXXXXX)"

stop_server() {
  if [ -n "${server_pid:-}" ]; then kill "$server_pid" 2>/dev/null; wait "$server_pid" 2>/dev/null || true; server_pid=""; fi
}
cleanup() { stop_server; rm -rf "$work"; }
trap cleanup EXIT

KYB_DATA="$work/data" KYB_INDEX="$work/index" KYB_AUDIT="$work/audit.jsonl" KYB_MODEL="$work/no-model" \
  KYB_ADDR="127.0.0.1:$port" KYB_AUTH=off "$server" > "$work/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do curl -sf "http://127.0.0.1:$port/healthz" >/dev/null && break; sleep 0.1; done

export KYB_URL="http://127.0.0.1:$port" HOME="$work/home" KYB_TODAY=2026-03-10
mkdir -p "$HOME"
fail() { echo "FAIL: $*" >&2; exit 1; }

# a base with far more of everything than a brief shows
for n in $(seq -w 1 12); do
  echo "what happened" | "$cli" incident --key "inc-2026-03-01-svc$n" --title "Service $n is down" \
    --service "svc$n" --severity high --detection "curl /healthz -> ok" >/dev/null
done
for n in $(seq -w 1 20); do
  echo "doing it" | "$cli" task --key "task-doing-$n" --title "Work item $n" --status in_progress >/dev/null
done
for n in $(seq -w 1 8); do
  echo "late" | "$cli" task --key "task-late-$n" --title "Late item $n" --due 2026-03-01 >/dev/null
done
for n in $(seq -w 1 30); do
  echo "a fact" | "$cli" add --key "fact-$n" --title "Fact $n" --tags "$([ "$n" -le 3 ] && echo edge || echo misc)" >/dev/null
done
for n in $(seq -w 1 8); do
  echo "a wrong turn" | "$cli" add --key "rejected-$n" --title "Rejected approach $n" >/dev/null
  "$cli" rm "rejected-$n" >/dev/null
done

"$cli" brief > "$work/brief.txt" || fail "kyb brief failed"
lines=$(wc -l < "$work/brief.txt")
[ "$lines" -le 40 ] || fail "the brief is $lines lines: $(cat "$work/brief.txt")"
for want in "DEADLINES: 8 overdue" "OPEN INCIDENTS (12)" "ACTIVE WORK (20)" "RECENTLY RETRACTED (8)" \
            "RECENTLY CHANGED (30)" "+7 more: kyb incidents" "+13 more: kyb tasks" "+3 more: kyb deadlines" \
            "+25 more: kyb brief --limit N" "  Rejected approach "; do
  grep -qF -- "$want" "$work/brief.txt" || fail "no '$want' in: $(cat "$work/brief.txt")"
done
grep -q "what happened\|a wrong turn\|doing it" "$work/brief.txt" && fail "a body leaked into the brief"

# --json is the server's answer, unchanged
"$cli" brief --json > "$work/cli.json"
curl -sf -H "x-kyb-today: 2026-03-10" "$KYB_URL/brief" > "$work/http.json"
python3 - "$work/cli.json" "$work/http.json" <<'PY' || fail "--json differs from GET /brief"
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:])
assert sorted(a) == sorted(b) == ["changed", "deadlines", "incidents", "retracted", "since", "today", "work"], (a, b)
for s in ("deadlines", "incidents", "work", "retracted", "changed"):
    assert a[s]["count"] == b[s]["count"] and [i["key"] for i in a[s]["items"]] == [i["key"] for i in b[s]["items"]], s
PY

# filters reach every section
"$cli" brief --tag edge --limit 2 > "$work/edge.txt"
grep -qF "RECENTLY CHANGED (3)" "$work/edge.txt" || fail "--tag: $(cat "$work/edge.txt")"
grep -qF "+1 more" "$work/edge.txt" || fail "--limit: $(cat "$work/edge.txt")"
! grep -q "OPEN INCIDENTS\|ACTIVE WORK" "$work/edge.txt" || fail "--tag left other sections in: $(cat "$work/edge.txt")"
"$cli" brief --service svc03 > "$work/svc.txt"
grep -qF "OPEN INCIDENTS (1)" "$work/svc.txt" || fail "--service: $(cat "$work/svc.txt")"
"$cli" brief --since 0x 2> "$work/err.txt" && fail "a malformed --since was accepted"
grep -q "since" "$work/err.txt" || fail "--since: the refusal does not say why: $(cat "$work/err.txt")"

# a closed incident leaves at once
"$cli" resolve inc-2026-03-01-svc03 --resolution "restarted" >/dev/null
"$cli" brief --service svc03 > "$work/svc.txt"
! grep -q "OPEN INCIDENTS" "$work/svc.txt" || fail "a resolved incident is still in the brief"

echo "kyb brief CLI tests passed"
