# Features

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
[Blocked hosts](configuration.md#blocked-hosts)); on the error page of a blocked site it
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
afterwards. Under `--alpha` the PNG is transparent wherever the page is,
its forced-transparent backgrounds included — keyed, locally, the way the
screen is, so the file is not magenta; a number after `--alpha` is not
applied to it, so the file is the page at full opacity.

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
[SECURITY.md](../SECURITY.md) for the details and the limits.

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
[Permissions](configuration.md#permissions)). A page with no origin — `about:blank`, a
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

## Site styles and scripts

A terminal pane is narrow, its font is not the one a site was designed
with, and a cookie banner or a sticky header takes more of it than of a
desktop window. A file of your own fixes a site for good: CSS or JavaScript,
named after the host, in a directory beside the settings file.

```text
~/.config/blinkterm/sites/         ($XDG_CONFIG_HOME/blinkterm/sites/)
  all.css                          every page
  *.github.com.css                 github.com and every host under it
  news.ycombinator.com.css         that host only
  example.com.js                   a script, on that host only
```

| name | the pages it is put on |
| --- | --- |
| `all.css`, `all.js` | every page, a `data:` page and `about:blank` included |
| `example.com.css` | `example.com` and no other host |
| `*.example.com.css` | `example.com`, `www.example.com`, `a.b.example.com` — not `notexample.com` |
| `127.0.0.1.css` | that address |

Names are matched without regard to case. A name that is not a host —
anything but letters, digits, `.` and `-` after an optional `*.` — is
refused, and said: in the shell as blinkterm starts, and on the row after
`alt+r`. Other files (`notes.txt`, an editor's `x.css.swp`), dotfiles and
subdirectories are passed over. Hosts only: a path, a port or an IPv6
address is not a pattern.

When several files fit a page they go on in order: `all` first, then the
`*.` patterns with the fewest labels, then the exact host, and files of the
same rank by name. A later style wins by the cascade and a later script runs
later, so the most specific file has the last word.

**Styles** are adopted as constructed stylesheets at the start of every
document, in the page and in its same-process iframes (a `srcdoc` or
`about:blank` frame goes by its page's host). A page's CSP does not stop
them, and the page's DOM is not changed. Use `!important` to beat a page's
own rules:

```css
/* *.github.com.css: no banner, no sticky header */
.flash-global, .js-notice { display: none !important; }
header.AppHeader { position: static !important; }
```

```css
/* all.css: a larger font for a narrow pane */
html { font-size: 18px !important; }
```

**Scripts** run at the start of every document, before any of the page's
own. By default in an isolated world: the page's DOM is shared, its
JavaScript is not, so the page cannot see the script's variables and the
script cannot call the page's functions. A file whose first line is
`// @world main` runs in the page's own world instead, as the page's code.
At document start the page's elements do not exist yet — even
`document.documentElement` is `null` — so a script that changes the page
waits for them:

```js
// example.com.js
addEventListener('DOMContentLoaded', () => {
  for (const el of document.querySelectorAll('.cookie-banner')) el.remove();
});
```

Each script is registered on its own, so a file with a syntax error stops
only itself; its top-level declarations are its own, as in a userscript
manager.

**`alt+r`** (`reload-sites`) reads the directory again and tells every tab.
The row says what was read, `site files: 2 styles, 1 script`. Styles change
on the page where it stands. Scripts apply from each page's next load — a
script run again on a live page would do its work twice. A tab stopped
behind a dialog, or crashed, keeps the files it had, and the row says how
many did. If `alt+r` is taken by your terminal, bind another key:
`key.f9 = reload-sites` (see [Rebinding keys](configuration.md#rebinding-keys)).

**The directory** is read once as blinkterm starts. `--sites-dir <dir>` (or
`sites-dir = <dir>` in the config file) reads another one, which must
exist; `--no-sites` (or `sites = false`) reads none. The files are read as
they are and never written. A file larger than 1 MiB or not UTF-8 is
refused, and so is a file, or the directory, that group or others can
write: a script runs with the page's powers, and one that another user can
change is a way into every page (see [SECURITY.md](../SECURITY.md)).
`chmod go-w` it.

What a page can see: of a style, `document.adoptedStyleSheets` one entry
longer; of a script in the isolated world, what it does to the DOM; a
`// @world main` script is the page's own code. What is not reached: an
iframe from another site that the engine runs in a process of its own, a
page that assigns `document.adoptedStyleSheets` wholesale, and a page's
inline `style="…!important"`.

Measured against `chrome-headless-shell` 153:

| | |
| --- | --- |
| a host's style, on load, after a navigation, after a reload | applied |
| a script, before the page's first inline script | ran first |
| a `// @world main` script under `script-src 'none'` | ran |
| a style after `alt+r` | in place, on the same document |
| the old script after `alt+r` | never ran again |
| five script files, registered on a new tab | 0.5 to 10 ms |
| 210 KB of CSS, to `domInteractive` | +7 ms on a page it fits, +1 ms on one it does not |
