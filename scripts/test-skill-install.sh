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
