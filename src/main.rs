//! `blinkterm`: a web page in a terminal pane.

use std::path::PathBuf;
use std::process::ExitCode;

use blinkterm::app::{self, Options};
use blinkterm::engine;
use blinkterm::profile::Choice;

const USAGE: &str = "\
blinkterm, a real browser in a terminal pane

usage: blinkterm [options] [url]

options:
  -h, --help       show this message
  -V, --version    show the version
  --profile <dir>  keep cookies, logins and storage in <dir>
                   (default: $XDG_DATA_HOME/blinkterm/profile, or
                   ~/.local/share/blinkterm/profile)
  --temp-profile   a profile that is thrown away when this program exits

The page is rendered by a headless Chromium, which this program starts and
stops. It is looked for in $BLINKTERM_ENGINE first, then on PATH as
chrome-headless-shell, chromium, chromium-browser, google-chrome or
chromium-shell. blinkterm does not ship one; install the one you want.

A profile is made readable by you alone (0700), and one blinkterm uses it at a
time: a second one started on the same profile is refused, and told which pid
has it.

The terminal has to speak the Kitty graphics protocol, the Kitty keyboard
protocol and SGR mouse reporting: a tOS pane, Kitty, WezTerm or Ghostty.

keys:
  ctrl+l         type a url
  ctrl+r         reload
  alt+left/right back and forward
  ctrl+t         a new tab, with the cursor in the url bar
  ctrl+w         close this tab; closing the last one quits
  ctrl+tab       the next tab, ctrl+shift+tab the one before
  alt+1 .. alt+9 the nth tab
  ctrl+q         quit
  a dialog       takes the top row: any key, y/n, or type and enter; esc is no
Everything else goes to the page. A link that asks for a new window gets a
new tab, and the tab is switched to.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("blinkterm {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    let options = match parse(&args) {
        Ok(options) => options,
        Err(why) => {
            eprintln!("blinkterm: {why}");
            eprintln!("try 'blinkterm --help'");
            return ExitCode::from(2);
        }
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

/// The command line, less `--help` and `--version`, which have been answered
/// by the time this is called.
///
/// Both spellings of `--profile` are taken — `--profile DIR` and
/// `--profile=DIR` — because both are what people type. `--profile` and
/// `--temp-profile` together is a contradiction rather than a preference, so it
/// is refused instead of one of them quietly winning, and so is either one
/// twice.
fn parse(args: &[String]) -> Result<Options, String> {
    let mut url = None;
    let mut profile = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let chosen = if arg == "--temp-profile" {
            Some(Choice::Temporary)
        } else if arg == "--profile" {
            let dir = args
                .next()
                .ok_or("--profile needs a directory: --profile <dir>")?;
            Some(Choice::At(PathBuf::from(dir)))
        } else if let Some(dir) = arg.strip_prefix("--profile=") {
            if dir.is_empty() {
                return Err("--profile needs a directory: --profile <dir>".to_string());
            }
            Some(Choice::At(PathBuf::from(dir)))
        } else {
            None
        };
        if let Some(chosen) = chosen {
            if profile.replace(chosen).is_some() {
                return Err("one profile at a time".to_string());
            }
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            return Err(format!("unknown option: {arg}"));
        }
        if url.replace(arg.clone()).is_some() {
            return Err("one page at a time".to_string());
        }
    }
    Ok(Options {
        url: url.unwrap_or_else(|| "about:blank".to_string()),
        profile: profile.unwrap_or(Choice::Default),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Options, String> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        parse(&args)
    }

    #[test]
    fn with_no_flags_the_profile_is_the_default_one() {
        let options = parsed(&[]).expect("nothing is fine");
        assert_eq!(options.profile, Choice::Default);
        assert_eq!(options.url, "about:blank");
    }

    #[test]
    fn a_profile_can_be_named_either_way_round_the_equals_sign() {
        for args in [
            &["--profile", "x", "example.com"][..],
            &["--profile=x", "example.com"][..],
            &["example.com", "--profile", "x"][..],
        ] {
            let options = parsed(args).expect("a profile");
            assert_eq!(options.profile, Choice::At(PathBuf::from("x")), "{args:?}");
            assert_eq!(options.url, "example.com", "{args:?}");
        }
    }

    #[test]
    fn a_temporary_profile_is_asked_for_by_name() {
        let options = parsed(&["--temp-profile"]).expect("a temporary profile");
        assert_eq!(options.profile, Choice::Temporary);
    }

    #[test]
    fn two_profiles_are_one_too_many() {
        for args in [
            &["--profile", "x", "--temp-profile"][..],
            &["--temp-profile", "--profile=x"][..],
            &["--profile=x", "--profile=y"][..],
        ] {
            let why = parsed(args).err().expect("refused");
            assert_eq!(why, "one profile at a time", "{args:?}");
        }
    }

    #[test]
    fn a_profile_with_no_directory_is_an_error_that_names_the_option() {
        for args in [&["--profile"][..], &["--profile="][..]] {
            let why = parsed(args).err().expect("refused");
            assert!(why.contains("--profile"), "{args:?}: {why}");
        }
    }

    #[test]
    fn one_page_at_a_time_and_no_options_that_do_not_exist() {
        let why = parsed(&["a.com", "b.com"]).err().expect("refused");
        assert_eq!(why, "one page at a time");
        let why = parsed(&["--incognito"]).err().expect("refused");
        assert!(why.contains("--incognito"), "{why}");
    }
}
