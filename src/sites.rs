//! Per-site user styles and user scripts: a file of CSS or JavaScript named
//! after a host, put on every page of that host.
//!
//! A terminal pane is not the window a site was designed for. It is narrow,
//! its font is somebody else's, and a cookie banner or a sticky header that
//! is a strip on a desktop is half of it. A browser answers that with an
//! extension (Stylus, Violentmonkey); this program has none, so it reads a
//! directory of plain files instead and tells every page's session about
//! them.
//!
//! # The directory, and the names in it
//!
//! `$XDG_CONFIG_HOME/blinkterm/sites/`, else `~/.config/blinkterm/sites/`,
//! beside the settings file ([`default_dir`]); `--sites-dir <dir>` or
//! `sites-dir = <dir>` names another, and `--no-sites` or `sites = false`
//! reads none. A missing default directory is no files; a named directory
//! that is missing is a sentence in the shell, as a missing `--block-list`
//! is. Each file is `<pattern>.css` or `<pattern>.js`, and the pattern is
//! ([`Pattern`]):
//!
//! | name | the pages it is put on |
//! | --- | --- |
//! | `all.css` | every page, a `data:` page and `about:blank` included |
//! | `example.com.css` | `example.com` and no other host |
//! | `*.example.com.css` | `example.com` and every host under it |
//!
//! `*.` reads a host the way [`crate::block`] reads a listed one:
//! `*.example.com` is `www.example.com` and `a.b.example.com`, and not
//! `notexample.com`. Names are lower-cased for matching. A name that is not
//! a host — anything but letters, digits, `.` and `-` after an optional `*.`
//! — is refused with a sentence naming it; a file that ends in neither
//! `.css` nor `.js` (an editor's `x.css.swp`), a dotfile (`.DS_Store`) and a
//! subdirectory are passed over without one. Hosts only: paths and IPv6
//! literals are not patterns.
//!
//! # The order
//!
//! When several files fit a page, `all` comes first, then the `*.` patterns
//! with the fewest labels, then the exact host, and files of the same rank
//! by name ([`Pattern::rank`]). A style later in that order wins by the
//! cascade and a script later in it runs later, so the most specific file
//! has the last word.
//!
//! # How a style gets there
//!
//! Every `.css` file goes in one table, `[[pattern, css], …]`, carried by
//! one script ([`Sites::style_params`]) that is registered with
//! `Page.addScriptToEvaluateOnNewDocument` once per session. At the start of
//! every document it reads the page's host, picks the rows that fit it and
//! adopts one constructed `CSSStyleSheet` for each: the technique
//! [`crate::appearance`] uses for `--alpha`, and for the same reasons — a
//! constructed sheet is not subject to a page's CSP `style-src`, the DOM is
//! not changed, the script runs in every same-process frame's new document,
//! and with `runImmediately` a page already loaded changes where it stands.
//! CDP's command has no url filter, so the match has to be the page's own at
//! document start; [`Pattern::matches`] is its Rust mirror. The sheets it
//! adopted are remembered on its world's global, so that when it runs on the
//! same document again — `reload-sites` — it replaces them rather than
//! adding a second set.
//!
//! The host is `location.hostname`, and where that is empty — an
//! `about:blank` or `srcdoc` frame, which carries its parent's base url —
//! the host of `document.baseURI`, so such a frame gets its page's styles.
//!
//! # How a script gets there
//!
//! One registration per `.js` file ([`Sites::script_params`]), wrapped in a
//! function that checks the host first. One each so that a syntax error in
//! one file is that file's alone, and so that each can choose its world:
//!
//! - by default the isolated world [`WORLD`], which shares the page's DOM
//!   and not its JavaScript — the page sees the elements a script changes,
//!   and nothing of its variables, which is where a script that only hides
//!   and restyles belongs;
//! - with `// @world main` as the file's first line ([`MAIN_WORLD_LINE`]),
//!   the page's own, where it can call the page's functions and the page
//!   can see it.
//!
//! The file is spliced into the source as code, not as a string: its
//! top-level declarations are the wrapper's, as they would be in a
//! userscript manager, and `this` is the world's global.
//!
//! It runs at the start of the document, before any of the page's own
//! scripts — and so before the page's elements exist: on every document but
//! a new session's first, `document.documentElement` is still `null` when it
//! runs (measured; the first one's context is made late, which hides it). A
//! script that changes the page waits for `DOMContentLoaded`; one that only
//! sets something up — a global in the page's world, a listener — need not.
//!
//! A script is run on a document already there only when the session is
//! new, so that a popup's first document is not missed. Unlike a sheet a
//! script cannot be adopted and taken back, and running it again on a live
//! document would double whatever it set up, so on `reload-sites` and after a
//! crash a script waits for the next document, while the styles change at
//! once.
//!
//! # Reload
//!
//! `alt+shift+r` (`reload-sites`) reads the directory again, and every tab that
//! can answer is told: `Page.removeScriptToEvaluateOnNewDocument` for every
//! identifier the tab was given ([`remove`]), then the new set ([`install`]).
//! By identifier, because that is the only handle the engine gives on a
//! registration, and because a session keeps what it was told for its whole
//! life. A tab stopped behind a dialog or crashed is left with the old files,
//! and the row says so.
//!
//! # What a page can see
//!
//! Of a style: `document.adoptedStyleSheets` one entry longer per file that
//! fits, and nothing in `document.styleSheets` or on `window`. Of a script in
//! [`WORLD`]: what it does to the DOM, and nothing else. A `// @world main`
//! script is the page's own code to the page.
//!
//! What is not reached, documented rather than defended: an iframe from
//! another site that the engine runs in a process of its own, which nothing
//! is attached to; a page that assigns `document.adoptedStyleSheets`
//! wholesale, dropping the sheets; and a page's `!important` in an inline
//! `style` attribute, which beats any sheet.
//!
//! # What is refused
//!
//! This program writes nothing here. It reads the files as they are, but
//! refuses a file, or the whole directory, that group or others can write
//! (`mode & 0o022`): a site script runs with the page's powers, and a file
//! another user can change is a way into every page. A file larger than
//! [`MAX_FILE_BYTES`] or not UTF-8 is refused too. Each refusal is one
//! sentence ([`Sites::skipped`]), said in the shell at start and on the row
//! after `alt+shift+r`.
//!
//! # Measured
//!
//! Against `chrome-headless-shell` 153 over the pipe (`tests/engine.rs`):
//!
//! | step | result |
//! | --- | --- |
//! | a host's style, on load, after a navigation, after `Page.reload` | applied |
//! | another host, a `data:` page | only `all` |
//! | `document.styleSheets`, `window.__blinktermSiteSheets` from the page | unchanged, `undefined` |
//! | a script at document start, before the page's inline script | ran first |
//! | a variable a [`WORLD`] script set, from the page | `undefined` |
//! | a `// @world main` script's variable, from the page | there |
//! | files registered in order | ran in order |
//! | `reload-sites` on a loaded page | new style in place, same document, one sheet |
//! | the old script after `removeScriptToEvaluateOnNewDocument` | never ran again |
//! | a same-process iframe | styled |
//! | a `srcdoc` iframe, by its parent's host | styled |
//! | a syntax error in one file | the others ran |
//! | a `// @world main` script under `script-src 'none'` | ran |
//! | five script files registered on a session | 0.5 to 10 ms, one call each |
//! | 210 KB of CSS, to `domInteractive` | 5.5 ms bare; +1 ms carried to a page it does not fit; +7 ms adopted |
//!
//! So a registration is cheap enough to be a call per file on every new tab,
//! and the size limit is about what a file is rather than what it costs: at
//! the limit a page that adopts it pays some 35 ms a document.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cdp::Client;
use crate::json::Json;

