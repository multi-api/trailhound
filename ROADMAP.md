# Roadmap

Trailhound 0.1 does one thing well: it serves many small search tenants from very little memory, behind the Meilisearch API. This page lists what we want next, in the order we think matters, so that you can pick something up.

Nothing here is assigned or dated. Every item is open for contribution:

- For anything larger than a bug fix, open an issue first (or comment on the existing one) and say how you plan to do it. It saves you from building something that cannot be merged.
- [ARCHITECTURE.md](ARCHITECTURE.md) explains how the code is put together and where each kind of change goes.
- [CONTRIBUTING.md](CONTRIBUTING.md) says how to build, test and what a pull request needs.

Items that already have an issue link to it; that is the place to ask questions or say you are taking it. Status is one of **not started**, **in progress** or **done**. If you start on something, say so in its issue and we will update this page.

## 1. Compatibility where people already are

People arrive with an application that already talks to Meilisearch. The fewer things that break when they point it at Trailhound, the better.

### Client libraries

The HTTP API is the same for every SDK, but each SDK exercises it differently (routes, query parameters, how it waits for tasks, which fields it expects in answers). The clients below are run against the server on every commit; the versions are in the README.

| Item | What and why | Status |
| --- | --- | --- |
| JavaScript | 34 checks with the official `meilisearch-js` in `tests/sdk`, run on every commit. | Done |
| [PHP and Laravel Scout](https://github.com/multi-api/trailhound/issues/3) | A test suite with `meilisearch-php` in `tests/sdk-php` and one with Scout's Meilisearch driver, from a minimal Laravel application, in `tests/sdk-laravel`. Laravel is where a large share of small Meilisearch deployments live, and they are the ones a low-memory server suits best. | Done |
| [Python](https://github.com/multi-api/trailhound/issues/2) | The same with the official `meilisearch` Python package, in `tests/sdk-python`. | Done |
| [Go](https://github.com/multi-api/trailhound/issues/4) | The same with `meilisearch-go`, in `tests/sdk-go`. | Done |

### Meilisearch features that are missing

In the order we expect people to miss them.

| Item | What and why | Status |
| --- | --- | --- |
| [`distinctAttribute`](https://github.com/multi-api/trailhound/issues/7) | Return one hit per value of an attribute (one result per product, not per variant). The setting is already accepted and stored, but search ignores it. | Not started |
| [CSV uploads](https://github.com/multi-api/trailhound/issues/5) | Accept `text/csv` on the document routes, with Meilisearch's header syntax for types. Many imports start from a spreadsheet export. | Not started |
| [Tenant tokens (JWT)](https://github.com/multi-api/trailhound/issues/9) | Tokens signed with an API key that carry search rules (a forced filter per index). They let a browser search only the rows its user may see, without a backend in between. | Not started |
| [Task cancellation and deletion](https://github.com/multi-api/trailhound/issues/8) | `POST /tasks/cancel` and `DELETE /tasks`. A wrong 200 MB import should be stoppable without restarting the server. | Not started |
| [Geosearch](https://github.com/multi-api/trailhound/issues/10) | The `_geo` field, `_geoRadius` and `_geoBoundingBox` filters and `_geoPoint` sorting. Needed by any store locator or listing site. | Not started |

Smaller gaps, each a good self-contained change: multi-word synonyms (only one-word synonyms are applied), [`typoTolerance.disableOnAttributes`](https://github.com/multi-api/trailhound/issues/6) (stored, not applied), index swaps, federated search, and the known differences listed in the [README](README.md#what-is-missing).

## 2. Easy adoption

| Item | What and why | Status |
| --- | --- | --- |
| Documentation site | The README is long enough to need a table of contents. A site with a getting-started guide, the API reference and the operations guide, built from Markdown in this repository. | Not started |
| Copy-paste examples | Small working projects under `examples/`: Laravel Scout, a Node backend, a browser search box with InstantSearch, Docker Compose behind Caddy. People evaluate by running something. | Not started |
| Migration between on-disk formats | The segment format (`THSEG002`) may change before 1.0. The one change so far kept reading the older format, but a change that cannot do that leaves loading the documents again as the only way across. An upgrade step that rewrites old segments in place removes the main reason not to depend on a 0.x version. | Not started |
| Hot backup | A command or endpoint that writes a consistent copy of a tenant, or of the whole data folder, while the server keeps running. Today a safe backup needs a volume snapshot or a stop. | Not started |
| Importing Meilisearch dumps | Read a Meilisearch `.dump` file and load its indexes, settings and keys into a tenant. This is the migration path for anyone who cannot easily replay their documents. | Not started |

## 3. Trust

A search engine that keeps other people's data has to earn it. These are the things we would want to see before depending on a project like this one.

| Item | What and why | Status |
| --- | --- | --- |
| A benchmark with real data, at scale | The same comparison on real text: English Wikipedia at 1 million and 6.4 million articles, with the download and benchmark scripts in `tests/bench/` so anyone can repeat it. The results are in the README. Still missing: a run on ARM, whole articles instead of their opening, and writes during searches. | Done for x86 |
| CPU per query and p99 latency | Meilisearch uses less CPU per query and has a better latency tail, because it precomputes on disk what Trailhound computes while searching, and on real text the gap grows with the size of the index (see the README). The benchmark shows where the time goes now: reading the positions of every candidate for `proximity`, `wordPosition` and `exactness` when the words are common, which is what saturates a server under a mixed load; walking each segment's dictionary for typos and prefixes; and paying both once per segment. The candidates: keep each word's best position per document next to its posting list, stop ranking once the page is full, and keep the typo derivations of recent words. Larger segments were measured and wait for the first two: with fewer, larger segments the light searches are 20 to 60% faster and the ones that read many positions are slower. A one-sided range that covers most of an index (`length > 100`) still reads a set of documents nearly as large as the index. Each has to be paid in disk, not in memory. | Started: the typo walk, sorted searches and ranges on one field are cheaper |
| Result order on real text | With the synthetic documents all 22 queries rank as in Meilisearch. On Wikipedia, 81 of 100 queries have an equivalent top 10 and 63 count the same number of matches. The known causes are the limit on the dictionary terms followed per query word, `exactness` not telling an attribute equal to the query from one that starts with it, symbols such as `$` and `=` that Meilisearch indexes as words, and the rules skipped above 100,000 tied candidates; some `wordPosition` differences are not explained yet. | Not started |
| [Fuzzing and crash tests in CI](https://github.com/multi-api/trailhound/issues/11) | Fuzz targets for the filter parser, the document scanner and the segment reader (a damaged file must give an error, never a panic or wrong data), and a test that kills the server in the middle of writes and checks what it finds on restart. | Not started |
| [API keys stored as hashes](https://github.com/multi-api/trailhound/issues/13) | Tenant and API keys are stored in clear text in the data folder. Storing a hash means a leaked backup does not leak working keys. It changes what the admin panel and `GET /keys` can show, which needs a decision. | Not started |
| [Prometheus metrics](https://github.com/multi-api/trailhound/issues/12) | A `/metrics` endpoint with request counts and latencies, task queue depth, and documents, disk and searches per tenant. Today monitoring means polling `/stats` and the admin API. | Not started |

## 4. Contributor experience

| Item | What and why | Status |
| --- | --- | --- |
| Roadmap, architecture document, issue and pull request templates | So that someone new can find a task, find the code and know what a pull request needs. | Done |
| Clippy in CI | `cargo clippy --all-targets` runs on every pull request with warnings as errors. | Done |
| Formatting with `rustfmt` | The code was not written with `rustfmt`'s defaults; running it today rewrites about 2,300 lines in all 14 Rust files. A one-off reformat in a commit of its own (or a `rustfmt.toml` that matches the current style), then `cargo fmt --check` in CI. | Not started |
| Tests for the server crate | The engine has its own tests; the HTTP API, tenants, quotas and task replay are covered only through the JavaScript SDK test. Rust tests that start a server on a temporary folder would make those areas safe to change. | Not started |

## Out of scope

Saying no to these keeps the project small enough to stay good at what it does.

- **Clustering and replication.** A Trailhound server is a single node. High availability is a restart on a persistent volume, or a second server fed with the same documents. We will not add consensus, sharding or replication.
- **Billing.** Trailhound has tenants, quotas and usage counters so that you can build plans on top of it. Invoices, payments, plans and customer accounts belong in your application.

Things that are neither listed above nor ruled out here (vector and hybrid search, TLS, rate limiting, access logs) have not been decided. If one of them matters to you, open an issue and make the case.
