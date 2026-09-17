#!/usr/bin/env bash
# Refuse to publish the operator's own infrastructure.
#
# KYB is built by running it on a real fleet, so the temptation is to reach for a
# real host, a real address or a real incident whenever an example is needed. The
# repository is public: every tracked file here is published, and a recognisable
# example is a disclosure even when it carries no secret.
#
# It runs in CI over everything publishable - tracked files plus new, non-ignored
# ones - because a file about to be added is exactly where internal data appears.
# This is a gate against *introducing* internal data, not a cleanup pass: the rule
# is cheap to hold from the start and expensive to apply after the fact.
#
# An intentional exception goes in ALLOW below with a reason. An exception that
# cannot be justified in one line is not an exception.
#
# Rows are TAB-separated, never "|" - every pattern here is an ERE and most of
# them contain "|" themselves.
#
#   scripts/check-no-internal-data.sh
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 2

TAB=$'\t'

# <ERE><TAB><why it must never ship>
PATTERNS=(
  "(^|[^0-9.])10\.(25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.[0-9]{1,3}\.[0-9]{1,3}${TAB}private RFC1918 address (10/8)"
  "(^|[^0-9.])192\.168\.[0-9]{1,3}\.[0-9]{1,3}${TAB}private RFC1918 address (192.168/16)"
  "(^|[^0-9.])172\.(1[6-9]|2[0-9]|3[01])\.[0-9]{1,3}\.[0-9]{1,3}${TAB}private RFC1918 address (172.16/12)"
  # ".local" as a hostname suffix only. The tail rules out a filename segment
  # ("fleet.local.sh") and an identifier ("self.local_assets"); a real mDNS name
  # is followed by a port, quote, slash or end of line.
  "[A-Za-z0-9_-]+\.local([^._A-Za-z0-9-]|\$)${TAB}mDNS hostname of a machine on the operator network"
  "\bram[0-9]+\b${TAB}operator host name"
  "\b(sk|pk)-[A-Za-z0-9]{16,}${TAB}API key"
  "\bghp_[A-Za-z0-9]{20,}${TAB}GitHub personal access token"
  "\bxox[baprs]-[A-Za-z0-9-]{10,}${TAB}Slack token"
  "\bAKIA[0-9A-Z]{16}\b${TAB}AWS access key id"
  "BEGIN [A-Z ]*PRIVATE KEY${TAB}private key material"
  "\b(postgres|postgresql|mysql|mongodb(\+srv)?|redis|amqp|nats)://[^[:space:]\"'\`]*:[^[:space:]\"'\`@/]+@${TAB}connection string with an inline password"
  "Authorization: *Bearer +[A-Za-z0-9._-]{12,}${TAB}bearer token"
)

# <path><TAB><ERE the line must match><TAB><why this one is allowed>
ALLOW=(
  "README.md${TAB}10\.0\.0\.10${TAB}documentation placeholder for \"bind one exact interface\", not a real address"
  "scripts/test-compose-bind.sh${TAB}10\.0\.0\.10${TAB}the same placeholder, asserted by the bind test"
  "scripts/check-no-internal-data.sh${TAB}.${TAB}this file necessarily contains every pattern it looks for"
  "src/model.rs${TAB}ghp_|xox|AKIA|PRIVATE KEY${TAB}fixtures for find_secret, which must be tested against real token shapes"
  "src/main.rs${TAB}ghp_${TAB}fixtures for the API-level secret rejection tests"
)

allowed() { # allowed <file> <line-text>
  local file="$1" line="$2" entry rest path frag
  for entry in "${ALLOW[@]}"; do
    path="${entry%%"$TAB"*}"
    [ "$path" = "$file" ] || continue
    rest="${entry#*"$TAB"}"
    frag="${rest%%"$TAB"*}"
    printf '%s' "$line" | grep -Eq "$frag" && return 0
  done
  return 1
}

# Tracked files plus not-yet-tracked ones that are not gitignored: a brand new
# file is exactly where internal data gets introduced, and checking it only after
# the first commit means checking it too late.
#
# bash 3.2 on macOS has no mapfile, and this script also runs from a developer's
# machine, not only from CI.
FILES=()
while IFS= read -r tracked; do
  FILES+=("$tracked")
done < <(git ls-files --cached --others --exclude-standard | sort -u)
[ "${#FILES[@]}" -gt 0 ] || { echo "no publishable files"; exit 2; }

found=0
for entry in "${PATTERNS[@]}"; do
  pattern="${entry%%"$TAB"*}"
  why="${entry#*"$TAB"}"
  while IFS= read -r hit; do
    [ -n "$hit" ] || continue
    file="${hit%%:*}"
    rest="${hit#*:}"
    lineno="${rest%%:*}"
    text="${rest#*:}"
    allowed "$file" "$text" && continue
    if [ "$found" -eq 0 ]; then
      echo "Internal data must not be published. Found:"
      echo
    fi
    found=$((found + 1))
    printf '  %s:%s\n    %s\n    %s\n\n' \
      "$file" "$lineno" "$why" \
      "$(printf '%s' "$text" | sed 's/^[[:space:]]*//' | cut -c1-120)"
  done < <(grep -nEI "$pattern" "${FILES[@]}" 2>/dev/null)
done

if [ "$found" -gt 0 ]; then
  echo "$found finding(s). Replace it with an invented example, or add a justified"
  echo "entry to ALLOW in scripts/check-no-internal-data.sh."
  exit 1
fi

echo "No internal data in ${#FILES[@]} publishable files."
