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
and runs with the rest of `cargo test`. `tests/profiles.rs` does the same for
`blinkterm profiles` and the profile selectors, in a scratch
`$XDG_DATA_HOME` with stdin from nowhere, so nothing in it can wait on the
picker; the picker itself is tested in `src/chooser.rs` over a fake stdin.

`tests/install.rs` runs `blinkterm --install-engine` for real: it downloads
the pinned `chrome-headless-shell` (about 100 MB) into a scratch
`$XDG_DATA_HOME`, and with `$BLINKTERM_ENGINE` removed and `PATH` empty checks
that the search finds it. It runs only when `BLINKTERM_NETWORK` is set, which
CI's two engine jobs do:

```sh
BLINKTERM_NETWORK=1 cargo test --release --test install
```

`--release` because several of them assert on timings, and one at a time
because each starts a Chromium of its own: two engines painting at once on a
small machine make the scroll tests measure the machine. `tools/Dockerfile`
builds the bookworm image with the engine and the fonts in it if you would
rather not install a Chromium; `tools/` also holds the Python tools the design
was measured with, and has its own README.

The six two-window tests at the end of `tests/engine.rs` (#83) measure what
an engine does with two windows: simultaneous screencasts, activation, input,
and which window a popup or a middle-clicked link belongs to. CI runs them
against the pinned headless shell only, and the two engines answer some of
those questions differently — the headless shell gives every target a window
of its own, Chrome puts a page opened from a page in its opener's window — so
when anything about windows or target routing changes, also run them by hand
against a full Chrome or Chromium and compare with the table in `src/tabs.rs`:

```sh
BLINKTERM_ENGINE=/usr/bin/google-chrome-stable cargo test --release --test engine -- \
  --test-threads=1 --nocapture two_windows activating_one input_to_one_window \
  a_popup_belongs an_openerless closing_every_target
```

`--nocapture` because the lines they print — frame rates, window ids, the
pipe order of the disposition and the target — are the measurement; the
assertions only hold what both engines agree on. One fact from there to keep
in mind while writing one: `Browser.getWindowForTarget` with an id the
headless shell does not have crashes the whole engine, so ask through
`app::window_of_target`, never directly.

`tests/windows.rs` is #83 itself: several terminals on one profile. Each
test starts the real backend the way a frontend does — `frontend::attach`
with the `Spawn` launcher, this build's binary as `--serve-fd`, a scratch
profile — and drives it with fake frontends: a connection each, made-up
terminal sizes, frames acknowledged as a terminal would, and the row's bytes
read for what they say. Two windows on one engine and one lock holder, each
at its own size and with its own input; one closed and the other painting;
the last one closed stopping the engine, the lock released and a cookie
kept for the next start; `--remote` reaching the window used last; a second
profile with an engine and a cookie jar of its own; a frontend that vanishes
and whose tabs are offered to the next window after the grace; and a link
middle-clicked in two windows at once landing in each window it was clicked
in. Engine-gated like `tests/engine.rs`, one at a time:

```sh
BLINKTERM_ENGINE=/opt/chrome-headless-shell-linux64/chrome-headless-shell \
  cargo test --release --test windows -- --test-threads=1
```

The grace a vanished frontend gets is fifteen seconds (`backend::GRACE`);
the tests pass the hidden `--grace-ms <n>`, which only a run with
`--serve-fd` accepts, to make it shorter. When a test leaves something
running, or a real run misbehaves, the backend's standard error is
`<profile>/backend.log`, rewritten by each backend a frontend starts: the
engine's warnings, a window that lost its terminal, a page that was closed
because no window could be shown to have asked for it.

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
`clippy::undocumented_unsafe_blocks`: there are ninety-eight `unsafe` blocks
in `src/` (`grep -o 'unsafe {' src/*.rs | wc -l`, the unit tests' among
them), nearly all of them one-line `libc` calls, and each says what makes it
sound.

CI also runs clippy, the unit tests and the real-engine suite on macOS. That
job exercises the `shm_open` frame path, app-bundle search and macOS scroll
clock that no Linux job can run; the `msrv` job separately checks the same
code for `aarch64-apple-darwin` on the floor toolchain.

`--locked` throughout, so that CI tests what is committed rather than what
cargo would resolve today, and a stale lock file is a red build.

The engine is pinned in one place, `install::SHELL_VERSION` and the three
checksums beside it in `src/install.rs`, and a unit test holds CI's two
`env:` blocks, `README.md`, `docs/install.md` and `--help` to it; the release
workflow's notes read it from there too. Moving to a
new engine is editing the constant, the three checksums (`mac-x64` by hand:
`curl -fsSL <url> | shasum -a 256`, since no job runs an Intel Mac) and
`ci.yml`, together.

The floor is Rust **1.87**: `src/` calls `is_multiple_of`. CI builds against
exactly the `rust-version` in `Cargo.toml`, so the number stays true.

`tools/` is checked too: `shellcheck --severity=warning tools/run.sh`, and
`ruff check --select E9,F tools/` — syntax and pyflakes, not style, since
those scripts are stdlib-only by design and some of what a style rule would
object to is deliberate.
