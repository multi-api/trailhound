# Changelog

All notable changes to Trailhound are recorded here. The project follows [semantic versioning](https://semver.org); before 1.0, minor versions may change the API and the on-disk format.

## 0.1.0 — unreleased

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
