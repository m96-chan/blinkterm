# Security

`blinkterm` points a real browser engine at whatever page you ask for and puts
the result in a terminal. Untrusted input is the normal case, not the edge one.

## Reporting something

Use GitHub's private vulnerability reporting: the **Security** tab of this
repository → **Report a vulnerability**. That opens a private thread with the
maintainer; please use it rather than a public issue for anything that lets a
page reach outside the page.

If private reporting is not available to you, open an issue saying only that
you have something to report and how to reach you, and it will be moved
somewhere private.

There is no release cadence to promise a fix against yet, and one person
maintains this. Expect a first reply rather than a patch.

## What is whose problem

**The engine's.** Everything about parsing and executing the page: HTML, CSS,
JavaScript, images, fonts, TLS, same-origin policy, and the renderer sandbox
that is supposed to contain a bug in any of them. `blinkterm` does not ship an
engine and does not patch one — keep your Chromium updated, and report engine
bugs to Chromium.

With one exception that is ours to say out loud: **as root, the sandbox is
off.** Chromium refuses to start as root without `--no-sandbox`, so `blinkterm`
adds it, which removes the main thing standing between a malicious page and the
machine. It prints a warning when it does. Do not browse as root.

**`blinkterm`'s.** The wire between the engine and the terminal:

