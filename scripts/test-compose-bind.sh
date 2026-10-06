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

# The auth settings must reach the container: Compose reads .env only for ${...},
# so a variable that is not named in the service never arrives and the server
# stays wide open while its operator believes otherwise.
assert_env() {
  python3 -c '
import json
import sys

want = dict(a.split("=", 1) for a in sys.argv[1:])
env = json.load(sys.stdin)["services"]["kyb"]["environment"]
bad = {k: (env.get(k), v) for k, v in want.items() if env.get(k) != v}
if bad:
    raise SystemExit(f"container environment differs (got, wanted): {bad!r}")
' "$@"
}

unset KYB_AUTH KYB_AUTH_FILE KYB_ANON
docker compose -f "$REPO/docker-compose.yml" config --format json | assert_env KYB_AUTH=off KYB_AUTH_FILE=/data/auth.json KYB_ANON=
KYB_AUTH=required KYB_ANON=read KYB_AUTH_FILE=/data/secrets/book.json \
  docker compose -f "$REPO/docker-compose.yml" config --format json | assert_env KYB_AUTH=required KYB_AUTH_FILE=/data/secrets/book.json KYB_ANON=read

echo "Docker Compose bind and auth tests passed"
