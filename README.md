# blinkterm

A real browser in a terminal pane. Not a text browser: the page is rendered by
a headless Chromium — Blink, the engine the page was made for — and arrives in
the pane as pixels, over the Kitty graphics protocol. Images, video, CSS,
JavaScript, the lot, in a terminal.

```sh
blinkterm https://example.com
```

`blinkterm` starts a Chromium as a child process, drives it over the Chrome
DevTools Protocol on a pipe only the two of them hold — no port, so nothing
else on the machine can drive the browser — takes the page's screencast,
decodes each frame, and hands the terminal raw pixels — turning the terminal's
own reports of keys and mouse back into CDP input events. The engine renders;
the terminal displays; this program is the wire between them and nothing else.

## The numbers

Measured at 1280x770 on two cores, no GPU, no display server:

| | |
| --- | --- |
| screencast, JPEG at quality 85 | 57.8 frames a second |
| the same, PNG | 33.8 frames a second |
| `Page.captureScreenshot` in a loop | 10 to 12, in every format CDP offers |
| a frame on the engine's side | about 185 kB |
| decoding one here | 8 ms |

So the frames are **JPEG while the page is moving, PNG when it stops**. After
150 ms with no frame the tab in front is asked for one lossless still and that
is what is left on the screen: text you are reading is always lossless, and the
lossy frames are the ones scrolling past, which nobody reads. A page that never
moves costs one still and then nothing.

The frames are decoded here rather than by the terminal, and go over as raw
pixels (`f=24`, `f=32`) rather than as a PNG the terminal has to decode on its
parse loop. In a tOS pane they go through `/dev/shm` as a name (`t=s`) instead
of base64 in the escape sequence, which is what keeps a 60 fps stream off a PTY
that carries 240 KB/s. Where the terminal does not read shared memory,
`blinkterm` notices the names piling up unread and falls back to sending the
pixels inline: correct, obviously correct, and slow.

## Where it runs

