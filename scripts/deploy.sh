#!/usr/bin/env bash
# The full KYB deploy: push -> CI -> server -> skill+CLI to every agent machine.
# This is what "deploy" means for this project. Idempotent — safe to rerun.
#
#   scripts/deploy.sh            # everything
#   scripts/deploy.sh --skills   # skip server build/deploy, only roll skills+CLI
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Machine-specific config lives OUTSIDE the repo: scripts/fleet.local.sh
# (gitignored) must define SERVER="user@host" and FLEET=("user@host" ...).
if [ -f "$REPO/scripts/fleet.local.sh" ]; then
  # shellcheck source=/dev/null
  . "$REPO/scripts/fleet.local.sh"
else
  echo "deploy: scripts/fleet.local.sh not found — define SERVER and FLEET there" >&2
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
    run_id=$(gh run list --workflow ci.yml --commit "$sha" --limit 1 --json databaseId --jq '.[0].databaseId' 2>/dev/null || true)
    [ -n "$run_id" ] && break
    sleep 5
  done
  [ -n "${run_id:-}" ] || { echo "deploy: CI run for $sha never appeared" >&2; exit 1; }
  gh run watch "$run_id" --exit-status --interval 20 >/dev/null
  echo "CI green (run $run_id)"

  step "server: install compose + pull + restart"
  short_sha=${sha:0:7}
  scp -q -o ConnectTimeout=10 -o BatchMode=yes \
    "$REPO/docker-compose.yml" "$SERVER:~/kyb/docker-compose.yml.new"
  ssh -o ConnectTimeout=10 -o BatchMode=yes "$SERVER" bash -s -- "$short_sha" <<'REMOTE'
set -euo pipefail
short_sha=$1
cd "$HOME/kyb"
trap 'rm -f docker-compose.yml.new' EXIT
docker compose -f docker-compose.yml.new config -q
if [ -f docker-compose.yml ]; then
  cp -p docker-compose.yml "docker-compose.yml.rollback-pre-$short_sha"
fi
mv docker-compose.yml.new docker-compose.yml
trap - EXIT
docker compose pull -q
docker compose up -d
REMOTE
  KYB_URL="http://${SERVER#*@}:9310"
  for _ in $(seq 1 40); do
    curl -sf -m 3 "$KYB_URL/healthz" >/dev/null && break
    sleep 1
  done
  curl -sf -m 3 "$KYB_URL/healthz" | head -c 200; echo
fi

# the CLI's server address comes from the fleet config, never from the repo
KYB_SERVER="${SERVER#*@}"

step "skills + CLI: this machine"
KYB_SERVER="$KYB_SERVER" bash "$REPO/skills/install.sh"

# a sleeping laptop must not abort the whole rollout — skip dead hosts
for h in "${FLEET[@]}"; do
  step "skills + CLI: $h"
  if ! scp -q -r -o ConnectTimeout=8 -o BatchMode=yes "$REPO/skills" "$h:/tmp/kyb-skills-rollout" 2>/dev/null; then
    echo "  skipped: $h is unreachable"
    continue
  fi
  # single quotes: several hosts run fish as the login shell
  ssh -o ConnectTimeout=8 -o BatchMode=yes "$h" \
    "bash -lc 'KYB_SERVER=$KYB_SERVER bash /tmp/kyb-skills-rollout/install.sh; rm -rf /tmp/kyb-skills-rollout'" \
    | grep -E '✓|ready|warning' || true
done

step "verify: kyb answers from every machine"
ok=0; fail=0; skipped=0
for h in "${FLEET[@]}"; do
  if ! ssh -o ConnectTimeout=8 -o BatchMode=yes "$h" true 2>/dev/null; then
    echo "  $h  SKIP (unreachable)"; skipped=$((skipped+1))
  elif ssh -o ConnectTimeout=8 -o BatchMode=yes "$h" 'bash -lc "kyb health >/dev/null 2>&1"'; then
    echo "  $h  ok"; ok=$((ok+1))
  else
    echo "  $h  FAIL"; fail=$((fail+1))
  fi
done
echo
echo "deploy done: $ok ok, $fail failed, $skipped unreachable"
[ "$fail" -eq 0 ]
