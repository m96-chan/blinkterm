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

On Linux and macOS, in any terminal that speaks all three of the Kitty
graphics protocol, the Kitty keyboard protocol and SGR mouse reporting —
Kitty, WezTerm, Ghostty — and in a [tOS](https://github.com/m96-chan/tOS)
pane, which is where it was written.
tOS owns the display: there is no X11 and no Wayland and there never will be,
so no browser can be ported to it in the ordinary sense. But a terminal that
speaks those three protocols is already a screen, a mouse and a keyboard, and
that is the whole of what an engine wants. None of that argument is about tOS,
so the same binary runs in the others. This repository exists because tOS's CI
has no Chromium to test against and this program is nothing without one.

On a Mac there is no `/dev/shm`, so the frames go through `shm_open(3)`
shared memory objects instead, which Kitty and Ghostty read; a terminal that
does not gets the inline fallback, as anywhere else.

The terminal is asked before the engine is started. One that does not
answer the graphics query gets a sentence in the shell saying why and what
to do, not a blank pane; `--no-probe` skips the question for a terminal
that draws and does not answer.

**Inside tmux** it works with `set -g allow-passthrough on` in `tmux.conf`
and a terminal behind tmux that speaks the protocol. tmux eats graphics
commands otherwise, so the picture goes wrapped in tmux's passthrough and is
drawn as Kitty's Unicode placeholders — text tmux can see, move and redraw —
with the engine's PNG as the frames (at most 297 columns of picture, and
30 frames a second). Two things are lost inside tmux: the Kitty keyboard
protocol (tmux re-encodes keys, so there are no key releases and `ctrl+i` is
`tab`) and mouse positions finer than a cell. `--tmux on|off` overrides
the detection.

**Over ssh** (`$SSH_CONNECTION` set) the frames are the engine's PNG as it
sent them, inline: no `/dev/shm` on the far side, and raw pixels would be
3.9 MB a frame. Each frame is acknowledged to the engine only when it has
gone to the terminal, so the frame rate is what the link carries, every
frame current; frames that wait on the link make the page cast at half, then
three-eighths, of the pane until the link catches up, and the still of a
page at rest is always full size. `--fps <n>` caps it (15 by default over
ssh), and `--frames raw|png` overrides the choice.

## Installing

