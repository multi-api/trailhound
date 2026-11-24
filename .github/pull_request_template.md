<!-- One change per pull request. See CONTRIBUTING.md for the details behind this list. -->

## What this does

<!-- What changes, and why. Link the issue if there is one: "Closes #123". -->

## How it was tested

<!-- The commands you ran and what they showed. For a bug fix, the test that failed before and passes now. -->

## Checklist

- [ ] `cargo test --release` passes, and `cargo clippy --all-targets -- -D warnings` is clean.
- [ ] A test was added or updated for the bug fixed or the behaviour added.
- [ ] If the HTTP API changed: the SDK suites (`tests/sdk*`) still pass against a running server, or CI ran them.

Only where they apply; delete the rest:

- [ ] **Ranking** changed: `tests/bench/ranking.sh` is still at 22 of 22.
- [ ] **Memory for speed**: the change holds more data in memory to be faster. Numbers for both, from `tests/bench/multitenant.sh`, are in the description.
- [ ] **On-disk format** changed: the magic number in `crates/engine/src/segment.rs` is bumped and `CHANGELOG.md` says so.
- [ ] **Admin panel** changed: `crates/server/src/panel.html` was rebuilt with `npm run build` and committed, and new strings are in `panel/src/lib/i18n.tsx` in English and Spanish.
- [ ] **Behaviour users will notice**: `CHANGELOG.md` and, if needed, `README.md` are updated.
