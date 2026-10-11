#!/usr/bin/env bash
# Fast UI deployment: assembles the UI from src/web/ and copies it to the server's /data volume.
# Takes < 1 second! The server immediately serves the updated UI on the next request.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ -f "$REPO/scripts/fleet.local.sh" ]; then
  # shellcheck source=/dev/null
  . "$REPO/scripts/fleet.local.sh"
else
  echo "Error: scripts/fleet.local.sh not found — define SERVER there" >&2
  exit 1
fi

OUT="$(mktemp -t kyb-ui.XXXXXX)"
trap 'rm -f "$OUT"' EXIT
python3 "$REPO/scripts/build_ui.py" "$OUT"
echo "== Uploading assembled UI to $SERVER:~/kyb/data/index.html..."
scp -q -o ConnectTimeout=10 -o BatchMode=yes \
  "$OUT" "$SERVER:~/kyb/data/index.html"

echo "✓ Deployed UI in ~0.5s! Changes are now live at http://$SERVER:9310"