/// The directory's name under the configuration directory.
pub const DIR: &str = "sites";

/// The isolated world the styles and the default scripts run in: this
/// module's own, so that nothing is shared with [`crate::appearance`]'s.
pub const WORLD: &str = "blinkterm-sites";

/// The largest file read. A stylesheet or a userscript is kilobytes; a
/// megabyte is a file that is something else, and it would be parsed at the
/// start of every document.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;

/// A script's first line that puts it in the page's own world.
pub const MAIN_WORLD_LINE: &str = "// @world main";

/// How long a registration is waited for: the engine answers at once, and a
/// page that does not is one that is not going to.
const CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// `--sites-dir` and `--no-sites`, as the settings resolve them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// The directory named, or `None` for the default one.
    pub dir: Option<PathBuf>,
    /// `false` for `sites = false` or `--no-sites`: no files are read.
    pub enabled: bool,
}

impl Default for Location {
    fn default() -> Location {
        Location {
            dir: None,
            enabled: true,
        }
    }
}

/// Which pages a file is for: its name, less `.css` or `.js`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// `all`: every page.
    All,
    /// `example.com`: that host.
    Host(String),
    /// `*.example.com`: that host and every host under it.
    Under(String),
}

impl Pattern {
    /// A file name's stem as a pattern, lower-cased; a sentence for one that
    /// is not a host.
    pub fn parse(stem: &str) -> Result<Pattern, String> {
        let lower = stem.to_ascii_lowercase();
        if lower == "all" {
            return Ok(Pattern::All);
        }
        let (under, host) = match lower.strip_prefix("*.") {
            Some(rest) => (true, rest),
            None => (false, lower.as_str()),
        };
        let fine = !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
            && !host.starts_with('.')
            && !host.ends_with('.')
            && !host.contains("..");
        if !fine {
            return Err(format!(
                "{stem:?} is not a host: name a file <host>, *.<host> or all"
            ));
        }
        Ok(if under {
            Pattern::Under(host.to_string())
        } else {
            Pattern::Host(host.to_string())
        })
    }

