# Using blinkterm

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
| `ctrl+shift+h` (or `alt+h`) | the history list: every page visited, newest first, `title — url` and when; type words in any order to filter by title or url, `↑`/`↓` to pick, `enter` opens it here, `alt+enter`/`ctrl+enter` (or a middle click) in a new tab, `shift+delete` forgets it, `esc` closes. See [History](configuration.md#history) |
| `ctrl+shift+j` (or `alt+j`) | the page's console: `console.*` calls, uncaught exceptions and requests that failed (status 400 and up, or a network error), newest last, each row `level  text — url:line`; type words in any order to filter, `↑`/`↓` scroll, `esc` or `enter` closes. Kept per tab, the last 1000, across navigations with a `-- navigated to …` row between. `ctrl+shift+j` is Kitty's and Ghostty's own by default; `alt+j` reaches every terminal. See [The console](features.md#the-console) |
| `ctrl+shift+pageup` / `pagedown` (or `alt+shift+pageup` / `pagedown`) | move this tab left or right |
| middle click or `ctrl`+click on a link | open it in a tab behind this one |
| a click on the status row | on a tab, switch to it; a middle click closes it; on `+N` at either end, the nearest tab past that end; on the url (or anywhere with one tab), the url bar, as `ctrl+l` |
| `alt+=` / `alt+-` | zoom in and out (`ctrl+=` / `ctrl+-` where your terminal lets them through) |
| `alt+0` / `ctrl+0` | back to 100% |
| your terminal's paste key | pastes into the page, the url bar, the find prompt, a `prompt()` or a file input's path — whichever has the cursor |
| `alt+c` | copy the page's selection to your clipboard; with the url bar, the find prompt, a `prompt()` or a file input's path open, copy that line |
| `alt+u` | copy the page's url to your clipboard |
| `alt+o` | open this page in the desktop browser — `open` on a Mac, `xdg-open` or `$BROWSER` on Linux, or `external-browser`; nothing of your login goes with it. Over ssh with no display the row says so; `alt+u` copies the url. See [Opening a page in the desktop browser](features.md#opening-a-page-in-the-desktop-browser) |
| `alt+s` | save this page as a PDF in the download directory, named after its title; the row says `saved ~/Downloads/<title>.pdf`. See [Saving a page](features.md#saving-a-page) |
| `alt+shift+s` | save the whole page, top to bottom, as a PNG there. A page past sixteen million pixels is cut to its top, and the row says how much |
| `alt+p` | allow this site something: the row says `allow https://site: ` and the words it is allowed now, all selected; type any of `camera` `microphone` `location` `notifications` `clipboard`, `enter` sets exactly those (an empty line takes them all back), `esc` leaves it. See [Sound, permissions and fullscreen](features.md#sound-permissions-and-fullscreen) |
| `alt+b` | stop blocking ads and trackers on this site, or start again: the row says `blocking off for example.com`, and `unblocked` while you are on it. Reload to get what was blocked. In the url bar `alt+b` is still a word back |
| `alt+shift+r` | read the site styles and scripts again: the row says `site files: 2 styles, 1 script`; styles change the page where it stands, scripts from the next load. See [Site styles and scripts](features.md#site-styles-and-scripts) |
| `alt+l` | fill the login form from your password manager: runs `password-command` for this page's host and puts what it printed into the password field and the user-name field before it, in the page or a same-origin frame; never submits; only on https or localhost. See [Filling a login from your password manager](features.md#filling-a-login-from-your-password-manager) |
| `alt+r` | reader mode: the article alone — its title, byline, text, pictures and links — at a readable width, in the page's colour scheme; `alt+r` again puts the page back where it was. The row says `reader` while it is on, and a page with no article says `no article on this page`. See [Reader mode](features.md#reader-mode) |
| `ctrl+q` | close this window (the other windows on the profile stay) |
| a page's dialog | its `alert`, `confirm`, `prompt` or "leave this page?" takes the top row: any key for an alert, `y`/`n` for a question, or type and `enter` for a prompt; `esc` says no |
| a page's file input | click it: the row asks for a path — `tab` completes names, `~` is home, one path per `enter` when the page takes several and an empty `enter` sends them; `esc` sends nothing |
| `esc` | while a page is fullscreen and nothing else has the row, leave fullscreen; while a page is loading, stop it |
| `ctrl+.` | normal mode on or off. In normal mode the letters are keys of their own and the row says `normal`: `f` labels everything clickable on the screen and typing a label clicks it (`F` opens a link in a tab behind this one); `j`/`k` scroll a notch, `d`/`u` half a screen, `gg`/`G` to the top and bottom; `H`/`L` back and forward, `r` reload, `o` the url bar, `O` a new tab, `/` find; `i` goes back to typing into the page, as does clicking into a field. Off by default: nothing changes until you press it |

These are the keys of `keymap = linux`, the default everywhere but macOS;
on a Mac the default is `keymap = mac`, whose keys are in
[On a Mac](#on-a-mac) below.

Everything else goes to the page, including the mouse, unless a `key.` line
in the [settings](configuration.md#rebinding-keys) takes it. A link that asks for a
new window gets a new tab, and the tab is switched to.

With more tabs than the row can name, the strip shows a run of them around
the one in front and `+N` at either end for how many are past it; it scrolls
when the tab in front reaches an edge. Clicking a tab in the strip switches
to it, a middle click closes it, and clicking `+N` brings the tab past that
end into view. The list (`ctrl+shift+a`) shows all of
them. Kitty and Ghostty keep `ctrl+shift+a` and `ctrl+shift+j` (the
console) for themselves and every terminal keeps
`ctrl+shift+pageup`/`pagedown` — Kitty and tOS for the scrollback,
WezTerm and Ghostty for their own tabs — so each has an `alt` form that
reaches the program everywhere. Ghostty also keeps `alt+1`..`alt+9` for its
tabs; its `alt+9` is its last tab, as it is here. Kitty on macOS keeps far
more, and Option is not `alt` there, which is what [On a Mac](#on-a-mac) is
about. A middle or `ctrl` click
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

### On a Mac

On macOS the built-in keys are `keymap = mac`, because the Linux ones mostly
never arrive there. Kitty on macOS keeps `cmd+t`, `cmd+w`, `cmd+l` (clear
the last command), `cmd+r` (resize the window), `cmd+f` (search the
scrollback), `cmd+1` … `cmd+9` (its windows), `cmd+=`, `cmd+-`, `cmd+0`,
`cmd+k`, `cmd+n`, `cmd+m`, `cmd+h`, `cmd+q`, `cmd+enter`, `cmd+,`, the
`cmd` arrows and page keys, `` cmd+` ``, `shift+cmd+[`/`]`, and on every
platform `ctrl+tab`, `ctrl+shift+tab` and its `ctrl+shift` keys — a key that
hits one of Kitty's mappings never reaches the program. And Option is not
`alt`: with Kitty's default `macos_option_as_alt no` it makes a character,
so Option+= arrives as `≠` and is not `alt+=`. So the Mac keymap puts the
keys on `cmd` where Kitty leaves it free and Chrome or Safari use it, and on
`ctrl` elsewhere, and needs nothing in `kitty.conf`. It is the default on
macOS and can be chosen anywhere with `keymap = mac` or `--keymap mac` — over
ssh from a Mac, say; `keymap = linux` is the other. `blinkterm --help`
lists the keys of this platform's keymap, and `blinkterm --keymap linux
--help` (or `--keymap mac`) those of the one named. These are the keys that
differ:

| action | linux | mac |
| --- | --- | --- |
| `back` | `alt+left` | `cmd+[` |
| `forward` | `alt+right` | `cmd+]` |
| `reopen-tab` | `ctrl+shift+t`, `alt+t` | `cmd+shift+t` |
| `bookmark` | `ctrl+d` | `cmd+d` |
| `next-tab` | `ctrl+tab` | `cmd+alt+right`, `ctrl+pagedown` |
| `previous-tab` | `ctrl+shift+tab` | `cmd+alt+left`, `ctrl+pageup` |
| `tab-1` … `tab-8` | `alt+1` … `alt+8` | `ctrl+1` … `ctrl+8` |
| `last-tab` | `alt+9` | `ctrl+9` |
| `list-tabs` | `ctrl+shift+a`, `alt+a` | `cmd+shift+a` |
| `history` | `ctrl+shift+h`, `alt+h` | `cmd+y` |
| `console` | `ctrl+shift+j`, `alt+j` | `cmd+alt+j` |
| `move-tab-left` | `ctrl+shift+pageup`, `alt+shift+pageup` | `cmd+shift+pageup` |
| `move-tab-right` | `ctrl+shift+pagedown`, `alt+shift+pagedown` | `cmd+shift+pagedown` |
| `zoom-in` | `alt+=`, `ctrl+=` | `ctrl+=` |
| `zoom-out` | `alt+-`, `ctrl+-` | `ctrl+-` |
| `zoom-reset` | `alt+0`, `ctrl+0` | `ctrl+0` |
| `reader` | `alt+r` | `cmd+shift+r` |
| `permissions` | `alt+p` | `cmd+p` |
| `block` | `alt+b` | `cmd+b` |
| `reload-sites` | `alt+shift+r` | `cmd+alt+shift+r` |
| `fill-login` | `alt+l` | `cmd+shift+l` |
| `copy` | `alt+c` | `cmd+c`, `cmd+shift+c` |
| `copy-url` | `alt+u` | `cmd+u` |
| `open-external` | `alt+o` | `cmd+shift+o` |
| `save-pdf` | `alt+s` | `cmd+s` |
| `save-screenshot` | `alt+shift+s` | `cmd+shift+s` |

`ctrl+q`, `ctrl+l`, `ctrl+r`, `ctrl+t`, `ctrl+w`, `ctrl+f` and `ctrl+.` are
the same on both. `cmd+c` copies because Kitty's own `cmd+c` copies only a
selection Kitty made and passes the key on when there is none — and with the
mouse given to the page, there is none. The Linux keys still answer under
the Mac ones, so with `macos_option_as_alt left` in `kitty.conf` the `alt`
chords above work too. `ctrl+1` … `ctrl+9`, `ctrl+=` and the other `ctrl`
chords need a terminal speaking the Kitty keyboard protocol, which Kitty,
WezTerm and Ghostty do; without it they arrive as the bare key.
`cmd+shift+r` is Safari's Reader and `cmd+alt+j` Chrome's console on a Mac;
`reload-sites` is `cmd+alt+shift+r` because Kitty keeps `cmd+r` and
`cmd+alt+r`.

A `key.` line on a chord Kitty keeps would do nothing, so in Kitty the
status row says so at start, with the `kitty.conf` line that frees it, and
`blinkterm --doctor` lists every such line in any terminal, with the ones
on a chord macOS keeps for itself (Mission Control's `ctrl+←`/`→`, Spotlight's
`cmd+space`). For the browser's own keys anyway, give them back to the
program in `kitty.conf` and bind them in blinkterm's settings:

```
# kitty.conf
macos_option_as_alt left
map cmd+t no_op
map cmd+w no_op
map cmd+l no_op
map cmd+r no_op
map cmd+f no_op
map cmd+1 no_op   # … cmd+9
map cmd+equal no_op
map cmd+minus no_op
map cmd+0 no_op
map ctrl+tab no_op
map ctrl+shift+tab no_op
```

```
# ~/.config/blinkterm/config
key.cmd+t = new-tab
key.cmd+w = close-tab
key.cmd+l = url
key.cmd+r = reload
key.cmd+f = find
key.cmd+1 = tab-1
# … key.cmd+8 = tab-8, key.cmd+9 = last-tab
key.cmd+= = zoom-in
key.cmd+- = zoom-out
key.cmd+0 = zoom-reset
```

`ctrl+tab` and `ctrl+shift+tab` need no `key.` line once Kitty lets them
through: they are the Linux keys, which still answer.

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
Ghostty keep them for their own font size. On a Mac the zoom keys are the
`ctrl` ones, since Option does not make `alt` (see [On a Mac](#on-a-mac)).
The level is remembered per
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

### Transparent pages

`--alpha` (`alpha = true` in the file) lets the terminal's background show
through a page — its colour, and its opacity or blur if it has them. The
engine paints nothing behind the page, and the page's own `html` and `body`
backgrounds are made transparent, image and all, so a light page is its
text and pictures over your terminal. What the page paints on anything else
stays: a site whose white is a container of its own — a wrapper `div`, a
card — keeps that part opaque, and so do an iframe from another site and a
page's own `!important` inline style on `body`.

A number is how opaque the whole picture is: `--alpha 70` (or
`--alpha=70`, `alpha = 70`) sends everything, text included, at 70%, and
`--alpha` alone is 100. `--alpha=false` turns off an `alpha = …` in the
file. Over ssh and inside tmux the number is not applied: the pages are
still see-through, and what is left is sent opaque.

It is see-through while the page moves as well as at rest. The moving
frames are JPEG, which has no transparency, so locally the page is painted
on magenta (`#ff00ff`) and the magenta is taken back out of every frame and
every still as they are decoded — a chroma key, about a millisecond a
frame. What that costs you is what the page itself shows in magenta or a
vivid purple, which goes transparent too; a half-transparent overlay over
the bare page comes out a little more opaque than it is; and pink or purple
text right against the see-through background loses its pink at the edges
(a visited link's purple goes navy there). Over ssh and inside tmux the
frames are the engine's PNG and carry real transparency, and there is no
key. A page that says nothing about its colours is black text on your
background, which on a dark terminal is unreadable: use `--force-dark` with
it, which makes the text light and keeps the transparency, or a light
terminal. There is no key to turn it off while running.

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
it how many requests a [block list](features.md#blocking-ads-and-trackers) stopped on
this page, `12 blocked` — counted from the page's last landing, per tab —
or `unblocked` on a site you turned blocking off for, and `reader` while
the page is in [reader mode](features.md#reader-mode). After that,
`2 errors`: the errors the page logged, threw or failed to fetch since you
last opened its [console](features.md#the-console) (`ctrl+shift+j`) on
this tab. Once you have more than one
[named profile](configuration.md#named-profiles), the profile's name is the
first word at the right, so a work window and a personal one cannot be
mistaken for each other; with one profile, `--profile <dir>` or
`--temp-profile` it is not shown.

The url bar, the find prompt, the tab list, the history list or the console, the allow
line (`alt+p`), a page's dialog and a file input's path take the whole row while they are
open, and `esc` goes to whichever of them has it before it leaves
fullscreen or stops a load; no link is shown while one of them is there.
The offer to restore the last run's tabs takes it too, after all of them.

The row answers the mouse only while the strip, or the title and url, is on
it: a click on a tab, a `+N` or the url does what the keys would (see
[Keys](#keys)), and a click while anything else has the row does nothing. A
press on the row is never the page's, and neither is its release — a drag
that began on the page and ended on the row still ends on the page.

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
as with `--restore`. Every window on the profile comes back with its own
tabs. If it dies again within a minute, or cannot be started, every window
on the profile closes, and the message says how many tabs were saved and
that `blinkterm --restore` reopens them; the next plain start offers to.

To know what is under the pointer the terminal is asked to report every
mouse movement, not only presses (`?1003h`), so the page now sees the
pointer move — hover styling and tooltips work — at the cost of one small
command to the engine per screen refresh while it moves and nothing while
it rests.

## Several terminals, one profile

Start `blinkterm` in a second terminal on a profile a first one is using and
it opens a window of its own: the same cookies and logins, the same history
and bookmarks, but its own tabs, its own tab in front, its own url bar and
prompts, at its own size, answering its own keys. A third terminal is a third
window. Closing one — `ctrl+q`, or closing the terminal — closes that window
and nothing else; the others go on as they were.

```sh
blinkterm https://mail.example      # in one terminal
blinkterm https://docs.example      # in another: a second window, same login
```

What does this is one background process per profile, its *backend*, which
the first terminal starts and which runs the engine and keeps every window:
one engine for the profile however many terminals are on it, as one cookie
database needs. It has no terminal and no window of its own, and it stops by
itself when the last window on the profile is closed — after asking the
engine to write its cookies and waiting the two seconds or so that takes,
which it does on its own once your terminal has been given back. So there is
no daemon to manage and nothing left running. A terminal started while it is
in those two seconds waits for it to finish, then starts a fresh one.

A window whose terminal disappears without closing it — the terminal
emulator killed, an ssh connection dropped — is kept for fifteen seconds,
its page no longer painting, and then closed; its tabs are kept in the
session as a lost window, and the next window opened on the profile is
offered them (`restore 3 tabs from last time? y/n`). The other windows are
not touched. The other way round, a terminal whose connection to the
backend drops while the backend is still running reconnects within those
fifteen seconds and carries on in the same window, redrawn; keys typed
while it was cut off are dropped rather than sent late. A different profile is a different backend with an engine of
its own, sharing nothing.

The settings that are about the engine as a whole (the engine, its
arguments, the user agent, the proxy, the download directory, the block
lists, the site files, the console) are set by the first terminal on a
profile; a second one asking for different ones is refused with the setting
named. Every other setting is each window's own. See
[Several terminals on one profile](configuration.md#several-terminals-on-one-profile).

When something goes wrong that the row cannot say, the backend's standard
error is `backend.log` in the profile directory: the engine's own warnings,
and a line for each window that lost its terminal or page that was closed
because no window could be shown to have asked for it.

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
behind it — in the window you last typed in or clicked, when the profile has
several — and exits at once with 0. A profile whose windows have all just
lost their terminals has nowhere to put a page, says so (`nowhere`), and the
sender starts a window of its own as if none were running. With none running it starts as usual,
with those urls — so the same `$BROWSER` works whether one is open or not, as
long as there is a terminal to start one in; with no terminal (a desktop's
`xdg-open`) it says there is nobody to hand the url to, and exits 1.

The profile decides which `blinkterm` is reached, exactly as it decides which
profile a start takes: `blinkterm --remote --profile ~/work-profile <url>`
reaches the one on `~/work-profile`, and `blinkterm --remote --profile-name
Work <url>` the one on the profile named `Work`. Without either it is the
default profile; `--remote` never opens the profile picker, so with no
default it says so and exits 1, and `--remote --choose-profile` is refused.
A `--temp-profile` run is its own and never listens, and `--remote
--temp-profile` is refused as a contradiction.

Each url is read as the url bar reads what is typed — `example.com` is
`https://example.com`, `localhost:3000` is `http://`, a path is `file://` —
and it must come out as an `http`, `https`, `file` or `about` url. Anything
else (`javascript:`, `data:`, `chrome://`, `mailto:`) is refused: the sender
prints one line per refused url on stderr and exits 1, and the urls that were
fine are opened anyway.

The other direction is `alt+o`, which opens the page in front in the desktop
browser. A `$BROWSER` that is `blinkterm --remote` is not what `alt+o` runs,
and is taken out of the environment of what it runs instead (see
[Opening a page in the desktop browser](features.md#opening-a-page-in-the-desktop-browser)).

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

## When a start on a profile in use says no

A second `blinkterm` on a profile opens a window of the one already serving
it, and most of what can go wrong there ends with one of these sentences.
What they mean, and what to do:

- `the profile at <dir> is in use by pid <pid>, which is not taking windows`
  — the lock is held by a process that never started listening for windows
  in thirty seconds: an older blinkterm (from before windows were shared), a
  backend stuck on its way out, or something else holding
  `<dir>/blinkterm.lock`. Quit that blinkterm; `ps -p <pid>` says what the
  pid is. Meanwhile `blinkterm --remote <url>` reaches an older blinkterm,
  and `--temp-profile` or `--profile <other dir>` starts one beside it.
- `the blinkterm serving <dir> (pid <pid>) is version <a> and this is <b>`
  — an upgrade while windows were open. Quit that version's windows (each
  `ctrl+q`); the next start runs the new one.
- `the blinkterm serving <dir> (pid <pid>) runs the engine with <key> =
  <value>, and this start asked for <other>` — `engine`, `engine-arg`,
  `user-agent`, `proxy`, `mute`, `download-dir`, `block-list`, `block`,
  `sites-dir`, `sites` and `console` are the engine's, so every window on a
  profile has the same ones, whether they came from the command line or the
  settings file. The key named is the first that differs. Start with the
  same settings, quit the other windows first, or give this one a profile
  of its own.
- `the blinkterm serving <dir> stopped unexpectedly; what it said is in
  <dir>/backend.log` — the process holding the profile and the engine went
  without a word: killed, or crashed. Each window on the profile ends with
  this; `backend.log` has its last words. A window whose connection drops
  while that process is still running tries for fifteen seconds to take
  the window back, and carries on in it if it can; this is said at once
  when nothing holds the profile any more, and otherwise once the fifteen
  seconds are out. The next start cleans up after it
  — the old engine is stopped first if any of it is still running — and
  offers the tabs back.
- `the blinkterm serving <dir> stopped: <why>` — it stopped on purpose,
  usually because the engine died twice in a minute; the sentence says how
  many tabs were saved, and `blinkterm --restore` reopens them.
- `an engine from a previous blinkterm (process group <n>) is still running
  on <dir> and would not stop` — a crashed blinkterm's engine is still on
  the profile and did not stop when killed. `kill -9 -<n>` (the minus sends
  it to the group; `pgrep -g <n>` lists what is in it), then start again.

A start that comes while the last window's blinkterm is writing the profile
out — the two seconds after the last `ctrl+q` that keep the cookies — waits
for it and then starts a fresh one; it says nothing unless that takes longer
than thirty seconds, when it prints `the blinkterm serving <dir> has been
shutting down for <n> s; try again in a moment`.
