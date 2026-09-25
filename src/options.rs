//! Where the settings come from — the command line, `$BLINKTERM_ENGINE`, the
//! config file — and the one struct they become.
//!
//! # Three sources of one shape, and one fold
//!
//! The command line is parsed into a [`Settings`], every field of which is an
//! `Option`; so is the config file, and so is the one environment variable.
//! [`resolve`] folds the three with [`Settings::over`] and fills in what none
//! of them said, which is what keeps "the command line overrides the file" a
//! fact of one function rather than a property of forty `if`s. Both parsers
//! call the same value parsers — [`Scale::parse`],
//! [`appearance::Choice::parse`], and the ones here — so a bad value is the
//! same sentence wherever it was written.
//!
//! Per setting, highest first: the command line, because it was typed just
//! now; `$BLINKTERM_ENGINE`, for `engine` only; the file, written once; the
//! built-in default. The variable sits *above* the file on purpose: the
//! README, the Homebrew caveat and the engine tests all say
//! `BLINKTERM_ENGINE=… blinkterm` is how an engine is named for one run, and
//! an `engine =` line written once must not make that stop working for the
//! person who wrote it and forgot. Lists — `engine-arg` — accumulate instead,
//! the file's first, so that the command line's come later and win where the
//! engine takes the last of a flag given twice.
//!
//! # The file is a line, not a language
//!
//! `$XDG_CONFIG_HOME/blinkterm/config`, else `~/.config/blinkterm/config`;
//! `key = value`, one per line. No sections, no quoting, no escapes, no
//! inline comments, no continuation. A `#` or `;` first on a line makes it a
//! comment; a blank line is nothing; anything else must have an `=`, and the
//! value is everything after the first one, trimmed at both ends — a search
//! url has `%s`, `?` and `#` in it, a user agent has spaces and parentheses,
//! a proxy has `://`, and nothing here needs a leading space. The keys are
//! the option names without their `--`, so `--help` documents the file too.
//!
//! TOML was the alternative, and it is a dependency or a second parser the
//! size of `json.rs` for a file of ten lines. `key value` without the `=`
//! would need quoting for a value with spaces, then escapes, which is the
//! language this avoids.
//!
//! Only the first error is reported, with `path:line`, where the line is the
//! number an editor shows — comments and blanks counted. A missing file is
//! the normal state and says nothing, unless it was named with `--config`,
//! in which case the person wants to know it was not there.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::appearance;
use crate::bindings::{Binding, Bindings};
use crate::download;
use crate::engine;
use crate::profile;
use crate::zoom::Scale;

/// The largest settings file that is read. Nothing here needs a tenth of
/// it; a binary named by mistake with `--config` should say so rather than
/// produce a hundred line errors.
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;

/// What [`parse_config`] says about every `key.` line that is understood.
///
/// The bindings are parsed and checked (see [`crate::bindings`]) but not yet
/// looked up by the loop, and a file that was read and quietly ignored would
/// be worse than one that says so.
pub const NOT_REMAPPABLE_YET: &str = "key bindings are not remappable yet (#17)";

/// Everything `app::run` is told. Every field is concrete: the folding of the
/// three sources has already happened.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// The pages to open, one tab each, the first in front. Empty means
    /// `home`. Not yet normalised: `app::drive` does that, as it did.
    pub urls: Vec<String>,
    /// `home`: the page a run with no url opens. `about:blank` unless said.
    pub home: String,
    /// Where cookies, logins and site data are kept; see [`crate::profile`].
    pub profile: profile::Choice,
    /// Where a file a page offers is saved; see [`crate::download`].
    pub download: download::Choice,
    /// `--search-url`: where words typed into the url bar are sent, with
    /// `%s` where they go; or none, in which case what is typed is always a
    /// url. See [`crate::app::destination`].
    pub search_url: Option<String>,
    /// `--scale`: terminal pixels per CSS pixel before any zoom, or `Auto`
    /// to guess it from the cell. See [`crate::zoom::Scale`].
    pub scale: Scale,
    /// `--color-scheme`: what a page is told it prefers, or `Auto` for what
    /// the terminal's background says. See [`crate::appearance`].
    pub scheme: appearance::Choice,
    /// `--force-dark`: every page painted dark, dark style or none.
    pub force_dark: bool,
    /// How the engine is started; see [`crate::engine::Launch`].
    pub engine: engine::Launch,
    /// `--restore`: reopen the last session's tabs at start. See
    /// [`crate::session`].
    pub restore: bool,
    /// `--normal-mode`: start in normal mode rather than insert: the letters
    /// are the program's keys from the first one. See [`crate::normal`].
    pub normal_mode: bool,
    /// `key.<chord> = <action>` lines from the file; see [`crate::bindings`].
    /// Always empty until the loop looks them up: see [`NOT_REMAPPABLE_YET`].
    pub bindings: Bindings,
}

/// What `main` was asked to do, once the command line has been read.
#[derive(Debug, Clone, PartialEq)]
pub enum Invocation {
    Help,
    Version,
    /// `--print-engine`: name the engine the search finds, and stop.
    PrintEngine(Options),
    /// `--doctor`: start the engine and ask the terminal, and stop.
    Doctor(Options, Provenance),
    Run(Options),
}

/// Where the settings came from, for `--doctor` to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// The file that was read or looked for; `None` with `--no-config`, or
    /// with no `$XDG_CONFIG_HOME` and no `$HOME` to put one under.
    pub config: Option<PathBuf>,
    /// Whether it was there.
    pub found: bool,
    /// How many settings it held: lines that are neither blank nor comments.
    pub settings: usize,
    /// Which source named the engine: `--engine`, `$BLINKTERM_ENGINE`,
    /// `config`, or `None` for the `PATH` search.
    pub engine_from: Option<&'static str>,
}

/// `--help`, `--version`, `--print-engine` or `--doctor`: the command-line
/// switches that are not settings but say what this run is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    Help,
    Version,
    PrintEngine,
    Doctor,
}

/// `--config <path>` or `--no-config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigChoice {
    At(PathBuf),
    None,
}

/// One source's worth of settings: every field optional, so that three of
/// them can be folded with [`Settings::over`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    /// Command line only: a page is what a run is for.
    pub urls: Vec<String>,
    pub home: Option<String>,
    /// `profile = <dir>` is `At`, `temp-profile = true` is `Temporary`.
    pub profile: Option<profile::Choice>,
    pub download_dir: Option<PathBuf>,
    pub search_url: Option<String>,
    pub scale: Option<Scale>,
    pub scheme: Option<appearance::Choice>,
    pub force_dark: Option<bool>,
    pub engine: Option<PathBuf>,
    /// Appended across sources, never replaced.
    pub engine_args: Vec<String>,
    pub user_agent: Option<String>,
    pub proxy: Option<String>,
    pub restore: Option<bool>,
    pub normal_mode: Option<bool>,
    /// File only: a binding is not a one-run thing.
    pub bindings: Vec<Binding>,
    /// Command line only.
    pub config: Option<ConfigChoice>,
    /// Command line only.
    pub what: Option<What>,
}

