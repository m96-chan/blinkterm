//! `blinkterm`: a web page in a terminal pane.

use std::path::PathBuf;
use std::process::ExitCode;

use blinkterm::app::{self, Options};
use blinkterm::appearance;
use blinkterm::download;
use blinkterm::engine;
use blinkterm::profile::Choice;
use blinkterm::zoom::Scale;

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

The page is rendered by a headless Chromium, which this program starts and
stops. It is looked for in $BLINKTERM_ENGINE first, then on PATH as
chrome-headless-shell, chromium, chromium-browser, google-chrome or
chromium-shell. blinkterm does not ship one; install the one you want.

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
/// twice. `--download-dir` is taken the same two ways and refused twice for
/// the same reason.
///
/// `--search-url` is taken the same two ways, and refused without a `%s`,
/// which is where the words go: a search url without one would send every
/// search to the same page, and the person would find that out by searching.
///
/// `--scale` and `--color-scheme` are taken the same two ways and refused
/// twice for the same reason, each with its own parser's sentence for a value
/// it does not know. `--force-dark` is a flag with nothing after it, and is
/// refused twice as everything else is: a command line that says a thing
/// twice was put together by something that meant two different things.
fn parse(args: &[String]) -> Result<Options, String> {
    let mut url = None;
    let mut profile = None;
    let mut downloads = None;
    let mut search_url = None;
    let mut scale = None;
    let mut scheme = None;
    let mut force_dark = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let dir = if arg == "--download-dir" {
            Some(
                args.next()
                    .filter(|dir| !dir.is_empty())
                    .ok_or(DOWNLOAD_DIR_NEEDED)?
                    .as_str(),
            )
        } else if let Some(dir) = arg.strip_prefix("--download-dir=") {
            if dir.is_empty() {
                return Err(DOWNLOAD_DIR_NEEDED.to_string());
            }
            Some(dir)
        } else {
            None
        };
        if let Some(dir) = dir {
            if downloads.replace(PathBuf::from(dir)).is_some() {
                return Err("one download directory at a time".to_string());
            }
            continue;
        }
        let search = if arg == "--search-url" {
            Some(args.next().map(String::as_str).unwrap_or_default())
        } else {
            arg.strip_prefix("--search-url=")
        };
        if let Some(search) = search {
            if search.is_empty() {
                return Err("--search-url needs a url: --search-url <url with %s>".to_string());
            }
            if !search.contains("%s") {
                return Err("--search-url needs a %s where the words go".to_string());
            }
            if search_url.replace(search.to_string()).is_some() {
                return Err("one search url at a time".to_string());
            }
            continue;
        }
        if let Some(text) = value_of(arg, "--scale", &mut args) {
            if scale.replace(Scale::parse(text)?).is_some() {
                return Err("one scale at a time".to_string());
            }
            continue;
        }
        if let Some(text) = value_of(arg, "--color-scheme", &mut args) {
            if scheme.replace(appearance::Choice::parse(text)?).is_some() {
                return Err("one colour scheme at a time".to_string());
            }
            continue;
        }
        if arg == "--force-dark" {
            if force_dark {
                return Err("--force-dark once is enough".to_string());
            }
            force_dark = true;
            continue;
        }
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
        download: downloads.map_or(download::Choice::Default, download::Choice::At),
        search_url,
        scale: scale.unwrap_or(Scale::Auto),
        scheme: scheme.unwrap_or_default(),
        force_dark,
    })
}

/// The value of `--name value` or `--name=value`, when `arg` is that option:
/// what came after it, or an empty string when nothing did, for the option's
/// own parser to refuse by name.
fn value_of<'a>(
    arg: &'a str,
    name: &str,
    rest: &mut std::slice::Iter<'a, String>,
) -> Option<&'a str> {
    if arg == name {
        return Some(rest.next().map(String::as_str).unwrap_or_default());
    }
    arg.strip_prefix(name)?.strip_prefix('=')
}

