//! Blocking ads and trackers by host, from the lists people already use.
//!
//! Every frame costs something here — bandwidth over ssh, decode time, the
//! terminal's parse loop — and an animated ad keeps the screencast running
//! on a page nobody is scrolling. So a request to a host on a list is failed
//! before it leaves the engine, the page carries on without it, and the row
//! says how many went: `12 blocked`.
//!
//! # How: `Fetch`, answered on the reader thread
//!
//! Every page's session is given `Fetch.enable` with one pattern that pauses
//! every request before it is sent, and every pause is answered at once:
//! `Fetch.failRequest` with `BlockedByClient` for a listed host,
//! `Fetch.continueRequest` for the rest. Measured against
//! chrome-headless-shell 153 over the pipe, on a local page of 300 images:
//! 2.12 s to load with every request paused and answered, against 2.10 s
//! with nothing, 6 µs to decide a request against 100 000 hosts, and about
//! 800 bytes on the pipe per pause.
//!
//! `Network.setBlockedURLs` was measured and not taken. The same 100 000
//! hosts are 200 000 patterns (the host and its subdomains), a 5.8 MB
//! command the engine took 16.3 s to acknowledge; the 300-image page then
//! loaded in 7.3 s instead of 2.1 s, because every request is matched
//! against every pattern; and it needs `Network.enable`, which sent 4 214
//! events and 2.7 MB into the page's mailbox per load of that page — a
//! mailbox that drops past 512 events.
//!
//! The answering happens on [`crate::cdp`]'s reader thread, before a message
//! is routed ([`crate::cdp::Intercept`]), and never in a mailbox: the engine
//! holds `Page.navigate`'s reply until its document's pause is answered, so
//! a main loop waiting on that reply could not answer it. The same hook sees
//! `Target.attachedToTarget` and gives the new session `Fetch.enable` before
//! the reply to the attach has reached anybody, which covers the first tab,
//! `ctrl+t`, a popup, a restored tab and a relaunched engine's tabs with
//! nothing added to the code that makes them (6 requests of 6 paused on a
//! session enabled this way). `Fetch.enable` is sent from here and nowhere
//! else, on purpose: a session with `Fetch` on and nobody answering is a
//! page whose every request hangs.
//!
//! # What is blocked
//!
//! A listed host, and every host under it: `ads.example.com` blocks
//! `x.ads.example.com`, and not `notads.example.com` or `example.com`. That
//! is how uBlock Origin and AdGuard read a host list. A page's own document
//! is blocked too, and the engine shows its error page with
//! `net::ERR_BLOCKED_BY_CLIENT`, which the row already says.
//!
//! What is not: the subresources of an iframe the engine runs in a process
//! of its own (on chrome-headless-shell's defaults a cross-site iframe is in
//! the page's process and is covered; with `--site-per-process` it is not,
//! until frames are attached to), requests a service worker makes, and
//! anything a list says in a syntax other than a host — EasyList's element
//! hiding and its `||host^` rules. Host blocking first.
//!
//! # The exceptions
//!
//! `alt+b` turns blocking off for the site in front — the page's host — and
//! on again. The sites are kept in `<profile>/unblocked` ([`FILE`]), the way
//! [`crate::zoom`] keeps its levels, and the row says `unblocked` on one.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::net::IpAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::cdp::{Intercept, Notifier};
use crate::json::Json;
use crate::text;

/// The lists the settings name, and whether to use them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lists {
    /// Every `block-list`, the file's first and then the command line's.
    pub paths: Vec<PathBuf>,
    /// `false` for `block = false` or `--no-block`: nothing is blocked,
    /// whatever the lists say.
    pub enabled: bool,
}

impl Default for Lists {
    /// No lists, and on: which blocks nothing until a list is named.
    fn default() -> Lists {
        Lists {
            paths: Vec::new(),
            enabled: true,
        }
    }
}

/// What reading a list found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Parsed {
    /// Hosts added.
    pub hosts: usize,
    /// Fields that were neither a host nor a hosts file's usual boilerplate:
    /// a rule in another syntax, a line with two names and no address.
    pub skipped: usize,
}

/// What one field of a list is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    /// A host to block, as it is kept: lower case, no trailing dot.
    Host(String),
    /// What every hosts file has and is not a host to block: an address,
    /// `localhost`, `broadcasthost`, a name with no dot.
    Boilerplate,
    /// Something this does not read.
    Junk,
}

/// The fields of one line of a list.
///
/// A hosts file's line is an address and then names (`0.0.0.0 ads.example`);
/// a plain list's is one name. A `#` starts a comment, on its own line or
/// after the names. A line of two or more fields that does not start with an
/// address is not either form and is junk as a whole, so that half of a
/// rule in some other syntax is never read as a host.
pub fn parse_line(line: &str) -> Vec<Field> {
    let line = line.split('#').next().unwrap_or_default();
    let fields: Vec<&str> = line.split_whitespace().collect();
    match fields.as_slice() {
        [] => Vec::new(),
        [only] => vec![field(only)],
        // An address's zone (`fe80::1%lo0`, in a Mac's hosts file) is not
        // part of what parses as one.
        [first, names @ ..] if address(first) => names.iter().map(|name| field(name)).collect(),
        _ => vec![Field::Junk],
    }
}

