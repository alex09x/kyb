#!/usr/bin/env bash
# The CLI against a server that asks for tokens: it sends the token it was given,
# says what the server said when it is refused, and keeps the token out of the
# process list and out of anything it prints.
#
#   scripts/test-cli-auth.sh            (needs target/release/kyb-server or KYB_SERVER_BIN)
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cli="$repo_root/skills/kyb/bin/kyb"
bin="${KYB_SERVER_BIN:-$repo_root/target/release/kyb-server}"
[ -x "$bin" ] || { echo "no server binary at $bin"; exit 2; }
tmp="$(mktemp -d -t kyb-cli-auth.XXXXXX)"
port="${KYB_CLI_AUTH_PORT:-9391}"
fails=0

cleanup() { [ -n "${pid:-}" ] && kill "$pid" 2>/dev/null; wait "${pid:-}" 2>/dev/null; rm -rf "$tmp"; }
trap cleanup EXIT

ok()   { echo "  ok   $1"; }
fail() { echo "  FAIL $1 - $2"; fails=$((fails + 1)); }
expect() { case "$2" in *"$3"*) ok "$1" ;; *) fail "$1" "wanted '$3' in: $2" ;; esac; }
refuse() { case "$2" in *"$3"*) fail "$1" "found '$3' in: $2" ;; *) ok "$1" ;; esac; }

export KYB_AUTH_FILE="$tmp/auth.json"
t_read="$("$bin" auth add --id reader --name "Rita Reader" --role read | head -1)"
t_write="$("$bin" auth add --id agent-w --name "Agent W" --role write --label ci | head -1)"
expect "the book lists principals without their tokens" "$("$bin" auth list)" "agent-w  write  Agent W"
refuse "the book never prints a token" "$("$bin" auth list)" "$t_write"
refuse "the stored book holds no token" "$(cat "$KYB_AUTH_FILE")" "$t_write"
expect "a duplicate id is refused" "$("$bin" auth add --id reader --name X --role read 2>&1)" "already exists"

KYB_AUTH=required KYB_DATA="$tmp/data" KYB_INDEX="$tmp/index" KYB_AUDIT="$tmp/audit.jsonl" KYB_MODEL="$tmp/no-model" \
  KYB_ADDR="127.0.0.1:$port" "$bin" > "$tmp/server.log" 2>&1 &
pid=$!
for _ in $(seq 1 100); do curl -s -o /dev/null "http://127.0.0.1:$port/healthz" && break; sleep 0.1; done

export KYB_ADDR="127.0.0.1:$port" HOME="$tmp/home"
mkdir -p "$HOME"
unset KYB_TOKEN

expect "without a token a read is refused, and the CLI shows why" "$("$cli" tags 2>&1)" "a token is required"
expect "whoami works for anyone" "$("$cli" whoami 2>&1)" '"auth"'

expect "a made-up token is not saved" "$(echo "kyb_made-up" | "$cli" login 2>&1)" "does not know this token"
[ ! -e "$HOME/.config/kyb/token" ] && ok "nothing was written for it" || fail "nothing was written for it" "token file exists"

expect "login names who the token belongs to" "$(echo "$t_read" | "$cli" login 2>&1)" "Rita Reader"
perm="$(stat -f '%Lp' "$HOME/.config/kyb/token" 2>/dev/null || stat -c '%a' "$HOME/.config/kyb/token")"
[ "$perm" = "600" ] && ok "the token file is owner-only" || fail "the token file is owner-only" "mode $perm"
expect "a reader reads" "$("$cli" tags 2>&1)" '"tags"'
expect "a reader's write is refused with the role it needs" "$(echo body | "$cli" add --key cli-note --title T 2>&1)" "needs the write role"

expect "KYB_TOKEN overrides the file" "$(echo "Body from the CLI." | KYB_TOKEN="$t_write" "$cli" add --key cli-note --title "CLI note" 2>&1)" '"changed"'
expect "the commit is the writer's" "$("$cli" history cli-note 2>&1)" "Agent W"
expect "retracting knowledge is an admin act" "$(KYB_TOKEN="$t_write" "$cli" rm cli-note 2>&1)" "admin"

# the token must not be visible to other users of the machine while a request is in flight
( KYB_TOKEN="$t_write" KYB_ADDR="192.0.2.1:9" "$cli" tags >/dev/null 2>&1 & )
sleep 0.5
refuse "the token is not on any command line" "$(ps -axww -o args= | grep -v 'grep' || true)" "$t_write"
pkill -f "192.0.2.1:9" 2>/dev/null

# debugging with a shell trace must not print it either
trace="$(KYB_TOKEN="$t_write" bash -x "$cli" tags 2>&1)"
refuse "a shell trace does not print the token" "$trace" "$t_write"
expect "and says why tracing went quiet" "$trace" "shell tracing is off"
expect "the same holds for the token file" "$(bash -x "$cli" whoami 2>&1 | grep -c "$t_read")" "0"

