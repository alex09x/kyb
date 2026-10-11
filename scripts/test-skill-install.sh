#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/kyb-skill-install.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT

make_home() {
  local home="$1"
  mkdir -p \
    "$home/.claude/skills" \
    "$home/.codex/skills" \
    "$home/.gemini/config/skills"
  : > "$home/.claude/CLAUDE.md"
  : > "$home/.codex/AGENTS.md"
  : > "$home/.gemini/GEMINI.md"
}

stub="$TEST_ROOT/kyb"
printf '%s\n' '#!/usr/bin/env bash' 'exit 0' > "$stub"
chmod 755 "$stub"

valid_home="$TEST_ROOT/valid-home"
make_home "$valid_home"
HOME="$valid_home" KYB_INSTALL_BINARY="$stub" bash "$REPO/skills/install.sh" >/dev/null

for installed in \
  "$valid_home/.claude/skills/kyb/SKILL.md" \
  "$valid_home/.codex/skills/kyb/SKILL.md" \
  "$valid_home/.gemini/config/skills/kyb/SKILL.md"; do
  cmp "$REPO/skills/kyb/SKILL.md" "$installed"
  python3 "$REPO/skills/validate-frontmatter.py" --require-yaml-parser "$installed" >/dev/null
done

# the Claude Code hook: installed, registered once, and other hooks left alone
[ -x "$valid_home/.local/bin/kyb-hook" ] || { echo "kyb-hook was not installed" >&2; exit 1; }
python3 - "$valid_home/.claude/settings.json" <<'PY'
import json, sys
pre = json.load(open(sys.argv[1]))["hooks"]["PreToolUse"]
mine = [h for h in pre if h.get("matcher") == "mcp__kyb__.*"]
assert len(mine) == 1, pre
assert mine[0]["hooks"][0]["command"].endswith("/.local/bin/kyb-hook"), mine
PY
# a settings file with somebody else's hook: ours is added beside it, and a second install changes nothing
python3 - "$valid_home/.claude/settings.json" <<'PY'
import json, sys
p = sys.argv[1]; s = json.load(open(p))
s["hooks"]["PreToolUse"].insert(0, {"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/bin/true"}]})
# ours moved by hand into a group with somebody else's handler, plus a look-alike that is not ours
mine = [h for h in s["hooks"]["PreToolUse"] if h["matcher"] == "mcp__kyb__.*"][0]
s["hooks"]["PreToolUse"] = [h for h in s["hooks"]["PreToolUse"] if h["matcher"] != "mcp__kyb__.*"]
s["hooks"]["PreToolUse"].append({"matcher": "mcp__.*", "hooks": [{"type": "command", "command": "/opt/other/kyb-hook-lookalike"}, mine["hooks"][0]]})
json.dump(s, open(p, "w"))
PY
HOME="$valid_home" KYB_INSTALL_BINARY="$stub" bash "$REPO/skills/install.sh" >/dev/null
python3 - "$valid_home/.claude/settings.json" <<'PY'
import json, sys
pre = json.load(open(sys.argv[1]))["hooks"]["PreToolUse"]
assert [h["matcher"] for h in pre] == ["Bash", "mcp__.*", "mcp__kyb__.*"], pre
assert [c["command"] for c in pre[1]["hooks"]] == ["/opt/other/kyb-hook-lookalike"], pre[1]
assert len(pre[2]["hooks"]) == 1, pre[2]
PY
# the hook itself: a kyb tool call gets its context, any other tool is left alone
out="$(printf '%s' '{"session_id":"s1","cwd":"/","tool_name":"mcp__kyb__kyb_add","tool_input":{"key":"k"}}' | python3 "$valid_home/.local/bin/kyb-hook")"
python3 - "$out" <<'PY'
import json, sys
o = json.loads(sys.argv[1])["hookSpecificOutput"]
assert o["updatedInput"]["key"] == "k" and o["updatedInput"]["_ctx"]["session"] == "s1" and o["updatedInput"]["_ctx"]["agent"] == "claude", o
assert "permissionDecision" not in o, "the hook must not decide permissions: " + str(o)
PY
[ -z "$(printf '%s' '{"tool_name":"Bash","tool_input":{}}' | python3 "$valid_home/.local/bin/kyb-hook")" ] || { echo "the hook touched a foreign tool" >&2; exit 1; }

cp -R "$REPO/skills" "$TEST_ROOT/invalid-skills"
python3 - "$TEST_ROOT/invalid-skills/kyb/SKILL.md" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
text = path.read_text()
start = text.index("description: >-")
end = text.index("\n---", start)
text = text[:start] + "description: invalid plain scalar with Triggers: broken\n" + text[end + 1:]
path.write_text(text)
PY

invalid_home="$TEST_ROOT/invalid-home"
make_home "$invalid_home"
if HOME="$invalid_home" KYB_INSTALL_BINARY="$stub" \
    bash "$TEST_ROOT/invalid-skills/install.sh" >/dev/null 2>&1; then
  echo "invalid frontmatter unexpectedly installed" >&2
  exit 1
fi
[ ! -e "$invalid_home/.local/bin/kyb" ] || {
  echo "installer mutated HOME before rejecting invalid frontmatter" >&2
  exit 1
}

echo "skill installer frontmatter tests passed"
