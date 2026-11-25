#!/bin/bash
# Same data, same requests, against Trailhound (:7711, or TH_PORT) and Meilisearch (:7712, or MEILI_PORT), both in containers.
#   DATASET=wiki-1m (see dataset.sh)  LINES=100000 (documents per batch)  MEM=512m  CPUS=0-1
#   COLD=1 empties the page cache before the restart, so start-up reads the index from disk.
HERE=$(cd "$(dirname "$0")" && pwd)
. $HERE/dataset.sh
KEY=bench-master-key-0123456789abcdef
H1="Authorization: Bearer $KEY"
MEM=${MEM:-}          # optional docker memory limit, e.g. MEM=256m
OUT=$B/out; mkdir -p $OUT
cd $B/data
LINES=${LINES:-100000}; PFX=$DATASET.p$LINES-
[ -f ${PFX}aaa ] || split -a 3 -l $LINES $DOCS $PFX

now() { date +%s.%N; }
url() { [ "$1" = th ] && echo http://127.0.0.1:$TH_PORT || echo http://127.0.0.1:$MEILI_PORT; }
proc() { [ "$1" = th ] && echo trailhound || echo meilisearch; }

start() {
  docker rm -f bench-$1 >/dev/null 2>&1
  local lim=""; [ -n "$MEM" ] && lim="--memory $MEM --memory-swap $MEM"
  [ -n "$CPUS" ] && lim="$lim --cpuset-cpus $CPUS"          # e.g. CPUS=0-1
  # Meilisearch sizes its indexer from the host, not from the container limit.
  local tune=""; [ -n "$MEILI_INDEX_MEM" ] && tune="-e MEILI_MAX_INDEXING_MEMORY=$MEILI_INDEX_MEM"
  [ -n "$MEILI_INDEX_THREADS" ] && tune="$tune -e MEILI_MAX_INDEXING_THREADS=$MEILI_INDEX_THREADS"
  if [ "$1" = th ]; then
    docker run -d --name bench-th $NOFILE $lim --user $(id -u):$(id -g) -p 127.0.0.1:$TH_PORT:7700 \
      $TH_ENV -v $TH_BIN:/trailhound:ro -v $B/d-th:/data rust:1-bookworm \
      /trailhound --data-dir /data --addr 0.0.0.0:7700 --master-key $KEY >/dev/null
  else
    docker run -d --name bench-meili $NOFILE $lim --user $(id -u):$(id -g) -p 127.0.0.1:$MEILI_PORT:7700 \
      -e MEILI_MASTER_KEY=$KEY -e MEILI_NO_ANALYTICS=true -e MEILI_ENV=production $tune \
      -v $B/d-meili:/meili_data getmeili/meilisearch:v1.58.0 >/dev/null
  fi
  for i in $(seq 1 6000); do curl -sf $(url $1)/health >/dev/null && return; sleep 0.02; done
  echo "$1 did not start"; docker logs bench-$1 | tail -5
}

# Host pid of the engine process inside the container.
pid() { for p in $(docker top bench-$1 -o pid | tail -n +2); do [ "$(cat /proc/$p/comm 2>/dev/null)" = "$(proc $1)" ] && echo $p && return; done; }
mem() { # label engine
  local p=$(pid $2)
  awk -v l="$1" '/VmHWM|VmRSS|RssAnon|RssFile/ {printf "%s=%dMB ", $1, $2/1024} END {print ""}' /proc/$p/status | sed "s/^/  $1: /"
}
wait_idle() { # engine: wait until no task is enqueued or processing
  while :; do
    local r=$(curl -s -H "$H1" "$(url $1)/tasks?statuses=enqueued,processing&limit=1")
    [ -z "$r" ] && { echo " SERVER DEAD ($(docker inspect -f 'status={{.State.Status}} killed-for-memory={{.State.OOMKilled}}' bench-$1))"; return; }
    case "$r" in *'"results":[]'*) return;; esac
    [ $(date +%s) -lt $DEADLINE ] || { echo " GAVE UP after ${INDEX_TIMEOUT}s, tasks still running"; return; }
    sleep 0.05
  done
}
DEADLINE=$(($(date +%s) + ${INDEX_TIMEOUT:=86400}))   # INDEX_TIMEOUT=3600 stops waiting for indexing after an hour
post() { curl -s -o /dev/null -w '%{http_code} ' -H "$H1" -H "Content-Type: $2" -X ${5:-POST} "$(url $1)$3" --data-binary "$4"; }

