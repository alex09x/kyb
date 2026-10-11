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

  # The image the run built is tagged with the full commit SHA; `latest` is whatever
  # finished last. The server pulls the SHA tag and the Compose it installs names
  # that image's manifest digest: what was tested is what runs, and a later build
  # cannot replace it. (Resolved on the server: it is the one with registry access.)
  image="ghcr.io/alex09x/kyb"

  step "server: resolve the digest, record what runs now, install the pinned compose, restart"
  short_sha=${sha:0:7}
  scp -q -o ConnectTimeout=10 -o BatchMode=yes "$REPO/docker-compose.yml" "$SERVER:~/kyb/docker-compose.yml.new"
  digest=$(ssh -o ConnectTimeout=10 -o BatchMode=yes "$SERVER" bash -s -- "$short_sha" "$image" "$sha" <<'REMOTE'
set -euo pipefail
short_sha=$1; image=$2; sha=$3
cd "$HOME/kyb"
trap 'rm -f docker-compose.yml.new' EXIT
docker pull -q "$image:$sha" >&2
digest=$(docker image inspect --format '{{join .RepoDigests "\n"}}' "$image:$sha" | grep "^$image@" | head -1 | sed "s|^$image@||")
[ -n "$digest" ] || { echo "no pullable digest for $image:$sha" >&2; exit 1; }
sed -i "s|^\( *image: \).*|\1$image@$digest|" docker-compose.yml.new
grep -q "image: $image@$digest" docker-compose.yml.new || { echo "could not pin the image in the compose" >&2; exit 1; }
docker compose -f docker-compose.yml.new config -q
# the bind comes from the host-local .env (owner's decision), never from the repo: show what will be published
docker compose -f docker-compose.yml.new config | grep -E "host_ip|published" | sed 's/^/  bind: /' >&2
# What is running, by pullable digest: the rollback compose names exactly that, not `latest`.
old_id=$(docker inspect --format '{{.Image}}' kyb 2>/dev/null || true)
old_digest=$(docker image inspect --format '{{join .RepoDigests "\n"}}' "$old_id" 2>/dev/null | grep "^$image@" | head -1 || true)
echo "running before: id=${old_id:-none} pullable=${old_digest:-none}" >&2
if [ -f docker-compose.yml ]; then
  if [ -n "$old_digest" ]; then
    sed "s|^\( *image: \).*|\1$old_digest|" docker-compose.yml > "docker-compose.yml.rollback-pre-$short_sha"
  else
    cp -p docker-compose.yml "docker-compose.yml.rollback-pre-$short_sha"
  fi
  echo "rollback: cp docker-compose.yml.rollback-pre-$short_sha docker-compose.yml && docker compose up -d" >&2
fi
mv docker-compose.yml.new docker-compose.yml
trap - EXIT
docker compose up -d >&2
echo "$digest"
REMOTE
)
  [ -n "$digest" ] || { echo "deploy: the server did not report a digest" >&2; exit 1; }
  echo "pinned: $image@$digest"

  # A cold start reindexes every entry and may rebuild embeddings before the port
  # opens: minutes, not seconds. Wait with a deadline, and stop early if the
  # container dies - a crash loop must not read as "still starting".
  step "readiness: healthz, version, and the image that is actually running"
  KYB_URL="http://${SERVER#*@}:9310"
  deadline=$((SECONDS + 600)); last_note=0
  until curl -sf -m 3 "$KYB_URL/healthz" >/dev/null; do
    if [ $SECONDS -ge $deadline ]; then
      echo "deploy: not ready after 600s; container state and last log lines:" >&2
      ssh -o BatchMode=yes "$SERVER" 'cd ~/kyb && docker compose ps && docker compose logs --tail 20 kyb' >&2 || true
      exit 1
    fi
    if [ $((SECONDS - last_note)) -ge 30 ]; then
      last_note=$SECONDS
      state=$(ssh -o ConnectTimeout=10 -o BatchMode=yes "$SERVER" 'docker inspect --format "{{.State.Status}} restarts={{.RestartCount}}" kyb 2>/dev/null' || echo unknown)
      echo "  ${SECONDS}s: container $state"
      case "$state" in exited*|dead*) echo "deploy: the container died" >&2; ssh -o BatchMode=yes "$SERVER" 'cd ~/kyb && docker compose logs --tail 30 kyb' >&2 || true; exit 1;; esac
    fi
    sleep 3
  done
  health=$(curl -sf -m 5 "$KYB_URL/healthz")
  echo "$health" | head -c 300; echo
  echo "$health" | grep -q '"ok":true' || { echo "deploy: healthz is not ok" >&2; exit 1; }
  want_version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO/Cargo.toml" | head -1)
  echo "$health" | grep -q "\"version\":\"$want_version\"" || { echo "deploy: healthz version is not $want_version" >&2; exit 1; }
  running=$(ssh -o ConnectTimeout=10 -o BatchMode=yes "$SERVER" "docker image inspect --format '{{join .RepoDigests \"\\n\"}}' \$(docker inspect --format '{{.Image}}' kyb)" | grep "^$image@" | head -1 || true)
  [ "$running" = "$image@$digest" ] || { echo "deploy: running image is '$running', expected $image@$digest" >&2; exit 1; }
  echo "running: $running"
  ssh -o ConnectTimeout=10 -o BatchMode=yes "$SERVER" 'cd ~/kyb && docker compose logs --tail 200 kyb 2>/dev/null | grep -E "kyb: (ready|reindex|semantic|lexical|listening|auth)" | tail -6' || true
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
  elif ssh -o ConnectTimeout=8 -o BatchMode=yes "$h" 'bash -lc "kyb health 2>/dev/null"' | grep -q '"ok":true'; then
    echo "  $h  ok"; ok=$((ok+1))
  else
    echo "  $h  FAIL"; fail=$((fail+1))
  fi
done
echo
echo "deploy done: $ok ok, $fail failed, $skipped unreachable"
[ "$fail" -eq 0 ]