/// Whether a hosts file's first field is an address.
fn address(field: &str) -> bool {
    field
        .split('%')
        .next()
        .is_some_and(|ip| ip.parse::<IpAddr>().is_ok())
}

/// One name, as a host to block or why not.
fn field(name: &str) -> Field {
    let lower = name.to_ascii_lowercase();
    let host = lower.trim_end_matches('.');
    let host = host.strip_prefix("*.").unwrap_or(host);
    if host.parse::<IpAddr>().is_ok() || !host.contains('.') || host == "localhost.localdomain" {
        return Field::Boilerplate;
    }
    let plain = host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if !plain || host.starts_with('.') || host.contains("..") {
        return Field::Junk;
    }
    Field::Host(host.to_string())
}

/// Read a list's text into `into`.
pub fn parse(text: &str, into: &mut HashSet<Box<str>>) -> Parsed {
    let mut parsed = Parsed::default();
    for line in text.lines() {
        for field in parse_line(line) {
            match field {
                Field::Host(host) => {
                    if into.insert(host.into_boxed_str()) {
                        parsed.hosts += 1;
                    }
                }
                Field::Boilerplate => {}
                Field::Junk => parsed.skipped += 1,
            }
        }
    }
    parsed
}

/// Read every list the settings name, once, before the terminal is taken.
///
/// `None` when blocking is off or no list is named: then no session is ever
/// given `Fetch.enable`, and blocking costs nothing at all. A list that
/// cannot be read is a sentence in the shell, as a download directory that
/// is a file is, and so is one with nothing in it this can read — an
/// EasyList given where a host list was meant — rather than a run that
/// silently blocks nothing. About 100 000 hosts take tens of milliseconds
/// and 6 to 10 MB.
pub fn load(lists: &Lists, unblocked: &Unblocked) -> Result<Option<Arc<Blocker>>, String> {
    if !lists.enabled || lists.paths.is_empty() {
        return Ok(None);
    }
    let mut hosts = HashSet::new();
    for path in &lists.paths {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("cannot read the block-list {}: {e}", path.display()))?;
        let parsed = parse(&String::from_utf8_lossy(&bytes), &mut hosts);
        if parsed.hosts == 0 && parsed.skipped > 0 {
            return Err(format!(
                "the block-list {} has no host in it that this can read: \
                 a block-list is a hosts file or one host per line",
                path.display()
            ));
        }
    }
    let sites = unblocked.sites().map(str::to_string);
    Ok(Some(Arc::new(Blocker::new(hosts, sites))))
}

/// A url's host, split from the rest: lower case, without a user name, a
/// port or an IPv6 address's brackets, and without a trailing dot. Only
/// `http`, `https`, `ws` and `wss`; anything else is `None`.
fn authority(url: &str) -> Option<String> {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let rest = ["http://", "https://", "ws://", "wss://"]
        .into_iter()
        .find(|scheme| lower.starts_with(scheme))
        .map(|scheme| url.get(scheme.len()..).unwrap_or_default())?;
    let authority = &rest[..rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len())];
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(inside) = host_port.strip_prefix('[') {
        &inside[..inside.find(']')?]
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let plain = !host.is_empty()
        && host
            .chars()
            .all(|c| text::is_plain(c) && !c.is_whitespace());
    plain.then_some(host)
}

/// The host a request is matched by: see `authority`, and `None` for an
/// address, which no host list can name.
pub fn host_of(url: &str) -> Option<String> {
    authority(url).filter(|host| host.parse::<IpAddr>().is_err())
}

/// The site an exception is kept under: the page's host, an address
/// included, so that a page on `127.0.0.1` can be unblocked like any other.
pub fn site_of(url: &str) -> Option<String> {
    authority(url)
}

/// Whether `host` is listed, itself or as a subdomain of a listed host.
///
/// At most one lookup per label: `a.b.example.com` asks for itself,
/// `b.example.com`, `example.com` and `com`.
pub fn listed(hosts: &HashSet<Box<str>>, host: &str) -> bool {
    let mut rest = host;
    loop {
        if hosts.contains(rest) {
            return true;
        }
        match rest.find('.') {
            Some(dot) => rest = &rest[dot + 1..],
            None => return false,
        }
    }
}

/// `Fetch.enable`'s parameters, in one place: every request, paused before
/// it is sent.
pub fn patterns() -> Json {
    Json::object(vec![(
        "patterns",
        Json::Array(vec![Json::object(vec![
            ("urlPattern", Json::string("*")),
            ("requestStage", Json::string("Request")),
        ])]),
    )])
}

