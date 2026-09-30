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
//! The one family that is not an option is `key.<chord> = <action>`
//! (`key.f5 = reload`), which rebinds one of the program's keys and may be
//! written as often as there are keys; see [`crate::bindings`].
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
use crate::block;
use crate::download;
use crate::engine;
use crate::login;
use crate::picker;
use crate::profile;
use crate::route;
use crate::save;
use crate::sites;
use crate::zoom::Scale;

/// The largest settings file that is read. Nothing here needs a tenth of
/// it; a binary named by mistake with `--config` should say so rather than
/// produce a hundred line errors.
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;

/// What [`parse_config`] says about a `normal.` line.
///
/// The letters of normal mode are a small language rather than a table — `gg`
/// is two keys, and half of them are not commands but things the loop does
/// to the page — so rebinding them is a design of its own. The shape is kept
/// for it, `normal.<key> = <action>`, and refused with a sentence now, so
/// that a file written ahead of it says what is wrong rather than "unknown
/// setting".
pub const NORMAL_NOT_REMAPPABLE_YET: &str =
    "normal-mode letters are not remappable yet; key.<chord> is for the program's own keys";

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
    /// `--pdf-paper`: what `alt+s` prints on, or `None` for the locale's.
    /// See [`crate::save::Paper`].
    pub pdf_paper: Option<save::Paper>,
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
    /// `--alpha`: the engine paints no default background, the page's
    /// `html` and `body` none either, and the terminal's shows through at an
    /// amount. See [`crate::appearance`].
    pub alpha: appearance::Alpha,
    /// How the engine is started; see [`crate::engine::Launch`].
    pub engine: engine::Launch,
    /// `--restore`: reopen the last session's tabs at start. See
    /// [`crate::session`].
    pub restore: bool,
    /// `--normal-mode`: start in normal mode rather than insert: the letters
    /// are the program's keys from the first one. See [`crate::normal`].
    pub normal_mode: bool,
    /// `key.<chord> = <action>` lines from the file, in file order; the loop
    /// asks them before its own table. See [`crate::bindings`].
    pub bindings: Bindings,
    /// `--tmux`, `--frames`, `--fps` and `--no-probe`: what overrides the
    /// route a run's frames take. See [`crate::route`].
    pub route: route::Choices,
    /// `--file-picker` and its three siblings: the programs that answer a
    /// page's file input instead of the row. See [`crate::picker`].
    pub pickers: picker::Pickers,
    /// `--remote`: hand the urls to the blinkterm already running on this
    /// profile and exit, starting as usual only when none is. Command line
    /// only. See [`crate::remote`].
    pub remote: bool,
    /// `--block-list` and `--no-block`: the host lists requests are blocked
    /// by, and whether to. See [`crate::block`].
    pub block: block::Lists,
    /// `--sites-dir` and `--no-sites`: where the site styles and scripts are
    /// read from, and whether. See [`crate::sites`].
    pub sites: sites::Location,
    /// `--password-command` and `--password-command-terminal`: what
    /// `fill-login` runs. See [`crate::login`].
    pub logins: login::Programs,
    /// `--external-browser`: what `open-external` runs; none for the
    /// platform's own. See [`crate::external`].
    pub external_browser: Option<picker::Command>,
    /// `false` with `--no-console` or `console = false`: the page's console
    /// is not listened to, and `ctrl+shift+j` says so. See
    /// [`crate::console`].
    pub console: bool,
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
    pub pdf_paper: Option<save::Paper>,
    pub search_url: Option<String>,
    pub scale: Option<Scale>,
    pub scheme: Option<appearance::Choice>,
    pub force_dark: Option<bool>,
    pub alpha: Option<appearance::Alpha>,
    pub engine: Option<PathBuf>,
    /// Appended across sources, never replaced.
    pub engine_args: Vec<String>,
    pub user_agent: Option<String>,
    pub proxy: Option<String>,
    /// `--mute`, `mute = true`: the engine's `--mute-audio`.
    pub mute: Option<bool>,
    pub restore: Option<bool>,
    pub normal_mode: Option<bool>,
    pub tmux: Option<route::Choice>,
    pub frames: Option<route::Frames>,
    pub fps: Option<u32>,
    /// `false` with `--no-probe` or `probe = false`.
    pub probe: Option<bool>,
    pub file_picker: Option<picker::Command>,
    pub file_picker_multiple: Option<picker::Command>,
    pub file_picker_terminal: Option<picker::Command>,
    pub file_picker_terminal_multiple: Option<picker::Command>,
    /// `--block-list`, `block-list`: appended across sources, never
    /// replaced, the file's first.
    pub block_lists: Vec<PathBuf>,
    /// `false` with `--no-block` or `block = false`.
    pub block: Option<bool>,
    /// `--sites-dir`, `sites-dir`.
    pub sites_dir: Option<PathBuf>,
    /// `false` with `--no-sites` or `sites = false`.
    pub sites: Option<bool>,
    /// `false` with `--no-console` or `console = false`.
    pub console: Option<bool>,
    pub password_command: Option<picker::Command>,
    pub password_command_terminal: Option<picker::Command>,
    pub external_browser: Option<picker::Command>,
    /// File only: a binding is not a one-run thing.
    pub bindings: Vec<Binding>,
    /// Command line only.
    pub config: Option<ConfigChoice>,
    /// Command line only.
    pub what: Option<What>,
    /// `--remote`. Command line only: it says what this one run is for, and
    /// a settings file that made every run a sender would never start one.
    pub remote: Option<bool>,
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
        let mut block_lists = under.block_lists;
        block_lists.extend(self.block_lists);
        Settings {
            urls,
            home: self.home.or(under.home),
            profile: self.profile.or(under.profile),
            download_dir: self.download_dir.or(under.download_dir),
            pdf_paper: self.pdf_paper.or(under.pdf_paper),
            search_url: self.search_url.or(under.search_url),
            scale: self.scale.or(under.scale),
            scheme: self.scheme.or(under.scheme),
            force_dark: self.force_dark.or(under.force_dark),
            alpha: self.alpha.or(under.alpha),
            engine: self.engine.or(under.engine),
            engine_args,
            user_agent: self.user_agent.or(under.user_agent),
            proxy: self.proxy.or(under.proxy),
            mute: self.mute.or(under.mute),
            restore: self.restore.or(under.restore),
            normal_mode: self.normal_mode.or(under.normal_mode),
            tmux: self.tmux.or(under.tmux),
            frames: self.frames.or(under.frames),
            fps: self.fps.or(under.fps),
            probe: self.probe.or(under.probe),
            file_picker: self.file_picker.or(under.file_picker),
            file_picker_multiple: self.file_picker_multiple.or(under.file_picker_multiple),
            file_picker_terminal: self.file_picker_terminal.or(under.file_picker_terminal),
            file_picker_terminal_multiple: self
                .file_picker_terminal_multiple
                .or(under.file_picker_terminal_multiple),
            block_lists,
            block: self.block.or(under.block),
            sites_dir: self.sites_dir.or(under.sites_dir),
            sites: self.sites.or(under.sites),
            console: self.console.or(under.console),
            password_command: self.password_command.or(under.password_command),
            password_command_terminal: self
                .password_command_terminal
                .or(under.password_command_terminal),
            external_browser: self.external_browser.or(under.external_browser),
            bindings,
            config: self.config.or(under.config),
            what: self.what.or(under.what),
            remote: self.remote.or(under.remote),
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
/// `--alpha` takes an amount if one follows: `--alpha=70`, or `--alpha 70`
/// when the next word starts with a digit or is `true` or `false`, which a
/// url never does; anything else after it is left alone, and it is 100.
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
        if let Some(text) = value_of(arg, "--pdf-paper", &mut args) {
            once(
                &mut s.pdf_paper,
                save::Paper::parse(text)?,
                "--pdf-paper once is enough",
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
        if let Some(text) = value_of(arg, "--tmux", &mut args) {
            once(
                &mut s.tmux,
                route::Choice::parse(text)?,
                "--tmux once is enough",
            )?;
            continue;
        }
        if let Some(text) = value_of(arg, "--frames", &mut args) {
            once(
                &mut s.frames,
                route::Frames::parse(text)?,
                "--frames once is enough",
            )?;
            continue;
        }
        if let Some(text) = value_of(arg, "--fps", &mut args) {
            once(
                &mut s.fps,
                route::parse_fps("--fps", text)?,
                "--fps once is enough",
            )?;
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
        if let Some(path) = value_of(arg, "--block-list", &mut args) {
            let path = needed(path, "--block-list needs a path: --block-list <path>")?;
            s.block_lists.push(PathBuf::from(path));
            continue;
        }
        if let Some(dir) = value_of(arg, "--sites-dir", &mut args) {
            let dir = needed(dir, "--sites-dir needs a directory: --sites-dir <dir>")?;
            once(
                &mut s.sites_dir,
                PathBuf::from(dir),
                "one sites directory at a time",
            )?;
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
        // Longest first, though `value_of` would not mistake one for the
        // other: `--file-picker-terminal=x` is not `--file-picker` with a
        // value, since what follows the name has to be `=` or nothing.
        let pickers = [
            "--file-picker-terminal-multiple",
            "--file-picker-terminal",
            "--file-picker-multiple",
            "--file-picker",
            "--password-command-terminal",
            "--password-command",
            "--external-browser",
        ];
        if let Some((name, text)) = pickers
            .iter()
            .find_map(|name| value_of(arg, name, &mut args).map(|text| (*name, text)))
        {
            let slot = match name {
                "--file-picker-terminal-multiple" => &mut s.file_picker_terminal_multiple,
                "--file-picker-terminal" => &mut s.file_picker_terminal,
                "--file-picker-multiple" => &mut s.file_picker_multiple,
                "--password-command-terminal" => &mut s.password_command_terminal,
                "--password-command" => &mut s.password_command,
                "--external-browser" => &mut s.external_browser,
                _ => &mut s.file_picker,
            };
            let text = needed(text, &format!("{name} needs a command: {name} <command>"))?;
            once(
                slot,
                picker::Command::parse(name, text)?,
                &format!("{name} once is enough"),
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
        if let Some(text) = arg.strip_prefix("--alpha=") {
            once(
                &mut s.alpha,
                appearance::Alpha::parse("--alpha", text)?,
                "--alpha once is enough",
            )?;
            continue;
        }
        if arg == "--alpha" {
            // The amount is optional, so the next word is it only if it
            // could be nothing else: a url does not start with a digit, and
            // `--alpha 0` is then refused by name rather than opened.
            let amount = args.as_slice().first().filter(|next| {
                next.starts_with(|c: char| c.is_ascii_digit())
                    || next.as_str() == "true"
                    || next.as_str() == "false"
            });
            let alpha = match amount {
                Some(text) => {
                    args.next();
                    appearance::Alpha::parse("--alpha", text)?
                }
                None => appearance::Alpha::On(100),
            };
            once(&mut s.alpha, alpha, "--alpha once is enough")?;
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
            "--mute" => once(&mut s.mute, true, "--mute once is enough")?,
            "--normal-mode" => once(&mut s.normal_mode, true, "--normal-mode once is enough")?,
            "--no-probe" => once(&mut s.probe, false, "--no-probe once is enough")?,
            "--no-block" => once(&mut s.block, false, "--no-block once is enough")?,
            "--no-sites" => once(&mut s.sites, false, "--no-sites once is enough")?,
            "--no-console" => once(&mut s.console, false, "--no-console once is enough")?,
            "--no-config" => config_choice(&mut s, ConfigChoice::None)?,
            "--print-engine" => what(&mut s, What::PrintEngine)?,
            "--doctor" => what(&mut s, What::Doctor)?,
            "--remote" => once(&mut s.remote, true, "--remote once is enough")?,
            _ if arg.starts_with('-') && arg.len() > 1 => {
                return Err(format!("unknown option: {arg}"));
            }
            _ => s.urls.push(arg.clone()),
        }
    }
    if s.remote == Some(true) {
        if s.urls.is_empty() {
            return Err("--remote needs a url to open: blinkterm --remote <url>".to_string());
        }
        // Only the command line's own --temp-profile: `temp-profile = true`
        // in the file with --remote simply starts as usual, so that a
        // setting nobody is thinking about does not break `gh browse`.
        if s.profile == Some(profile::Choice::Temporary) {
            return Err("--remote and --temp-profile together is a contradiction: \
                 a temporary profile is its run's alone, so there is no blinkterm \
                 to reach on it"
                .to_string());
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
const KEYS: [&str; 33] = [
    "home",
    "profile",
    "temp-profile",
    "download-dir",
    "pdf-paper",
    "search-url",
    "scale",
    "color-scheme",
    "force-dark",
    "alpha",
    "engine",
    "engine-arg",
    "user-agent",
    "proxy",
    "mute",
    "restore",
    "normal-mode",
    "tmux",
    "frames",
    "fps",
    "probe",
    "file-picker",
    "file-picker-multiple",
    "file-picker-terminal",
    "file-picker-terminal-multiple",
    "block-list",
    "block",
    "sites-dir",
    "sites",
    "console",
    "password-command",
    "password-command-terminal",
    "external-browser",
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
            // Repeatable, like `engine-arg`: a later line for the same chord
            // is the one that counts, which `Bindings::lookup` decides.
            s.bindings.push(Binding::parse(chord, value).map_err(at)?);
            continue;
        }
        if key == "normal" || key.starts_with("normal.") {
            return Err(at(NORMAL_NOT_REMAPPABLE_YET.to_string()));
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
        if key != "engine-arg" && key != "block-list" {
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
            "pdf-paper" => s.pdf_paper = Some(save::Paper::parse(value).map_err(at)?),
            "search-url" => s.search_url = Some(parse_search_url(key, value).map_err(at)?),
            "scale" => s.scale = Some(Scale::parse(value).map_err(at)?),
            "color-scheme" => s.scheme = Some(appearance::Choice::parse(value).map_err(at)?),
            "force-dark" => s.force_dark = Some(parse_bool(key, value).map_err(at)?),
            "alpha" => s.alpha = Some(appearance::Alpha::parse(key, value).map_err(at)?),
            "engine" => s.engine = Some(PathBuf::from(value)),
            "engine-arg" => s
                .engine_args
                .push(parse_engine_arg(key, value).map_err(at)?),
            "user-agent" => s.user_agent = Some(value.to_string()),
            "proxy" => s.proxy = Some(parse_proxy(key, value).map_err(at)?),
            "mute" => s.mute = Some(parse_bool(key, value).map_err(at)?),
            "restore" => s.restore = Some(parse_bool(key, value).map_err(at)?),
            "normal-mode" => s.normal_mode = Some(parse_bool(key, value).map_err(at)?),
            "tmux" => s.tmux = Some(route::Choice::parse(value).map_err(at)?),
            "frames" => s.frames = Some(route::Frames::parse(value).map_err(at)?),
            "fps" => s.fps = Some(route::parse_fps(key, value).map_err(at)?),
            "probe" => s.probe = Some(parse_bool(key, value).map_err(at)?),
            "file-picker" => s.file_picker = Some(picker::Command::parse(key, value).map_err(at)?),
            "file-picker-multiple" => {
                s.file_picker_multiple = Some(picker::Command::parse(key, value).map_err(at)?)
            }
            "file-picker-terminal" => {
                s.file_picker_terminal = Some(picker::Command::parse(key, value).map_err(at)?)
            }
            "file-picker-terminal-multiple" => {
                s.file_picker_terminal_multiple =
                    Some(picker::Command::parse(key, value).map_err(at)?)
            }
            "block-list" => s.block_lists.push(PathBuf::from(value)),
            "block" => s.block = Some(parse_bool(key, value).map_err(at)?),
            "sites-dir" => s.sites_dir = Some(PathBuf::from(value)),
            "sites" => s.sites = Some(parse_bool(key, value).map_err(at)?),
            "console" => s.console = Some(parse_bool(key, value).map_err(at)?),
            "password-command" => {
                s.password_command = Some(picker::Command::parse(key, value).map_err(at)?)
            }
            "password-command-terminal" => {
                s.password_command_terminal = Some(picker::Command::parse(key, value).map_err(at)?)
            }
            "external-browser" => {
                s.external_browser = Some(picker::Command::parse(key, value).map_err(at)?)
            }
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
        pdf_paper: s.pdf_paper,
        search_url: s.search_url,
        scale: s.scale.unwrap_or(Scale::Auto),
        scheme: s.scheme.unwrap_or_default(),
        force_dark: s.force_dark.unwrap_or(false),
        alpha: s.alpha.unwrap_or_default(),
        engine: engine::Launch {
            path: s.engine,
            args: s.engine_args,
            user_agent: s.user_agent,
            proxy: s.proxy,
            mute: s.mute.unwrap_or(false),
        },
        restore: s.restore.unwrap_or(false),
        normal_mode: s.normal_mode.unwrap_or(false),
        bindings: Bindings::from_rows(s.bindings),
        route: route::Choices {
            tmux: s.tmux.unwrap_or_default(),
            frames: s.frames.unwrap_or_default(),
            fps: s.fps,
            probe: s.probe.unwrap_or(true),
        },
        pickers: picker::Pickers {
            gui: s.file_picker,
            gui_multiple: s.file_picker_multiple,
            terminal: s.file_picker_terminal,
            terminal_multiple: s.file_picker_terminal_multiple,
        },
        remote: s.remote.unwrap_or(false),
        block: block::Lists {
            paths: s.block_lists,
            enabled: s.block.unwrap_or(true),
        },
        sites: sites::Location {
            dir: s.sites_dir,
            enabled: s.sites.unwrap_or(true),
        },
        logins: login::Programs {
            gui: s.password_command,
            terminal: s.password_command_terminal,
        },
        external_browser: s.external_browser,
        console: s.console.unwrap_or(true),
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
    let settings = parse_config_bytes(&path, &bytes)?;
    provenance.found = true;
    provenance.settings = count_settings(&String::from_utf8_lossy(&bytes));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    Ok((home_expanded(settings, home.as_deref()), provenance))
}

/// A file's settings with `~/` made `$HOME`'s wherever a path or a program
/// is: see [`expand_home`]. The command line's are left alone, because the
/// shell has already done it for them — and did not, on purpose, for one
/// that was quoted.
fn home_expanded(mut settings: Settings, home: Option<&Path>) -> Settings {
    settings.profile = settings.profile.map(|choice| match choice {
        profile::Choice::At(dir) => profile::Choice::At(expand_home(dir, home)),
        other => other,
    });
    settings.download_dir = settings.download_dir.map(|dir| expand_home(dir, home));
    settings.engine = settings.engine.map(|path| expand_home(path, home));
    settings.sites_dir = settings.sites_dir.map(|dir| expand_home(dir, home));
    settings.block_lists = std::mem::take(&mut settings.block_lists)
        .into_iter()
        .map(|path| expand_home(path, home))
        .collect();
    for command in [
        &mut settings.file_picker,
        &mut settings.file_picker_multiple,
        &mut settings.file_picker_terminal,
        &mut settings.file_picker_terminal_multiple,
        &mut settings.password_command,
        &mut settings.password_command_terminal,
        &mut settings.external_browser,
    ] {
        *command = command.take().map(|command| command.expand_home(home));
    }
    settings
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
        assert!(!options.remote);
    }

    #[test]
    fn remote_is_a_flag_that_needs_a_url_and_a_profile_somebody_else_could_be_on() {
        let options = resolved(&["--remote", "a.example", "b.example"]).expect("a sender");
        assert!(options.remote);
        assert_eq!(options.urls, ["a.example", "b.example"]);
        let options = resolved(&["--profile", "/p", "--remote", "a.example"]).expect("sender");
        assert_eq!(options.profile, Choice::At(PathBuf::from("/p")));
        let why = parsed(&["--remote"]).unwrap_err();
        assert!(why.contains("needs a url"), "{why}");
        let why = parsed(&["--remote", "--temp-profile", "a.example"]).unwrap_err();
        assert!(why.contains("contradiction"), "{why}");
        assert_eq!(
            parsed(&["--remote", "--remote", "a.example"]),
            Err("--remote once is enough".to_string())
        );
        assert!(parsed(&["--remote=yes", "a.example"]).is_err());
        // A temporary profile from the file is not the command line's
        // contradiction: the run starts as usual, and listens nowhere.
        let options = resolve(
            parsed(&["--remote", "a.example"]).expect("sender"),
            Settings::default(),
            file("temp-profile = true").expect("a file"),
        )
        .expect("folded");
        assert!(options.remote);
        assert_eq!(options.profile, Choice::Temporary);
    }

    #[test]
    fn remote_is_off_unless_asked() {
        assert!(!resolved(&["a.example"]).expect("a page").remote);
        assert!(
            file("remote = true").is_err(),
            "a file cannot make a sender"
        );
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
    fn mute_is_a_flag_with_nothing_after_it_and_a_file_boolean() {
        let s = parsed(&["--mute", "example.com"]).expect("a flag");
        assert_eq!(s.mute, Some(true));
        assert_eq!(s.urls, ["example.com"], "what follows is the page");
        let why = parsed(&["--mute=yes"]).expect_err("refused");
        assert!(why.contains("--mute=yes"), "{why}");
        assert_eq!(
            parsed(&["--mute", "--mute"]),
            Err("--mute once is enough".to_string())
        );
        assert_eq!(file("mute = true").map(|s| s.mute), Ok(Some(true)));
        assert_eq!(
            file("mute = loud"),
            Err("/c:1: mute is true or false, not \"loud\"".to_string())
        );
        let options = resolve(
            Settings::default(),
            Settings::default(),
            Settings::default(),
        )
        .expect("the defaults");
        assert!(!options.engine.mute, "sound is on unless asked");
        let options = resolve(
            Settings::default(),
            Settings::default(),
            file("mute = true").expect("a file"),
        )
        .expect("folded");
        assert!(options.engine.mute, "the file's word reaches the launch");
    }

    #[test]
    fn alpha_is_on_off_or_an_amount_from_either_source() {
        use appearance::Alpha;
        let s = parsed(&["--alpha", "example.com"]).expect("a flag");
        assert_eq!(s.alpha, Some(Alpha::On(100)), "alone, it is 100");
        assert_eq!(s.urls, ["example.com"], "what follows is the page");
        let s = parsed(&["--alpha", "70", "example.com"]).expect("an amount");
        assert_eq!(s.alpha, Some(Alpha::On(70)));
        assert_eq!(s.urls, ["example.com"], "the amount is not a page");
        assert_eq!(
            parsed(&["--alpha=70"]).map(|s| s.alpha),
            Ok(Some(Alpha::On(70)))
        );
        assert_eq!(
            parsed(&["--alpha", "true"]).map(|s| s.alpha),
            Ok(Some(Alpha::On(100)))
        );
        assert_eq!(
            parsed(&["--alpha=false"]).map(|s| s.alpha),
            Ok(Some(Alpha::Off))
        );
        for bad in [&["--alpha", "0"][..], &["--alpha=101"], &["--alpha=yes"]] {
            let why = parsed(bad).expect_err("refused");
            assert!(why.contains("--alpha"), "{bad:?}: {why}");
        }
        assert_eq!(
            parsed(&["--alpha", "200"]),
            Err("--alpha is true, false or a number from 1 to 100, not \"200\"".to_string()),
            "refused by name rather than opened as a page"
        );
        assert_eq!(
            parsed(&["--alpha", "--alpha"]),
            Err("--alpha once is enough".to_string())
        );
        let s = parsed(&["--alpha", "--", "80"]).expect("-- ends the options");
        assert_eq!(s.alpha, Some(Alpha::On(100)));
        assert_eq!(s.urls, ["80"]);
        assert_eq!(
            parsed(&["--alpha", "-5"]),
            Err("unknown option: -5".to_string())
        );

        assert_eq!(file("alpha = 70").map(|s| s.alpha), Ok(Some(Alpha::On(70))));
        assert_eq!(
            file("alpha = true").map(|s| s.alpha),
            Ok(Some(Alpha::On(100)))
        );
        assert_eq!(
            file("alpha = loud"),
            Err("/c:1: alpha is true, false or a number from 1 to 100, not \"loud\"".to_string())
        );
        let options = resolved(&[]).expect("the defaults");
        assert_eq!(
            options.alpha,
            Alpha::Off,
            "the engine paints its white unless asked"
        );
        let options = resolve(
            Settings::default(),
            Settings::default(),
            file("alpha = 70").expect("a file"),
        )
        .expect("folded");
        assert_eq!(
            options.alpha,
            Alpha::On(70),
            "the file's word reaches the options"
        );
        let options = resolve(
            parsed(&["--alpha=false"]).expect("cli"),
            Settings::default(),
            file("alpha = 70").expect("a file"),
        )
        .expect("folded");
        assert_eq!(options.alpha, Alpha::Off, "the command line turns it off");
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
    fn pdf_paper_comes_from_the_flag_or_the_file_and_the_flag_wins() {
        for args in [&["--pdf-paper", "letter"][..], &["--pdf-paper=Letter"][..]] {
            assert_eq!(
                resolved(args).map(|o| o.pdf_paper),
                Ok(Some(save::Paper::Letter))
            );
        }
        assert_eq!(resolved(&[]).map(|o| o.pdf_paper), Ok(None), "the locale's");
        assert_eq!(
            parsed(&["--pdf-paper", "a4", "--pdf-paper", "a4"]),
            Err("--pdf-paper once is enough".to_string())
        );
        assert_eq!(
            parsed(&["--pdf-paper"]),
            Err("--pdf-paper is a4 or letter, not \"\"".to_string())
        );
        assert_eq!(
            file("pdf-paper = a5"),
            Err("/c:1: --pdf-paper is a4 or letter, not \"a5\"".to_string())
        );
        let from_file = file("pdf-paper = a4").expect("file");
        assert_eq!(from_file.pdf_paper, Some(save::Paper::A4));
        let cli = parsed(&["--pdf-paper=letter"]).expect("cli");
        let options = resolve(cli, Settings::default(), from_file.clone()).expect("resolves");
        assert_eq!(options.pdf_paper, Some(save::Paper::Letter));
        let options =
            resolve(Settings::default(), Settings::default(), from_file).expect("resolves");
        assert_eq!(options.pdf_paper, Some(save::Paper::A4));
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
    fn a_key_line_becomes_a_binding_in_file_order() {
        let s = file("key.ctrl+b = back\n# a comment\nkey.alt+w = none\nkey.ctrl+= = zoom-in")
            .expect("three bindings");
        assert_eq!(
            s.bindings,
            vec![
                Binding::parse("ctrl+b", "back").expect("a row"),
                Binding::parse("alt+w", "none").expect("a row"),
                Binding::parse("ctrl+=", "zoom-in").expect("a row"),
            ]
        );
        assert_eq!(s.bindings[2].action, Some(Action::ZoomIn));
        assert_eq!(
            s.bindings[2].chord,
            Chord::parse("ctrl+=").expect("the chord that ends in =")
        );
    }

    #[test]
    fn a_normal_line_is_refused_with_the_sentence_that_says_it_is_not_yet() {
        for line in ["normal.j = x", "normal = x"] {
            assert_eq!(
                file(line),
                Err(format!("/c:1: {NORMAL_NOT_REMAPPABLE_YET}")),
                "{line}"
            );
        }
        assert_eq!(
            file("normal-mode = true").map(|s| s.normal_mode),
            Ok(Some(true)),
            "the setting is not the namespace"
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
        let why = file("key.ctrl+a = bookmarks").expect_err("refused");
        assert!(
            why.starts_with("/c:1: \"bookmarks\" is not an action; they are quit, url, reload"),
            "{why}"
        );
        assert!(why.ends_with("normal-mode, tab-1..tab-8, or none"), "{why}");
        assert_eq!(
            file("# keys\n\nkey.j = back"),
            Err("/c:3: key.j: a key with no ctrl, alt or super is the page's; add one, or use f1..f24"
                .to_string())
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
    fn a_file_picker_is_a_command_from_either_source_and_each_is_its_own() {
        let words = |s: &Option<picker::Command>| s.as_ref().map(|c| c.words.clone());
        let cli = parsed(&[
            "--file-picker",
            "zenity --file-selection",
            "--file-picker-multiple=zenity --file-selection --multiple",
            "--file-picker-terminal",
            "yazi --chooser-file={out} '{dir}'",
            "--file-picker-terminal-multiple=fzf -m",
        ])
        .expect("four settings");
        assert_eq!(
            words(&cli.file_picker),
            Some(vec!["zenity".to_string(), "--file-selection".to_string()])
        );
        assert_eq!(words(&cli.file_picker_multiple).map(|w| w.len()), Some(3));
        assert_eq!(
            words(&cli.file_picker_terminal),
            Some(vec![
                "yazi".to_string(),
                "--chooser-file={out}".to_string(),
                "{dir}".to_string()
            ])
        );
        assert_eq!(
            words(&cli.file_picker_terminal_multiple).map(|w| w.len()),
            Some(2)
        );

        assert_eq!(
            parsed(&["--file-picker=a", "--file-picker=b"]),
            Err("--file-picker once is enough".to_string())
        );
        assert_eq!(
            parsed(&["--file-picker"]),
            Err("--file-picker needs a command: --file-picker <command>".to_string())
        );
        assert_eq!(
            parsed(&["--file-picker-terminal="]),
            Err(
                "--file-picker-terminal needs a command: --file-picker-terminal <command>"
                    .to_string()
            )
        );
        assert_eq!(
            parsed(&["--file-picker", "  "]),
            Err("--file-picker needs a command".to_string())
        );
        assert_eq!(
            parsed(&["--file-picker-multiple", "pick 'x"]),
            Err("--file-picker-multiple has a quote that is never closed".to_string())
        );

        let s = file(
            "file-picker = osascript -e 'POSIX path of (choose file)'\n\
             file-picker-multiple = kdialog --getopenfilename --multiple {dir}\n\
             file-picker-terminal = kitten choose-files --write-output-to={out}\n\
             file-picker-terminal-multiple = fzf -m",
        )
        .expect("a file");
        assert_eq!(
            words(&s.file_picker),
            Some(vec![
                "osascript".to_string(),
                "-e".to_string(),
                "POSIX path of (choose file)".to_string()
            ])
        );
        assert!(s.file_picker_multiple.is_some());
        assert!(s.file_picker_terminal.expect("set").has_out());
        assert!(s.file_picker_terminal_multiple.is_some());
        assert_eq!(
            file("scale = 2\nfile-picker = pick \"x"),
            Err("/c:2: file-picker has a quote that is never closed".to_string())
        );
        assert_eq!(
            file("file-picker = a\nfile-picker = b"),
            Err("/c:2: file-picker is already set on line 1".to_string())
        );
    }

    #[test]
    fn the_command_line_picker_wins_and_the_file_fills_the_others() {
        let from_file = file("file-picker = zenity --file-selection\nfile-picker-terminal = fzf")
            .expect("a file");
        let cli = parsed(&["--file-picker=kdialog --getopenfilename"]).expect("cli");
        let options = resolve(cli, Settings::default(), from_file).expect("resolves");
        assert_eq!(
            options.pickers.gui.map(|c| c.words),
            Some(vec!["kdialog".to_string(), "--getopenfilename".to_string()])
        );
        assert_eq!(
            options.pickers.terminal.map(|c| c.words),
            Some(vec!["fzf".to_string()])
        );
        assert_eq!(options.pickers.gui_multiple, None);
        assert!(resolved(&[]).expect("resolves").pickers.is_empty());
    }

    #[test]
    fn the_password_commands_are_read_from_the_line_and_the_file() {
        let words = |c: Option<picker::Command>| c.map(|c| c.words);
        let cli = parsed(&[
            "--password-command",
            "pass show web/{domain}",
            "--password-command-terminal=sh -c 'pass ls | fzf'",
        ])
        .expect("cli");
        assert_eq!(
            words(cli.password_command.clone()),
            Some(vec![
                "pass".to_string(),
                "show".to_string(),
                "web/{domain}".to_string()
            ])
        );
        assert_eq!(
            words(cli.password_command_terminal.clone()),
            Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "pass ls | fzf".to_string()
            ])
        );
        assert_eq!(
            parsed(&["--password-command=a", "--password-command=b"]),
            Err("--password-command once is enough".to_string())
        );
        assert_eq!(
            parsed(&["--password-command"]),
            Err("--password-command needs a command: --password-command <command>".to_string())
        );
        assert_eq!(
            parsed(&["--password-command-terminal", "x 'y"]),
            Err("--password-command-terminal has a quote that is never closed".to_string())
        );

        let from_file = home_expanded(
            file(
                "password-command = ~/bin/login {host}\n\
                 password-command-terminal = rbw get --full {host}",
            )
            .expect("a file"),
            Some(Path::new("/h")),
        );
        assert_eq!(
            words(from_file.password_command.clone()),
            Some(vec!["/h/bin/login".to_string(), "{host}".to_string()])
        );
        assert_eq!(
            file("password-command = a\npassword-command = b"),
            Err("/c:2: password-command is already set on line 1".to_string())
        );

        // The line's wins, the file fills the other.
        let options = resolve(
            parsed(&["--password-command=rbw get {host}"]).expect("cli"),
            Settings::default(),
            from_file,
        )
        .expect("resolves");
        assert_eq!(
            words(options.logins.gui),
            Some(vec![
                "rbw".to_string(),
                "get".to_string(),
                "{host}".to_string()
            ])
        );
        assert_eq!(words(options.logins.terminal).map(|w| w.len()), Some(4));
        assert!(resolved(&[]).expect("resolves").logins.is_empty());
    }

    #[test]
    fn the_external_browser_is_read_from_the_line_and_the_file() {
        let words = |c: Option<picker::Command>| c.map(|c| c.words);
        let strings = |w: &[&str]| Some(w.iter().map(|w| w.to_string()).collect::<Vec<_>>());
        let cli = parsed(&["--external-browser", "firefox --new-tab {url}"]).expect("cli");
        assert_eq!(
            words(cli.external_browser),
            strings(&["firefox", "--new-tab", "{url}"])
        );
        let cli = parsed(&["--external-browser=open -a 'Google Chrome'"]).expect("cli");
        assert_eq!(
            words(cli.external_browser),
            strings(&["open", "-a", "Google Chrome"])
        );
        assert_eq!(
            parsed(&["--external-browser=a", "--external-browser=b"]),
            Err("--external-browser once is enough".to_string())
        );
        assert_eq!(
            parsed(&["--external-browser"]),
            Err("--external-browser needs a command: --external-browser <command>".to_string())
        );
        assert_eq!(
            parsed(&["--external-browser", "x 'y"]),
            Err("--external-browser has a quote that is never closed".to_string())
        );

        let from_file = home_expanded(
            file("external-browser = ~/bin/open-it {url}").expect("a file"),
            Some(Path::new("/h")),
        );
        assert_eq!(
            words(from_file.external_browser.clone()),
            strings(&["/h/bin/open-it", "{url}"])
        );
        assert_eq!(
            file("external-browser = a\nexternal-browser = b"),
            Err("/c:2: external-browser is already set on line 1".to_string())
        );

        // The line's wins over the file's.
        let options = resolve(
            parsed(&["--external-browser=chromium"]).expect("cli"),
            Settings::default(),
            from_file.clone(),
        )
        .expect("resolves");
        assert_eq!(words(options.external_browser), strings(&["chromium"]));
        let options =
            resolve(parsed(&[]).expect("cli"), Settings::default(), from_file).expect("resolves");
        assert_eq!(
            words(options.external_browser),
            strings(&["/h/bin/open-it", "{url}"])
        );
        assert_eq!(resolved(&[]).expect("resolves").external_browser, None);
    }

    #[test]
    fn a_tilde_starting_a_picker_from_the_file_is_home_and_from_the_command_line_is_not() {
        let home = Some(Path::new("/h"));
        let from_file = home_expanded(
            file("file-picker-terminal = ~/bin/pick ~/x\nprofile = ~/p").expect("a file"),
            home,
        );
        assert_eq!(
            from_file.file_picker_terminal.map(|c| c.words),
            Some(vec!["/h/bin/pick".to_string(), "~/x".to_string()])
        );
        assert_eq!(from_file.profile, Some(Choice::At(PathBuf::from("/h/p"))));
        // What `invocation` does with the command line is nothing: the
        // shell has had its chance.
        let cli = parsed(&["--file-picker-terminal", "~/bin/pick"]).expect("cli");
        let options = resolve(cli, Settings::default(), Settings::default()).expect("resolves");
        assert_eq!(
            options.pickers.terminal.map(|c| c.words),
            Some(vec!["~/bin/pick".to_string()])
        );
    }

    #[test]
    fn block_lists_accumulate_the_files_first_and_no_block_turns_them_off() {
        let s = parsed(&["--block-list", "/a", "--block-list=/b"]).expect("repeatable");
        assert_eq!(s.block_lists, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(
            parsed(&["--block-list"]),
            Err("--block-list needs a path: --block-list <path>".to_string())
        );
        let from_file = file("block-list = /f1\nblock-list = /f2").expect("repeatable");
        let options = resolve(s, Settings::default(), from_file).expect("resolves");
        assert_eq!(
            options.block,
            block::Lists {
                paths: ["/f1", "/f2", "/a", "/b"].map(PathBuf::from).to_vec(),
                enabled: true,
            }
        );

        let off = parsed(&["--no-block"]).expect("a flag");
        assert_eq!(off.block, Some(false));
        assert_eq!(
            parsed(&["--no-block", "--no-block"]),
            Err("--no-block once is enough".to_string())
        );
        let options = resolve(
            off,
            Settings::default(),
            file("block = true").expect("a file"),
        )
        .expect("resolves");
        assert!(!options.block.enabled, "the command line's word wins");
        assert!(resolved(&[]).expect("resolves").block.enabled);
        assert_eq!(file("block = false").map(|s| s.block), Ok(Some(false)));
        assert_eq!(
            file("block = true\nblock = false"),
            Err("/c:2: block is already set on line 1".to_string())
        );

        let home = Some(Path::new("/h"));
        let expanded = home_expanded(file("block-list = ~/lists/hosts").expect("a file"), home);
        assert_eq!(expanded.block_lists, [PathBuf::from("/h/lists/hosts")]);
    }

    #[test]
    fn a_sites_directory_comes_from_either_source_no_sites_turns_it_off_and_the_command_line_wins()
    {
        let s = parsed(&["--sites-dir", "/a"]).expect("a directory");
        assert_eq!(s.sites_dir, Some(PathBuf::from("/a")));
        let s = parsed(&["--sites-dir=/b"]).expect("the = spelling");
        assert_eq!(s.sites_dir, Some(PathBuf::from("/b")));
        assert_eq!(
            parsed(&["--sites-dir"]),
            Err("--sites-dir needs a directory: --sites-dir <dir>".to_string())
        );
        assert_eq!(
            parsed(&["--sites-dir", "/a", "--sites-dir", "/b"]),
            Err("one sites directory at a time".to_string())
        );
        let options = resolve(
            parsed(&["--sites-dir", "/cli"]).expect("cli"),
            Settings::default(),
            file("sites-dir = /file").expect("a file"),
        )
        .expect("resolves");
        assert_eq!(
            options.sites,
            sites::Location {
                dir: Some(PathBuf::from("/cli")),
                enabled: true,
            },
            "the command line's directory wins"
        );

        let off = parsed(&["--no-sites"]).expect("a flag");
        assert_eq!(off.sites, Some(false));
        assert_eq!(
            parsed(&["--no-sites", "--no-sites"]),
            Err("--no-sites once is enough".to_string())
        );
        let options = resolve(
            off,
            Settings::default(),
            file("sites = true").expect("a file"),
        )
        .expect("resolves");
        assert!(!options.sites.enabled, "the command line's word wins");
        assert_eq!(file("sites = false").map(|s| s.sites), Ok(Some(false)));
        let maybe = file("sites = maybe").expect_err("not a bool");
        assert!(maybe.starts_with("/c:1: "), "{maybe}");
        assert_eq!(
            file("sites = true\nsites = false"),
            Err("/c:2: sites is already set on line 1".to_string())
        );
        assert_eq!(
            resolved(&[]).expect("resolves").sites,
            sites::Location {
                dir: None,
                enabled: true,
            }
        );

        let home = Some(Path::new("/h"));
        let expanded = home_expanded(file("sites-dir = ~/my-sites").expect("a file"), home);
        assert_eq!(expanded.sites_dir, Some(PathBuf::from("/h/my-sites")));
    }

    #[test]
    fn the_console_is_listened_to_unless_no_console_or_the_file_says_not() {
        assert!(resolved(&[]).expect("resolves").console);
        let off = parsed(&["--no-console"]).expect("a flag");
        assert_eq!(off.console, Some(false));
        assert_eq!(
            parsed(&["--no-console", "--no-console"]),
            Err("--no-console once is enough".to_string())
        );
        let options = resolve(
            off,
            Settings::default(),
            file("console = true").expect("a file"),
        )
        .expect("resolves");
        assert!(!options.console, "the command line's word wins");
        assert_eq!(file("console = false").map(|s| s.console), Ok(Some(false)));
        let options = resolve(
            Settings::default(),
            Settings::default(),
            file("console = false").expect("a file"),
        )
        .expect("resolves");
        assert!(!options.console);
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
             user-agent = fa\nproxy = f:1\nrestore = false\nnormal-mode = false\n\
             alpha = false",
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
            "--alpha",
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
        assert_eq!(options.alpha, appearance::Alpha::On(100));
        assert_eq!(options.engine.path, Some(PathBuf::from("/ce")));
        assert_eq!(options.engine.user_agent.as_deref(), Some("ca"));
        assert_eq!(options.engine.proxy.as_deref(), Some("c:1"));
        assert!(options.restore);
        assert!(options.normal_mode);
    }

    #[test]
    fn the_file_fills_what_the_command_line_did_not_say() {
        let from_file =
            file("scale = 2\ncolor-scheme = dark\nhome = h.example\nalpha = true").expect("file");
        let cli = parsed(&["--force-dark", "a.example"]).expect("cli");
        let options = resolve(cli, Settings::default(), from_file).expect("resolves");
        assert_eq!(options.scale, Scale::Fixed(2.0));
        assert_eq!(options.scheme, appearance::Choice::Dark);
        assert_eq!(options.home, "h.example");
        assert!(options.force_dark);
        assert_eq!(options.alpha, appearance::Alpha::On(100));
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

    #[test]
    fn tmux_frames_fps_and_no_probe_are_parsed_from_the_line_and_the_file_and_refused_twice() {
        let s = parsed(&["--tmux", "on", "--frames=png", "--fps", "7", "--no-probe"])
            .expect("four settings");
        assert_eq!(s.tmux, Some(route::Choice::On));
        assert_eq!(s.frames, Some(route::Frames::Png));
        assert_eq!(s.fps, Some(7));
        assert_eq!(s.probe, Some(false));
        for twice in [
            &["--tmux=on", "--tmux=off"][..],
            &["--frames=raw", "--frames", "png"][..],
            &["--fps=1", "--fps=2"][..],
            &["--no-probe", "--no-probe"][..],
        ] {
            let why = parsed(twice).expect_err("twice");
            assert!(why.contains("once is enough"), "{twice:?}: {why}");
        }
        assert!(parsed(&["--no-probe=true"]).is_err());

        let from_file = file("tmux = off\nframes = raw\nfps = 20\nprobe = false").expect("file");
        let options = resolve(Settings::default(), Settings::default(), from_file).expect("ok");
        assert_eq!(
            options.route,
            route::Choices {
                tmux: route::Choice::Off,
                frames: route::Frames::Raw,
                fps: Some(20),
                probe: false,
            }
        );
        let cli = parsed(&["--tmux=on", "--fps=30"]).expect("cli");
        let from_file = file("tmux = off\nfps = 20").expect("file");
        let options = resolve(cli, Settings::default(), from_file).expect("ok");
        assert_eq!(options.route.tmux, route::Choice::On, "the line wins");
        assert_eq!(options.route.fps, Some(30));
        assert!(options.route.probe, "probing is the default");
        assert_eq!(
            file("tmux = on\ntmux = off"),
            Err("/c:2: tmux is already set on line 1".to_string())
        );
    }

    #[test]
    fn fps_outside_one_to_sixty_is_refused_by_name() {
        for bad in ["0", "61", "fast", "2.5"] {
            let why = parsed(&["--fps", bad]).expect_err(bad);
            assert!(why.contains("--fps"), "{bad}: {why}");
        }
        assert!(parsed(&["--fps"]).is_err());
        let why = file("fps = 90").expect_err("too many");
        assert!(why.starts_with("/c:1: fps is"), "{why}");
        assert!(parsed(&["--tmux=maybe"]).unwrap_err().contains("--tmux"));
        assert!(parsed(&["--frames=jpeg"]).unwrap_err().contains("--frames"));
        assert_eq!(
            resolved(&[]).expect("defaults").route,
            route::Choices::default()
        );
    }
}
