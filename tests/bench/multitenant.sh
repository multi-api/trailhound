#!/bin/bash
# Multi-tenant load: 8 customers with their own index, many clients asking at once.
# Trailhound: 8 tenants, each with its key and an index `bench`.
# Meilisearch: one instance, 8 indexes (it has no tenants of its own).
#   ENGINES="th meili"  CPUS=0-3 (server cores)  MEM=512m  LEVELS="8 32 128"  SECONDS_PER_LEVEL=10
#   DATASET=wiki-1m (see dataset.sh)  TENANTS=1 for a single index  LOAD_TIMEOUT=3600 (seconds)
#   COLD=1 empties the page cache before the restart at the end.
HERE=$(cd "$(dirname "$0")" && pwd)
. $HERE/dataset.sh
KEY=bench-master-key-0123456789abcdef
ADMIN=bench-admin-key-0123456789abcdef
TENANTS=${TENANTS:-8}
LEVELS=${LEVELS:-"8 32 128"}
DUR=${SECONDS_PER_LEVEL:-10}
cd $B/data
# Portable across Linux and macOS: no GNU split, no %N in date, no host /proc.
# Tenant i gets the i-th slice of the file, in pieces of at most LINES documents.
LINES=${LINES:-100000}; PFX=$DATASET.mt$TENANTS.$LINES
[ -f $PFX-00-000 ] || python3 -c "
import sys
path, n, per, pfx = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
total = sum(1 for _ in open(path, 'rb'))
out, tenant, piece, written = None, -1, 0, 0
for i, line in enumerate(open(path, 'rb')):
    t = i * n // total
    if t != tenant or written == per:
        piece = piece + 1 if t == tenant else 0
        tenant, written = t, 0
        out = open('%s-%02d-%03d' % (pfx, tenant, piece), 'wb')
    out.write(line); written += 1
" $DOCS $TENANTS $LINES $PFX
now() { python3 -c 'import time; print("%.3f" % time.time())'; }

url() { [ "$1" = th ] && echo http://127.0.0.1:$TH_PORT || echo http://127.0.0.1:$MEILI_PORT; }
port() { [ "$1" = th ] && echo $TH_PORT || echo $MEILI_PORT; }
proc() { [ "$1" = th ] && echo trailhound || echo meilisearch; }
# Read from inside the container, where /proc exists on every host.
rss() { docker exec bench-$1 sh -c 'cat /proc/[0-9]*/status 2>/dev/null' | awk -v name=$(proc $1) '$1 == "Name:" {on = ($2 == name)} on && /VmRSS|RssAnon/ {printf "%s%dMB ", $1, $2/1024}'; }
cpu() { docker exec bench-$1 sh -c 'cat /proc/[0-9]*/stat 2>/dev/null' | awk -v name="($(proc $1))" '$2 == name {print ($14 + $15) / 100; exit}'; }   # seconds of CPU used so far
disk() { if [ -n "$INTERNAL_DATA" ]; then docker exec bench-$1 du -sm $([ $1 = th ] && echo /data || echo /meili_data) | cut -f1; else du -sm $B/d-$1 | cut -f1; fi; }
wait_idle() {
  while :; do
    r=$(curl -s -H "Authorization: Bearer $2" "$(url $1)/tasks?statuses=enqueued,processing&limit=1")
    [ -z "$r" ] && { echo "  SERVER DEAD ($(docker inspect -f 'status={{.State.Status}} killed-for-memory={{.State.OOMKilled}}' bench-$1))"; return 1; }
    case "$r" in *'"results":[]'*) return 0;; esac
    [ $(date +%s) -lt $DEADLINE ] || { echo "  GAVE UP after ${LOAD_TIMEOUT}s, tasks still running"; return 1; }
    sleep 0.05
  done
}
wait_health() { for i in $(seq 1 6000); do curl -sf $(url $1)/health >/dev/null && return; sleep 0.02; done; echo "  $1 did not start"; }
# Runs one level of load and prints its line, with the highest memory seen during it.
level() { # engine connections
  local c0=$(cpu $1) top=0 m
  python3 $HERE/load.py 127.0.0.1 $(port $1) $B/targets-$1 $2 $DUR $QUERIES_FILE > $B/level-$1 &
  while kill -0 $! 2>/dev/null; do
    m=$(rss $1 | sed -n 's/.*VmRSS:\([0-9]*\)MB.*/\1/p'); [ "${m:-0}" -gt $top ] && top=$m
    sleep 0.5
  done
  echo "  $(head -1 $B/level-$1) cpu=$(echo "$(cpu $1) - $c0" | bc)s  highest VmRSS=${top}MB  after: $(rss $1)"
  tail -n +2 $B/level-$1
}

