# Releasing

How a version of `blinkterm` is cut, and what the number means.

## What the version number promises

SemVer, **on the command-line surface**: the flags, the key bindings, the
environment variables, and the configuration keys when there are any. Those are
what somebody's fingers and somebody's scripts depend on.

- **major** — a key binding changes meaning, a flag is removed, `$BLINKTERM_ENGINE`
  stops being consulted, a default flips in a way that changes what you see.
- **minor** — a new key, a new flag, a new engine on the candidate list, a
  raised Rust floor.
- **patch** — a fix with no new surface.

**`lib.rs` is not a stable API.** The crate is a binary; the library target
exists so that `tests/` can reach the modules, and `cargo doc` builds it because
the reasoning lives in those doc comments and is worth reading. Every module in
it may change shape in a patch release. Nothing should depend on `blinkterm` as
a library, and nothing can conveniently do so anyway — see the crates.io note
below.

## Before a release

- CI green on `main`, all five jobs in `CI`. The `engine` job is the one that
  matters most and the one that cannot run on a laptop without a Chromium;
  `mac` is the same tests on macOS.
- The `Homebrew` workflow is green too: it builds `packaging/homebrew/blinkterm.rb`
  the way `brew install` would, on Linux and on macOS, and runs on pull requests that touch it, the
  lock file, or the workflow.
- `CHANGELOG.md` `Unreleased` section says what a person would notice.
- The README's Rust floor still matches `Cargo.toml`; the `msrv` job checks
  this, so a green `main` has already confirmed it.

## Cutting it

```sh
# 1. the number, in one place
$EDITOR Cargo.toml                    # version = "X.Y.Z"
cargo update -p blinkterm             # Cargo.lock carries it too, and CI
                                      # builds --locked, so a stale lock is a
                                      # red build rather than a surprise

# 2. the changelog: rename Unreleased to X.Y.Z with today's date,
#    and open a fresh empty Unreleased above it
$EDITOR CHANGELOG.md

# 3. land it through a pull request like anything else, then tag the
#    commit that main ends up on
git tag -a vX.Y.Z -m "blinkterm X.Y.Z"
git push origin vX.Y.Z
```

## Homebrew

The formula is `packaging/homebrew/blinkterm.rb` here, and
[m96-chan/homebrew-tap](https://github.com/m96-chan/homebrew-tap) carries a
copy at `Formula/blinkterm.rb`; `brew install m96-chan/tap/blinkterm` reads
the copy. The formula is edited here, where the `Homebrew` workflow builds it,
and copied over once the tag exists, so the tap never holds a formula that was
not built first.

After step 3 above, with the tag pushed:

```sh
# 4. the stable block: GitHub's archive of the tag, and its checksum
url=https://github.com/m96-chan/blinkterm/archive/refs/tags/vX.Y.Z.tar.gz
curl -fsSL "$url" | sha256sum
$EDITOR packaging/homebrew/blinkterm.rb
#   above `license "MIT"`:
#     url "https://github.com/m96-chan/blinkterm/archive/refs/tags/vX.Y.Z.tar.gz"
#     sha256 "<the sum>"
#   `head` stays. Land it through a pull request: the Homebrew workflow
#   installs the stable spec now that there is one, then HEAD.

# 5. copy it to the tap
git clone https://github.com/m96-chan/homebrew-tap
cp packaging/homebrew/blinkterm.rb homebrew-tap/Formula/blinkterm.rb
(cd homebrew-tap && git commit -am "blinkterm X.Y.Z" && git push)
```

`brew bump-formula-pr --url "$url" --sha256 <sum> m96-chan/tap/blinkterm` does
step 4's edit and step 5's commit against the tap in one go, but it edits the
tap's copy rather than the canonical file and wants a GitHub token in
`HOMEBREW_GITHUB_API_TOKEN`; it is the right tool once the tap has more than
one formula and no canonical copy elsewhere, and not before.

The archive url is what Homebrew expects for a GitHub tag, and GitHub keeps
the checksum of a tag's archive stable. The build inside `brew` runs `cargo
install --locked` against the `Cargo.lock` in the archive and fetches the tOS
revisions from GitHub as it goes; Homebrew allows a build network access by
default, on Linux (Landlock) as on macOS, and the formula does not opt out.
Once the stable block is in, the README's `brew install` line drops `--HEAD`.

Step 5 is what the release workflow will do once there is one (#22): a job
after the tag that checks out the tap with a `HOMEBREW_TAP_TOKEN` secret,
copies the formula, and pushes. Until then it is two commands.

## What goes in the release notes

The changelog section, and two things that are not in the repository's diff but
are part of what the release *is*:

- **the tOS revision** the dependencies are pinned to — `c7677bde` at the time
  of writing, from `Cargo.toml`. It is the PNG and JPEG decoders, the terminal
  handling and the cell arithmetic, so it is as much of the program as `src/`.
- **the Chromium the engine tests passed against**. The `engine` job pins a
  `chrome-headless-shell` by version and checksum and prints its `--version`;
  take it from that run's log, and the mac-arm64 one the `mac` job prints
  from the same run's. The program does not
  ship an engine, so "it works" is always "it worked against this one".

## Not on crates.io

`cargo publish` will refuse this crate: every dependency but `libc` is a git
revision, and crates.io does not accept git dependencies. That is not an
oversight — pinning a revision is how a protocol change is taken deliberately
rather than inherited — but it does mean the tag and its artifacts are the
release, and `cargo install --git` is the install. Distribution beyond that is
[#24](https://github.com/m96-chan/blinkterm/issues/24), and building binaries
for a tag is [#22](https://github.com/m96-chan/blinkterm/issues/22).

## If this gets tedious

[`release-plz`](https://release-plz.dev/) opens a release pull request on every
merge to `main` — version bumped, changelog written from the commits — and tags
when it is merged. It works without crates.io publishing. Worth reaching for at
the point where the list above is being followed by hand for the third time.
