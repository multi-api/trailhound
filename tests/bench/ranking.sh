#!/bin/bash
# Loads the same 100,000 documents into Trailhound and Meilisearch and
# compares their rankings with ranking_check.py.
#   SEARCHABLE='["title","description"]' to rank with weighted attributes.
B=~/trailhound-bench
HERE=$(cd "$(dirname "$0")" && pwd)
KEY=bench-master-key-0123456789abcdef
SEARCHABLE=${SEARCHABLE:-'["*"]'}
cd $B/data
[ -f rank.ndjson ] || head -100000 docs.ndjson > rank.ndjson
docker rm -f bench-th bench-meili >/dev/null 2>&1; rm -rf $B/d-th $B/d-meili; mkdir -p $B/d-th $B/d-meili
docker run -d --name bench-th --user $(id -u):$(id -g) -p 127.0.0.1:7711:7700 -v $B/target/release/trailhound:/trailhound:ro -v $B/d-th:/data rust:1-bookworm \
  /trailhound --data-dir /data --addr 0.0.0.0:7700 --master-key $KEY >/dev/null
docker run -d --name bench-meili --user $(id -u):$(id -g) -p 127.0.0.1:7712:7700 -e MEILI_MASTER_KEY=$KEY -e MEILI_NO_ANALYTICS=true -e MEILI_ENV=production \
  -v $B/d-meili:/meili_data getmeili/meilisearch:v1.58.0 >/dev/null
SETTINGS="{\"searchableAttributes\":$SEARCHABLE,\"filterableAttributes\":[\"id\",\"category\",\"price\",\"stock\"],\"sortableAttributes\":[\"price\",\"created\"],\"pagination\":{\"maxTotalHits\":1000000}}"
for u in http://127.0.0.1:7711 http://127.0.0.1:7712; do
  for i in $(seq 1 100); do curl -sf $u/health >/dev/null && break; sleep 0.1; done
  curl -s -o /dev/null -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' -X POST $u/indexes -d '{"uid":"bench","primaryKey":"id"}'
  curl -s -o /dev/null -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' -X PATCH $u/indexes/bench/settings -d "$SETTINGS"
  curl -s -o /dev/null -H "Authorization: Bearer $KEY" -H 'Content-Type: application/x-ndjson' -X POST $u/indexes/bench/documents --data-binary @rank.ndjson
  while :; do case "$(curl -s -H "Authorization: Bearer $KEY" "$u/tasks?statuses=enqueued,processing&limit=1")" in *'"results":[]'*) break;; esac; sleep 0.2; done
done
echo "searchableAttributes: $SEARCHABLE"
python3 $HERE/ranking_check.py http://127.0.0.1:7711 http://127.0.0.1:7712 $KEY
docker rm -f bench-th bench-meili >/dev/null 2>&1; rm -rf $B/d-th $B/d-meili
