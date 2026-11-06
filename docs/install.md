# Install KYB on this machine (instructions for an AI coding agent)

You are an AI coding agent (Claude Code, Codex, Antigravity or similar) and the user
asked you to install KYB (Know Your Business): a self-hosted, git-backed knowledge base
that AI agents share. Follow these steps in order. Run every command yourself, and show
the user what each one printed. Stop and ask the user whenever a step says so.

## 1. Find out what the installer will do

```bash
curl -fsSL https://kybmemory.com/install.sh | KYB_DRY_RUN=1 bash
```

It prints one `plan:` line and changes nothing. Tell the user what it means:

- `server=brew`: Homebrew on Apple Silicon (`brew install alex09x/apps/kyb`, then a brew service).
- `server=native`: macOS without Homebrew. The release archive is checked against its SHA-256
  and runs as a launchd agent; the data lives in `~/kyb/data`.
- `server=docker`: the `ghcr.io/alex09x/kyb` image via Docker Compose in `~/kyb`, data in `~/kyb/data`.
- `server=existing`: a KYB server already answers on port 9310; it is left alone.
- `server=client`: only the CLI and the skill, for a server on another machine.

If it printed an error instead (for example "Docker is installed but not usable here"), relay
it, say what the user has to do, and stop.

If the user already runs a KYB server on another machine, ask for its host, and use client mode
in step 2.

## 2. Install

Ask the user to confirm the plan, then run one of these:

```bash
curl -fsSL https://kybmemory.com/install.sh | bash
# or, for a server elsewhere:
curl -fsSL https://kybmemory.com/install.sh | KYB_MODE=client KYB_SERVER=<host> bash
```

The installer writes to these places, and nothing else:

- `~/.local/bin/kyb` and `~/.local/bin/kyb-hook`;
- `~/.config/kyb/host`;
- the skill in `~/.claude/skills/kyb`, `~/.codex/skills/kyb` and `~/.gemini/config/skills/kyb`, for
  each agent present;
- a marked kyb section in `~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md` and `~/.gemini/GEMINI.md`. It
  keeps a backup of each file as `<file>.bak-kyb`;
- a PreToolUse hook in `~/.claude/settings.json`;
- the server: a brew service, the launchd agent `com.kybmemory.kyb-server`, or a container in `~/kyb`.

It does not use sudo. If it fails, show the last lines of output and stop. Do not try a different
install method on your own.

## 3. Verify

```bash
export PATH="$HOME/.local/bin:$PATH"
kyb health
printf 'Installed by an agent on %s.\n' "$(date -u +%F)" | kyb add --key kyb-install-check --title "KYB install check"
kyb query "install check"
```

`kyb health` must print JSON whose `ok` is `true`, and the query must return `kyb-install-check`.
Then check the skill for each agent:

```bash
for f in ~/.claude/skills/kyb/SKILL.md ~/.codex/skills/kyb/SKILL.md ~/.gemini/config/skills/kyb/SKILL.md; do
  [ -f "$f" ] && echo "skill: $f"
done
grep -l 'kyb:begin' ~/.claude/CLAUDE.md ~/.codex/AGENTS.md ~/.gemini/GEMINI.md 2>/dev/null
```

## 4. Report to the user

Tell the user:

- which plan ran and which version is installed (`kyb health` prints `version`);
- where the data lives;
- which agents got the skill;
- that agents load the skill in their next session, so you and any other agent should be
  restarted to use `kyb`;
- how to update: run the same `curl ... | bash` again, which upgrades in place and keeps the data;
- the web console: http://127.0.0.1:9310 on the machine that runs the server.

Never print or store tokens or passwords. KYB starts with auth off and listens on 127.0.0.1
only. Before exposing it to a network, read the Access section of
https://github.com/alex09x/kyb#access-tokens-and-roles.
