//! Saving what a page hands over, which a headless engine refuses to do
//! until it is told where.
//!
//! The headless shell has no download shelf and no Downloads folder: asked
//! for an attachment it answers `Page.navigate` with `net::ERR_ABORTED` and
//! `isDownload: true`, sends `Page.frameStoppedLoading`, and writes nothing.
//! Until this module that was the whole story, and the row said `loading
//! report.pdf` forever, because nothing came after that reply to take it
//! off: no `frameNavigated`, no `targetInfoChanged`, no history entry — the
//! page stays exactly where it was, which is the right thing for a page and
//! the wrong thing for a note that says it is going somewhere. [`became_download`]
//! is how [`crate::app::navigated`] now tells that reply from a failure, and
//! the tab is put back the way a "leave this page?" answered no puts it back.
//!
//! # `allowAndName`, because `allow` overwrites
//!
//! `Browser.setDownloadBehavior` has two ways of saying yes. `allow` lets
//! the engine name the file, and measured against `chrome-headless-shell`
//! 153 it names it `report.pdf` three times running, into one file, and
//! reports `completed` with a clear conscience each time. `allowAndName`
//! writes `<dir>/<guid>` and this program does the naming, on the
//! `completed` event, which is when the file has stopped being
//! `<guid>.crdownload` and is whole — the engine renames it before it sends
//! the event, so the handler always finds it there. Collisions are then this
//! program's to avoid, and it avoids them the way every browser's shelf
//! does: `report (1).pdf`, `report (2).pdf`, reserved with `O_EXCL` so that
//! two downloads finishing in the same pass cannot pick the same name. See
//! [`reserve`] for why that and not `RENAME_NOREPLACE` or `link(2)`.
//!
//! The command goes once, on the browser's own connection, before the pane
//! is taken. It covers every page in the default context, including tabs
//! opened afterwards by `Target.createTarget` or by a `target=_blank` link —
//! measured. Sent without a `downloadPath` it is never answered at all, and
//! with a relative one it is resolved against the engine's working
//! directory rather than this program's, so the path is always absolute.
//!
//! # What the engine says, and when
//!
//! Every download event arrives twice: on the browser connection as
//! `Browser.downloadWillBegin` and `Browser.downloadProgress`, and on the
//! page session that started it as the deprecated `Page.*` pair, in the same
//! millisecond. Only the browser's are read. They carry on after the tab
//! that started the download is closed — the file still completes — when the
//! page's copies stop with the session, and a download is not a tab's.
//!
//! ```text
//!    0 ms   reply to Page.navigate: errorText net::ERR_ABORTED, isDownload true
//!  +16 ms   Page.frameStoppedLoading                       (page session)
//!  +18 ms   Browser.downloadWillBegin {frameId, guid, url, suggestedFilename}
//!  +18 ms   Browser.downloadProgress {guid, totalBytes 22, receivedBytes 0, inProgress}
//!  +19 ms   ... two to five more inProgress (0 then 22 bytes)
//!  +22 ms   Browser.downloadProgress {state completed, receivedBytes 22, filePath}
//! ```
//!
//! That is a 22-byte attachment. An 8 MB file at 2.5 MB/s is the same burst
//! of events at zero bytes and then one every 500 ms until it is done;
//! `totalBytes` is 0 when the server sent no `Content-Length`. What ends a
//! download:
//!
//! | cause | events | on disk afterwards |
//! | --- | --- | --- |
//! | finished | `completed` | `<dir>/<guid>` |
//! | `Browser.cancelDownload` | `canceled` within a millisecond | nothing: the engine removes the partial |
//! | the server hangs up mid-body | five retries 300 ms apart, then `canceled` | nothing |
//! | a `downloadPath` that is a file, or cannot be made | `canceled` at once, no reason | nothing |
//! | the tab closed | the browser's copies carry on to `completed` | the file, whole |
//! | `Browser.close` | `canceled` for each in flight | nothing |
//! | `SIGKILL` | nothing | `<guid>.crdownload`, as far as it got |
//!
//! So `canceled` is the only failure the engine reports, and it never says
//! why. When this program knows — it could not make the directory — the row
//! says so; otherwise it says that the file did not arrive, which is all
//! anybody knows.
//!
//! # The name is the page's, and then it is not
//!
//! `suggestedFilename` is `Content-Disposition`, or the `download` attribute,
//! or the url's last segment, and every one of those is written by the
//! page. The engine sanitizes it once — `../../evil.txt` arrives as
//! `_.._evil.txt`, a `\x1b` as `_`, a right-to-left override as `_`, a
//! leading dot dropped — and this program sanitizes it again in
//! [`safe_name`], for the reason a program that puts a page's bytes on a
//! terminal sanitizes twice: the engine's rules are the engine's and can
//! change under a version bump, and the one thing the engine measurably does
//! not do is bound the length, passing a 407-character name through to a
//! filesystem whose limit is 255. The name that comes out is also plain
//! text by [`crate::text::is_plain`]'s rule, so that the same string can be
//! a file's name and words on the row. The file that is renamed is
//! `<dir>/<guid>`, and the guid is checked to be thirty-six characters of
//! hex and dashes before it is joined to anything, so neither end of the
//! rename is spelled by the page. The event's own `filePath` — sent by 153,
//! absent from the protocol as documented — is not used for the same reason.
//!
//! # Where, and why the file is parsed
//!
//! `$XDG_DOWNLOAD_DIR` is honoured when it is set and absolute, and it
//! almost never is: the XDG user-dirs convention puts that name in
//! `~/.config/user-dirs.dirs` as `XDG_DOWNLOAD_DIR="$HOME/Downloads"`, and
//! that file is what every desktop program reads. Parsing it is twenty
//! lines and [`user_dir`] is them; a value that comes out equal to `$HOME`
//! is the file's way of saying the directory is disabled, and falls
//! through to `~/Downloads` like a missing line does.
//!
//! The directory is not made at startup. The engine makes it at the first
//! download, with its parents, 0700 each — measured — so a person who
//! never downloads anything never gets a `~/Downloads` they did not ask
//! for. What is checked at startup is only that a path which exists is a
//! directory, because a `downloadPath` that is a file gets every download
//! `canceled` with no reason given, and that is a sentence better said in
//! the shell before the pane is taken.
//!
//! # What is on the row, and what is not
//!
//! A download is the one thing here that is not a tab's: it started in one,
//! it survives that tab closing, its events come on the browser's
//! connection. So it is not a [`crate::tabs::Tab::note`], which stands in
//! for a title and is wiped by that tab's next event. It is words of its
//! own, put beside the tab's line rather than instead of it — on the right,
//! where a dialog's hint goes — and taken off eight seconds after the
//! download ends. Progress redraws the row when the words change and not
//! when an event arrives: the engine sends a burst of up to five events at
//! zero bytes and then one every half second, and a percentage that has not
//! moved is not a reason to write to the terminal.
//!
//! # What is cleaned up, and what is never touched
//!
//! On the way out every download still coming is cancelled, which answers
//! in under a millisecond and has the engine remove its own partial file.
//! A temporary profile's quit kills the engine rather than closing it, and
//! a kill leaves `<guid>.crdownload` behind, so once the engine is gone the
//! partials of the guids this run saw begin and not save are removed —
//! those and nothing else. The directory is the person's, and
//! `*.crdownload` in it may be any Chromium's, including one still running.

