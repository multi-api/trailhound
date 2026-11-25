<p><img src="brand/trailhound-logo.svg" alt="Trailhound" width="260"></p>

**Search that stays on the trail.** A low-memory, multi-tenant search engine with a Meilisearch-compatible API, written in Rust.

[![CI](https://github.com/multi-api/trailhound/actions/workflows/ci.yml/badge.svg)](https://github.com/multi-api/trailhound/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/trailhound.svg)](https://crates.io/crates/trailhound)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Trailhound gives you typo-tolerant, filterable, sortable full-text search over JSON documents. It speaks the Meilisearch HTTP API, so the official Meilisearch SDKs work against it, and it is built for one thing Meilisearch is not: running many customers on a small machine.

- **Small.** Indexes live on disk in memory-mapped files. One million Wikipedia articles are indexed in 224 MB of RAM and kept in 660 MB of disk, where Meilisearch needs 7 GB of RAM and 6.5 GB of disk; at rest the server holds 9 MB. The price is speed: on the same data Meilisearch answers two to five times as many searches, with a much better latency tail. ([Measurements](#comparison-with-meilisearch-1580))
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

These figures come from the benchmarks below, on English Wikipedia (the opening of each article, about 800 bytes a document) and on synthetic documents of about 320 bytes; measure with your own data.

| Resource | What to expect |
| --- | --- |
| Memory | About 10 MB at rest whatever the size of the index, and the process's own memory stays in the tens of megabytes under load. On top of that come the pages of the index that searches touch, which the system keeps while it has room: 200 to 670 MB for 1 million articles under load, up to 1.9 GB for 6.4 million. Under a 512 MB limit both sizes were indexed and served, the larger one slowly |
| Disk | About 80% of the size of the documents as NDJSON for real text (650 MB for 800 MB), two thirds for the synthetic documents. Leave free space for merges, which write the new segment before the ones it replaces are removed: while loading, the data folder reached twice the final size for 1 million articles and 1.3 times for 6.4 million |
| CPU | Searches use one thread each when several run at once, and their cost grows with the index. On x86, for the benchmarks' query mixes: about 1,100 queries per second per core on eight tenants of 37,500 synthetic documents, about 150 on eight tenants of 125,000 articles, and about 10 on one index of 6.4 million |
| Indexing memory | `TRAILHOUND_INDEXING_MEMORY_MB` (default 24) caps the index under construction. Raise it for faster bulk loads, lower it on very small machines |

Memory use depends on what is being searched more than on how much is stored: index files are memory-mapped and the operating system keeps in memory only the pages in use, and takes them back when something else needs the room. With less memory than index, searches wait for the disk.

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

Before 1.0, a new minor version may change the on-disk segment format; the changelog says when it does. A server refuses to open segments in a format it does not know rather than misread them. Where the changelog does not say that the older format is still read, upgrading across a format change means loading the documents again into the new version, so keep the source of your documents.

The current format is `THSEG002`. Segments written as `THSEG001` (version 0.1.0) are still read, so a data folder from that version opens as it is; its segments are rewritten in the new format as merges reach them, and what the new format makes faster (sorted searches, ranges on one field) applies to them from then on. Going back is not possible: once a newer server has written to a folder, an older one refuses it. Back up the folder before upgrading.

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

The [roadmap](ROADMAP.md) says which of these are planned, in what order, and what is out of scope. All of it is open for contribution.

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
- A query word is matched against a limited number of terms of each segment's dictionary (its typos and, for the last word, the terms it is a prefix of): 300, or the segment's share of 2,400 for the whole index if that is more. A short prefix of a common word therefore counts fewer matches than Meilisearch and can miss documents that would rank first.
- Symbols such as `$`, `=`, `<` and `|` separate words and nothing more; Meilisearch appears to give them a position of their own, which changes `proximity` around them.
- The `distinctAttribute` setting and the `distinct` search parameter are accepted and stored, but not applied: no error is returned.
- When merging, the sort columns of the resulting segment are built in memory (8 bytes per document and numeric field; one string per document for text fields).
- Opening a segment verifies the checksum of all its sections, which reads the whole file on start: one to two seconds for a 4 GB index that is not in the system's cache.

## How it works

A summary; [ARCHITECTURE.md](ARCHITECTURE.md) has the file format, the write and search paths and where each kind of change goes.

- **Immutable segments** (`crates/engine/src/segment.rs`): each batch of documents is written to one file holding the documents in zstd blocks, an FST term dictionary, compressed posting lists, a filter index and sort columns. Files are read through `mmap`, so process memory does not grow with index size.
- **Lossless documents**: documents are stored and returned with the same key order and the same numbers (`9.50` stays `9.50`; large integers are not rounded).
- **Integrity**: every section and every document block carries a CRC32. A damaged block produces an error, never wrong data.
- **Crash safety** (`crates/engine/src/index.rs`, `crates/server/src/platform.rs`): a write creates new files, calls `fsync` and replaces the manifest with an atomic `rename`. Tasks are recorded on disk before the server answers `202`; unfinished ones are repeated on start and partial files are removed.
- **Bounded memory while writing** (`crates/server/src/api.rs`, `crates/engine/src/index.rs`): the request body goes to disk as it arrives and documents are read 5,000 at a time. A segment under construction is closed when its in-memory index uses up its share of the budget (24 MB across all threads). Freed memory is returned to the system after each task.
- **Parallel indexing**: the reading thread only extracts the primary key and assigns field ids; documents are routed by the hash of their key to the indexing threads, which each tokenize their share and build their own segments. A document always lands on the same thread, so the last version wins.
- **Merging** (`segment::merge`): above 8 segments, the smallest ones that are of a size are merged without reindexing, up to a million documents per segment: documents are copied block by block and dictionaries are merged as sorted streams, dropping deleted documents. The compressed document blocks of a segment without deletions are copied as they are.
- **Search** (`crates/engine/src/search.rs`): typo tolerance walks the dictionary with an automaton computed on the fly. Range filters use precomputed unions over spans of 64 and 4,096 values. A lone search spreads segments across cores; concurrent searches use one thread each.
- **Stable memory** (`tune_allocator`): on Linux the server limits what glibc keeps after freeing. `TRAILHOUND_TRACE=1` prints the time spent in each phase of each search.

## Comparison with Meilisearch 1.58.0

Two sets of measurements with the scripts in `tests/bench/`: English Wikipedia, at 1 million and 6.4 million articles, and the 300,000 synthetic documents the project started with. Meilisearch was measured on 2026-11-24; Trailhound's figures are from 2026-11-25, after the changes to sorting, range filters and merging listed in the [changelog](CHANGELOG.md), on the same machine. Short version of the real-text results: Trailhound indexes faster, in a fraction of the memory and of the disk, and runs in 512 MB where Meilisearch is killed while indexing; Meilisearch answers more searches per second in every test, by more the larger the index, and its latency tail is far better.

### Real text: English Wikipedia

**Data.** The 2023-11-01 snapshot of English Wikipedia that Wikimedia publishes on Hugging Face (`wikimedia/wikipedia`, CC BY-SA 3.0 and GFDL), pinned to one revision. `tests/bench/wiki.sh` downloads it, checks the files against their published SHA-256 and writes one document per article: `id` (the page id), `title`, `url`, `text` and `length`. `text` is the opening of the article, whole paragraphs up to 1,000 characters; `length` is the size of the whole article in characters. Documents average 800 bytes of NDJSON.

| Set | Articles | Download | NDJSON |
| --- | --- | --- | --- |
| `wiki.sh 1m` | the first 1,000,000 of 7 files spread over the snapshot | 2.2 GB | 0.8 GB |
| `wiki.sh all` | all 6,407,814 | 11.6 GB | 5.1 GB |

**Settings.** `searchableAttributes` is `["title", "text"]`, `length` is filterable and `length` and `title` are sortable; everything else is each engine's default. Both servers run in Docker with the same limit of 65,536 open files (with Docker's default of 1,024, Meilisearch fails on the larger set with "No file descriptors available").

**Queries.** 100 search requests that `wiki.py` derives from the documents with a fixed seed, ten of each kind: one word of a title (`word`), a whole title of two to four words (`title`), two or three consecutive words of a text (`words`), a title with one letter dropped, swapped or replaced (`typo`), a title cut short in its last word (`prefix`), two consecutive words in quotes (`phrase`), three words of the same text with `matchingStrategy: all` (`all`), a word with a filter on `length` (`filter`), a word sorted by `length` or `title` (`sort`), and no words at all, a range of `length` sorted by `length` or `title` (`browse`). All ask for 10 hits and only the `id`. Each set has its own 100 queries, drawn from its own documents, so a kind of query is not word for word the same at the two sizes.

**Machine.** A 32-core x86 PC with 31 GB of RAM and an NVMe disk, under WSL2 with Docker. There was no ARM run this time.

```sh
# On Linux with Docker, Python 3 and curl; the Linux binary goes in ~/trailhound-bench/target/release.
bash tests/bench/wiki.sh 1m                                   # or: all
DATASET=wiki-1m bash tests/bench/bench.sh
DATASET=wiki-1m TENANTS=8 bash tests/bench/multitenant.sh     # or TENANTS=1; MEM=512m CPUS=0-1 for the small size
DATASET=wiki-1m RANK_DOCS=1000000 bash tests/bench/ranking.sh
```

#### One index, one client (`bench.sh`)

The documents go in as batches of 100,000. The search figures are over the 100 queries, each run 15 times, taking the median of each.

| | 1 million: Trailhound | 1 million: Meilisearch | 6.4 million: Trailhound | 6.4 million: Meilisearch |
| --- | --- | --- | --- | --- |
| Indexing | 32 – 36 s | 108 – 120 s | 211 – 219 s | 658 – 852 s |
| Peak memory while indexing | 111 MB | 6,995 MB | 146 MB | 21,961 MB |
| Disk | 649 MB | 6,524 MB | 4,123 MB | 52,140 MB |
| Start-up after a restart | 0.3 s | 0.4 – 0.7 s | 0.4 – 0.6 s | 0.5 – 0.9 s |
| Start-up with an empty page cache | — | — | 1.0 – 2.3 s | 0.4 s |
| Memory at rest after a restart | 6 MB | 78 MB | 9 MB | 77 MB |
| Memory after the 1,500 searches | 170 MB | 941 – 954 MB | 734 MB | 1,248 MB |
| of which the process's own (not file pages) | 3 MB | 124 MB | 15 MB | 259 MB |
| Search: median of the 100 queries | 3.4 ms | 2.6 – 3.4 ms | 6.0 ms | 5.5 ms |
| Search: 90th percentile of the 100 | 18.6 ms | 4.2 – 5.1 ms | 123 ms | 11.4 ms |
| Search: slowest of the 100 | 52.3 ms | 6.7 – 7.9 ms | 216 ms | 23.8 ms |

#### Under load, on the whole machine (`multitenant.sh`)

8, 32 and 128 connections send a random mix of the 100 queries for 10 s, to one index holding everything (`TENANTS=1`) or to eight tenants holding an eighth each (eight tenants with their own key in Trailhound; eight indexes in Meilisearch).

| 1 million articles | Trailhound, 8 tenants | Meilisearch, 8 indexes | Trailhound, 1 index | Meilisearch, 1 index |
| --- | --- | --- | --- | --- |
| Queries per second (8 / 32 / 128 connections) | 3,100 / 7,400 / 8,500 | 4,000 – 4,900 / 8,000 – 10,300 / 7,700 – 9,900 | 1,100 / 2,700 / 2,700 | 2,400 – 2,700 / 5,000 – 5,700 / 4,900 – 5,700 |
| p50 latency, ms | 1.4 / 2.7 / 9.8 | 1.5 – 1.8 / 2.9 – 3.7 / 13 – 16 | 3.5 / 6.3 / 37 | 2.7 – 3.1 / 5.1 – 5.9 / 22 – 26 |
| p99 latency, ms | 38 / 50 / 98 | 3.4 – 4.1 / 6.1 – 7.7 / 16 – 21 | 48 / 71 / 177 | 6.9 – 7.8 / 12 – 14 / 31 – 36 |
| Highest memory under load | 526 – 669 MB | 9,370 – 9,639 MB | 211 – 409 MB | 3,628 – 3,851 MB |
| of which the process's own | 34 – 72 MB | 4,500 MB | 12 – 195 MB | 1,100 – 1,200 MB |
| Loading the documents | 9 – 11 s | 167 – 186 s | 34 s | 110 – 120 s |
| Disk | 677 MB | 7,474 MB | 649 MB | 6,525 MB |

| 6.4 million articles | Trailhound, 8 tenants | Meilisearch, 8 indexes | Trailhound, 1 index | Meilisearch, 1 index |
| --- | --- | --- | --- | --- |
| Queries per second (8 / 32 / 128 connections) | 890 / 2,300 / 2,400 | 2,800 / 7,900 / 7,600 | 170 / 400 / 430 | 720 / 1,800 / 1,700 |
| p50 latency, ms | 2.8 / 5.3 / 20 | 2.1 / 3.8 / 17 | 12 / 23 / 103 | 7.6 / 16 / 71 |
| p99 latency, ms | 76 / 105 / 430 | 15 / 8.4 / 22 | 435 / 641 / 2,441 | 76 / 53 / 135 |
| Highest memory under load | 1,261 – 1,471 MB | 10,671 – 10,805 MB | 875 – 1,872 MB | 10,855 – 15,920 MB |
| of which the process's own | 28 – 257 MB | 4,800 MB | 48 – 368 MB | 1,600 – 1,700 MB |
| Loading the documents | 100 s | 549 s | 208 s | 852 s |
| Disk | 4,153 MB | 38,119 MB | 4,123 MB | 50,171 MB |

Median latency of each kind of query with 8 connections on one index, in ms. This is where the two engines differ most:

| | `word` | `title` | `words` | `typo` | `prefix` | `phrase` | `all` | `filter` | `sort` | `browse` |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 million: Trailhound | 1.7 | 3.1 | 19.6 | 3.9 | 6.6 | 5.5 | 7.5 | 3.3 | 3.1 | 1.1 |
| 1 million: Meilisearch | 2.0 | 3.5 | 3.7 | 3.6 | 2.7 | 1.6 | 3.9 | 3.3 | 3.1 | 3.0 |
| 6.4 million: Trailhound | 6.2 | 14.4 | 232 | 19.6 | 7.4 | 23.8 | 23.9 | 6.6 | 17.7 | 3.2 |
| 6.4 million: Meilisearch | 4.4 | 6.7 | 14.0 | 8.5 | 4.6 | 3.1 | 7.9 | 9.7 | 6.5 | 12.9 |

#### 512 MB and 2 vCPU

The same load with the server held to two cores and 512 MB (`MEM=512m CPUS=0-1`), the size of the smallest cloud instances.

| Trailhound | 1 million, 8 tenants | 1 million, 1 index | 6.4 million, 8 tenants | 6.4 million, 1 index |
| --- | --- | --- | --- | --- |
| Loading the documents | 25 s | 36 s | 208 s | 282 s |
| Queries per second (8 / 32 / 128 connections) | 220 / 190 / 410 | 160 / 140 / 140 | 45 / 44 / 56 | 11 / 10 / 19 |
| p50 latency, ms | 28 / 147 / 299 | 38 / 206 / 912 | 162 / 682 / 2,940 | 594 / 3,863 / 15,061 |
| p99 latency, ms | 177 / 516 / 565 | 200 / 614 / 1,523 | 558 / 1,938 / 4,327 | 2,214 / 8,372 / 22,435 |
| Highest memory under load | 113 – 183 MB | 119 – 137 MB | 151 – 168 MB | 165 – 213 MB |

Meilisearch was killed for lack of memory while loading the 1 million articles, in each of five attempts: two with eight indexes and two with one, with its default settings, and one with `MEILI_MAX_INDEXING_MEMORY=200Mb` and one indexing thread. It was not tried on the larger set. An index built elsewhere and copied onto a machine of this size was not tried either, so these tables say nothing about how fast Meilisearch searches in 512 MB.

#### Result order (`ranking.sh`)

For each of the 100 queries on the 1 million articles, Trailhound's top 10 hits are sent to Meilisearch to be scored, and the per-rule scores are compared with those of Meilisearch's own top 10, position by position.

- In 81 of the 100 the top 10 is equivalent: the same scores on every rule, in the same order. In 32 of those the ids are identical too; in the rest the differences are documents tied on every rule.
- In 63 of the 100 both engines count the same number of matches. Most of the others differ by a few documents in thousands; a few differ widely (`bar` as a prefix: 27,628 against 60,198), because Trailhound follows a limited number of dictionary terms per query word.
- 56 of the 100 pass both checks.
- Where the top 10 differs, the first rule to differ is `wordPosition` in 11 queries, `exactness` in 4, `proximity` in 3 and `attributeRank` in 1. The `exactness` cases are the difference listed under [What is missing](#what-is-missing). The `wordPosition` and `proximity` cases are partly the rules skipped above 100,000 tied candidates, which real text reaches with common words. The rest are not explained yet.

By kind of query, the top 10 is equivalent in 10 of 10 for `browse`, `word` and `sort`, 9 for `typo` and `filter`, 8 for `title` and `prefix`, 7 for `all`, and 5 for `words` and `phrase`.

#### How to read these tables

Where Meilisearch does better:

- **Searches per second, everywhere.** On 1 million articles Meilisearch serves from a little more to two and a half times as many queries; on 6.4 million, three to four and a half times as many. It uses less CPU per query: on one index of 6.4 million, about 9 ms of CPU against about 50 ms. At the median of a single kind of query Trailhound is now ahead in some: a word, a title and a listing with no words on 1 million articles, where filters and sorted searches are level, and filters and listings on 6.4 million.
- **The latency tail.** Meilisearch's p99 is one to three times its p50 in most runs (up to ten times with 8 connections on 6.4 million articles). Trailhound's is five to forty times its p50: 177 ms against 36 ms at 1 million, and 2.4 s against 135 ms on one index of 6.4 million with 128 connections.
- **Growth with the size of the index.** From 1 to 6.4 million articles, Meilisearch's median for `words` goes from 3.7 to 14 ms and Trailhound's from 20 to 232 ms. Trailhound computes proximity and word position while searching, reading the positions of every candidate; Meilisearch keeps them precomputed on disk, which is most of why its index is ten times larger. These are the searches that saturate Trailhound under a mixed load, and nothing in this round of changes touched them: queries per second on one index of 6.4 million are what they were.
- **Queries with common words and phrases** are where the gap is widest, as the table by kind shows.
- **One search at a time on 1 million articles.** That index now ends up in a single segment. Under load that is cheaper, but a search that runs alone spreads its segments over threads, and with one segment the slowest queries take longer than they did with seven: the 90th percentile of the 100 queries went from 10 to 19 ms and the slowest from 24 to 52 ms.
- **Start-up with an empty page cache.** Trailhound reads the index once to verify its checksums: 1.0 to 2.3 s for 4 GB, against 0.4 s.
- **Fewer limits on what a query sees.** Trailhound follows a limited number of dictionary terms per query word (2,400 over an index, or 300 per segment) and skips `proximity` and `wordPosition` above 100,000 tied candidates; on real text both limits are reached, and the result order and the count of matches then differ from Meilisearch's.

Where Trailhound does better:

- **Memory while indexing**: 111 MB against 7 GB at 1 million and 146 MB against 22 GB at 6.4 million. When indexing ended, the process's own memory was under 20 MB.
- **Disk**: ten to thirteen times less. While it indexes, Trailhound needs more than the final size, because a merge writes the new segment before the old ones go: the data folder peaked at 1,391 MB for the 649 MB index and at 5,310 MB for the 4,123 MB one.
- **Indexing time**: three to four times faster on one index, and five to twenty times faster on eight tenants, which Trailhound indexes at the same time.
- **Memory at rest**: 6 to 9 MB for one index, 13 to 17 MB for eight tenants, against 77 MB.
- **It runs in 512 MB**, including indexing 6.4 million articles there, where Meilisearch could not index 1 million. What it serves at that size is slow: about 140 to 400 queries per second on 1 million articles and 10 to 60 on 6.4 million, with medians of seconds on the larger set once more than a few connections ask at once.

Things to keep in mind:

- **Memory under load is mostly file pages.** Both engines read their index through `mmap`, and the pages a search touches count as memory of the process until the system takes them back. For Trailhound that is nearly all of the figure (734 MB after the searches on 6.4 million articles, 15 MB of it its own; after a start with an empty cache the same searches left 1,856 MB mapped). It is memory the system can reclaim, as the 512 MB runs show, but it is not free: with less memory than index, searches wait for the disk.
- **Figures vary between runs.** Indexing times moved by 10 to 40% and queries per second by up to 20% between repetitions on this machine; ranges are given where there were several runs, and single figures are one run. Trailhound's p99 with eight tenants of the 1 million articles was 9, 13 and 51 ms in one run and 38, 50 and 98 ms in the one in the table. Trailhound's start-up after a restart was under half a second in most runs and 2.5 s in one, with eight tenants of the 1 million articles. Meilisearch's 6.4 million figures are one run each.
- **The load generator** is a Python script on the same machine and competes with the server for CPU; under the 512 MB limit only the server is held to two cores.
- **The documents are short.** They are the opening of each article, up to 1,000 characters. Whole articles, which are several times longer, were not measured.
- **Not tested:** ARM; searches while documents are being written; updates and deletions at this scale; facets (this data has no field to facet on); highlighting and cropping; more than eight tenants; and Meilisearch with any setting other than the ones named above.
- The figures are for the code in this repository on the dates above. Typo tolerance, sorting, range filters, memory at rest and merging were changed while taking them (see the [changelog](CHANGELOG.md)); version 0.1.0 is slower and uses more memory than these tables say.

### Synthetic documents

300,000 documents of about 320 bytes (95 MB of NDJSON) with an 80-word vocabulary, written by `tests/bench/gen.js`. With so few distinct words, typo tolerance and the dictionary cost almost nothing, which flatters Trailhound's search speed: compare with the tables above. The x86 figures for Trailhound were taken again on 2026-11-25 with the current code, on the same 32-core PC, and Meilisearch's on 2026-11-24; the ARM figures are from version 0.1.0 on a Mac M1 Max (Docker with 8 CPUs and 4 GB) and were not repeated.

#### Result order (`ranking.sh`)

For 22 queries (one or several words, typos, prefixes, filters, sorting), Trailhound's top 10 hits are sent to Meilisearch to be scored. In all 22, the per-rule scores match those of Meilisearch's own top 10 position by position and both count the same matches, both with the default `searchableAttributes` and with `["title", "description"]`. In 9 to 11 of them, depending on the run and the configuration, the ids are identical too; in the rest, the only differences are documents tied on every rule.

#### One index, one client (`bench.sh`, x86)

| Measure | Trailhound | Meilisearch |
| --- | --- | --- |
| Disk | 63 MB | 601 MB |
| Memory at rest after a restart | 6 MB | 78 MB |
| Memory after 150 searches | 22 MB | 194 MB |
| Peak memory while indexing | 81 MB | 2,105 MB |
| Indexing, 3 batches of 100,000 | 3.4 – 3.9 s | 5.4 s |
| Search (median, 10 different queries) | 0.8 – 7.3 ms | 1.3 – 2.8 ms |

The 300,000 documents now end up in one segment. A search that runs alone can no longer spread segments over threads, which is why the slowest of these ten queries, the ones with two or more common words, take 6 to 7 ms where they took 2.4 to 4 ms; under load, below, nothing is lost. Indexing was 2.7 s and its peak memory 66 MB before the last change to merging.

#### Several tenants at once (`multitenant.sh`)

Eight tenants with 37,500 documents each (eight tenants with their own key in Trailhound; eight indexes in Meilisearch), with 8, 32 and 128 connections sending a mix of the 10 queries for 10 s.

| x86, 32 cores | Trailhound | Meilisearch |
| --- | --- | --- |
| Queries per second (8 / 32 / 128 connections) | 5,100 / 10,800 / 13,100 | 5,800 / 11,900 / 11,100 |
| p50 latency, ms | 1.6 / 2.8 / 8.7 | 1.2 / 2.5 / 11.3 |
| p99 latency, ms | 2.4 / 5.5 / 26 | 2.5 / 4.8 / 13.9 |
| Highest memory under load | 109 – 206 MB | 1,099 – 1,129 MB |
| Loading the eight tenants | 1.3 – 2.2 s | 6.1 s |
| Disk | 75 MB | 544 MB |

| x86, 4 cores | Trailhound | Meilisearch |
| --- | --- | --- |
| Queries per second (8 / 32 / 128 connections) | 3,000 / 3,700 / 3,700 | 4,000 / 3,900 / 3,800 |
| p50 latency, ms | 2.4 / 6.9 / 33 | 1.9 / 8.0 / 33 |
| p99 latency, ms | 6.7 / 27 / 59 | 4.4 / 11 / 40 |
| Highest memory under load | 94 – 103 MB | 844 – 874 MB |
| Loading the eight tenants | 1.8 s | 16.2 s |

| ARM (Mac M1 Max), 8 Docker cores, version 0.1.0 | Trailhound | Meilisearch |
| --- | --- | --- |
| Queries per second (8 / 32 / 128 connections) | 2,700 / 4,000 / 4,300 | 3,600 / 4,300 / 4,200 |
| p50 latency, ms | 2.7 / 7.3 / 28 | 2.0 / 7.1 / 30 |
| p99 latency, ms | 6.0 / 20 / 58 | 5.1 / 14 / 42 |
| CPU used in 10 s (8 / 32 / 128) | 43 / 64 / 70 s | 37 / 44 / 43 s |
| Memory under load | 92 – 113 MB | 874 – 981 MB |
| Loading the eight tenants | 2.2 s | 11.0 s |
| Disk | 72 MB | 552 MB |

#### Simulated t4g.nano size: 512 MB

| | Trailhound, 2 vCPU x86 | Trailhound, 1 vCPU x86 | Trailhound, 2 vCPU ARM (0.1.0) | Meilisearch, 2 vCPU (x86 and ARM) |
| --- | --- | --- | --- | --- |
| Loading the eight tenants | 2.7 s | 5.1 s | 3.7 s | killed for lack of memory |
| Queries per second (8 / 32 / 128) | 2,200 / 2,400 / 2,400 | 1,260 / 1,280 / 1,280 | 1,550 / 1,650 / 1,660 | — |
| p50 latency, ms | 3.0 / 8.8 / 48 | 4.7 / 12 / 89 | 4.5 / 16 / 74 | — |
| p99 latency, ms | 12 / 56 / 103 | 53 / 105 / 200 | 15 / 51 / 116 | — |
| Memory under load | 89 – 97 MB | 77 – 82 MB | 80 – 98 MB | — |

How to read these tables:

- The ARM runs used a Mac M1 Max with Docker limited to 2 vCPUs and 512 MB, not a real t4g.nano: M1 cores are faster than Graviton2 cores and a nano runs on CPU credits, so figures there will be lower.
- At the nano size, Meilisearch was tried with `MEILI_MAX_INDEXING_MEMORY` at 200 Mb and at 100 Mb and one indexing thread; in every case the system killed it for memory while loading.
- Meilisearch uses less CPU per query (on ARM, 1.0 ms against 1.6 ms): it keeps precomputed on disk what Trailhound computes while searching. With few connections it serves more queries; with many, the two are level. On real text they are not level: see above.
- Trailhound's latency tail (p99) is worse: some queries cost considerably more than others.
- The load generator is a Python script on the same machine and competes with the server for CPU.
- The `proximity` and `wordPosition` rules, and the part of `exactness` that looks at the start of the attribute, are skipped for groups of more than 100,000 tied candidates.

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

See [ROADMAP.md](ROADMAP.md) for what to work on, [ARCHITECTURE.md](ARCHITECTURE.md) for how the code is organized, [CONTRIBUTING.md](CONTRIBUTING.md) for what a pull request needs, [SECURITY.md](SECURITY.md) to report a vulnerability, and [CHANGELOG.md](CHANGELOG.md) for what changed in each version. The logo, colours and voice are described in [`brand/guide.html`](brand/guide.html).

## License

[MIT](LICENSE) © Multi API SpA.
