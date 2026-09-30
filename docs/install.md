# Installing blinkterm

## Installing

### Prebuilt binaries

Every release after v0.3.0 has an archive per target on its
[GitHub Release](https://github.com/m96-chan/blinkterm/releases), built by
GitHub's runners from the tagged commit:

| archive target | for |
|---|---|
| `x86_64-unknown-linux-gnu` | x86-64 Linux with glibc 2.35 or newer (Debian 12, Ubuntu 22.04, Fedora 36 on) |
| `aarch64-unknown-linux-gnu` | the same on arm64 |
| `x86_64-unknown-linux-musl` | x86-64 Linux with any libc or none: Alpine, a bare container, a tOS rootfs |
| `aarch64-unknown-linux-musl` | the same on arm64 |
| `aarch64-apple-darwin` | a Mac with Apple silicon |
| `x86_64-apple-darwin` | an Intel Mac |

The gnu build is the one for an ordinary Linux; the musl build is static,
links nothing at run time, and is the one for anywhere a glibc is too old or
missing. Each archive is `blinkterm-<version>-<target>.tar.gz` and unpacks to
a directory of the same name holding `blinkterm`, the README, the licence and
the changelog. For arm64 Linux:

```sh
v=X.Y.Z t=aarch64-unknown-linux-gnu      # the release's version, as on its page
curl -fsSLO https://github.com/m96-chan/blinkterm/releases/download/v$v/blinkterm-$v-$t.tar.gz
tar xzf blinkterm-$v-$t.tar.gz
install -m 0755 blinkterm-$v-$t/blinkterm ~/.local/bin/
```

With [`cargo-binstall`](https://github.com/cargo-bins/cargo-binstall), which
picks the archive for your machine (gnu before musl on Linux) and falls back
to building when there is none:

```sh
cargo binstall blinkterm
```

To check an archive before running it: each release has a `SHA256SUMS`, and
every archive carries a build provenance attestation, a statement signed
through [Sigstore](https://www.sigstore.dev) that GitHub's runner built this
file from this repository at the tagged commit, in the release workflow.

```sh
curl -fsSLO https://github.com/m96-chan/blinkterm/releases/download/v$v/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS      # shasum -a 256 -c --ignore-missing on a Mac
gh attestation verify blinkterm-$v-$t.tar.gz --repo m96-chan/blinkterm
```

The Mac binaries are not signed or notarised. Fetched with `curl` as above,
the binary runs; one saved by Safari or Finder is quarantined, and macOS
refuses to start it until `xattr -d com.apple.quarantine blinkterm` has
removed the attribute — the same as the engine below.

### From source

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
command, not a prebuilt one; the prebuilt ones are
[above](#prebuilt-binaries). The engine below is
still yours to install, and `brew` says so when it is done. It installs on
macOS too, from v0.2.0 on.
The formula lives
in this repository, at `packaging/homebrew/blinkterm.rb`, and
[m96-chan/homebrew-tap](https://github.com/m96-chan/homebrew-tap) carries a
copy.

### The engine

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

Anything Chromium-shaped will do, with one caution. `blinkterm` looks at
`--engine <path>` first, then `$BLINKTERM_ENGINE`, then `engine = <path>` in
the [settings](configuration.md#settings), then on `PATH` for `chrome-headless-shell`,
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
