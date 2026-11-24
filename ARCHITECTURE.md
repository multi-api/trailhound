# Architecture

This document explains how Trailhound is put together, so that you can find where a change belongs. It names the files and functions involved rather than line numbers; search for the name.

- [The two crates](#the-two-crates)
- [The data folder](#the-data-folder)
- [An index: manifest and segments](#an-index-manifest-and-segments)
- [The segment file](#the-segment-file)
- [A write, from HTTP request to committed segment](#a-write-from-http-request-to-committed-segment)
- [A search](#a-search)
- [Merging](#merging)
- [Tenants, keys and quotas](#tenants-keys-and-quotas)
- [Memory](#memory)
- [Where to make a change](#where-to-make-a-change)
- [Tests](#tests)

## The two crates

```
crates/engine   trailhound-engine   a library: one index on disk, no HTTP, no tenants
  segment.rs      the segment file: builder, reader, merge
  index.rs        an index = manifest + segments; writes, deletions, commit, merge policy
  search.rs       query parsing, typo automaton, ranking, facets, hit formatting
  filter.rs       filter expressions: lexer, parser, evaluation against a segment
  tokenizer.rs    tokens, normalization, edit distance and typo counting
  settings.rs     index settings and attribute matching
  util.rs         varints, document sets, f64 keys, atomic file replacement

crates/server   trailhound          the binary
  main.rs         flags, router, background ticks, shutdown
  api.rs          the Meilisearch-compatible routes
  platform.rs     tenants, API keys, quotas, the task log and its workers
  admin.rs        the admin API and the embedded panel
  error.rs        errors in the Meilisearch shape

panel/          the admin panel (React); built into crates/server/src/panel.html
```

The engine knows nothing about tenants or HTTP. The server gives every tenant its own set of `Index` values and puts authentication, quotas and an asynchronous task queue in front of them.

## The data folder

```
data/
  platform.json              admin key; every tenant's id, name, master key, quotas, suspended flag
  tenants/<tenant id>/
    keys.json                the tenant's API keys
    tasks.log                one JSON line per task state change
    usage.json               search and indexing counters
    payloads/                uploaded documents waiting to be indexed, one document per line
    indexes/<uid>-<8 hex>/   one folder per index
      manifest.json          settings, field names, the list of segments and their deletions
      seg-00000001.seg       immutable segment files
```

Small JSON files are always replaced with `util::atomic_write`: write a `.tmp` file, `fsync`, rename over the old one, `fsync` the folder. After a crash a file holds its old or its new content, never part of each.

## An index: manifest and segments

`index::Index` is a folder with a `manifest.json` and some segment files.

- A **segment** is an immutable file with a batch of documents and everything needed to search them. Documents inside a segment are numbered from 0 (`docid`, a `u32` local to the segment).
- The **manifest** (`index::Manifest`) lists the segments in use and, for each, a bitmap of deleted docids (`SegMeta::deleted`, a base64 roaring bitmap). It also holds the settings, the primary key, the list of field names (a field's position in `fields` is its field id, `fid`) and `applied_task`, the highest task id whose effects it contains.

Nothing in a segment is ever modified. Replacing a document writes the new version into a new segment and sets a bit in the old segment's deletion bitmap; both happen in one manifest replacement.

In memory, `Index::state` is an `Arc<State>` (manifest, open segments, `Schema`). A reader takes a clone of the `Arc` with `Index::snapshot` and works on it for as long as it needs, so searches never wait for writes and never see half of one. A writer holds `Index::wlock`, builds new files, and calls `Index::commit`, which writes the manifest atomically and swaps the `Arc`. Segments that are no longer listed are marked with `Segment::mark_obsolete`; the file is removed when the last reader drops it (`segment::Deleter`).

`Index::open` deletes `.tmp` files and segment files the manifest does not list: they belong to a write that never committed.

`segment::Schema` maps flattened field names (`author.name`) to field ids and remembers which are searchable, filterable and sortable under the current settings (`F_SEARCH`, `F_FILTER`, `F_SORT`). Those three settings decide what goes into a segment, so changing one of them rebuilds the segments (`Settings::needs_reindex`, `Index::rewrite`).

## The segment file

Defined in `crates/engine/src/segment.rs`. Written by `SegmentBuilder` (new documents) and `merge` (existing segments), read by `Segment`.

```
MAGIC "THSEG001"                         8 bytes
section 0 .. section 9                   in the order of the table below
footer   10 × (offset u64, length u64, CRC32 u32), little-endian
         CRC32 of those 200 bytes        4 bytes
         MAGIC again                     8 bytes
```

`Segment::open` maps the file, checks both magic numbers and the footer checksum, then the checksum of every section except the documents, whose blocks carry their own and are checked when read. A file in a format the server does not know is refused.

| Constant | Section | Contents |
| --- | --- | --- |
| `S_DOCS` | Documents | zstd blocks of about 16 KB of raw data (`BLOCK_SIZE`). Inside a block, each document is a varint length followed by its JSON bytes exactly as they will be returned. |
| `S_DOCIDX` | Block index | 20 bytes per block: offset in the documents section (u64), compressed length (u32), docid of its first document (u32), CRC32 of the compressed block (u32). `Segment::doc_bytes` binary-searches it. |
| `S_TERMS` | Term dictionary | An FST (`fst::Map`) from term to an offset in the term postings. |
| `S_TERMS_POST` | Term postings | Per term: the number of fields it appears in, then per field the field id, the set of documents, and an offset into the positions. Read by `Segment::postings`. |
| `S_FACETS` | Filter dictionary | An FST from `field id (2 bytes, big-endian) + tag (1 byte) + value` to an offset in the filter postings. Built by `facet_key`. |
| `S_FACETS_POST` | Filter postings | Per key: the value as it appeared in the document (for facet distributions) and the set of documents. |
| `S_PKS` | Primary keys | An FST from the document's primary key, as text, to its docid. |
| `S_SORT` | Sort columns | Per sortable field: a column of `f64` (NaN where missing) and, for text, a column of ordinals into a sorted table of distinct strings. Read by `Segment::sort_key`. |
| `S_META` | Metadata | JSON: the number of documents, how many documents have each field (`fieldDistribution`), and where each sort column is. |
| `S_POS` | Positions | For each posting list, per document in order, the token positions as varints holding the distance from the previous position, shifted left by one, with the low bit saying whether another position of the same document follows. |

Details that matter when you touch this file:

- **Filter keys.** The tag says what the value is: `TAG_EXISTS` (the document has the field), `TAG_NULL`, `TAG_NUM` (8 bytes from `util::encode_f64`, which keeps numeric order under byte comparison) and `TAG_STR` (lowercased and trimmed by `tokenizer::normalize_facet`). Because keys sort by field, tag and value, a numeric range is a range scan of the FST.
- **Range levels.** For fields with more than 64 distinct numbers, `RangeLevels` adds precomputed unions: `TAG_NUM_L1` entries cover runs of 64 consecutive values and `TAG_NUM_L2` entries cover 64 of those runs. `Segment::num_range` takes whole runs where it can and single values at the two ends.
- **Document sets** (`util::write_docset`): up to 32 documents as delta varints, more as a serialized roaring bitmap.
- **Positions** count tokens within a field. Between two values of the same field (array items) there is a gap of 8 (`VALUE_GAP`), so a phrase never matches across values.
- **Lossless documents.** `serde_json` is built with `preserve_order` and `arbitrary_precision`: key order and number text survive the round trip.

Changing anything above changes the format: bump `MAGIC` and call it out in `CHANGELOG.md`, as [CONTRIBUTING.md](CONTRIBUTING.md) asks.

## A write, from HTTP request to committed segment

Following `POST /indexes/{uid}/documents`.

### 1. Receive (`crates/server/src/api.rs`)

1. `write_documents` checks the index uid, the key's permission (`Auth::check`) and the content type, and refuses at once a tenant that is already over quota.
2. `receive` streams the request body to `payloads/<id>.upload` as it arrives, up to 256 MB. The body is never held in memory.
3. `check_upload` validates the upload from disk, one document at a time, and leaves `payloads/<id>.ndjson` with one document per line, fsynced. NDJSON is validated and renamed; a JSON array is read element by element by the `DocumentArray` visitor and rewritten as lines.
4. `Tenant::enqueue` appends the task to `tasks.log` with `fsync`, and only then does the server answer `202` with the `taskUid`.

Every other write (settings, deletions, index creation) is the same without the upload: validate, `enqueue`, answer `202`. The kinds of task are the `platform::Kind` enum.

### 2. The task log and its workers (`crates/server/src/platform.rs`)

- `TaskStore` is the task list of one tenant: an append-only file of JSON lines, the newest line for a task id winning. It keeps the last 10,000 finished tasks or 32 MB of them and rewrites the file as it grows (`prune`, `compact`).
- `Platform::open` starts a small pool of worker threads (half the cores, at least two) fed by a channel of tenants. `Platform::schedule` puts a tenant in the channel unless it is already there, so the tasks of one tenant run one at a time and in order, while different tenants run in parallel.
- `Tenant::drain` takes the next task, records it as `processing`, runs it with `Tenant::run` (a panic becomes a failed task) and records the outcome with `fsync`.
- **Replay.** On start, `TaskStore::load` puts back in the queue every task that was `enqueued` or `processing`, the latter flagged `retry`. Every task is written to be safe to run twice; `Manifest::applied_task` and the `retry` flag cover the cases where the first run had already taken effect (an index that now exists, a payload file that is already gone).

### 3. Indexing (`crates/engine/src/index.rs`)

`Tenant::run` opens the payload as an iterator of lines and calls `Index::add_documents_stream`:

1. **Chunks.** `ChunkReader::next_chunk` reads up to 5,000 documents or 4 MB. For each it calls `segment::scan_document`, which walks the JSON without building it, gives every field its id in document order and picks out the primary key. If the index has no primary key yet, it is inferred from the first document (`infer_primary_key`).
2. **Threads.** A write of fewer than 2,000 documents uses one thread. Otherwise there is one per core, up to four (`index_threads`), and each document goes to the thread chosen by the hash of its id, so two versions of a document always meet on the same thread and the last one wins. The channels have no buffer: the reader waits until a thread is ready.
3. **Per thread** (`Index::index_part`). The thread parses its documents, keeps one version per id in the batch, and for each calls `Index::supersede`, which finds the stored version (in the segment being built, in the segments this write has already finished, then in the index) and marks it deleted, merging fields first if the write is a partial update. Then `Writer::add` hands the document to a `SegmentBuilder`.
4. **Building a segment** (`SegmentBuilder::add_doc`). The document's bytes go into the current block. `walk` and `leaf` visit every field: searchable text is tokenized into the term table with positions, filterable values go to the filter table, sortable values to the sort columns. The builder estimates its memory (`mem`); `Writer::add` closes the segment when it reaches its share of the indexing budget (24 MB across threads by default, `configure_indexing`), 200,000 documents or 128 MB of raw documents.
5. **Closing a segment** (`SegmentBuilder::finish`). The sections are written to `seg-N.tmp`, the file is fsynced and renamed to `seg-N.seg`.
6. **Commit.** When every thread is done, the deletions they recorded are joined, segments left with no live document are dropped, and `Index::commit` replaces the manifest. This is the only moment the write becomes visible, and it is all or nothing: if any thread failed, `Writer::abort` discards the new files and the index is as it was.
7. `Index::maybe_merge` runs before the write lock is released.

Back in `Tenant::run`, the payload file is removed and the task is recorded as `succeeded` or `failed`.

Quotas are checked in `Tenant::run` just before indexing (`check_document_quota`), which is why a write over quota is accepted and then fails as a task.

## A search

Following `POST /indexes/{uid}/search`.

`api::run_search` checks the key, counts the search for the tenant and runs `Index::search` on a blocking thread. That takes a snapshot and calls `search::search` in `crates/engine/src/search.rs`. Everything below works per segment on bitmaps of docids (`Cands`, one `RoaringBitmap` per segment).

1. **Filter.** `index::universe` starts from the live documents of each segment (all minus deleted) and intersects them with the filter. `filter::parse_value` turns the filter into a `Filter` tree; `filter::eval` evaluates it with lookups in the filter dictionary (`Segment::facet_get`, `Segment::num_range`).
2. **Parse the query.** `parse_query` tokenizes `q` into at most 10 words and at most 10 quoted phrases, drops stop words, marks the last word as a prefix when the query does not end in a separator, and gives each word its typo budget from its length (`typoTolerance.minWordSizeForTypos`).
3. **Find the terms** (per segment, `word_hits`). For each word the term dictionary is asked for the exact term, for every term with that prefix if the word is a prefix, and for every term within the typo budget:
   - `Typos` implements `fst::Automaton`. Its state is one row of the edit-distance table (plus the previous row, so that swapping two neighbouring letters is one edit), advanced one UTF-8 character at a time. The FST is walked once with it and only branches that can still match are followed. There is nothing to precompute per query.
   - Each term found is scored by `tokenizer::typo_count`, which applies Meilisearch's rule that a typo in the first letter counts as two.
   - At most 300 terms are taken per word and segment (`MAX_DERIVATIONS`).
   
   The result is a `WordHits`: the documents matching the word with 0, 1 and 2 typos, per field, and the exact matches. Quoted phrases are resolved first by `phrase_docs`, which reads positions, and narrow the candidates.
4. **Candidates.** With `matchingStrategy: "all"` a document must contain every word; otherwise it must contain the first one, and the `words` rule sorts by how many of the following words it also has.
5. **Rank.** The ranking rules from the settings become a list of `Rule` values. `rank` is a recursive bucket sort: the first rule splits the candidates into ordered buckets, and each bucket is refined by the remaining rules only if it is still needed to fill the page (`need` is `offset + limit`, capped by `pagination.maxTotalHits`). Most searches stop after a bucket or two.

   | Rule | How it splits a bucket |
   | --- | --- |
   | `Words` | By how many of the query words, counted from the first, the document contains. |
   | `Typo` | By the total number of typos, with bitmap operations on `WordHits::lvl`. |
   | `Proximity` | By the distance between consecutive query words, read from positions (`Segment::scan_positions`, `proximity`). |
   | `AttributeRank` | By the order of the matching attribute in `searchableAttributes`. |
   | `WordPosition` | By how far the words are from the start of the attribute (`position_cost`). |
   | `Exactness` | By whether the words appear exactly as typed, and at the start of an attribute. |
   | `Sort` | By the sort columns (`Segment::sort_key`, `cmp_keys`); ties go on to the next rule. |

   `Proximity`, `WordPosition` and part of `Exactness` read positions for every candidate, and are skipped for buckets of more than 100,000 documents (`PROXIMITY_MAX_DOCS`).
6. **Facets.** `facet_distribution` scans the filter dictionary of each requested attribute and counts, per value, the documents that are also candidates.
7. **Build the hits.** Only now are documents read: `Segment::doc` decompresses the block of each hit on the page. `displayedAttributes` and `attributesToRetrieve` are applied, and `Format` adds highlighting, cropping and match positions by tokenizing the returned text again.

**Threads.** The per-segment stages go through `per_segment`. A search that is the only one running (`ACTIVE`) and has enough candidates spreads its segments over threads; when several searches run at once each stays on its own thread.

**Timing.** With `TRAILHOUND_TRACE=1`, `search` prints the time spent in each of these phases for every query. Start there when a query is slow.

## Merging

Every write adds at least one segment, and every search visits all of them, so their number is kept bounded.

`Index::maybe_merge` runs after each write: above 8 segments (`MERGE_TRIGGER`) it takes the 5 with the fewest live documents (`MERGE_WIDTH`), up to a million documents in total, and joins them with `segment::merge`, repeating until the count is back under the limit.

`segment::merge` does not re-index:

- It first numbers the surviving documents (`remap`), dropping deleted ones.
- **Documents.** For inputs without deletions, in merges under 400,000 documents, the compressed blocks are copied as they are. Otherwise documents are read and recompressed, at zstd level 19 for merges of 400,000 documents or more.
- **Terms, filter values and primary keys.** The FSTs of the inputs are read together as one sorted stream (`fst::map::OpBuilder::union`); for each key the posting lists are joined with docids renumbered. The new dictionary is written straight to the file while postings and positions go to side files (`Spill`) that are appended afterwards, so memory depends on the largest single posting list, not on the size of the segments.
- **Range levels** are built again from the merged values.
- **Sort columns** are rebuilt in memory, which is the one part whose cost grows with the number of documents.

The result is committed like any other write: a new manifest that lists the merged segment instead of its inputs.

## Tenants, keys and quotas

All in `crates/server/src/platform.rs`, with the HTTP side in `api.rs` and `admin.rs`.

- **`Platform`** holds the admin key and the map of tenants, and persists both in `platform.json`. A **`Tenant`** holds its own indexes, API keys, task store and usage counters, in its own folder.
- **Which tenant.** There is no tenant in the URL. `Platform::rebuild_keymap` builds a map from every key (each tenant's master key and its API keys) to its tenant, and `Platform::resolve` looks the request's bearer token up in it. The `api::Auth` extractor does this for every route, and refuses a suspended tenant or an expired key.
- **What the key may do.** A master key may do anything in its tenant. For an API key, `Auth::check` compares the action of the route (`search`, `documents.add`, ...) and the index against the key's `actions` and `indexes`, with `*` wildcards. Routes that list things use `Auth::allows_index` to hide what the key cannot see.
- **Admin.** `/_admin/api` uses a separate extractor, `admin::Admin`, that compares the admin key in constant time before the body is read. Tenant keys do not work there, and the admin key does not work on tenant routes.
- **Quotas** (`platform::Quotas`) are per tenant. `Tenant::create_index` enforces the number of indexes; `Tenant::check_document_quota` enforces documents and bytes, counting the upload at its uploaded size.
- **Usage.** `Tenant::count_search` and the indexing path update counters in memory; a tick in `main.rs` saves them to `usage.json` every 30 seconds and on shutdown.
- **Single-tenant mode.** `--master-key` makes `Platform::open` create or update a tenant called `default` with that key.
- **Index folders.** `Tenant::create_index` gives every index a fresh folder (`<uid>-<8 hex>`), because the files of a deleted index with the same uid may still be open in a search.

## Memory

Low memory use is the reason the project exists, and it comes from a few rules that changes should keep:

- Segments are read through `mmap`. Opening a segment reads it once to verify its checksums but keeps only its metadata; after that the operating system holds in memory the pages that searches touch, and can drop them.
- A request body is never held in memory, and a write is processed in chunks whatever its size.
- A segment under construction is bounded by an estimate of its own memory, not by a number of documents.
- Merging streams dictionaries and postings instead of loading them.
- Each `Segment` caches one decompressed document block, not more.
- On Linux with glibc, `platform::tune_allocator` and `platform::release_memory` keep the allocator from holding on to memory freed after a write or a burst of searches.

If a change trades memory for speed, measure both with `tests/bench/multitenant.sh`.

## Where to make a change

| To do this | Start here |
| --- | --- |
| Add an API route | `api::router`, then a handler next to the similar ones. Check permissions with `Auth::check`. |
| Add a kind of write | A variant of `platform::Kind`, its name in `Kind::type_name`, its effect in `Tenant::run`, and a route that calls `Tenant::enqueue`. Make it safe to run twice. |
| Accept a new upload format (CSV) | `api::write_documents` for the content type and `api::check_upload` to turn it into one JSON document per line. The engine does not change. |
| Add a search parameter | A field of `search::SearchQuery`, used in `search::search`; and `api::query_from_params` if it must work on `GET`. |
| Add a setting | A field of `settings::Settings`. If it changes what segments contain, extend `Settings::needs_reindex`. The settings routes need no change. |
| Add a filter operator | `Filter`, `Parser::primary` and `eval` in `filter.rs`. |
| Add a ranking rule or change one | `search::Rule`, the list built in `search::search`, and a branch of `search::rank`. Keep `tests/bench/ranking.sh` at 22 of 22. |
| Change how text is split or normalized | `tokenizer::tokenize`. Existing segments were built with the old rules, so this is a format change. |
| Store something new per document or per term | A section or a key in `segment.rs`: `SegmentBuilder`, `merge` and `Segment` all need it. Bump `MAGIC`. |
| Change when segments are merged | The constants at the top of `index.rs` and `Index::maybe_merge`. |
| Authentication, keys, tenant tokens | `api::Auth` and `Platform::resolve`. |
| Quotas and usage | `Tenant::check_document_quota`, `Tenant::create_index`, `Tenant::usage`. |
| The admin API or panel | `admin.rs` and `panel/`. Rebuild `panel.html` after changing the panel. |

## Tests

- `crates/engine/tests/basic.rs`: the engine through its public API, including round trips, merges compared against a freshly built index, range filters compared against a full scan, and the typo automaton compared against plain edit distance.
- `tests/sdk/compat.mjs`: the HTTP API through the official JavaScript SDK, against a running server.
- `tests/sdk-python`, `tests/sdk-go`, `tests/sdk-php`: the same checks through the official Python, Go and PHP clients.
- `tests/sdk-laravel`: a minimal Laravel application that uses the server through Laravel Scout's Meilisearch driver.
- `tests/bench/ranking.sh`: result order compared with a real Meilisearch, query by query.
- `tests/bench/bench.sh` and `multitenant.sh`: memory, disk, indexing time and query latency, next to Meilisearch.

CI (`.github/workflows/ci.yml`) builds and runs the engine tests on Linux (x86-64 and ARM64), macOS and Windows, runs clippy, the SDK tests, the panel build and the Docker build.
