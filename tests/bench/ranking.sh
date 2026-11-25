#!/bin/bash
# Loads the same documents (the first 100,000, or RANK_DOCS) into Trailhound and
# Meilisearch and compares their rankings with ranking_check.py.
#   SEARCHABLE='["title","description"]' to rank with weighted attributes.
#   DATASET=wiki-1m RANK_DOCS=1000000 for Wikipedia and its queries (see dataset.sh).
#   KEEP=1 leaves both servers running, to look into a difference.
HERE=$(cd "$(dirname "$0")" && pwd)
. $HERE/dataset.sh
KEY=bench-master-key-0123456789abcdef
RANK_DOCS=${RANK_DOCS:-100000}; LINES=${LINES:-100000}; PFX=$DATASET.rank$RANK_DOCS.$LINES-
cd $B/data
[ -f ${PFX}aaa ] || head -$RANK_DOCS $DOCS | split -a 3 -l $LINES - $PFX
docker rm -f bench-th bench-meili >/dev/null 2>&1; rm -rf $B/d-th $B/d-meili; mkdir -p $B/d-th $B/d-meili
docker run -d --name bench-th $NOFILE --user $(id -u):$(id -g) -p 127.0.0.1:$TH_PORT:7700 -v $TH_BIN:/trailhound:ro -v $B/d-th:/data rust:1-bookworm \
  /trailhound --data-dir /data --addr 0.0.0.0:7700 --master-key $KEY >/dev/null
docker run -d --name bench-meili $NOFILE --user $(id -u):$(id -g) -p 127.0.0.1:$MEILI_PORT:7700 -e MEILI_MASTER_KEY=$KEY -e MEILI_NO_ANALYTICS=true -e MEILI_ENV=production \
  -v $B/d-meili:/meili_data getmeili/meilisearch:v1.58.0 >/dev/null
for u in http://127.0.0.1:$TH_PORT http://127.0.0.1:$MEILI_PORT; do
  for i in $(seq 1 100); do curl -sf $u/health >/dev/null && break; sleep 0.1; done
  curl -s -o /dev/null -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' -X POST $u/indexes -d '{"uid":"bench","primaryKey":"id"}'
  curl -s -o /dev/null -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' -X PATCH $u/indexes/bench/settings -d "$SETTINGS_WITH_ID"
  for f in ${PFX}*; do curl -s -o /dev/null -H "Authorization: Bearer $KEY" -H 'Content-Type: application/x-ndjson' -X POST $u/indexes/bench/documents --data-binary @$f; done
  while :; do case "$(curl -s -H "Authorization: Bearer $KEY" "$u/tasks?statuses=enqueued,processing&limit=1")" in *'"results":[]'*) break;; esac; sleep 0.2; done
done
echo "searchableAttributes: $SEARCHABLE"
python3 $HERE/ranking_check.py http://127.0.0.1:$TH_PORT http://127.0.0.1:$MEILI_PORT $KEY bench $QUERIES_FILE
[ -n "$KEEP" ] || { docker rm -f bench-th bench-meili >/dev/null 2>&1; rm -rf $B/d-th $B/d-meili; }
