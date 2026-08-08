#!/usr/bin/env bash
# Search quality harness: paraphrase questions that deliberately share few or
# no tokens with the entries they should land on, paired with the keys a human
# would expect. Reports top-1 / top-3 hit rate, so a change to the model, the
# analyzer or the fusion weights is judged by measurement instead of a hunch.
#
# The cases below are examples — replace them with real queries against your
# own base. Queries in any language work: the vector side is multilingual.
#
#   scripts/eval-search.sh [addr] [--lexical]
#   scripts/eval-search.sh 127.0.0.1:9310            # hybrid (default)
#   scripts/eval-search.sh 127.0.0.1:9310 --lexical  # lexical baseline
set -euo pipefail

ADDR="${1:-127.0.0.1:9310}"
MODE="${2:-}"
EXTRA=""
[ "$MODE" = "--lexical" ] && EXTRA="&semantic=false"

# query | acceptable keys (any one counts as a hit)
CASES=(
  "why does the build fail at link time|onnx-runtime-glibc"
  "which database holds the documents|orders-api-architecture,host-a-services"
  "which signals does the daemon watch|web-app-daemon-watch,web-app-architecture"
  "how do we ship the web app|web-app-deploy"
  "what kind of server is atlas|atlas-server"
  "how to roll a new image to the server|atlas-deploy,kyb-architecture,kyb-agents-install"
  "where do we keep our knowledge|kyb-architecture,kyb-conventions"
  "digging up old versions of entries|kyb-architecture,kyb-conventions"
  "anomalies in incoming messages|web-app-daemon-watch,web-app-architecture"
  "naming rules for keys|kyb-conventions"
  "where does the agent skill get installed|kyb-agents-install"
  "full text search over court cases|case-search-deploy"
  "message bus streams|nats-streams"
  "how is data replicated between hosts|host-a-services"
)

enc() { python3 -c 'import sys,urllib.parse; print(urllib.parse.quote(sys.argv[1]))' "$1"; }

top1=0; top3=0; total=0
for case in "${CASES[@]}"; do
  q="${case%%|*}"; want="${case##*|}"
  hits=$(curl -s "http://$ADDR/search?q=$(enc "$q")&limit=3$EXTRA" | jq -r '.hits[].key' | tr '\n' ' ')
  first=$(echo "$hits" | awk '{print $1}')
  total=$((total + 1))
  mark="MISS"
  for w in ${want//,/ }; do
    if [ "$first" = "$w" ]; then top1=$((top1 + 1)); top3=$((top3 + 1)); mark="top1"; break; fi
  done
  if [ "$mark" = "MISS" ]; then
    for w in ${want//,/ }; do
      case " $hits " in *" $w "*) top3=$((top3 + 1)); mark="top3"; break;; esac
    done
  fi
  printf "%-6s %-38s -> %s\n" "$mark" "$q" "${hits:-<none>}"
done
echo "---"
echo "top1: $top1/$total   top3: $top3/$total   ($ADDR ${MODE:-hybrid})"
