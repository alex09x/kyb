#!/usr/bin/env bash
# Installs the kyb skill for every agent on this machine.
# This repo is the source of truth; agents only get copies.
#   CLI         -> ~/.local/bin/kyb            (single copy, referenced via PATH)
#   Claude Code -> ~/.claude/skills/kyb/SKILL.md         + pointer in ~/.claude/CLAUDE.md
#   Codex       -> ~/.codex/skills/kyb/SKILL.md          + pointer in ~/.codex/AGENTS.md
#   Antigravity -> ~/.gemini/config/skills/kyb/SKILL.md  + pointer in ~/.gemini/GEMINI.md
#
# The pointer goes into the always-loaded global instructions: a skill the agent
# never opens is a skill it never uses. Sections are delimited by kyb:begin/kyb:end
# markers, so re-running updates them in place instead of appending copies.
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# A Homebrew post-install step runs from a different working tree than a normal checkout of this repo.
cli_bin="${KYB_INSTALL_BINARY:-$SRC/kyb/bin/kyb}"
install -d "$HOME/.local/bin"
install -m 755 "$cli_bin" "$HOME/.local/bin/kyb"
echo "✓ CLI      ~/.local/bin/kyb"

# The server address is deployment-specific and never lives in this repo:
# the deployer passes KYB_SERVER (from its own gitignored fleet config) and
# the CLI reads the host file at run time.
if [ -n "${KYB_SERVER:-}" ]; then
  install -d "$HOME/.config/kyb"
  printf '%s\n' "$KYB_SERVER" > "$HOME/.config/kyb/host"
  echo "✓ HOST     ~/.config/kyb/host -> $KYB_SERVER"
fi

for agent in "$HOME/.claude/skills" "$HOME/.codex/skills" "$HOME/.gemini/config/skills"; do
  if [ -d "$(dirname "$agent")" ]; then
    install -d "$agent/kyb"
    install -m 644 "$SRC/kyb/SKILL.md" "$agent/kyb/SKILL.md"
    # the old layout bundled bin/ per agent; the CLI now comes from PATH
    rm -rf "$agent/kyb/bin"
    echo "✓ SKILL    $agent/kyb/SKILL.md"
  fi
done

inject() { # inject <file> <manual-path>
  local file="$1" manual="$2"
  [ -f "$file" ] || return 0
  [ -f "$file.bak-kyb" ] || cp "$file" "$file.bak-kyb"
  python3 - "$file" "$manual" <<'PY'
import sys, re, pathlib
path, manual = sys.argv[1], sys.argv[2]
begin, end = "<!-- kyb:begin -->", "<!-- kyb:end -->"
section = f"""{begin}
## Know Your Business (KYB) — our infrastructure knowledge base

We keep a shared, searchable knowledge base about our own systems: which servers exist and what
runs on them, how services are wired and who calls whom, deploys, ports, configs, and the
decisions we made and why. It is stored in git with full version history, it is shared by every
agent on every machine, and it is reached through the `kyb` CLI.

- **Query it before reasoning about our infrastructure.** Do not guess about our hosts, ports,
  services or deploys — `kyb query "..."` first. One phrasing is not a search; try a couple.
- **Write back what you learn.** Found nothing, or only a thin entry while you now understand
  the architecture? Enrich the base: `kyb add --key <slug> --title "..." [--tags a,b]` with the
  body on stdin. Updating is the same command with the same key.
- **Entries keep one canonical language (English)**; ask in any language — hybrid search
  bridges the query side, but a base written in two languages fragments and half of it goes
  silently missing.
- Only verified facts (read the code, saw the config, ran the command). Never secrets in the
  body — pointers in `--refs` instead.
- Old versions stay searchable: `kyb query "..." --history` tells you what moved where.
- **Incident reports.** Something broke or degraded — file it: `kyb incident --key inc-<date>-<slug>
  --title "..." --service <svc> --severity low|medium|high|critical [--knowledge key1,key2]` with
  what happened / impact / workaround on stdin. Before infra work check `kyb incidents`.
  Close with the outcome: `kyb resolve <key> <<< "what fixed it"` — a resolution is required;
  closing archives the report (still searchable, `kyb incidents --all`).
- **Tasks and ideas** — short actionable notes: `kyb task --key task-<slug> --title "..."`
  (body on stdin), list open ones with `kyb tasks`, close with `kyb done <key> <<< "outcome"`.
- Full manual — data model, response fields, naming conventions: {manual}
{end}"""
p = pathlib.Path(path)
text = p.read_text()
pattern = re.compile(re.escape(begin) + r".*?" + re.escape(end), re.S)
text = pattern.sub(section, text) if pattern.search(text) else text.rstrip() + "\n\n" + section + "\n"
p.write_text(text)
PY
  echo "✓ POINTER  $file"
}

# shellcheck disable=SC2088
inject "$HOME/.claude/CLAUDE.md" "~/.claude/skills/kyb/SKILL.md"
# shellcheck disable=SC2088
inject "$HOME/.codex/AGENTS.md"  "~/.codex/skills/kyb/SKILL.md"
# shellcheck disable=SC2088
inject "$HOME/.gemini/GEMINI.md" "~/.gemini/config/skills/kyb/SKILL.md"

echo
"$HOME/.local/bin/kyb" health >/dev/null 2>&1 \
  && echo "kyb is responding — ready" \
  || echo "warning: kyb health did not answer (is the server reachable?)"