    /// Where a file of this pattern comes in the order: `all` first, then
    /// `*.` patterns by how many labels they have, fewest first, then an
    /// exact host.
    pub fn rank(&self) -> (u8, usize) {
        match self {
            Pattern::All => (0, 0),
            Pattern::Under(host) => (1, host.split('.').count()),
            Pattern::Host(_) => (2, 0),
        }
    }

    /// Whether a page on `host` gets this file: the rule the scripts apply
    /// in the page (`HOST_JS`, `FITS_JS`), here for the tests and the
    /// docs. `host` is lower-cased and a trailing dot dropped, as there.
    pub fn matches(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        let host = host.strip_suffix('.').unwrap_or(&host);
        match self {
            Pattern::All => true,
            Pattern::Host(h) => host == h,
            Pattern::Under(h) => {
                host == h
                    || host
                        .strip_suffix(h.as_str())
                        .is_some_and(|rest| rest.ends_with('.'))
            }
        }
    }

    /// The pattern as a file name spells it, and as the scripts match it.
    pub fn text(&self) -> String {
        match self {
            Pattern::All => "all".to_string(),
            Pattern::Host(host) => host.clone(),
            Pattern::Under(host) => format!("*.{host}"),
        }
    }
}

/// Which world a script runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum World {
    /// [`WORLD`]: the page's DOM, not its JavaScript.
    Isolated,
    /// The page's own, asked for with [`MAIN_WORLD_LINE`].
    Main,
}

/// One `.css` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Style {
    /// The file's name, for the sentences and the order.
    pub name: String,
    pub pattern: Pattern,
    pub css: String,
}

/// One `.js` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// The file's name, for the sentences and the order.
    pub name: String,
    pub pattern: Pattern,
    pub world: World,
    pub source: String,
}

/// Everything read from the directory, in the order it is put on a page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sites {
    /// Sorted by the pattern's rank, then the name.
    styles: Vec<Style>,
    /// Sorted the same way.
    scripts: Vec<Script>,
    /// Where they were read from; `None` when nothing was looked for.
    pub dir: Option<PathBuf>,
    /// One sentence per file refused, `<name>: <why>`.
    pub skipped: Vec<String>,
}

impl Sites {
    /// No files, and none looked for: `--no-sites`.
    pub fn none() -> Sites {
        Sites::default()
    }