const DOWNLOAD_DIR_NEEDED: &str = "--download-dir needs a directory: --download-dir <dir>";

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
        assert_eq!(options.download, download::Choice::Default);
        assert_eq!(options.url, "about:blank");
        assert_eq!(options.search_url, None, "nothing typed is searched");
    }

    #[test]
    fn a_search_url_can_be_named_either_way_round_the_equals_sign() {
        let search = "https://duckduckgo.com/?q=%s";
        for args in [
            &["--search-url", search, "example.com"][..],
            &["--search-url=https://duckduckgo.com/?q=%s", "example.com"][..],
            &["example.com", "--search-url", search][..],
        ] {
            let options = parsed(args).expect("a search url");
            assert_eq!(options.search_url.as_deref(), Some(search), "{args:?}");
            assert_eq!(options.url, "example.com", "{args:?}");
        }
    }

    #[test]
    fn a_search_url_needs_a_percent_s() {
        let why = parsed(&["--search-url", "https://duckduckgo.com/"])
            .err()
            .expect("refused");
        assert!(why.contains("%s"), "{why}");
        for args in [&["--search-url"][..], &["--search-url="][..]] {
            let why = parsed(args).err().expect("refused");
            assert!(why.contains("--search-url"), "{args:?}: {why}");
        }
    }

    #[test]
    fn two_search_urls_are_one_too_many() {
        let why = parsed(&["--search-url=a%s", "--search-url", "b%s"])
            .err()
            .expect("refused");
        assert_eq!(why, "one search url at a time");
    }

    #[test]
    fn with_no_flags_the_scale_and_the_scheme_are_auto() {
        let options = parsed(&[]).expect("nothing is fine");
        assert_eq!(options.scale, Scale::Auto);
        assert_eq!(options.scheme, appearance::Choice::Auto);
        assert!(!options.force_dark, "nothing is painted dark unasked");
    }

    #[test]
    fn a_scale_can_be_named_either_way_round_the_equals_sign() {
        for args in [
            &["--scale", "2", "example.com"][..],
            &["--scale=2", "example.com"][..],
            &["example.com", "--scale", "2"][..],
        ] {
            let options = parsed(args).expect("a scale");
            assert_eq!(options.scale, Scale::Fixed(2.0), "{args:?}");
            assert_eq!(options.url, "example.com", "{args:?}");
        }
        let options = parsed(&["--scale=auto"]).expect("auto");
        assert_eq!(options.scale, Scale::Auto);
    }

    #[test]
    fn a_scale_that_is_not_a_number_in_range_is_an_error_that_names_the_option() {
        for args in [
            &["--scale"][..],
            &["--scale="][..],
            &["--scale", "0"][..],
            &["--scale=5"][..],
            &["--scale", "big"][..],
        ] {
            let why = parsed(args).err().expect("refused");
            assert!(why.contains("--scale"), "{args:?}: {why}");
        }
    }

    #[test]
    fn two_scales_are_one_too_many() {
        let why = parsed(&["--scale=2", "--scale", "auto"])
            .err()
            .expect("refused");
        assert_eq!(why, "one scale at a time");
    }

    #[test]
    fn a_colour_scheme_is_one_of_three_words() {
        for (word, choice) in [
            ("auto", appearance::Choice::Auto),
            ("light", appearance::Choice::Light),
            ("dark", appearance::Choice::Dark),
        ] {
            let options = parsed(&["--color-scheme", word]).expect("a scheme");
            assert_eq!(options.scheme, choice, "{word}");
            let options = parsed(&[&format!("--color-scheme={word}")]).expect("a scheme");
            assert_eq!(options.scheme, choice, "{word}");
        }
        for args in [&["--color-scheme"][..], &["--color-scheme=black"][..]] {
            let why = parsed(args).err().expect("refused");
            assert!(why.contains("--color-scheme"), "{args:?}: {why}");
        }
        let why = parsed(&["--color-scheme=dark", "--color-scheme=light"])
            .err()
            .expect("refused");
        assert_eq!(why, "one colour scheme at a time");
    }

    #[test]
    fn force_dark_is_a_flag_with_nothing_after_it() {
        let options = parsed(&["--force-dark", "example.com"]).expect("a flag");
        assert!(options.force_dark);
        assert_eq!(options.url, "example.com", "what follows is the page");
        let why = parsed(&["--force-dark=yes"]).err().expect("refused");
        assert!(why.contains("--force-dark=yes"), "{why}");
        let why = parsed(&["--force-dark", "--force-dark"])
            .err()
            .expect("refused");
        assert_eq!(why, "--force-dark once is enough");
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
    fn a_download_directory_can_be_named_either_way_round_the_equals_sign() {
        for args in [
            &["--download-dir", "x", "example.com"][..],
            &["--download-dir=x", "example.com"][..],
            &["example.com", "--download-dir", "x", "--temp-profile"][..],
        ] {
            let options = parsed(args).expect("a download directory");
            assert_eq!(
                options.download,
                download::Choice::At(PathBuf::from("x")),
                "{args:?}"
            );
            assert_eq!(options.url, "example.com", "{args:?}");
        }
    }

    #[test]
    fn two_download_directories_are_one_too_many() {
        for args in [
            &["--download-dir", "x", "--download-dir", "y"][..],
            &["--download-dir=x", "--download-dir=x"][..],
        ] {
            let why = parsed(args).err().expect("refused");
            assert_eq!(why, "one download directory at a time", "{args:?}");
        }
    }

    #[test]
    fn a_download_directory_with_no_directory_is_an_error_that_names_the_option() {
        for args in [
            &["--download-dir"][..],
            &["--download-dir="][..],
            &["--download-dir", ""][..],
        ] {
            let why = parsed(args).err().expect("refused");
            assert!(why.contains("--download-dir"), "{args:?}: {why}");
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