use std::ffi::OsStr;
use std::fs::{DirBuilder, OpenOptions};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cdp::{Client, Event};
use crate::json::Json;
use crate::screen;
use crate::text;

/// The variable honoured before the file, when it is set and absolute.
pub const DIR_ENV: &str = "XDG_DOWNLOAD_DIR";

/// The file `xdg-user-dirs-update` writes, under `$XDG_CONFIG_HOME` or `~/.config`.
pub const USER_DIRS: &str = "user-dirs.dirs";

/// What the engine calls a file it is still writing.
pub const PARTIAL_SUFFIX: &str = ".crdownload";

/// The longest a Linux filename can be, in bytes; ext4, xfs, btrfs and tmpfs
/// all agree.
pub const NAME_BYTES: usize = 255;

/// How long "saved …" or "couldn't save …" stays on the row.
pub const NOTICE_FOR: Duration = Duration::from_secs(8);

/// The most cells a name takes on the row before it is clipped, so that one
/// 255-byte name cannot take the row from the tab's line.
pub const NAME_CELLS: usize = 40;

/// The longest "extension" that is taken for one. Past this, a dot in a name
/// is punctuation, and keeping everything after it whole would be keeping the
/// wrong half.
const EXTENSION_BYTES: usize = 16;

/// How many `(n)` suffixes are tried before the guid is kept as the name.
const COLLISIONS: u32 = 1000;

/// How long [`Downloads::cancel_all`] spends on the way out, in total. A
/// cancel is answered in under a millisecond; this is for an engine that has
/// stopped answering, which must not hold a quit up.
const CANCEL_FOR: Duration = Duration::from_secs(1);

/// Which directory the person asked for; the shape of [`crate::profile::Choice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// `$XDG_DOWNLOAD_DIR`, `user-dirs.dirs`, or `~/Downloads`.
    Default,
    /// `--download-dir <dir>`.
    At(PathBuf),
}

/// Where downloads go, given the environment as three values and the
/// contents of `user-dirs.dirs` if it could be read.
///
/// In order: the variable when absolute; the file's `XDG_DOWNLOAD_DIR` when
/// it names an absolute directory other than `$HOME`; `$HOME/Downloads`.
/// With no `$HOME` and nothing else there is no answer that is not a guess,
/// and a file saved somewhere guessed is a file nobody finds, so it is an
/// error that names the option round it.
pub fn resolve(
    env: Option<&OsStr>,
    user_dirs: Option<&str>,
    home: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let usable = |value: Option<&OsStr>| {
        value
            .map(Path::new)
            .filter(|path| path.is_absolute())
            .map(Path::to_path_buf)
    };
    if let Some(dir) = usable(env) {
        return Ok(dir);
    }
    if let Some(home) = usable(home) {
        if let Some(dir) = user_dirs.and_then(|contents| user_dir(contents, DIR_ENV, &home)) {
            return Ok(dir);
        }
        return Ok(home.join("Downloads"));
    }
    Err(format!(
        "no ${DIR_ENV} and no $HOME, so nowhere to save a file; use --download-dir <dir>"
    ))
}

/// One `XDG_*_DIR` line of `user-dirs.dirs`, resolved against `home`.
///
/// `KEY="$HOME/x"` and `KEY="/abs"` are the two forms the file has; a
/// relative or unquoted value is ignored as if the line were not there, the
/// last line that does match wins, and a value equal to `home` is "disabled"
/// and comes back `None`.
pub fn user_dir(contents: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let mut found = None;
    for line in contents.lines() {
        let line = line.trim();
        let Some(value) = line
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
        else {
            continue;
        };
        let Some(value) = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
        else {
            continue;
        };
        let path = if let Some(rest) = value.strip_prefix("$HOME") {
            if !rest.is_empty() && !rest.starts_with('/') {
                // `$HOMEWORK/x` is not under `$HOME`.
                continue;
            }
            home.join(rest.trim_start_matches('/'))
        } else if value.starts_with('/') {
            PathBuf::from(value)
        } else {
            continue;
        };
        found = Some(path);
    }
    found.filter(|path| path != home)
}

/// Where `user-dirs.dirs` is, from `$XDG_CONFIG_HOME` (absolute) or `$HOME/.config`.
pub fn user_dirs_file(config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    fn absolute(value: Option<&OsStr>) -> Option<&Path> {
        value.map(Path::new).filter(|path| path.is_absolute())
    }
    if let Some(config) = absolute(config_home) {
        return Some(config.join(USER_DIRS));
    }
    absolute(home).map(|home| home.join(".config").join(USER_DIRS))
}

/// Whether a `Page.navigate` reply says the url turned out to be a file to
/// save rather than a page to show: `isDownload` **and** `net::ERR_ABORTED`.
///
/// Both, because `isDownload` alone is also what an attachment served with a
/// 404 says, with `net::ERR_INVALID_RESPONSE` beside it and the engine's
/// error page landing afterwards — measured — and that is a page that did
/// not come, which is [`crate::load`]'s to report.
pub fn became_download(reply: &Json) -> bool {
    reply.get("isDownload").and_then(Json::as_bool) == Some(true)
        && reply.get("errorText").and_then(Json::as_str) == Some("net::ERR_ABORTED")
}

