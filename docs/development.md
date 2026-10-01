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
`<profile>/backend.log`, rotated by the backend that takes the profile lock
(the run before is `backend.log.1`): the engine's warnings, a window that lost its terminal, a page that was closed
because no window could be shown to have asked for it.

### Several terminals on one profile: the failure modes

`tests/hardening.rs` holds the backend (#83) to what it promises when
something goes wrong: two terminals starting at once on a free profile, the
frontend that started the backend leaving first, a frontend, the backend or
the engine killed outright, a start during the last window's shutdown, a
browser-wide setting that differs (a proxy), the socket and `engine.pgid` a
crash leaves, a terminal that stops reading beside one that keeps up, and
the cookie jar across a stop at the end of a grace. Like the window tests it
starts the real backend through `frontend::attach` and drives it with fake
frontends, with the hidden `--grace-ms` passed through to make the reconnect
grace short. Some of it is done by force to make it certain: the engine is
stopped (`SIGSTOP`) so that a shutdown takes its whole close timeout and an
attach is sure to land inside it, and a stand-in process group with
`--user-data-dir=<profile>` on its command line plays the orphaned engine
that `engine::reap_orphan` must kill (a real one cannot be kept: a stopped
engine whose backend is killed is an orphaned process group with a stopped
member, which the kernel ends with `SIGHUP`). One test there needs no
engine and runs everywhere: a profile path longer than a socket address
gets a private fallback for both sockets.

`tests/terminal.rs` runs the real binary in a pseudoterminal
(`tests/support/pty.rs`: `openpty`, the child in a session of its own with
the pty as its controlling terminal, `TIOCSWINSZ` with pixels so that
`--no-probe` has a cell size) with `--no-probe --frames raw --tmux off`, and
reads it through `tos_term` as `tests/engine.rs` does: the status row comes
up, `ctrl+q` exits 0 and the lock is let go once the backend has stopped, a
second pane on the same named profile gets its own row with the profile's
name, closing one pane leaves the other drawing, and a pane whose terminal
closes (the master end dropped, a hang-up) or whose frontend is killed costs
only its own window — the last one after the full fifteen-second grace,
which makes that test take about twenty seconds. On a Mac the frames go
inline (the tests set `SSH_CONNECTION`), because the terminal's reader looks
for shared memory under `/dev/shm`. CI's macOS job runs both files beside the
engine suite.

Reading `backend.log` after a failure: it is the log of the latest run, and
`backend.log.1` the one before. Every candidate backend a frontend starts
appends to it, and only the one that takes the profile lock rotates it, so a
candidate that loses appends nothing and removes nothing; one that fails
before the lock may add a line to the end. Each line starts
`blinkterm:` and says which window (`window 2 lost its terminal: …`, `window
2 was not taken back; its tabs are saved`), what was closed and why
(`closed a page the engine opened (<target>): no window asked for it`), and
an engine death (`… ; starting it again`); the engine's own complaints are
there between them. The tests' scratch profiles are under the temporary
directory as `blinkterm-it-hardening-*` and `blinkterm-it-terminal-*` and
are removed when a test ends, so to read the log of a failing one, run it
alone with a `sleep` added before its end, or copy the file in the test.

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
sound. The integration tests' — the pty and the process groups in
`tests/hardening.rs`, `tests/terminal.rs` and `tests/support/pty.rs` among
them — are held to the same lint, tOS's copied file reader excepted.

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