/// The file in the profile the exceptions are kept in.
pub const FILE: &str = "unblocked";

/// Sites kept: more than anybody unblocks, and a fold of this many lines is
/// instant.
pub const CAP: usize = 500;

/// Lines in the file at which it is compacted on load.
pub const COMPACT_AT: usize = 2 * CAP;

/// The sites blocking is off for, in `<profile>/unblocked` — or nowhere, for
/// a temporary profile.
///
/// One line per change — the site, a tab, `on` or `off` — appended, folded
/// on load, and compacted when it has grown to [`COMPACT_AT`] lines or ends
/// in a line cut short, exactly as [`crate::zoom`] keeps its levels. It is a
/// list of sites somebody visited, so it is 0600.
#[derive(Debug)]
pub struct Unblocked {
    set: HashSet<String>,
    /// The sites in `set`, oldest change first: what goes past [`CAP`].
    order: Vec<String>,
    /// The file, or `None` for a temporary profile.
    path: Option<PathBuf>,
}

impl Unblocked {
    /// Exceptions that are never written anywhere: a temporary profile's.
    pub fn in_memory() -> Unblocked {
        Unblocked {
            set: HashSet::new(),
            order: Vec::new(),
            path: None,
        }
    }

    /// The exceptions kept in the profile at `dir`, and where to add to them.
    ///
    /// As [`crate::zoom::Zooms::load`]: a missing file is none, a line that
    /// does not parse is skipped, as is a last line with no newline after
    /// it, and nothing here fails. The last line for a site says whether it
    /// is unblocked.
    pub fn load(dir: &Path) -> Unblocked {
        let path = dir.join(FILE);
        let file = std::fs::read(&path).unwrap_or_default();
        let file = String::from_utf8_lossy(&file);
        let mut unblocked = Unblocked::in_memory();
        let mut lines = 0;
        for line in file.split_inclusive('\n') {
            lines += 1;
            let Some(line) = line.strip_suffix('\n') else {
                continue;
            };
            if let Some((site, off)) = Unblocked::parse_line(line) {
                unblocked.remember(site, off);
            }
        }
        unblocked.path = Some(path);
        if lines >= COMPACT_AT || !(file.is_empty() || file.ends_with('\n')) {
            let _ = unblocked.compact();
        }
        unblocked
    }

    /// One line of the file: the site, a tab, `off` (blocking off: the site
    /// is unblocked) or `on`; `None` for one that is not.
    pub fn parse_line(line: &str) -> Option<(String, bool)> {
        let (site, word) = line.split_once('\t')?;
        let plain = !site.is_empty()
            && site
                .chars()
                .all(|c| text::is_plain(c) && !c.is_whitespace());
        let off = match word {
            "off" => true,
            "on" => false,
            _ => return None,
        };
        plain.then(|| (site.to_string(), off))
    }

    /// Whether blocking is off for this site.
    pub fn contains(&self, site: &str) -> bool {
        self.set.contains(site)
    }

    /// Every site blocking is off for.
    pub fn sites(&self) -> impl Iterator<Item = &str> {
        self.order.iter().map(String::as_str)
    }

    /// Turn blocking off for `site` if it was on, and on if it was off, and
    /// append a line saying so if there is a file. `Ok(true)` is "now
    /// unblocked".
    ///
    /// The error is the file's; the change has been made all the same, for
    /// this run.
    pub fn toggle(&mut self, site: &str) -> Result<bool, String> {
        let off = !self.contains(site);
        self.remember(site.to_string(), off);
        let Some(path) = &self.path else {
            return Ok(off);
        };
        OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut file| file.write_all(line(site, off).as_bytes()))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(off)
    }

    /// The set's half of [`Unblocked::toggle`] and of every line read.
    fn remember(&mut self, site: String, off: bool) {
        self.order.retain(|known| *known != site);
        if !off {
            self.set.remove(&site);
            return;
        }
        self.set.insert(site.clone());
        self.order.push(site);
        if self.order.len() > CAP {
            let gone = self.order.remove(0);
            self.set.remove(&gone);
        }
    }

    /// Write the exceptions as a fresh file, oldest first, to a file beside
    /// it that is then renamed over it.
    fn compact(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let fresh = path.with_extension("tmp");
        let file: String = self.order.iter().map(|site| line(site, true)).collect();
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&fresh)
            .and_then(|mut out| out.write_all(file.as_bytes()))
            .and_then(|()| std::fs::rename(&fresh, path))
            .map_err(|e| format!("cannot compact {}: {e}", path.display()))
    }
}

/// One line of the file, newline included.
fn line(site: &str, off: bool) -> String {
    format!("{site}\t{}\n", if off { "off" } else { "on" })
}

/// What to do with one paused request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Let it go.
    Continue,
    /// Fail it as blocked by the client.
    Block,
}

