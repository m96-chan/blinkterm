# Settings and profiles

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
(`force-dark = true`, `alpha = true`), and a path may start with `~/`.
There is no `url` setting: the page to open is what the command line is
for, and `home = <url>` is the page opened when none is given.
`normal-mode = true` starts in normal mode (`ctrl+.`), `restore = true`
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
`password-command` and `password-command-terminal` name a password
manager's command that `alt+l` fills a login form from (see
[Filling a login from your password manager](features.md#filling-a-login-from-your-password-manager)).

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
nothing else, for scripts.

### Rebinding keys

The platform default is `keymap = mac` on macOS and `keymap = linux` elsewhere.
`--keymap mac|linux` overrides the file. Both sets avoid stock Kitty 0.45
shortcuts; the mac set uses Control for most commands and Command where
Kitty leaves it free. Option is not required on macOS.

`key.<chord> = <action>` overrides the selected default; `none` gives that
global chord to the page. These remain plain `name = value` settings, with
no TOML sections:

```ini
keymap = mac
key.f5 = reload
key.ctrl+w = none
key.line.ctrl+a = line-home
key.find.ctrl+n = find-next
key.normal.shift+j = scroll-bottom
key.prompt.ctrl+w = none
key.history.shift+delete = none
key.url.enter = none
key.hints.esc = none
```

| action | Linux default | macOS default | does |
| --- | --- | --- | --- |
| `quit` | `ctrl+q` | `ctrl+q` | quit |
| `url` | `ctrl+l` | `ctrl+l` | type a url |
| `reload` | `ctrl+r` | `ctrl+r` | reload |
| `back` | `alt+left` | `cmd+[` | back |
| `forward` | `alt+right` | `cmd+]` | forward |
| `new-tab` | `ctrl+t` | `ctrl+t` | a new tab |
| `close-tab` | `ctrl+w` | `ctrl+w` | close this tab |
| `reopen-tab` | `alt+t` | `ctrl+y` | reopen the tab closed last |
| `bookmark` | `ctrl+d` | `cmd+d` | bookmark this page, or remove the bookmark |
| `next-tab` | `ctrl+]` | `ctrl+]` | the next tab |
| `previous-tab` | `ctrl+[` | `ctrl+[` | the tab before |
| `tab-1 .. tab-8` | `alt+1 .. alt+8` | `ctrl+1 .. ctrl+8` | the nth tab |
| `last-tab` | `alt+9` | `ctrl+9` | the last tab |
| `list-tabs` | `alt+a` | `ctrl+e` | the tab list |
| `history` | `alt+h` | `ctrl+h` | the history list |
| `move-tab-left` | `alt+shift+pageup` | `ctrl+pageup` | move this tab left |
| `move-tab-right` | `alt+shift+pagedown` | `ctrl+pagedown` | move this tab right |
| `zoom-in` | `alt+=`, `ctrl+=` | `ctrl+=` | zoom in |
| `zoom-out` | `alt+-`, `ctrl+-` | `ctrl+-` | zoom out |
| `zoom-reset` | `alt+0`, `ctrl+0` | `ctrl+0` | back to 100% |
| `find` | `ctrl+f` | `ctrl+f` | find in the page |
| `permissions` | `alt+p` | `ctrl+p` | allow this site the camera, microphone, location, notifications or clipboard |
| `block` | `alt+b` | `ctrl+b` | stop blocking ads and trackers on this site, or start again |
| `fill-login` | `alt+l` | `ctrl+g` | fill the login form from your password manager |
| `copy` | `alt+c` | `ctrl+c` | copy the selection, or the line being typed |
| `copy-url` | `alt+u` | `ctrl+;` | copy the url |
| `save-pdf` | `alt+s` | `ctrl+s` | save this page as a PDF |
| `save-screenshot` | `alt+shift+s` | `ctrl+shift+d` | save the whole page as a picture |
| `normal-mode` | `ctrl+.` | `ctrl+.` | normal mode on or off |

Chords join `ctrl`, `alt` (`opt`), `shift`, `super` (`cmd` or `meta`) with `+`
to a character or `plus`, `space`, `tab`, `enter`, `esc`, `backspace`,
`insert`, `delete`, arrows, `home`, `end`, `pageup`, `pagedown`, `f1`…`f24`.
They match modifiers exactly: `ctrl+=` does not also bind `ctrl+shift+=`.
Lock bits are ignored. Global bindings need Control, Alt or Command, or an
f-key, so bare letters still type into the page. Scoped bindings may use
bare or shifted letters. A later line replaces an earlier line for the same
scope and chord; rebinding one chord leaves the action's other chords alone.

For Option chords when using the Linux set on macOS, Kitty can send Left
Option as Alt with `macos_option_as_alt left`. To restore the old tab-switch
chords, first free them in Kitty:

```conf
macos_option_as_alt left
map ctrl+tab
map ctrl+shift+tab
```

Then configure blinkterm:

```ini
key.ctrl+tab = next-tab
key.ctrl+shift+tab = previous-tab
```

### Scoped editing and normal-mode keys

`key.<scope>.<chord> = <action>` changes only that interaction. `none` consumes
the key without editing, inserting text, or falling back to its default.
The most specific scope wins over common editing and global copy/quit.
`url`, `find`, `list`, `history`, `prompt` and `upload` inherit `line` edits;
`history` also inherits `list`, and `prompt` inherits explicit `dialog`
overrides. Prompt text does not inherit confirmation-letter defaults.

| scope | actions |
| --- | --- |
| line (also URL, find, list, history, prompt, upload) | `line-home`, `line-end`, `line-left`, `line-right`, `line-word-left`, `line-word-right`, `line-delete-back`, `line-delete-forward`, `line-delete-word-back`, `line-delete-word-forward`, `line-kill-start`, `line-kill-end`, `line-hint`, `line-previous`, `line-next`, `accept`, `cancel` |
| find | `find-next`, `find-previous` |
| list (also history) | `list-previous`, `list-next`, `list-page-up`, `list-page-down` |
| history | `history-new-tab`, `history-forget` |
| dialog (also explicit prompt overrides and the restore offer) | `dialog-accept`, `dialog-dismiss` |
| upload | `upload-complete` |
| normal | `hints`, `hints-new-tab`, `scroll-down`, `scroll-up`, `half-down`, `half-up`, `top-prefix`, `scroll-top`, `scroll-bottom`, `back`, `forward`, `reload`, `url`, `new-tab`, `find`, `insert` |
| hints | `hints-cancel`, `hints-backspace` |
| page | `page-escape` (leave fullscreen or stop loading) |

`quit` is also available in every scope. Normal-mode `top-prefix` is the
first `g` of `gg`; `scroll-top` jumps there in one press. Outside normal
mode its bare bindings never take typing from the page. Global `copy`,
`copy-url` and `quit` reach through editors unless a scoped binding overrides
them. In dialogs and uploads, tab commands still work as before.

`blinkterm --help` lists the platform defaults and scoped action names;
`blinkterm --keymap linux --help` shows the Linux set without reading config.
`blinkterm --doctor` warns about effective custom chords reserved by stock
Kitty 0.45, naming the Kitty action and the `map <chord>` line that frees it.
Warnings use the host OS, independently of the selected keymap. Overridden
lines and `none` do not warn; custom Kitty configurations are not inspected.

To choose Command-L/R/F despite Kitty's stock reservations, first add to
`kitty.conf` and reload Kitty's configuration:

```conf
map cmd+l
map cmd+r
map cmd+f
```

Then add to blinkterm's config:

```ini
key.cmd+l = url
key.cmd+r = reload
key.cmd+f = find
```

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

`alt+h` (Linux) or `ctrl+h` (macOS) lists it over the screen, newest first: each row
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
`--temp-profile` keeps the session in memory, for reopening closed tabs, and writes
nothing.
