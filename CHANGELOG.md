# Changelog

All notable changes to Trailhound are recorded here. The project follows [semantic versioning](https://semver.org); before 1.0, minor versions may change the API and the on-disk format.

## Unreleased

Compatibility with the official Meilisearch clients for Python, Go and PHP and with Laravel Scout, each now tested on every commit next to the JavaScript one.

- A path with a trailing slash is the same route as without it. `GET /tasks/` and `GET /keys/`, which is how `meilisearch-php` lists tasks and keys, answered `404`.
- `fieldDistribution` in the index and global stats counts only live documents. It used to include deleted and replaced documents until their segment was merged.
- `GET /tasks` accepts the `uids`, `canceledBy`, `reverse`, `beforeEnqueuedAt`, `afterEnqueuedAt`, `beforeStartedAt`, `afterStartedAt`, `beforeFinishedAt` and `afterFinishedAt` parameters, and `*` in the list filters. They were ignored, so a client asking for some tasks got all of them. An invalid `from`, uid or date is now a `400` instead of being ignored.

A benchmark on real text, English Wikipedia at 1 million and 6.4 million articles, next to the synthetic one (`tests/bench/wiki.sh`; the results, including where Meilisearch does better, are in the README). What it turned up and what was changed for it:

- Typo tolerance gives up on a dictionary term at its first letter when that letter rules it out, instead of walking the whole dictionary and discarding those terms afterwards. On 1 million articles a search for a word of 5 to 8 letters spent 4 to 5 ms there and now spends 1 to 2, and a longer word went from 20 to 35 ms down to 6 to 10. Results are the same.
- A sorted search reads the sort key in full only for the documents that can reach the page. It used to copy the string of every candidate, so sorting 100,000 candidates by title took 24 ms and as many allocations; it takes 2.5 ms now.
- A server at rest no longer counts its whole index as memory. Verifying the checksums on open left every page of the index mapped in the process: 343 MB at rest for 1 million articles, now 9 MB.
- Merging no longer compresses documents again. Merges of 400,000 documents or more recompressed all of them at zstd level 19, including those already compressed that way, which made indexing slower the larger the index was: 6.4 million articles took 39 minutes and now take between 5 and 6. Document blocks are 16 KB, where level 19 gains 2% over level 6; the index is about 1% larger.
- The end of a sentence or clause (`!`, `?`, `;`, and a full stop or comma followed by a space) keeps words apart, as in Meilisearch: a quoted phrase does not match across it and words on both sides do not count as close. Positions used to be counted straight through punctuation, which no synthetic document had. Documents indexed before this change keep their old positions until they are indexed again.
- The benchmark scripts take a dataset (`DATASET=wiki-1m`), a file of queries, the ports and the binary to test, report start-up time after a restart, the highest memory seen during each level of load and latency per kind of query, raise the open-file limit of both servers, and stop when an index does not hold every document it was sent.

- Roadmap, architecture document, issue forms and a pull request template.
- CI runs clippy with warnings as errors.

## 0.1.0 — 2026-11-24

First public version.

- Search engine with immutable, memory-mapped segments: zstd-compressed documents, FST term dictionary, posting lists, filter index and sort columns, with CRC32 on every section.
- Meilisearch-compatible API: indexes, documents (JSON and NDJSON), search with typo tolerance, prefix, filters, sorting, facets, highlighting and cropping, multi-search, settings, tasks, API keys, stats.
- Ranking rules and typo counting that match Meilisearch 1.58 on the 22 queries of `tests/bench/ranking.sh`.
- Multi-tenancy: isolated tenants with their own keys, task queue, quotas and usage counters.
- Bounded-memory, parallel indexing and streaming segment merges.
- Crash-safe writes: atomic manifest replacement and a task log replayed on start.
- Admin panel at `/_admin`, in English and Spanish, embedded in the binary.
- Docker image and Compose file.
- On-disk segment format `THSEG001`.
- Clean shutdown on SIGTERM and a health check in the Docker image.

Hardening from a security review before release:

- Filters nested more than 64 levels deep are refused. Before, a long chain of `NOT` or parentheses overflowed the stack and stopped the server, and as a delete-by-filter task it did so again on every restart.
- The admin API checks the key before reading the request body, limits bodies to 64 KB, and no longer accepts an empty or short admin key.
- Request bodies other than document uploads are limited to 4 MB.
- Finished tasks no longer keep their request payload, the task history is bounded in size as well as in count, and the task log is rewritten as it grows instead of only at start.
- A search is limited to 10 quoted phrases and 100 facets, and a multi-search request to 100 queries.
- The disk quota counts the upload being written, and a tenant over quota is refused before uploading.
- A key restricted to some indexes can no longer read the tasks of others through `GET /tasks/{uid}`.
- `expiresAt` on API keys is validated.
