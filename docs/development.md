# Development

## Tests

`cargo test` runs the unit tests and skips everything that needs an engine.
The tests in `tests/engine.rs` are the ones this repository is for: a real
Chromium, real frames, and tOS's terminal (`tos_term::Terminal`, copied into
`vendor/tos-term` and not published) with the compositor's own `ImageFiles`
installed parsing what would go down the pane's pseudoterminal.
They run only when `BLINKTERM_ENGINE` names the engine to use, and they say so
when they skip — naming the engine is the consent, because a machine with a
Chromium on it did not thereby agree to have it started.

```sh
BLINKTERM_ENGINE=/opt/chrome-headless-shell-linux64/chrome-headless-shell \
  cargo test --release -- --test-threads=1
```

`tests/remote.rs` runs the binary itself as `blinkterm --remote`, as `gh` or
`xdg-open` would, against a socket the test listens on; it needs no engine
and runs with the rest of `cargo test`.

`--release` because several of them assert on timings, and one at a time
because each starts a Chromium of its own: two engines painting at once on a
small machine make the scroll tests measure the machine. `tools/Dockerfile`
builds the bookworm image with the engine and the fonts in it if you would
rather not install a Chromium; `tools/` also holds the Python tools the design
was measured with, and has its own README.

## Checks

What CI runs, and what to run before pushing:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo doc --no-deps --locked          # RUSTDOCFLAGS=-D warnings in CI
cargo test --locked
```

The lint set is a `[lints]` table in `Cargo.toml` rather than a list of flags
in the workflow, so a laptop and a runner disagree about `-D warnings` and
nothing else. The one worth knowing about is
`clippy::undocumented_unsafe_blocks`: there are eighty-one `unsafe` blocks in
`src/`, nearly all of them one-line `libc` calls, and each says what makes it
sound.

CI also runs clippy, the unit tests and the real-engine suite on macOS. That
job exercises the `shm_open` frame path, app-bundle search and macOS scroll
clock that no Linux job can run; the `msrv` job separately checks the same
code for `aarch64-apple-darwin` on the floor toolchain.

`--locked` throughout, so that CI tests what is committed rather than what
cargo would resolve today, and a stale lock file is a red build.

The floor is Rust **1.87**: `src/` calls `is_multiple_of`. CI builds against
exactly the `rust-version` in `Cargo.toml`, so the number stays true.

`tools/` is checked too: `shellcheck --severity=warning tools/run.sh`, and
`ruff check --select E9,F tools/` — syntax and pyflakes, not style, since
those scripts are stdlib-only by design and some of what a style rule would
object to is deliberate.