- **The DevTools transport.** The engine is started with
  `--remote-debugging-pipe` and driven over two inherited pipes, its
  descriptors 3 and 4, which nobody else holds. It opens no port, and the
  engine tests check that no process in its group is listening on one
  ([#5](https://github.com/m96-chan/blinkterm/issues/5)). That holds for a
  real headless shell — `chrome-headless-shell`, or Chromium itself. It does
  not hold for Debian's `chromium-shell`, which is Chromium's `content_shell`
  and opens its DevTools port whatever it is told; with that engine any local
  process running as you can still attach, and docs/install.md says so.

- **The `--remote` socket.** A running `blinkterm` listens on
  `<profile>/blinkterm.sock` (0600, inside the 0700 profile; on a Mac with a
  long profile path, or a profile on a filesystem without sockets, a symlink
  there to a socket in a fresh 0700 directory of its own) for one url per line
  from `blinkterm --remote`. It takes urls and nothing else: no keys, no
  scripts, no commands, and a line that is not an `http`, `https`, `file` or
  `about` url is answered "no" and dropped, after the same reading the url bar
  gives what is typed. What a sender can do is what `ctrl+t` can: open a page.
  The socket is only ever made under the profile lock, so nothing left by a
  crash is trusted, and a temporary profile never has one
  ([#60](https://github.com/m96-chan/blinkterm/issues/60)).

- **What gets written to your terminal.** A terminal executes the bytes it is
  sent, so anything page-derived that reaches the status row is a place where a
  page could try to speak to your terminal instead of to you. The row is the
  only text this program writes, and everything on it that a page or the
  engine can put words into — `document.title`, the url, a dialog's message
  and a `prompt()`'s default, the engine's error text, and the last lines of
  its stderr quoted in an error after the terminal has been given back — goes
  through `text::sanitize` where it is read off the pipe, and once more as the
  row is built. C0 and C1 control characters and DEL are dropped (a line break
  becomes a space); so are Unicode's bidi controls, which could make one url
  read as another, and the invisible format characters that make two
  different strings look the same. A title that is `\x1b]0;x\x07` is shown as
  `]0;x`; the engine tests set one against a real Chromium and parse the row
  with the compositor's own terminal
  ([#28](https://github.com/m96-chan/blinkterm/issues/28)). What is
  deliberately not filtered is visible text: a title in Cyrillic that looks
  like Latin is the page's to write and yours to read, as in any browser's tab
  strip, and a character the row measures wrongly — a keycap sequence, a
  Hangul jamo — is a row one cell short, not an escape. The page *body* is safe in this respect
  by construction: it arrives as decoded pixels and is written as a graphics
  payload, never as text.

  A paste is page-adjacent in the same sense — a page's "copy" button may be
  what put it on your clipboard — so pasted text shown on the row, in the url
  bar or a `prompt()`'s line, is kept to one line of plain text as it is
  pasted and goes through the same `text::sanitize` as the title as the row is
  built. Nothing on the row is ever the text of a copy: `alt+c` says how many
  characters it copied, not which. The copy itself is written as OSC 52 with
  a base64 payload, an alphabet a terminal cannot be spoken to in, and
  `blinkterm` never sends the OSC 52 query that would ask your terminal to
  hand your clipboard back ([#9](https://github.com/m96-chan/blinkterm/issues/9)).

  The hover url is the page's string and goes through the same sanitizer as
  the title (#28). The tab list (`ctrl+shift+a`) is the one other text this
  program writes, on the rows under the status row while it is open: its rows
  are titles and urls and go through the same sanitizer as the row. The pointer shape sent to the terminal is one of a fixed
  table of names this program owns — the page's `cursor` value chooses among
  them and is never itself written
  ([#15](https://github.com/m96-chan/blinkterm/issues/15)).

- **Files a page hands over.** A download's name is the page's
  (`Content-Disposition`, the `download` attribute, the url). The engine
  sanitizes it once and `blinkterm` again: one path component, control and
  bidi characters replaced, no leading dot, at most 255 bytes, and the file
  it renames is `<dir>/<guid>` with the guid checked, never a path the page
  spelled. Nothing outside the download directory is ever written, and
  nothing in it is removed except this run's own `<guid>.crdownload`
  partials. What is *not* done: no prompt before saving, so a page can put a
  file into that directory without a click, as it can in any browser; and
  nothing is opened or run — the row says a file arrived and that is all
  ([#10](https://github.com/m96-chan/blinkterm/issues/10)).

- **Files a page asks for.** A page gets a file from this machine only after
  a path was typed on the row and confirmed with `enter`. The path is made
  absolute and checked here — it exists, it is a regular file, it can be
  opened for reading — because the engine checks nothing: measured against
  `chrome-headless-shell` 153, a relative path is a renderer killed, a
  missing one a 0-byte file handed to the page under that name, and a
  directory a 4096-byte "file" or, on a `webkitdirectory` input, every file
  under it. Directories are refused, always. A page cannot open the prompt
  without a click (the engine refuses one from a script with no user
  activation), but it can turn any click into one — a `<label>`, a handler
  that calls `input.click()` — so a prompt after a click that did not look
  like an upload is the tell, and `esc` costs nothing. Completion reads your
  filesystem to offer names and sends none of it; the page's only word in
  the event is a node number, so nothing it says is drawn for this. No
  record of what was uploaded is kept, in the profile or anywhere
  ([#11](https://github.com/m96-chan/blinkterm/issues/11)). With
  `file-picker` or `file-picker-terminal` set, the click starts that
  program instead — the one you named, run without a shell, with nothing
  from the page in its arguments (`{dir}` is a directory of yours, `{out}` a
  new file only you can read) — and what it answers goes through the same
  checks before anything is sent
  ([#58](https://github.com/m96-chan/blinkterm/issues/58)).

- **Logins from a password manager.** Nothing here stores a password:
  `password-command` names a program of yours (`pass`, `rbw`, `op` …), run
  without a shell with `{host}`, `{domain}` or `{url}` of the page in front
  in its arguments — the host name and the path, never the query string,
  since a process's arguments are readable by every local process — and
  only when you press `alt+l`; no page event, load or dialog can start it.
  It runs only when the page in front is `https`, or `http` on
  `localhost`/`127.*`/`[::1]`, decided from the url this program has before
  anything is started. What it prints is read from a pipe nobody else
  holds, kept in one buffer that is overwritten with zeros when it is
  dropped, handed to the page as an argument of one
  `Runtime.callFunctionOn` — never spliced into script source — and
  forgotten; the buffers the output passed through are overwritten the same
  way. It never reaches the status row (which says `filled login for
  example.com`, not what with), the tab list, the history, the session, the
  zoom or permissions files, or this program's stderr. In the page, the
  script runs in an isolated world and fills only a document whose
  `location` passes the same scheme rule and whose host name is the one the
  secret was fetched for: the top document and its same-origin frames; a
  cross-origin frame is unreachable from that world by the engine's own
  rule, and a `srcdoc`/`about:blank` frame has no host name and is skipped.
  It sets the fields and fires `input`/`change`, and never submits. What is
  *not* done: the copies this program cannot scrub are the DevTools message
  on the private pipe and the JSON it is serialized from, both built and
  freed inside one call; the engine's stderr does not carry DevTools
  traffic unless you hand it `--engine-arg=--enable-logging` with a verbose
  level — do not, on a profile you fill logins into; and a page that has
  already been compromised can read its own form, as in any browser. The
  user name printed by the command is treated exactly as the password
  ([#65](https://github.com/m96-chan/blinkterm/issues/65)).

- **The page in the desktop browser.** `alt+o` hands the url of the page in
  front — query string included — to a program of yours as one argument:
  `open` on a Mac, `$BROWSER` or `xdg-open` on Linux, or what
  `external-browser` names. Only on the key, run without a shell, with
  nothing else from the page: no cookie, no login, nothing the page said.
  The url is the one this program has for the tab; the page is not asked.
  A `$BROWSER` whose program is `blinkterm` is not run, and is taken out of
  the environment of what is run, so the url cannot come back here. Nothing
  is opened for a page that is not `http`, `https` or `file` (`about:`,
  `chrome:`, `data:`, `javascript:`, `blob:` …). The program is started in
  a process group of its own with its standard input, output and error on
  `/dev/null`, and is never signalled
  ([#61](https://github.com/m96-chan/blinkterm/issues/61)).

- **Site styles and scripts.** The files in `~/.config/blinkterm/sites/`
  (or `--sites-dir`) are yours: read from your configuration directory as
  they are and never written by this program. A `.js` file is code put on
  every page its name matches. By default it runs in an isolated world that
  shares the page's DOM and not its JavaScript; a file whose first line is
  `// @world main` runs as the page's own code, with everything the page can
  do — its cookies, its storage, its network access — and a page's CSP does
  not stop it (measured with `script-src 'none'`). So a file another local
  user could change is a way into every page you open, and a file, or the
  directory itself, that group or others can write (`mode & 0o022`) is
  refused by name, in the shell at start and on the row after `alt+shift+r`.
  Nothing in the files leaves the machine unless a script you wrote sends it
  ([#66](https://github.com/m96-chan/blinkterm/issues/66)).

- **Shared memory.** Frames go through POSIX shared memory objects named
  `blinkterm-<pid>-...` and unlinked by the terminal as it reads them — a
  frame is a picture of whatever you are looking at. On Linux they are files
  in `/dev/shm`; on macOS they are `shm_open(3)` objects with no path. Either
  way they are created readable and writable by you alone (0600), whatever
  your umask, so in the window between write and unlink another local user
  cannot read one. `Painter` falls back to inline base64 when no object can be
  made.

- **The history.** The url bar's history is a record of the pages you
  visited — url, title, how often, when — kept in the profile as `history`,
  made readable by you alone (0600) like the cookies beside it, and never
  written for a `--temp-profile`. It is read by nothing but the url bar, and
  deleting the file forgets it. The zoom levels are kept beside it as
  `zoom`, which is a list of the hosts you zoomed — as private as the
  history, so 0600 too, and never written for a `--temp-profile`
  ([#16](https://github.com/m96-chan/blinkterm/issues/16)). The session
  file, `session`, is the list of pages open, kept 0600 like the history and
  never written for a `--temp-profile`; the bookmarks file is yours rather
  than a profile's and lives beside the profiles, in
  `$XDG_DATA_HOME/blinkterm/bookmarks`, 0600 — written under
  `--temp-profile` too, but only when you press `ctrl+d`. Titles in both go
  through the same plain-text filter as the row on the way in and on the way
  out ([#18](https://github.com/m96-chan/blinkterm/issues/18)).

- **What the terminal answers.** `blinkterm` asks the terminal one question
  whose answer is not a key: its background colour (`OSC 11 ; ?`), for
  whether pages are told dark is preferred. The answer arrives on the same
  descriptor as your typing, so it is parsed as a colour and nothing else —
  `11;rgb:…` or `#rrggbb`, at most 256 bytes — and any other operating
  system command that arrives there is read to its end and dropped, never
  typed into the page or shown on the row. The colour itself goes nowhere:
  a page is told light or dark, as any browser tells it, and nothing more
  ([#16](https://github.com/m96-chan/blinkterm/issues/16)).

- **The engine's lifetime.** `blinkterm` starts Chromium in a process group of
  its own and kills the group on exit, on a signal, and from a panic hook.
  A Chromium left running after `blinkterm` has gone — holding your profile,
  and with `chromium-shell` an open debugging port — would be a security
  problem, so failures of that machinery count here.

## Out of scope

- Bugs in Chromium itself — report upstream.
- Anything that needs the attacker to already run code as your user. Such an
  attacker can read the profile directory and ptrace the engine; the pipe
  keeps them from driving it through a port, not from everything.
- Running as root after being told not to.
- The File System Access API (`showOpenFilePicker()` and the rest): refused,
  not supported. And a file input inside a cross-site iframe of a full
  Chromium with site isolation, which lives in a target this program does not
  attach to and gets no prompt — the headless shell keeps such a frame
  in-process, measured, and there the prompt works.
- A password manager command of your own that logs or echoes what it
  printed.
- The proof-of-concept scripts in `tools/`. `tools/Dockerfile` binds the
  debugging port to `0.0.0.0` and says so in a comment: it is a development
  image and an open CDP port is remote code execution by design. Do not run it
  anywhere reachable.
