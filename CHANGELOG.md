# Changelog

Notable changes to `blinkterm`. The format is
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the versioning policy
and what counts as a breaking change are in [RELEASING.md](RELEASING.md).

Entries are for the person running the program. A refactor that nobody can
observe from outside does not need a line here; a changed key binding, a new
flag, a different default, a raised Rust floor all do.

## [Unreleased]

Nothing has been released yet. Everything below is what a first tag would
carry, and the list is kept as things land rather than written at the end.

### Added

- Tabs: `ctrl+t`, `ctrl+w`, `ctrl+tab`, `alt+1`..`alt+9`, and a page that asks
  for a new window gets one instead of being dropped.
- JPEG while the page moves, a lossless PNG still 150 ms after it stops.
- Frames through `/dev/shm` (`t=s`) as raw pixels where the terminal will read
  them, rather than base64 down the pseudoterminal.
- A warning on stderr when running as root, because the engine is then started
  with `--no-sandbox`.
- A profile kept between runs, so logins survive a restart: in
  `$XDG_DATA_HOME/blinkterm/profile` by default, `--profile <dir>` for another,
  `--temp-profile` for one thrown away on exit. One `blinkterm` per profile at
  a time; a second is refused and told which pid holds it. On quit the engine
  is asked to close, which is what writes the cookies.
- A page's `alert`, `confirm`, `prompt` and "leave this page?" appear on the
  status row and wait for an answer: any key, `y`/`n`, or type and Enter;
  `Esc` says no. A tab behind with one open is marked `2!` in the strip.
- A page that did not come says why — "can't reach example.cmo: name not
  resolved" — rather than showing `chrome-error://`, and a 404 or 500 is shown
  beside the title.
- Downloads: a file a page offers is saved under its own name in
  `$XDG_DOWNLOAD_DIR`, the `XDG_DOWNLOAD_DIR` of `~/.config/user-dirs.dirs`,
  or `~/Downloads` — `--download-dir <dir>` for elsewhere — with `report
  (1).pdf` rather than an overwrite when the name is taken, progress and
  where it went on the status row, and the page left where it was
  ([#10](https://github.com/m96-chan/blinkterm/issues/10)).

### Changed

- The engine is driven over `--remote-debugging-pipe` instead of a debugging
  port, so no other process can attach to it
  ([#5](https://github.com/m96-chan/blinkterm/issues/5)).
- The engine is looked for as `chrome-headless-shell` first and
  `chromium-shell` last. Debian's `chromium-shell` is `content_shell`: it keeps
  a DevTools port open, answers dialogs itself and does not close when asked.

- The minimum Rust is **1.87**. It was documented as 1.75, which had never been
  true: the dependency closure has not built below 1.87. It is checked by CI
  now, against both `Cargo.toml` and the README.

### Fixed

- `tools/run.sh` no longer trips shellcheck's SC1007 on `CDPATH= cd`.

### Security

- A page's title, its url, its dialogs' words and the engine's error text are
  stripped of control characters, bidi overrides and invisible characters
  before they reach the status row, so a `document.title` that is an escape
  sequence is shown as its letters and cannot set the terminal's title or
  clipboard ([#28](https://github.com/m96-chan/blinkterm/issues/28)).

[Unreleased]: https://github.com/m96-chan/blinkterm/commits/main
