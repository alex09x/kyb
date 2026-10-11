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

  # The run's docker-smoke job tested one manifest digest and recorded it as an
  # artifact. That digest - not `latest`, not the SHA tag, both of which a rebuild
  # can replace - is what the server pulls and what the installed Compose names.
  step "image: the digest this run smoke-tested"
  image="ghcr.io/alex09x/kyb"
  dl=$(mktemp -d -t kyb-digest.XXXXXX)
  gh run download "$run_id" -n image-digest -D "$dl" >/dev/null
  digest=$(tr -d '[:space:]' < "$dl/image-digest.txt")
  rm -rf "$dl"
  case "$digest" in sha256:*) ;; *) echo "deploy: run $run_id recorded no usable digest: '$digest'" >&2; exit 1;; esac
  echo "$image@$digest"

  step "server: record what runs now, install the pinned compose, restart"
  short_sha=${sha:0:7}
  scp -q -o ConnectTimeout=10 -o BatchMode=yes "$REPO/docker-compose.yml" "$SERVER:~/kyb/docker-compose.yml.new"
  post_up=$(ssh -o ConnectTimeout=10 -o BatchMode=yes "$SERVER" bash -s -- "$short_sha" "$image" "$digest" <<'REMOTE'
set -euo pipefail
short_sha=$1; image=$2; digest=$3
cd "$HOME/kyb"
trap 'rm -f docker-compose.yml.new' EXIT
docker pull -q "$image@$digest" >&2
sed -i "s|^\( *image: \).*|\1$image@$digest|" docker-compose.yml.new
grep -q "image: $image@$digest" docker-compose.yml.new || { echo "could not pin the image in the compose" >&2; exit 1; }
docker compose -f docker-compose.yml.new config -q
# the bind comes from the host-local .env (owner's decision), never from the repo: show what will be published
docker compose -f docker-compose.yml.new config | grep -E "host_ip|published" | sed 's/^/  bind: /' >&2
# What runs now, by pullable digest. The rollback Compose must name exactly that;
# a running service whose image cannot be named this way is not replaced at all.
old_id=$(docker inspect --format '{{.Image}}' kyb 2>/dev/null || true)
old_digest=$(docker image inspect --format '{{join .RepoDigests "\n"}}' "$old_id" 2>/dev/null | grep "^$image@" | head -1 || true)
echo "running before: id=${old_id:-none} pullable=${old_digest:-none}" >&2
if [ -n "$old_id" ] && [ -z "$old_digest" ]; then
  echo "the running image has no pullable digest for $image; a rollback could not name it - stopping before anything is replaced" >&2
  exit 1
fi
rollback="docker-compose.yml.rollback-pre-$short_sha"
if [ -f docker-compose.yml ] && [ -n "$old_digest" ]; then
  # the first attempt's backup is the one that names the last known-good image; a retry must not overwrite it
  if [ -f "$rollback" ]; then
    echo "rollback: $rollback already exists from an earlier attempt and is kept; this attempt's state goes to $rollback.$(date +%s)" >&2
    sed "s|^\( *image: \).*|\1$old_digest|" docker-compose.yml > "$rollback.$(date +%s)"
  else
    sed "s|^\( *image: \).*|\1$old_digest|" docker-compose.yml > "$rollback"
  fi
  echo "rollback: cp $rollback docker-compose.yml && docker compose up -d" >&2
elif [ -z "$old_id" ]; then
  echo "first install on this host: nothing to roll back to" >&2
fi
mv docker-compose.yml.new docker-compose.yml
trap - EXIT
docker compose up -d >&2
# the new container's identity and restart baseline, for the readiness watch
sleep 2
echo "$(docker inspect --format '{{.Id}} {{.RestartCount}}' kyb)"
REMOTE
)
  new_id=${post_up%% *}; base_restarts=${post_up##* }
  [ -n "$new_id" ] || { echo "deploy: the server did not report the new container" >&2; exit 1; }
  echo "container ${new_id:0:12} started (restarts=$base_restarts)"

  # A cold start reindexes every entry and may rebuild embeddings before the port
  # opens: minutes, not seconds. Wait with a deadline, and stop early if the
  # container dies - a crash loop must not read as "still starting".
  step "readiness: healthz, version, and the image that is actually running"
  KYB_URL="http://${SERVER#*@}:9310"
  deadline=$((SECONDS + 600)); last_note=-30
  until curl -sf -m 3 "$KYB_URL/healthz" >/dev/null; do
    if [ $SECONDS -ge $deadline ]; then
      echo "deploy: not ready after 600s; container state and last log lines:" >&2
      ssh -o BatchMode=yes "$SERVER" 'cd ~/kyb && docker compose ps && docker compose logs --tail 20 kyb' >&2 || true
      exit 1
    fi
    if [ $((SECONDS - last_note)) -ge 30 ]; then
      last_note=$SECONDS
      # a crash loop under restart:unless-stopped shows as "restarting" or as a growing restart count, not as "exited"
      st=$(ssh -o ConnectTimeout=10 -o BatchMode=yes "$SERVER" "docker inspect --format '{{.Id}} {{.State.Status}} {{.RestartCount}}' kyb 2>/dev/null" || echo "? unknown ?")
      set -- $st; cur_id=$1; state=$2; restarts=$3
      echo "  ${SECONDS}s: container $state restarts=$restarts"
      if [ "$cur_id" != "$new_id" ] || [ "$state" = restarting ] || [ "$state" = exited ] || [ "$state" = dead ] || { [ "$restarts" != "?" ] && [ "$restarts" -gt "$base_restarts" ]; }; then
        echo "deploy: the container is not coming up (state=$state, restarts $base_restarts -> $restarts); last log lines:" >&2
        ssh -o BatchMode=yes "$SERVER" 'cd ~/kyb && docker compose logs --tail 30 kyb' >&2 || true
        exit 1
      fi
    fi
    sleep 3
  done
  health=$(curl -sf -m 5 "$KYB_URL/healthz")
  want_version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO/Cargo.toml" | head -1)
  printf '%s' "$health" | python3 -c '
import json, sys
h = json.load(sys.stdin); want = sys.argv[1]
print(json.dumps({k: h.get(k) for k in ("ok", "version", "entries", "open_incidents", "open_tasks")}))
if h.get("ok") is not True: sys.exit("deploy: healthz ok is not true")
if h.get("version") != want: sys.exit("deploy: healthz version is %r, Cargo.toml says %r" % (h.get("version"), want))
' "$want_version"
  # the running container's image must be the pinned digest's image: compare image ids, not
  # the first of possibly several RepoDigests one image can carry
  ids=$(ssh -o ConnectTimeout=10 -o BatchMode=yes "$SERVER" "docker inspect --format '{{.Image}}' kyb; docker image inspect --format '{{.Id}}' '$image@$digest'")
  set -- $ids; running_id=$1; wanted_id=$2
  [ -n "$running_id" ] && [ "$running_id" = "$wanted_id" ] || { echo "deploy: running image id $running_id is not the pinned digest's id $wanted_id" >&2; exit 1; }
  echo "running: $image@$digest (image ${running_id:7:12})"
  # what the start cost, and whether the vector cache was reused rather than recomputed
  ssh -o ConnectTimeout=10 -o BatchMode=yes "$SERVER" 'cd ~/kyb && docker compose logs --tail 300 kyb 2>/dev/null | grep -E "kyb: (ready|reindex|semantic|lexical|listening|auth|vector cache|embeddings:|embedded)" | tail -8' || true
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
  elif ssh -o ConnectTimeout=8 -o BatchMode=yes "$h" 'bash -lc "kyb health 2>/dev/null"' | python3 -c 'import json,sys; sys.exit(0 if json.load(sys.stdin).get("ok") is True else 1)' 2>/dev/null; then
    echo "  $h  ok"; ok=$((ok+1))
  else
    echo "  $h  FAIL"; fail=$((fail+1))
  fi
done
echo
echo "deploy done: $ok ok, $fail failed, $skipped unreachable"
[ "$fail" -eq 0 ]
