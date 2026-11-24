#!/bin/bash
# Multi-tenant load: 8 customers with their own index, many clients asking at once.
# Trailhound: 8 tenants, each with its key and an index `bench`.
# Meilisearch: one instance, 8 indexes (it has no tenants of its own).
#   ENGINES="th meili"  CPUS=0-3 (server cores)  MEM=512m  LEVELS="8 32 128"  SECONDS_PER_LEVEL=10
B=~/trailhound-bench
HERE=$(cd "$(dirname "$0")" && pwd)
KEY=bench-master-key-0123456789abcdef
ADMIN=bench-admin-key-0123456789abcdef
TENANTS=${TENANTS:-8}
LEVELS=${LEVELS:-"8 32 128"}
DUR=${SECONDS_PER_LEVEL:-10}
cd $B/data
# Portable across Linux and macOS: no GNU split, no %N in date, no host /proc.
[ -f mt-00 ] || python3 -c "
import sys
n = int(sys.argv[1]); lines = open('docs.ndjson', 'rb').read().splitlines(keepends=True)
for i in range(n):
    open('mt-%02d' % i, 'wb').writelines(lines[i * len(lines) // n:(i + 1) * len(lines) // n])
" $TENANTS
now() { python3 -c 'import time; print("%.3f" % time.time())'; }

url() { [ "$1" = th ] && echo http://127.0.0.1:7711 || echo http://127.0.0.1:7712; }
port() { [ "$1" = th ] && echo 7711 || echo 7712; }
proc() { [ "$1" = th ] && echo trailhound || echo meilisearch; }
# Read from inside the container, where /proc exists on every host.
rss() { docker exec bench-$1 sh -c 'cat /proc/[0-9]*/status 2>/dev/null' | awk -v name=$(proc $1) '$1 == "Name:" {on = ($2 == name)} on && /VmRSS|RssAnon/ {printf "%s%dMB ", $1, $2/1024}'; }
cpu() { docker exec bench-$1 sh -c 'cat /proc/[0-9]*/stat 2>/dev/null' | awk -v name="($(proc $1))" '$2 == name {print ($14 + $15) / 100; exit}'; }   # seconds of CPU used so far
disk() { if [ -n "$INTERNAL_DATA" ]; then docker exec bench-$1 du -sm $([ $1 = th ] && echo /data || echo /meili_data) | cut -f1; else du -sm $B/d-$1 | cut -f1; fi; }
wait_idle() {
  while :; do
    r=$(curl -s -H "Authorization: Bearer $2" "$(url $1)/tasks?statuses=enqueued,processing&limit=1")
    [ -z "$r" ] && { echo " SERVER DEAD"; return; }
    case "$r" in *'"results":[]'*) return;; esac
    sleep 0.05
  done
}
SETTINGS='{"filterableAttributes":["category","price","stock"],"sortableAttributes":["price","created"],"pagination":{"maxTotalHits":1000000}}'

for e in ${ENGINES:-th meili}; do
  echo "=== $e  cpus=${CPUS:-all} mem=${MEM:-none} tenants=$TENANTS"
  docker rm -f bench-$e >/dev/null 2>&1; rm -rf $B/d-$e; mkdir -p $B/d-$e
  # INTERNAL_DATA=1 keeps the index inside the container: on macOS a shared
  # folder is slow enough to distort both engines.
  if [ -n "$INTERNAL_DATA" ]; then user=""; vth=""; vmeili=""; else user="--user $(id -u):$(id -g)"; vth="-v $B/d-th:/data"; vmeili="-v $B/d-meili:/meili_data"; fi
  lim=""; [ -n "$MEM" ] && lim="--memory $MEM --memory-swap $MEM"; [ -n "$CPUS" ] && lim="$lim --cpuset-cpus $CPUS"
  if [ $e = th ]; then
    docker run -d --name bench-th $lim $user -p 127.0.0.1:7711:7700 $TH_ENV -v $B/target/release/trailhound:/trailhound:ro $vth rust:1-bookworm \
      /trailhound --data-dir /data --addr 0.0.0.0:7700 --admin-key $ADMIN >/dev/null
  else
    docker run -d --name bench-meili $lim $user -p 127.0.0.1:7712:7700 -e MEILI_MASTER_KEY=$KEY -e MEILI_NO_ANALYTICS=true -e MEILI_ENV=production $MEILI_EXTRA \
      $vmeili getmeili/meilisearch:v1.58.0 >/dev/null
  fi
  for i in $(seq 1 100); do curl -sf $(url $e)/health >/dev/null && break; sleep 0.1; done

  : > $B/targets-$e
  t0=$(now)
  for n in $(seq 0 $((TENANTS - 1))); do
    part=$(printf 'mt-%02d' $n)
    if [ $e = th ]; then
      k=$(curl -s -H "Authorization: Bearer $ADMIN" -H 'Content-Type: application/json' -X POST $(url $e)/_admin/api/tenants -d "{\"name\":\"cliente $n\"}" | python3 -c 'import sys,json; print(json.load(sys.stdin)["masterKey"])')
      uid=bench
    else
      k=$KEY; uid=t$n
    fi
    echo "$k $uid" >> $B/targets-$e
    curl -s -o /dev/null -H "Authorization: Bearer $k" -H 'Content-Type: application/json' -X POST $(url $e)/indexes -d "{\"uid\":\"$uid\",\"primaryKey\":\"id\"}"
    curl -s -o /dev/null -H "Authorization: Bearer $k" -H 'Content-Type: application/json' -X PATCH $(url $e)/indexes/$uid/settings -d "$SETTINGS"
    curl -s -o /dev/null -H "Authorization: Bearer $k" -H 'Content-Type: application/x-ndjson' -X POST $(url $e)/indexes/$uid/documents --data-binary @$part
  done
  while read k uid; do wait_idle $e $k; done < $B/targets-$e
  echo "  load of $TENANTS tenants: $(echo "$(now) - $t0" | bc) s   disk: $(disk $e) MB   $(rss $e)"
  read k uid < $B/targets-$e
  echo "  docs in first tenant: $(curl -s -H "Authorization: Bearer $k" $(url $e)/indexes/$uid/stats | grep -o '"numberOfDocuments":[0-9]*')"

  for c in $LEVELS; do
    c0=$(cpu $e)
    line=$(python3 $HERE/load.py 127.0.0.1 $(port $e) $B/targets-$e $c $DUR)
    c1=$(cpu $e)
    echo "  $line cpu=$(echo "$c1 - $c0" | bc)s  $(rss $e)"
  done
  sleep 2; echo "  two seconds later: $(rss $e)"
  docker rm -f bench-$e >/dev/null 2>&1; rm -rf $B/d-$e
done