impl Settings {
    /// `self` where it says something, else `under`. Lists concatenate,
    /// `under`'s first: a file's `engine-arg`s come before the command
    /// line's, so that the command line's win where the engine takes the
    /// last.
    pub fn over(self, under: Settings) -> Settings {
        let mut engine_args = under.engine_args;
        engine_args.extend(self.engine_args);
        let mut bindings = under.bindings;
        bindings.extend(self.bindings);
        let mut urls = under.urls;
        urls.extend(self.urls);
        Settings {
            urls,
            home: self.home.or(under.home),
            profile: self.profile.or(under.profile),
            download_dir: self.download_dir.or(under.download_dir),
            search_url: self.search_url.or(under.search_url),
            scale: self.scale.or(under.scale),
            scheme: self.scheme.or(under.scheme),
            force_dark: self.force_dark.or(under.force_dark),
            engine: self.engine.or(under.engine),
            engine_args,
            user_agent: self.user_agent.or(under.user_agent),
            proxy: self.proxy.or(under.proxy),
            restore: self.restore.or(under.restore),
            normal_mode: self.normal_mode.or(under.normal_mode),
            bindings,
            config: self.config.or(under.config),
            what: self.what.or(under.what),
        }
    }
}

/// `--search-url`'s value, from either source: it needs a `%s`, which is
/// where the words go. A search url without one would send every search to
/// the same page, and the person would find that out by searching.
fn parse_search_url(name: &str, text: &str) -> Result<String, String> {
    if !text.contains("%s") {
        return Err(format!("{name} needs a %s where the words go"));
    }
    Ok(text.to_string())
}

/// One `--engine-arg`, from either source: a flag, and not one of
/// [`engine::RESERVED_ARGS`]. A word that is not a flag would be a url the
/// engine opened as a page nobody attaches to, and one the first-page search
/// could pick.
fn parse_engine_arg(name: &str, text: &str) -> Result<String, String> {
    if !text.starts_with('-') {
        return Err(format!("{name} needs a flag starting with -, not {text:?}"));
    }
    if let Some(why) = engine::reserved(text) {
        return Err(format!("{text} is refused as an engine argument: {why}"));
    }
    Ok(text.to_string())
}

/// `--proxy`'s value: Chromium's `--proxy-server`, which is `host:port`,
/// `scheme://host:port` or `direct://`. Not checked further than having no
/// whitespace: a proxy that does not answer is a page that says "the proxy
/// would not connect", which is the right place to hear it.
fn parse_proxy(name: &str, text: &str) -> Result<String, String> {
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return Err(format!(
            "{name} needs host:port, scheme://host:port or direct://, not {text:?}"
        ));
    }
    Ok(text.to_string())
}

/// A file's boolean: `true` or `false`, and nothing else, because `yes`,
/// `on` and `1` are each a guess about what somebody else's format meant.
fn parse_bool(name: &str, text: &str) -> Result<bool, String> {
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("{name} is true or false, not {text:?}")),
    }
}

/// The value of `--name value` or `--name=value`, when `arg` is that option:
/// what came after it, or an empty string when nothing did, for the caller
/// to refuse by name.
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

/// Put `value` in `slot`, or say `twice` if something already was.
fn once<T>(slot: &mut Option<T>, value: T, twice: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(twice.to_string());
    }
    Ok(())
}

/// A value that must not be empty, or `needed`.
fn needed<'a>(value: &'a str, needed: &str) -> Result<&'a str, String> {
    if value.is_empty() {
        return Err(needed.to_string());
    }
    Ok(value)
}

/// The command line, less `argv[0]`.
///
/// `-h`/`--help` and `-V`/`--version` anywhere before `--` win over
/// everything else on the line, errors included, as they always have: a
/// person asking for help with a broken command line is the person who needs
/// it.
///
/// Every value option is taken as `--name value` and `--name=value`, because
/// both are what people type, and refused twice: a command line that says a
/// thing twice was put together by something that meant two different
/// things. `--profile` and `--temp-profile` together is the same refusal,
/// since they are one setting with three values. The flags — `--force-dark`,
/// `--temp-profile`, `--restore`, `--normal-mode` and the rest — take
/// nothing after them, and `--force-dark=yes` is an unknown option.
///
/// Every word that is not an option is a url, one tab each; `--` ends the
/// options, so a url that starts with `-` can still be given.
pub fn parse_args(args: &[String]) -> Result<Settings, String> {
    let options = args.iter().take_while(|arg| *arg != "--");
    for arg in options {
        if arg == "-h" || arg == "--help" {
            return Ok(Settings {
                what: Some(What::Help),
                ..Settings::default()
            });
        }
    }
    let options = args.iter().take_while(|arg| *arg != "--");
    for arg in options {
        if arg == "-V" || arg == "--version" {
            return Ok(Settings {
                what: Some(What::Version),
                ..Settings::default()
            });
        }
    }

    let mut s = Settings::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            s.urls.extend(args.by_ref().cloned());
            break;
        }
        if let Some(dir) = value_of(arg, "--download-dir", &mut args) {
            let dir = needed(
                dir,
                "--download-dir needs a directory: --download-dir <dir>",
            )?;
            once(
                &mut s.download_dir,
                PathBuf::from(dir),
                "one download directory at a time",
            )?;
            continue;
        }
        if let Some(search) = value_of(arg, "--search-url", &mut args) {
            let search = needed(
                search,
                "--search-url needs a url: --search-url <url with %s>",
            )?;
            let search = parse_search_url("--search-url", search)?;
            once(&mut s.search_url, search, "one search url at a time")?;
            continue;
        }
        if let Some(text) = value_of(arg, "--scale", &mut args) {
            once(&mut s.scale, Scale::parse(text)?, "one scale at a time")?;
            continue;
        }
        if let Some(text) = value_of(arg, "--color-scheme", &mut args) {
            once(
                &mut s.scheme,
                appearance::Choice::parse(text)?,
                "one colour scheme at a time",
            )?;
            continue;
        }
        if let Some(dir) = value_of(arg, "--profile", &mut args) {
            let dir = needed(dir, "--profile needs a directory: --profile <dir>")?;
            once(
                &mut s.profile,
                profile::Choice::At(PathBuf::from(dir)),
                "one profile at a time",
            )?;
            continue;
        }
        if let Some(path) = value_of(arg, "--engine", &mut args) {
            let path = needed(path, "--engine needs a path: --engine <path>")?;
            once(&mut s.engine, PathBuf::from(path), "one engine at a time")?;
            continue;
        }
        if let Some(flag) = value_of(arg, "--engine-arg", &mut args) {
            let flag = needed(flag, "--engine-arg needs a flag: --engine-arg <--flag>")?;
            s.engine_args.push(parse_engine_arg("--engine-arg", flag)?);
            continue;
        }
        if let Some(agent) = value_of(arg, "--user-agent", &mut args) {
            let agent = needed(agent, "--user-agent needs text")?;
            once(
                &mut s.user_agent,
                agent.to_string(),
                "one user agent at a time",
            )?;
            continue;
        }
        if let Some(proxy) = value_of(arg, "--proxy", &mut args) {
            let proxy = needed(
                proxy,
                "--proxy needs host:port, scheme://host:port or direct://",
            )?;
            once(
                &mut s.proxy,
                parse_proxy("--proxy", proxy)?,
                "one proxy at a time",
            )?;
            continue;
        }
        if let Some(home) = value_of(arg, "--home", &mut args) {
            let home = needed(home, "--home needs a url")?;
            once(&mut s.home, home.to_string(), "one home page at a time")?;
            continue;
        }
        if let Some(path) = value_of(arg, "--config", &mut args) {
            let path = needed(path, "--config needs a path")?;
            config_choice(&mut s, ConfigChoice::At(PathBuf::from(path)))?;
            continue;
        }
        match arg.as_str() {
            "--temp-profile" => once(
                &mut s.profile,
                profile::Choice::Temporary,
                "one profile at a time",
            )?,
            "--force-dark" => once(&mut s.force_dark, true, "--force-dark once is enough")?,
            "--restore" => once(&mut s.restore, true, "--restore once is enough")?,
            "--normal-mode" => once(&mut s.normal_mode, true, "--normal-mode once is enough")?,
            "--no-config" => config_choice(&mut s, ConfigChoice::None)?,
            "--print-engine" => what(&mut s, What::PrintEngine)?,
            "--doctor" => what(&mut s, What::Doctor)?,
            _ if arg.starts_with('-') && arg.len() > 1 => {
                return Err(format!("unknown option: {arg}"));
            }
            _ => s.urls.push(arg.clone()),
        }
    }
    Ok(s)
}

