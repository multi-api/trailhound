<p><img src="brand/trailhound-logo.svg" alt="Trailhound" width="260"></p>

**Search that stays on the trail.** A low-memory, multi-tenant search engine with a Meilisearch-compatible API, written in Rust.

Trailhound is built to run many customers on small machines: indexes live on disk in memory-mapped files, so the process stays under about 100 MB while serving eight tenants with 300,000 documents between them. It is an independent implementation and is not affiliated with Meilisearch.

> **Status: 0.1, early.** The API surface listed below works and is tested against the official JavaScript SDK, but the on-disk format may still change between versions and several Meilisearch features are missing. See [What is missing](#what-is-missing).

## Run it

```sh
cargo build --release
target/release/trailhound --data-dir ./data --addr 127.0.0.1:7700
```

On first start the server prints the admin key. The admin panel is at `http://127.0.0.1:7700/_admin`.

| Flag | Environment variable | Purpose |
| --- | --- | --- |
| `--data-dir` | `TRAILHOUND_DATA_DIR` | Data folder (default `data`) |
| `--addr` | `TRAILHOUND_ADDR`, `MEILI_HTTP_ADDR` | Listen address |
| `--admin-key` | `TRAILHOUND_ADMIN_KEY` | Key for the panel and `/_admin/api` |
| `--master-key` | `TRAILHOUND_MASTER_KEY`, `MEILI_MASTER_KEY` | Creates a `default` tenant with that key: drop-in replacement for a single Meilisearch instance |
| `--indexing-threads` | `TRAILHOUND_INDEXING_THREADS` | Threads per write (default: one per core, up to 4) |
| `--indexing-memory-mb` | `TRAILHOUND_INDEXING_MEMORY_MB` | Memory for the index being built, shared by the threads (default 24) |

### With Docker

```sh
docker compose up -d --build
docker exec trailhound cat /data/platform.json   # shows the admin key
```

The server listens on `http://127.0.0.1:7700`, keeps its data in the `trailhound-data` volume and is limited to 256 MB of memory. The admin key is also printed in `docker logs trailhound` the first time the container starts.

On Windows the linker fails (`LNK1104`) when the project path is very long; build from a short path or set `CARGO_TARGET_DIR`.

## Tenants

Trailhound is multi-tenant: one server holds many isolated customers or projects. Each tenant has its own folder, API keys, task queue, limits and usage counters, and the API key of a request decides which tenant it belongs to.

Create a tenant in the admin panel (or with `POST /_admin/api/tenants`) to get its master key and search key, then point any Meilisearch client at the server with that key. If you only need one tenant, start the server with `--master-key` instead.

## Admin panel

The panel (`/_admin`) is a React, Tailwind and shadcn/ui application in `panel/`. It builds into a single HTML file (571 KB, no external resources) at `crates/server/src/panel.html`, which is embedded in the binary, so `cargo build` does not need Node. It is in English by default, with Spanish available, and follows the system's light or dark mode.

To change it:

```sh
cd panel
npm install
npm run dev      # against a Trailhound server on localhost:7700 (or TRAILHOUND_URL)
npm run build    # regenerates crates/server/src/panel.html
```

## How it works

- **Immutable segments** (`crates/engine/src/segment.rs`): each batch of documents is written to one file holding the documents in zstd blocks, an FST term dictionary, compressed posting lists, a filter index and sort columns. Files are read through `mmap`, so process memory does not grow with index size.
- **Lossless documents**: documents are stored and returned with the same key order and the same numbers (`9.50` stays `9.50`; large integers are not rounded).
- **Integrity**: every section and every document block carries a CRC32. A damaged block produces an error, never wrong data.
- **Crash safety** (`crates/engine/src/index.rs`, `crates/server/src/platform.rs`): a write creates new files, calls `fsync` and replaces the manifest with an atomic `rename`. Tasks are recorded on disk before the server answers `202`; unfinished ones are repeated on start and partial files are removed.
- **Bounded memory while writing** (`crates/server/src/api.rs`, `crates/engine/src/index.rs`): the request body goes to disk as it arrives and documents are read 5,000 at a time. A segment under construction is closed when its in-memory index uses up its share of the budget (24 MB across all threads). Freed memory is returned to the system after each task.
- **Parallel indexing**: the reading thread only extracts the primary key and assigns field ids; documents are routed by the hash of their key to the indexing threads, which each tokenize their share and build their own segments. A document always lands on the same thread, so the last version wins.
- **Merging** (`segment::merge`): above 8 segments, the 5 smallest are merged without reindexing: documents are copied block by block and dictionaries are merged as sorted streams, dropping deleted documents. Merges under 400,000 documents copy the compressed blocks as they are; larger ones recompress at zstd level 19.
- **Search** (`crates/engine/src/search.rs`): typo tolerance walks the dictionary with an automaton computed on the fly. Range filters use precomputed unions over spans of 64 and 4,096 values. A lone search spreads segments across cores; concurrent searches use one thread each.
- **Stable memory** (`tune_allocator`): on Linux the server limits what glibc keeps after freeing. `TRAILHOUND_TRACE=1` prints the time spent in each phase of each search.

## Meilisearch-compatible API

Indexes, documents (JSON and NDJSON; add, replace, partial update, delete by id, by batch and by filter), search (`q`, typos, prefix, `filter`, `sort`, `facets`, pagination, highlighting, cropping, `matchingStrategy`, `attributesToSearchOn`, `showMatchesPosition`), `multi-search`, settings and their sub-routes, tasks, API keys with actions and indexes, `stats`, `health`, `version`.

Ranking applies the same rules as Meilisearch 1.58, in the same default order: `words`, `typo`, `proximity`, `attributeRank`, `sort`, `wordPosition`, `exactness` (the older name `attribute` means `attributeRank` plus `wordPosition`). Typos are counted the same way: swapping two neighbouring letters is one typo, and a typo in the first letter counts as two. Quoted phrases require the words to be adjacent and in order.

### SDK compatibility test

`tests/sdk/compat.mjs` runs 34 checks with `meilisearch-js` 0.63.0 against a running server:

```sh
target/release/trailhound --data-dir ./data-test --addr 127.0.0.1:7711 --master-key test-master-key
cd tests/sdk && npm install && npm test
```

## Comparison with Meilisearch 1.58.0

Measured on 2026-11-24 with 300,000 synthetic documents (95 MB of NDJSON, an 80-word vocabulary) on two machines: a 32-core x86 PC (WSL2 with Docker) and a Mac M1 Max (ARM, Docker with 8 CPUs and 4 GB). The scripts are in `tests/bench/`.

### Result order (`ranking.sh`)

For 22 queries (one or several words, typos, prefixes, filters, sorting), Trailhound's top 10 hits are sent to Meilisearch to be scored. In all 22, the per-rule scores match those of Meilisearch's own top 10 position by position, both with the default `searchableAttributes` and with `["title", "description"]`. In 6 to 13 of them, depending on the configuration, the ids are identical too; in the rest, the only differences are documents tied on every rule.

### One index, one client (`bench.sh`, x86)

| Measure | Trailhound | Meilisearch |
| --- | --- | --- |
| Disk | 63 MB | 598 MB |
| Memory at rest after a restart | 45 MB | 78 MB |
| Memory after 150 searches | 55 MB | 193 MB |
| Peak memory while indexing | 95 MB | 2,104 MB |
| Indexing, 3 batches of 100,000 | 2.7 – 3.2 s | 5.3 – 6.8 s |
| Search (median, 10 different queries) | 0.9 – 4.1 ms | 1.3 – 2.7 ms |

### Several tenants at once (`multitenant.sh`)

Eight tenants with 37,500 documents each (eight tenants with their own key in Trailhound; eight indexes in Meilisearch), with 8, 32 and 128 connections sending a mix of the 10 queries for 10 s.

| x86, 32 cores | Trailhound | Meilisearch |
| --- | --- | --- |
| Queries per second (8 / 32 / 128 connections) | 4,700 / 10,400 / 11,900 | 5,900 / 12,100 / 11,400 |
| p50 latency, ms | 1.7 / 2.9 / 9.4 | 1.2 / 2.5 / 11.0 |
| p99 latency, ms | 2.8 / 5.8 / 30 | 2.5 / 4.7 / 13.5 |
| Memory under load | 104 – 190 MB | 1,094 – 1,127 MB |
| Loading the eight tenants | 1.3 s | 5.0 s |
| Disk | 72 MB | 546 MB |

| x86, 4 cores | Trailhound | Meilisearch |
| --- | --- | --- |
| Queries per second (8 / 32 / 128 connections) | 2,700 / 3,300 / 3,300 | 3,600 / 3,400 / 3,300 |
| p50 latency, ms | 2.6 / 7.9 / 37 | 2.1 / 9.3 / 38 |
| p99 latency, ms | 8.0 / 30 / 64 | 4.9 / 13 / 50 |
| Memory under load | 94 – 107 MB | 831 – 863 MB |
| Loading the eight tenants | 1.7 s | 16.9 s |

| ARM (Mac M1 Max), 8 Docker cores | Trailhound | Meilisearch |
| --- | --- | --- |
| Queries per second (8 / 32 / 128 connections) | 2,700 / 4,000 / 4,300 | 3,600 / 4,300 / 4,200 |
| p50 latency, ms | 2.7 / 7.3 / 28 | 2.0 / 7.1 / 30 |
| p99 latency, ms | 6.0 / 20 / 58 | 5.1 / 14 / 42 |
| CPU used in 10 s (8 / 32 / 128) | 43 / 64 / 70 s | 37 / 44 / 43 s |
| Memory under load | 92 – 113 MB | 874 – 981 MB |
| Loading the eight tenants | 2.2 s | 11.0 s |
| Disk | 72 MB | 552 MB |

### Simulated t4g.nano size: 512 MB

| | Trailhound, 2 vCPU x86 | Trailhound, 1 vCPU x86 | Trailhound, 2 vCPU ARM | Meilisearch, 2 vCPU (x86 and ARM) |
| --- | --- | --- | --- | --- |
| Loading the eight tenants | 2.5 s | 5.0 s | 3.7 s | killed for lack of memory |
| Queries per second (8 / 32 / 128) | 2,050 / 2,200 / 2,200 | 1,200 / 1,230 / 1,235 | 1,550 / 1,650 / 1,660 | — |
| p50 latency, ms | 3.3 / 9.6 / 53 | 4.6 / 16 / 92 | 4.5 / 16 / 74 | — |
| p99 latency, ms | 13 / 59 / 109 | 56 / 108 / 204 | 15 / 51 / 116 | — |
| Memory under load | 80 – 97 MB | 77 – 89 MB | 80 – 98 MB | — |

How to read these tables:

- The ARM runs used a Mac M1 Max with Docker limited to 2 vCPUs and 512 MB, not a real t4g.nano: M1 cores are faster than Graviton2 cores and a nano runs on CPU credits, so figures there will be lower.
- At the nano size, Meilisearch was tried with `MEILI_MAX_INDEXING_MEMORY` at 200 Mb and at 100 Mb and one indexing thread; in every case the system killed it for memory while loading.
- Meilisearch uses less CPU per query (on ARM, 1.0 ms against 1.6 ms): it keeps precomputed on disk what Trailhound computes while searching. With few connections it serves more queries; with many, the two are level.
- Trailhound's latency tail (p99) is worse: some queries cost considerably more than others.
- The load generator is a Python script on the same machine and competes with the server for CPU.
- The `proximity` and `wordPosition` rules, and the part of `exactness` that looks at the start of the attribute, are skipped for groups of more than 100,000 tied candidates.
- The data is synthetic; proportions may change with real text.

## What is missing

- `distinctAttribute`, geosearch, CSV, federated search, tenant tokens (JWT), dumps and snapshots, task cancellation and deletion, index swaps.
- Vector and hybrid search.
- TLS and per-tenant rate limiting. Put a TLS proxy in front to expose the server.
- API keys are stored in clear text on disk.
- `exactness` does not tell an attribute that equals the query from one that only starts with it.
- With many ties, tied documents come out in a different order than in Meilisearch.
- `fieldDistribution` also counts deleted documents that have not been through a merge yet.
- When merging, the sort columns of the resulting segment are built in memory (8 bytes per document and numeric field; one string per document for text fields).
- Opening a segment verifies the CRC of all its sections, which reads the whole file on start.

## Brand

Logo, colours, typography and voice are described in [`brand/guide.html`](brand/guide.html).

## License

[MIT](LICENSE) © Multi API SpA.