    /// Every file in `dir`. Never an error: a directory that is not there is
    /// no files, and a file or a directory that cannot be used is a sentence
    /// in [`Sites::skipped`].
    pub fn read(dir: &Path) -> Sites {
        let mut sites = Sites {
            dir: Some(dir.to_path_buf()),
            ..Sites::default()
        };
        let Ok(meta) = fs::metadata(dir) else {
            return sites;
        };
        if let Some(why) = writable_by_others(&meta) {
            sites.skipped.push(format!("{}: {why}", dir.display()));
            return sites;
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(why) => {
                sites
                    .skipped
                    .push(format!("{}: cannot be read: {why}", dir.display()));
                return sites;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let (stem, css) = if let Some(stem) = name.strip_suffix(".css") {
                (stem, true)
            } else if let Some(stem) = name.strip_suffix(".js") {
                (stem, false)
            } else {
                continue;
            };
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                continue;
            }
            let pattern = match Pattern::parse(stem) {
                Ok(pattern) => pattern,
                Err(why) => {
                    sites.skipped.push(format!("{name}: {why}"));
                    continue;
                }
            };
            let text = match read_file(&path, &meta) {
                Ok(text) => text,
                Err(why) => {
                    sites.skipped.push(format!("{name}: {why}"));
                    continue;
                }
            };
            if css {
                sites.styles.push(Style {
                    name,
                    pattern,
                    css: text,
                });
            } else {
                let world = if text.lines().next().map(str::trim_end) == Some(MAIN_WORLD_LINE) {
                    World::Main
                } else {
                    World::Isolated
                };
                sites.scripts.push(Script {
                    name,
                    pattern,
                    world,
                    source: text,
                });
            }
        }
        sites
            .styles
            .sort_by(|a, b| (a.pattern.rank(), &a.name).cmp(&(b.pattern.rank(), &b.name)));
        sites
            .scripts
            .sort_by(|a, b| (a.pattern.rank(), &a.name).cmp(&(b.pattern.rank(), &b.name)));
        sites.skipped.sort();
        sites
    }

    /// The styles, in order.
    pub fn styles(&self) -> &[Style] {
        &self.styles
    }

    /// The scripts, in order.
    pub fn scripts(&self) -> &[Script] {
        &self.scripts
    }

    /// Whether there is nothing to put on a page.
    pub fn is_empty(&self) -> bool {
        self.styles.is_empty() && self.scripts.is_empty()
    }

    /// What was read, for the row: `site files: 2 styles, 1 script`, or
    /// `no site files in <dir>`, and how many were refused.
    pub fn words(&self) -> String {
        let count = |n: usize, one: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {one}s")
            }
        };
        let mut out = if self.is_empty() {
            match &self.dir {
                Some(dir) => format!("no site files in {}", dir.display()),
                None => "no site files".to_string(),
            }
        } else {
            format!(
                "site files: {}, {}",
                count(self.styles.len(), "style"),
                count(self.scripts.len(), "script")
            )
        };
        if !self.skipped.is_empty() {
            out.push_str(&format!("; skipped {}", self.skipped.len()));
        }
        out
    }

    /// `Page.addScriptToEvaluateOnNewDocument`'s parameters for the one
    /// script that carries every style (`STYLE_BODY`): in [`WORLD`], and
    /// run on the document already there as well. `None` with no `.css`.
    pub fn style_params(&self) -> Option<Json> {
        if self.styles.is_empty() {
            return None;
        }
        let table = Json::Array(
            self.styles
                .iter()
                .map(|style| {
                    Json::Array(vec![
                        Json::string(style.pattern.text()),
                        Json::string(style.css.as_str()),
                    ])
                })
                .collect(),
        );
        // The table is a JSON array, which is a JavaScript literal as
        // [`Json`] writes it.
        let source = format!(
            "(() => {{\nconst table = {table};\nconst host = {HOST_JS};\nconst fits = {FITS_JS};\n{STYLE_BODY}\n}})()"
        );
        Some(Json::object(vec![
            ("source", Json::string(source)),
            ("worldName", Json::string(WORLD)),
            ("runImmediately", Json::Bool(true)),
        ]))
    }

    /// The same for each `.js` file, in order: in [`WORLD`] unless the file
    /// asked for the page's, and run on the document already there only on
    /// a `fresh` session.
    pub fn script_params(&self, fresh: bool) -> Vec<Json> {
        self.scripts
            .iter()
            .map(|script| {
                let source = format!(
                    "(() => {{\nconst host = {HOST_JS};\nconst fits = {FITS_JS};\nif (!fits({pattern})) return;\n(function () {{\n{body}\n}}).call(globalThis);\n}})()",
                    pattern = Json::string(script.pattern.text()),
                    body = script.source,
                );
                let mut fields = vec![("source", Json::string(source))];
                if script.world == World::Isolated {
                    fields.push(("worldName", Json::string(WORLD)));
                }
                fields.push(("runImmediately", Json::Bool(fresh)));
                Json::object(fields)
            })
            .collect()
    }

    /// Every registration, the styles' first and then the scripts in order.
    pub fn params(&self, fresh: bool) -> Vec<Json> {
        self.style_params()
            .into_iter()
            .chain(self.script_params(fresh))
            .collect()
    }
}

