# Settings and profiles

## Settings

Everything on the command line can also be kept in
`$XDG_CONFIG_HOME/blinkterm/config` — `~/.config/blinkterm/config` when
`XDG_CONFIG_HOME` is not set, on macOS as on Linux — one setting per line,
named as the option is without its `--`:

    # what a page is told about you
    color-scheme = dark
    scale = 2
    # the terminal through the page, at 70%
    alpha = 70
    force-dark = true
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
(`force-dark = true`), and a path may start with `~/`. `alpha` is
`true`, `false` or an opacity from 1 to 100 (`alpha = 70`; see
[Transparent pages](usage.md#transparent-pages)).
There is no `url` setting: the page to open is what the command line is
for, and `home = <url>` is the page opened when none is given.
`normal-mode = true` starts in normal mode (`ctrl+.`), `keymap = mac|linux`
(or `--keymap`) picks the built-in keys (see [Rebinding keys](#rebinding-keys)),
`restore = true`
reopens the last session's tabs (see [The session](#the-session)), and
`mute = true` (or `--mute`) starts the engine silent (see
[Sound](features.md#sound-permissions-and-fullscreen)). `tmux = on|off|auto`,
`frames = raw|png|auto`, `fps = <n>` and `probe = false` are `--tmux`,
`--frames`, `--fps` and `--no-probe` (see [Where it runs](design.md#where-it-runs)).
`file-picker`, `file-picker-terminal`, `file-picker-multiple` and
`file-picker-terminal-multiple` name a program that chooses a file for a
page's file input instead of the row (see
[Choosing it with another program](features.md#choosing-it-with-another-program)).
`pdf-paper = a4|letter` (or `--pdf-paper`) is the paper `alt+s` prints on
(see [Saving a page](features.md#saving-a-page)).
`block-list = <path>`, repeatable, names a list of hosts to block, and
`block = false` (or `--no-block`) blocks nothing whatever the lists say
(see [Blocking ads and trackers](features.md#blocking-ads-and-trackers)).
`sites-dir = <dir>` names where site styles and scripts are read from, and
`sites = false` (or `--no-sites`) reads none (see
[Site styles and scripts](features.md#site-styles-and-scripts)).
`console = false` (or `--no-console`) stops listening to the page's console,
and `ctrl+shift+j` says so (see [The console](features.md#the-console)).
`password-command` and `password-command-terminal` name a password
manager's command that `alt+l` fills a login form from (see
[Filling a login from your password manager](features.md#filling-a-login-from-your-password-manager)).
`external-browser` names the program `alt+o` opens the page with (see
[Opening a page in the desktop browser](features.md#opening-a-page-in-the-desktop-browser)).

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
[What a page is told](design.md#what-a-page-is-told).)

Several urls open several tabs, the first in front:

    blinkterm https://example.com https://example.org

`--doctor` is the first thing to run in a new terminal: it starts the engine
on a throwaway profile, asks the terminal whether it speaks the Kitty
graphics and keyboard protocols — through tmux's passthrough as well, inside
tmux — and prints one line per answer and the route frames will take,
exiting 1 when the engine did not answer or the terminal cannot draw. In
tmux without `allow-passthrough` it says the graphics query went unanswered
raw and through tmux. `--print-engine` prints the path the search finds and
nothing else, for scripts. `--install-engine` fetches the engine the tests
pass against, checks it, and starts it once, like `--doctor`
([Installing](install.md#the-engine)).

### Rebinding keys

There are two sets of built-in keys, the keymaps. `keymap = linux` is the
one the program was built with: `ctrl` for what a browser has taught
everybody, `alt` for the rest. `keymap = mac` is the one for a Mac, where
Kitty keeps most `cmd` chords for itself and Option makes characters rather
than being `alt`: `cmd` where Kitty leaves it free and Chrome or Safari use
it, `ctrl` elsewhere. Each is the default on its platform — `mac` on macOS,
`linux` everywhere else — so a Mac in a stock Kitty gets keys that arrive
with no change to `kitty.conf`. Either can be set, in the file or with
`--keymap mac|linux`: over ssh from a Mac's Kitty to a Linux machine,
`keymap = mac` on the far side gives the keys the Mac's Kitty lets through.
[On a Mac](usage.md#on-a-mac) says what Kitty keeps and why.

`key.<chord> = <action>` in the settings file puts an action on a key, and
`key.<chord> = none` takes a key back for the page, on top of either
keymap:

    key.f5 = reload
    key.ctrl+b = back
    key.ctrl+w = none

| action | linux | mac | does |
| --- | --- | --- | --- |
| `quit` | `ctrl+q` | `ctrl+q` | quit |
| `url` | `ctrl+l` | `ctrl+l` | type a url |
| `reload` | `ctrl+r` | `ctrl+r` | reload |
| `back` | `alt+left` | `cmd+[` | back |
| `forward` | `alt+right` | `cmd+]` | forward |
| `new-tab` | `ctrl+t` | `ctrl+t` | a new tab |
| `close-tab` | `ctrl+w` | `ctrl+w` | close this tab |
| `reopen-tab` | `ctrl+shift+t`, `alt+t` | `cmd+shift+t` | reopen the tab closed last |
| `bookmark` | `ctrl+d` | `cmd+d` | bookmark this page, or remove the bookmark |
| `next-tab` | `ctrl+tab` | `cmd+alt+right`, `ctrl+pagedown` | the next tab |
| `previous-tab` | `ctrl+shift+tab` | `cmd+alt+left`, `ctrl+pageup` | the tab before |
| `tab-1` … `tab-8` | `alt+1` … `alt+8` | `ctrl+1` … `ctrl+8` | the nth tab |
| `last-tab` | `alt+9` | `ctrl+9` | the last tab |
| `list-tabs` | `ctrl+shift+a`, `alt+a` | `cmd+shift+a` | the tab list |
| `history` | `ctrl+shift+h`, `alt+h` | `cmd+y` | the history list |
| `console` | `ctrl+shift+j`, `alt+j` | `cmd+alt+j` | the page's console: logs, errors and failed requests |
| `move-tab-left` | `ctrl+shift+pageup`, `alt+shift+pageup` | `cmd+shift+pageup` | move this tab left |
| `move-tab-right` | `ctrl+shift+pagedown`, `alt+shift+pagedown` | `cmd+shift+pagedown` | move this tab right |
| `zoom-in` | `alt+=`, `ctrl+=` | `ctrl+=` | zoom in |
| `zoom-out` | `alt+-`, `ctrl+-` | `ctrl+-` | zoom out |
| `zoom-reset` | `alt+0`, `ctrl+0` | `ctrl+0` | back to 100% |
| `find` | `ctrl+f` | `ctrl+f` | find in the page |
| `reader` | `alt+r` | `cmd+shift+r` | the article without the page around it |
| `permissions` | `alt+p` | `cmd+p` | allow this site the camera, microphone, location, notifications or clipboard |
| `block` | `alt+b` | `cmd+b` | stop blocking ads and trackers on this site, or start again |
| `reload-sites` | `alt+shift+r` | `cmd+alt+shift+r` | read the site styles and scripts again |
| `fill-login` | `alt+l` | `cmd+shift+l` | fill the login form from your password manager |
| `copy` | `alt+c` | `cmd+c`, `cmd+shift+c` | copy the selection, or the line being typed |
| `copy-url` | `alt+u` | `cmd+u` | copy the url |
| `open-external` | `alt+o` | `cmd+shift+o` | open this page in the desktop browser |
| `save-pdf` | `alt+s` | `cmd+s` | save this page as a PDF |
| `save-screenshot` | `alt+shift+s` | `cmd+shift+s` | save the whole page as a picture |
| `normal-mode` | `ctrl+.` | `ctrl+.` | normal mode on or off |

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
actions too, with the keymap of the platform it runs on (the settings file
is not read for it; `blinkterm --keymap linux --help` lists the other), and
`blinkterm --doctor` says which keymap is in effect and names each `key.`
line on a chord Kitty or macOS keeps. In Kitty, such a line is also named
on the status row at start, with the `kitty.conf` line that frees it.

Under `keymap = mac` the `linux` column still answers underneath, so the
`alt` chords work on a Mac whose Kitty has `macos_option_as_alt left` (or
`right`, or `yes`), and the `ctrl` ones wherever Kitty lets them through.

## Profiles

Cookies, logins, local storage and the rest of what a site keeps are kept
between runs, in a profile: a directory, made readable by you alone (0700),
since a cookie is a login. You can have several, each with a name — a work
one, a personal one, one for testing — and each is its own identity, with
its own cookies, history and saved tabs. The first is `Default`, in
`$XDG_DATA_HOME/blinkterm/profile` — or `~/.local/share/blinkterm/profile`
when `XDG_DATA_HOME` is not set. The same on macOS, rather than
`~/Library/Application Support`: this is a program run from a shell, the
terminals it runs in keep their own settings under `~/.config` there too, and
a cookie jar is better kept out of what Time Machine and iCloud copy about.

```sh
blinkterm profiles create Work                           # a new, empty identity
blinkterm --profile-name Work https://example.com        # open it by name
blinkterm --choose-profile                               # ask which
blinkterm --profile ~/work-profile https://example.com   # a directory of yours
blinkterm --temp-profile https://example.com             # nothing kept
```

### Named profiles

The names are listed in `$XDG_DATA_HOME/blinkterm/profiles.json`, beside the
profiles, 0600; `blinkterm profiles` reads and changes it:

```sh
blinkterm profiles                          # the list; * marks the default
blinkterm profiles create Work              # made under profiles/<id>/
blinkterm profiles create Testing --dir ~/testing-profile   # one you already have
blinkterm profiles rename Work Office       # only the name changes
blinkterm profiles default Office           # what a plain start opens
blinkterm profiles remove Office
```

A name is up to 64 characters of plain text, with no `/` or `\` and no space
or `-` at either end, and two profiles cannot have names that differ only in
case — `--profile-name work` finds `Work`. A name is never part of a path: a
profile blinkterm makes lives in `profiles/<id>/` under the data directory,
where the id is twelve random hex characters, so renaming moves nothing.
A directory registered with `--dir` stays where it is and is never deleted
by blinkterm.

Which profile a start opens, highest first: `--profile-name`, `--profile`,
`--temp-profile` or `--choose-profile` on the command line (one of them;
two is an error); then `profile-name =`, `profile =`, `temp-profile = true`
or `choose-profile = true` in the settings file (again one of them); then the
default profile; then, when there is no default, a question. The question —
and `--choose-profile` always — is a numbered list on the terminal before
anything else starts: type a number to open that profile, `n` to make a new
one (and say whether it should be the default), `d N` to make profile N the
default, `r N` to rename it, `x N` to remove it, `q` to quit. A start with
no terminal to ask in — a script, a pipe — never waits: with no default it
says so and exits 1, and `blinkterm --remote` never asks at all. A name that
is not in the list is an error rather than a new profile made by a typo.
`--profile <dir>` and `--temp-profile` do not read the list.

The first start after upgrading writes `profiles.json` with one entry,
`Default`, pointing at the `profile` directory that was already there: no
cookie, history entry or saved tab moves, and an older blinkterm run
afterwards finds everything where it left it. The file has a `version`; one
that cannot be read, or that a newer blinkterm wrote, is refused with a
sentence naming it, and never overwritten — `--profile <dir>` works while it
is being fixed. Every change holds `profiles.lock` and replaces the file
whole, so two `blinkterm profiles` commands at once both land.

`remove` refuses a profile a running blinkterm is using. A profile blinkterm
made is moved, not deleted, to `$XDG_DATA_HOME/blinkterm/trash/<id>-<time>/`,
where its cookies and logins stay until you delete that directory; a `--dir`
profile is only taken out of the list. Removing the default leaves no
default, rather than quietly making another identity the one a start opens:
`blinkterm profiles default <name>` sets a new one. Once there are two
profiles or more, the status row shows the name of the one in use.

`--temp-profile` makes a fresh profile under the system's temporary directory
and removes it when `blinkterm` exits, including when it panics; one left by a
`blinkterm` that was killed outright is removed by the next one.

One `blinkterm` uses a profile at a time. A second one started on a profile
that is in use is refused, and told which pid has it; it does not quietly fall
back to a throwaway profile, because a login you thought was being kept and was
not is worse than an error. To open a url in the one that has it, say so:
`blinkterm --remote <url>` (see
[Opening a url from another program](usage.md#opening-a-url-from-another-program)). The lock is `blinkterm`'s own — an `flock` on
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
[Blocking ads and trackers](features.md#blocking-ads-and-trackers)).

### Bookmarks

`ctrl+d` bookmarks the page in front, and `ctrl+d` again removes the
bookmark; the row says which. They are kept in the profile, in a file called
`bookmarks` beside the history, so each profile has its own: the bookmarks
you make under `--profile-name Work` are not offered under your personal
profile. `--temp-profile` keeps them for the run only — `ctrl+d` works, the
row says the bookmark is for this run, and nothing is written. The file is
readable by you alone (0600), one bookmark a line:

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
that match before the history. Every change takes a lock on
`bookmarks.lock` beside the file and reads the file again first, so a change
made by hand or by a script while `blinkterm` runs is not lost; it shows up
in the url bar after the next `ctrl+d` or the next start.

Before profiles had their own, every profile shared one file,
`$XDG_DATA_HOME/blinkterm/bookmarks` (or `~/.local/share/blinkterm/bookmarks`).
The first start on the `Default` profile — the one an upgrade registers,
`$XDG_DATA_HOME/blinkterm/profile` — copies that file into it, once, says so
on the row, and leaves `bookmarks.migrated` beside the old file so that it is
never copied again, even if you empty the profile's. The old file is kept as
it was, for an older `blinkterm`; what either adds afterwards is its own. No
other profile gets a copy: to give one the old bookmarks, copy the file into
its directory yourself (`blinkterm profiles` lists where each one is), while
that profile is not running:

```sh
cp ~/.local/share/blinkterm/bookmarks <profile dir>/bookmarks
```

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

The tabs in the file are grouped under a line for the window they were in —
`# window 1 live`, `closed` or `lost` — after a `# format: 2` line. A run has
one window, so today the file has one group; the groups are there for the
profile that serves several terminals at once. A file written by an older
`blinkterm` reads as one group, and an older `blinkterm` reads this one as
one window with every group's tabs, since every line this version adds
starts with `#`.
