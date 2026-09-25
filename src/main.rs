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
  --download-dir <dir>  save files a page offers in <dir>
                   (default: $XDG_DOWNLOAD_DIR, the XDG_DOWNLOAD_DIR of
                   ~/.config/user-dirs.dirs, or ~/Downloads)
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
  --proxy <host:port|scheme://host:port|direct://>
                   send requests through a proxy (loopback never is)
  --home <url>     the page opened when no url is given (default: about:blank)
  --restore        reopen the tabs the last run had (not yet: #18)
  --normal-mode    start in keyboard navigation mode (not yet: #13)
  --config <path>  read settings from <path> instead of
                   $XDG_CONFIG_HOME/blinkterm/config (~/.config/blinkterm/config)
  --no-config      read no settings file
  --print-engine   print which engine would be run, and exit
  --doctor         start the engine, ask the terminal whether it speaks the
                   Kitty graphics and keyboard protocols, print what was
                   found, and exit; 1 if something is missing
  --               everything after it is a url

Several urls open one tab each, the first in front.

Settings can be kept in $XDG_CONFIG_HOME/blinkterm/config, one per line as
\"name = value\", where the names are the options above without their --
(scale = 2, color-scheme = dark, engine-arg = --accept-lang=ja, which may
be repeated; a line starting with # is a comment). The command line
overrides the file; $BLINKTERM_ENGINE sits between them.

The page is rendered by a headless Chromium, which this program starts and
stops. It is looked for in --engine, then $BLINKTERM_ENGINE, then the
settings file, then on PATH as chrome-headless-shell, chromium,
chromium-browser, google-chrome or chromium-shell. blinkterm does not ship
one; install the one you want.

A profile is made readable by you alone (0700), and one blinkterm uses it at a
time: a second one started on the same profile is refused, and told which pid
has it.

A file a page offers — a link to a PDF, a Content-Disposition: attachment —
is saved in the download directory under its own name, \"report (1).pdf\" if
that name is taken, and the status row says so; the page stays where it was.
Quitting cancels a download that is still coming.

The terminal has to speak the Kitty graphics protocol, the Kitty keyboard
protocol and SGR mouse reporting: a tOS pane, Kitty, WezTerm or Ghostty.

keys:
  ctrl+l         type a url; in the url bar, left/right, home/end, ctrl+a/e
                 and alt+b/f move, ctrl+w and alt+d delete a word, ctrl+u/k
                 to either end, up/down walk the pages visited, and tab takes
                 the suggestion
  ctrl+r         reload
  alt+left/right back and forward
  ctrl+t         a new tab, with the cursor in the url bar
  ctrl+w         close this tab; closing the last one quits
  ctrl+tab       the next tab, ctrl+shift+tab the one before
  alt+1 .. alt+9 the nth tab
  alt+= / alt+-  zoom in, out (ctrl+= / ctrl+- where the terminal lets them
                 through); alt+0 / ctrl+0 back to 100%
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
            print!("{USAGE}");
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
