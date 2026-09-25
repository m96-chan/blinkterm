<!--
What changed and why. If it changes a decision, change the paragraph in the
code that argued for the old one -- the comments here carry the reasoning and
the measurements, and a stale one is worse than none.
-->

## What this changes

## Why

## Checks

CI runs these four; ticking them means you ran them, not that you expect them
to pass.

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --locked -- -D warnings`
- [ ] `cargo doc --no-deps --locked` (with `RUSTDOCFLAGS=-D warnings`)
- [ ] `cargo test --locked`

Touching `tools/`?

- [ ] `shellcheck --severity=warning tools/run.sh`
- [ ] `ruff check --select E9,F tools/`

Touching anything the engine drives — frames, input, scrolling, tabs?

- [ ] `BLINKTERM_ENGINE=… cargo test --release -- --test-threads=1`, or said
      below that the `engine` job is doing it instead

## Notes

- [ ] Something a person can notice changed (a key, a flag, a default, the Rust
      floor) → there is a line in `CHANGELOG.md` under `Unreleased`
