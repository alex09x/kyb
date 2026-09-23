#!/usr/bin/env bash
# Hit /mcp on a RUNNING kyb-server.
#
# The unit tests in src/mcp.rs build their own router, so they stay green even if
# main() never wires /mcp into the app it actually serves. Only a request to the
# real process can tell those two apart, which is exactly the mistake this script
# exists to catch.
#
#   scripts/test-mcp-endpoint.sh [base-url]        (default http://127.0.0.1:9310)
set -euo pipefail

base="${1:-http://127.0.0.1:9310}"

rpc() {
  curl -sf -X POST "$base/mcp${2:-}" -H 'content-type: application/json' -d "$1"
}

rpc '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' \
  | python3 -c '
import json, sys
info = json.load(sys.stdin)["result"]["serverInfo"]
assert info["name"] == "kyb", info
print("initialize: %s %s" % (info["name"], info["version"]))
'

rpc '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | python3 -c '
import json, sys
names = [t["name"] for t in json.load(sys.stdin)["result"]["tools"]]
assert "kyb_query" in names, names
# retracting an entry and rebuilding the index are not agent-reachable
assert "kyb_rm" not in names and "kyb_reindex" not in names, names
assert len(names) == len(set(names)), names
print("tools/list: %d tools" % len(names))
'

rpc '{"jsonrpc":"2.0","id":3,"method":"tools/list"}' '?readonly=1' | python3 -c '
import json, sys
names = [t["name"] for t in json.load(sys.stdin)["result"]["tools"]]
for write in ("kyb_add", "kyb_incident", "kyb_task", "kyb_done", "kyb_resolve",
              "kyb_task_status"):
    assert write not in names, "%s advertised on a read-only registration" % write
print("readonly: %d tools, no writes" % len(names))
'

# a read that changes nothing, to prove a tool call reaches a real route
rpc '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"kyb_tags","arguments":{}}}' \
  | python3 -c '
import json, sys
result = json.load(sys.stdin)["result"]
assert not result.get("isError"), result
json.loads(result["content"][0]["text"])   # the route answered JSON, not prose
print("tools/call: kyb_tags answered")
'

status=$(curl -s -o /dev/null -w '%{http_code}' "$base/mcp")
[ "$status" = "405" ] || { echo "GET /mcp should be 405, got $status" >&2; exit 1; }
echo "GET /mcp: 405 as expected"

echo "mcp endpoint: all good"
