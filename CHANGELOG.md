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