/// The page's host, as both scripts read it: `location.hostname`, or for a
/// frame that has none — `about:blank`, `srcdoc` — the host of the base url
/// it inherited; lower-cased, a trailing dot dropped.
const HOST_JS: &str = "(location.hostname || (() => { try { return new URL(document.baseURI).hostname; } catch (e) { return ''; } })()).toLowerCase().replace(/\\.$/, '')";

/// Whether a pattern fits `host`: [`Pattern::matches`], in the page.
const FITS_JS: &str = "p => p === 'all' || p === host || (p.startsWith('*.') && (host === p.slice(2) || host.endsWith(p.slice(1))))";

/// The body of the style script, after the table, the host and the rule:
/// the sheets that fit, adopted, in place of any this script adopted into
/// the same document before.
const STYLE_BODY: &str = concat!(
    "try {",
    "\nconst old = globalThis.__blinktermSiteSheets || [];",
    "\nconst mine = table.filter(([p]) => fits(p)).map(([, css]) => { const s = new CSSStyleSheet(); s.replaceSync(css); return s; });",
    "\ndocument.adoptedStyleSheets = [...document.adoptedStyleSheets.filter(s => !old.includes(s)), ...mine];",
    "\nglobalThis.__blinktermSiteSheets = mine;",
    "\n} catch (e) {}",
);

/// Why a file or a directory is not to be trusted: another user can write
/// it.
fn writable_by_others(meta: &fs::Metadata) -> Option<String> {
    let mode = meta.permissions().mode() & 0o777;
    (mode & 0o022 != 0).then(|| {
        format!("another user could change it (mode {mode:o}); chmod go-w it to have it read")
    })
}

/// A file's text, or why not.
fn read_file(path: &Path, meta: &fs::Metadata) -> Result<String, String> {
    if let Some(why) = writable_by_others(meta) {
        return Err(why);
    }
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(format!(
            "larger than {} KiB, the most a site file may be",
            MAX_FILE_BYTES / 1024
        ));
    }
    let bytes = fs::read(path).map_err(|why| format!("cannot be read: {why}"))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(format!(
            "larger than {} KiB, the most a site file may be",
            MAX_FILE_BYTES / 1024
        ));
    }
    String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_string())
}

/// The default directory: `$XDG_CONFIG_HOME/blinkterm/sites`, else
/// `~/.config/blinkterm/sites`, each only when absolute — the same reading
/// as [`crate::options::default_config_path`].
pub fn default_dir(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let usable = |value: Option<&OsStr>| {
        value
            .map(Path::new)
            .filter(|path| path.is_absolute())
            .map(Path::to_path_buf)
    };
    if let Some(config) = usable(xdg) {
        return Some(config.join("blinkterm").join(DIR));
    }
    usable(home).map(|home| home.join(".config").join("blinkterm").join(DIR))
}