# KYB_URL names the server whole, scheme included (how a server behind TLS is reached)
expect "KYB_URL is used as given" "$(KYB_ADDR=192.0.2.1:9 KYB_URL="http://127.0.0.1:$port/" KYB_TOKEN="$t_write" "$cli" whoami 2>&1)" "Agent W"
expect "an https URL is passed to curl as https" "$(KYB_URL="https://127.0.0.1:$port" "$cli" whoami 2>&1)" "cannot reach 127.0.0.1:$port"

mode="$(stat -f '%Lp' "$KYB_AUTH_FILE" 2>/dev/null || stat -c '%a' "$KYB_AUTH_FILE")"
[ "$mode" = "600" ] && ok "the book is owner-only" || fail "the book is owner-only" "mode $mode"
expect "the server says at start what is in force" "$(cat "$tmp/server.log")" "auth required - 2 principals"
expect "a reader cannot read the request log" "$("$cli" tags >/dev/null 2>&1; curl -s -H "authorization: Bearer $t_read" "http://127.0.0.1:$port/api/audit")" "needs the admin role"

# two commands at once: both tokens end up in the book
"$bin" auth add --id par-a --name A --role read >"$tmp/par-a" 2>&1 & apid=$!
"$bin" auth add --id par-b --name B --role read >"$tmp/par-b" 2>&1; wait "$apid"   # not a bare wait: that would wait for the server too
expect "two overlapping adds both land" "$("$bin" auth list | grep -c '^par-')" "2"
[ -z "$(ls "$tmp"/auth.json.*.tmp 2>/dev/null)" ] && ok "no temp file is left" || fail "no temp file is left" "$(ls "$tmp")"
# the book configured through a link into the canon is refused too
mkdir -p "$tmp/data/kyb-data/sub"; ln -s "$tmp/data/kyb-data/sub" "$tmp/linked"
ln -s "$tmp/not-yet/kyb-data/sub" "$tmp/dangling"   # the canon does not exist until the server creates it
expect "a book behind a dangling symlink into a canon not yet created refuses to start" "$(KYB_AUTH=required KYB_AUTH_FILE="$tmp/dangling/auth.json" KYB_DATA="$tmp/not-yet/kyb-data" KYB_INDEX="$tmp/idx4" KYB_AUDIT="$tmp/a4" KYB_MODEL="$tmp/no-model" KYB_ADDR=127.0.0.1:0 "$bin" 2>&1)" "inside the git canon"
[ ! -e "$tmp/not-yet/kyb-data/sub/auth.json" ] && ok "and wrote no book there" || fail "and wrote no book there" "book written into the canon"
expect "auth add refuses a book inside the canon" "$(KYB_AUTH_FILE="$tmp/data/kyb-data/auth.json" KYB_DATA="$tmp/data/kyb-data" "$bin" auth add --id x --name X --role read 2>&1)" "inside the git canon"
[ ! -e "$tmp/data/kyb-data/auth.json" ] && ok "and writes nothing there" || fail "and writes nothing there" "book written"
ln -s "$tmp/not-yet-2/kyb-data/sub" "$tmp/dangling2"   # canon not created yet
expect "auth add refuses a book behind a dangling link into the canon" "$(KYB_AUTH_FILE="$tmp/dangling2/auth.json" KYB_DATA="$tmp/not-yet-2/kyb-data" "$bin" auth add --id x --name X --role read 2>&1)" "inside the git canon"
[ ! -e "$tmp/not-yet-2" ] && ok "and creates nothing behind the link" || fail "and creates nothing behind the link" "$(ls -R "$tmp/not-yet-2")"
expect "a book behind a symlink into the canon refuses to start" "$(KYB_AUTH=required KYB_AUTH_FILE="$tmp/linked/auth.json" KYB_DATA="$tmp/data/kyb-data" KYB_INDEX="$tmp/idx3" KYB_AUDIT="$tmp/a3" KYB_MODEL="$tmp/no-model" KYB_ADDR=127.0.0.1:0 "$bin" 2>&1)" "inside the git canon"
expect "a book inside the canon refuses to start" "$(KYB_AUTH=required KYB_AUTH_FILE="$tmp/data/auth.json" KYB_DATA="$tmp/data" KYB_INDEX="$tmp/idx2" KYB_AUDIT="$tmp/a2" KYB_MODEL="$tmp/no-model" KYB_ADDR=127.0.0.1:0 "$bin" 2>&1)" "inside the git canon"

tok_id="$("$bin" auth list | awk '/ci$/ {print $1}')"
expect "revoking by token id" "$("$bin" auth revoke --token "$tok_id")" "revoked"
expect "a revoked token stops working without a restart" "$(KYB_TOKEN="$t_write" "$cli" tags 2>&1)" "not known here"
expect "logout forgets the token" "$("$cli" logout && "$cli" tags 2>&1)" "a token is required"
expect "the audit log names the caller" "$(cat "$tmp/audit.jsonl")" '"actor":"agent-w"'

if [ "$fails" -eq 0 ]; then echo "all CLI auth checks passed"; else echo "$fails failed"; exit 1; fi