Rust 1.87 or newer; see [Checks](#checks):

```sh
cargo install --locked blinkterm
```

`--locked` builds with the lock file the release was tested with. For what is
on `main` rather than the last release:

```sh
cargo install --locked --git https://github.com/m96-chan/blinkterm
```

Or, on Linux, with [Homebrew](https://brew.sh):

```sh
brew install m96-chan/tap/blinkterm
```

That builds from source too — the tap's formula asks Homebrew for a Rust and
runs the same `cargo install --locked` — so it is the same binary by a shorter
command, not a prebuilt one; prebuilt binaries are
[#22](https://github.com/m96-chan/blinkterm/issues/22). The engine below is
still yours to install, and `brew` says so when it is done. On macOS, use
`cargo install` for now: the formula's release, v0.1.0, predates macOS
support, and the formula opens to macOS with the first release that has it.
The formula lives
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
the [settings](#settings), then on `PATH` for `chrome-headless-shell`,
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

## Profiles

Cookies, logins, local storage and the rest of what a site keeps are kept
between runs, in `$XDG_DATA_HOME/blinkterm/profile` — or
`~/.local/share/blinkterm/profile` when `XDG_DATA_HOME` is not set. The
directory is made readable by you alone (0700), since a cookie is a login.
The same on macOS, rather than `~/Library/Application Support`: this is a
program run from a shell, the terminals it runs in keep their own settings
under `~/.config` there too, and a cookie jar is better kept out of what Time
Machine and iCloud copy about.

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
not is worse than an error. To open a url in the one that has it, say so:
`blinkterm --remote <url>` (see
[Opening a url from another program](#opening-a-url-from-another-program)). The lock is `blinkterm`'s own — an `flock` on
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
writes none. Nothing else reads it.

`ctrl+shift+h` (or `alt+h`) lists it over the screen, newest first: each row
says when — `just now`, `3 hours ago`, `2 weeks ago` — then the title and the
url, with a `*` after the date for a page you have bookmarked too. Type
words, in any order, and the list keeps the pages with every one of them in
the title or the url, whatever the case: `rust release` finds the page
titled "Rust 1.87 released" at `news.example`. `enter` opens the pick here,
`alt+enter` or `ctrl+enter` (or a middle click on the row) in a new tab.
`shift+delete` forgets the pick: it goes from the list, from the url bar's
suggestions and from the file, which is written again without it, so it
stays forgotten after a restart. Deleting the file forgets everything.

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

### Blocked hosts

The sites you turned blocking off for with `alt+b` are remembered by host
in a file called `unblocked` beside the permissions: one line per change,
the host, a tab, `off` or `on`, the last line for a host being the one that
counts. It is a list of sites you have visited, so it is readable by you
alone (0600), and keeps the last 500. `--temp-profile` keeps it in memory
for the run and writes none; deleting it turns blocking back on everywhere.
The lists themselves are yours and are only read (see
[Blocking ads and trackers](#blocking-ads-and-trackers)).

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
## Opening a url from another program

Terminal programs open a link through `$BROWSER`: `gh browse`, `git
web--browse`, `man -H`, a mail client. Point it at the `blinkterm` you have
open:

```sh
export BROWSER='blinkterm --remote'
gh browse                      # a tab in the blinkterm that is running
man -H ls                      # the page man writes, as file:///tmp/...
git web--browse https://example.com
```

`blinkterm --remote <url>…` hands the urls to the `blinkterm` already running
on the same profile, which opens the first as a new tab in front and the rest
behind it, and exits at once with 0. With none running it starts as usual,
with those urls — so the same `$BROWSER` works whether one is open or not, as
long as there is a terminal to start one in; with no terminal (a desktop's
`xdg-open`) it says there is nobody to hand the url to, and exits 1.

The profile decides which `blinkterm` is reached, exactly as it decides which
profile a start takes: `blinkterm --remote --profile ~/work-profile <url>`
reaches the one on `~/work-profile`. A `--temp-profile` run is its own and
never listens, and `--remote --temp-profile` is refused as a contradiction.

Each url is read as the url bar reads what is typed — `example.com` is
`https://example.com`, `localhost:3000` is `http://`, a path is `file://` —
and it must come out as an `http`, `https`, `file` or `about` url. Anything
else (`javascript:`, `data:`, `chrome://`, `mailto:`) is refused: the sender
prints one line per refused url on stderr and exits 1, and the urls that were
fine are opened anyway.

How it works: a running `blinkterm` listens on a Unix socket,
`blinkterm.sock`, next to `blinkterm.lock` in its profile, made 0600 inside the
0700 profile. It is sent one url per line and answers one line per url, `ok
<url>` or `no <why>`; it takes nothing else — no keys, no scripts, no
commands. The socket is made only once the profile lock is held, so one left
by a `blinkterm` that was killed is simply replaced, and a sender that finds
one with nobody on it starts as usual. Where the profile's path is too long
for a socket — a Mac allows 104 bytes — or the profile is on a filesystem
that cannot hold one, the socket goes in a fresh private directory under the
temporary directory, and `blinkterm.sock` in the profile is a symlink to it.
A `blinkterm` busy with a file picker in its terminal takes the url when it
is back; the sender waits ten seconds for its answer and then says so and
exits 0.

For `xdg-open` on Linux, `packaging/blinkterm.desktop` registers
`blinkterm --remote %u` as a web browser:

```sh
cp packaging/blinkterm.desktop ~/.local/share/applications/
xdg-settings set default-web-browser blinkterm.desktop
```

It reaches a `blinkterm` running on the default profile, and has no terminal
to start one in, so with none running a link says so in the journal and
opens nothing. On macOS `gh` and `git` honour `$BROWSER`, but `open` and the
rest of the system do not: they go to the default browser, which a program
with no window cannot be.

## Settings

Everything on the command line can also be kept in
`$XDG_CONFIG_HOME/blinkterm/config` — `~/.config/blinkterm/config` when
`XDG_CONFIG_HOME` is not set, on macOS as on Linux — one setting per line,
named as the option is without its `--`:

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
[Sound](#sound-permissions-and-fullscreen)). `tmux = on|off|auto`,
`frames = raw|png|auto`, `fps = <n>` and `probe = false` are `--tmux`,
`--frames`, `--fps` and `--no-probe` (see [Where it runs](#where-it-runs)).
`file-picker`, `file-picker-terminal`, `file-picker-multiple` and
`file-picker-terminal-multiple` name a program that chooses a file for a
page's file input instead of the row (see
[Choosing it with another program](#choosing-it-with-another-program)).
`pdf-paper = a4|letter` (or `--pdf-paper`) is the paper `alt+s` prints on
(see [Saving a page](#saving-a-page)).
`block-list = <path>`, repeatable, names a list of hosts to block, and
`block = false` (or `--no-block`) blocks nothing whatever the lists say
(see [Blocking ads and trackers](#blocking-ads-and-trackers)).
`password-command` and `password-command-terminal` name a password
manager's command that `alt+l` fills a login form from (see
[Filling a login from your password manager](#filling-a-login-from-your-password-manager)).

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
one that changes what pages are told, over the locale — see
[What a page is told](#what-a-page-is-told).)

Several urls open several tabs, the first in front:

    blinkterm https://example.com https://example.org

`--doctor` is the first thing to run in a new terminal: it starts the engine
on a throwaway profile, asks the terminal whether it speaks the Kitty
graphics and keyboard protocols — through tmux's passthrough as well, inside
tmux — and prints one line per answer and the route frames will take,
exiting 1 when the engine did not answer or the terminal cannot draw. In
tmux without `allow-passthrough` it says the graphics query went unanswered
raw and through tmux. `--print-engine` prints the path the search finds and
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
| `history` | `ctrl+shift+h`, `alt+h` | the history list |
| `move-tab-left` | `ctrl+shift+pageup`, `alt+shift+pageup` | move this tab left |
| `move-tab-right` | `ctrl+shift+pagedown`, `alt+shift+pagedown` | move this tab right |
| `zoom-in` | `alt+=`, `ctrl+=` | zoom in |
| `zoom-out` | `alt+-`, `ctrl+-` | zoom out |
| `zoom-reset` | `alt+0`, `ctrl+0` | back to 100% |
| `find` | `ctrl+f` | find in the page |
| `permissions` | `alt+p` | allow this site the camera, microphone, location, notifications or clipboard |
| `block` | `alt+b` | stop blocking ads and trackers on this site, or start again |
| `fill-login` | `alt+l` | fill the login form from your password manager |
| `copy` | `alt+c` | copy the selection, or the line being typed |
| `copy-url` | `alt+u` | copy the url |
| `save-pdf` | `alt+s` | save this page as a PDF |
| `save-screenshot` | `alt+shift+s` | save the whole page as a picture |
| `normal-mode` | `ctrl+.` | normal mode on or off |

A chord is `ctrl`, `alt`, `shift` or `super` joined with `+` to a key — a
character, or `plus`, `space`, `tab`, `enter`, `esc`, `backspace`,
`insert`, `delete`, the arrows, `home`, `end`, `pageup`, `pagedown`,
`f1`…`f24` — and needs `ctrl`, `alt` or `super` unless it is an f-key: a
bare letter is the page's, always. A chord is exact: `ctrl+tab` is not
`ctrl+shift+tab`, and `key.ctrl+= = none` leaves `ctrl+shift+=` zooming in.
A bound key is taken from every page — `key.ctrl+b = back` is no longer an
editor's bold. The editing keys of the url bar, the find prompt, the tab
list, the history list and a dialog are not remappable, and `copy`, `copy-url` and `quit` are
the only actions that reach through them, on whatever keys they are. A
later line for the same chord replaces an earlier one; binding a chord the
program already used moves nothing else, so `key.ctrl+t = quit` leaves
`new-tab` on no key. Normal mode's letters are not affected by `key.` lines:
`key.ctrl+r = none` leaves `r` as reload. `blinkterm --help` lists the
actions too.

## What a page is told

A person is at the terminal, so a page is not told a program is driving.
With nothing set, on `chrome-headless-shell` 153 in a `ja_JP.UTF-8` locale:

```
navigator.webdriver   false
navigator.userAgent   Mozilla/5.0 (…) Chrome/153.0.0.0 Safari/537.36 blinkterm/0.2.0
userAgentData.brands  Chromium 153, blinkterm 0.2.0, and a GREASE brand
navigator.languages   ja-JP, ja, en
```

- **`navigator.webdriver` is `false`.** Blink sets it to say the browser is
  under automation, which is the first thing most bot checks read, and here
  it would be untrue. `--disable-blink-features=AutomationControlled` turns it
  off.
- **The user agent is the engine's own, without `HeadlessChrome`,** and with
  `blinkterm/<version>` on the end, the way a browser built on somebody
  else's engine names itself. The client hints say the same.
- **The languages come from the locale** — `LC_ALL`, then `LC_MESSAGES`, then
  `LANG` — with English last, and `en-US, en` when it is unset or `C`.
  `--engine-arg --accept-lang=…` (or `engine-arg = --accept-lang=…`) wins
  over the locale.
- **`--user-agent` is taken whole:** nothing is appended, and no client hints
  are sent beside it. `navigator.userAgentData.brands` then comes back
  empty, since nothing this program could write there would match a string it
  did not write.

Three things still say "headless", and nothing here changes them:
`navigator.plugins` is empty, `window.chrome` is missing, and
`Notification.permission` is `denied`. That is not something to hide. This
is a headless browser, and a site that challenges it on those grounds will
keep doing so. Only the one false claim is corrected. There is no
fingerprint spoofing, and none is planned.

A full Chrome (`engine = …`) reports those three the way a desktop Chrome
does. It also costs five to six times the memory: 13 processes and 1.6 GB
against the headless shell's 4 and 282 MB, measured on one page. That is why
the headless shell stays first in the search. The measurements are in
[#48](https://github.com/m96-chan/blinkterm/issues/48).

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

### Saving a page

`alt+s` saves the page in front as a PDF and `alt+shift+s` the whole of it,
top to bottom, as a PNG, into the same directory:

    saving Receipt #123.pdf        →        saved ~/Downloads/Receipt #123.pdf

The name is the page's title with `/` made `_`, else the site's name, else
`page`, and a name already taken becomes `(1)` as a download's does. The
PDF is printed with the page's backgrounds, on Letter where the locale is a
country that uses it (`en_US`, `en_CA`, `es_MX`, …) and A4 everywhere else;
`pdf-paper = a4|letter` says which. The picture is at the tab's zoom and
the pane's scale, as sharp as the page is on screen, and at most sixteen
million pixels: a page taller than that is saved to that depth and the row
says `saved …png, the top 12500 of 40000 px`. Neither touches the page —
it stays where it was scrolled, at the size it was — and neither is opened
afterwards.

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

### Choosing it with another program

The row is what you get with nothing set. To see the files you are choosing
from — an image for an image search, say — name a program in the settings
and the click opens that instead
([#58](https://github.com/m96-chan/blinkterm/issues/58)):

    # a window of its own: Finder's dialog on a Mac, GTK's or KDE's on Linux
    file-picker = osascript -e 'POSIX path of (choose file)'
    file-picker = zenity --file-selection
    file-picker = kdialog --getopenfilename {dir}
    # something that runs in this terminal
    file-picker-terminal = yazi --chooser-file={out} {dir}
    file-picker-terminal = fzf
    file-picker-terminal = kitten choose-files --write-output-to={out}

(one of each, of course). `file-picker` is a program that opens a window and
leaves the terminal alone: the page keeps drawing and the keys keep going to
it while the window is open. `file-picker-terminal` is one that needs the
terminal: `blinkterm` steps aside — the picture, the mouse, the keyboard
modes, the screen — runs it, and comes back when it exits. They are two
settings because nothing can tell the two kinds apart from outside, and they
need the opposite. With both set, the window is used where there is a
display (`$DISPLAY` or `$WAYLAND_DISPLAY`, or a Mac not reached over ssh)
and the terminal one elsewhere, so one settings file does for the desk and
for ssh; with one set, that one. Both are options too, `--file-picker` and
`--file-picker-terminal`.

The command is split into words the way a shell splits it — quotes and
backslashes as a shell has them — and run without one: no `$VARIABLES`, no
globs, no `~` except at the very start of a command in the file. Write
`sh -c '…'` for a shell. `{dir}` is where the picker should start, by the
row's own rule (the directory of the last upload, else where `blinkterm`
was started), and it is also the picker's working directory. `{out}` is a
new empty file, readable by you alone and removed afterwards, that the
picker writes its answer to; without `{out}` the answer is what it prints.
Neither is split again, so a directory with a space in it is still one
argument.

The answer is one path per line. Blank lines are nothing, a `file://` url
is taken, and a relative path is under `{dir}` (which is what `fzf`
prints). A picker that exits with anything but 0, or answers nothing, was
cancelled — `osascript` and `zenity` exit 1 on Cancel, `fzf` 130 on `esc`,
`yazi` writes nothing on `q` — and the page is told so, as it is by `esc`
on the row. Whatever it chose is checked as a typed path is: absolute,
there, a regular file, readable, no directories; one that fails sends
nothing, the row says why, and the page is told the picker was dismissed.
An input that takes one file gets the first path.

For an input that takes several files, `file-picker-multiple` and
`file-picker-terminal-multiple` are used instead, when they are set,
because no flag means "several" to every picker:

    file-picker-multiple = sh -c 'zenity --file-selection --multiple | tr "|" "\n"'
    file-picker-terminal-multiple = fzf -m

(`zenity` puts `|` between the files it was given, and a newline cannot be
written in a value, so `tr` makes the lines.) Each stands in for its plain
one and the other way round: with only `file-picker` set, a `multiple`
input gets what it chose; with only `file-picker-multiple`, a plain input
gets the first line.

One picker runs at a time; a click on a file input while a window is
open is told no at once. Closing the tab, the page going somewhere else,
or quitting ends the picker, and what it would have chosen goes nowhere.
While a terminal picker runs, `ctrl+c` is the picker's: `blinkterm`
ignores it until the picker exits.

## Filling a login from your password manager

`blinkterm` keeps no passwords. If yours are in a password manager with a
command line, name its command and `alt+l` fills the login form in front
from it ([#65](https://github.com/m96-chan/blinkterm/issues/65)):

    password-command = pass show web/{domain}
    password-command = rbw get --full {host}
    # a terminal one: pick the entry yourself
    password-command-terminal = sh -c 'gopass show "$(gopass ls --flat | fzf)"'
    # 1Password, through a wrapper of your own that prints the two lines
    password-command = ~/bin/op-login {domain}

(one of each, of course). What the command prints is the `pass` convention:
the password on the first line, exactly as it is, and on a later line
`login: <user>` (or `username:` or `user:`) for the user name. That is what
`pass`, `gopass` and `rbw get --full` print already. JSON is not read —
every manager's is different — so for `op`, `bw` or `keepassxc-cli` a few
lines of `jq` in a script make the two lines. A command that exits with
anything but 0, or prints nothing, has no login for the page, and the row
says `no login for example.com`.

In the command, `{host}` is the page's host name (`accounts.example.com`),
`{domain}` the host without its subdomains (`example.com`, and
`example.co.uk` for `www.example.co.uk`) — a guess made without the
public-suffix list, right for the names people keep entries under; use
`{host}` when it is wrong for you — and `{url}` the page's url without its
query string and fragment, since every program on the machine can read
another's arguments and a query can hold a session token. The command is
split and run as a file picker's is, with no shell; it starts in your home
directory.

`password-command` is a program that leaves the terminal alone — one that
answers straight away, or opens a window of its own — and runs while the
page keeps drawing; the row says `asking for the login for example.com`.
`password-command-terminal` is one that needs the terminal: `blinkterm`
steps aside while it runs, as for a terminal file picker. With both set the
choice is the file picker's: the window where there is a display, the
terminal elsewhere (see
[Choosing it with another program](#choosing-it-with-another-program)).
Both are options too, `--password-command` and
`--password-command-terminal`.

The password goes into the password field of the form you are in — or,
with the focus in no form, the first password field on the page — and the
user name into the text or email field before it, preferring one whose name
says user, login, email or account. A login form in a frame of the page's
own site is reached too; one in a frame of another site is not. Each field
is set the way typing would leave it, so the page's scripts see it, and
nothing is submitted: `enter` is yours to press. The row says
`filled login for example.com`, or `filled password for example.com` when
there was no user name to fill, or `no password field on this page`.

It runs only when you press `alt+l`: no page, load or dialog can start it,
and one runs at a time. It runs only for a page on `https`, or on `http` to
this machine (`localhost`, `127.0.0.1`, `[::1]`); anywhere else the row
says `fill-login needs https, or localhost` and nothing is run. The page is
checked again when the answer comes: a tab that went somewhere else in the
meantime is not filled, and the script in the page fills only documents
whose host is the one the login was looked up for. The password is never
on the row, in the history, in the session or in any file, and what held
it is overwritten once it has been handed over. See
[SECURITY.md](SECURITY.md) for the details and the limits.

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
| `ctrl+shift+h` (or `alt+h`) | the history list: every page visited, newest first, `title — url` and when; type words in any order to filter by title or url, `↑`/`↓` to pick, `enter` opens it here, `alt+enter`/`ctrl+enter` (or a middle click) in a new tab, `shift+delete` forgets it, `esc` closes. See [History](#history) |
| `ctrl+shift+pageup` / `pagedown` (or `alt+shift+pageup` / `pagedown`) | move this tab left or right |
| middle click or `ctrl`+click on a link | open it in a tab behind this one |
| `alt+=` / `alt+-` | zoom in and out (`ctrl+=` / `ctrl+-` where your terminal lets them through) |
| `alt+0` / `ctrl+0` | back to 100% |
| your terminal's paste key | pastes into the page, the url bar, the find prompt, a `prompt()` or a file input's path — whichever has the cursor |
| `alt+c` | copy the page's selection to your clipboard; with the url bar, the find prompt, a `prompt()` or a file input's path open, copy that line |
| `alt+u` | copy the page's url to your clipboard |
| `alt+s` | save this page as a PDF in the download directory, named after its title; the row says `saved ~/Downloads/<title>.pdf`. See [Saving a page](#saving-a-page) |
| `alt+shift+s` | save the whole page, top to bottom, as a PNG there. A page past sixteen million pixels is cut to its top, and the row says how much |
| `alt+p` | allow this site something: the row says `allow https://site: ` and the words it is allowed now, all selected; type any of `camera` `microphone` `location` `notifications` `clipboard`, `enter` sets exactly those (an empty line takes them all back), `esc` leaves it. See [Sound, permissions and fullscreen](#sound-permissions-and-fullscreen) |
| `alt+b` | stop blocking ads and trackers on this site, or start again: the row says `blocking off for example.com`, and `unblocked` while you are on it. Reload to get what was blocked. In the url bar `alt+b` is still a word back |
| `alt+l` | fill the login form from your password manager: runs `password-command` for this page's host and puts what it printed into the password field and the user-name field before it, in the page or a same-origin frame; never submits; only on https or localhost. See [Filling a login from your password manager](#filling-a-login-from-your-password-manager) |
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
find prompt, the tab list and the history list are the same.

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
100% is a word at the right too, `150%`, after the loading hint, and after
it how many requests a [block list](#blocking-ads-and-trackers) stopped on
this page, `12 blocked` — counted from the page's last landing, per tab —
or `unblocked` on a site you turned blocking off for.

The url bar, the find prompt, the tab list or the history list, the allow
line (`alt+p`), a page's dialog and a file input's path take the whole row while they are
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

## Blocking ads and trackers

Every frame costs something here — bandwidth over ssh, decoding, the
terminal's parse loop — and an animated ad keeps the screencast running on a
page nobody is scrolling. So `blinkterm` can block requests to the hosts on
the lists most people use already:

    block-list = ~/.config/blinkterm/hosts
    block-list = ~/.config/blinkterm/more-hosts

or `--block-list <path>` on the command line, as many as you like; the
file's come first and the command line's are added. Two forms are read,
which between them are what public lists come in: a hosts file
(`0.0.0.0 ads.example.com`, several names after one address allowed) and one
host per line. `#` starts a comment, on its own line or after the names;
blank lines, addresses, `localhost` and names with no dot are skipped. A
list in another syntax — EasyList's `||host^` rules, element hiding — is
not read, and a file with nothing readable in it is refused at start with a
sentence rather than blocking nothing in silence. Nothing is bundled; for
example, [StevenBlack/hosts](https://github.com/StevenBlack/hosts):

    curl -o ~/.config/blinkterm/hosts \
      https://raw.githubusercontent.com/StevenBlack/hosts/master/hosts

A listed host blocks itself and every host under it: `ads.example.com`
blocks `x.ads.example.com`, and not `notads.example.com` or `example.com`,
as uBlock Origin and AdGuard read a host list. A request to one fails before
it leaves the engine, and the page carries on without it; a page of a listed
site itself is the engine's error page with `net::ERR_BLOCKED_BY_CLIENT` on
the row. `alt+b` turns blocking off for the site in front — the page's host
— and on again, remembered in the profile (see
[Blocked hosts](#blocked-hosts)); on the error page of a blocked site it
unblocks that site, and a reload brings it. `block = false` or `--no-block`
turns every list off for a run without removing them. With no list nothing
is intercepted at all.

How: every page's session is given `Fetch.enable`, every request is paused
before it is sent, and the pause is answered at once on the thread that
reads the engine's pipe — failed for a listed host, let go for the rest.
Measured against chrome-headless-shell 153 over the pipe, with a list of
100 000 hosts and a local page of 300 images:

| | |
| --- | --- |
| the page, nothing blocked | 2.10 s |
| the page, every request paused and answered | 2.12 s |
| deciding one request | 6 µs |
| on the pipe, per request | about 800 bytes |
| `Network.setBlockedURLs` instead: the command | 5.8 MB, 16.3 s to be acknowledged |
| `Network.setBlockedURLs` instead: the page | 7.3 s |

Reading 100 000 hosts takes tens of milliseconds and 6 to 10 MB, once at
start.

What is not blocked: requests made by an iframe the engine runs in a process
of its own — on chrome-headless-shell's defaults a cross-site iframe runs in
the page's process and is covered, but a full Chromium, or
`--site-per-process`, isolates it, and its requests go through until
`blinkterm` attaches to frames — requests a service worker makes, and
anything that is not a host: no element hiding, no cosmetic filters, no
paths.

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
While something needs the row — the url bar, the find prompt, a list,
the allow line, a dialog the page opens, a file input's path — the row comes
back and the page is a row shorter until it closes: a `confirm()` in a
fullscreen video is still answered on the row. Link hints and normal mode
work in fullscreen as anywhere.

## Tests

`cargo test` runs the unit tests and skips everything that needs an engine.
The tests in `tests/engine.rs` are the ones this repository is for: a real
Chromium, real frames, and tOS's terminal (`tos_term::Terminal`, copied into
`vendor/tos-term` and not published) with the compositor's own `ImageFiles`
installed parsing what would go down the pane's pseudoterminal.
They run only when `BLINKTERM_ENGINE` names the engine to use, and they say so
when they skip — naming the engine is the consent, because a machine with a
Chromium on it did not thereby agree to have it started.

```sh
BLINKTERM_ENGINE=/opt/chrome-headless-shell-linux64/chrome-headless-shell \
  cargo test --release -- --test-threads=1
```

`tests/remote.rs` runs the binary itself as `blinkterm --remote`, as `gh` or
`xdg-open` would, against a socket the test listens on; it needs no engine
and runs with the rest of `cargo test`.

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
`clippy::undocumented_unsafe_blocks`: there are eighty-one `unsafe` blocks in
`src/`, nearly all of them one-line `libc` calls, and each says what makes it
sound.

CI also runs clippy, the unit tests and the real-engine suite on macOS. That
job exercises the `shm_open` frame path, app-bundle search and macOS scroll
clock that no Linux job can run; the `msrv` job separately checks the same
code for `aarch64-apple-darwin` on the floor toolchain.

`--locked` throughout, so that CI tests what is committed rather than what
cargo would resolve today, and a stale lock file is a red build.

The floor is Rust **1.87**: `src/` calls `is_multiple_of`. CI builds against
exactly the `rust-version` in `Cargo.toml`, so the number stays true.

`tools/` is checked too: `shellcheck --severity=warning tools/run.sh`, and
`ruff check --select E9,F tools/` — syntax and pyflakes, not style, since
those scripts are stdlib-only by design and some of what a style rule would
object to is deliberate.

## Contributing, releases, security

[CONTRIBUTING.md](CONTRIBUTING.md) has the checks, how to run the engine tests,
and where the code copied from tOS came from. [RELEASING.md](RELEASING.md) says what the
version number promises — SemVer on the command-line surface, and `lib.rs` is
not a stable API — and how a tag is cut. [CHANGELOG.md](CHANGELOG.md) is what
changed.

[SECURITY.md](SECURITY.md) has the threat model and how to report something
privately. Worth reading before pointing this at a page you do not trust: it
says which parts are Chromium's problem, which are this program's, and which
are currently open holes.

## Licence

MIT. See [LICENSE](LICENSE).
