#!/bin/bash
# Real text for the benchmarks: English Wikipedia, the 2023-11-01 snapshot that
# Wikimedia publishes on Hugging Face (CC BY-SA 3.0 and GFDL), pinned to one
# revision so that every run gets the same files.
#
#   wiki.sh 1m     7 of the 41 files, spread over the whole snapshot (2.2 GB):
#                  the first 1,000,000 articles in them  -> data/wiki-1m.ndjson
#   wiki.sh all    the 41 files (11.6 GB): 6,407,814 articles -> data/wiki-all.ndjson
#
# Each one also writes data/<name>.queries.json, derived from the documents with
# a fixed seed. Then: DATASET=wiki-1m tests/bench/bench.sh (or multitenant.sh,
# ranking.sh). MAX_CHARS (default 1000) is how much of each article is kept;
# 0 keeps whole articles, which the published figures do not cover.
#
# The downloads stay in ~/trailhound-bench/wiki/raw, away from any code, and are
# only read by wiki.py inside a container.
set -e
B=${B:-~/trailhound-bench}
HERE=$(cd "$(dirname "$0")" && pwd)
REV=b04c8d1ceb2f5cd4588862100d08de323dccfbaa
BASE=https://huggingface.co/datasets/wikimedia/wikipedia/resolve/$REV/20231101.en
LIST=https://huggingface.co/api/datasets/wikimedia/wikipedia/tree/$REV/20231101.en
RAW=$B/wiki/raw
SEED=${SEED:-42}

case "$1" in
  1m)   name=wiki-1m;   files="0 6 12 18 24 30 36"; limit=1000000; chars=${MAX_CHARS:-1000} ;;
  all)  name=wiki-all;  files=$(seq 0 40);          limit=0;       chars=${MAX_CHARS:-1000} ;;
  *) sed -n '2,15p' "$0"; exit 1 ;;
esac

mkdir -p $RAW $B/data
[ -f $RAW/list.json ] || curl -sfL -o $RAW/list.json $LIST
paths=""
for n in $files; do
  f=$(printf 'train-%05d-of-00041.parquet' $n)
  want=$(python3 -I -c 'import json, sys; print(next(e["lfs"]["oid"] for e in json.load(open(sys.argv[1])) if e["path"].endswith(sys.argv[2])))' $RAW/list.json $f)
  if [ "$(sha256sum $RAW/$f 2>/dev/null | cut -d' ' -f1)" != "$want" ]; then
    echo "downloading $f"
    curl -fL --retry 5 -C - -o $RAW/$f $BASE/$f
    [ "$(sha256sum $RAW/$f | cut -d' ' -f1)" = "$want" ] || { echo "$f: checksum mismatch"; exit 1; }
  fi
  paths="$paths /raw/$f"
done

if [ ! -f $B/data/$name.ndjson ]; then
  docker run --rm --user $(id -u):$(id -g) -e HOME=/tmp -w /tmp \
    -v $RAW:/raw:ro -v $B/data:/out -v $HERE/wiki.py:/wiki.py:ro python:3.12-slim sh -c "
      python -m venv /tmp/venv && /tmp/venv/bin/pip install -q --disable-pip-version-check pyarrow==18.1.0 &&
      /tmp/venv/bin/python -I /wiki.py convert /out/$name.ndjson.part $chars $limit $paths"
  mv $B/data/$name.ndjson.part $B/data/$name.ndjson
fi
python3 -I $HERE/wiki.py queries $B/data/$name.ndjson $B/data/$name.queries.json $SEED
ls -l $B/data/$name.ndjson $B/data/$name.queries.json