/// `--config` or `--no-config`, either once, not both.
fn config_choice(s: &mut Settings, choice: ConfigChoice) -> Result<(), String> {
    match (&s.config, &choice) {
        (None, _) => {
            s.config = Some(choice);
            Ok(())
        }
        (Some(ConfigChoice::At(_)), ConfigChoice::At(_)) => {
            Err("one config file at a time".to_string())
        }
        (Some(ConfigChoice::None), ConfigChoice::None) => {
            Err("--no-config once is enough".to_string())
        }
        _ => Err("--config and --no-config together is a contradiction".to_string()),
    }
}

/// `--print-engine` or `--doctor`, one of them once.
fn what(s: &mut Settings, what: What) -> Result<(), String> {
    match s.what {
        None => {
            s.what = Some(what);
            Ok(())
        }
        Some(already) if already == what => Err(format!(
            "{} once is enough",
            if what == What::Doctor {
                "--doctor"
            } else {
                "--print-engine"
            }
        )),
        Some(_) => Err("--print-engine or --doctor, not both".to_string()),
    }
}

/// A settings file's bytes: refused whole if it is too big or not UTF-8,
/// else [`parse_config`].
pub fn parse_config_bytes(path: &Path, bytes: &[u8]) -> Result<Settings, String> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(format!(
            "{}: {} bytes is too big for a settings file; the limit is {} KiB",
            path.display(),
            bytes.len(),
            MAX_CONFIG_BYTES / 1024
        ));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| format!("{}: not UTF-8 text", path.display()))?;
    parse_config(path, text)
}

/// The keys a settings line may have, besides `key.<chord>`.
const KEYS: [&str; 14] = [
    "home",
    "profile",
    "temp-profile",
    "download-dir",
    "search-url",
    "scale",
    "color-scheme",
    "force-dark",
    "engine",
    "engine-arg",
    "user-agent",
    "proxy",
    "restore",
    "normal-mode",
];

/// One file's text. `path` is only for the sentences, every one of which is
/// `path:line: why`. Line numbers are 1-based and count every line, comments
/// and blanks included, so that they are the numbers an editor shows.
pub fn parse_config(path: &Path, text: &str) -> Result<Settings, String> {
    let mut s = Settings::default();
    // Where each scalar was first set, for the sentence about a second.
    let mut seen: Vec<(&str, usize)> = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        let number = index + 1;
        let at = |why: String| format!("{}:{number}: {why}", path.display());
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = split_setting(line) else {
            return Err(at("expected `key = value`".to_string()));
        };
        if key.is_empty() {
            return Err(at("a setting needs a name before the =".to_string()));
        }
        if value.is_empty() {
            return Err(at(format!("{key} needs a value")));
        }
        if key == "key" {
            return Err(at(
                "key needs a chord after a dot: key.ctrl+a = back".to_string()
            ));
        }
        if let Some(chord) = key.strip_prefix("key.") {
            // Understood in full, so that a misspelt chord or action is named
            // as precisely as it will be once bindings are looked up; then
            // refused, because nothing looks them up yet.
            let _binding = Binding::parse(chord, value).map_err(at)?;
            return Err(at(format!("{key}: {NOT_REMAPPABLE_YET}")));
        }
        if key == "url" {
            return Err(at(
                "unknown setting \"url\"; the page to open goes on the command line, or in home"
                    .to_string(),
            ));
        }
        let Some(&key) = KEYS.iter().find(|known| **known == key) else {
            return Err(at(format!(
                "unknown setting {key:?}; the settings are the options in --help without their --"
            )));
        };
        if key != "engine-arg" {
            if let Some((_, first)) = seen.iter().find(|(name, _)| *name == key) {
                return Err(at(format!("{key} is already set on line {first}")));
            }
            seen.push((key, number));
        }
        let line_of = |name: &str| {
            seen.iter()
                .find(|(seen, _)| *seen == name)
                .map(|(_, line)| *line)
        };
        match key {
            "home" => s.home = Some(value.to_string()),
            "profile" => {
                if s.profile == Some(profile::Choice::Temporary) {
                    let other = line_of("temp-profile").unwrap_or_default();
                    return Err(at(format!(
                        "profile is set, but temp-profile = true is on line {other}"
                    )));
                }
                s.profile = Some(profile::Choice::At(PathBuf::from(value)));
            }
            "temp-profile" => {
                if parse_bool(key, value).map_err(at)? {
                    if s.profile.is_some() {
                        let other = line_of("profile").unwrap_or_default();
                        return Err(at(format!(
                            "temp-profile = true, but profile is set on line {other}"
                        )));
                    }
                    s.profile = Some(profile::Choice::Temporary);
                }
            }
            "download-dir" => s.download_dir = Some(PathBuf::from(value)),
            "search-url" => s.search_url = Some(parse_search_url(key, value).map_err(at)?),
            "scale" => s.scale = Some(Scale::parse(value).map_err(at)?),
            "color-scheme" => s.scheme = Some(appearance::Choice::parse(value).map_err(at)?),
            "force-dark" => s.force_dark = Some(parse_bool(key, value).map_err(at)?),
            "engine" => s.engine = Some(PathBuf::from(value)),
            "engine-arg" => s
                .engine_args
                .push(parse_engine_arg(key, value).map_err(at)?),
            "user-agent" => s.user_agent = Some(value.to_string()),
            "proxy" => s.proxy = Some(parse_proxy(key, value).map_err(at)?),
            "restore" => s.restore = Some(parse_bool(key, value).map_err(at)?),
            "normal-mode" => s.normal_mode = Some(parse_bool(key, value).map_err(at)?),
            _ => unreachable!("every key in KEYS has an arm"),
        }
    }
    Ok(s)
}

