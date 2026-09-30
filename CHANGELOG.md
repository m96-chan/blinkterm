# Changelog

Notable changes to `blinkterm`. The format is
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the versioning policy
and what counts as a breaking change are in [RELEASING.md](RELEASING.md).

Entries are for the person running the program. A refactor that nobody can
observe from outside does not need a line here; a changed key binding, a new
flag, a different default, a raised Rust floor all do.

## [Unreleased]

### Added

- **Clicking the status row.** A click on a tab in the strip switches to
  it, a middle click closes it (never the last one), a click on `+N` at
  either end brings the nearest tab past that end into view, and a click on
  the url — or anywhere on the row with one tab — opens the url bar as
  `ctrl+l` does; in cells and in Kitty's pixel reports, in tmux too. The
  release of a press that landed on the row no longer reaches the page
  ([#90](https://github.com/m96-chan/blinkterm/issues/90)).
- **`--install-engine`.** Fetches `chrome-headless-shell` 153.0.8010.52 —
  the build the tests pass against — from Chrome for Testing for this
  machine (`linux64`, `mac-arm64`, `mac-x64`), checks it against a SHA-256
  compiled into the program, unpacks it under
  `$XDG_DATA_HOME/blinkterm/engine/`, starts it once and says where it is
  and how to remove it. It is found before `PATH` from then on; `--engine`,
  `$BLINKTERM_ENGINE` and `engine =` still win. Nothing is downloaded unless
  asked, and a second run of the same version does nothing
  ([#91](https://github.com/m96-chan/blinkterm/issues/91)).

## [0.3.0] - 2026-10-01

### Added

- **Reader mode.** `alt+r` (`cmd+shift+r` on a Mac; the action `reader`)
  shows the article on the page alone — its title, byline, text, pictures
  and links in one readable column, in the page's colour scheme — with
  find, hints, zoom, the saves, `--alpha` and `--force-dark` still working
  in it. `alt+r` again puts the page back where it was; nothing is
  reloaded. The row says `reader` while it is on, and `no article on this
  page` where there is nothing to read
  ([#64](https://github.com/m96-chan/blinkterm/issues/64)).
- **The page's console.** `ctrl+shift+j` or `alt+j` (`cmd+alt+j` on a Mac;
  the action `console`) shows what the page in front logged, threw and
  failed to fetch, newest last, each row the level, the text and where it
  came from, with a `-- navigated to` row between pages. Words typed in any
  order filter it; `esc` or `enter` closes it. The row says `2 errors`
  until you look. The last 1000 entries per tab, in memory only;
  `--no-console` or `console = false` turns it off
  ([#67](https://github.com/m96-chan/blinkterm/issues/67)).
- **Site styles and scripts.** A file `<host>.css` or `<host>.js` in
  `~/.config/blinkterm/sites/` (`$XDG_CONFIG_HOME/blinkterm/sites/`) is put
  on every page of that host — `*.<host>` for a site and every host under
  it, `all` for every page — the general ones first, so the most specific
  file has the last word. Styles are adopted at document start and are
  nothing the page can see in its own sheets; scripts run at document start
  in an isolated world that shares only the DOM, or in the page's own when
  the first line is `// @world main`. `alt+shift+r` (`cmd+alt+shift+r` on a
  Mac; `reload-sites`) reads the directory again: styles change where the
  page stands, scripts from the next load. `--sites-dir <dir>`
  (`sites-dir =`) reads another directory, `--no-sites` (`sites = false`)
  none. A file or directory another user could write is refused by name
  ([#66](https://github.com/m96-chan/blinkterm/issues/66)).
- **Open in the desktop browser.** `alt+o` (`cmd+shift+o` on a Mac; the
  action `open-external`) opens the page in front with `open` on a Mac,
  `$BROWSER` or `xdg-open` on Linux, or `--external-browser <command>`
  (`external-browser =`) with `{url}` where the url goes. It is started
  detached and outlives `blinkterm`. Only `http`, `https` and `file` pages
  are opened (`nothing to open here` otherwise), and over ssh with no
  display the row says so and points at `alt+u`. Cookies and logins do not
  travel ([#61](https://github.com/m96-chan/blinkterm/issues/61)).
- **Transparent pages.** `--alpha [<1-100>]` (`alpha = true|false|<1-100>`)
  lets the terminal's background show through a page: the engine paints
  nothing behind it and the page's own `html` and `body` backgrounds are
  forced transparent, so a light page is its text and pictures over the
  terminal; a page that paints a container of its own keeps it. With a
  number the whole picture, text included, is sent at that opacity. It
  stays see-through while the page moves. Locally the page is painted on
  magenta and the magenta keyed back out, so what a page shows in magenta
  goes too; over ssh and in tmux the frames are PNG with real transparency
  and the number is not applied. A page's default black text sits on the
  terminal's colour, so `--force-dark` goes with it on a dark terminal
  ([#79](https://github.com/m96-chan/blinkterm/issues/79),
  [#84](https://github.com/m96-chan/blinkterm/issues/84)).
- **Keys that work on a Mac.** `keymap = mac|linux` (or `--keymap`) picks
  the built-in keys. The `mac` keymap arrives in a stock Kitty on macOS
  with no change to `kitty.conf`: `cmd` where Kitty leaves it free and
  Chrome or Safari use it, `ctrl` elsewhere — `cmd+[`/`cmd+]` back and
  forward, `cmd+alt+left`/`right` or `ctrl+pageup`/`pagedown` the tabs,
  `ctrl+1` … `ctrl+9` a tab by number, `cmd+d` bookmark, `cmd+shift+t`
  reopen, `cmd+shift+a` the tab list, `cmd+y` history, `cmd+c` copy,
  `cmd+s` save a PDF, and `ctrl+l`, `ctrl+t`, `ctrl+w`, `ctrl+r`, `ctrl+f`,
  `ctrl+=`, `ctrl+q` as on Linux. The Linux keys still answer underneath,
  so the `alt` chords work with `macos_option_as_alt left`, and `key.`
  lines apply on top of either keymap. `--help` lists the keymap of the
  platform, or the one a `--keymap` on the same line names; the full table
  is in [docs/usage.md](docs/usage.md#on-a-mac)
  ([#80](https://github.com/m96-chan/blinkterm/issues/80)).
- `--doctor` prints a `keys:` line saying which keymap is in effect, and
  names every `key.` line on a chord Kitty or macOS keeps by default; in
  Kitty such a line is also named on the status row at start, with the
  `map <chord> no_op` line for `kitty.conf` that frees it
  ([#80](https://github.com/m96-chan/blinkterm/issues/80),
  [#78](https://github.com/m96-chan/blinkterm/issues/78)).

### Changed

- **On macOS the default keys are now the `mac` keymap** (see Added). This
  is the change that can surprise somebody's fingers: on a Mac, `cmd+[`,
  `cmd+]`, `cmd+d`, `cmd+c`, `cmd+s`, `cmd+p`, `cmd+b`, `cmd+u`, `cmd+y`,
  `ctrl+1` … `ctrl+9`, `ctrl+pageup`/`pagedown` and the other chords of the
  Mac column are now the program's where they used to reach the page, and
  `--help` lists the Mac keys. Every Linux key still does what it did.
  `keymap = linux` in the settings file, or `--keymap linux`, puts things
  back as they were. Linux and other platforms are unchanged.
- New keys are taken from the page on every platform: `alt+o`, `alt+r`,
  `alt+shift+r`, `alt+j` and `ctrl+shift+j` (see Added). A `key.<chord> =
  none` line gives any of them back.
- The README is short now: a screenshot, the philosophy, how to install
  and build, and the main keys for both keymaps. The full manual moved to
  `docs/`: installing, keys and use, settings and profiles, features, how
  it works, and development.

### Fixed

- A file input inside a **cross-site iframe** — an embedded form service,
  a support chat, a webmail attachment button — now asks for a path on the
  row like any other. Its click never reached `blinkterm`: the engine
  opened its own picker, which headless does not have and cancels at once,
  so nothing happened and the page heard `cancel`. Each tab now attaches
  to its frames, and the file goes back to the frame that asked
  ([#57](https://github.com/m96-chan/blinkterm/issues/57)).
- A page opened straight into a new tab — `alt+enter` in the history list,
  a url from `blinkterm --remote`, a pick opened in a new tab — is recorded
  in the history again, and its tab stops saying `loading <url>` once the
  page is there. A page quick enough finished loading before the program
  was listening, and nothing counted the visit. A page counts once per
  document, so answering a dialog on a loaded page no longer counts it
  twice ([#70](https://github.com/m96-chan/blinkterm/issues/70)).
- A death `blinkterm` cannot catch — `SIGSEGV`, `SIGBUS`, `SIGILL`,
  `SIGFPE`, `SIGABRT` — now puts the terminal back and stops the engine on
  the way down, instead of leaving a shell with mouse reporting on and the
  keyboard flags pushed, typing every mouse report and every key back as
  text ([#78](https://github.com/m96-chan/blinkterm/issues/78)).

## [0.2.0] - 2026-09-30

### Added

- A history list: `ctrl+shift+h` (or `alt+h`) shows every page visited
  over the screen, newest first, each row when, the title and the url, with
  a `*` for one that is bookmarked. Words typed in any order filter it by
  title and url; `enter` opens the pick here, `alt+enter`, `ctrl+enter` or a
  middle click in a new tab, and `shift+delete` forgets it, from the file
  too. `history` in a `key.` line
  ([#68](https://github.com/m96-chan/blinkterm/issues/68)).
- `blinkterm --remote <url>…` opens the urls as new tabs in the `blinkterm`
  already running on the same profile and exits, over a 0600 socket next to
  the lock; with none running it starts as usual, so `export
  BROWSER='blinkterm --remote'` makes it the browser `gh browse`, `git
  web--browse` and `man -H` open. Only `http`, `https`, `file` and `about`
  urls are taken. `packaging/blinkterm.desktop` lets `xdg-open` reach it
  ([#60](https://github.com/m96-chan/blinkterm/issues/60)).
- `alt+s` saves the page as a PDF, and `alt+shift+s` the whole page, top
  to bottom, as a PNG, into the download directory under the page's title,
  `(1)` when the name is taken; the row says `saved ~/Downloads/<title>.pdf`.
  The paper is Letter or A4 by the locale, or `pdf-paper = a4|letter` /
  `--pdf-paper`. A picture is at most sixteen million pixels, and a taller
  page is saved to that depth and said to be. `save-pdf` and
  `save-screenshot` for `key.` lines
  ([#63](https://github.com/m96-chan/blinkterm/issues/63)).
- Ads and trackers blocked by host: `block-list = <path>` (or
  `--block-list`, repeatable) reads a hosts file or one host per line, and a
  request to a listed host or any host under it fails before it leaves the
  engine; the row says `12 blocked`. `alt+b` (the action `block`) turns it
  off for the site in front and on again, kept in the profile's `unblocked`
  file; `block = false` or `--no-block` turns every list off. Answered on
  the pipe's reader thread through `Fetch`, which cost a 300-image page
  2.12 s against 2.10 s unblocked
  ([#62](https://github.com/m96-chan/blinkterm/issues/62)).
- `alt+l` (`fill-login`) fills a login form from your password manager:
  `password-command = <command>` (`pass show web/{domain}`, `rbw get --full
  {host}`) prints the password on its first line and `login: <user>` on
  another, and the password field and the user-name field before it are
  filled and never submitted, in the page or a same-origin frame. Only on
  the key, only for an https page or one on this machine, and the password
  is never shown or kept. `password-command-terminal` for one that needs
  the terminal, with the file picker's display rule; both are options too
  ([#65](https://github.com/m96-chan/blinkterm/issues/65)).
- A page's file input can be answered by a program the settings name
  instead of the row: `file-picker = <command>` for one with a window of
  its own (Finder's dialog through `osascript`, `zenity`, `kdialog`),
  which runs while the page keeps drawing, and `file-picker-terminal =
  <command>` for one that needs the terminal (`yazi`, `fzf`, `kitten
  choose-files`), which `blinkterm` steps aside for. `{dir}` and `{out}` in
  the command; one path per line back, checked as a typed one is; a
  non-zero exit or no answer is a cancel. `file-picker-multiple` and
  `file-picker-terminal-multiple` for an input that takes several files,
  and every one of them as an option too
  ([#58](https://github.com/m96-chan/blinkterm/issues/58)).
- Ready for crates.io: the one dependency is `libc`. The PNG and JPEG
  decoders, inflate, the tty helpers and the cell arithmetic that came from
  tOS as git revisions are copied into `src/`, and the tests' copy of tOS's
  terminal is in `vendor/`, unpublished
  ([#33](https://github.com/m96-chan/blinkterm/issues/33)).
- macOS support: frames through `shm_open` shared memory, Chrome and Chromium
  found in `/Applications`, `--use-mock-keychain` for the engine, and the unit
  and real-engine suites run on Apple silicon in CI
  ([#46](https://github.com/m96-chan/blinkterm/issues/46)).
- The terminal is asked before the engine starts, and one that does not
  answer the graphics query gets a sentence instead of a blank pane
  (`--no-probe` skips it). Inside tmux (`set -g allow-passthrough on`) the
  picture goes through passthrough as Unicode placeholders; over ssh the
  frames are the engine's PNG, paced by the link. `--tmux`, `--frames`,
  `--fps`, and `--doctor` says which route it took
  ([#21](https://github.com/m96-chan/blinkterm/issues/21)).
- `key.<chord> = <action>` in the settings file rebinds any of the
  program's keys (`key.f5 = reload`), and `key.<chord> = none` gives one
  back to the page; the actions are listed by `--help`. A chord needs
  `ctrl`, `alt` or `super` unless it is an f-key
  ([#17](https://github.com/m96-chan/blinkterm/issues/17)).
- A settings file, `$XDG_CONFIG_HOME/blinkterm/config`: every option as
  `name = value`, `engine-arg` repeatable. The command line wins over the
  file; `$BLINKTERM_ENGINE` sits between. `--config <path>`, `--no-config`
  ([#17](https://github.com/m96-chan/blinkterm/issues/17)).
- `--engine <path>`, `--engine-arg <flag>` (repeatable; four that would
  reopen the DevTools port or move the profile are refused),
  `--user-agent <text>`, `--proxy <host:port>`, `--home <url>`
  ([#17](https://github.com/m96-chan/blinkterm/issues/17)).
- Several urls on the command line open one tab each, the first in front
  ([#17](https://github.com/m96-chan/blinkterm/issues/17)).
- `--doctor` starts the engine and asks the terminal whether it speaks the
  Kitty graphics and keyboard protocols; `--print-engine` says which engine
  would run ([#17](https://github.com/m96-chan/blinkterm/issues/17)).
- Tabs: `alt+9` is the last tab; the strip scrolls with `+N` markers when
  the titles do not fit; `ctrl+shift+a`/`alt+a` lists every tab with a
  filter; `ctrl+shift+pageup`/`pagedown` (`alt+shift+…`) move the current
  tab; a middle click or `ctrl`+click on a link opens it in a tab behind
  ([#19](https://github.com/m96-chan/blinkterm/issues/19)).
- Normal mode, `ctrl+.`: link hints (`f`/`F`), scrolling from the
  keyboard (`j`/`k`/`d`/`u`/`gg`/`G`), `H`/`L`/`r`/`o`/`O`/`/`, `i` back to
  the page. `F` opens a link in a tab behind. Off by default; nothing
  changes until it is turned on, or `--normal-mode` starts in it
  ([#13](https://github.com/m96-chan/blinkterm/issues/13)).
- Bookmarks: `ctrl+d` bookmarks the page (again removes it) to
  `$XDG_DATA_HOME/blinkterm/bookmarks`, one `url<TAB>title` per line,
  hand-editable, shared by every profile; the url bar offers them before the
  history ([#18](https://github.com/m96-chan/blinkterm/issues/18)).
- The open tabs are saved in the profile (`session`) as they change;
  `--restore` reopens them, loading each when it is first looked at, and
  after a crash the next start offers to. `ctrl+shift+t` — or `alt+t`, which
  reaches a tOS pane and a legacy terminal — reopens the last closed tab
  ([#18](https://github.com/m96-chan/blinkterm/issues/18)).

### Changed

- Pages are no longer told a program is driving: `navigator.webdriver` is
  `false`. The user agent and the client hints are the engine's own without
  `HeadlessChrome`, with `blinkterm/<version>` on the end, and the languages
  come from the locale unless `--engine-arg --accept-lang=…` says otherwise.
  A `--user-agent` of your own is still sent as written
  ([#48](https://github.com/m96-chan/blinkterm/issues/48)).
- `brew install m96-chan/tap/blinkterm` installs the tagged release; `--HEAD`
  is only needed for main.
- `blinkterm a b` opens two tabs rather than refusing with "one page at a
  time".
- A page whose renderer crashes keeps its tab, with `this page crashed;
  ctrl+r reloads it` on the row, rather than the tab closing
  ([#18](https://github.com/m96-chan/blinkterm/issues/18)).
- When the engine dies it is started again in place and the tabs come back,
  the one in front loading; the program exits with the saved-tabs sentence
  only if it dies twice in a minute or cannot be started again
  ([#18](https://github.com/m96-chan/blinkterm/issues/18)).

### Fixed

- Scrolling on macOS now keeps its 16 ms schedule instead of inheriting the
  operating system's roughly 8 ms timed-wait coalescing, which made a steady
  wheel visibly lurch. Going to the end of a page no longer leaves elastic
  overscroll that swallows the next notch in the opposite direction
  ([#46](https://github.com/m96-chan/blinkterm/issues/46)).
- A terminal's `ctrl`+click opens a background tab on macOS too; it is sent as
  the `cmd`+click Chromium expects there
  ([#46](https://github.com/m96-chan/blinkterm/issues/46)).

- A middle click or `ctrl`+click on a link used to open a page the
  program never attached to — a renderer running for nobody until the
  engine exited — because the engine announces such a page with no opener
  and the tab list required one.

## [0.1.0] - 2026-09-25

The first tag.

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
- The clipboard: the terminal's paste key pastes into the page, the url bar or
  a `prompt()` as text rather than keystrokes (bracketed paste; a newline no
  longer submits a form, and a paste over 64 KiB is refused whole), `alt+c`
  copies the page's selection and `alt+u` the url to the host's clipboard over
  OSC 52 ([#9](https://github.com/m96-chan/blinkterm/issues/9)).
- The url bar is an editor: a cursor that moves by character — a letter and
  its accent, a flag, an emoji are one — readline's keys (`ctrl+a`/`ctrl+e`,
  `alt+b`/`alt+f`, `ctrl+w`, `alt+d`, `ctrl+u`/`ctrl+k`, Delete), and a row
  that scrolls sideways when the url is wider than the pane. A page's
  `prompt()` gets the same editor
  ([#14](https://github.com/m96-chan/blinkterm/issues/14)).
- Pages visited are remembered in the profile (`history`, 0600, the last
  2000); `↑`/`↓` in the url bar walk them, and a match is offered dim after
  what is typed, taken with `tab` or `→`. A temporary profile keeps none on
  disk.
- `--search-url <url with %s>`: words typed in the url bar go to that search.
  Off by default; without it nothing typed is sent anywhere it does not name.
- A page's `<input type=file>` asks for a path on the status row, with tab
  completion; several files for a `multiple` input, one per `enter` and an
  empty `enter` to send them; `esc` sends nothing
  ([#11](https://github.com/m96-chan/blinkterm/issues/11)).
- Find in page: `ctrl+f` opens a `find:` prompt on the status row, matches
  are highlighted as you type with the current one scrolled into view and
  counted (`3/17`), `enter`/`ctrl+g` next and `shift+enter` previous, `esc`
  clears. Case-insensitive, CJK included, same-origin frames included,
  hidden text excluded; nothing in the page is modified
  ([#12](https://github.com/m96-chan/blinkterm/issues/12)).
- The status row: a link under the pointer shows where it goes (`link:
  https://…`), a plain-http page on a named host is marked `not secure`, a
  loading page says `esc stops` and counts the seconds, and `esc` stops it.
  The terminal is asked for all mouse motion (mode 1003) and, where it
  understands OSC 22, told the pointer's shape
  ([#15](https://github.com/m96-chan/blinkterm/issues/15)).
- Zoom: `alt+=`/`alt+-` (and `ctrl+=`/`ctrl+-` where the terminal passes
  them) through Chrome's steps from 25% to 300%, `alt+0`/`ctrl+0` back to
  100%; remembered per host in the profile's `zoom` file (0600), shown on
  the row as `150%` ([#16](https://github.com/m96-chan/blinkterm/issues/16)).
- `--scale <n|auto>` for HiDPI terminals: auto says 2 when a cell is 28 px
  or taller ([#16](https://github.com/m96-chan/blinkterm/issues/16)).
- `--color-scheme auto|light|dark`: a dark terminal gets dark pages, by
  asking the terminal its background (`OSC 11`); `--force-dark` paints even
  pages with no dark style dark
  ([#16](https://github.com/m96-chan/blinkterm/issues/16)).
- Over ssh, or in a terminal whose `winsize` has no pixels, the cell size is
  asked with `CSI 16 t`, so the picture is the pane's size there too
  ([#16](https://github.com/m96-chan/blinkterm/issues/16)).
- A Homebrew tap: `brew install m96-chan/tap/blinkterm` on Linux builds the
  same source `cargo install` does, with Homebrew's Rust; `--HEAD` until the
  first tag. The formula is `packaging/homebrew/blinkterm.rb` here, built by
  CI, and the tap carries a copy
  ([#32](https://github.com/m96-chan/blinkterm/issues/32)).

### Changed

- The engine is driven over `--remote-debugging-pipe` instead of a debugging
  port, so no other process can attach to it
  ([#5](https://github.com/m96-chan/blinkterm/issues/5)).
- The engine is looked for as `chrome-headless-shell` first and
  `chromium-shell` last. Debian's `chromium-shell` is `content_shell`: it keeps
  a DevTools port open, answers dialogs itself and does not close when asked.
- `localhost`, `*.localhost`, `127.*` and `[::1]` typed without a scheme get
  `http://`, not `https://`.
- `ctrl+u` in the url bar deletes to the start of the line rather than the
  whole line — the same thing until the cursor could move.
- Pages now see the pointer move, not only click and drag.

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

[Unreleased]: https://github.com/m96-chan/blinkterm/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/m96-chan/blinkterm/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/m96-chan/blinkterm/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/m96-chan/blinkterm/commits/v0.1.0
