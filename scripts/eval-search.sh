#!/usr/bin/env bash
# Search quality harness: Russian questions against an English base, with the
# entry a human would expect. Reports top-1 and top-3 hit rate so a change to
# the model, the analyzer or the fusion weights can be judged by measurement
# instead of by a hunch.
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
# Illustrative example set — swap these for real queries and keys from your own base.
CASES=(
  "почему сборка падает на линковке|onnx-runtime-glibc"
  "где живёт монга|orders-api-architecture,host-a-services"
  "какие сигналы отслеживает демон|web-app-daemon-watch,web-app-architecture"
  "как деплоим web app|web-app-deploy"
  "что за сервер booster|booster-server"
  "как обновить образ на сервере|booster-deploy,kyb-architecture,kyb-agents-install"
  "чем мы храним знания|kyb-architecture,kyb-conventions"
  "как искать по старым версиям|kyb-architecture,kyb-conventions"
  "аномалии в сообщениях|web-app-daemon-watch,web-app-architecture"
  "правила именования ключей|kyb-conventions"
  "куда ставится скилл|kyb-agents-install"
  "поиск по судебным делам|case-search-deploy"
  "нат стримы|nats-streams"
  "как реплицируются данные между хостами|host-a-services"
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
  printf "%-6s %-38s -> %s\n" "$mark" "$q" "${hits:-<пусто>}"
done
echo "---"
echo "top1: $top1/$total   top3: $top3/$total   ($ADDR ${MODE:-hybrid})"