/// A line's key and value, trimmed, split at the first `=` — except in a
/// `key.` line, whose chord may itself end in `=` (`key.ctrl+= = zoom-in`):
/// there the `=` that separates is the first one with a chord before it
/// that does not end in `+`, which is the one place a chord's own `=` can
/// be.
fn split_setting(line: &str) -> Option<(&str, &str)> {
    let mut from = 0;
    loop {
        let at = from + line[from..].find('=')?;
        let before = &line[..at];
        let chord = before.strip_prefix("key.");
        let inside_chord = chord.is_some_and(|chord| chord.is_empty() || chord.ends_with('+'));
        if !inside_chord {
            return Some((before.trim(), line[at + 1..].trim()));
        }
        from = at + 1;
    }
}

/// How many settings a file holds: the lines that are neither blank nor a
/// comment. For `--doctor`, and only after the file parsed.
pub fn count_settings(text: &str) -> usize {
    text.split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with(';'))
        .count()
}

/// `$XDG_CONFIG_HOME/blinkterm/config`, or `$HOME/.config/blinkterm/config`;
/// `None` with neither, since a config file is optional and a person with no
/// `$HOME` has not lost anything by not having one. A relative
/// `$XDG_CONFIG_HOME` is ignored, as the XDG specification says and as
/// [`profile::Profile::resolve`] does.
pub fn default_config_path(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let usable = |value: Option<&OsStr>| {
        value
            .map(Path::new)
            .filter(|path| path.is_absolute())
            .map(Path::to_path_buf)
    };
    if let Some(config) = usable(xdg) {
        return Some(config.join("blinkterm").join("config"));
    }
    usable(home).map(|home| home.join(".config").join("blinkterm").join("config"))
}

/// A leading `~/` in a path written in the file, made `$HOME`'s. The shell
/// does this for the command line; nothing does it for a file, and
/// `profile = ~/x` meaning a directory named `~` would be a login kept where
/// nobody will look.
pub fn expand_home(path: PathBuf, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path,
    }
}

/// The environment's contribution: `$BLINKTERM_ENGINE` as `engine`, and
/// nothing else. An empty variable says nothing, as it would to a shell.
/// ([`engine::locate`] keeps its own reading of the variable for the tests;
/// this is the same value, in the fold.)
pub fn from_env(engine: Option<&OsStr>) -> Settings {
    Settings {
        engine: engine.filter(|value| !value.is_empty()).map(PathBuf::from),
        ..Settings::default()
    }
}

/// Fold and fill: `cli` over `env` over `file`, then the defaults. Pure,
/// and the function the precedence tests call.
pub fn resolve(cli: Settings, env: Settings, file: Settings) -> Result<Options, String> {
    let s = cli.over(env).over(file);
    Ok(Options {
        urls: s.urls,
        home: s.home.unwrap_or_else(|| "about:blank".to_string()),
        profile: s.profile.unwrap_or(profile::Choice::Default),
        download: s
            .download_dir
            .map_or(download::Choice::Default, download::Choice::At),
        search_url: s.search_url,
        scale: s.scale.unwrap_or(Scale::Auto),
        scheme: s.scheme.unwrap_or_default(),
        force_dark: s.force_dark.unwrap_or(false),
        engine: engine::Launch {
            path: s.engine,
            args: s.engine_args,
            user_agent: s.user_agent,
            proxy: s.proxy,
        },
        restore: s.restore.unwrap_or(false),
        normal_mode: s.normal_mode.unwrap_or(false),
        bindings: Bindings::from_rows(s.bindings),
    })
}

/// The file, read: its settings, what `--doctor` says about it.
fn read_config(choice: Option<&ConfigChoice>) -> Result<(Settings, Provenance), String> {
    let mut provenance = Provenance {
        config: None,
        found: false,
        settings: 0,
        engine_from: None,
    };
    let (path, named) = match choice {
        Some(ConfigChoice::None) => return Ok((Settings::default(), provenance)),
        Some(ConfigChoice::At(path)) => (Some(path.clone()), true),
        None => (
            default_config_path(
                std::env::var_os("XDG_CONFIG_HOME").as_deref(),
                std::env::var_os("HOME").as_deref(),
            ),
            false,
        ),
    };
    let Some(path) = path else {
        return Ok((Settings::default(), provenance));
    };
    provenance.config = Some(path.clone());
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if named {
                return Err(format!("--config {}: no such file", path.display()));
            }
            return Ok((Settings::default(), provenance));
        }
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut settings = parse_config_bytes(&path, &bytes)?;
    provenance.found = true;
    provenance.settings = count_settings(&String::from_utf8_lossy(&bytes));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let home = home.as_deref();
    settings.profile = settings.profile.map(|choice| match choice {
        profile::Choice::At(dir) => profile::Choice::At(expand_home(dir, home)),
        other => other,
    });
    settings.download_dir = settings.download_dir.map(|dir| expand_home(dir, home));
    settings.engine = settings.engine.map(|path| expand_home(path, home));
    Ok((settings, provenance))
}

