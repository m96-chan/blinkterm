# blinkterm

A real browser in a terminal pane. Not a text browser: the page is rendered by
a headless Chromium — Blink, the engine the page was made for — and arrives in
the pane as pixels, over the Kitty graphics protocol. Images, video, CSS,
JavaScript, the lot, in a terminal.

```sh
blinkterm https://example.com
blinkterm localhost:3000       # this machine gets http://, everything else https://
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

Or, on Linux, with [Homebrew](https://brew.sh):

```sh
brew install m96-chan/tap/blinkterm
```

That builds from source too — the tap's formula asks Homebrew for a Rust and
runs the same `cargo install --locked` — so it is the same binary by a shorter
command, not a prebuilt one; prebuilt binaries are
[#22](https://github.com/m96-chan/blinkterm/issues/22). The engine below is
still yours to install, and `brew` says so when it is done. The formula lives
in this repository, at `packaging/homebrew/blinkterm.rb`, and
[m96-chan/homebrew-tap](https://github.com/m96-chan/homebrew-tap) carries a
copy.

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
`--engine <path>` first, then `$BLINKTERM_ENGINE`, then `engine = <path>` in
the [settings](#settings), then on `PATH` for `chrome-headless-shell`,
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

### History

The pages you visit are remembered in the profile, in a file called
`history`: each page's url, its title, how many times you have been there and
when last — a page that loaded, not one that failed, and not `about:` or
`data:`. It is readable by you alone (0600), like the cookies beside it, and
keeps the last 2000 pages. `--temp-profile` keeps it in memory for the run and
writes none. Deleting the file is how to forget it; nothing else reads it.

A full `chromium` rather than the headless shell still writes
`~/.config/chromium/Crash Reports` whatever profile it is given; that
directory is Chromium's, not `blinkterm`'s.

### Zoom levels

A page you zoom is remembered by its host, in a file called `zoom` beside
the history: one line per change, the host and the level, and nothing for a
site at 100%. It is a list of sites you have visited, so it is readable by
you alone (0600) too, and keeps the last 500. `--temp-profile` keeps the
levels in memory for the run and writes none; deleting the file forgets
every level.

### Permissions

What you allowed a site with `alt+p` is remembered by its origin —
`https://meet.example`, or `http://wiki.corp:8080` with its port — in a file
called `permissions` beside the zoom levels: one line per change, the
origin, a tab, the word, a tab, `allow` or `deny`, the last line for an
origin and a word being the one that counts. It is a list of sites you have
visited, so it is readable by you alone (0600), and keeps the last 500
origins. Every engine started on the profile is told it as it starts;
`--temp-profile` keeps it in memory for the run and writes none. Edit it by
hand if you like — a line that is not one of these is skipped — or delete
it to take every allowance back.

### Bookmarks

`ctrl+d` bookmarks the page in front, and `ctrl+d` again removes the
bookmark; the row says which. They are kept in one file for every profile,
`$XDG_DATA_HOME/blinkterm/bookmarks` (or `~/.local/share/blinkterm/bookmarks`),
beside the default profile rather than in any of them — the same file under
`--profile` and under `--temp-profile`, because a profile is the engine's data
and a bookmark is yours: pressing `ctrl+d` in a throwaway profile is asking
for that one page to outlive it. The file is readable by you alone (0600), one
bookmark a line:

```text
https://example.com/<TAB>Example Domain
# a comment
https://a.example/notitle
```

A url and its title, separated by a tab; a line that is only a url is a
bookmark with no title, and anything else — a `#` comment, a blank, a
mistake — is left exactly where it is when `blinkterm` changes the file. Edit
it with anything; `grep` it to see what is kept. A url is matched exactly, so
`https://example.com/` and `https://example.com` are two bookmarks. The url
bar offers bookmarks before the pages you visited: the dim suggestion is a
bookmark's when one starts with what you typed, and `↑` walks the bookmarks
that match before the history. Two `blinkterm`s can share the file: every
change takes a lock on `bookmarks.lock` and reads the file again first, so
neither loses the other's bookmark; what one adds shows up in the other after
its next `ctrl+d` or its next start.

### The session

