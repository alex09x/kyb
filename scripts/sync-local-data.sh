#!/usr/bin/env bash
# Sync kyb-data and audit log from remote server to local workspace
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ -f "$REPO/scripts/fleet.local.sh" ]; then
  # shellcheck source=/dev/null
  . "$REPO/scripts/fleet.local.sh"
else
  echo "Error: scripts/fleet.local.sh not found — define SERVER there" >&2
  exit 1
fi

echo "== Syncing kyb-data from $SERVER..."
mkdir -p "$REPO/kyb-data"
rsync -avz --delete "$SERVER:~/kyb/data/kyb-data/" "$REPO/kyb-data/"

if [ "${1:-}" == "--all" ]; then
  echo "== Syncing audit.jsonl..."
  rsync -avz "$SERVER:~/kyb/data/audit.jsonl" "$REPO/audit.jsonl"
fi

echo "== Local data sync complete!"
echo "   Canon Git repository: $REPO/kyb-data"
