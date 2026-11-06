#!/usr/bin/env bash
# After an install, checks that the kyb CLI and the agent skill landed where the
# agents look for them, for every agent whose home exists on this machine:
#   Claude Code  ~/.claude/skills/kyb/SKILL.md, the kyb section in ~/.claude/CLAUDE.md,
#                the attribution hook in ~/.claude/settings.json
#   Codex        ~/.codex/skills/kyb/SKILL.md, the kyb section in ~/.codex/AGENTS.md
#   Antigravity  ~/.gemini/config/skills/kyb/SKILL.md, the kyb section in ~/.gemini/GEMINI.md
# and that the CLI points at the expected server.
#
#   scripts/check-installed-skills.sh [expected-host]     (default 127.0.0.1)
set -euo pipefail

want_host="${1:-127.0.0.1}"
fail=0
pass() { echo "ok   $*"; }
bad()  { echo "FAIL $*"; fail=1; }

check_file() { if [ -s "$1" ]; then pass "$1"; else bad "$1 missing or empty"; fi; }
check_section() {
  if grep -q '<!-- kyb:begin -->' "$1" 2>/dev/null && grep -q '<!-- kyb:end -->' "$1"; then
    if [ "$(grep -c '<!-- kyb:begin -->' "$1")" = 1 ]; then pass "$1 has one kyb section"; else bad "$1 has more than one kyb section"; fi
  else
    bad "$1 has no kyb section"
  fi
}

for bin in kyb kyb-hook; do
  if [ -x "$HOME/.local/bin/$bin" ]; then pass "$HOME/.local/bin/$bin"; else bad "$HOME/.local/bin/$bin missing"; fi
done
host="$(cat "$HOME/.config/kyb/host" 2>/dev/null || true)"
if [ "$host" = "$want_host" ]; then pass "CLI points at $host"; else bad "CLI host file says '$host', want '$want_host'"; fi

agents=0
if [ -d "$HOME/.claude" ]; then
  agents=$((agents + 1))
  check_file "$HOME/.claude/skills/kyb/SKILL.md"
  [ -f "$HOME/.claude/CLAUDE.md" ] && check_section "$HOME/.claude/CLAUDE.md"
  if python3 - "$HOME/.claude/settings.json" "$HOME/.local/bin/kyb-hook" <<'PY'
import json, sys
settings = json.load(open(sys.argv[1]))
groups = settings.get("hooks", {}).get("PreToolUse", [])
mine = [g for g in groups if g.get("matcher") == "mcp__kyb__.*"
        and any(h.get("command") == sys.argv[2] for h in g.get("hooks", []))]
sys.exit(0 if len(mine) == 1 else 1)
PY
  then pass "Claude Code hook registered once"; else bad "Claude Code hook not registered (or twice)"; fi
fi
if [ -d "$HOME/.codex" ]; then
  agents=$((agents + 1))
  check_file "$HOME/.codex/skills/kyb/SKILL.md"
  [ -f "$HOME/.codex/AGENTS.md" ] && check_section "$HOME/.codex/AGENTS.md"
fi
if [ -d "$HOME/.gemini/config" ]; then
  agents=$((agents + 1))
  check_file "$HOME/.gemini/config/skills/kyb/SKILL.md"
  [ -f "$HOME/.gemini/GEMINI.md" ] && check_section "$HOME/.gemini/GEMINI.md"
fi
[ "$agents" -gt 0 ] || bad "no agent home (~/.claude, ~/.codex, ~/.gemini/config) to check"

[ "$fail" = 0 ] && echo "skills: installed for $agents agent(s)"
exit "$fail"