The tabs you have open — their urls, their titles and which one is in front —
are kept in the profile, in a file called `session`, readable by you alone
(0600) and written within half a second of any change. Nothing else of a tab
is: not how far down a page you were, not what you had typed into a form.
`blinkterm --restore` (or `restore = true` in the configuration file) reopens
them at start, in order, with the one that was in front in front; a url on
the command line as well opens in one more tab, in front of them. A restored
tab loads its page the first time you look at it, not all at once: the strip
shows the saved titles straight away, and twenty tabs cost half a second of
start rather than twenty pages fetched. At most 100 tabs are restored.

After a run that did not end with a quit — a crash, a `kill -9`, the engine
dying — the next start asks on the row: `restore 3 tabs from last time?
y/n`. `y` or `enter` restores them; `n`, `esc`, or simply getting on with
something else declines, and the question waits under anything else that
wants the row. How a run that did not quit is known is the file's first line,
`# blinkterm session: open` until the run quits and writes `closed`.
`--temp-profile` keeps the session in memory, for `ctrl+shift+t`, and writes
nothing.
## Settings

Everything on the command line can also be kept in
`$XDG_CONFIG_HOME/blinkterm/config` — `~/.config/blinkterm/config` when
`XDG_CONFIG_HOME` is not set — one setting per line, named as the option is
without its `--`:

    # what a page is told about you
    color-scheme = dark
    scale = 2
    user-agent = Mozilla/5.0 (X11; Linux x86_64) blinkterm
    # the engine
    engine = /opt/chrome-headless-shell-linux64/chrome-headless-shell
    engine-arg = --accept-lang=ja
    engine-arg = --disable-features=Translate
    proxy = socks5://127.0.0.1:1080