/// A page's name for a file, made safe to join to the directory and to show.
///
/// The last path component only, split at `/` and `\`: `../../evil.txt` is
/// `evil.txt`, which is more generous than the engine's `_.._evil.txt` and
/// just as safe, since what is left is one component either way. Then every
/// control character and every bidi control becomes `_`, which is what the
/// engine does and so what a person has seen other browsers do; anything
/// else [`crate::text::is_plain`] would not keep — the invisible format
/// characters, the line and paragraph separators — is dropped, so that the
/// name is the same string on disk and on the row. Whitespace and dots are
/// trimmed from both ends: leading dots so that a page cannot hand the person
/// a hidden file, trailing ones because `trailing. ` meant `trailing`. What
/// is left empty is `download`. Then [`fit`] to [`NAME_BYTES`]. `CON` is
/// left alone: this is Linux.
pub fn safe_name(suggested: &str) -> String {
    let last = suggested
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("");
    let name: String = last
        .chars()
        .filter_map(|c| {
            if c.is_control() || is_bidi_control(c) {
                Some('_')
            } else if text::is_plain(c) {
                Some(c)
            } else {
                None
            }
        })
        .collect();
    let name = name.trim_matches(|c: char| c.is_whitespace() || c == '.');
    if name.is_empty() {
        return "download".to_string();
    }
    fit(name, NAME_BYTES)
}

/// Unicode's Bidi_Control set, the same twelve [`crate::text`] drops.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// The name and its extension: `report.pdf` → (`report`, `.pdf`);
/// `archive.tar.gz` → (`archive.tar`, `.gz`); `report` → (`report`, ``).
///
/// A dot at the very start is not an extension's (`.bashrc` has none), nor
/// is a lone dot at the end, and an "extension" longer than sixteen bytes or
/// with whitespace in it is not one either: it is a sentence with a full
/// stop in it.
pub fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot)
            if dot > 0
                && name.len() - dot > 1
                && name.len() - dot <= EXTENSION_BYTES
                && !name[dot..].chars().any(char::is_whitespace) =>
        {
            name.split_at(dot)
        }
        _ => (name, ""),
    }
}

/// `name` cut to `bytes` bytes on a char boundary, from the stem, with the
/// extension kept whole; a name whose extension alone would not leave room is
/// cut from the end like any other string.
pub fn fit(name: &str, bytes: usize) -> String {
    if name.len() <= bytes {
        return name.to_string();
    }
    let (stem, extension) = split_extension(name);
    if extension.len() >= bytes {
        return cut(name, bytes).to_string();
    }
    format!("{}{extension}", cut(stem, bytes - extension.len()))
}

/// The longest front of `text` that is at most `bytes` bytes and ends on a
/// char boundary.
fn cut(text: &str, bytes: usize) -> &str {
    if text.len() <= bytes {
        return text;
    }
    let mut end = bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The nth alternative: `report (1).pdf`, with the stem shortened so that the
/// whole still fits [`NAME_BYTES`].
///
/// `archive.tar.gz` becomes `archive.tar (1).gz`, which is what a shelf does
/// that does not keep a list of double extensions, and the file still opens.
pub fn numbered(name: &str, n: u32) -> String {
    let (stem, extension) = split_extension(name);
    let suffix = format!(" ({n})");
    let room = NAME_BYTES.saturating_sub(suffix.len() + extension.len());
    format!("{}{suffix}{extension}", cut(stem, room))
}

/// Thirty-six characters of `[0-9a-f-]`, which is what the engine's guids
/// are and what a path component may be made of blind.
pub fn is_guid(text: &str) -> bool {
    text.len() == 36
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
}

/// `1.1 MB`, `312 kB`, `22 B`: SI, the way the README writes its numbers.
///
/// One decimal below ten and none above, which is as much as a number that
/// changes twice a second can be read to.
pub fn bytes(n: u64) -> String {
    if n < 1000 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    for unit in ["kB", "MB", "GB", "TB"] {
        value /= 1000.0;
        if value < 9.95 {
            return format!("{value:.1} {unit}");
        }
        if value < 999.5 {
            return format!("{value:.0} {unit}");
        }
    }
    format!("{:.0} PB", value / 1000.0)
}

/// The words for one download in flight: `downloading NAME 42%`, or
/// `downloading NAME 1.1 MB` when the total is not known (0).
///
/// The name is clipped to [`NAME_CELLS`]. The percentage is floored, so that
/// `100%` means it has all arrived, and clamped, because a retry can report
/// more received than the total for a moment.
pub fn progress_words(name: &str, received: u64, total: u64) -> String {
    let name = screen::clip_to(name, NAME_CELLS);
    if total == 0 {
        return format!("downloading {name} {}", bytes(received));
    }
    let percent = (u128::from(received) * 100 / u128::from(total)).min(100);
    format!("downloading {name} {percent}%")
}

/// `~/Downloads/report.pdf` for a path under `home`, the path otherwise.
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    let home = home.filter(|home| home.is_absolute() && home.parent().is_some());
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// One download the engine has announced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub guid: String,
    /// [`safe_name`] of the page's suggestion, already safe to show and to join.
    pub name: String,
    pub received: u64,
    /// 0 until the engine knows.
    pub total: u64,
    pub state: State,
    /// When it began.
    pub began: Instant,
}

/// Where a download has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    InProgress,
    /// Renamed into place, at this path.
    Saved(PathBuf),
    /// `canceled` from the engine — which is every failure — or a rename that
    /// could not be done, with a reason when this program has one.
    Failed(Option<String>),
}

/// Every download of this run, and what the row should say about them.
#[derive(Debug)]
pub struct Downloads {
    dir: PathBuf,
    home: Option<PathBuf>,
    /// In the order they began. Finished ones stay until the notice after
    /// them goes, which is when nothing on the row can be about them any more.
    list: Vec<Download>,
    /// The sentence for the last one that ended, and when it goes.
    notice: Option<(String, Instant)>,
    /// Whether this run cancelled them itself, on the way out, so that the
    /// `canceled` events that follow are not reported as failures.
    quitting: bool,
    /// Whether the directory was there or could be made, the first time a
    /// download began; the reason, if not, for the sentence that says the
    /// file did not arrive.
    dir_ready: Option<Result<(), String>>,
}