/// The whole thing, for `main`: parse the arguments; unless they said
/// `--no-config`, read the file they named or the default one; fold;
/// resolve. `--help` and `--version` are answered without reading anything
/// else, so a broken settings file never stands between a person and the
/// help.
pub fn invocation(args: &[String]) -> Result<Invocation, String> {
    let cli = parse_args(args)?;
    match cli.what {
        Some(What::Help) => return Ok(Invocation::Help),
        Some(What::Version) => return Ok(Invocation::Version),
        _ => {}
    }
    let (file, mut provenance) = read_config(cli.config.as_ref())?;
    let env = from_env(std::env::var_os(engine::ENGINE_ENV).as_deref());
    provenance.engine_from = if cli.engine.is_some() {
        Some("--engine")
    } else if env.engine.is_some() {
        Some("$BLINKTERM_ENGINE")
    } else if file.engine.is_some() {
        Some("config")
    } else {
        None
    };
    let what = cli.what;
    let options = resolve(cli, env, file)?;
    Ok(match what {
        Some(What::PrintEngine) => Invocation::PrintEngine(options),
        Some(What::Doctor) => Invocation::Doctor(options, provenance),
        _ => Invocation::Run(options),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{Action, Chord};
    use crate::profile::Choice;

    fn parsed(args: &[&str]) -> Result<Settings, String> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        parse_args(&args)
    }

    fn resolved(args: &[&str]) -> Result<Options, String> {
        resolve(parsed(args)?, Settings::default(), Settings::default())
    }

    fn file(text: &str) -> Result<Settings, String> {
        parse_config(Path::new("/c"), text)
    }

    // The command line.

    #[test]
    fn with_no_flags_every_setting_is_unsaid_and_resolve_fills_the_defaults() {
        assert_eq!(parsed(&[]), Ok(Settings::default()));
        let options = resolved(&[]).expect("nothing is fine");
        assert_eq!(options.profile, Choice::Default);
        assert_eq!(options.download, download::Choice::Default);
        assert!(options.urls.is_empty());
        assert_eq!(options.home, "about:blank");
        assert_eq!(options.search_url, None, "nothing typed is searched");
        assert_eq!(options.scale, Scale::Auto);
        assert_eq!(options.scheme, appearance::Choice::Auto);
        assert!(!options.force_dark, "nothing is painted dark unasked");
        assert_eq!(options.engine, engine::Launch::default());
        assert!(!options.restore && !options.normal_mode);
        assert!(options.bindings.is_empty());
    }

    #[test]
    fn several_urls_are_several_tabs_in_the_order_given() {
        let options =
            resolved(&["a.example", "--scale=2", "b.example", "c.example"]).expect("three pages");
        assert_eq!(options.urls, ["a.example", "b.example", "c.example"]);
    }

    #[test]
    fn a_double_dash_ends_the_options_and_what_follows_is_a_url_even_with_a_dash() {
        let s = parsed(&["--force-dark", "--", "-weird.example", "--help"]).expect("urls");
        assert_eq!(s.urls, ["-weird.example", "--help"]);
        assert_eq!(s.what, None, "a --help after -- is a url, not a question");
        assert_eq!(s.force_dark, Some(true));
    }

    #[test]
    fn help_and_version_win_over_everything_else_on_the_line() {
        for args in [
            &["--incognito", "--help"][..],
            &["--scale", "big", "-h"][..],
            &["-V", "--help"][..],
        ] {
            assert_eq!(
                parsed(args).map(|s| s.what),
                Ok(Some(What::Help)),
                "{args:?}"
            );
        }
        assert_eq!(
            parsed(&["--profile=", "--version"]).map(|s| s.what),
            Ok(Some(What::Version))
        );
    }

    #[test]
    fn a_search_url_can_be_named_either_way_round_the_equals_sign() {
        let search = "https://duckduckgo.com/?q=%s";
        for args in [
            &["--search-url", search, "example.com"][..],
            &["--search-url=https://duckduckgo.com/?q=%s", "example.com"][..],
            &["example.com", "--search-url", search][..],
        ] {
            let s = parsed(args).expect("a search url");
            assert_eq!(s.search_url.as_deref(), Some(search), "{args:?}");
            assert_eq!(s.urls, ["example.com"], "{args:?}");
        }
    }

    #[test]
    fn a_search_url_needs_a_percent_s() {
        let why = parsed(&["--search-url", "https://duckduckgo.com/"]).expect_err("refused");
        assert_eq!(why, "--search-url needs a %s where the words go");
        for args in [&["--search-url"][..], &["--search-url="][..]] {
            let why = parsed(args).expect_err("refused");
            assert!(why.contains("--search-url"), "{args:?}: {why}");
        }
        let why = parsed(&["--search-url=a%s", "--search-url", "b%s"]).expect_err("refused");
        assert_eq!(why, "one search url at a time");
    }

    #[test]
    fn a_scale_and_a_colour_scheme_are_parsed_by_their_own_parsers() {
        for args in [
            &["--scale", "2", "example.com"][..],
            &["--scale=2", "example.com"][..],
        ] {
            assert_eq!(parsed(args).map(|s| s.scale), Ok(Some(Scale::Fixed(2.0))));
        }
        for args in [
            &["--scale"][..],
            &["--scale="][..],
            &["--scale", "0"][..],
            &["--scale=5"][..],
            &["--scale", "big"][..],
        ] {
            let why = parsed(args).expect_err("refused");
            assert!(why.contains("--scale"), "{args:?}: {why}");
        }
        assert_eq!(
            parsed(&["--scale=2", "--scale", "auto"]),
            Err("one scale at a time".to_string())
        );
        for (word, choice) in [
            ("auto", appearance::Choice::Auto),
            ("light", appearance::Choice::Light),
            ("dark", appearance::Choice::Dark),
        ] {
            assert_eq!(
                parsed(&["--color-scheme", word]).map(|s| s.scheme),
                Ok(Some(choice))
            );
        }
        let why = parsed(&["--color-scheme=black"]).expect_err("refused");
        assert!(why.contains("--color-scheme"), "{why}");
        assert_eq!(
            parsed(&["--color-scheme=dark", "--color-scheme=light"]),
            Err("one colour scheme at a time".to_string())
        );
    }

    #[test]
    fn force_dark_is_a_flag_with_nothing_after_it() {
        let s = parsed(&["--force-dark", "example.com"]).expect("a flag");
        assert_eq!(s.force_dark, Some(true));
        assert_eq!(s.urls, ["example.com"], "what follows is the page");
        let why = parsed(&["--force-dark=yes"]).expect_err("refused");
        assert!(why.contains("--force-dark=yes"), "{why}");
        assert_eq!(
            parsed(&["--force-dark", "--force-dark"]),
            Err("--force-dark once is enough".to_string())
        );
    }

    #[test]
    fn a_profile_is_named_either_way_round_or_temporary_and_only_once() {
        for args in [
            &["--profile", "x", "example.com"][..],
            &["--profile=x", "example.com"][..],
        ] {
            assert_eq!(
                parsed(args).map(|s| s.profile),
                Ok(Some(Choice::At(PathBuf::from("x"))))
            );
        }
        assert_eq!(
            parsed(&["--temp-profile"]).map(|s| s.profile),
            Ok(Some(Choice::Temporary))
        );
        for args in [
            &["--profile", "x", "--temp-profile"][..],
            &["--temp-profile", "--profile=x"][..],
            &["--profile=x", "--profile=y"][..],
        ] {
            assert_eq!(
                parsed(args),
                Err("one profile at a time".to_string()),
                "{args:?}"
            );
        }
        for args in [&["--profile"][..], &["--profile="][..]] {
            let why = parsed(args).expect_err("refused");
            assert!(why.contains("--profile"), "{args:?}: {why}");
        }
    }

    #[test]
    fn a_download_directory_is_named_either_way_round_and_only_once() {
        for args in [
            &["--download-dir", "x", "example.com"][..],
            &["--download-dir=x", "example.com"][..],
        ] {
            assert_eq!(
                parsed(args).map(|s| s.download_dir),
                Ok(Some(PathBuf::from("x")))
            );
        }
        assert_eq!(
            parsed(&["--download-dir", "x", "--download-dir", "y"]),
            Err("one download directory at a time".to_string())
        );
        for args in [
            &["--download-dir"][..],
            &["--download-dir="][..],
            &["--download-dir", ""][..],
        ] {
            let why = parsed(args).expect_err("refused");
            assert!(why.contains("--download-dir"), "{args:?}: {why}");
        }
    }

    #[test]
    fn an_unknown_option_is_named() {
        let why = parsed(&["--incognito"]).expect_err("refused");
        assert_eq!(why, "unknown option: --incognito");
    }

    #[test]
    fn an_engine_can_be_named_either_way_round_the_equals_sign() {
        for args in [
            &["--engine", "/opt/c/chrome-headless-shell"][..],
            &["--engine=/opt/c/chrome-headless-shell"][..],
        ] {
            assert_eq!(
                parsed(args).map(|s| s.engine),
                Ok(Some(PathBuf::from("/opt/c/chrome-headless-shell")))
            );
        }
        assert_eq!(
            parsed(&["--engine=a", "--engine=b"]),
            Err("one engine at a time".to_string())
        );
    }

    #[test]
    fn an_engine_with_no_path_is_an_error_that_names_the_option() {
        for args in [&["--engine"][..], &["--engine="][..]] {
            assert_eq!(
                parsed(args),
                Err("--engine needs a path: --engine <path>".to_string())
            );
        }
    }

    #[test]
    fn engine_args_accumulate_in_order_and_each_needs_a_flag() {
        let s = parsed(&[
            "--engine-arg",
            "--accept-lang=ja",
            "--engine-arg=--disable-features=Translate",
        ])
        .expect("two flags");
        assert_eq!(
            s.engine_args,
            ["--accept-lang=ja", "--disable-features=Translate"]
        );
        assert_eq!(
            parsed(&["--engine-arg"]),
            Err("--engine-arg needs a flag: --engine-arg <--flag>".to_string())
        );
        assert_eq!(
            parsed(&["--engine-arg", "foo"]),
            Err("--engine-arg needs a flag starting with -, not \"foo\"".to_string())
        );
        let s = parsed(&["--engine-arg=--no-sandbox"]).expect("allowed");
        assert_eq!(s.engine_args, ["--no-sandbox"]);
    }

    #[test]
    fn the_four_reserved_engine_args_are_refused_by_name_with_and_without_a_value() {
        for flag in [
            "--remote-debugging-port",
            "--remote-debugging-port=0",
            "--user-data-dir=/x",
            "--remote-allow-origins=*",
            "--remote-debugging-pipe",
        ] {
            let why = parsed(&["--engine-arg", flag]).expect_err(flag);
            assert!(
                why.starts_with(&format!("{flag} is refused as an engine argument: ")),
                "{why}"
            );
        }
        let s = parsed(&["--engine-arg=--remote-debugging-portal"]).expect("not the flag");
        assert_eq!(s.engine_args, ["--remote-debugging-portal"]);
    }

    #[test]
    fn a_user_agent_keeps_its_spaces_and_parentheses() {
        let agent = "Mozilla/5.0 (X11; Linux x86_64) blinkterm";
        assert_eq!(
            parsed(&["--user-agent", agent]).map(|s| s.user_agent),
            Ok(Some(agent.to_string()))
        );
        assert_eq!(
            parsed(&[&format!("--user-agent={agent}")]).map(|s| s.user_agent),
            Ok(Some(agent.to_string()))
        );
        assert_eq!(
            parsed(&["--user-agent="]),
            Err("--user-agent needs text".to_string())
        );
    }

    #[test]
    fn a_proxy_is_host_port_or_a_scheme_and_never_has_a_space_in_it() {
        for proxy in ["127.0.0.1:3128", "socks5://127.0.0.1:1080", "direct://"] {
            assert_eq!(
                parsed(&["--proxy", proxy]).map(|s| s.proxy),
                Ok(Some(proxy.to_string()))
            );
        }
        let why = parsed(&["--proxy", "a b"]).expect_err("a space");
        assert_eq!(
            why,
            "--proxy needs host:port, scheme://host:port or direct://, not \"a b\""
        );
        let why = parsed(&["--proxy="]).expect_err("empty");
        assert_eq!(
            why,
            "--proxy needs host:port, scheme://host:port or direct://"
        );
    }

    #[test]
    fn restore_and_normal_mode_are_flags_with_nothing_after_them_and_once_is_enough() {
        let s = parsed(&["--restore", "--normal-mode"]).expect("two flags");
        assert_eq!((s.restore, s.normal_mode), (Some(true), Some(true)));
        assert_eq!(
            parsed(&["--restore", "--restore"]),
            Err("--restore once is enough".to_string())
        );
        assert_eq!(
            parsed(&["--normal-mode", "--normal-mode"]),
            Err("--normal-mode once is enough".to_string())
        );
        assert!(parsed(&["--restore=true"]).is_err());
    }

    #[test]
    fn config_and_no_config_together_is_a_contradiction() {
        assert_eq!(
            parsed(&["--config", "/x"]).map(|s| s.config),
            Ok(Some(ConfigChoice::At(PathBuf::from("/x"))))
        );
        assert_eq!(
            parsed(&["--no-config"]).map(|s| s.config),
            Ok(Some(ConfigChoice::None))
        );
        for args in [
            &["--config=/x", "--no-config"][..],
            &["--no-config", "--config", "/x"][..],
        ] {
            assert_eq!(
                parsed(args),
                Err("--config and --no-config together is a contradiction".to_string())
            );
        }
        assert_eq!(
            parsed(&["--config="]),
            Err("--config needs a path".to_string())
        );
    }

    #[test]
    fn home_needs_a_url() {
        assert_eq!(
            parsed(&["--home", "example.com"]).map(|s| s.home),
            Ok(Some("example.com".to_string()))
        );
        assert_eq!(parsed(&["--home"]), Err("--home needs a url".to_string()));
        assert_eq!(file("home ="), Err("/c:1: home needs a value".to_string()));
    }

    #[test]
    fn print_engine_and_doctor_say_what_the_run_is_for_and_one_of_them_at_a_time() {
        assert_eq!(
            parsed(&["--print-engine"]).map(|s| s.what),
            Ok(Some(What::PrintEngine))
        );
        assert_eq!(
            parsed(&["--doctor", "--engine=/x"]).map(|s| s.what),
            Ok(Some(What::Doctor))
        );
        assert_eq!(
            parsed(&["--doctor", "--print-engine"]),
            Err("--print-engine or --doctor, not both".to_string())
        );
    }

    // The file.

    #[test]
    fn a_comment_a_blank_line_and_a_windows_line_ending_are_nothing() {
        let s = file("# a comment\n\n  ; another\r\n   \nscale = 2\r\n").expect("a file");
        assert_eq!(
            s,
            Settings {
                scale: Some(Scale::Fixed(2.0)),
                ..Settings::default()
            }
        );
        assert_eq!(file(""), Ok(Settings::default()));
    }

    #[test]
    fn a_setting_is_the_key_the_equals_and_the_value_trimmed() {
        let s =
            file("  color-scheme=dark  \nuser-agent =   Mozilla/5.0 (X11) b  ").expect("a file");
        assert_eq!(s.scheme, Some(appearance::Choice::Dark));
        assert_eq!(s.user_agent.as_deref(), Some("Mozilla/5.0 (X11) b"));
    }

    #[test]
    fn a_value_keeps_everything_after_the_first_equals_including_more_of_them() {
        let s = file("search-url = https://x/?q=%s&a=b=c").expect("a file");
        assert_eq!(s.search_url.as_deref(), Some("https://x/?q=%s&a=b=c"));
        let s = file("engine-arg = --disable-features=Translate").expect("a file");
        assert_eq!(s.engine_args, ["--disable-features=Translate"]);
    }

    #[test]
    fn a_line_without_an_equals_says_so_with_its_line_number() {
        assert_eq!(
            file("# x\nscale 2"),
            Err("/c:2: expected `key = value`".to_string())
        );
        assert_eq!(
            file("= 2"),
            Err("/c:1: a setting needs a name before the =".to_string())
        );
        assert_eq!(
            file("scale ="),
            Err("/c:1: scale needs a value".to_string())
        );
    }

    #[test]
    fn an_unknown_setting_is_named_with_its_line_number() {
        assert_eq!(
            file("colour-scheme = dark"),
            Err("/c:1: unknown setting \"colour-scheme\"; the settings are the options in --help without their --".to_string())
        );
        for key in ["config", "help", "doctor", "--scale"] {
            let why = file(&format!("{key} = x")).expect_err(key);
            assert!(why.contains("unknown setting"), "{why}");
        }
    }

    #[test]
    fn a_bad_value_carries_the_value_parsers_sentence_and_the_line_number() {
        assert_eq!(
            file("\nscale = big"),
            Err("/c:2: --scale is auto or a number from 0.5 to 4, not \"big\"".to_string())
        );
        assert_eq!(
            file("search-url = https://x/"),
            Err("/c:1: search-url needs a %s where the words go".to_string())
        );
        assert_eq!(
            file("proxy = a b"),
            Err(
                "/c:1: proxy needs host:port, scheme://host:port or direct://, not \"a b\""
                    .to_string()
            )
        );
        let why = file("color-scheme = black").expect_err("refused");
        assert!(why.starts_with("/c:1: --color-scheme"), "{why}");
    }

    #[test]
    fn a_bool_is_true_or_false_and_nothing_else() {
        let s = file("force-dark = true\nrestore = false\nnormal-mode = true").expect("a file");
        assert_eq!(
            (s.force_dark, s.restore, s.normal_mode),
            (Some(true), Some(false), Some(true))
        );
        for word in ["yes", "1", "on", "True"] {
            assert_eq!(
                file(&format!("restore = {word}")),
                Err(format!("/c:1: restore is true or false, not {word:?}"))
            );
        }
    }

    #[test]
    fn a_scalar_set_twice_names_both_lines() {
        assert_eq!(
            file("# x\n\nscale = 2\nscale = 1"),
            Err("/c:4: scale is already set on line 3".to_string())
        );
    }

    #[test]
    fn engine_arg_may_be_repeated_and_the_reserved_ones_are_refused_here_too() {
        let s = file("engine-arg = --accept-lang=ja\nengine-arg = --a\nengine-arg = --b")
            .expect("a file");
        assert_eq!(s.engine_args, ["--accept-lang=ja", "--a", "--b"]);
        assert_eq!(
            file("engine-arg = --user-data-dir=/x"),
            Err("/c:1: --user-data-dir=/x is refused as an engine argument: it is what --profile sets".to_string())
        );
        assert_eq!(
            file("engine-arg = foo"),
            Err("/c:1: engine-arg needs a flag starting with -, not \"foo\"".to_string())
        );
    }

    #[test]
    fn profile_and_temp_profile_true_in_one_file_is_refused_at_the_second() {
        assert_eq!(
            file("\n\n\nprofile = /p\n\n\n\n\ntemp-profile = true"),
            Err("/c:9: temp-profile = true, but profile is set on line 4".to_string())
        );
        assert_eq!(
            file("temp-profile = true\nprofile = /p"),
            Err("/c:2: profile is set, but temp-profile = true is on line 1".to_string())
        );
        assert_eq!(
            file("temp-profile = true").map(|s| s.profile),
            Ok(Some(Choice::Temporary))
        );
    }

    #[test]
    fn temp_profile_false_says_nothing() {
        assert_eq!(file("temp-profile = false"), Ok(Settings::default()));
        assert_eq!(
            file("temp-profile = false\nprofile = /p").map(|s| s.profile),
            Ok(Some(Choice::At(PathBuf::from("/p"))))
        );
    }

    #[test]
    fn url_is_not_a_setting_and_the_sentence_points_at_home() {
        let why = file("url = https://x").expect_err("refused");
        assert!(why.starts_with("/c:1: unknown setting \"url\""), "{why}");
        assert!(why.contains("home"), "{why}");
    }

    #[test]
    fn a_key_line_is_understood_and_then_refused_as_not_remappable_yet() {
        for line in [
            "key.ctrl+b = back",
            "key.alt+w = none",
            "key.ctrl+= = zoom-in",
        ] {
            let why = file(line).expect_err(line);
            assert!(why.starts_with("/c:1: key."), "{why}");
            assert!(why.ends_with(NOT_REMAPPABLE_YET), "{why}");
        }
        assert_eq!(
            file("key.ctrl+= = zoom-in"),
            Err(format!("/c:1: key.ctrl+=: {NOT_REMAPPABLE_YET}"))
        );
    }

    #[test]
    fn a_key_line_with_a_bad_chord_or_action_is_named_with_its_line_number() {
        assert_eq!(
            file("# keys\nkey.ctrl+ = back"),
            Err("/c:2: key.ctrl+: no key after the last +".to_string())
        );
        assert_eq!(
            file("key.hyper+a = back"),
            Err(
                "/c:1: key.hyper+a: \"hyper\" is not a modifier; ctrl, alt, shift or super"
                    .to_string()
            )
        );
        let why = file("key.ctrl+a = bookmark").expect_err("refused");
        assert!(
            why.starts_with("/c:1: \"bookmark\" is not an action; they are quit, url, reload"),
            "{why}"
        );
        assert_eq!(
            file("key = ctrl+a"),
            Err("/c:1: key needs a chord after a dot: key.ctrl+a = back".to_string())
        );
    }

    #[test]
    fn line_numbers_count_comments_and_blanks() {
        let text = "# one\n\n; three\n\n   \n# six\nnonsense = 1";
        let why = file(text).expect_err("refused");
        assert!(why.starts_with("/c:7: "), "{why}");
    }

    #[test]
    fn a_file_that_is_not_utf8_is_refused_whole() {
        assert_eq!(
            parse_config_bytes(Path::new("/c"), b"scale = 2\n\xff\xfe"),
            Err("/c: not UTF-8 text".to_string())
        );
    }

    #[test]
    fn a_file_over_the_limit_is_refused_with_its_size() {
        let big = vec![b'#'; MAX_CONFIG_BYTES + 1];
        let why = parse_config_bytes(Path::new("/c"), &big).expect_err("too big");
        assert!(
            why.starts_with(&format!("/c: {} bytes", MAX_CONFIG_BYTES + 1)),
            "{why}"
        );
        let fine = vec![b'#'; MAX_CONFIG_BYTES];
        assert_eq!(
            parse_config_bytes(Path::new("/c"), &fine),
            Ok(Settings::default())
        );
    }

    #[test]
    fn the_default_path_is_under_xdg_config_home_else_under_home_else_none() {
        let os = |s: &'static str| Some(OsStr::new(s));
        assert_eq!(
            default_config_path(os("/x"), os("/h")),
            Some(PathBuf::from("/x/blinkterm/config"))
        );
        assert_eq!(
            default_config_path(os("relative"), os("/h")),
            Some(PathBuf::from("/h/.config/blinkterm/config")),
            "a relative XDG_CONFIG_HOME is ignored"
        );
        assert_eq!(
            default_config_path(os(""), os("/h")),
            Some(PathBuf::from("/h/.config/blinkterm/config"))
        );
        assert_eq!(default_config_path(None, None), None);
        assert_eq!(default_config_path(None, os("relative")), None);
    }

    #[test]
    fn a_tilde_in_a_path_from_the_file_is_home() {
        let home = Some(Path::new("/h"));
        assert_eq!(
            expand_home(PathBuf::from("~/p"), home),
            PathBuf::from("/h/p")
        );
        assert_eq!(expand_home(PathBuf::from("~"), home), PathBuf::from("/h"));
        assert_eq!(
            expand_home(PathBuf::from("/a/~/p"), home),
            PathBuf::from("/a/~/p")
        );
        assert_eq!(
            expand_home(PathBuf::from("~x/p"), home),
            PathBuf::from("~x/p")
        );
        assert_eq!(
            expand_home(PathBuf::from("~/p"), None),
            PathBuf::from("~/p")
        );
    }

    #[test]
    fn a_settings_file_is_counted_by_its_settings() {
        assert_eq!(
            count_settings("# a\n\nscale = 2\n ; b\nforce-dark = true\n"),
            2
        );
    }

    // The fold.

    #[test]
    fn the_command_line_beats_the_environment_which_beats_the_file() {
        let cli = parsed(&["--engine", "/cli"]).expect("cli");
        let env = from_env(Some(OsStr::new("/env")));
        let from_file = file("engine = /file").expect("file");
        let engine = |cli: &Settings, env: &Settings, f: &Settings| {
            resolve(cli.clone(), env.clone(), f.clone())
                .expect("resolves")
                .engine
                .path
        };
        let none = Settings::default();
        assert_eq!(engine(&cli, &env, &from_file), Some(PathBuf::from("/cli")));
        assert_eq!(engine(&none, &env, &from_file), Some(PathBuf::from("/env")));
        assert_eq!(
            engine(&none, &none, &from_file),
            Some(PathBuf::from("/file"))
        );
        assert_eq!(engine(&none, &none, &none), None);
        assert_eq!(from_env(Some(OsStr::new(""))), Settings::default());
    }

    #[test]
    fn the_command_line_beats_the_file_on_every_scalar() {
        let from_file = file(
            "home = f.example\nprofile = /fp\ndownload-dir = /fd\nsearch-url = f%s\n\
             scale = 1\ncolor-scheme = light\nforce-dark = false\nengine = /fe\n\
             user-agent = fa\nproxy = f:1\nrestore = false\nnormal-mode = false",
        )
        .expect("file");
        let cli = parsed(&[
            "--home=c.example",
            "--temp-profile",
            "--download-dir=/cd",
            "--search-url=c%s",
            "--scale=2",
            "--color-scheme=dark",
            "--force-dark",
            "--engine=/ce",
            "--user-agent=ca",
            "--proxy=c:1",
            "--restore",
            "--normal-mode",
        ])
        .expect("cli");
        let options = resolve(cli, Settings::default(), from_file).expect("resolves");
        assert_eq!(options.home, "c.example");
        assert_eq!(options.profile, Choice::Temporary);
        assert_eq!(options.download, download::Choice::At("/cd".into()));
        assert_eq!(options.search_url.as_deref(), Some("c%s"));
        assert_eq!(options.scale, Scale::Fixed(2.0));
        assert_eq!(options.scheme, appearance::Choice::Dark);
        assert!(options.force_dark);
        assert_eq!(options.engine.path, Some(PathBuf::from("/ce")));
        assert_eq!(options.engine.user_agent.as_deref(), Some("ca"));
        assert_eq!(options.engine.proxy.as_deref(), Some("c:1"));
        assert!(options.restore);
        assert!(options.normal_mode);
    }

    #[test]
    fn the_file_fills_what_the_command_line_did_not_say() {
        let from_file = file("scale = 2\ncolor-scheme = dark\nhome = h.example").expect("file");
        let cli = parsed(&["--force-dark", "a.example"]).expect("cli");
        let options = resolve(cli, Settings::default(), from_file).expect("resolves");
        assert_eq!(options.scale, Scale::Fixed(2.0));
        assert_eq!(options.scheme, appearance::Choice::Dark);
        assert_eq!(options.home, "h.example");
        assert!(options.force_dark);
        assert_eq!(options.urls, ["a.example"]);
    }

    #[test]
    fn engine_args_are_the_files_then_the_command_lines() {
        let from_file = file("engine-arg = --f1\nengine-arg = --f2").expect("file");
        let cli = parsed(&["--engine-arg=--c1", "--engine", "/x"]).expect("cli");
        let options = resolve(cli, Settings::default(), from_file).expect("resolves");
        assert_eq!(options.engine.args, ["--f1", "--f2", "--c1"]);
        assert_eq!(
            options.engine.path,
            Some(PathBuf::from("/x")),
            "an engine named does not clear the file's arguments"
        );
    }

    #[test]
    fn temp_profile_on_the_command_line_beats_a_profile_in_the_file_and_vice_versa() {
        let resolved = |args: &[&str], text: &str| {
            resolve(
                parsed(args).expect("cli"),
                Settings::default(),
                file(text).expect("file"),
            )
            .expect("resolves")
            .profile
        };
        assert_eq!(
            resolved(&["--temp-profile"], "profile = /p"),
            Choice::Temporary
        );
        assert_eq!(
            resolved(&["--profile=/c"], "temp-profile = true"),
            Choice::At(PathBuf::from("/c"))
        );
    }

    #[test]
    fn the_environment_sets_only_the_engine() {
        assert_eq!(
            from_env(Some(OsStr::new("/e"))),
            Settings {
                engine: Some(PathBuf::from("/e")),
                ..Settings::default()
            }
        );
        assert_eq!(from_env(None), Settings::default());
    }

    #[test]
    fn bindings_come_from_the_file_alone() {
        let row = Binding {
            chord: Chord::parse("alt+b").expect("a chord"),
            action: Some(Action::Back),
        };
        let from_file = Settings {
            bindings: vec![row.clone()],
            ..Settings::default()
        };
        let options =
            resolve(Settings::default(), Settings::default(), from_file).expect("resolves");
        assert_eq!(options.bindings, Bindings::from_rows(vec![row]));
        assert!(parsed(&["--key.alt+b=back"]).is_err(), "not an option");
    }
}
