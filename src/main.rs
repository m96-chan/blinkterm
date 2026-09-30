//! `blinkterm`: a web page in a terminal pane.
//!
//! Everything between `argv` and `app::run` is `blinkterm::options`; this
//! file is the help text and what to do with the answer.

use std::process::ExitCode;

use blinkterm::app;
use blinkterm::bindings::{self, Keymap};
use blinkterm::doctor;
use blinkterm::engine;
use blinkterm::options::{self, Invocation};

/// `--help` up to its keys: the same whatever the keymap.
const USAGE_HEAD: &str = "\
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
                   the paper save-pdf (alt+s; cmd+s on a Mac) prints on
                   (default: letter where the locale is one of the
                   countries that use it, else a4)
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
                   {url}. Run by fill-login (alt+l; cmd+shift+l on a Mac),
                   never by itself
  --password-command-terminal <command>
                   the same, for one that needs this terminal
  --external-browser <command>
                   what alt+o opens this page with, {url} where the url goes
                   (default: open on a Mac, else $BROWSER unless it is
                   blinkterm, else xdg-open; nothing without a display)
  --proxy <host:port|scheme://host:port|direct://>
                   send requests through a proxy (loopback never is)
  --mute           start the engine silent (--mute-audio); pages play and
                   cannot tell
  --block-list <path>
                   block requests to the hosts in <path>, and to every host
                   under them (a hosts file, or one host per line); repeatable
  --no-block       block nothing, whatever block-list says
  --sites-dir <dir>
                   read site styles and scripts from <dir>: <host>.css and
                   <host>.js, *.<host> for a site and everything under it,
                   all.css for every page (default:
                   $XDG_CONFIG_HOME/blinkterm/sites, ~/.config/blinkterm/sites)
  --no-sites       read no site styles or scripts
  --no-console     do not listen to the page's console (ctrl+shift+j)
  --home <url>     the page opened when no url is given (default: about:blank)
  --restore        reopen the tabs the last run had
  --normal-mode    start in normal mode (ctrl+., below)
  --keymap <mac|linux>
                   the built-in keys: mac is cmd where Kitty leaves it free and
                   ctrl elsewhere (default on macOS); linux is ctrl and alt
                   (default elsewhere). key.<chord> lines apply on top
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

";

/// The keys of `keymap = linux`: `app::command`.
const KEYS_LINUX: &str = "\
keys (keymap linux; --keymap mac or keymap = mac for the other):
  ctrl+l         type a url; in the url bar, left/right, home/end, ctrl+a/e
                 and alt+b/f move, ctrl+w and alt+d delete a word, ctrl+u/k
                 to either end, up/down walk the pages visited, and tab takes
                 the suggestion
  ctrl+f         find in the page: enter or down the next match, shift+enter
                 or up the one before, esc closes
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
  ctrl+shift+j   the page's console (alt+j too, where the terminal keeps
                 ctrl+shift+j, as Kitty does): what it logged, threw and
                 failed to fetch, newest last; type words to filter, esc
                 closes; the row counts its errors until you look
  ctrl+shift+pageup/pagedown
                 move this tab left, right (alt+shift+pageup/pagedown too)
  middle click or ctrl+click on a link
                 open it in a tab behind this one
  a click on the row
                 a tab switches to it, a middle click closes it, +N the
                 nearest tab past that end, the url the url bar
  alt+= / alt+-  zoom in, out (ctrl+= / ctrl+- where the terminal lets them
                 through); alt+0 / ctrl+0 back to 100%
  ctrl+.         normal mode on or off; in it the letters are keys: f labels
                 what can be clicked and typing a label clicks it (F opens a
                 link in a tab behind), j/k scroll a notch, d/u half a
                 screen, gg/G to the ends, H/L back and forward, r reload,
                 o the url bar, O a new tab, / find, i back to the page
  alt+c          copy the selection, or the line being typed; alt+u copies
                 the url
  alt+p          allow this site camera, microphone, location, notifications
                 or clipboard: type the words, enter sets exactly those
  alt+l          fill the login form from --password-command
  alt+s          save this page as a PDF in the download directory
  alt+shift+s    save the whole page as a picture (PNG) there
  alt+b          stop blocking ads and trackers on this site, or start again
                 (in the url bar alt+b is still a word back)
  alt+o          open this page in the desktop browser; cookies and logins
                 stay here
  alt+r          reader mode: the article alone, without the page around it;
                 again to put the page back
  alt+shift+r    read the site styles and scripts again
  esc            leave a page's fullscreen; stop a page that is loading
  ctrl+q         quit
  a dialog       takes the top row: any key, y/n, or type and enter; esc is no
";