impl Downloads {
    /// Downloads into `dir`, said on the row relative to `$HOME`.
    pub fn new(dir: PathBuf) -> Downloads {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute());
        Downloads {
            dir,
            home,
            list: Vec::new(),
            notice: None,
            quitting: false,
            dir_ready: None,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every download this run still remembers, oldest first.
    pub fn all(&self) -> &[Download] {
        &self.list
    }

    /// What one event from the browser connection does. `true` when the
    /// words on the row are not what they were.
    ///
    /// `Browser.downloadWillBegin` adds an entry, and makes the directory if
    /// it is not there — 0700, as the engine would, but with the error kept
    /// for the sentence. `Browser.downloadProgress` updates it; on
    /// `completed` it renames `<dir>/<guid>` into place — see [`keep`] — and
    /// on `canceled` marks it failed, unless this run did the cancelling.
    /// Anything else is ignored, which includes a guid that is not one: a
    /// name that could not be joined to the directory is not tracked at all.
    pub fn take(&mut self, event: &Event, now: Instant) -> bool {
        let params = &event.params;
        let Some(guid) = params
            .get("guid")
            .and_then(Json::as_str)
            .filter(|guid| is_guid(guid))
        else {
            return false;
        };
        let before = self.line(now);
        match event.method.as_str() {
            "Browser.downloadWillBegin" => {
                if self.list.iter().any(|download| download.guid == guid) {
                    return false;
                }
                if self.dir_ready.is_none() {
                    self.dir_ready = Some(ensure_dir(&self.dir, self.home.as_deref()));
                }
                let suggested = params
                    .get("suggestedFilename")
                    .and_then(Json::as_str)
                    .unwrap_or("");
                self.list.push(Download {
                    guid: guid.to_string(),
                    name: safe_name(suggested),
                    received: 0,
                    total: 0,
                    state: State::InProgress,
                    began: now,
                });
            }
            "Browser.downloadProgress" => {
                let count = |key: &str| {
                    params
                        .get(key)
                        .and_then(Json::as_f64)
                        .map(|n| n.max(0.0) as u64)
                };
                let state = params.get("state").and_then(Json::as_str).unwrap_or("");
                let Some(download) = self
                    .list
                    .iter_mut()
                    .find(|download| download.guid == guid && download.state == State::InProgress)
                else {
                    return false;
                };
                if let Some(received) = count("receivedBytes") {
                    download.received = received;
                }
                if let Some(total) = count("totalBytes") {
                    download.total = total;
                }
                let words = match state {
                    "completed" => match keep(&self.dir, guid, &download.name) {
                        Ok(path) => {
                            let words = format!(
                                "saved {}",
                                saved_as(&path, &self.dir, self.home.as_deref())
                            );
                            download.state = State::Saved(path);
                            Some(words)
                        }
                        Err(why) => {
                            let words = format!(
                                "couldn't save {}: {why}",
                                screen::clip_to(&download.name, NAME_CELLS)
                            );
                            download.state = State::Failed(Some(why));
                            Some(words)
                        }
                    },
                    "canceled" => {
                        let why = self.dir_ready.clone().and_then(Result::err);
                        let name = screen::clip_to(&download.name, NAME_CELLS);
                        let words = match &why {
                            Some(why) => format!("couldn't save {name}: {why}"),
                            None => format!("couldn't save {name}"),
                        };
                        download.state = State::Failed(why);
                        (!self.quitting).then_some(words)
                    }
                    _ => None,
                };
                if let Some(words) = words {
                    self.notice = Some((words, now + NOTICE_FOR));
                }
            }
            _ => return false,
        }
        self.line(now) != before
    }

    /// The words for the row right now, or nothing: the newest in flight
    /// (`, 1 more` for the rest), else the notice while it lasts.
    ///
    /// A download in flight wins over a notice. A person watching a second
    /// file come does not need telling for eight more seconds that the first
    /// arrived, and the second's own notice replaces it when it ends. Plain
    /// text already — the name is [`safe_name`]'s and the directory the
    /// person's — and passed through [`crate::text::sanitize`] all the same,
    /// because a reason from the file system quotes a path and this is words
    /// on a terminal.
    pub fn line(&self, now: Instant) -> Option<String> {
        let mut coming = self.in_flight();
        let words = if let Some(newest) = coming.next_back() {
            let others = coming.count();
            let words = progress_words(&newest.name, newest.received, newest.total);
            if others > 0 {
                format!("{words}, {others} more")
            } else {
                words
            }
        } else {
            let (words, until) = self.notice.as_ref()?;
            if now >= *until {
                return None;
            }
            words.clone()
        };
        Some(text::sanitize(&words).into_owned())
    }

    /// When the words will change without an event: the notice's end.
    pub fn expires(&self) -> Option<Instant> {
        self.notice.as_ref().map(|(_, until)| *until)
    }

    /// Drop a notice whose time has come, and the finished downloads it was
    /// the last word on. `true` when the row changed.
    pub fn expire(&mut self, now: Instant) -> bool {
        match self.expires() {
            Some(until) if now >= until => {
                let before = self.line(until - Duration::from_nanos(1));
                self.notice = None;
                self.list
                    .retain(|download| download.state == State::InProgress);
                before != self.line(now)
            }
            _ => false,
        }
    }

    /// The downloads still coming, oldest first.
    pub fn in_flight(&self) -> impl DoubleEndedIterator<Item = &Download> {
        self.list
            .iter()
            .filter(|download| download.state == State::InProgress)
    }

    /// Say that this run is on its way out: a `canceled` from here on is one
    /// this program asked for, and is nobody's news.
    pub fn quit(&mut self) {
        self.quitting = true;
    }

    /// The engine is gone: every download still coming is over, and did not
    /// arrive. Each becomes `Failed(Some("the engine died"))`, and the newest
    /// of them is the row's notice for [`NOTICE_FOR`], as a `canceled` would
    /// be. `true` when the row's words changed.
    ///
    /// Not [`quit`]: the program goes on, on a new engine, and a download
    /// that begins there is news like any other. The partial files are the
    /// caller's to remove — this never touches the file system on an event —
    /// and they stay in [`partials`] until they are.
    ///
    /// [`quit`]: Downloads::quit
    /// [`partials`]: Downloads::partials
    pub fn engine_died(&mut self, now: Instant) -> bool {
        const WHY: &str = "the engine died";
        let before = self.line(now);
        let mut newest = None;
        for download in self
            .list
            .iter_mut()
            .filter(|download| download.state == State::InProgress)
        {
            download.state = State::Failed(Some(WHY.to_string()));
            newest = Some(screen::clip_to(&download.name, NAME_CELLS));
        }
        if let Some(name) = newest {
            self.notice = Some((format!("couldn't save {name}: {WHY}"), now + NOTICE_FOR));
        }
        self.line(now) != before
    }

    /// Ask the engine to stop every download still coming, and [`quit`].
    ///
    /// One `Browser.cancelDownload` each, all of them within a second: the
    /// reply takes under a millisecond and the engine removes the partial
    /// itself, so the second is for an engine that has stopped answering.
    ///
    /// [`quit`]: Downloads::quit
    pub fn cancel_all(&mut self, browser: &mut Client) {
        self.quit();
        let deadline = Instant::now() + CANCEL_FOR;
        let guids: Vec<String> = self
            .in_flight()
            .map(|download| download.guid.clone())
            .collect();
        for guid in guids {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let _ = browser.call_within(
                "Browser.cancelDownload",
                Json::object(vec![("guid", Json::string(guid))]),
                left,
            );
        }
    }

    /// The `<guid>.crdownload` files of downloads this run saw begin and not
    /// save, for removal once the engine is gone and cannot be writing them.
    /// Only those: the directory is the person's.
    ///
    /// A cancelled one is among them although the engine removes its own
    /// partial, because it removes it *after* it has said `canceled` —
    /// measured, the file is still there when the event is read — and an
    /// engine killed in between leaves it. Removing a file that has already
    /// gone costs an `ENOENT`.
    pub fn partials(&self) -> Vec<PathBuf> {
        self.list
            .iter()
            .filter(|download| !matches!(download.state, State::Saved(_)))
            .map(|download| self.dir.join(format!("{}{PARTIAL_SUFFIX}", download.guid)))
            .collect()
    }
}

/// How a saved file is named on the row: the directory as the person would
/// say it, and the file's name clipped so that a long one leaves the tab's
/// line some room.
fn saved_as(path: &Path, dir: &Path, home: Option<&Path>) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = tilde(dir, home);
    let dir = dir.trim_end_matches('/');
    format!("{dir}/{}", screen::clip_to(&name, NAME_CELLS))
}

