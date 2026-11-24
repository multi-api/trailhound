<p><img src="brand/trailhound-logo.svg" alt="Trailhound" width="260"></p>

**Search that stays on the trail.** A low-memory, multi-tenant search engine with a Meilisearch-compatible API, written in Rust.

[![CI](https://github.com/multi-api/trailhound/actions/workflows/ci.yml/badge.svg)](https://github.com/multi-api/trailhound/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/trailhound.svg)](https://crates.io/crates/trailhound)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Trailhound gives you typo-tolerant, filterable, sortable full-text search over JSON documents. It speaks the Meilisearch HTTP API, so the official Meilisearch SDKs work against it, and it is built for one thing Meilisearch is not: running many customers on a small machine.

- **Small.** Indexes live on disk in memory-mapped files. Eight tenants with 300,000 documents between them are served in about 100 MB of RAM and 72 MB of disk; the same data in Meilisearch takes about 1 GB of RAM and 550 MB of disk. ([Measurements](#comparison-with-meilisearch-1580))
- **Multi-tenant.** One server holds many isolated tenants, each with its own indexes, API keys, quotas and usage counters.
- **Compatible.** Indexes, documents, search, settings, tasks and keys follow the Meilisearch API, and results are ranked by the same rules in the same order.
- **Careful with data.** Documents come back byte for byte as they went in, every block on disk is checksummed, and writes survive a crash or power cut.
- **One binary.** No external services. The admin panel is embedded.

Trailhound is an independent implementation and is not affiliated with Meilisearch.

> **Status: 0.1.** The API listed under [API compatibility](#api-compatibility) is tested against the official JavaScript, Python, Go and PHP SDKs and against Laravel Scout on every commit. Several Meilisearch features are not implemented and the on-disk format may still change between minor versions; read [What is missing](#what-is-missing) and [Running in production](#running-in-production) before you depend on it.

## Contents

- [Quick start](#quick-start)
- [Tenants and keys](#tenants-and-keys)
- [Using an SDK](#using-an-sdk)
- [Running in production](#running-in-production)
- [Configuration](#configuration)
- [Admin panel and admin API](#admin-panel-and-admin-api)
- [API compatibility](#api-compatibility)
- [What is missing](#what-is-missing)
- [How it works](#how-it-works)
- [Comparison with Meilisearch 1.58.0](#comparison-with-meilisearch-1580)
- [Development](#development)

## Quick start

With Docker, using the published image (x86-64 and ARM64):

```sh
docker run -d --name trailhound -p 127.0.0.1:7700:7700 -v trailhound-data:/data ghcr.io/multi-api/trailhound:0.1.0
docker exec trailhound cat /data/platform.json     # shows the admin key
```

The repository also has a `docker-compose.yml` that builds the image from source: `docker compose up -d --build`.

Or download a binary for Linux (x86-64 and ARM64), macOS (Apple silicon) or Windows from the [latest release](https://github.com/multi-api/trailhound/releases/latest), unpack it and run it:

```sh
./trailhound --data-dir ./data --addr 127.0.0.1:7700
```

Or install it from [crates.io](https://crates.io/crates/trailhound) with a stable Rust toolchain:

```sh
cargo install trailhound --locked
trailhound --data-dir ./data --addr 127.0.0.1:7700
```

Or build from source:

```sh
cargo build --release
target/release/trailhound --data-dir ./data --addr 127.0.0.1:7700
```

The server listens on `http://127.0.0.1:7700`. The first time it starts it generates an admin key, prints it and stores it in `platform.json` inside the data folder.

**1. Create a tenant.** Open `http://127.0.0.1:7700/_admin`, sign in with the admin key and choose "New tenant", or use the admin API:

```sh
curl -X POST http://127.0.0.1:7700/_admin/api/tenants \
  -H "Authorization: Bearer $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"name": "Acme", "quotas": {"maxIndexes": 10, "maxDocuments": 100000}}'
```

The answer carries the tenant's `masterKey` (full access to that tenant; keep it on your server) and `searchKey` (search only; safe to ship to a browser).

**2. Add documents.** The index is created on the first write:

```sh
curl -X POST "http://127.0.0.1:7700/indexes/books/documents?primaryKey=id" \
  -H "Authorization: Bearer $MASTER_KEY" -H "Content-Type: application/json" \
  -d '[
    {"id": 1, "title": "One Hundred Years of Solitude", "author": "Gabriel Garcia Marquez", "year": 1967},
    {"id": 2, "title": "The House of the Spirits", "author": "Isabel Allende", "year": 1982},
    {"id": 3, "title": "Ficciones", "author": "Jorge Luis Borges", "year": 1944}
  ]'
```

Writes are asynchronous, as in Meilisearch: the answer is `202` with a `taskUid`, and `GET /tasks/{taskUid}` tells you when it has `succeeded`.

**3. Search,** typos included:

```sh
curl -X POST http://127.0.0.1:7700/indexes/books/search \
  -H "Authorization: Bearer $SEARCH_KEY" -H "Content-Type: application/json" \
  -d '{"q": "garsia marques"}'
```

```json
{"hits":[{"id":1,"title":"One Hundred Years of Solitude","author":"Gabriel Garcia Marquez","year":1967}],
 "query":"garsia marques","processingTimeMs":0,"limit":20,"offset":0,"estimatedTotalHits":1}
```

**4. Filter and sort.** Declare the attributes first, then use them:

```sh
curl -X PATCH http://127.0.0.1:7700/indexes/books/settings \
  -H "Authorization: Bearer $MASTER_KEY" -H "Content-Type: application/json" \
  -d '{"filterableAttributes": ["year"], "sortableAttributes": ["year"]}'

curl -X POST http://127.0.0.1:7700/indexes/books/search \
  -H "Authorization: Bearer $SEARCH_KEY" -H "Content-Type: application/json" \
  -d '{"q": "", "filter": "year > 1950", "sort": ["year:desc"], "attributesToRetrieve": ["title", "year"]}'
```

## Tenants and keys

A **tenant** is one customer or project of yours. Tenants are isolated: each has its own folder on disk, indexes, API keys, task queue, quotas and usage counters. There is no tenant id in the URL; the API key of a request decides which tenant it belongs to.

There are three kinds of key:

| Key | Who holds it | What it can do |
| --- | --- | --- |
| Admin key | You, the operator | Create, suspend and delete tenants, set quotas, rotate keys. Works only on `/_admin` |
| Tenant master key | The tenant's backend | Everything inside that tenant: indexes, documents, settings, tasks, API keys |
| Tenant API keys | The tenant's applications | Only the actions and indexes each key was created with. Every tenant starts with one search-only key |

A tenant can create more keys with `POST /keys`, exactly as in Meilisearch, limited to a set of actions and indexes.

**Quotas** are per tenant: maximum number of indexes, of documents and of bytes on disk. A write that would exceed one is accepted like any other and its task then fails with a `quota_exceeded` error; searches keep working. For the disk quota, an upload counts at its uploaded size, which is more than it takes once indexed, so a tenant cannot get past the quota in one large write. A **suspended** tenant is refused on every request until it is resumed, and its data is kept.

**If you only need one tenant,** start the server with `--master-key`. It creates a tenant called `default` with that key, which makes Trailhound a drop-in replacement for a single Meilisearch instance started with the same master key.

## Using an SDK

Point any Meilisearch client at the server with a tenant key. With the official JavaScript SDK:

```js
import { MeiliSearch } from 'meilisearch'

const client = new MeiliSearch({ host: 'https://search.example.com', apiKey: process.env.TRAILHOUND_KEY })

await client.index('books').addDocuments(books, { primaryKey: 'id' })
const { hits } = await client.index('books').search('garsia marques', { filter: 'year > 1950' })
```

These clients are run against the server on every commit. Each suite covers the index lifecycle, adding, updating and deleting documents, waiting for tasks, search (typos, filters, sort, facets, pagination, highlighting), settings, keys and multi-search:

| Client | Version tested | Suite |
| --- | --- | --- |
| JavaScript: [`meilisearch`](https://github.com/meilisearch/meilisearch-js) | 0.63.0 | `tests/sdk` |
| Python: [`meilisearch`](https://github.com/meilisearch/meilisearch-python) | 0.43.0 | `tests/sdk-python` |
| Go: [`meilisearch-go`](https://github.com/meilisearch/meilisearch-go) | 0.37.0 | `tests/sdk-go` |
| PHP: [`meilisearch/meilisearch-php`](https://github.com/meilisearch/meilisearch-php) | 1.17.0 | `tests/sdk-php` |
| Laravel: [`laravel/scout`](https://laravel.com/docs/scout) with its Meilisearch driver | Scout 11.11.0 on Laravel 13.44.1 | `tests/sdk-laravel` |

In each suite, the calls that need a feature listed under [What is missing](#what-is-missing) are marked as expected failures, with the reason. Other SDKs use the same HTTP API but are not tested here.

### Laravel Scout

Scout's Meilisearch driver works unchanged. Install it as the Scout documentation says:

```sh
composer require laravel/scout meilisearch/meilisearch-php http-interop/http-factory-guzzle
php artisan vendor:publish --provider="Laravel\Scout\ScoutServiceProvider"
```

Point it at Trailhound in `.env`, with a tenant key:

```ini
SCOUT_DRIVER=meilisearch
MEILISEARCH_HOST=http://127.0.0.1:7700
MEILISEARCH_KEY=your-tenant-master-key
```

As with Meilisearch, the attributes used by `where` and `orderBy` have to be declared, in `config/scout.php`, and sent to the server with `php artisan scout:sync-index-settings`:

```php
'meilisearch' => [
    'host' => env('MEILISEARCH_HOST', 'http://localhost:7700'),
    'key' => env('MEILISEARCH_KEY'),
    'index-settings' => [
        Book::class => [
            'filterableAttributes' => ['author', 'year'],
            'sortableAttributes' => ['year'],
        ],
    ],
],
```

```php
Book::search('garsia marques')->where('year', '>', 1950)->orderBy('year')->paginate(20);
```

`tests/sdk-laravel` is a minimal Laravel application that checks indexing on save, `search`, `where`, `whereIn`, `whereNotIn`, `orderBy`, `paginate`, and the `scout:index`, `scout:sync-index-settings`, `scout:import`, `scout:flush` and `scout:delete-index` commands.

## Running in production

A checklist first; the details follow.

- [ ] Run behind a reverse proxy that terminates TLS. Trailhound speaks plain HTTP.
- [ ] Do not expose `/_admin` to the internet. Block it at the proxy or restrict it by IP.
- [ ] Set `TRAILHOUND_ADMIN_KEY` yourself, to a long random value, from your secrets store.
- [ ] Keep the data folder on a persistent volume, readable only by the server's user. Keys are stored there in clear text.
- [ ] Give each tenant quotas, and use search-only keys in browsers and mobile apps.
- [ ] Add request rate limiting at the proxy. Trailhound has none.
- [ ] Back up the data folder, and test a restore.
- [ ] Monitor `GET /health` and the memory and disk of the host.

### Docker

This Compose file is a working starting point: it binds the port to `127.0.0.1` only, keeps the data in a named volume, restarts the server unless stopped, limits it to 256 MB of memory and runs as an unprivileged user. Set the admin key in an `.env` file or your orchestrator's secrets rather than in the file itself:

```yaml
services:
  trailhound:
    image: ghcr.io/multi-api/trailhound:0.1.0
    restart: unless-stopped
    ports:
      - "127.0.0.1:7700:7700"
    volumes:
      - trailhound-data:/data
    mem_limit: 256m
    environment:
      TRAILHOUND_ADMIN_KEY: ${TRAILHOUND_ADMIN_KEY}

volumes:
  trailhound-data:
```

The image has a health check on `/health`. On `docker stop` the server stops accepting connections, lets requests in flight finish and saves its usage counters.

### Binary with systemd

```ini
[Unit]
Description=Trailhound search server
After=network.target

[Service]
User=trailhound
ExecStart=/usr/local/bin/trailhound
Environment=TRAILHOUND_DATA_DIR=/var/lib/trailhound
Environment=TRAILHOUND_ADDR=127.0.0.1:7700
EnvironmentFile=/etc/trailhound/env
Restart=on-failure
MemoryMax=256M
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/var/lib/trailhound

[Install]
WantedBy=multi-user.target
```

Put `TRAILHOUND_ADMIN_KEY=...` in `/etc/trailhound/env`, owned by root with mode `600`.

### Reverse proxy

With Caddy, which obtains the certificate for you. This serves the search API publicly and keeps the admin panel reachable from one address only:

```
search.example.com {
    @admin_from_outside {
        path /_admin*
        not remote_ip 203.0.113.10
    }
    respond @admin_from_outside 403

    reverse_proxy 127.0.0.1:7700
}
```

With nginx, inside a `server` block that already has TLS configured:

```nginx
client_max_body_size 256m;            # Trailhound accepts request bodies up to 256 MB

location /_admin {
    allow 203.0.113.10;
    deny all;
    proxy_pass http://127.0.0.1:7700;
}

location / {
    proxy_pass http://127.0.0.1:7700;
    proxy_request_buffering off;      # stream large document uploads straight through
}
```

The server answers CORS requests from any origin, which is what lets a browser search directly with a search key. Restrict origins at the proxy if you need to.

### Sizing

These figures come from the benchmarks below, with synthetic documents of about 320 bytes each; measure with your own data.

| Resource | What to expect |
| --- | --- |
| Memory | 45 MB at rest and about 100 MB under load for 300,000 documents in 8 tenants. Indexing peaks at about 95 MB. A 256 MB limit leaves room; the simulated 512 MB, 2-vCPU machine ran it comfortably |
| Disk | About two thirds of the size of the documents as NDJSON (63 MB for 95 MB). Leave free space for merges, which rewrite the segments they join |
| CPU | Searches use one thread each when several run at once. About 1,100 queries per second per core on x86 for the benchmark's query mix |
| Indexing memory | `TRAILHOUND_INDEXING_MEMORY_MB` (default 24) caps the index under construction. Raise it for faster bulk loads, lower it on very small machines |

Memory use depends on what is being searched, not on how much is stored: index files are memory-mapped and the operating system keeps in memory only the pages in use.

### Data folder and backups

```
data/
  platform.json            admin key and the list of tenants, with their master keys and quotas
  tenants/<tenant id>/
    keys.json              the tenant's API keys
    tasks.log              the task queue and history
    payloads/              uploaded documents waiting to be indexed
    indexes/<index uid>/   segments, settings and manifest
    usage.json             usage counters
```

Everything the server knows is in this folder; restoring it restores the server. There is no dump or snapshot command yet, so back up at the filesystem level:

- **Simplest:** stop the server, archive the folder (or the Docker volume), start it again.
- **Without downtime:** take an atomic snapshot of the volume (LVM, ZFS, EBS and similar). The folder is always in a state the server can start from, because writes replace the manifest atomically and unfinished tasks are replayed on start.

Copying the folder file by file while the server is writing is not safe: the copy can mix files from before and after a write.

### Upgrades

Before 1.0, a new minor version may change the on-disk segment format; the changelog says when it does. A server refuses to open segments in a format it does not know rather than misread them. Until an in-place migration exists, upgrading across a format change means loading the documents again into the new version, so keep the source of your documents.

### Monitoring and limits

- `GET /health` answers `{"status":"available"}` without a key. Use it for load balancer and container health checks.
- `GET /stats` (tenant key) and `GET /_admin/api/tenants` (admin key) report documents, disk and searches per index and per tenant.
- `TRAILHOUND_TRACE=1` prints the time spent in each phase of each search to standard output. The server does not write an access log; take request logs from the proxy.
- A document upload may be up to 256 MB; it is streamed to disk, not held in memory. Send larger imports as several requests; NDJSON is the cheapest format to parse.
- Every other request body (searches, settings, id lists) may be up to 4 MB, and admin API bodies up to 64 KB.
- A filter may nest `NOT` and parentheses up to 64 levels deep. A search may ask for up to 100 facets and use up to 10 words and 10 quoted phrases; a `multi-search` request may carry up to 100 queries.
- Each tenant keeps its last 10,000 finished tasks, or 32 MB of them, whichever is less; older ones are dropped.

## Configuration

| Flag | Environment variable | Purpose |
| --- | --- | --- |
| `--data-dir` | `TRAILHOUND_DATA_DIR` | Data folder (default `data`) |
| `--addr` | `TRAILHOUND_ADDR`, `MEILI_HTTP_ADDR` | Listen address (default `127.0.0.1:7700`; the Docker image uses `0.0.0.0:7700`) |
| `--admin-key` | `TRAILHOUND_ADMIN_KEY` | Key for the panel and `/_admin/api`, at least 16 characters. If unset, one is generated on first start and stored in `platform.json` |
| `--master-key` | `TRAILHOUND_MASTER_KEY`, `MEILI_MASTER_KEY` | Creates a `default` tenant with that master key |
| `--indexing-threads` | `TRAILHOUND_INDEXING_THREADS` | Threads per write (default: one per core, up to 4) |
| `--indexing-memory-mb` | `TRAILHOUND_INDEXING_MEMORY_MB` | Memory for the index being built, shared by the threads (default 24) |
| | `TRAILHOUND_TRACE` | `1` prints per-phase search timings |

On Windows the linker fails (`LNK1104`) when the project path is very long; build from a short path or set `CARGO_TARGET_DIR`.

## Admin panel and admin API

The panel at `/_admin` lists tenants with their usage, creates, suspends and deletes them, shows and rotates their keys, edits quotas and runs test searches. It is in English by default, with Spanish available, follows the system's light or dark mode and loads nothing from other servers.

Everything the panel does goes through the admin API, authenticated with `Authorization: Bearer <admin key>`:

| Request | Effect |
| --- | --- |
| `GET /_admin/api/tenants` | List tenants with keys, quotas, usage, indexes and task counts |
| `POST /_admin/api/tenants` | Create one. Body: `{"name": "...", "quotas": {"maxIndexes", "maxDocuments", "maxStorageBytes"}}`; quotas are optional and `null` means no limit |
| `PATCH /_admin/api/tenants/{id}` | Change `name`, `suspended` or `quotas` |
| `POST /_admin/api/tenants/{id}/rotate-key` | Replace the master key; the old one stops working immediately |
| `DELETE /_admin/api/tenants/{id}` | Delete the tenant and all its data |

## API compatibility

Implemented, following the Meilisearch API:

| Area | Supported |
| --- | --- |
| Indexes | Create, list, get, update the primary key, delete, stats |
| Documents | JSON and NDJSON; add or replace, partial update, get, fetch, delete by id, by batch and by filter |
| Search | `q`, typo tolerance, prefix search, phrase search with quotes, `filter`, `sort`, `facets`, `limit`/`offset` and `page`/`hitsPerPage`, highlighting, cropping, `matchingStrategy`, `attributesToSearchOn`, `attributesToRetrieve`, `showMatchesPosition`; `POST /multi-search` |
| Settings | The settings object and its sub-routes: searchable, filterable, sortable and displayed attributes, ranking rules, stop words, synonyms, typo tolerance, faceting, pagination |
| Tasks | List and get, with the Meilisearch task shape; filter by `uids`, `indexUids`, `statuses`, `types` and the `before…`/`after…` dates, newest first or `reverse` |
| Keys | Create, list, get, update, delete, with actions and indexes |
| Other | `health`, `version`, `stats` |

Ranking applies the same rules as Meilisearch 1.58, in the same default order: `words`, `typo`, `proximity`, `attributeRank`, `sort`, `wordPosition`, `exactness` (the older name `attribute` means `attributeRank` plus `wordPosition`). Typos are counted the same way: swapping two neighbouring letters is one typo, and a typo in the first letter counts as two.

`GET /version` reports `pkgVersion` `1.12.0` so that SDKs that check the server version accept it; the real version is in the `engine` field.

## What is missing

Not implemented:

- `distinctAttribute`, geosearch, CSV uploads, federated search, tenant tokens (JWT), index swaps.
- Vector and hybrid search.
- Dumps and snapshots, and task cancellation and deletion.
- Batches (`GET /batches`). Every task reports its own uid as its `batchUid`.
- TLS, request rate limiting and access logs. Use a reverse proxy.
- Encryption of keys at rest: API keys are stored in clear text in the data folder.
- Replication and clustering: a server is a single node.

Known differences and rough edges:

- `exactness` does not tell an attribute that equals the query from one that only starts with it.
- With many ties, tied documents come out in a different order than in Meilisearch.
- A search keeps running on the server after its client disconnects.
- The `proximity` and `wordPosition` rules are skipped for groups of more than 100,000 tied candidates.
- The `distinctAttribute` setting and the `distinct` search parameter are accepted and stored, but not applied: no error is returned.
- When merging, the sort columns of the resulting segment are built in memory (8 bytes per document and numeric field; one string per document for text fields).
- Opening a segment verifies the checksum of all its sections, which reads the whole file on start.

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

## Development

```sh
cargo build --release
cargo test --release                 # engine tests
```

SDK compatibility tests, against a running server. Each suite is independent; run the ones whose toolchain you have:

```sh
target/release/trailhound --data-dir ./data-test --addr 127.0.0.1:7711 --master-key test-master-key

cd tests/sdk && npm install && npm test                                    # JavaScript
cd tests/sdk-python && pip install -r requirements.txt && python compat.py # Python
cd tests/sdk-go && go run .                                                # Go
cd tests/sdk-php && composer install && php compat.php                     # PHP
cd tests/sdk-laravel && composer install && cp .env.example .env && php compat.php   # Laravel Scout
```

The suites read the server's address from `TRAILHOUND_URL` (`MEILISEARCH_HOST` for the Laravel application) when it is not `http://127.0.0.1:7711`.

The admin panel is a React, Tailwind and shadcn/ui application in `panel/`. It builds into a single HTML file at `crates/server/src/panel.html`, which is committed and embedded in the binary, so `cargo build` does not need Node:

```sh
cd panel
npm install
npm run dev      # against a Trailhound server on localhost:7700 (or TRAILHOUND_URL)
npm run build    # regenerates crates/server/src/panel.html
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the layout of the repository and what a pull request needs, [SECURITY.md](SECURITY.md) to report a vulnerability, and [CHANGELOG.md](CHANGELOG.md) for what changed in each version. The logo, colours and voice are described in [`brand/guide.html`](brand/guide.html).

## License

[MIT](LICENSE) © Multi API SpA.
