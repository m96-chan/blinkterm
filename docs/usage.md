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
| `alt+=` / `alt+-` | zoom in and out (`ctrl+=` / `ctrl+-` where your terminal lets them through) |
| `alt+0` / `ctrl+0` | back to 100% |
| your terminal's paste key | pastes into the page, the url bar, the find prompt, a `prompt()` or a file input's path — whichever has the cursor |
| `alt+c` | copy the page's selection to your clipboard; with the url bar, the find prompt, a `prompt()` or a file input's path open, copy that line |
| `alt+u` | copy the page's url to your clipboard |
| `alt+s` | save this page as a PDF in the download directory, named after its title; the row says `saved ~/Downloads/<title>.pdf`. See [Saving a page](features.md#saving-a-page) |
| `alt+shift+s` | save the whole page, top to bottom, as a PNG there. A page past sixteen million pixels is cut to its top, and the row says how much |
| `alt+p` | allow this site something: the row says `allow https://site: ` and the words it is allowed now, all selected; type any of `camera` `microphone` `location` `notifications` `clipboard`, `enter` sets exactly those (an empty line takes them all back), `esc` leaves it. See [Sound, permissions and fullscreen](features.md#sound-permissions-and-fullscreen) |
| `alt+b` | stop blocking ads and trackers on this site, or start again: the row says `blocking off for example.com`, and `unblocked` while you are on it. Reload to get what was blocked. In the url bar `alt+b` is still a word back |
| `alt+l` | fill the login form from your password manager: runs `password-command` for this page's host and puts what it printed into the password field and the user-name field before it, in the page or a same-origin frame; never submits; only on https or localhost. See [Filling a login from your password manager](features.md#filling-a-login-from-your-password-manager) |
| `ctrl+q` | quit |
| a page's dialog | its `alert`, `confirm`, `prompt` or "leave this page?" takes the top row: any key for an alert, `y`/`n` for a question, or type and `enter` for a prompt; `esc` says no |
| a page's file input | click it: the row asks for a path — `tab` completes names, `~` is home, one path per `enter` when the page takes several and an empty `enter` sends them; `esc` sends nothing |
| `esc` | while a page is fullscreen and nothing else has the row, leave fullscreen; while a page is loading, stop it |
| `ctrl+.` | normal mode on or off. In normal mode the letters are keys of their own and the row says `normal`: `f` labels everything clickable on the screen and typing a label clicks it (`F` opens a link in a tab behind this one); `j`/`k` scroll a notch, `d`/`u` half a screen, `gg`/`G` to the top and bottom; `H`/`L` back and forward, `r` reload, `o` the url bar, `O` a new tab, `/` find; `i` goes back to typing into the page, as does clicking into a field. Off by default: nothing changes until you press it |

Everything else goes to the page, including the mouse, unless a `key.` line
in the [settings](configuration.md#rebinding-keys) takes it. A link that asks for a
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
or `unblocked` on a site you turned blocking off for. After that, `2 errors`:
the errors the page logged, threw or failed to fetch since you last opened
its [console](features.md#the-console) (`ctrl+shift+j`) on this tab.

The url bar, the find prompt, the tab list, the history list or the console, the allow
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