/// The files `location` names: none when turned off; the named directory,
/// which has to be there; or the default one, which need not be.
pub fn load(location: &Location) -> Result<Sites, String> {
    load_with(
        location,
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// [`load`] with the environment given.
fn load_with(
    location: &Location,
    xdg: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<Sites, String> {
    if !location.enabled {
        return Ok(Sites::none());
    }
    match &location.dir {
        Some(dir) => {
            if !dir.is_dir() {
                return Err(format!("--sites-dir {}: no such directory", dir.display()));
            }
            Ok(Sites::read(dir))
        }
        None => Ok(default_dir(xdg, home)
            .map(|dir| Sites::read(&dir))
            .unwrap_or_default()),
    }
}

/// Register every file on a session, returning the identifiers the engine
/// gave, for [`remove`]. Asked rather than told, because the identifier is
/// in the reply; one that fails is left out, and the page goes without that
/// file, as a page goes without a scheme that did not take.
pub fn install(client: &mut Client, sites: &Sites, fresh: bool) -> Vec<String> {
    sites
        .params(fresh)
        .into_iter()
        .filter_map(|params| {
            client
                .call_within(
                    "Page.addScriptToEvaluateOnNewDocument",
                    params,
                    CALL_TIMEOUT,
                )
                .ok()
                .and_then(|reply| {
                    reply
                        .get("identifier")
                        .and_then(Json::as_str)
                        .map(str::to_string)
                })
        })
        .collect()
}

/// Take registrations back, so that the next document runs none of them.
/// Told: the reply says nothing, and a session that has lost them already —
/// a renderer that crashed — answers with an error nobody needs.
pub fn remove(client: &mut Client, identifiers: &[String]) {
    for identifier in identifiers {
        let _ = client.notify(
            "Page.removeScriptToEvaluateOnNewDocument",
            Json::object(vec![("identifier", Json::string(identifier.as_str()))]),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of this test's own, gone again before.
    fn scratch(what: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "blinkterm-unit-sites-{what}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch directory");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod");
        dir
    }

    fn put(dir: &Path, name: &str, text: &[u8]) {
        let path = dir.join(name);
        fs::write(&path, text).expect("a file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
    }

    fn source(params: &Json) -> &str {
        params
            .get("source")
            .and_then(Json::as_str)
            .expect("a source")
    }

    #[test]
    fn a_file_name_is_a_host_pattern_and_anything_else_is_refused_by_name() {
        assert_eq!(Pattern::parse("all"), Ok(Pattern::All));
        assert_eq!(Pattern::parse("ALL"), Ok(Pattern::All));
        assert_eq!(
            Pattern::parse("Example.COM"),
            Ok(Pattern::Host("example.com".to_string()))
        );
        assert_eq!(
            Pattern::parse("127.0.0.1"),
            Ok(Pattern::Host("127.0.0.1".to_string()))
        );
        assert_eq!(
            Pattern::parse("*.example.com"),
            Ok(Pattern::Under("example.com".to_string()))
        );
        assert_eq!(
            Pattern::parse("my-site.example"),
            Ok(Pattern::Host("my-site.example".to_string()))
        );
        for bad in [
            "", "*", "*.", "a..b", ".a", "a.", "a b", "a/b", "*.*.a", "a_b", "[::1]", "*a.com",
        ] {
            let why = Pattern::parse(bad).expect_err(bad);
            assert!(why.contains(&format!("{bad:?}")), "{why}");
            assert!(why.contains("<host>, *.<host> or all"), "{why}");
        }
    }

    #[test]
    fn patterns_are_ordered_general_first_so_the_most_specific_file_has_the_last_word() {
        let mut patterns = [
            "example.com",
            "*.a.example.com",
            "all",
            "*.example.com",
            "*.com",
        ]
        .map(|p| Pattern::parse(p).unwrap());
        patterns.sort_by_key(Pattern::rank);
        assert_eq!(
            patterns.map(|p| p.text()),
            [
                "all",
                "*.com",
                "*.example.com",
                "*.a.example.com",
                "example.com"
            ]
        );
    }

    #[test]
    fn a_pattern_matches_a_host_the_way_a_block_list_reads_one() {
        let exact = Pattern::parse("example.com").unwrap();
        assert!(exact.matches("example.com"));
        assert!(exact.matches("EXAMPLE.com."));
        assert!(!exact.matches("www.example.com"));
        let under = Pattern::parse("*.example.com").unwrap();
        assert!(under.matches("example.com"), "the bare host is included");
        assert!(under.matches("www.example.com"));
        assert!(under.matches("a.b.example.com"));
        assert!(!under.matches("notexample.com"));
        assert!(!under.matches("notads.example.com.evil"));
        assert!(!Pattern::parse("*.ads.example.com")
            .unwrap()
            .matches("notads.example.com"));
        assert!(
            Pattern::All.matches(""),
            "all is every page, a data: one too"
        );
        assert!(!exact.matches(""));
    }

    #[test]
    fn a_directory_is_read_into_styles_and_scripts_and_the_rest_is_ignored_or_skipped_with_a_sentence(
    ) {
        let dir = scratch("read");
        put(&dir, "example.com.css", b"a{}");
        put(&dir, "all.css", b"b{}");
        put(&dir, "*.example.com.css", b"c{}");
        put(&dir, "example.com.js", b"1");
        put(&dir, ".DS_Store", b"\0\0");
        put(&dir, "x.css.swp", b"swap");
        put(&dir, "notes.txt", b"hello");
        fs::create_dir_all(dir.join("sub.css")).unwrap();
        put(&dir, "latin1.example.css", b"a{content:'\xe9'}");
        put(&dir, "big.example.css", &vec![b' '; MAX_FILE_BYTES + 1]);
        put(&dir, "bad name.js", b"1");
        let sites = Sites::read(&dir);
        assert_eq!(
            sites
                .styles()
                .iter()
                .map(|s| &s.name[..])
                .collect::<Vec<_>>(),
            ["all.css", "*.example.com.css", "example.com.css"]
        );
        assert_eq!(sites.styles()[0].css, "b{}");
        assert_eq!(
            sites
                .scripts()
                .iter()
                .map(|s| &s.name[..])
                .collect::<Vec<_>>(),
            ["example.com.js"]
        );
        assert_eq!(sites.skipped.len(), 3, "{:?}", sites.skipped);
        assert!(
            sites.skipped[0].starts_with("bad name.js: "),
            "{:?}",
            sites.skipped
        );
        assert!(sites.skipped[1].starts_with("big.example.css: larger than 1024 KiB"));
        assert_eq!(sites.skipped[2], "latin1.example.css: not UTF-8 text");
        assert_eq!(sites.dir.as_deref(), Some(dir.as_path()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_script_s_first_line_chooses_the_main_world_and_is_otherwise_code() {
        let dir = scratch("world");
        put(&dir, "a.example.js", b"// @world main\nwindow.x = 1");
        put(&dir, "b.example.js", b"window.y = 1\n// @world main");
        put(&dir, "c.example.js", b"// @world main  \r\nwindow.z = 1");
        let sites = Sites::read(&dir);
        let worlds: Vec<World> = sites.scripts().iter().map(|s| s.world).collect();
        assert_eq!(worlds, [World::Main, World::Isolated, World::Main]);
        assert_eq!(sites.scripts()[0].source, "// @world main\nwindow.x = 1");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_or_a_directory_another_user_could_write_is_refused() {
        let dir = scratch("mode");
        put(&dir, "all.css", b"a{}");
        put(&dir, "all.js", b"1");
        fs::set_permissions(dir.join("all.js"), fs::Permissions::from_mode(0o666)).unwrap();
        let sites = Sites::read(&dir);
        assert_eq!(sites.styles().len(), 1);
        assert!(sites.scripts().is_empty());
        assert_eq!(sites.skipped.len(), 1);
        assert!(
            sites.skipped[0].starts_with("all.js: another user could change it (mode 666)"),
            "{:?}",
            sites.skipped
        );

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o775)).unwrap();
        let sites = Sites::read(&dir);
        assert!(sites.is_empty());
        assert_eq!(sites.skipped.len(), 1);
        assert!(
            sites.skipped[0].contains("another user could change it (mode 775)"),
            "{:?}",
            sites.skipped
        );
        assert!(sites.skipped[0].starts_with(&dir.display().to_string()));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_style_script_carries_the_table_in_order_in_the_sites_world_and_runs_at_once() {
        let dir = scratch("style");
        put(&dir, "example.com.css", b"p { color: \"red\" }\n");
        put(&dir, "all.css", b"a{}");
        let sites = Sites::read(&dir);
        let params = sites.style_params().expect("a style");
        assert_eq!(params.get("worldName").and_then(Json::as_str), Some(WORLD));
        assert_eq!(
            params.get("runImmediately").and_then(Json::as_bool),
            Some(true)
        );
        let source = source(&params);
        assert!(
            source.contains(r#"[["all","a{}"],["example.com","p { color: \"red\" }\n"]]"#),
            "{source}"
        );
        assert!(source.contains("adoptedStyleSheets"));
        assert!(source.contains("__blinktermSiteSheets"));
        assert!(sites.script_params(true).is_empty());
        assert_eq!(sites.params(false).len(), 1);
        assert_eq!(Sites::none().style_params(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_script_s_params_name_the_sites_world_unless_the_file_asked_for_the_page_s() {
        let dir = scratch("script");
        put(&dir, "all.js", b"document.title = 'x' // no newline");
        put(&dir, "*.example.com.js", b"// @world main\nwindow.y = 1");
        let sites = Sites::read(&dir);
        let fresh = sites.script_params(true);
        assert_eq!(fresh.len(), 2);
        assert_eq!(
            fresh[0].get("worldName").and_then(Json::as_str),
            Some(WORLD)
        );
        assert_eq!(fresh[1].get("worldName"), None);
        assert!(fresh
            .iter()
            .all(|p| p.get("runImmediately").and_then(Json::as_bool) == Some(true)));
        let later = sites.script_params(false);
        assert!(later
            .iter()
            .all(|p| p.get("runImmediately").and_then(Json::as_bool) == Some(false)));
        let all = source(&fresh[0]);
        assert!(all.contains("if (!fits(\"all\")) return;"), "{all}");
        assert!(
            all.contains("document.title = 'x' // no newline\n}"),
            "a trailing comment does not swallow the wrapper: {all}"
        );
        assert!(source(&fresh[1]).contains("fits(\"*.example.com\")"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_default_directory_is_no_files_and_a_named_one_that_is_missing_is_a_sentence() {
        let nowhere = std::env::temp_dir().join(format!(
            "blinkterm-unit-sites-nowhere-{}",
            std::process::id()
        ));
        let default = load_with(&Location::default(), Some(nowhere.as_os_str()), None)
            .expect("a missing default is fine");
        assert!(default.is_empty());
        assert_eq!(default.dir, Some(nowhere.join("blinkterm").join("sites")));
        let named = Location {
            dir: Some(nowhere.clone()),
            enabled: true,
        };
        assert_eq!(
            load_with(&named, None, None),
            Err(format!(
                "--sites-dir {}: no such directory",
                nowhere.display()
            ))
        );
        let off = Location {
            dir: Some(nowhere),
            enabled: false,
        };
        assert_eq!(load_with(&off, None, None), Ok(Sites::none()));
        assert_eq!(
            default_dir(None, Some(OsStr::new("/h"))),
            Some(PathBuf::from("/h/.config/blinkterm/sites"))
        );
        assert_eq!(
            default_dir(Some(OsStr::new("relative")), Some(OsStr::new("/h"))),
            Some(PathBuf::from("/h/.config/blinkterm/sites"))
        );
        assert_eq!(default_dir(None, None), None);
    }

    #[test]
    fn the_words_count_what_was_read_and_what_was_skipped() {
        let dir = scratch("words");
        assert_eq!(
            Sites::read(&dir).words(),
            format!("no site files in {}", dir.display())
        );
        put(&dir, "all.css", b"a{}");
        put(&dir, "a.example.css", b"a{}");
        put(&dir, "all.js", b"1");
        assert_eq!(Sites::read(&dir).words(), "site files: 2 styles, 1 script");
        put(&dir, "b c.js", b"1");
        assert_eq!(
            Sites::read(&dir).words(),
            "site files: 2 styles, 1 script; skipped 1"
        );
        assert_eq!(Sites::none().words(), "no site files");
        let _ = fs::remove_dir_all(&dir);
    }
}
