# Sourced by bench.sh, multitenant.sh and ranking.sh: which documents, settings
# and queries a run uses.
#   DATASET=docs      (default) the synthetic documents of gen.js
#   DATASET=wiki-1m   or wiki-all: Wikipedia, written by wiki.sh
B=${B:-~/trailhound-bench}
DATASET=${DATASET:-docs}
DOCS=$B/data/$DATASET.ndjson
case $DATASET in
  wiki*)
    SEARCHABLE=${SEARCHABLE:-'["title","text"]'}
    FILTERABLE='"length"'
    SORTABLE='"length","title"'
    QUERIES_FILE=${QUERIES_FILE:-$B/data/$DATASET.queries.json} ;;
  *)
    SEARCHABLE=${SEARCHABLE:-'["*"]'}
    FILTERABLE='"category","price","stock"'
    SORTABLE='"price","created"'
    QUERIES_FILE=${QUERIES_FILE:-} ;;   # empty: the queries built into the scripts
esac
[ -f $DOCS ] || { echo "$DOCS is missing (gen.js or wiki.sh writes it)"; exit 1; }
SETTINGS="{\"searchableAttributes\":$SEARCHABLE,\"filterableAttributes\":[$FILTERABLE],\"sortableAttributes\":[$SORTABLE],\"pagination\":{\"maxTotalHits\":1000000}}"
# The same, with `id` filterable, which ranking_check.py needs.
SETTINGS_WITH_ID="{\"searchableAttributes\":$SEARCHABLE,\"filterableAttributes\":[\"id\",$FILTERABLE],\"sortableAttributes\":[$SORTABLE],\"pagination\":{\"maxTotalHits\":1000000}}"
# Empties the page cache, so that the next start reads the index from disk.
drop_caches() { docker run --rm --privileged alpine sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'; }
# The Trailhound binary under test, built for Linux.
TH_BIN=${TH_BIN:-$B/target/release/trailhound}
# Host ports of the two servers.
TH_PORT=${TH_PORT:-7711}; MEILI_PORT=${MEILI_PORT:-7712}
# Both servers get the same limit on open files. Docker's default of 1024 is
# too low for Meilisearch on a few million documents: its indexing fails with
# "No file descriptors available".
NOFILE="--ulimit nofile=65536:65536"
# Stops the run when an index does not hold every document it was sent.
check_count() { # label, documents sent, stats url, key
  local have=$(curl -s -H "Authorization: Bearer $4" "$3" | grep -o '"numberOfDocuments":[0-9]*' | cut -d: -f2)
  [ "$have" = "$2" ] && return 0
  echo "  $1 holds ${have:-no} documents of $2; failed tasks: $(curl -s -H "Authorization: Bearer $4" "${3%/indexes/*}/tasks?statuses=failed&limit=1" | head -c 400)"
  return 1
}
