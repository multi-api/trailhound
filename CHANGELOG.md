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

Cheaper searches on large indexes, and less memory while indexing, measured on the same Wikipedia benchmark (figures for 6.4 million articles; the README has the tables):

- **New on-disk segment format, `THSEG002`.** Segments of the previous format, `THSEG001`, are still read: an existing data folder opens as it is and nothing has to be loaded again. New segments are written in the new format and old ones are rewritten into it as merges reach them, so the changes below that depend on it apply to an old index little by little; loading the documents again applies them at once. There is no way back: a server older than this one refuses a folder that holds a `THSEG002` segment. The new format adds, per sortable field, the documents in sorted order (4 bytes per document that has a value; the index of the benchmark is 1% larger), and a note of which filterable fields hold one number per document.
- A sorted search walks the stored order of the field until the page is full, instead of reading the sort value of every candidate. When the filter bounds the field that is sorted (`price >= 10`, sorted by `price`), the walk starts at the bound. With few candidates, and on segments of the old format, the search works as before.
- Two bounds on the same field (`length >= 756 AND length < 909`) are read as one range where no document has two numbers in that field. They used to be read as two ranges that each cover most of the index, which was most of the time of such a search. A numeric range also joins its sets of documents in one pass. Together with the sorted walk, a search with no words, a range and a sort went from 14 – 44 ms to 2 – 3.5 ms, and a word with a range from a median of 12 ms to 6 ms.
- Merging picks its segments by size: the smallest ones that are of a size are joined in one pass, up to 16, and a large segment is not written again to take in a few documents. It used to join the five smallest. Loading the 6.4 million articles takes about a quarter less time (from 295 – 311 s to 211 – 219 s). An index of a million documents now ends up in one segment instead of seven. The largest segment a merge makes is still a million documents; larger ones were measured and are not better for every search (see `ARCHITECTURE.md`).
- A merge no longer holds its sort columns, nor the positions of a word, in memory, and it lets go of the pages of the segments it reads as it advances. Opening a segment lets go of them as it verifies the checksums. Peak memory while indexing went from 949 MB to 146 MB, nearly all of it file pages before and after, and the highest memory seen just after a restart from 302 MB to 14 MB.
- The limit on the dictionary terms a query word follows is 300 per segment or the segment's share of 2,400 for the whole index, whichever is more. It was 300 per segment whatever its size, so an index merged into fewer segments matched fewer prefixes and typos than the same index in many.
- A search running alone uses up to eight threads whatever the number of segments. It used to spread one thread per segment, so on an index merged into one segment, as an index of a million documents now is, it ran on a single thread, and the slowest of the benchmark's queries on 1 million articles had gone from 24 to 52 ms. The candidates of a segment are now cut into runs of documents that are read on a thread each, the dictionary is walked per word, and the postings of the terms found are read in runs. `TRAILHOUND_SEARCH_THREADS` sets how many threads such a search may use.
- Reading positions jumps to the first candidate of a posting list and steps over the documents between candidates eight bytes at a time, instead of going through the list byte by byte from its start. This is what makes the runs above worth their threads, and it makes every search that reads positions cheaper, alone or under load.
- `proximity`, `wordPosition` and `exactness` share one pass over the posting lists of a set of candidates. Each used to make its own, and `wordPosition` one more for every bucket the rules before it had made. The proximity of a document walks the lists of two words side by side instead of comparing every occurrence with every other.
- The heap is trimmed at 256 KB instead of 1 MB (`TRAILHOUND_MALLOC_TRIM_KB`), so that the threads of a search do not leave the process larger: its own memory after the benchmark's searches is 7 to 8 MB.
- With these four, on Wikipedia and one client, the 90th percentile of the 100 queries goes from 20 to 7 ms on 1 million articles and from 124 to 13 ms on 6.4 million, and the slowest from 54 to 12 ms and from 227 to 28 ms. Under load the server answers 1.1 to 3 times as many searches. Results are the same, and nothing changes on disk.
- `attributeRank` places a document that matches every word without a typo by the attributes where it matches that way. It used to count an attribute that only holds a misspelling of the word, which could put a document with the word in its text ahead of one with the word in its title. On Wikipedia, the queries whose top 10 is equivalent to Meilisearch's went from 77 to 81 of 100.

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