for e in ${ENGINES:-th meili}; do
  echo "=== $e  cpus=${CPUS:-all} mem=${MEM:-none} tenants=$TENANTS"
  docker rm -f bench-$e >/dev/null 2>&1; rm -rf $B/d-$e; mkdir -p $B/d-$e
  # INTERNAL_DATA=1 keeps the index inside the container: on macOS a shared
  # folder is slow enough to distort both engines.
  if [ -n "$INTERNAL_DATA" ]; then user=""; vth=""; vmeili=""; else user="--user $(id -u):$(id -g)"; vth="-v $B/d-th:/data"; vmeili="-v $B/d-meili:/meili_data"; fi
  lim=""; [ -n "$MEM" ] && lim="--memory $MEM --memory-swap $MEM"; [ -n "$CPUS" ] && lim="$lim --cpuset-cpus $CPUS"
  if [ $e = th ]; then
    docker run -d --name bench-th $NOFILE $lim $user -p 127.0.0.1:$TH_PORT:7700 $TH_ENV -v $TH_BIN:/trailhound:ro $vth rust:1-bookworm \
      /trailhound --data-dir /data --addr 0.0.0.0:7700 --admin-key $ADMIN >/dev/null
  else
    docker run -d --name bench-meili $NOFILE $lim $user -p 127.0.0.1:$MEILI_PORT:7700 -e MEILI_MASTER_KEY=$KEY -e MEILI_NO_ANALYTICS=true -e MEILI_ENV=production $MEILI_EXTRA \
      $vmeili getmeili/meilisearch:v1.58.0 >/dev/null
  fi
  wait_health $e

  : > $B/targets-$e
  t0=$(now); DEADLINE=$(($(date +%s) + ${LOAD_TIMEOUT:=86400}))
  for n in $(seq 0 $((TENANTS - 1))); do
    if [ $e = th ]; then
      k=$(curl -s -H "Authorization: Bearer $ADMIN" -H 'Content-Type: application/json' -X POST $(url $e)/_admin/api/tenants -d "{\"name\":\"cliente $n\"}" | python3 -c 'import sys,json; print(json.load(sys.stdin)["masterKey"])')
      uid=bench
    else
      k=$KEY; uid=t$n
    fi
    echo "$k $uid" >> $B/targets-$e
    curl -s -o /dev/null -H "Authorization: Bearer $k" -H 'Content-Type: application/json' -X POST $(url $e)/indexes -d "{\"uid\":\"$uid\",\"primaryKey\":\"id\"}"
    curl -s -o /dev/null -H "Authorization: Bearer $k" -H 'Content-Type: application/json' -X PATCH $(url $e)/indexes/$uid/settings -d "$SETTINGS"
    for part in $(printf '%s-%02d-' $PFX $n)*; do
      curl -s -o /dev/null -H "Authorization: Bearer $k" -H 'Content-Type: application/x-ndjson' -X POST $(url $e)/indexes/$uid/documents --data-binary @$part
    done
  done
  ok=1; while read k uid; do wait_idle $e $k || { ok=0; break; }; done < $B/targets-$e
  echo "  load of $TENANTS tenants: $(echo "$(now) - $t0" | bc) s   disk: $(disk $e) MB   $(rss $e)"
  [ $ok = 1 ] || { docker rm -f bench-$e >/dev/null 2>&1; rm -rf $B/d-$e; continue; }
  read k uid < $B/targets-$e
  check_count "the first tenant of $e" $(cat $PFX-00-* | wc -l) "$(url $e)/indexes/$uid/stats" $k || { docker rm -f bench-$e >/dev/null 2>&1; rm -rf $B/d-$e; continue; }

  for c in $LEVELS; do level $e $c; done
  sleep 2; echo "  two seconds later: $(rss $e)"

  # Restart with the data in place: time until the server answers, memory at
  # rest, and the first search of every tenant.
  docker stop bench-$e >/dev/null; [ -z "$COLD" ] || drop_caches
  t0=$(now); docker start bench-$e >/dev/null; wait_health $e
  echo "  start-up after a restart${COLD:+ (cold page cache)}: $(echo "$(now) - $t0" | bc) s until /health answers"
  sleep 1; echo "  at rest after the restart: $(rss $e)"
  t0=$(now)
  while read k uid; do
    curl -s -o /dev/null -H "Authorization: Bearer $k" -H 'Content-Type: application/json' -X POST $(url $e)/indexes/$uid/search -d '{"q":"a","limit":1}'
  done < $B/targets-$e
  echo "  first search in each of the $TENANTS tenants: $(echo "$(now) - $t0" | bc) s in all   $(rss $e)"
  docker rm -f bench-$e >/dev/null 2>&1; rm -rf $B/d-$e
done
