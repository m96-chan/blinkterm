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

- CI green on `main`, all five jobs in `CI`. The `engine` and `mac` jobs run
  the real-engine suite on Linux and macOS; they are the ones that cannot run
  on a laptop without a Chromium.
- The `Homebrew` workflow is green too: it builds `packaging/homebrew/blinkterm.rb`
  the way `brew install` would, and runs on pull requests that touch it, the
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

The tag runs `.github/workflows/release.yml`, and that is the release: it
checks the tag against `Cargo.toml` and `main`, builds the six archives on
runners of their own architecture and runs each one, writes `SHA256SUMS`
and a build provenance attestation, creates the GitHub Release with the
changelog section as its notes and the archives attached, and publishes to
crates.io. `cargo binstall blinkterm` works as soon as both are up: the
template in `Cargo.toml`'s `[package.metadata.binstall]` names the archives
on the release, and `tests/release.rs` keeps the two agreeing.

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
#   installs the stable spec, then HEAD, on Linux and on macOS.

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
install --locked` against the `Cargo.lock` in the archive and fetches `libc`
from crates.io as it goes; Homebrew allows a build network access by default,
on Linux (Landlock) as on macOS, and the formula does not opt out.
Once the stable block is in, the README's `brew install` line drops `--HEAD`.

Step 5 stays by hand. The release workflow could do it, but only with a
token that can push to the tap, and there is none in this repository's
secrets; a formula copy a few times a year is not worth a second long-lived
credential. The formula builds from source, so it does not use the release's
archives either.

## What goes in the release notes

The workflow writes them: the version's changelog section, without its
heading, and one thing that is not in the repository's diff but is part of
what the release *is*:

- **the Chromium the engine tests passed against**. The `engine` and `mac`
  jobs pin Linux and macOS `chrome-headless-shell` builds by version and
  checksum as `SHELL_VERSION` in `.github/workflows/ci.yml`, and the notes
  step reads it from there. The program does not ship an engine, so "it
  works" is always "it worked against this one".

`tests/release.rs` checks, on every push, that the changelog has a section
for the version `Cargo.toml` says, so a bump without step 2 is a red build
before it is an empty release. If the workflow cannot write the notes, or
`SHELL_VERSION` has been renamed and the engine line is missing, edit the
release by hand with the same two things; the engine jobs' logs print the
engine's `--version`.

## crates.io

The one dependency is `libc`. The code that used to come from tOS as git
revisions is in `src/` (see CONTRIBUTING.md), and the tests' copy of tOS's
terminal is a path dev-dependency that `cargo publish` leaves out, so the crate
publishes. `include` in `Cargo.toml` keeps the package to the program: the
tests are not in it, because they need that terminal.

Publishing is the tag's: pushing `vX.Y.Z` (step 3 above) runs
`.github/workflows/release.yml`, whose `verify` job checks that the tag names
the version in `Cargo.toml` and sits on `main` and builds the package as
crates.io will (`cargo publish --dry-run`). The `publish` job then waits for
all six archives to build, and only then uploads the package with the
`CRATESIO_KEY` repository secret. A version on crates.io cannot be replaced,
only yanked, which is why the checks come first, why a version never reaches
crates.io without its archives, and why the tag is the only way in.

To look before tagging:

```sh
cargo publish --dry-run --locked
cargo package --list                  # src/, Cargo.*, README, LICENSE, CHANGELOG
```

If the workflow cannot be used, `cargo login` and `cargo publish --locked`
from the tagged commit do the same by hand.

`cargo install` uses the published lock file only when `--locked` is passed,
which is why the README says `cargo install --locked blinkterm`.

## Trying the release workflow

A manual run is a dry run: it builds, runs and packs all six archives and
publishes nothing, whatever branch it is on.

```sh
gh workflow run release.yml --ref <branch>
gh run watch                              # or the Actions tab
gh run download <run-id> -n dist-X.Y.Z    # the archives, SHA256SUMS, notes.md
```

Each target's archive is also its own artifact, `blinkterm-X.Y.Z-<target>`.
The tag checks, the attestation, the release and crates.io are skipped.

## If a job fails after the tag

Re-run the failed jobs from the run's page (`gh run rerun <run-id> --failed`).
Nothing is published until every build has passed. The `release` job is safe
to run again: it edits a release that exists and replaces its assets. A
`publish` that already went through fails harmlessly on a second attempt,
with crates.io saying the version is already uploaded. If `gh release create`
refuses the tag right after it was pushed, that is GitHub not having caught
up; re-run the job.

## If this gets tedious

[`release-plz`](https://release-plz.dev/) opens a release pull request on every
merge to `main` — version bumped, changelog written from the commits — and tags
when it is merged. It works without crates.io publishing. Worth reaching for at
the point where the list above is being followed by hand for the third time.