/// Tell the engine where, and to say what it is doing.
///
/// On the browser's client, once, before any page can be asked for anything:
/// a download that begins before this is one the headless shell silently
/// refuses. `dir` must be absolute — see the module documentation for what a
/// relative one is resolved against — and [`prepare`] makes it so.
pub fn enable(browser: &mut Client, dir: &Path) -> Result<(), String> {
    browser
        .call(
            "Browser.setDownloadBehavior",
            Json::object(vec![
                ("behavior", Json::string("allowAndName")),
                ("downloadPath", Json::string(dir.to_string_lossy())),
                ("eventsEnabled", Json::Bool(true)),
            ]),
        )
        .map(|_| ())
        .map_err(|why| format!("cannot tell the engine where to save files: {why}"))
}

/// Resolve the choice to an absolute path and refuse one that exists and is
/// not a directory. Does not create it: the first download does.
pub fn prepare(choice: Choice) -> Result<PathBuf, String> {
    let dir = match choice {
        Choice::Default => {
            let home = std::env::var_os("HOME");
            let file = user_dirs_file(
                std::env::var_os("XDG_CONFIG_HOME").as_deref(),
                home.as_deref(),
            );
            let contents = file.and_then(|file| std::fs::read_to_string(file).ok());
            resolve(
                std::env::var_os(DIR_ENV).as_deref(),
                contents.as_deref(),
                home.as_deref(),
            )?
        }
        Choice::At(dir) if dir.is_absolute() => dir,
        Choice::At(dir) => std::env::current_dir()
            .map_err(|e| format!("cannot tell where {} is: {e}", dir.display()))?
            .join(dir),
    };
    match std::fs::metadata(&dir) {
        Ok(meta) if !meta.is_dir() => Err(format!(
            "{} is not a directory, so nothing can be saved in it",
            dir.display()
        )),
        _ => Ok(dir),
    }
}

/// Make the directory if it is missing, 0700 with its parents — the mode the
/// engine gives it, and the profile's — and say why when it cannot be.
///
/// One that exists keeps the mode it has: it is the person's.
pub fn ensure_dir(dir: &Path, home: Option<&Path>) -> Result<(), String> {
    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => return Ok(()),
        Ok(_) => return Err(format!("{} is not a directory", tilde(dir, home))),
        Err(_) => {}
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("cannot make {}: {e}", tilde(dir, home)))
}