/// A page's session, as the blocker knows it.
#[derive(Debug, Default)]
struct Page {
    /// The main frame's id, which is the page target's: what tells the
    /// page's own document from an iframe's.
    frame: Option<String>,
    /// The site of the document in its main frame, for the exceptions.
    site: Option<String>,
    /// Requests blocked since that document landed.
    blocked: u32,
}

/// What the reader thread and the main loop share.
#[derive(Debug, Default)]
struct State {
    /// The sites blocking is off for: a mirror of [`Unblocked`], which the
    /// main loop keeps, for the reader thread to read.
    unblocked: HashSet<String>,
    /// Every page session attached, by `sessionId`.
    sessions: HashMap<String, Page>,
}

/// The hosts, and what has been blocked on which page.
///
/// Shared between the reader thread, which asks it about every request and
/// tells it about every page attached, landed and gone, and the main loop,
/// which draws its count and changes its exceptions. Both in pipe order on
/// the reader's side, so a count is never reset by a landing that has not
/// happened yet. The hosts are read-only once made; the rest is behind one
/// mutex held for a hash lookup at a time.
#[derive(Debug)]
pub struct Blocker {
    hosts: HashSet<Box<str>>,
    state: Mutex<State>,
}

impl Blocker {
    /// A blocker for `hosts`, with blocking off for `unblocked`.
    pub fn new(hosts: HashSet<Box<str>>, unblocked: impl IntoIterator<Item = String>) -> Blocker {
        Blocker {
            hosts,
            state: Mutex::new(State {
                unblocked: unblocked.into_iter().collect(),
                sessions: HashMap::new(),
            }),
        }
    }

    /// How many hosts are listed.
    pub fn hosts(&self) -> usize {
        self.hosts.len()
    }

    /// Turn blocking off for a site, or on again.
    pub fn set_unblocked(&self, site: &str, unblocked: bool) {
        if let Ok(mut state) = self.state.lock() {
            if unblocked {
                state.unblocked.insert(site.to_string());
            } else {
                state.unblocked.remove(site);
            }
        }
    }

    /// A page's session was attached to `target`, showing `url`.
    pub fn attached(&self, session: &str, target: &str, url: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.sessions.insert(
                session.to_string(),
                Page {
                    frame: Some(target.to_string()),
                    site: site_of(url),
                    blocked: 0,
                },
            );
        }
    }

    /// A page's main frame landed on `url`: a new document, a new count.
    pub fn landed(&self, session: &str, url: &str) {
        if let Ok(mut state) = self.state.lock() {
            let page = state.sessions.entry(session.to_string()).or_default();
            page.site = site_of(url);
            page.blocked = 0;
        }
    }

    /// A page's session is gone.
    pub fn detached(&self, session: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.sessions.remove(session);
        }
    }

    /// The engine is new: none of the sessions known are.
    pub fn forget_all(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.sessions.clear();
        }
    }

    /// Whether to block a request for `url` on `session`, counted if it is.
    /// `document` is the frame a document is being loaded into, and `None`
    /// for anything else.
    ///
    /// A listed host is blocked unless the page it is for is on a site
    /// blocking is off for. The page's own document is the exception to the
    /// exception: it is the page about to be, not the one being left, so it
    /// is let through only when its own site is unblocked — which is what
    /// makes `alt+b` on a blocked site's error page and a reload bring it
    /// back, and what keeps an unblocked site's link to a listed one from
    /// carrying the exception with it.
    pub fn decide(&self, session: &str, url: &str, document: Option<&str>) -> Verdict {
        let Some(host) = host_of(url) else {
            return Verdict::Continue;
        };
        if !listed(&self.hosts, &host) {
            return Verdict::Continue;
        }
        let Ok(mut state) = self.state.lock() else {
            return Verdict::Continue;
        };
        let State {
            unblocked,
            sessions,
        } = &mut *state;
        let page = sessions.entry(session.to_string()).or_default();
        let main = document.is_some() && document == page.frame.as_deref();
        let site = if main {
            Some(&host)
        } else {
            page.site.as_ref()
        };
        if site.is_some_and(|site| unblocked.contains(site)) {
            return Verdict::Continue;
        }
        page.blocked = page.blocked.saturating_add(1);
        Verdict::Block
    }

    /// What the row says about `session`'s page: `unblocked` on a site
    /// blocking is off for, how many requests were blocked when some were,
    /// and nothing otherwise.
    pub fn words(&self, session: Option<&str>) -> Option<String> {
        let state = self.state.lock().ok()?;
        let page = state.sessions.get(session?)?;
        if page
            .site
            .as_ref()
            .is_some_and(|site| state.unblocked.contains(site))
        {
            return Some("unblocked".to_string());
        }
        (page.blocked > 0).then(|| format!("{} blocked", page.blocked))
    }
}

