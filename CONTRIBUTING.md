# Contributing

## The checks

Four of them, and CI runs exactly these:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo doc --no-deps --locked          # with RUSTDOCFLAGS=-D warnings
cargo test --locked
```

`--locked` everywhere is deliberate: CI tests what is committed, not what
cargo would resolve today. If you change the version in `Cargo.toml`, run `cargo update -p blinkterm` so
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
BLINKTERM_ENGINE=/opt/chrome-headless-shell-linux64/chrome-headless-shell \
  cargo test --release -- --test-threads=1
```

**Naming the engine is the consent.** A machine with a Chromium on it did not
thereby agree to have one started, so the tests will not go looking. `--release`
because several of them assert on timings, and `--test-threads=1` because each
starts an engine of its own and two painting at once make the scroll tests
measure the machine instead of the program.

No Chromium to hand? Push the branch — the `engine` and `mac` jobs run it
against pinned Linux and macOS builds, and print which Chromium they used.

## Code that came from tOS

`src/png.rs`, `src/jpeg.rs`, `src/inflate.rs` and `src/tty.rs` came from tOS's
`tos-term` and `tos-platform`, and `src/fit.rs` from its image viewer, all at
[tOS](https://github.com/m96-chan/tOS) `c7677bde`; each file's first paragraph
says where. They were git dependencies until this program ran on a Mac and
wanted to be on crates.io, which takes no git dependencies. They are this
crate's code now: change them here, and a fix that tOS also needs is a fix to
carry over by hand, in either direction.

`vendor/tos-term` is different. It is tOS's whole terminal, copied at the same
revision, and the tests parse this program's output with it — the terminal the
program was written for, as it was. It is a path dev-dependency, so it is not
published, and it is not a workspace member, so this crate's lints do not
apply to it. Leave it as tOS wrote it; to take a newer tOS terminal, copy
`compositor/tos-term` over it whole, say which revision in its `Cargo.toml`,
and run the engine tests. `tests/support/imagefile.rs` is the compositor's
`t=s` reader, copied the same way and kept the same way.

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
