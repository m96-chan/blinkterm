//! `blinkterm`: a web page in a terminal pane.
//!
//! Everything between `argv` and `app::run` is `blinkterm::options`; this
//! file is the help text and what to do with the answer.

use std::process::ExitCode;

use blinkterm::app;
use blinkterm::doctor;
use blinkterm::engine;
use blinkterm::options::{self, Invocation};

const USAGE: &str = "\
blinkterm, a real browser in a terminal pane

usage: blinkterm [options] [url ...]

options:
  -h, --help       show this message
  -V, --version    show the version
  --profile <dir>  keep cookies, logins and storage in <dir>
                   (default: $XDG_DATA_HOME/blinkterm/profile, or
                   ~/.local/share/blinkterm/profile)
  --temp-profile   a profile that is thrown away when this program exits
  --remote         open the urls as new tabs in the blinkterm already running
                   on this profile, and exit; with none running, start as
                   usual. For other programs to open links in it:
                   export BROWSER='blinkterm --remote'
  --download-dir <dir>  save files a page offers in <dir>
                   (default: $XDG_DOWNLOAD_DIR, the XDG_DOWNLOAD_DIR of
                   ~/.config/user-dirs.dirs, or ~/Downloads)
  --pdf-paper <a4|letter>
                   the paper alt+s prints on (default: letter where the
                   locale is one of the countries that use it, else a4)
  --search-url <url>
                   send what is typed in the url bar and is not a url to
                   <url>, with %s where the words go (off by default: nothing
                   typed leaves the machine unless you say so)
  --scale <n|auto> terminal pixels per CSS pixel before any zoom: 2 on a
                   HiDPI terminal. auto (the default) says 2 when a cell is
                   28 px or taller, else 1
  --color-scheme <auto|light|dark>
                   what a page is told it prefers (prefers-color-scheme).
                   auto (the default) asks the terminal its background
  --force-dark     paint every page dark, even one with no dark style
                   (Chromium's auto dark mode)
  --alpha [<1-100>]
                   let the terminal show through the page: the engine paints
                   nothing behind it and the page's own html/body background
                   is made transparent (text and pictures stay). With a
                   number the whole picture is sent at that opacity, text
                   included; --alpha alone is 100. Locally the page is
                   painted on magenta and the magenta taken back out, so
                   what the page itself shows in magenta goes too. Black
                   text over a dark terminal is yours to read: --force-dark
                   helps. Over ssh and in tmux the number is not applied
  --engine <path>  the Chromium to run (default: $BLINKTERM_ENGINE, else
                   the first of chrome-headless-shell, chromium,
                   chromium-browser, google-chrome, chromium-shell on PATH)
  --engine-arg <--flag>
                   one more argument for the engine; repeatable
                   (--accept-lang=ja, --disable-features=...). Four are
                   refused because they would undo what this program set:
                   --remote-debugging-port, --remote-allow-origins,
                   --remote-debugging-pipe, --user-data-dir
  --user-agent <text>
                   what pages are told the browser is
  --file-picker <command>
                   a program that chooses the file for a page's file input,
                   in a window of its own (zenity --file-selection;
                   osascript -e 'POSIX path of (choose file)'). {dir} is the
                   directory to start in, {out} a file it writes the paths
                   to; without {out}, what it prints is read. Split like a
                   shell splits it, and run without one
  --file-picker-terminal <command>
                   the same, for a program that needs this terminal (yazi
                   --chooser-file={out} {dir}; fzf): blinkterm steps aside
                   while it runs. With both set, this one is used where
                   there is no display
  --file-picker-multiple, --file-picker-terminal-multiple <command>
                   used instead for an input that takes several files
  --password-command <command>
                   a program that prints the login for a site: the password
                   on the first line, `login: <user>` on another (pass show
                   web/{domain}; rbw get --full {host}). {host}, {domain},
                   {url}. Run by alt+l, never by itself
  --password-command-terminal <command>
                   the same, for one that needs this terminal
  --proxy <host:port|scheme://host:port|direct://>
                   send requests through a proxy (loopback never is)
  --mute           start the engine silent (--mute-audio); pages play and
                   cannot tell
  --block-list <path>
                   block requests to the hosts in <path>, and to every host
                   under them (a hosts file, or one host per line); repeatable
  --no-block       block nothing, whatever block-list says
  --home <url>     the page opened when no url is given (default: about:blank)
  --restore        reopen the tabs the last run had
  --normal-mode    start in normal mode (ctrl+., below)
  --config <path>  read settings from <path> instead of
                   $XDG_CONFIG_HOME/blinkterm/config (~/.config/blinkterm/config)
  --no-config      read no settings file
  --print-engine   print which engine would be run, and exit
  --doctor         start the engine, ask the terminal whether it speaks the
                   Kitty graphics and keyboard protocols and which route
                   frames will take, print what was found, and exit; 1 if
                   something is missing
  --tmux <auto|on|off>
                   wrap pictures for tmux's passthrough and draw them as
                   unicode placeholders (at most 297 columns of picture).
                   auto (the default) does when the terminal answers only
                   through tmux
  --frames <auto|raw|png>
                   send frames as raw pixels, or as the engine's PNG as it
                   came. auto (the default) is png over ssh and in tmux
  --fps <n>        at most n frames a second, 1 to 60 (default: 60; 30 in
                   tmux, 15 over ssh)
  --no-probe       start without asking the terminal whether it draws
  --               everything after it is a url

Several urls open one tab each, the first in front.

Settings can be kept in $XDG_CONFIG_HOME/blinkterm/config, one per line as
\"name = value\", where the names are the options above without their --
(scale = 2, color-scheme = dark, engine-arg = --accept-lang=ja, which may
be repeated; a line starting with # is a comment). The command line
overrides the file; $BLINKTERM_ENGINE sits between them. key.<chord> =
<action> rebinds a key, and key.<chord> = none gives it to the page; the
actions are listed after the keys.

The page is rendered by a headless Chromium, which this program starts and
stops. It is looked for in --engine, then $BLINKTERM_ENGINE, then the
settings file, then on PATH as chrome-headless-shell, chromium,
chromium-browser, google-chrome or chromium-shell. blinkterm does not ship
one; install the one you want.

A profile is made readable by you alone (0700), and one blinkterm uses it at a
time: a second one started on the same profile is refused, and told which pid
has it. A running blinkterm takes urls from \"blinkterm --remote <url>\" over
blinkterm.sock in its profile, so it can be $BROWSER. The open tabs are saved
in the profile; --restore reopens them, and after a crash the next start
offers to. Bookmarks are one file for every
profile, $XDG_DATA_HOME/blinkterm/bookmarks, one url<TAB>title per line.

A file a page offers — a link to a PDF, a Content-Disposition: attachment —
is saved in the download directory under its own name, \"report (1).pdf\" if
that name is taken, and the status row says so; the page stays where it was.
Quitting cancels a download that is still coming.

The terminal has to speak the Kitty graphics protocol, the Kitty keyboard
protocol and SGR mouse reporting: a tOS pane, Kitty, WezTerm or Ghostty. It
is asked before the engine is started, and a terminal that does not answer
is told so in the shell. Inside tmux, set allow-passthrough on; keys then
come in tmux's own encoding. Over ssh, frames go as PNG and come as fast as
the link carries them.

keys:
  ctrl+l         type a url; in the url bar, left/right, home/end, ctrl+a/e
                 and alt+b/f move, ctrl+w and alt+d delete a word, ctrl+u/k
                 to either end, up/down walk the pages visited, and tab takes
                 the suggestion
  ctrl+r         reload
  alt+left/right back and forward
  ctrl+t         a new tab, with the cursor in the url bar
  ctrl+w         close this tab; closing the last one quits
  ctrl+shift+t   reopen the last tab closed (alt+t where the terminal or the
                 compositor keeps ctrl+shift+t)
  ctrl+d         bookmark this page, or remove the bookmark
  ctrl+tab       the next tab, ctrl+shift+tab the one before
  alt+1 .. alt+8 the nth tab; alt+9 the last tab
  ctrl+shift+a   the tab list (alt+a too): type to filter, up/down to pick,
                 enter to switch, esc to close
  ctrl+shift+h   the history list (alt+h too): type words to filter, enter
                 opens here, alt+enter or ctrl+enter in a new tab,
                 shift+delete forgets the page
  ctrl+shift+pageup/pagedown
                 move this tab left, right (alt+shift+pageup/pagedown too)
  middle click or ctrl+click on a link
                 open it in a tab behind this one
  alt+= / alt+-  zoom in, out (ctrl+= / ctrl+- where the terminal lets them
                 through); alt+0 / ctrl+0 back to 100%
  ctrl+.         normal mode on or off; in it the letters are keys: f labels
                 what can be clicked and typing a label clicks it (F opens a
                 link in a tab behind), j/k scroll a notch, d/u half a
                 screen, gg/G to the ends, H/L back and forward, r reload,
                 o the url bar, O a new tab, / find, i back to the page
  alt+p          allow this site camera, microphone, location, notifications
                 or clipboard: type the words, enter sets exactly those
  alt+s          save this page as a PDF in the download directory
  alt+shift+s    save the whole page as a picture (PNG) there
  alt+b          stop blocking ads and trackers on this site, or start again
                 (in the url bar alt+b is still a word back)
  esc            leave a page's fullscreen; stop a page that is loading
  ctrl+q         quit
  a dialog       takes the top row: any key, y/n, or type and enter; esc is no
Everything else goes to the page. A link that asks for a new window gets a
new tab, and the tab is switched to.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let invocation = match options::invocation(&args) {
        Ok(invocation) => invocation,
        Err(why) => {
            // A settings file is the person's own text and can hold anything,
            // so the sentence that quotes it is plain text first.
            let why = blinkterm::text::sanitize(&why);
            eprintln!("blinkterm: {why}");
            eprintln!("try 'blinkterm --help'");
            return ExitCode::from(2);
        }
    };
    let options = match invocation {
        Invocation::Help => {
            print!("{USAGE}{}", blinkterm::bindings::help());
            return ExitCode::SUCCESS;
        }
        Invocation::Version => {
            println!("blinkterm {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Invocation::PrintEngine(options) => return exit(doctor::print_engine(&options)),
        Invocation::Doctor(options, provenance) => {
            return exit(doctor::report(&options, &provenance));
        }
        Invocation::Run(options) => options,
    };

    match app::run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            // The terminal has already been put back by the time this runs, so
            // the sentence lands in a shell the person can read it in. A shell
            // is still a terminal, and the sentence can quote the engine, so it
            // is plain text first, whatever made it.
            let message = blinkterm::text::sanitize(&message);
            eprintln!("blinkterm: {message}");
            if message.contains(engine::ENGINE_ENV) || message.contains("PATH") {
                eprintln!(
                    "blinkterm: install a chromium, or set {}",
                    engine::ENGINE_ENV
                );
            }
            ExitCode::FAILURE
        }
    }
}

/// 0 for a yes, 1 for a no.
fn exit(fine: bool) -> ExitCode {
    if fine {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