Any terminal that speaks all three of the Kitty graphics protocol, the Kitty
keyboard protocol and SGR mouse reporting — Kitty, WezTerm, Ghostty — and a
[tOS](https://github.com/m96-chan/tOS) pane, which is where it was written.
tOS owns the display: there is no X11 and no Wayland and there never will be,
so no browser can be ported to it in the ordinary sense. But a terminal that
speaks those three protocols is already a screen, a mouse and a keyboard, and
that is the whole of what an engine wants. None of that argument is about tOS,
so the same binary runs in the others. This repository exists because tOS's CI
has no Chromium to test against and this program is nothing without one.

## Installing

Rust 1.87 or newer — the floor comes from the dependency closure, not from
anything in `src/`; see [Checks](#checks):

```sh
cargo install --git https://github.com/m96-chan/blinkterm
```

Then a browser engine, which `blinkterm` does not ship — a Chromium is 482 MB
installed, twice a tOS ISO, and a choice about which browser somebody runs.
The one it is tested against is `chrome-headless-shell` from
[Chrome for Testing](https://googlechromelabs.github.io/chrome-for-testing/):
the same Chromium with no desktop browser UI compiled in. On x86-64 Linux:

```sh
curl -fsSLO https://storage.googleapis.com/chrome-for-testing-public/153.0.8010.52/linux64/chrome-headless-shell-linux64.zip
unzip chrome-headless-shell-linux64.zip -d /opt
BLINKTERM_ENGINE=/opt/chrome-headless-shell-linux64/chrome-headless-shell blinkterm
```

It wants the usual Chromium libraries (its `deb.deps` lists them) and, if you
read any CJK, `fonts-noto-cjk`: without it every Japanese glyph is a box.

Anything Chromium-shaped will do, with one caution. `blinkterm` looks at
`$BLINKTERM_ENGINE` first, then on `PATH` for `chrome-headless-shell`,
`chromium`, `chromium-browser`, `google-chrome` and `chromium-shell`, in that
order. Debian's `chromium-shell` is last because it is Chromium's
`content_shell`, not a headless shell: it keeps a DevTools port open beside
the pipe whatever it is told, answers a page's dialogs itself, and does not
close when asked, so a kept profile is not flushed. It renders pages; it does
not keep the promises below.

## Profiles

Cookies, logins, local storage and the rest of what a site keeps are kept
between runs, in `$XDG_DATA_HOME/blinkterm/profile` — or
`~/.local/share/blinkterm/profile` when `XDG_DATA_HOME` is not set. The
directory is made readable by you alone (0700), since a cookie is a login.

```sh
blinkterm --profile ~/work-profile https://example.com   # somewhere else
blinkterm --temp-profile https://example.com             # nothing kept
```

`--temp-profile` makes a fresh profile under the system's temporary directory
and removes it when `blinkterm` exits, including when it panics; one left by a
`blinkterm` that was killed outright is removed by the next one.

One `blinkterm` uses a profile at a time. A second one started on a profile
that is in use is refused, and told which pid has it; it does not quietly fall
back to a throwaway profile, because a login you thought was being kept and was
not is worse than an error. The lock is `blinkterm`'s own — an `flock` on
`blinkterm.lock` in the profile — because the headless shell has no lock of its
own and will happily run two engines on one cookie database.

A login survives a quit because the engine is asked to close
(`Browser.close`) and waited for, which is when Chromium writes its cookie
jar: measured against Chromium 141, that takes about two seconds, and every
other way of stopping it — `SIGTERM` included — loses what was not yet
written. So a `blinkterm` that is itself `SIGKILL`ed can lose the last thirty
seconds or so of cookies, which is Chromium's own flush interval.

A full `chromium` rather than the headless shell still writes
`~/.config/chromium/Crash Reports` whatever profile it is given; that
directory is Chromium's, not `blinkterm`'s.

## Downloads

A file a page offers — a link to a PDF, anything served as
`Content-Disposition: attachment`, an `<a download>` — is saved, under the
name the page suggested, and the status row says so:

    downloading report.pdf 42%        →        saved ~/Downloads/report.pdf

The page stays where it was: a link to a file is not a place to go. A name
that is already taken becomes `report (1).pdf`, the way a browser's shelf
does it, rather than overwriting. What the row says when a download did not
finish is `couldn't save report.pdf`; the engine gives no reason, and this
program adds one when it has it.

Files go to `$XDG_DOWNLOAD_DIR` when that is set, else to the
`XDG_DOWNLOAD_DIR` in `~/.config/user-dirs.dirs` — the file every desktop
reads — else to `~/Downloads`; `--download-dir <dir>` for somewhere else.
The directory is made, 0700, the first time something is saved into it.
The name a page suggests is checked here: one path component, no control
characters, no leading dot, at most 255 bytes.

Quitting cancels anything still coming and leaves no partial file. A
`blinkterm` that is killed outright can leave `<guid>.crdownload` in the
directory, which is the engine's partial file and is safe to delete.

## Keys

| | |
| --- | --- |
| `ctrl+l` | type a url |
| `ctrl+r` | reload |
| `alt+left` / `alt+right` | back and forward |
| `ctrl+t` | a new tab, with the cursor in the url bar |
| `ctrl+w` | close this tab; closing the last one quits |
| `ctrl+tab` / `ctrl+shift+tab` | the next tab, the one before |
| `alt+1` … `alt+9` | the nth tab |
| `ctrl+q` | quit |
| a page's dialog | its `alert`, `confirm`, `prompt` or "leave this page?" takes the top row: any key for an alert, `y`/`n` for a question, or type and `enter` for a prompt; `esc` says no |

Everything else goes to the page, including the mouse. A link that asks for a
new window gets a new tab, and the tab is switched to.

While a page is waiting on its dialog, only the tab keys and `ctrl+q` still
work, and the page gets no keys or mouse until it has its answer. A tab behind
that opens one is marked `!` in the strip — `2! Title` — and keeps its question
until you go to it. `ctrl+w` closes a tab without asking the page, so a tab
with something unsaved in it is closed without a "leave this page?".

## Tests

`cargo test` runs the unit tests and skips everything that needs an engine.
The tests in `tests/engine.rs` are the ones this repository is for: a real
Chromium, real frames, and `tos_term::Terminal` with the compositor's own
`ImageFiles` installed parsing what would go down the pane's pseudoterminal.
They run only when `BLINKTERM_ENGINE` names the engine to use, and they say so
when they skip — naming the engine is the consent, because a machine with a
Chromium on it did not thereby agree to have it started.

```sh
BLINKTERM_ENGINE=/opt/chrome-headless-shell-linux64/chrome-headless-shell \
  cargo test --release -- --test-threads=1
```

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
`clippy::undocumented_unsafe_blocks`: there are thirty-five `unsafe` blocks in
`src/`, nearly all of them one-line `libc` calls, and each says what makes it
sound.

`--locked` throughout, because every dependency but `libc` is a git revision
and `Cargo.lock` is the only record of which tree of tOS was built.

The floor is Rust **1.87**, and it comes from the dependency closure rather
than from anything in `src/` — `fontdue` calls `integer_sign_cast`. CI builds
against exactly the `rust-version` in `Cargo.toml`, so the number stays true.

`tools/` is checked too: `shellcheck --severity=warning tools/run.sh`, and
`ruff check --select E9,F tools/` — syntax and pyflakes, not style, since
those scripts are stdlib-only by design and some of what a style rule would
object to is deliberate.

## Contributing, releases, security

[CONTRIBUTING.md](CONTRIBUTING.md) has the checks, how to run the engine tests,
and how the tOS revision is moved. [RELEASING.md](RELEASING.md) says what the
version number promises — SemVer on the command-line surface, and `lib.rs` is
not a stable API — and how a tag is cut. [CHANGELOG.md](CHANGELOG.md) is what
changed.

[SECURITY.md](SECURITY.md) has the threat model and how to report something
privately. Worth reading before pointing this at a page you do not trust: it
says which parts are Chromium's problem, which are this program's, and which
are currently open holes.

## Licence

MIT. See [LICENSE](LICENSE).
