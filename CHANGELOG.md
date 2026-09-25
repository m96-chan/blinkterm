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

### Changed

- The minimum Rust is **1.87**. It was documented as 1.75, which had never been
  true: the dependency closure has not built below 1.87. It is checked by CI
  now, against both `Cargo.toml` and the README.

### Fixed

- `tools/run.sh` no longer trips shellcheck's SC1007 on `CDPATH= cd`.

[Unreleased]: https://github.com/m96-chan/blinkterm/commits/main
