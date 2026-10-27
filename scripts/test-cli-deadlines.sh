#!/usr/bin/env bash
# Copyright (c) 2026 Alexander Panasenko
# Contact: alex@prod.codes
# Author: https://prod.codes/about/
# Project: https://github.com/alex09x/kyb
# SPDX-License-Identifier: MIT
# Deadlines through the CLI against a real server: the banner every read prints
# on stderr while stdout stays the server's JSON, --no-deadlines, a rewrite that
# keeps the deadline it does not mention, a moved due date in `kyb diff`, the
# `kyb deadlines` listing, and a closed task gone from all of it.
#
# "Today" is pinned with KYB_TODAY, the date the CLI sends as X-KYB-Today.
#
#   cargo build --release --bin kyb-server && scripts/test-cli-deadlines.sh
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cli="$repo_root/skills/kyb/bin/kyb"
server="${KYB_SERVER:-$repo_root/target/release/kyb-server}"
port="${KYB_DEADLINES_PORT:-9393}"
work="$(mktemp -d -t kyb-deadlines-test.XXXXXX)"

cleanup() {
  if [ -n "${server_pid:-}" ]; then kill "$server_pid" 2>/dev/null; wait "$server_pid" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT

KYB_DATA="$work/data" KYB_INDEX="$work/index" KYB_AUDIT="$work/audit.jsonl" KYB_MODEL="$work/no-model" \
  KYB_ADDR="127.0.0.1:$port" KYB_AUTH=off "$server" > "$work/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do curl -sf "http://127.0.0.1:$port/healthz" >/dev/null && break; sleep 0.1; done

export KYB_URL="http://127.0.0.1:$port" HOME="$work/home" KYB_TODAY=2026-03-10
mkdir -p "$HOME"
fail() { echo "FAIL: $*" >&2; exit 1; }
json_has() { # json_has <file> <python expression over `j`>
  python3 -c 'import json,sys; j=json.load(open(sys.argv[1])); sys.exit(0 if eval(sys.argv[2]) else 1)' "$1" "$2"
}

echo "renew before expiry" | "$cli" task --key task-renew-cert --title "Renew the API certificate" \
  --due 2026-03-08 --hard --tags tls >/dev/null
echo "answer within ten days of the quote" | "$cli" task --key task-answer-quote --title "Answer the vendor quote" \
  --waits-on "the vendor's quote" --expected 2026-03-15 >/dev/null
echo "someday" | "$cli" task --key task-far --title "Far away" --due 2026-09-01 >/dev/null
echo "no date at all" | "$cli" task --key task-plain --title "Plain task" >/dev/null
echo "a fact" | "$cli" add --key some-fact --title "Some fact" >/dev/null

# every read: a banner on stderr, the server's JSON untouched on stdout
for read in "query fact" "get some-fact" "tasks" "incidents"; do
  # shellcheck disable=SC2086
  "$cli" $read > "$work/out.json" 2> "$work/err.txt" || fail "kyb $read failed: $(cat "$work/err.txt")"
  json_has "$work/out.json" 'j["deadlines"]["count"] == 2' || fail "kyb $read: stdout lacks the block: $(cat "$work/out.json")"
  grep -q "1 overdue, 1 due soon" "$work/err.txt" || fail "kyb $read: no banner: $(cat "$work/err.txt")"
  grep -Eq "2d late +HARD task-renew-cert" "$work/err.txt" || fail "kyb $read: banner line: $(cat "$work/err.txt")"
  grep -q "expected 2026-03-15, waits on the vendor's quote" "$work/err.txt" || fail "kyb $read: provisional line: $(cat "$work/err.txt")"
  ! grep -q "task-far" "$work/err.txt" || fail "kyb $read: a deadline outside its window was surfaced"
done

# --no-deadlines: no banner, and the server leaves the block off
"$cli" tasks --no-deadlines > "$work/out.json" 2> "$work/err.txt"
[ ! -s "$work/err.txt" ] || fail "--no-deadlines still printed: $(cat "$work/err.txt")"
json_has "$work/out.json" '"deadlines" not in j' || fail "--no-deadlines: the block is still in the answer"

# a rewrite that does not mention the deadline keeps it
echo "renew before expiry, through the CA portal" | "$cli" task --key task-renew-cert --title "Renew the API certificate" --tags tls >/dev/null
"$cli" get task-renew-cert --no-deadlines > "$work/out.json"
json_has "$work/out.json" 'j["due"] == "2026-03-08" and j["hard"] is True' || fail "a rewrite dropped the deadline: $(cat "$work/out.json")"

# moving only the date: a new version, visible in the diff, status untouched
"$cli" task-status task-renew-cert --due 2026-03-12 >/dev/null
"$cli" diff task-renew-cert > "$work/diff.json"
json_has "$work/diff.json" 'any(f["field"] == "due" and f["from"] == "2026-03-08" and f["to"] == "2026-03-12" for f in j["fields"])' \
  || fail "the moved due date is not in kyb diff: $(cat "$work/diff.json")"
"$cli" get task-renew-cert --no-deadlines > "$work/out.json"
json_has "$work/out.json" 'j["status"] == "open"' || fail "moving the date changed the status"

# the listing, as text and as JSON
"$cli" deadlines > "$work/list.txt"
grep -q "deadlines, today 2026-03-10: 3 open tasks with a date" "$work/list.txt" || fail "listing header: $(cat "$work/list.txt")"
grep -q "^THIS WEEK" "$work/list.txt" && grep -q "^LATER" "$work/list.txt" || fail "listing groups: $(cat "$work/list.txt")"
! grep -q "task-plain" "$work/list.txt" || fail "a task without a date is on the calendar"
"$cli" deadlines --within 7d --json > "$work/list.json"
json_has "$work/list.json" '[r["key"] for r in j["deadlines"]] == ["task-renew-cert", "task-answer-quote"]' \
  || fail "--within 7d: $(cat "$work/list.json")"
"$cli" deadlines --hard --json > "$work/list.json"
json_has "$work/list.json" '[r["key"] for r in j["deadlines"]] == ["task-renew-cert"]' || fail "--hard: $(cat "$work/list.json")"

# a malformed flag stops before anything is sent
if "$cli" task-status task-renew-cert --remind-days soon > /dev/null 2> "$work/err.txt"; then
  fail "--remind-days soon was accepted"
fi
grep -q "whole number of days" "$work/err.txt" || fail "--remind-days error: $(cat "$work/err.txt")"

# closing removes it from every deadline answer at once
"$cli" done task-renew-cert --resolution "renewed" >/dev/null
"$cli" tasks > /dev/null 2> "$work/err.txt"
! grep -q "task-renew-cert" "$work/err.txt" || fail "a closed task is still in the banner"
"$cli" deadlines --json > "$work/list.json"
json_has "$work/list.json" '"task-renew-cert" not in [r["key"] for r in j["deadlines"]]' || fail "a closed task is still listed"

echo "kyb deadlines: banner on four reads, --no-deadlines, a kept and a moved deadline, the listing, closing"
