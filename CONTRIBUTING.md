# Contributing to Trailhound

Thanks for your interest. Trailhound is at an early stage, so the most useful contributions right now are bug reports with a way to reproduce them, and compatibility gaps with the Meilisearch API.

## Reporting a bug

Open an issue with:

- the Trailhound version (`GET /version`) and how you run it (binary, Docker, operating system);
- the requests that show the problem, ideally as `curl` commands with a small set of documents;
- what you expected and what you got. If Meilisearch answers differently, include its answer and version.

Security problems go through a private report instead: see [SECURITY.md](SECURITY.md).

## Building and testing

You need a stable Rust toolchain. Node 20 or later is only needed for the SDK test and the admin panel.

```sh
cargo build --release
cargo test --release                 # engine tests
```

SDK compatibility test (34 checks with the official `meilisearch-js`), against a running server:

```sh
target/release/trailhound --data-dir ./data-test --addr 127.0.0.1:7711 --master-key test-master-key
cd tests/sdk && npm install && npm test
```

The benchmarks in `tests/bench/` compare against Meilisearch in Docker and are described in the README.

## Layout

| Path | Contents |
| --- | --- |
| `crates/engine` | The search engine: segments, indexing, merging, search, filters, tokenizer |
| `crates/server` | The HTTP server: Meilisearch-compatible API, tenants, tasks, admin API |
| `panel` | The admin panel (React, Tailwind, shadcn/ui) |
| `tests/sdk` | Compatibility test with the official JavaScript SDK |
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
- Add or update a test when you fix a bug or add behaviour. Ranking changes should keep `tests/bench/ranking.sh` at 22 of 22.
- Memory use is the point of the project. A change that makes indexing or search faster by holding more data in memory needs numbers for both, measured with `tests/bench/multitenant.sh`.
- Changes to the on-disk segment format must bump the magic number in `crates/engine/src/segment.rs` and be called out in `CHANGELOG.md`.

By contributing you agree that your contribution is licensed under the [MIT license](LICENSE).
