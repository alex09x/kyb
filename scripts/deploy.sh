#!/usr/bin/env bash
# The full KYB deploy: push -> CI -> booster -> skill+CLI to every agent machine.
# This is what "deploy" means for this project. Idempotent — safe to rerun.
#
#   scripts/deploy.sh            # everything
#   scripts/deploy.sh --skills   # skip server build/deploy, only roll skills+CLI
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Machine-specific config lives OUTSIDE the repo: scripts/fleet.local.sh
# (gitignored) must define BOOSTER="user@host" and FLEET=("user@host" ...).
if [ -f "$REPO/scripts/fleet.local.sh" ]; then
  # shellcheck source=/dev/null
  . "$REPO/scripts/fleet.local.sh"
else
  echo "deploy: scripts/fleet.local.sh not found — define BOOSTER and FLEET there" >&2
  exit 1
fi

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

if [ "${1:-}" != "--skills" ]; then
  step "git: everything committed and pushed?"
  cd "$REPO"
  if [ -n "$(git status --porcelain -- src skills scripts Cargo.toml Cargo.lock Dockerfile docker-compose.yml README.md)" ]; then
    echo "deploy: uncommitted changes — commit first" >&2
    git status --short >&2
    exit 1
  fi
  git push
  sha=$(git rev-parse HEAD)

  step "CI for $sha"
  for _ in $(seq 1 30); do
    run_id=$(gh run list --commit "$sha" --limit 1 --json databaseId --jq '.[0].databaseId' 2>/dev/null || true)
    [ -n "$run_id" ] && break
    sleep 5
  done
  [ -n "${run_id:-}" ] || { echo "deploy: CI run for $sha never appeared" >&2; exit 1; }
  gh run watch "$run_id" --exit-status --interval 20 >/dev/null
  echo "CI green (run $run_id)"

  step "booster: pull + restart"
  ssh -o ConnectTimeout=10 -o BatchMode=yes "$BOOSTER" \
    'cd ~/kyb && docker compose pull -q && docker compose up -d'
  KYB_URL="http://${BOOSTER#*@}:9310"
  for _ in $(seq 1 40); do
    curl -sf -m 3 "$KYB_URL/healthz" >/dev/null && break
    sleep 1
  done
  curl -sf -m 3 "$KYB_URL/healthz" | head -c 200; echo
fi

# the CLI's server address comes from the fleet config, never from the repo
KYB_SERVER="${BOOSTER#*@}"

step "skills + CLI: this machine"
KYB_SERVER="$KYB_SERVER" bash "$REPO/skills/install.sh"

for h in "${FLEET[@]}"; do
  step "skills + CLI: $h"
  scp -q -r "$REPO/skills" "$h:/tmp/kyb-skills-rollout"
  # single quotes: several hosts run fish as the login shell
  ssh -o ConnectTimeout=8 -o BatchMode=yes "$h" \
    "bash -lc 'KYB_SERVER=$KYB_SERVER bash /tmp/kyb-skills-rollout/install.sh; rm -rf /tmp/kyb-skills-rollout'" \
    | grep -E '✓|ready|warning' || true
done

step "verify: kyb answers from every machine"
ok=0; fail=0
for h in "${FLEET[@]}"; do
  if ssh -o ConnectTimeout=8 -o BatchMode=yes "$h" 'bash -lc "kyb health >/dev/null 2>&1"'; then
    echo "  $h  ok"; ok=$((ok+1))
  else
    echo "  $h  FAIL"; fail=$((fail+1))
  fi
done
echo
echo "deploy done: $ok ok, $fail failed"
[ "$fail" -eq 0 ]