/// Reserve a free name in `dir`: `name`, then `name (1)`, … each tried with
/// `OpenOptions::create_new`, which is `O_EXCL` — the one atomic way to ask
/// "is this name mine" — so that two downloads ending in one pass, or another
/// program writing the same directory, cannot both get it. The reserved file
/// is empty and is what [`keep`] renames over. After a thousand tries the
/// guid itself is the name, which is the finished file's own and so always
/// the engine's to have.
///
/// Why not `renameat2(RENAME_NOREPLACE)` or `link(2)` and `unlink`: both are
/// atomic too, but `RENAME_NOREPLACE` is Linux-only and refused by some
/// filesystems (`EINVAL` on older overlayfs), and `link` fails on filesystems
/// without hard links, which is exactly what a USB stick's exFAT
/// `~/Downloads` is. `O_EXCL` and then `rename` over one's own placeholder
/// works everywhere `open(2)` does.
pub fn reserve(dir: &Path, name: &str, guid: &str) -> io::Result<PathBuf> {
    for n in 0..COLLISIONS {
        let candidate = if n == 0 {
            name.to_string()
        } else {
            numbered(name, n)
        };
        let path = dir.join(candidate);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(dir.join(guid))
}

/// Move the finished `<dir>/<guid>` to its name: [`reserve`], then
/// `rename(2)`, which replaces the empty placeholder atomically with the
/// engine's file, mode and all.
///
/// If the guid file is not there — an engine that put it somewhere else, a
/// directory somebody emptied — nothing is reserved and the reason comes
/// back; if the rename fails, the placeholder is removed again.
pub fn keep(dir: &Path, guid: &str, name: &str) -> Result<PathBuf, String> {
    if !is_guid(guid) {
        return Err("the engine named it strangely".to_string());
    }
    let finished = dir.join(guid);
    if let Err(e) = std::fs::symlink_metadata(&finished) {
        return Err(format!("the engine's file is not there: {e}"));
    }
    let name = safe_name(name);
    let path = reserve(dir, &name, guid).map_err(|e| format!("cannot name it: {e}"))?;
    if path == finished {
        return Ok(path);
    }
    if let Err(e) = std::fs::rename(&finished, &path) {
        let _ = std::fs::remove_file(&path);
        return Err(format!("cannot name it: {e}"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUID: &str = "2b3f5c1e-9d4a-4e8b-a6f0-1c2d3e4f5a6b";
    const GUID_2: &str = "7e6d5c4b-3a29-4180-9f8e-7d6c5b4a3928";
    const GUID_3: &str = "0a1b2c3d-4e5f-4a0b-8c1d-2e3f4a5b6c7d";

    fn json(text: &str) -> Json {
        Json::parse(text).expect("json")
    }

    fn os(text: &str) -> Option<&OsStr> {
        Some(OsStr::new(text))
    }

    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blinkterm-download-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn began(guid: &str, name: &str) -> Event {
        Event {
            method: "Browser.downloadWillBegin".to_string(),
            params: json(&format!(
                r#"{{"frameId":"F00D","guid":"{guid}","url":"http://127.0.0.1/{name}","suggestedFilename":"{name}"}}"#
            )),
        }
    }

    fn progress(guid: &str, received: u64, total: u64, state: &str) -> Event {
        Event {
            method: "Browser.downloadProgress".to_string(),
            params: json(&format!(
                r#"{{"guid":"{guid}","totalBytes":{total},"receivedBytes":{received},"state":"{state}"}}"#
            )),
        }
    }

    fn home() -> Option<PathBuf> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
    }

    #[test]
    fn the_directory_is_the_variable_then_the_file_then_downloads_under_home() {
        let file = "XDG_DOWNLOAD_DIR=\"$HOME/Fetched\"\n";
        assert_eq!(
            resolve(os("/a"), Some(file), os("/home/u")),
            Ok(PathBuf::from("/a"))
        );
        assert_eq!(
            resolve(os("relative"), Some(file), os("/home/u")),
            Ok(PathBuf::from("/home/u/Fetched")),
            "a relative variable is ignored"
        );
        assert_eq!(
            resolve(os(""), None, os("/home/u")),
            Ok(PathBuf::from("/home/u/Downloads")),
            "an empty variable is none"
        );
        assert_eq!(
            resolve(None, Some("# nothing\n"), os("/home/u")),
            Ok(PathBuf::from("/home/u/Downloads"))
        );
        let why = resolve(None, Some(file), None).expect_err("no home");
        assert!(why.contains("--download-dir"), "{why}");
    }

    #[test]
    fn a_user_dirs_line_is_read_with_its_home_expanded_and_its_quotes_dropped() {
        let home = Path::new("/home/u");
        let read = |contents: &str| user_dir(contents, DIR_ENV, home);
        assert_eq!(
            read("XDG_DOWNLOAD_DIR=\"$HOME/Downloads\""),
            Some(PathBuf::from("/home/u/Downloads"))
        );
        assert_eq!(
            read("XDG_DOWNLOAD_DIR=\"/srv/dl\""),
            Some(PathBuf::from("/srv/dl"))
        );
        assert_eq!(read("XDG_DOWNLOAD_DIR=\"$HOME/\""), None, "disabled");
        assert_eq!(read("XDG_DOWNLOAD_DIR=\"$HOME\""), None, "disabled");
        assert_eq!(read("# XDG_DOWNLOAD_DIR=\"/srv/dl\""), None);
        assert_eq!(read("XDG_DOWNLOAD_DIR=\"dl\""), None, "relative");
        assert_eq!(read("XDG_DOWNLOAD_DIR=/srv/dl"), None, "unquoted");
        assert_eq!(read("XDG_DESKTOP_DIR=\"$HOME/Desktop\""), None);
        assert_eq!(read("XDG_DOWNLOAD_DIRX=\"/srv/dl\""), None);
        assert_eq!(read("XDG_DOWNLOAD_DIR=\"$HOMEWORK/dl\""), None);
        assert_eq!(
            read(
                "# written by xdg-user-dirs-update\n\
                 XDG_DESKTOP_DIR=\"$HOME/Desktop\"\n\
                 XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n\
                 XDG_DOWNLOAD_DIR=\"/srv/dl\"\n"
            ),
            Some(PathBuf::from("/srv/dl")),
            "the last line wins"
        );
        assert_eq!(
            user_dirs_file(os("/c"), os("/home/u")),
            Some(PathBuf::from("/c/user-dirs.dirs"))
        );
        assert_eq!(
            user_dirs_file(os("c"), os("/home/u")),
            Some(PathBuf::from("/home/u/.config/user-dirs.dirs"))
        );
        assert_eq!(user_dirs_file(None, None), None);
    }

    #[test]
    fn a_navigation_that_became_a_download_is_told_apart_from_one_that_failed() {
        assert!(became_download(&json(
            r#"{"frameId":"F","loaderId":"L","errorText":"net::ERR_ABORTED","isDownload":true}"#
        )));
        assert!(
            !became_download(&json(
                r#"{"errorText":"net::ERR_INVALID_RESPONSE","isDownload":true}"#
            )),
            "an attachment served with a 404 is a page that did not come"
        );
        assert!(
            !became_download(&json(
                r#"{"errorText":"net::ERR_ABORTED","isDownload":false}"#
            )),
            "a navigation overtaken by another"
        );
        assert!(!became_download(&json("{}")));
    }

    #[test]
    fn a_pages_name_becomes_one_safe_path_component() {
        for (sent, wanted) in [
            ("report.pdf", "report.pdf"),
            // The last component, which is more than the engine keeps and as
            // safe: one component either way.
            ("../../evil.txt", "evil.txt"),
            ("_.._evil.txt", "_.._evil.txt"),
            ("a/b.txt", "b.txt"),
            ("a\\b.txt", "b.txt"),
            ("dir/", "dir"),
            ("..", "download"),
            (".", "download"),
            ("", "download"),
            ("/", "download"),
            (".hidden", "hidden"),
            ("...hidden", "hidden"),
            ("ctl\u{1}\u{1b}[31m.txt", "ctl__[31m.txt"),
            ("evil\u{202e}fdp.exe", "evil_fdp.exe"),
            ("a\tb.txt", "a_b.txt"),
            ("nul\0.txt", "nul_.txt"),
            (" trailing. ", "trailing"),
            ("zero\u{200b}width.txt", "zerowidth.txt"),
            ("line\u{2028}break.txt", "linebreak.txt"),
            ("日本語 報告.pdf", "日本語 報告.pdf"),
            ("CON", "CON"),
        ] {
            let name = safe_name(sent);
            assert_eq!(name, wanted, "{sent:?}");
            assert!(name.chars().all(text::is_plain), "{sent:?} → {name:?}");
        }
    }

    #[test]
    fn a_name_is_cut_to_what_a_filesystem_takes_and_keeps_its_extension() {
        let long = format!("{}.tar.gz", "y".repeat(400));
        let name = safe_name(&long);
        assert_eq!(name.len(), NAME_BYTES);
        assert!(
            name.ends_with("yyy.gz"),
            "the double extension's first half is stem: {name}"
        );

        let wide = format!("{}.txt", "日".repeat(100));
        let name = safe_name(&wide);
        assert!(name.len() <= NAME_BYTES, "{}", name.len());
        assert!(name.ends_with("日.txt"), "{name}");

        let dotted = format!("a.{}", "x".repeat(300));
        let name = safe_name(&dotted);
        assert_eq!(
            name.len(),
            NAME_BYTES,
            "not an extension, so cut like a string"
        );
        assert!(name.starts_with("a.x"), "{name}");

        assert_eq!(split_extension("report.pdf"), ("report", ".pdf"));
        assert_eq!(split_extension("archive.tar.gz"), ("archive.tar", ".gz"));
        assert_eq!(split_extension("report"), ("report", ""));
        assert_eq!(split_extension(".bashrc"), (".bashrc", ""));
        assert_eq!(split_extension("done. really"), ("done. really", ""));
        assert_eq!(fit("short.txt", NAME_BYTES), "short.txt");
    }

    #[test]
    fn an_alternative_name_is_numbered_before_its_extension() {
        assert_eq!(numbered("report.pdf", 1), "report (1).pdf");
        assert_eq!(numbered("report", 2), "report (2)");
        assert_eq!(numbered("archive.tar.gz", 1), "archive.tar (1).gz");
        let full = safe_name(&format!("{}.pdf", "z".repeat(400)));
        let alternative = numbered(&full, 12);
        assert!(alternative.len() <= NAME_BYTES, "{}", alternative.len());
        assert!(alternative.ends_with("z (12).pdf"), "{alternative}");
    }

    #[test]
    fn a_guid_is_thirty_six_hex_and_dashes_and_nothing_else() {
        assert!(is_guid(GUID));
        assert!(!is_guid("../x"));
        assert!(!is_guid(&GUID[1..]));
        assert!(!is_guid(&GUID.to_uppercase()));
        assert!(!is_guid("../../../../../../../../../etc/passw"));
    }

    #[test]
    fn progress_is_a_percentage_when_the_total_is_known_and_a_size_when_it_is_not() {
        assert_eq!(
            progress_words("report.pdf", 42, 100),
            "downloading report.pdf 42%"
        );
        assert_eq!(progress_words("x", 100_000, 0), "downloading x 100 kB");
        assert_eq!(progress_words("x", 150, 100), "downloading x 100%");
        assert_eq!(progress_words("x", 999, 1000), "downloading x 99%");
        let long = "n".repeat(200);
        let words = progress_words(&long, 0, 10);
        assert_eq!(
            words,
            format!("downloading {} 0%", screen::clip_to(&long, NAME_CELLS))
        );
        assert_eq!(screen::width(&words), "downloading ".len() + NAME_CELLS + 3);
    }

    #[test]
    fn sizes_read_the_way_the_readme_writes_them() {
        assert_eq!(bytes(22), "22 B");
        assert_eq!(bytes(185_000), "185 kB");
        assert_eq!(bytes(1_100_000), "1.1 MB");
        assert_eq!(bytes(2_097_152), "2.1 MB");
        assert_eq!(bytes(999_999), "1.0 MB", "never 1000 kB");
        assert_eq!(bytes(12_400_000), "12 MB");
    }

    #[test]
    fn a_path_under_home_is_said_with_a_tilde() {
        let home = Some(Path::new("/home/u"));
        assert_eq!(
            tilde(Path::new("/home/u/Downloads/r.pdf"), home),
            "~/Downloads/r.pdf"
        );
        assert_eq!(tilde(Path::new("/home/u"), home), "~");
        assert_eq!(tilde(Path::new("/home/uv/x"), home), "/home/uv/x");
        assert_eq!(tilde(Path::new("/srv/x"), home), "/srv/x");
        assert_eq!(tilde(Path::new("/srv/x"), Some(Path::new("/"))), "/srv/x");
        assert_eq!(tilde(Path::new("/srv/x"), None), "/srv/x");
    }

    #[test]
    fn the_events_of_one_download_become_words_and_then_a_file() {
        let dir = scratch("one");
        let mut downloads = Downloads::new(dir.clone());
        let now = Instant::now();
        assert!(downloads.take(&began(GUID, "report.pdf"), now));
        assert!(downloads.take(&progress(GUID, 0, 22, "inProgress"), now));
        assert_eq!(
            downloads.line(now).as_deref(),
            Some("downloading report.pdf 0%")
        );
        assert!(
            !downloads.take(&progress(GUID, 0, 22, "inProgress"), now),
            "the same words again are not a redraw"
        );
        assert!(downloads.take(&progress(GUID, 11, 22, "inProgress"), now));
        assert_eq!(
            downloads.line(now).as_deref(),
            Some("downloading report.pdf 50%")
        );

        std::fs::write(dir.join(GUID), b"%PDF-1.4 hello report\n").expect("the engine's file");
        assert!(downloads.take(&progress(GUID, 22, 22, "completed"), now));
        assert_eq!(
            std::fs::read(dir.join("report.pdf")).expect("the file, named"),
            b"%PDF-1.4 hello report\n"
        );
        assert!(!dir.join(GUID).exists(), "the guid's file was renamed");
        assert!(
            downloads.partials().is_empty(),
            "a saved file is nobody's partial"
        );
        let saved = format!("saved {}/report.pdf", tilde(&dir, home().as_deref()));
        assert_eq!(downloads.line(now), Some(saved.clone()));
        assert_eq!(downloads.expires(), Some(now + NOTICE_FOR));
        assert!(!downloads.expire(now + NOTICE_FOR / 2));
        assert_eq!(downloads.line(now + NOTICE_FOR / 2), Some(saved));
        assert!(downloads.expire(now + NOTICE_FOR));
        assert_eq!(downloads.line(now + NOTICE_FOR), None);
        assert!(downloads.all().is_empty(), "nothing left to say about it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_download_with_the_same_name_is_numbered_and_two_ending_at_once_do_not_collide() {
        let dir = scratch("same");
        let mut downloads = Downloads::new(dir.clone());
        let now = Instant::now();
        for guid in [GUID, GUID_2] {
            downloads.take(&began(guid, "report.pdf"), now);
            std::fs::write(dir.join(guid), guid).expect("the engine's file");
        }
        for guid in [GUID, GUID_2] {
            downloads.take(&progress(guid, 36, 36, "completed"), now);
        }
        downloads.take(&began(GUID_3, "report.pdf"), now);
        std::fs::write(dir.join(GUID_3), GUID_3).expect("the engine's file");
        downloads.take(&progress(GUID_3, 36, 36, "completed"), now);
        for (name, guid) in [
            ("report.pdf", GUID),
            ("report (1).pdf", GUID_2),
            ("report (2).pdf", GUID_3),
        ] {
            assert_eq!(
                std::fs::read_to_string(dir.join(name)).expect(name),
                guid,
                "{name}"
            );
        }
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .expect("the directory")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["report (1).pdf", "report (2).pdf", "report.pdf"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_download_is_a_failure_unless_this_program_cancelled_it() {
        let dir = scratch("cancel");
        let now = Instant::now();
        let mut downloads = Downloads::new(dir.clone());
        downloads.take(&began(GUID, "report.pdf"), now);
        assert_eq!(
            downloads.partials(),
            [dir.join(format!("{GUID}.crdownload"))]
        );
        assert!(downloads.take(&progress(GUID, 0, 0, "canceled"), now));
        assert_eq!(
            downloads.line(now).as_deref(),
            Some("couldn't save report.pdf")
        );
        assert_eq!(
            downloads.partials(),
            [dir.join(format!("{GUID}.crdownload"))],
            "the engine removes it after it says so, and may be killed first"
        );

        let mut downloads = Downloads::new(dir.clone());
        downloads.take(&began(GUID, "report.pdf"), now);
        downloads.quit();
        assert!(downloads.take(&progress(GUID, 0, 0, "canceled"), now));
        assert_eq!(downloads.line(now), None, "nobody's news");

        // A finished file the engine did not leave where it said: the reason
        // is said, and no placeholder is left under the name.
        let mut downloads = Downloads::new(dir.clone());
        downloads.take(&began(GUID_2, "gone.bin"), now);
        downloads.take(&progress(GUID_2, 5, 5, "completed"), now);
        let line = downloads.line(now).expect("a sentence");
        assert!(line.starts_with("couldn't save gone.bin: "), "{line}");
        assert!(!dir.join("gone.bin").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_download_in_flight_when_the_engine_dies_is_a_failure_that_says_so() {
        let dir = scratch("died");
        let now = Instant::now();
        let mut downloads = Downloads::new(dir.clone());
        downloads.take(&began(GUID, "a"), now);
        downloads.take(&began(GUID_2, "b"), now);
        assert!(downloads.engine_died(now));
        let died = State::Failed(Some("the engine died".to_string()));
        assert!(downloads
            .all()
            .iter()
            .all(|download| download.state == died));
        assert_eq!(
            downloads.line(now).as_deref(),
            Some("couldn't save b: the engine died")
        );
        assert_eq!(
            downloads.partials(),
            [
                dir.join(format!("{GUID}.crdownload")),
                dir.join(format!("{GUID_2}.crdownload"))
            ]
        );
        assert!(!downloads.engine_died(now), "nothing left in flight");

        // A relaunch is not a quit: the next download is news.
        downloads.take(&began(GUID_3, "c"), now);
        assert_eq!(downloads.all()[2].state, State::InProgress);
        assert_eq!(downloads.line(now).as_deref(), Some("downloading c 0 B"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_that_cannot_be_made_is_the_reason_given() {
        let dir = scratch("unmakeable");
        let file = dir.join("a-file");
        std::fs::write(&file, b"").expect("a file");
        let mut downloads = Downloads::new(file.join("below"));
        let now = Instant::now();
        downloads.take(&began(GUID, "report.pdf"), now);
        downloads.take(&progress(GUID, 0, 0, "canceled"), now);
        let line = downloads.line(now).expect("a sentence");
        assert!(
            line.starts_with("couldn't save report.pdf: cannot make "),
            "{line}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_newest_download_is_the_one_named_and_the_rest_are_counted() {
        let mut downloads = Downloads::new(scratch("two"));
        let now = Instant::now();
        downloads.take(&began(GUID, "first.bin"), now);
        downloads.take(&began(GUID_2, "second.bin"), now);
        assert_eq!(
            downloads.line(now).as_deref(),
            Some("downloading second.bin 0 B, 1 more")
        );
        assert!(downloads.take(&progress(GUID_2, 0, 0, "canceled"), now));
        assert_eq!(
            downloads.line(now).as_deref(),
            Some("downloading first.bin 0 B"),
            "one in flight beats the notice"
        );
        let _ = std::fs::remove_dir_all(downloads.dir());
    }

    #[test]
    fn progress_that_does_not_move_does_not_redraw() {
        let mut downloads = Downloads::new(scratch("still"));
        let now = Instant::now();
        downloads.take(&began(GUID, "big.bin"), now);
        let redraws: Vec<bool> = [1000, 1001, 1002, 1003, 1004]
            .into_iter()
            .map(|received| downloads.take(&progress(GUID, received, 100_000, "inProgress"), now))
            .collect();
        assert_eq!(redraws, [true, false, false, false, false]);
        let _ = std::fs::remove_dir_all(downloads.dir());
    }

    #[test]
    fn events_that_are_not_about_a_download_by_a_real_guid_change_nothing() {
        let mut downloads = Downloads::new(scratch("strange"));
        let now = Instant::now();
        assert!(!downloads.take(&began("../../etc", "x"), now));
        assert!(!downloads.take(&progress(GUID, 1, 2, "inProgress"), now));
        let other = Event {
            method: "Target.targetInfoChanged".to_string(),
            params: json(&format!(r#"{{"guid":"{GUID}"}}"#)),
        };
        assert!(!downloads.take(&other, now));
        assert!(downloads.all().is_empty());
        let _ = std::fs::remove_dir_all(downloads.dir());
    }
}
