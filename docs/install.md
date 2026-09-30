# Installing blinkterm

## Installing

Rust 1.87 or newer; see [Checks](development.md#checks):

```sh
cargo install --locked blinkterm
```

`--locked` builds with the lock file the release was tested with. For what is
on `main` rather than the last release:

```sh
cargo install --locked --git https://github.com/m96-chan/blinkterm
```

Or with [Homebrew](https://brew.sh), on Linux or macOS:

```sh
brew install m96-chan/tap/blinkterm
```

That builds from source too — the tap's formula asks Homebrew for a Rust and
runs the same `cargo install --locked` — so it is the same binary by a shorter
command, not a prebuilt one; prebuilt binaries are
[#22](https://github.com/m96-chan/blinkterm/issues/22). The engine below is
still yours to install, and `brew` says so when it is done. It installs on
macOS too, from v0.2.0 on.
The formula lives
in this repository, at `packaging/homebrew/blinkterm.rb`, and
[m96-chan/homebrew-tap](https://github.com/m96-chan/homebrew-tap) carries a
copy.

## The engine

Then a browser engine, which `blinkterm` does not ship — a Chromium is 482 MB
installed, twice a tOS ISO, and a choice about which browser somebody runs.
The one it is tested against is `chrome-headless-shell` from
[Chrome for Testing](https://googlechromelabs.github.io/chrome-for-testing/):
the same Chromium with no desktop browser UI compiled in. `blinkterm` fetches
that one when asked:

```sh
blinkterm --install-engine
```

It downloads `chrome-headless-shell` 153.0.8010.52 — the version CI installs
and the engine tests pass against, and no other — for this machine
(`linux64`, `mac-arm64` or `mac-x64`), over https only, and checks the zip
against a SHA-256 compiled into `blinkterm`: a zip that does not match is
thrown away and nothing is installed. It unpacks it under
`$XDG_DATA_HOME/blinkterm/engine/153.0.8010.52/` (`~/.local/share/blinkterm/engine/153.0.8010.52/`
without `$XDG_DATA_HOME`), in a 0700 directory beside the profile, starts it
once as `--doctor` would, and prints where it is and the `rm -r` line that
removes it. A second run finds it there and fetches nothing. Nothing is
downloaded otherwise: no run, setting or key reaches for the network on its
own.

It needs `curl` and `unzip`, which a Mac has and Debian or Ubuntu gets with
`apt install curl unzip`. On Linux the engine also wants the usual Chromium
libraries; if one is missing it does not answer, the report quotes its own
error, and `deb.deps` in the unpacked directory lists the packages. What CI
installs on bookworm is

```sh
apt-get install libasound2 libatk1.0-0 libatk-bridge2.0-0 libatspi2.0-0 \
  libdbus-1-3 libdrm2 libexpat1 libgbm1 libglib2.0-0 libnspr4 libnss3 \
  libudev1 libx11-6 libxcb1 libxcomposite1 libxdamage1 libxext6 libxfixes3 \
  libxi6 libxkbcommon0 libxrandr2 libxrender1 fonts-noto-cjk
```

The installed engine is found before anything on `PATH`, since installing it
was a choice; `--engine`, `$BLINKTERM_ENGINE` and `engine =` still win over
it. Only the pinned version is looked at, so after an upgrade of `blinkterm`
that moves the pin, the old one is no longer found; `--install-engine` fetches
the new one beside it and lists the old one for you to remove. Chrome for
Testing publishes no Linux build for arm64; there, install your distribution's
`chromium`.

### By hand

The same engine, unpacked where you like. On x86-64 Linux:

```sh
curl -fsSLO https://storage.googleapis.com/chrome-for-testing-public/153.0.8010.52/linux64/chrome-headless-shell-linux64.zip
unzip chrome-headless-shell-linux64.zip -d /opt
BLINKTERM_ENGINE=/opt/chrome-headless-shell-linux64/chrome-headless-shell blinkterm
```

It wants the usual Chromium libraries (its `deb.deps` lists them) and, if you
read any CJK, `fonts-noto-cjk`: without it every Japanese glyph is a box.

On a Mac with Apple silicon, the same version's `mac-arm64` build (an Intel
Mac wants `mac-x64` in both places):

```sh
curl -fsSLO https://storage.googleapis.com/chrome-for-testing-public/153.0.8010.52/mac-arm64/chrome-headless-shell-mac-arm64.zip
unzip chrome-headless-shell-mac-arm64.zip -d ~/engine
BLINKTERM_ENGINE=~/engine/chrome-headless-shell-mac-arm64/chrome-headless-shell blinkterm
```

The frameworks it needs are in the zip and the fonts are the system's. A zip
fetched with `curl` runs at once; one saved by Safari or Finder is
quarantined, and macOS refuses to start it until
`xattr -dr com.apple.quarantine ~/engine/chrome-headless-shell-mac-arm64`
has removed the attribute.

### How the engine is looked for

Anything Chromium-shaped will do, with one caution. `blinkterm` looks at
`--engine <path>` first, then `$BLINKTERM_ENGINE`, then `engine = <path>` in
the [settings](configuration.md#settings), then for the engine
`--install-engine` installed, then on `PATH` for `chrome-headless-shell`,
`chromium`, `chromium-browser`, `google-chrome` and `chromium-shell`, in that
order — and on macOS, where a browser is an app rather than a command, then
for Google Chrome and Chromium in `/Applications` and `~/Applications`. On a
Mac the engine is also given `--use-mock-keychain`, so that Chromium never
asks the Keychain for its cookie key in a dialog nobody can see. Debian's
`chromium-shell` is last because it is Chromium's
`content_shell`, not a headless shell: it keeps a DevTools port open beside
the pipe whatever it is told, answers a page's dialogs itself, and does not
close when asked, so a kept profile is not flushed. It renders pages; it does
not keep the promises below.