The command line wins over the file, and `$BLINKTERM_ENGINE` sits between
the two for `engine`. `--config <path>` reads another file, `--no-config`
none. A line the program does not understand stops it with the file and
line number; a missing file is nothing. A flag is `true` or `false`
(`force-dark = true`), and a path may start with `~/`. There is no `url`
setting: the page to open is what the command line is for, and
`home = <url>` is the page opened when none is given. `normal-mode = true`
starts in normal mode (`ctrl+.`), `restore = true`
reopens the last session's tabs (see [The session](#the-session)), and
`mute = true` (or `--mute`) starts the engine silent (see
[Sound](#sound-permissions-and-fullscreen)).

`--engine-arg` (and `engine-arg =`) hands Chromium one more argument,
repeatable. Four are refused because they would undo something this
program set on purpose: `--remote-debugging-port` and
`--remote-allow-origins` would open the DevTools port the pipe replaced
([#5](https://github.com/m96-chan/blinkterm/issues/5)), `--user-data-dir`
is `--profile`, and `--remote-debugging-pipe` is already there. Everything
else goes through as written, and where it repeats a flag the program set,
Chromium takes the last. `--user-agent` and `--proxy` are the two everybody
wants and have names of their own; loopback never goes through the proxy.
(`--lang=ja` does nothing in the headless shell; `--accept-lang=ja` is the
one that changes what pages are told.)

Several urls open several tabs, the first in front:

    blinkterm https://example.com https://example.org

`--doctor` is the first thing to run in a new terminal: it starts the engine
on a throwaway profile, asks the terminal whether it speaks the Kitty
graphics and keyboard protocols, and prints one line per answer, exiting 1
when the engine did not answer or the terminal answered neither. In tmux
without `allow-passthrough` it will tell you the terminal did not answer,
which is the truth. `--print-engine` prints the path the search finds and
nothing else, for scripts.

### Rebinding keys

`key.<chord> = <action>` in the settings file puts an action on a key, and
`key.<chord> = none` takes a key back for the page:

    key.f5 = reload
    key.ctrl+b = back
    key.ctrl+w = none

| action | default | does |
| --- | --- | --- |
| `quit` | `ctrl+q` | quit |
| `url` | `ctrl+l` | type a url |
| `reload` | `ctrl+r` | reload |
| `back` | `alt+left` | back |
| `forward` | `alt+right` | forward |
| `new-tab` | `ctrl+t` | a new tab |
| `close-tab` | `ctrl+w` | close this tab |
| `reopen-tab` | `ctrl+shift+t`, `alt+t` | reopen the tab closed last |
| `bookmark` | `ctrl+d` | bookmark this page, or remove the bookmark |
| `next-tab` | `ctrl+tab` | the next tab |
| `previous-tab` | `ctrl+shift+tab` | the tab before |
| `tab-1` … `tab-8` | `alt+1` … `alt+8` | the nth tab |
| `last-tab` | `alt+9` | the last tab |
| `list-tabs` | `ctrl+shift+a`, `alt+a` | the tab list |
| `move-tab-left` | `ctrl+shift+pageup`, `alt+shift+pageup` | move this tab left |
| `move-tab-right` | `ctrl+shift+pagedown`, `alt+shift+pagedown` | move this tab right |
| `zoom-in` | `alt+=`, `ctrl+=` | zoom in |
| `zoom-out` | `alt+-`, `ctrl+-` | zoom out |
| `zoom-reset` | `alt+0`, `ctrl+0` | back to 100% |
| `find` | `ctrl+f` | find in the page |
| `permissions` | `alt+p` | allow this site the camera, microphone, location, notifications or clipboard |
| `copy` | `alt+c` | copy the selection, or the line being typed |
| `copy-url` | `alt+u` | copy the url |
| `normal-mode` | `ctrl+.` | normal mode on or off |

A chord is `ctrl`, `alt`, `shift` or `super` joined with `+` to a key — a
character, or `plus`, `space`, `tab`, `enter`, `esc`, `backspace`,
`insert`, `delete`, the arrows, `home`, `end`, `pageup`, `pagedown`,
`f1`…`f24` — and needs `ctrl`, `alt` or `super` unless it is an f-key: a
bare letter is the page's, always. A chord is exact: `ctrl+tab` is not
`ctrl+shift+tab`, and `key.ctrl+= = none` leaves `ctrl+shift+=` zooming in.
A bound key is taken from every page — `key.ctrl+b = back` is no longer an
editor's bold. The editing keys of the url bar, the find prompt, the tab
list and a dialog are not remappable, and `copy`, `copy-url` and `quit` are
the only actions that reach through them, on whatever keys they are. A
later line for the same chord replaces an earlier one; binding a chord the
program already used moves nothing else, so `key.ctrl+t = quit` leaves
`new-tab` on no key. Normal mode's letters are not affected by `key.` lines:
`key.ctrl+r = none` leaves `r` as reload. `blinkterm --help` lists the
actions too.

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

## Uploading a file

A click on a page's file input — an "attach", a "choose file" — takes the
status row for a path:

    upload: ~/work/report/rep▌ort.pdf

It starts in the directory the last file was uploaded from, or the one
`blinkterm` was started in. `tab` completes a name (a second `tab` lists what
it could be), `~` is your home, and the line is edited with the url bar's
keys; `enter` sends the file. A page that takes several files asks once per
file — `upload (2 added, enter to send):` — and an `enter` with nothing typed
sends them. `esc` sends nothing, and the page is told the picker was
dismissed, as a browser would tell it.

What `enter` sends is checked here, because the engine checks nothing: the
path is made absolute, and it must be a file that exists and can be read.
Directories are refused. The page then gets what any browser gives it — the
file's name, size and contents — and nothing is sent before `enter`: what
`tab` reads of your disk to complete a name stays on this side.

Two things a browser has that this does not: a page that uses the newer
file-picker API (`showOpenFilePicker()`) is told no, and the row says "this
page's file picker isn't supported"; and there is no drag and drop, which is
not a thing a terminal has.

## Keys

| | |
| --- | --- |
| `ctrl+l` | type a url. In the url bar, `←`/`→`, `home`/`end`, `ctrl+a`/`ctrl+e` and `alt+b`/`alt+f` (or `ctrl+←`/`ctrl+→`) move; `ctrl+w`/`alt+backspace` and `alt+d` delete a word; `ctrl+u`/`ctrl+k` delete to either end; `↑`/`↓` walk the pages visited; a dim suggestion after what you typed is taken with `tab` or `→` |
| `ctrl+f` | find in the page. Type and the matches are highlighted as you go, the current one in orange and scrolled into view; the row says `3/17`. `enter`/`ctrl+g`/`↓` next, `shift+enter`/`ctrl+shift+g`/`↑` previous; the same editing keys as the url bar; `esc` closes and clears. The next `ctrl+f` offers the last needle again |
| `ctrl+r` | reload; a page whose renderer crashed comes back with it |
| `alt+left` / `alt+right` | back and forward |
| `ctrl+t` | a new tab, with the cursor in the url bar |
| `ctrl+w` | close this tab; closing the last one quits |
| `ctrl+shift+t` / `alt+t` | reopen the tab closed last, and the one before it on the next press (up to twenty, this run). `alt+t` is there because `ctrl+shift+t` is `ctrl+t` in a terminal without the Kitty keyboard protocol, and never reaches a tOS pane, whose compositor takes it for a workspace |
| `ctrl+d` | bookmark this page; again to remove the bookmark |
| `ctrl+tab` / `ctrl+shift+tab` | the next tab, the one before |
| `alt+1` … `alt+8`, `alt+9` | the nth tab, the last tab |
| `ctrl+shift+a` (or `alt+a`) | the tab list: type to filter by title or url, `↑`/`↓` to pick, `enter` to switch, `esc` to close |
| `ctrl+shift+pageup` / `pagedown` (or `alt+shift+pageup` / `pagedown`) | move this tab left or right |
| middle click or `ctrl`+click on a link | open it in a tab behind this one |
| `alt+=` / `alt+-` | zoom in and out (`ctrl+=` / `ctrl+-` where your terminal lets them through) |
| `alt+0` / `ctrl+0` | back to 100% |
| your terminal's paste key | pastes into the page, the url bar, the find prompt, a `prompt()` or a file input's path — whichever has the cursor |
| `alt+c` | copy the page's selection to your clipboard; with the url bar, the find prompt, a `prompt()` or a file input's path open, copy that line |
| `alt+u` | copy the page's url to your clipboard |
| `alt+p` | allow this site something: the row says `allow https://site: ` and the words it is allowed now, all selected; type any of `camera` `microphone` `location` `notifications` `clipboard`, `enter` sets exactly those (an empty line takes them all back), `esc` leaves it. See [Sound, permissions and fullscreen](#sound-permissions-and-fullscreen) |
| `ctrl+q` | quit |
| a page's dialog | its `alert`, `confirm`, `prompt` or "leave this page?" takes the top row: any key for an alert, `y`/`n` for a question, or type and `enter` for a prompt; `esc` says no |
| a page's file input | click it: the row asks for a path — `tab` completes names, `~` is home, one path per `enter` when the page takes several and an empty `enter` sends them; `esc` sends nothing |
| `esc` | while a page is fullscreen and nothing else has the row, leave fullscreen; while a page is loading, stop it |
| `ctrl+.` | normal mode on or off. In normal mode the letters are keys of their own and the row says `normal`: `f` labels everything clickable on the screen and typing a label clicks it (`F` opens a link in a tab behind this one); `j`/`k` scroll a notch, `d`/`u` half a screen, `gg`/`G` to the top and bottom; `H`/`L` back and forward, `r` reload, `o` the url bar, `O` a new tab, `/` find; `i` goes back to typing into the page, as does clicking into a field. Off by default: nothing changes until you press it |

Everything else goes to the page, including the mouse, unless a `key.` line
in the [settings](#rebinding-keys) takes it. A link that asks for a
new window gets a new tab, and the tab is switched to.

With more tabs than the row can name, the strip shows a run of them around
the one in front and `+N` at either end for how many are past it; it scrolls
when the tab in front reaches an edge. The list (`ctrl+shift+a`) shows all of
them. Kitty and Ghostty keep `ctrl+shift+a` for themselves and every terminal
keeps `ctrl+shift+pageup`/`pagedown` — Kitty and tOS for the scrollback,
WezTerm and Ghostty for their own tabs — so each has an `alt` form that
reaches the program everywhere. Ghostty also keeps `alt+1`..`alt+9` for its
tabs; its `alt+9` is its last tab, as it is here. A middle or `ctrl` click
opens a link behind the current tab, as a desktop browser does; a link that
asks for a window (`target=_blank`) still comes to the front.

`ctrl+c` and `ctrl+v` are the page's own: they copy and paste within the
engine, not with your clipboard. Your terminal's paste key (`ctrl+shift+v`, a
middle click) is how text gets in, and `alt+c` is how it gets out. A paste
reaches the page as text, never as keys, so the newlines in it do not submit
a form; one over 64 KiB is refused whole rather than cut. Copying goes out as
OSC 52, which your terminal may need to be told to allow.

The url bar is asked about every key first while it is open, so `alt+←`/`→`
are back and forward only when it is closed — in the bar they move by a word —
and `ctrl+w`, `ctrl+t` and the rest do nothing there; `esc` closes it. The
find prompt and the tab list are the same.

Normal mode is for browsing without a mouse. The labels are drawn by the
page itself, in one element this program adds to the document while they
show and takes away after; a page can see that element and could remove
it, and the next key puts it back. Labelled: links, buttons, fields,
anything with `onclick`, a `role`, or a pointer cursor, in the page and in
its same-origin frames and open shadow roots; not hidden things, not what
something else covers, not image-map areas, not cross-origin frames. In
normal mode an unbound letter does nothing, so nothing is typed into a
field by mistake; arrows, Tab, Enter, Space and every `ctrl`/`alt` key
still reach the page. `esc` takes the labels off; `ctrl+.` turns the mode
off.

Find is case-insensitive, matches text as the page shows it — spaces
collapsed, a word split across `<b>` still one word, never across a
paragraph — and looks in same-origin frames but not cross-origin ones, nor in
hidden text, a closed `<details>`, or the value of an `<input>`. Every match
is counted; the first ten thousand are highlighted. Nothing is changed in the
page: the highlights are the CSS Custom Highlight API from a world of their
own, and the page's selection is left alone. A page that navigates closes the
prompt.

### Searching

Nothing you type in the url bar is sent anywhere but where it names. With
`--search-url`, words are sent to the search you choose:

```sh
blinkterm --search-url 'https://duckduckgo.com/?q=%s'
```

What counts as words: anything with a space in it, or a single word with no
dot that is not `localhost` and has no port (`rust`, `what?`), or a number
that is not an address (`3.14`). `example.com`, `localhost:3000`, `myhost:8080`,
`192.168.1.1` and anything with a scheme or starting with `/` are still places.
Without the flag, `rust` goes to `https://rust`, and that is the point: a
mistyped intranet name or a half-pasted token is not handed to a third party
unless you said so, once, on the command line.

While a page is waiting on its dialog, only the tab keys and `ctrl+q` still
work, and the page gets no keys or mouse until it has its answer. A tab behind
that opens one is marked `!` in the strip — `2! Title` — and keeps its question
until you go to it. `ctrl+w` closes a tab without asking the page, so a tab
with something unsaved in it is closed without a "leave this page?".

A file input's path keeps the same keys — the tab keys and `ctrl+q` work,
`ctrl+l`, reload, back and forward wait for `enter` or `esc` — but it does not
stop the page, and the mouse still reaches it. A tab behind with a path
waiting is marked `!` as one with a dialog is. A dialog the page opens while a
path is half typed takes the row first; the path is there again once it is
answered.

### Zoom, scale and dark pages

`alt+=` and `alt+-` zoom the page in and out through Chrome's own steps
(25% to 300%), `alt+0` puts it back; `ctrl+=`, `ctrl+-` and `ctrl+0` do the
same where your terminal lets them through — Kitty and tOS do, WezTerm and
Ghostty keep them for their own font size. The level is remembered per
site, in the profile's `zoom` file (0600, a list of hosts; `--temp-profile`
keeps none), and shows on the row as `150%` while it is not 100%. Zooming
reflows the page as a browser's zoom does — `devicePixelRatio` and
`innerWidth` change, and the page lays itself out for the narrower width —
rather than magnifying a picture of it. While the page is moving the frames
are at the page's own resolution and the terminal scales them; the lossless
still that follows is at the pane's.

On a HiDPI terminal a page at one CSS pixel per terminal pixel is tiny.
`--scale 2` makes it two, and the default, `auto`, says 2 when a cell is 28
px or taller — a 2x display's cells are, a 1x display's are not — and
follows the terminal's font size as it changes.

A page is told whether you prefer dark: `--color-scheme dark|light|auto`.
`auto` (the default) asks the terminal its background colour (`OSC 11`) and
calls it dark below mid-grey; a terminal that does not answer gets light. A
page with no dark style stays white; `--force-dark` has the engine paint
every page dark regardless, which is Chromium's auto dark mode and is off
unless you ask.

## The status row

The top row is the page's title and url, and five other things when they
apply. A link under the pointer shows where it goes — `link: https://…` —
which is the one defence against a link whose text says one place and whose
href says another; the url shown is the one the engine resolved, with
control characters and invisible characters removed. A plain `http://` page
on a host that is not this machine is marked `not secure` before its title;
`https://`, `file:` and `localhost` get nothing, as in a desktop browser. A
page that is loading says `esc stops` at the right, and after a second how
many seconds it has been going — no percentage, because without the
engine's network domain there is nothing honest to compute one from, and
that domain costs more than the row is worth (`src/load.rs` has the
numbers). `esc` stops the load and leaves the page where it was: the
previous page if nothing had arrived, the half-loaded page if something had.
A terminal that understands OSC 22 (Kitty, Ghostty) also gets a hand over a
link and an I-beam over a text field; the rest ignore it. A zoom that is not
100% is a word at the right too, `150%`, after the loading hint.

The url bar, the find prompt, the tab list, the allow line (`alt+p`), a
page's dialog and a file input's path take the whole row while they are
open, and `esc` goes to whichever of them has it before it leaves
fullscreen or stops a load; no link is shown while one of them is there.
The offer to restore the last run's tabs takes it too, after all of them.

A page whose renderer crashes stays in its tab: the picture goes, and the row
says `this page crashed; ctrl+r reloads it` — a tab behind that crashed says
the same in the strip. `ctrl+r` brings it back, painting, at the size it was;
`ctrl+w` closes it; keys and the mouse do nothing to it until then. When the
engine itself dies it is started again in place, on the same profile: the
row says `the engine died; starting it again…`, and a moment later the same
tabs are back in the same order with the same one in front, loading again,
the others loading when they are next looked at. What was being typed in the
url bar stays; a download that was coming says it did not arrive, and scroll
positions, form contents and a login made in the last half minute are lost,
as with `--restore`. If it dies again within a minute, or cannot be started,
`blinkterm` exits, and the message says how many tabs were saved and that
`blinkterm --restore` reopens them; the next plain start offers to.

To know what is under the pointer the terminal is asked to report every
mouse movement, not only presses (`?1003h`), so the page now sees the
pointer move — hover styling and tooltips work — at the cost of one small
command to the engine per screen refresh while it moves and nothing while
it rests.

## Sound, permissions and fullscreen

**Sound** comes out of the machine `blinkterm` runs on, through the
engine's own audio: PulseAudio or PipeWire where `libpulse.so.0` is
installed and a server answers, else ALSA — the headless shell has the whole
of Chrome's audio stack and tries them in that order (measured with
`strace`), and plays into nothing when there is neither. Over SSH that is
the far machine's speakers, or nothing; a terminal cannot carry sound.
Chrome's autoplay rule applies: a page cannot start sound before you have
clicked it, and a click here is a click to the page, so a video you click
plays with sound and a page that shouts on load does not.
`--engine-arg=--autoplay-policy=no-user-gesture-required` lifts that.
`--mute` (`mute = true`) starts the engine with `--mute-audio`: pages play,
silently, and cannot tell. Chrome for Testing's zip does not bring
`libpulse0`; a distribution's `chromium` does.

**Permissions**: every page is told no. From the moment the engine starts,
notifications, location, camera, microphone and the clipboard API are
`denied` for every site, so a site that checks hears no at once and stops
asking, rather than waiting on a question the headless engine would never
show. No engine says when a page asks, so there is no prompt; instead
`alt+p` allows the site in front yourself:

```text
allow https://meet.example: camera microphone
allowed https://meet.example: camera, microphone
```

The words are `camera`, `microphone`, `location`, `notifications` and
`clipboard`; `enter` sets exactly the ones on the line for that origin and
denies the rest, and the allowance is remembered in the profile (see
[Permissions](#permissions)). A page with no origin — `about:blank`, a
`data:` or `file:` url — has nothing to allow. What a grant buys is the
engine's: on `chrome-headless-shell` it makes the clipboard API work, into
the engine's own clipboard rather than yours, and lets a site believe it may
use a camera, microphone or location that the shell then cannot find — a
full Chromium with a webcam uses them. No headless engine shows a
notification, whatever is allowed; the word is there so that a site stops
asking.

**Fullscreen**: when the page in front takes something fullscreen — a
video's button, a slide deck — the status row goes and the page gets every
row of the pane. `esc` leaves it, as do a navigation, a reload and going to
another tab (the page's own Escape does nothing in a headless engine).
While something needs the row — the url bar, the find prompt, the tab list,
the allow line, a dialog the page opens, a file input's path — the row comes
back and the page is a row shorter until it closes: a `confirm()` in a
fullscreen video is still answered on the row. Link hints and normal mode
work in fullscreen as anywhere.

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
