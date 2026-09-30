#!/usr/bin/env bash
# Run kyb-server locally against local kyb-data
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ ! -d "$REPO/kyb-data/.git" ]; then
  echo "Local kyb-data not found. Running sync first..."
  "$REPO/scripts/sync-local-data.sh"
fi

mkdir -p "$REPO/index"
touch "$REPO/audit.jsonl"

export KYB_DATA="$REPO/kyb-data"
export KYB_INDEX="$REPO/index"
export KYB_AUDIT="$REPO/audit.jsonl"
export KYB_ADDR="${KYB_ADDR:-127.0.0.1:9310}"
export KYB_UI_PATH="$REPO/src/web/index.html"

echo "============================================================"
echo "  🚀 Starting local kyb-server"
echo "  • Canon Data:    $KYB_DATA"
echo "  • Index:         $KYB_INDEX"
echo "  • UI HTML:       $KYB_UI_PATH"
echo "  • Address:       http://$KYB_ADDR"
echo "============================================================"

cd "$REPO"
exec cargo run --bin kyb-server