index_phase() {
  local e=$1
  rm -rf $B/d-$e; mkdir -p $B/d-$e
  start $e
  post $e application/json /indexes '{"uid":"bench","primaryKey":"id"}'; wait_idle $e
  post $e application/json /indexes/bench/settings "$SETTINGS" PATCH
  wait_idle $e
  local t0=$(now)
  for f in ${PFX}*; do post $e application/x-ndjson /indexes/bench/documents @$f; done
  wait_idle $e
  local t1=$(now)
  echo; echo "  indexing wall: $(echo "$t1 - $t0" | bc) s  ($(ls ${PFX}* | wc -l) batches of $LINES)"
  curl -s -H "$H1" "$(url $e)/tasks?statuses=failed&limit=3" | head -c 300; echo
  curl -s -H "$H1" "$(url $e)/indexes/bench/stats" | head -c 200; echo
  mem "after indexing (HWM = peak)" $e
  sleep 2
  echo "  disk right after: $(du -sm $B/d-$e | cut -f1) MB"
  check_count $e $(wc -l < $DOCS) "$(url $e)/indexes/bench/stats" $KEY
}

QUERIES=(
'{"q":"motor"}'
'{"q":"motor búsqueda"}'
'{"q":"servidr rapdo"}'
'{"q":"compre"}'
'{"q":"\"precio calidad\""}'
'{"q":"portátil solar eléctrico manual","matchingStrategy":"all"}'
'{"q":"casa 12345"}'
'{"q":"","filter":"category = Libros AND price < 100","sort":["price:asc"]}'
'{"q":"garantía","facets":["category"],"filter":"stock > 400"}'
'{"q":"descuento envío","sort":["created:desc"]}'
)

[ -z "$QUERIES_FILE" ] || mapfile -t QUERIES < <(python3 -c '
import json, sys
for q in json.load(open(sys.argv[1], encoding="utf-8")):
    q.pop("kind", None); print(json.dumps(q, ensure_ascii=False))' $QUERIES_FILE)

query_phase() {
  local e=$1
  docker rm -f bench-$e >/dev/null 2>&1
  [ -z "$COLD" ] || drop_caches
  local t0=$(now)
  start $e
  echo "  start-up after a restart${COLD:+ (cold page cache)}: $(echo "$(now) - $t0" | bc) s until /health answers"
  sleep 1
  mem "idle after restart" $e
  local i=0; : > $OUT/$e-medians
  for q in "${QUERIES[@]}"; do
    local body=$(echo "$q" | sed 's/^{/{"limit":10,"attributesToRetrieve":["id"],/')
    local ts=()
    for n in $(seq 1 15); do
      ts+=($(curl -s -o $OUT/$e-$i.json -w '%{time_total}' -H "$H1" -H 'Content-Type: application/json' -X POST "$(url $e)/indexes/bench/search" -d "$body"))
    done
    local med=$(printf '%s\n' "${ts[@]}" | sort -n | sed -n 8p)
    printf '  q%d wall-median=%.1fms first=%.1fms  %s\n' $i $(echo "$med*1000" | bc) $(echo "${ts[0]}*1000" | bc) "$q"
    echo "$med" >> $OUT/$e-medians
    i=$((i+1))
  done
  sort -n $OUT/$e-medians | awk '{a[NR] = $1 * 1000} END {printf "  medians of %d queries: min=%.1fms median=%.1fms p90=%.1fms max=%.1fms\n", NR, a[1], a[int((NR + 1) / 2)], a[int(NR * 0.9 + 0.5)], a[NR]}'
  sleep 1.5  # freed memory is handed back within a second of the last search
  mem "after queries" $e
  echo "  disk: $(du -sm $B/d-$e | cut -f1) MB"
}

for e in ${ENGINES:-th meili}; do
  echo "=== $e ${MEM:+(limit $MEM)}"
  [ "$SKIP_INDEX" = 1 ] || index_phase $e || continue
  query_phase $e
done
[ -n "$KEEP" ] || docker rm -f bench-th bench-meili >/dev/null 2>&1
