# Contributing

## The checks

Four of them, and CI runs exactly these:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo doc --no-deps --locked          # with RUSTDOCFLAGS=-D warnings
cargo test --locked
```

`--locked` everywhere is deliberate: every dependency but `libc` is a git
revision, so `Cargo.lock` is the only record of which tree of tOS was built.
If you change the version in `Cargo.toml`, run `cargo update -p blinkterm` so
the lock file agrees, or CI will tell you it does not.

The lint set is a `[lints]` table in `Cargo.toml` rather than flags in the
workflow, so the answer is the same on a laptop and on a runner. CI adds
`-D warnings`; locally they stay warnings so work in progress still builds.

`tools/` is checked too:

```sh
shellcheck --severity=warning tools/run.sh
ruff check --select E9,F tools/
```

Syntax and pyflakes only, not style — those are stdlib-only proof-of-concept
scripts and some of what a style rule objects to is deliberate.

## The engine tests

`cargo test` runs the unit tests and **skips** everything in `tests/engine.rs`,
saying so as it goes. Those are the tests this repository exists for: a real
Chromium, real frames, and the compositor's own reader parsing what would go
down a pane's pseudoterminal.

They run only when you name an engine:

```sh
BLINKTERM_ENGINE=chromium-shell cargo test --release -- --test-threads=1
```

**Naming the engine is the consent.** A machine with a Chromium on it did not
thereby agree to have one started, so the tests will not go looking. `--release`
because several of them assert on timings, and `--test-threads=1` because each
starts an engine of its own and two painting at once make the scroll tests
measure the machine instead of the program.

No Chromium to hand? Push the branch — the `engine` job runs it against a
pinned bookworm image, and prints which Chromium it used.

## Bumping the tOS revision

`tos-term`, `tos-platform`, `tos-preview` and `tos-compositor` are pinned to one
revision of [tOS](https://github.com/m96-chan/tOS), currently `c7677bde`. They
are the decoders, the terminal handling and the cell arithmetic — as much of the
program as `src/` is.

Move them **together**, never one at a time:

1. Change `rev = "..."` on all four in `Cargo.toml` to the same new commit.
2. `cargo update` — the lock file has to follow.
3. Run the engine tests. A decoder or a graphics-command change is exactly the
   kind of thing that compiles and then renders wrongly, and the engine tests
   are what would notice.
4. Say in the changelog that the revision moved and why.

A protocol change — a new graphics format, an escape sequence, a keyboard flag —
is taken by moving the pin, not by vendoring or reimplementing it here.

## Style

Follow what is there. The comments in this codebase explain **why**, at length,
including the measurements a decision rests on; a patch that changes a decision
should change the paragraph that argued for it. `unsafe` blocks carry a
`SAFETY:` comment saying what makes them sound, and `clippy::undocumented_unsafe_blocks`
enforces it.

If you change something a person can notice — a key, a flag, a default, the
Rust floor — add a line to `CHANGELOG.md` under `Unreleased`.

## Releases

See [RELEASING.md](RELEASING.md) for what the version number promises and how a
tag is cut.