impl Intercept for Blocker {
    /// Answer a paused request; give a page attached `Fetch.enable`; keep
    /// the count's pages as they come, land and go.
    ///
    /// Only the pause is consumed: the rest is the main loop's news too.
    fn intercept(&self, message: &Json, wire: &Notifier) -> bool {
        let Some(method) = message.get("method").and_then(Json::as_str) else {
            return false;
        };
        let session = message.get("sessionId").and_then(Json::as_str);
        match method {
            "Fetch.requestPaused" => {
                let request = message
                    .path(&["params", "requestId"])
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                let url = message
                    .path(&["params", "request", "url"])
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                let document = (message
                    .path(&["params", "resourceType"])
                    .and_then(Json::as_str)
                    == Some("Document"))
                .then(|| message.path(&["params", "frameId"]).and_then(Json::as_str))
                .flatten();
                // A pause with no session would be a browser-wide `Fetch`,
                // which nothing here enables; let it go rather than hang it.
                let verdict = session.map_or(Verdict::Continue, |session| {
                    self.decide(session, url, document)
                });
                let id = ("requestId", Json::string(request));
                let _ = match verdict {
                    Verdict::Block => wire.on(session).notify(
                        "Fetch.failRequest",
                        Json::object(vec![id, ("errorReason", Json::string("BlockedByClient"))]),
                    ),
                    Verdict::Continue => wire
                        .on(session)
                        .notify("Fetch.continueRequest", Json::object(vec![id])),
                };
                true
            }
            "Target.attachedToTarget" => {
                let kind = message
                    .path(&["params", "targetInfo", "type"])
                    .and_then(Json::as_str);
                let attached = message
                    .path(&["params", "sessionId"])
                    .and_then(Json::as_str);
                if let (Some("page" | "iframe"), Some(attached)) = (kind, attached) {
                    let _ = wire.on(Some(attached)).notify("Fetch.enable", patterns());
                    let info = |key: &str| {
                        message
                            .path(&["params", "targetInfo", key])
                            .and_then(Json::as_str)
                            .unwrap_or_default()
                    };
                    self.attached(attached, info("targetId"), info("url"));
                }
                false
            }
            "Target.detachedFromTarget" => {
                if let Some(gone) = message
                    .path(&["params", "sessionId"])
                    .and_then(Json::as_str)
                {
                    self.detached(gone);
                }
                false
            }
            "Page.frameNavigated" => {
                let frame = message.path(&["params", "frame"]);
                let main = frame.is_some_and(|frame| frame.get("parentId").is_none());
                if let (true, Some(session), Some(frame)) = (main, session, frame) {
                    // An error page's `url` is the engine's own; the page it
                    // stands for is `unreachableUrl`, and that is the site an
                    // `alt+b` on it means.
                    let url = frame
                        .get("unreachableUrl")
                        .or_else(|| frame.get("url"))
                        .and_then(Json::as_str)
                        .unwrap_or_default();
                    self.landed(session, url);
                }
                false
            }
            _ => false,
        }
    }
}

/// What `alt+b` says on a page with no site: `about:blank`, `data:`, a file.
pub const NO_SITE: &str = "this page has no site to unblock";

/// What `alt+b` says when there is nothing to block with.
pub const NO_LISTS: &str = "nothing is blocked: no block-list is set";