/// The keys of `keymap = mac`: [`bindings::Keymap::Mac`]'s rows. Option
/// composes on a Mac, so the url bar's word keys are the arrows there.
const KEYS_MAC: &str = "\
keys (keymap mac; --keymap linux or keymap = linux for the other):
  ctrl+l         type a url; in the url bar, left/right, home/end, ctrl+a/e
                 and alt+left/right move, ctrl+w and alt+backspace delete a
                 word, ctrl+u/k to either end, up/down walk the pages
                 visited, and tab takes the suggestion
  ctrl+f         find in the page: enter or down the next match, shift+enter
                 or up the one before, esc closes
  ctrl+r         reload
  cmd+[ / cmd+]  back and forward
  ctrl+t         a new tab, with the cursor in the url bar
  ctrl+w         close this tab; closing the last one quits
  cmd+shift+t    reopen the last tab closed
  cmd+d          bookmark this page, or remove the bookmark
  cmd+alt+right  the next tab, cmd+alt+left the one before (ctrl+pagedown,
                 ctrl+pageup too)
  ctrl+1 .. ctrl+8 the nth tab; ctrl+9 the last tab
  cmd+shift+a    the tab list: type to filter, up/down to pick, enter to
                 switch, esc to close
  cmd+y          the history list: type words to filter, enter opens here,
                 alt+enter or ctrl+enter in a new tab, shift+delete forgets
                 the page
  cmd+alt+j      the page's console: what it logged, threw and failed to
                 fetch, newest last; type words to filter, esc closes; the
                 row counts its errors until you look
  cmd+shift+pageup/pagedown
                 move this tab left, right
  middle click or ctrl+click on a link
                 open it in a tab behind this one
  a click on the row
                 a tab switches to it, a middle click closes it, +N the
                 nearest tab past that end, the url the url bar
  ctrl+= / ctrl+- zoom in, out; ctrl+0 back to 100%
  ctrl+.         normal mode on or off; in it the letters are keys: f labels
                 what can be clicked and typing a label clicks it (F opens a
                 link in a tab behind), j/k scroll a notch, d/u half a
                 screen, gg/G to the ends, H/L back and forward, r reload,
                 o the url bar, O a new tab, / find, i back to the page
  cmd+c          copy the selection, or the line being typed (cmd+shift+c
                 too); cmd+u copies the url
  cmd+p          allow this site camera, microphone, location, notifications
                 or clipboard: type the words, enter sets exactly those
  cmd+shift+l    fill the login form from --password-command
  cmd+s          save this page as a PDF in the download directory
  cmd+shift+s    save the whole page as a picture (PNG) there
  cmd+b          stop blocking ads and trackers on this site, or start again
  cmd+shift+o    open this page in the desktop browser; cookies and logins
                 stay here
  cmd+shift+r    reader mode: the article alone, without the page around it;
                 again to put the page back
  cmd+alt+shift+r
                 read the site styles and scripts again
  esc            leave a page's fullscreen; stop a page that is loading
  ctrl+q         quit
  a dialog       takes the top row: any key, y/n, or type and enter; esc is no
";

/// After the keys, whichever they are.
const USAGE_TAIL: &str = "\
Everything else goes to the page. A link that asks for a new window gets a
new tab, and the tab is switched to.
";

/// The `keys:` block of `keymap`.
fn keys_block(keymap: Keymap) -> &'static str {
    match keymap {
        Keymap::Linux => KEYS_LINUX,
        Keymap::Mac => KEYS_MAC,
    }
}

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
        Invocation::Help(keymap) => {
            // The file is not read for --help, so this is --keymap's, else
            // the platform's: not necessarily the one a run would have.
            print!(
                "{USAGE_HEAD}{}{USAGE_TAIL}{}",
                keys_block(keymap),
                bindings::help(keymap)
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `block` names `chord`: as written, or as the second half of a
    /// pair on one line — `alt+left/right` names `alt+right`.
    fn names(block: &str, chord: &str) -> bool {
        if block.contains(chord) {
            return true;
        }
        let Some((held, key)) = chord.rsplit_once('+') else {
            return false;
        };
        block
            .lines()
            .any(|line| line.contains(&format!("{held}+")) && line.contains(&format!("/{key}")))
    }

    #[test]
    fn the_keys_block_of_each_keymap_names_the_first_chord_of_every_action() {
        for keymap in [Keymap::Linux, Keymap::Mac] {
            let block = keys_block(keymap);
            assert!(
                block.starts_with(&format!("keys (keymap {}", keymap.name())),
                "{block}"
            );
            for row in &bindings::ACTIONS {
                let first = keymap.column(row).split(',').next().unwrap_or_default();
                let first = first.split(" .. ").next().unwrap_or(first).trim();
                assert!(
                    names(block, first),
                    "{} for {} in the {} keys",
                    first,
                    row.name,
                    keymap.name()
                );
            }
        }
    }

    #[test]
    fn the_options_list_names_keymap() {
        assert!(USAGE_HEAD.contains("  --keymap <mac|linux>\n"));
        assert!(USAGE_TAIL.starts_with("Everything else goes to the page."));
    }
}
