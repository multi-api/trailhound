# Contributing to Trailhound

Thanks for your interest. Trailhound is at an early stage, so the most useful contributions right now are bug reports with a way to reproduce them, and compatibility gaps with the Meilisearch API.

If you want to write code, the [roadmap](ROADMAP.md) lists what is wanted and what is out of scope, and [ARCHITECTURE.md](ARCHITECTURE.md) explains how the code is organized and where each kind of change goes. For anything larger than a bug fix, open an issue before you start.

## Reporting a bug

Open an issue with the bug report form, or the compatibility form when Trailhound answers differently from Meilisearch. Both ask for:

- the Trailhound version (`GET /version`) and how you run it (binary, Docker, operating system);
- the requests that show the problem, ideally as `curl` commands with a small set of documents;
- what you expected and what you got. If Meilisearch answers differently, include its answer and version.

Security problems go through a private report instead: see [SECURITY.md](SECURITY.md).

## Building and testing

You need a stable Rust toolchain. Node 20 or later is only needed for the JavaScript SDK test and the admin panel; Python, Go, PHP and Composer only for the SDK test of each.

```sh
cargo build --release
cargo test --release                 # engine tests
cargo clippy --all-targets -- -D warnings
```

CI runs clippy with warnings as errors. It does not check formatting: the code is not formatted with `rustfmt`, so please do not run `cargo fmt` over files you touch; keep the style of the code around your change.

SDK compatibility tests, against a running server. There is one suite per client; run the ones whose toolchain you have, and CI runs them all:

```sh
target/release/trailhound --data-dir ./data-test --addr 127.0.0.1:7711 --master-key test-master-key

cd tests/sdk && npm install && npm test                                    # JavaScript
cd tests/sdk-python && pip install -r requirements.txt && python compat.py # Python
cd tests/sdk-go && go run .                                                # Go
cd tests/sdk-php && composer install && php compat.php                     # PHP
cd tests/sdk-laravel && composer install && cp .env.example .env && php compat.php   # Laravel Scout
```

A check that needs a feature Trailhound does not have is marked as an expected failure with the reason (`missing` in the suites), not removed; when you implement the feature, the suite tells you to drop the mark.

The benchmarks in `tests/bench/` compare against Meilisearch in Docker and are described in the README.

## Layout

| Path | Contents |
| --- | --- |
| `crates/engine` | The search engine: segments, indexing, merging, search, filters, tokenizer |
| `crates/server` | The HTTP server: Meilisearch-compatible API, tenants, tasks, admin API |
| `panel` | The admin panel (React, Tailwind, shadcn/ui) |
| `tests/sdk` | Compatibility test with the official JavaScript SDK |
| `tests/sdk-python`, `tests/sdk-go`, `tests/sdk-php` | The same test with the official Python, Go and PHP clients |
| `tests/sdk-laravel` | A minimal Laravel application that tests Laravel Scout's Meilisearch driver |
| `tests/bench` | Benchmarks and the ranking comparison against Meilisearch |
| `brand` | Logo, colours and brand guide |

## Changing the admin panel

The panel is built into a single file, `crates/server/src/panel.html`, which is committed so that `cargo build` does not need Node. After changing anything under `panel/`, rebuild it and commit the result with your change:

```sh
cd panel
npm install
npm run build
```

CI fails if the committed `panel.html` does not match the sources. Every string shown to the user goes in `panel/src/lib/i18n.tsx`, in English and in Spanish.

## Pull requests

- Keep each pull request to one change, and say what it does and how you tested it.
- Add or update a test when you fix a bug or add behaviour. Ranking changes should keep `tests/bench/ranking.sh` at 22 of 22, and should not lower the count on Wikipedia (`DATASET=wiki-1m RANK_DOCS=1000000`, 77 of 100 with an equivalent top 10).
- Memory use is the point of the project. A change that makes indexing or search faster by holding more data in memory needs numbers for both, measured with `tests/bench/multitenant.sh`, on the Wikipedia data as well as the synthetic documents.
- Changes to the on-disk segment format must bump the magic number in `crates/engine/src/segment.rs` and be called out in `CHANGELOG.md`.

By contributing you agree that your contribution is licensed under the [MIT license](LICENSE).