/// What `alt+b` says it did.
pub fn toggled(site: &str, unblocked: bool) -> String {
    if unblocked {
        format!("blocking off for {site}")
    } else {
        format!("blocking on for {site}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A scratch directory of this test's own, gone again before and after.
    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blinkterm-block-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn hosts(text: &str) -> HashSet<Box<str>> {
        let mut hosts = HashSet::new();
        parse(text, &mut hosts);
        hosts
    }

    fn sorted(hosts: &HashSet<Box<str>>) -> Vec<&str> {
        let mut all: Vec<&str> = hosts.iter().map(|host| &**host).collect();
        all.sort_unstable();
        all
    }

    #[test]
    fn a_hosts_file_gives_its_names_and_not_its_comments_or_its_boilerplate() {
        let text = "\
# Title: a list
127.0.0.1 localhost
127.0.0.1 localhost.localdomain
255.255.255.255 broadcasthost
::1 localhost ip6-localhost ip6-loopback
fe80::1%lo0 localhost
0.0.0.0 0.0.0.0

0.0.0.0 ads.example.com   # an inline comment
0.0.0.0\tTracker.Example.NET. pixel.example.org
127.0.0.1 *.wild.example
   # indented comment
";
        let mut into = HashSet::new();
        let parsed = parse(text, &mut into);
        assert_eq!(
            sorted(&into),
            [
                "ads.example.com",
                "pixel.example.org",
                "tracker.example.net",
                "wild.example"
            ]
        );
        assert_eq!(
            parsed,
            Parsed {
                hosts: 4,
                skipped: 0
            }
        );
    }

    #[test]
    fn a_plain_list_is_one_host_a_line_and_another_syntax_is_skipped() {
        let text = "ads.example.com\n\nads.example.com\n||rule.example^\nexample.com/path\ntwo names.example\n";
        let mut into = HashSet::new();
        let parsed = parse(text, &mut into);
        assert_eq!(sorted(&into), ["ads.example.com"]);
        assert_eq!(
            parsed,
            Parsed {
                hosts: 1,
                skipped: 3
            },
            "a repeat is not counted twice"
        );
        assert_eq!(parse_line("# nothing"), Vec::<Field>::new());
        assert_eq!(parse_line("localhost"), [Field::Boilerplate]);
        assert_eq!(parse_line("10.0.0.1"), [Field::Boilerplate]);
        assert_eq!(parse_line("a..b"), [Field::Junk]);
    }

    #[test]
    fn a_listed_host_blocks_itself_and_its_subdomains_and_nothing_beside() {
        let hosts = hosts("0.0.0.0 ads.example.com\n");
        for host in [
            "ads.example.com",
            "x.ads.example.com",
            "a.b.ads.example.com",
        ] {
            assert!(listed(&hosts, host), "{host}");
        }
        for host in [
            "notads.example.com",
            "example.com",
            "com",
            "ads.example.co",
            "",
        ] {
            assert!(!listed(&hosts, host), "{host}");
        }
    }

    #[test]
    fn a_urls_host_loses_its_port_its_user_and_its_case_and_an_address_is_none() {
        let cases = [
            (
                "https://Ads.Example.com:8443/x.js?y#z",
                Some("ads.example.com"),
            ),
            ("http://user:pw@ads.example.com/", Some("ads.example.com")),
            ("wss://ads.example.com./socket", Some("ads.example.com")),
            ("HTTP://ads.example.com", Some("ads.example.com")),
            ("http://127.0.0.1:8000/", None),
            ("http://[::1]:8000/", None),
            ("data:text/html,x", None),
            ("about:blank", None),
            ("blob:https://ads.example.com/uuid", None),
            ("", None),
        ];
        for (url, host) in cases {
            assert_eq!(host_of(url).as_deref(), host, "{url:?}");
        }
        assert_eq!(
            site_of("http://127.0.0.1:8000/").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(site_of("http://[::1]/").as_deref(), Some("::1"));
        assert_eq!(site_of("file:///tmp/x.html"), None);
    }

    fn blocker(unblocked: &[&str]) -> Blocker {
        Blocker::new(
            hosts("0.0.0.0 ads.test\n"),
            unblocked.iter().map(|site| site.to_string()),
        )
    }

    #[test]
    fn requests_are_counted_per_page_and_a_landing_starts_the_count_again() {
        let blocker = blocker(&[]);
        blocker.attached("S1", "T1", "about:blank");
        blocker.attached("S2", "T2", "about:blank");
        assert_eq!(blocker.words(Some("S1")), None, "nothing blocked yet");
        blocker.landed("S1", "http://news.example/");
        for _ in 0..3 {
            assert_eq!(
                blocker.decide("S1", "http://x.ads.test/pixel.gif", None),
                Verdict::Block
            );
        }
        assert_eq!(
            blocker.decide("S1", "http://news.example/story.css", None),
            Verdict::Continue
        );
        assert_eq!(
            blocker.decide("S2", "https://ads.test/a.js", None),
            Verdict::Block
        );
        assert_eq!(blocker.words(Some("S1")).as_deref(), Some("3 blocked"));
        assert_eq!(blocker.words(Some("S2")).as_deref(), Some("1 blocked"));
        assert_eq!(blocker.words(None), None);

        blocker.landed("S1", "http://news.example/next");
        assert_eq!(blocker.words(Some("S1")), None);
        blocker.detached("S2");
        assert_eq!(blocker.words(Some("S2")), None);
        assert_eq!(
            blocker.decide("S1", "http://ads.test/", None),
            Verdict::Block
        );
        blocker.forget_all();
        assert_eq!(blocker.words(Some("S1")), None);
    }

    #[test]
    fn an_unblocked_site_is_neither_blocked_nor_counted_and_says_so() {
        let blocker = blocker(&["news.example"]);
        blocker.attached("S1", "T1", "http://news.example/");
        assert_eq!(
            blocker.decide("S1", "http://x.ads.test/pixel.gif", None),
            Verdict::Continue
        );
        assert_eq!(blocker.words(Some("S1")).as_deref(), Some("unblocked"));

        blocker.set_unblocked("news.example", false);
        assert_eq!(
            blocker.decide("S1", "http://x.ads.test/pixel.gif", None),
            Verdict::Block
        );
        assert_eq!(blocker.words(Some("S1")).as_deref(), Some("1 blocked"));

        // The page's own document is judged by its own site, not by the one
        // being left: an unblocked site's link to a listed one is still
        // blocked, and unblocking the listed site — `alt+b` on its error
        // page — lets it through.
        blocker.set_unblocked("news.example", true);
        assert_eq!(
            blocker.decide("S1", "http://ads.test/", Some("T1")),
            Verdict::Block
        );
        assert_eq!(
            blocker.decide("S1", "http://ads.test/frame.html", Some("F9")),
            Verdict::Continue,
            "an iframe's document is the page's to allow"
        );
        blocker.set_unblocked("news.example", false);
        blocker.set_unblocked("ads.test", true);
        assert_eq!(
            blocker.decide("S1", "http://ads.test/", Some("T1")),
            Verdict::Continue
        );
        assert_eq!(
            blocker.decide("S1", "http://ads.test/a.js", None),
            Verdict::Block,
            "a subresource is the page's to allow"
        );
    }

    /// A pipe the test reads the hook's answers from: the browser's client
    /// for its notifier, the exchange, where the commands arrive, and the
    /// engine's writing end, held so that the reader sees no end of file.
    fn wired() -> (
        crate::cdp::Client,
        Arc<crate::cdp::Exchange>,
        std::io::PipeReader,
        std::io::PipeWriter,
    ) {
        use std::os::unix::io::IntoRawFd;
        let (commands, ours_write) = std::io::pipe().expect("a pipe");
        let (ours_read, replies) = std::io::pipe().expect("a pipe");
        let exchange =
            crate::cdp::Exchange::over(ours_read.into_raw_fd(), ours_write.into_raw_fd());
        let browser = crate::cdp::Client::browser(&exchange).expect("a browser client");
        (browser, exchange, commands, replies)
    }

    fn next_command(commands: &mut std::io::PipeReader) -> Json {
        use std::io::Read;
        let mut text = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            commands.read_exact(&mut byte).expect("a command");
            if byte[0] == 0 {
                break;
            }
            text.push(byte[0]);
        }
        Json::parse(std::str::from_utf8(&text).expect("text")).expect("JSON")
    }

    fn event(text: &str) -> Json {
        Json::parse(text).expect("the test's own JSON")
    }

    #[test]
    fn the_hook_enables_fetch_on_a_page_attached_and_answers_its_pauses() {
        let (browser, exchange, mut commands, _replies) = wired();
        let wire = browser.notifier();
        let blocker = blocker(&[]);

        let attached = event(
            r#"{"method":"Target.attachedToTarget","params":{"sessionId":"S1","targetInfo":{"targetId":"F","type":"page","url":"about:blank"}}}"#,
        );
        assert!(
            !blocker.intercept(&attached, &wire),
            "still the loop's news"
        );
        let enable = next_command(&mut commands);
        assert_eq!(
            enable.get("method").and_then(Json::as_str),
            Some("Fetch.enable")
        );
        assert_eq!(enable.get("sessionId").and_then(Json::as_str), Some("S1"));
        assert_eq!(enable.get("params"), Some(&patterns()));

        // A worker is not a page and is left alone.
        let worker = event(
            r#"{"method":"Target.attachedToTarget","params":{"sessionId":"W1","targetInfo":{"type":"service_worker","url":""}}}"#,
        );
        assert!(!blocker.intercept(&worker, &wire));

        let landed = event(
            r#"{"method":"Page.frameNavigated","sessionId":"S1","params":{"frame":{"id":"F","url":"http://news.example/"}}}"#,
        );
        assert!(!blocker.intercept(&landed, &wire));
        let paused = event(
            r#"{"method":"Fetch.requestPaused","sessionId":"S1","params":{"requestId":"r1","resourceType":"Image","request":{"url":"http://x.ads.test/p.gif"}}}"#,
        );
        assert!(blocker.intercept(&paused, &wire), "a pause is consumed");
        let failed = next_command(&mut commands);
        assert_eq!(
            failed.get("method").and_then(Json::as_str),
            Some("Fetch.failRequest")
        );
        assert_eq!(failed.get("sessionId").and_then(Json::as_str), Some("S1"));
        assert_eq!(
            failed.path(&["params", "requestId"]).and_then(Json::as_str),
            Some("r1")
        );
        assert_eq!(
            failed
                .path(&["params", "errorReason"])
                .and_then(Json::as_str),
            Some("BlockedByClient")
        );
        let allowed = event(
            r#"{"method":"Fetch.requestPaused","sessionId":"S1","params":{"requestId":"r2","request":{"url":"http://news.example/a.css"}}}"#,
        );
        assert!(blocker.intercept(&allowed, &wire));
        let continued = next_command(&mut commands);
        assert_eq!(
            continued.get("method").and_then(Json::as_str),
            Some("Fetch.continueRequest")
        );
        assert_eq!(blocker.words(Some("S1")).as_deref(), Some("1 blocked"));

        // An iframe's landing is not the page's.
        let framed = event(
            r#"{"method":"Page.frameNavigated","sessionId":"S1","params":{"frame":{"id":"G","parentId":"F","url":"http://other.example/"}}}"#,
        );
        assert!(!blocker.intercept(&framed, &wire));
        assert_eq!(blocker.words(Some("S1")).as_deref(), Some("1 blocked"));

        // An error page's landing is the site it stands for.
        let error = event(
            r#"{"method":"Page.frameNavigated","sessionId":"S1","params":{"frame":{"id":"F","url":"chrome-error://chromewebdata/","unreachableUrl":"http://ads.test/"}}}"#,
        );
        assert!(!blocker.intercept(&error, &wire));
        blocker.set_unblocked("ads.test", true);
        assert_eq!(blocker.words(Some("S1")).as_deref(), Some("unblocked"));

        let detached =
            event(r#"{"method":"Target.detachedFromTarget","params":{"sessionId":"S1"}}"#);
        assert!(!blocker.intercept(&detached, &wire));
        assert_eq!(blocker.words(Some("S1")), None);
        drop(browser);
        exchange.shutdown();
    }

    #[test]
    fn the_unblocked_file_toggles_folds_and_is_readable_by_its_owner_alone() {
        let dir = scratch("file");
        let mut unblocked = Unblocked::load(&dir);
        assert!(!unblocked.contains("example.com"), "no file");
        assert_eq!(unblocked.toggle("example.com"), Ok(true));
        assert_eq!(unblocked.toggle("127.0.0.1"), Ok(true));
        assert_eq!(unblocked.toggle("example.com"), Ok(false));
        let file = std::fs::read_to_string(dir.join(FILE)).expect("a file");
        assert_eq!(file, "example.com\toff\n127.0.0.1\toff\nexample.com\ton\n");
        let mode = std::fs::metadata(dir.join(FILE))
            .expect("the file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "a list of sites visited nobody else may read");

        let again = Unblocked::load(&dir);
        assert!(!again.contains("example.com"));
        assert!(again.contains("127.0.0.1"));
        assert_eq!(again.sites().collect::<Vec<_>>(), ["127.0.0.1"]);

        // A line cut short, and a line that is not one, are not read.
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join(FILE))
            .expect("the file");
        file.write_all(b"junk\nexample.org\tof").expect("written");
        drop(file);
        let again = Unblocked::load(&dir);
        assert!(!again.contains("example.org"));
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE)).expect("a file"),
            "127.0.0.1\toff\n",
            "compacted to the sites unblocked"
        );

        // A long file is compacted on load, and the oldest go past the cap.
        let long: String = (0..COMPACT_AT)
            .map(|n| line(&format!("{n}.example"), true))
            .collect();
        std::fs::write(dir.join(FILE), long).expect("written");
        let loaded = Unblocked::load(&dir);
        assert_eq!(loaded.set.len(), CAP);
        assert!(!loaded.contains("0.example"));
        assert!(loaded.contains(&format!("{}.example", COMPACT_AT - 1)));
        let file = std::fs::read_to_string(dir.join(FILE)).expect("a file");
        assert_eq!(file.lines().count(), CAP);
        assert!(!dir.join("unblocked.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_in_memory_exception_writes_nothing() {
        let mut unblocked = Unblocked::in_memory();
        assert_eq!(unblocked.toggle("example.com"), Ok(true));
        assert!(unblocked.path.is_none());
        assert!(unblocked.contains("example.com"));
        assert_eq!(
            Unblocked::parse_line("example.com\toff"),
            Some(("example.com".to_string(), true))
        );
        for broken in ["", "example.com", "example.com\tmaybe", "\toff", "a b\toff"] {
            assert_eq!(Unblocked::parse_line(broken), None, "{broken:?}");
        }
    }

    #[test]
    fn lists_are_read_once_and_a_list_that_cannot_be_is_a_sentence() {
        let dir = scratch("lists");
        let none = Unblocked::in_memory();
        assert!(load(&Lists::default(), &none)
            .expect("nothing to read")
            .is_none());

        let first = dir.join("hosts");
        let second = dir.join("plain");
        std::fs::write(&first, "0.0.0.0 ads.example\n").expect("written");
        std::fs::write(&second, "tracker.example\n").expect("written");
        let lists = Lists {
            paths: vec![first.clone(), second.clone()],
            enabled: true,
        };
        let blocker = load(&lists, &none).expect("read").expect("a blocker");
        assert_eq!(blocker.hosts(), 2);

        let off = Lists {
            enabled: false,
            ..lists.clone()
        };
        assert!(load(&off, &none).expect("nothing to read").is_none());

        let missing = Lists {
            paths: vec![dir.join("missing")],
            enabled: true,
        };
        let why = load(&missing, &none).expect_err("a missing list");
        assert!(why.contains("cannot read the block-list"), "{why}");

        let easylist = dir.join("easylist.txt");
        std::fs::write(&easylist, "[Adblock Plus 2.0]\n||ads.example^\n").expect("written");
        let wrong = Lists {
            paths: vec![easylist],
            enabled: true,
        };
        let why = load(&wrong, &none).expect_err("a list in another syntax");
        assert!(why.contains("one host per line"), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_alt_b_says() {
        assert_eq!(toggled("example.com", true), "blocking off for example.com");
        assert_eq!(toggled("example.com", false), "blocking on for example.com");
    }
}
