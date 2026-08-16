#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

assert_bind() {
  local expected="$1"
  python3 -c '
import json
import sys

expected = sys.argv[1]
ports = json.load(sys.stdin)["services"]["kyb"]["ports"]
matches = [p for p in ports if str(p["target"]) == "9310" and str(p["published"]) == "9310"]
if len(matches) != 1 or matches[0].get("host_ip") != expected:
    raise SystemExit(f"expected one {expected}:9310:9310 mapping, got {ports!r}")
' "$expected"
}

unset KYB_PUBLISH_ADDR
docker compose -f "$REPO/docker-compose.yml" config --format json | assert_bind 127.0.0.1
KYB_PUBLISH_ADDR=10.0.0.10 \
  docker compose -f "$REPO/docker-compose.yml" config --format json | assert_bind 10.0.0.10

echo "Docker Compose bind tests passed"
