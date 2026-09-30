//! The page's console: what it logged, what it threw, and what it failed to
//! fetch, kept per tab for [`crate::consolelist`] to show.
//!
//! A page that shows nothing because a script threw, or an image that is a
//! blank box because its request came back 404, looks the same from the
//! terminal as a page that is still loading. Desktop browsers answer that
//! with a console; this is the part of one that reads, and nothing that
//! evaluates.
//!
//! # Three sources, two domains
//!
//! - `console.*` calls: `Runtime.consoleAPICalled`, one per call, with the
//!   arguments as remote objects ([`format_args()`] makes them one line).
//! - Uncaught exceptions and unhandled rejections: `Runtime.exceptionThrown`.
//! - Requests that failed — a status of 400 and up, or a network error — and
//!   what the engine itself has to say (a blocked mixed-content image, a
//!   violation): `Log.entryAdded`, whose `source` is `network` for the
//!   first. The text is the engine's own sentence, with the url beside it:
//!   `Failed to load resource: the server responded with a status of 404
//!   (Not Found)` for a 404, `Failed to load resource:
//!   net::ERR_CONNECTION_REFUSED` for a port nobody listens on, and
//!   `net::ERR_BLOCKED_BY_CLIENT` for what [`crate::block`] failed — all
//!   three measured against chrome-headless-shell 153 for a subresource.
//!   (A 404 image on a `data:` page is `net::ERR_FAILED` instead: the
//!   response is blocked before its status is looked at.)
//!
//! `Network.enable` would say more about a failed request, and it is not
//! used, for the reason [`crate::load`] gives with its numbers: on a page of
//! 51 requests it is 341 events and 212 kB through a tab's mailbox that
//! holds 512 and drops the oldest. `Log` is one entry per failed resource
//! and nothing for anything that succeeded.
//!
//! A `Log.entryAdded` with `source: "javascript"` is dropped: the `Runtime`
//! events already carry whatever it would say, and keeping both would show
//! one exception twice. (Against chrome-headless-shell 153 the `Log` domain
//! sends none of these for a page's own console calls or exceptions; the
//! engine test counts one entry each, four in all for four things said.)
//!
//! # Recorded on the reader thread
//!
//! Every page's session is given `Runtime.enable` and `Log.enable` when it
//! is attached (`prepare_session` in the app), and every event those send is
//! taken by a [`Recorder`] on [`crate::cdp`]'s reader thread — an
//! [`Intercept`], as [`crate::block::Blocker`] is — and consumed there, so
//! that none of it reaches a tab's mailbox. That is the whole reason for
//! the thread: the mailbox holds 512 events and drops the oldest, and a page
//! whose inline script logs a thousand lines right after its
//! `Page.frameNavigated` would push that landing out, and with it the url,
//! the title and the load the rest of the program waits on. [`crate::hover`]
//! measured 200 `consoleAPICalled` in 2.6 s from a page that logs every ten
//! milliseconds and turned `Runtime.enable` down for exactly this; here the
//! events never see a mailbox. `Runtime`'s context events, which come only
//! because it is enabled and which nothing in the program reads, are
//! consumed with them. Two thousand lines logged by a document's own script
//! between its landing and its load leave the landing and the load event in
//! the mailbox and the newest thousand here (an engine test).
//!
//! `Runtime.enable` replays what the page said before it was asked, which
//! is right for a page opened by another and wrong for the engine's own
//! first page: on a Mac that is the directory listing Cocoa's `NO` opens,
//! whose script throws twenty-one times. So the app's `boot` enables the
//! first page's `Runtime` a second time as a call — the renderer answers it
//! after the first one's replay, which the reader has recorded by then in
//! pipe order — and [`Recorder::forget`]s what came before the answer.
//!
//! # What it costs
//!
//! The one change to what a page is told is that its console is heard, and
//! a page looking for a debugger that way does not find one: none of the
//! getters pages set to catch an open DevTools is read, heard or not (an
//! engine test tries seven). What it can find is time — 200 logs of a
//! 1000-object array took 36 ms unheard and 53 ms heard, because a heard
//! console makes a preview of every object — which `docs/design.md` says.
//! A page's load, from landing to load event: a page of links 109 ms with
//! the console heard or not, a page that logs 500 lines 51 ms and 59 ms.
//!
//! The inspector keeps the objects a page logs for as long as `Runtime` is
//! on, in case they are asked about — 10 000 logged objects kept 3.0 MB of
//! the page's heap heard, against 0.4 MB unheard — and nothing here asks,
//! so the `console` object group is released on every landing and every
//! [`CAP`] calls, which gave all of it back.
//!
//! One thing is not bounded here: a line longer than the pipe's
//! [`MAX_MESSAGE`](crate::cdp::MAX_MESSAGE). `console.log('x'.repeat(7e7))`
//! is sent whole, 67 MB, and ends the pipe, which the program survives by
//! starting the engine again. That is the class an equally long
//! `document.title` is already in; a pipe that skips a message too long
//! rather than ending is a change to [`crate::cdp`] of its own. `console =
//! false` (`--no-console`) leaves `Runtime` and `Log` off for a person who
//! meets such a page, and then nothing here is ever sent anything.
//! Everything else is bounded: [`CAP`] entries per tab, each cut to
//! [`LINE_CAP`] characters.
//!
//! # What is kept
//!
//! The last [`CAP`] entries of each session, oldest first, in memory only:
//! nothing is written to disk, and a session's log goes when its tab does,
//! or the engine is relaunched. A navigation keeps the log, with a
//! separator row, `-- navigated to <url>`, because an error thrown just
//! before a redirect is often the one somebody came for. Every string is
//! passed through [`text::sanitize`] as it is recorded, and again when the
//! row is drawn: `console.log` is the most page-controlled text there is.
//!
//! The row counts errors the tab in front has logged since its console was
//! last opened: `2 errors`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::cdp::{Intercept, Notifier};
use crate::json::Json;
use crate::text;

/// How many entries a session keeps; the oldest goes past it.
pub const CAP: usize = 1000;

/// How many characters of one entry's text are kept; the rest is `…`.
pub const LINE_CAP: usize = 1000;

/// How loud an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Log,
    Info,
    Warn,
    Error,
}

impl Level {
    /// The word a row starts with.
    pub fn word(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Log => "log",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// Where an entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A `console.*` call.
    Console,
    /// An uncaught exception or unhandled rejection.
    Exception,
    /// A request that failed.
    Network,
    /// Anything else the engine logged about the page.
    Browser,
    /// Not the page's: the separator a landing puts in the log.
    Navigation,
}

/// One line of the console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub level: Level,
    pub source: Source,
    /// What was said, on one line: sanitized, at most [`LINE_CAP`]
    /// characters and a `…`.
    pub text: String,
    /// Where: `url:line`, a url, or nothing. Sanitized and cut the same way.
    pub place: String,
}

impl Entry {
    /// The word a row starts with: the level's, or `--` for a separator.
    pub fn lead(&self) -> &'static str {
        match self.source {
            Source::Navigation => "--",
            _ => self.level.word(),
        }
    }

    /// An entry, with both strings cut and sanitized.
    fn new(level: Level, source: Source, text: &str, place: &str) -> Entry {
        Entry {
            level,
            source,
            text: kept(text),
            place: kept(place),
        }
    }
}

/// A string as an entry keeps it: at most [`LINE_CAP`] characters, then
/// sanitized.
fn kept(text: &str) -> String {
    let cut = match text.char_indices().nth(LINE_CAP) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    };
    text::sanitize(&cut).into_owned()
}

/// One CDP event as an entry, or `None` for an event that is not one.
///
/// Pure, so that every rule in the module's docs is a unit test.
pub fn entry_of(method: &str, params: &Json) -> Option<Entry> {
    match method {
        "Runtime.consoleAPICalled" => {
            let level = match params.get("type").and_then(Json::as_str)? {
                "profile" | "profileEnd" | "clear" => return None,
                "error" | "assert" => Level::Error,
                "warning" => Level::Warn,
                "info" | "count" | "timeEnd" => Level::Info,
                "debug" => Level::Debug,
                _ => Level::Log,
            };
            let args = params
                .get("args")
                .and_then(Json::as_array)
                .unwrap_or_default();
            let place = params
                .path(&["stackTrace", "callFrames"])
                .and_then(Json::as_array)
                .and_then(|frames| frames.first())
                .map(frame_place)
                .unwrap_or_default();
            Some(Entry::new(
                level,
                Source::Console,
                &format_args(args),
                &place,
            ))
        }
        "Runtime.exceptionThrown" => {
            let details = params.get("exceptionDetails")?;
            let said = details
                .get("text")
                .and_then(Json::as_str)
                .unwrap_or_default();
            let thrown = details.get("exception").map(|exception| {
                match exception.get("description").and_then(Json::as_str) {
                    Some(description) => first_line(description).to_string(),
                    None => format_args(std::slice::from_ref(exception)),
                }
            });
            let text = match thrown {
                Some(thrown) if !thrown.is_empty() => {
                    if said.is_empty() {
                        thrown
                    } else {
                        format!("{said} {thrown}")
                    }
                }
                _ => said.to_string(),
            };
            let url = details
                .get("url")
                .and_then(Json::as_str)
                .unwrap_or_default();
            let place = if url.is_empty() {
                details
                    .path(&["stackTrace", "callFrames"])
                    .and_then(Json::as_array)
                    .and_then(|frames| frames.first())
                    .map(frame_place)
                    .unwrap_or_default()
            } else {
                with_line(url, details.get("lineNumber"))
            };
            Some(Entry::new(Level::Error, Source::Exception, &text, &place))
        }
        "Log.entryAdded" => {
            let entry = params.get("entry")?;
            let source = entry.get("source").and_then(Json::as_str);
            if source == Some("javascript") {
                return None;
            }
            let level = match entry.get("level").and_then(Json::as_str) {
                Some("verbose") => Level::Debug,
                Some("warning") => Level::Warn,
                Some("error") => Level::Error,
                _ => Level::Info,
            };
            let source = if source == Some("network") {
                Source::Network
            } else {
                Source::Browser
            };
            let text = entry.get("text").and_then(Json::as_str).unwrap_or_default();
            let place = match entry.get("url").and_then(Json::as_str) {
                Some(url) if !url.is_empty() => with_line(url, entry.get("lineNumber")),
                _ => String::new(),
            };
            Some(Entry::new(level, source, text, &place))
        }
        _ => None,
    }
}

/// A stack frame's `url:line`, the line counted from one as a person does;
/// the url alone when it has no line, and nothing when it has no url.
fn frame_place(frame: &Json) -> String {
    match frame.get("url").and_then(Json::as_str) {
        Some(url) if !url.is_empty() => with_line(url, frame.get("lineNumber")),
        _ => String::new(),
    }
}

/// `url:line` for a zero-based `line`, or the url when there is none.
fn with_line(url: &str, line: Option<&Json>) -> String {
    match line.and_then(Json::as_i64) {
        Some(line) if line >= 0 => format!("{url}:{}", line + 1),
        _ => url.to_string(),
    }
}

/// The first line of a description: an error's message without its stack.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// A `console.*` call's arguments as one line, the way a console prints
/// them: a string as itself, a number as a number, an object as its preview
/// (`{a: 1, b: "x", …}`, `[1, 2, …]`), one space between.
///
/// No `%s`/`%c` substitution: a format string is shown as it was written,
/// followed by the arguments it would have taken.
pub fn format_args(args: &[Json]) -> String {
    args.iter().map(remote).collect::<Vec<_>>().join(" ")
}

/// One `RemoteObject` as text.
fn remote(object: &Json) -> String {
    if let Some(value) = object.get("value") {
        match value {
            Json::String(text) => return text.clone(),
            Json::Number(_) | Json::Bool(_) => return scalar(value),
            _ => {}
        }
    }
    if let Some(value) = object.get("unserializableValue").and_then(Json::as_str) {
        return value.to_string();
    }
    let kind = object
        .get("type")
        .and_then(Json::as_str)
        .unwrap_or_default();
    let subtype = object.get("subtype").and_then(Json::as_str);
    if kind == "undefined" {
        return "undefined".to_string();
    }
    if subtype == Some("null") {
        return "null".to_string();
    }
    let description = object
        .get("description")
        .and_then(Json::as_str)
        .unwrap_or_default();
    if subtype == Some("error") || kind == "function" {
        return first_line(description).to_string();
    }
    match object.get("preview") {
        Some(preview) => previewed(preview, description),
        None => description.to_string(),
    }
}

/// An object's preview: `{a: 1, b: "x", …}`, an array's `[1, 2, …]`, with
/// the class in front of an object that is not a plain one.
fn previewed(preview: &Json, description: &str) -> String {
    let array = preview.get("subtype").and_then(Json::as_str) == Some("array");
    let mut parts: Vec<String> = preview
        .get("properties")
        .and_then(Json::as_array)
        .unwrap_or_default()
        .iter()
        .map(|property| {
            let value = property_value(property);
            if array {
                value
            } else {
                let name = property
                    .get("name")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                format!("{name}: {value}")
            }
        })
        .collect();
    if preview.get("overflow").and_then(Json::as_bool) == Some(true) {
        parts.push("…".to_string());
    }
    let inside = parts.join(", ");
    if array {
        format!("[{inside}]")
    } else if description.is_empty() || description == "Object" {
        format!("{{{inside}}}")
    } else {
        format!("{description} {{{inside}}}")
    }
}

/// One property in a preview: a string quoted, anything else as the engine
/// wrote it.
fn property_value(property: &Json) -> String {
    let kind = property
        .get("type")
        .and_then(Json::as_str)
        .unwrap_or_default();
    match property.get("value").and_then(Json::as_str) {
        Some(value) if kind == "string" => format!("\"{value}\""),
        Some(value) => value.to_string(),
        None if kind == "accessor" => "(…)".to_string(),
        None => kind.to_string(),
    }
}

/// A JSON number or bool as JavaScript would print it.
fn scalar(value: &Json) -> String {
    match value {
        Json::Number(n) => format!("{n}"),
        Json::Bool(b) => format!("{b}"),
        _ => String::new(),
    }
}

/// One session's console.
#[derive(Debug, Default)]
struct Log {
    /// Oldest first, at most [`CAP`].
    entries: VecDeque<Entry>,
    /// Errors since the panel was last opened on this session.
    unseen_errors: u32,
    /// `console.*` calls since the page's `console` object group was last
    /// released.
    held: usize,
}

/// Every page's console, by `sessionId`.
///
/// Shared between the reader thread, which records every entry and every
/// landing as it is read, and the main loop, which copies a log out when
/// the panel opens and reads the row's count once a pass. One mutex, held
/// for a push or a copy at a time. A session is made the first time
/// anything is recorded on it, so a page attached before the recorder was
/// told about it — the first one, in `boot` — needs nothing.
#[derive(Debug, Default)]
pub struct Recorder {
    sessions: Mutex<HashMap<String, Log>>,
}

impl Recorder {
    /// A recorder with nothing recorded.
    pub fn new() -> Recorder {
        Recorder::default()
    }

    /// Keep `entry` in `session`'s log, pushing out the oldest past [`CAP`].
    pub fn record(&self, session: &str, entry: Entry) {
        if let Ok(mut sessions) = self.sessions.lock() {
            let log = sessions.entry(session.to_string()).or_default();
            if entry.level == Level::Error && entry.source != Source::Navigation {
                log.unseen_errors = log.unseen_errors.saturating_add(1);
            }
            if entry.source == Source::Console {
                log.held += 1;
            }
            log.entries.push_back(entry);
            while log.entries.len() > CAP {
                log.entries.pop_front();
            }
        }
    }

    /// `session`'s main frame landed on `url`: a separator, when there is
    /// anything for it to separate.
    pub fn landed(&self, session: &str, url: &str) {
        let empty = self
            .sessions
            .lock()
            .map(|sessions| {
                sessions
                    .get(session)
                    .is_none_or(|log| log.entries.is_empty())
            })
            .unwrap_or(true);
        if !empty {
            self.record(
                session,
                Entry::new(
                    Level::Info,
                    Source::Navigation,
                    &format!("navigated to {url}"),
                    "",
                ),
            );
        }
    }

    /// Whether `session`'s page is due a release of its `console` object
    /// group: once every [`CAP`] `console.*` calls, counted from the last.
    fn release_due(&self, session: &str) -> bool {
        let Ok(mut sessions) = self.sessions.lock() else {
            return false;
        };
        match sessions.get_mut(session) {
            Some(log) if log.held >= CAP => {
                log.held = 0;
                true
            }
            _ => false,
        }
    }

    /// `session` is gone, and its log with it.
    pub fn detached(&self, session: &str) {
        self.forget(session);
    }

    /// Everything `session` has said so far is not the person's to see: the
    /// engine's own first page, replayed by `Runtime.enable` (see the
    /// module's docs).
    pub fn forget(&self, session: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(session);
        }
    }

    /// The engine is new: none of the sessions known are.
    pub fn forget_all(&self) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.clear();
        }
    }

    /// A copy of `session`'s log, oldest first.
    pub fn entries(&self, session: &str) -> Vec<Entry> {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| {
                sessions
                    .get(session)
                    .map(|log| log.entries.iter().cloned().collect())
            })
            .unwrap_or_default()
    }

    /// The panel was opened on `session`: the errors so far are seen.
    pub fn opened(&self, session: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(log) = sessions.get_mut(session) {
                log.unseen_errors = 0;
            }
        }
    }

    /// What the row says about `session`: `1 error`, `2 errors`, or nothing
    /// when none arrived since the panel was last opened.
    pub fn words(&self, session: Option<&str>) -> Option<String> {
        let sessions = self.sessions.lock().ok()?;
        let count = sessions.get(session?)?.unseen_errors;
        match count {
            0 => None,
            1 => Some("1 error".to_string()),
            n => Some(format!("{n} errors")),
        }
    }
}

impl Intercept for Recorder {
    /// Record the console's events and consume them; keep the logs' pages
    /// as they land and go, and let those through.
    fn intercept(&self, message: &Json, wire: &Notifier) -> bool {
        let Some(method) = message.get("method").and_then(Json::as_str) else {
            return false;
        };
        let session = message.get("sessionId").and_then(Json::as_str);
        match method {
            "Runtime.consoleAPICalled" | "Runtime.exceptionThrown" | "Log.entryAdded" => {
                let params = message.get("params").unwrap_or(&Json::Null);
                if let (Some(session), Some(entry)) = (session, entry_of(method, params)) {
                    self.record(session, entry);
                    if self.release_due(session) {
                        release(wire, session);
                    }
                }
                true
            }
            // Only there because `Runtime` is enabled; nothing reads them.
            "Runtime.executionContextCreated"
            | "Runtime.executionContextDestroyed"
            | "Runtime.executionContextsCleared"
            | "Runtime.exceptionRevoked" => true,
            "Page.frameNavigated" => {
                let frame = message.path(&["params", "frame"]);
                let main = frame.is_some_and(|frame| frame.get("parentId").is_none());
                if let (true, Some(session), Some(frame)) = (main, session, frame) {
                    let url = frame
                        .get("unreachableUrl")
                        .or_else(|| frame.get("url"))
                        .and_then(Json::as_str)
                        .unwrap_or_default();
                    self.landed(session, url);
                    release(wire, session);
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
            _ => false,
        }
    }
}

/// Let the page's inspector drop the objects it keeps for the console's
/// arguments, which it holds for as long as `Runtime` is on in case they are
/// asked about — and nothing here ever asks. Measured against
/// chrome-headless-shell 153: 10 000 objects logged grew the page's heap by
/// 0.36 MB with the console not heard and by 2.96 MB heard, and releasing
/// the group took it back to where the unheard page was. Told, not asked,
/// from the reader thread, which must never wait on a reply.
fn release(wire: &Notifier, session: &str) {
    let _ = wire.on(Some(session)).notify(
        "Runtime.releaseObjectGroup",
        Json::object(vec![("objectGroup", Json::string("console"))]),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Json {
        Json::parse(text).expect("the test's own JSON")
    }

    fn logged(level: Level, text: &str) -> Entry {
        Entry {
            level,
            source: Source::Console,
            text: text.to_string(),
            place: String::new(),
        }
    }

    #[test]
    fn a_console_log_is_its_arguments_on_one_line_with_objects_from_their_previews() {
        let params = json(
            r#"{"type":"log","args":[
                {"type":"string","value":"hello"},
                {"type":"number","value":1,"description":"1"},
                {"type":"number","unserializableValue":"NaN","description":"NaN"},
                {"type":"boolean","value":true},
                {"type":"undefined"},
                {"type":"object","subtype":"null","value":null},
                {"type":"bigint","unserializableValue":"123n","description":"123n"},
                {"type":"object","className":"Object","description":"Object","preview":{
                    "type":"object","description":"Object","overflow":false,"properties":[
                        {"name":"a","type":"number","value":"2"},
                        {"name":"b","type":"string","value":"x"}]}},
                {"type":"object","subtype":"array","className":"Array","description":"Array(9)",
                 "preview":{"type":"object","subtype":"array","description":"Array(9)",
                    "overflow":true,"properties":[
                        {"name":"0","type":"number","value":"1"},
                        {"name":"1","type":"number","value":"2"}]}},
                {"type":"object","className":"Map","description":"Map(0)","preview":{
                    "type":"object","subtype":"map","description":"Map(0)",
                    "overflow":false,"properties":[]}},
                {"type":"function","className":"Function","description":"function f() {\n  return 1;\n}"},
                {"type":"object","subtype":"error","className":"Error",
                 "description":"Error: nope\n    at x.js:1:1"}
            ],"stackTrace":{"callFrames":[
                {"functionName":"","url":"https://a.example/app.js","lineNumber":41,"columnNumber":3}
            ]}}"#,
        );
        let entry = entry_of("Runtime.consoleAPICalled", &params).expect("an entry");
        assert_eq!(
            entry.text,
            "hello 1 NaN true undefined null 123n {a: 2, b: \"x\"} [1, 2, …] Map(0) {} \
             function f() { Error: nope"
        );
        assert_eq!(entry.place, "https://a.example/app.js:42");
        assert_eq!(entry.level, Level::Log);
        assert_eq!(entry.source, Source::Console);
        assert_eq!(entry.lead(), "log");

        for (kind, level) in [
            ("error", Some(Level::Error)),
            ("assert", Some(Level::Error)),
            ("warning", Some(Level::Warn)),
            ("info", Some(Level::Info)),
            ("count", Some(Level::Info)),
            ("timeEnd", Some(Level::Info)),
            ("debug", Some(Level::Debug)),
            ("dir", Some(Level::Log)),
            ("table", Some(Level::Log)),
            ("trace", Some(Level::Log)),
            ("startGroup", Some(Level::Log)),
            ("profile", None),
            ("profileEnd", None),
            ("clear", None),
        ] {
            let params = json(&format!(r#"{{"type":"{kind}","args":[]}}"#));
            assert_eq!(
                entry_of("Runtime.consoleAPICalled", &params).map(|entry| entry.level),
                level,
                "{kind}"
            );
        }
        // No stack: no place.
        let params = json(r#"{"type":"log","args":[{"type":"string","value":"x"}]}"#);
        assert_eq!(
            entry_of("Runtime.consoleAPICalled", &params).map(|entry| entry.place),
            Some(String::new())
        );
    }

    #[test]
    fn an_uncaught_exception_reads_as_its_first_line_with_where_it_was_thrown() {
        let params = json(
            r#"{"timestamp":1,"exceptionDetails":{"exceptionId":1,"text":"Uncaught",
                "lineNumber":0,"columnNumber":20,"url":"data:text/html,x",
                "exception":{"type":"object","subtype":"error","className":"Error",
                    "description":"Error: boom\n    at <anonymous>:1:21"}}}"#,
        );
        let entry = entry_of("Runtime.exceptionThrown", &params).expect("an entry");
        assert_eq!(entry.text, "Uncaught Error: boom");
        assert_eq!(entry.place, "data:text/html,x:1");
        assert_eq!(entry.level, Level::Error);
        assert_eq!(entry.source, Source::Exception);

        // A thrown string, and a place from the stack when there is no url.
        let params = json(
            r#"{"exceptionDetails":{"text":"Uncaught","lineNumber":0,"columnNumber":0,
                "exception":{"type":"string","value":"just a string"},
                "stackTrace":{"callFrames":[{"url":"https://a.example/x.js","lineNumber":9}]}}}"#,
        );
        let entry = entry_of("Runtime.exceptionThrown", &params).expect("an entry");
        assert_eq!(entry.text, "Uncaught just a string");
        assert_eq!(entry.place, "https://a.example/x.js:10");

        // Nothing but the text.
        let params = json(r#"{"exceptionDetails":{"text":"Uncaught SyntaxError"}}"#);
        let entry = entry_of("Runtime.exceptionThrown", &params).expect("an entry");
        assert_eq!(entry.text, "Uncaught SyntaxError");
        assert_eq!(entry.place, "");
    }

    #[test]
    fn a_failed_request_from_the_log_domain_is_an_error_with_its_url_and_a_javascript_entry_is_left_to_runtime(
    ) {
        let params = json(
            r#"{"entry":{"source":"network","level":"error",
                "text":"Failed to load resource: the server responded with a status of 404 (Not Found)",
                "url":"http://127.0.0.1:8000/404","timestamp":1}}"#,
        );
        let entry = entry_of("Log.entryAdded", &params).expect("an entry");
        assert_eq!(entry.level, Level::Error);
        assert_eq!(entry.source, Source::Network);
        assert!(entry.text.contains("404"), "{}", entry.text);
        assert_eq!(entry.place, "http://127.0.0.1:8000/404");

        let params = json(
            r#"{"entry":{"source":"security","level":"warning","text":"Mixed Content: x",
                "url":"https://a.example/","lineNumber":4}}"#,
        );
        let entry = entry_of("Log.entryAdded", &params).expect("an entry");
        assert_eq!(entry.level, Level::Warn);
        assert_eq!(entry.source, Source::Browser);
        assert_eq!(entry.place, "https://a.example/:5");

        let params = json(r#"{"entry":{"source":"other","level":"verbose","text":"v"}}"#);
        assert_eq!(
            entry_of("Log.entryAdded", &params).map(|entry| entry.level),
            Some(Level::Debug)
        );

        let params = json(r#"{"entry":{"source":"javascript","level":"error","text":"boom"}}"#);
        assert_eq!(entry_of("Log.entryAdded", &params), None);
        assert_eq!(entry_of("Page.loadEventFired", &Json::empty()), None);
    }

    #[test]
    fn a_message_with_an_escape_sequence_is_only_its_letters() {
        let params = json(
            r#"{"type":"log","args":[{"type":"string","value":"\u001b]0;pwned\u0007\u202emoc\nnext"}],
                "stackTrace":{"callFrames":[{"url":"https://a.example/\u001b[2J","lineNumber":0}]}}"#,
        );
        let entry = entry_of("Runtime.consoleAPICalled", &params).expect("an entry");
        assert_eq!(entry.text, "]0;pwnedmoc next");
        assert_eq!(entry.place, "https://a.example/[2J:1");
    }

    #[test]
    fn a_line_past_the_cap_is_cut_with_an_ellipsis() {
        let long = "é".repeat(LINE_CAP + 50);
        let params = json(&format!(
            r#"{{"type":"log","args":[{{"type":"string","value":"{long}"}}]}}"#
        ));
        let entry = entry_of("Runtime.consoleAPICalled", &params).expect("an entry");
        assert_eq!(entry.text.chars().count(), LINE_CAP + 1);
        assert!(entry.text.ends_with("é…"));
        let exact = "x".repeat(LINE_CAP);
        assert_eq!(kept(&exact), exact, "at the cap is not past it");
    }

    #[test]
    fn the_thousand_and_first_entry_pushes_the_first_out() {
        let recorder = Recorder::new();
        for n in 0..=CAP {
            recorder.record("S1", logged(Level::Log, &n.to_string()));
        }
        let entries = recorder.entries("S1");
        assert_eq!(entries.len(), CAP);
        assert_eq!(entries[0].text, "1");
        assert_eq!(entries[CAP - 1].text, CAP.to_string());
        assert!(recorder.entries("S2").is_empty());
    }

    #[test]
    fn errors_are_counted_until_the_panel_is_opened_and_the_row_words_them() {
        let recorder = Recorder::new();
        assert_eq!(recorder.words(Some("S1")), None);
        assert_eq!(recorder.words(None), None);
        recorder.record("S1", logged(Level::Warn, "careful"));
        assert_eq!(recorder.words(Some("S1")), None, "a warning is no error");
        recorder.record("S1", logged(Level::Error, "one"));
        assert_eq!(recorder.words(Some("S1")).as_deref(), Some("1 error"));
        recorder.record("S1", logged(Level::Error, "two"));
        assert_eq!(recorder.words(Some("S1")).as_deref(), Some("2 errors"));
        assert_eq!(recorder.words(Some("S2")), None, "per session");
        recorder.opened("S1");
        assert_eq!(recorder.words(Some("S1")), None);
        assert_eq!(recorder.entries("S1").len(), 3, "seen, not cleared");
        recorder.record("S1", logged(Level::Error, "three"));
        assert_eq!(recorder.words(Some("S1")).as_deref(), Some("1 error"));
    }

    #[test]
    fn a_landing_adds_a_separator_only_to_a_log_that_has_something_in_it() {
        let recorder = Recorder::new();
        recorder.landed("S1", "https://a.example/");
        assert!(recorder.entries("S1").is_empty());
        recorder.record("S1", logged(Level::Log, "hi"));
        recorder.landed("S1", "https://b.example/\x1b[2J");
        let entries = recorder.entries("S1");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].lead(), "--");
        assert_eq!(entries[1].text, "navigated to https://b.example/[2J");
        assert_eq!(entries[1].source, Source::Navigation);
        assert_eq!(recorder.words(Some("S1")), None);
    }

    #[test]
    fn a_detached_session_takes_its_log_with_it() {
        let recorder = Recorder::new();
        recorder.record("S1", logged(Level::Error, "a"));
        recorder.record("S2", logged(Level::Error, "b"));
        recorder.detached("S1");
        assert!(recorder.entries("S1").is_empty());
        assert_eq!(recorder.words(Some("S1")), None);
        assert_eq!(recorder.entries("S2").len(), 1);
        recorder.forget_all();
        assert!(recorder.entries("S2").is_empty());
    }

    /// The browser's notifier over a pipe nobody reads: the recorder never
    /// writes, so nothing need.
    fn wired() -> (
        crate::cdp::Client,
        std::sync::Arc<crate::cdp::Exchange>,
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

    #[test]
    fn a_landing_and_every_thousandth_call_let_the_page_drop_the_console_s_objects() {
        let (browser, exchange, mut commands, _replies) = wired();
        let wire = browser.notifier();
        let recorder = Recorder::new();
        let released = |command: &Json| {
            assert_eq!(
                command.get("method").and_then(Json::as_str),
                Some("Runtime.releaseObjectGroup")
            );
            assert_eq!(command.get("sessionId").and_then(Json::as_str), Some("S1"));
            assert_eq!(
                command
                    .path(&["params", "objectGroup"])
                    .and_then(Json::as_str),
                Some("console")
            );
        };
        let call = json(
            r#"{"method":"Runtime.consoleAPICalled","sessionId":"S1","params":{"type":"log","args":[]}}"#,
        );
        for _ in 0..CAP {
            assert!(recorder.intercept(&call, &wire));
        }
        released(&next_command(&mut commands));
        let landed = json(
            r#"{"method":"Page.frameNavigated","sessionId":"S1","params":{"frame":{"id":"F","url":"https://a.example/"}}}"#,
        );
        assert!(!recorder.intercept(&landed, &wire));
        released(&next_command(&mut commands));
        // The count starts again after a release.
        for _ in 0..CAP - 1 {
            assert!(recorder.intercept(&call, &wire));
        }
        assert!(!recorder.release_due("S1"));
        assert!(recorder.intercept(&call, &wire));
        released(&next_command(&mut commands));
        drop(browser);
        exchange.shutdown();
    }

    #[test]
    fn the_recorder_consumes_console_events_and_context_events_and_lets_the_rest_through() {
        let (browser, exchange, _commands, _replies) = wired();
        let wire = browser.notifier();
        let recorder = Recorder::new();

        let consumed = [
            r#"{"method":"Runtime.consoleAPICalled","sessionId":"S1","params":{"type":"error","args":[{"type":"string","value":"x"}]}}"#,
            r#"{"method":"Runtime.exceptionThrown","sessionId":"S1","params":{"exceptionDetails":{"text":"Uncaught"}}}"#,
            r#"{"method":"Log.entryAdded","sessionId":"S1","params":{"entry":{"source":"network","level":"error","text":"404"}}}"#,
            r#"{"method":"Log.entryAdded","sessionId":"S1","params":{"entry":{"source":"javascript","level":"error","text":"dup"}}}"#,
            r#"{"method":"Runtime.consoleAPICalled","sessionId":"S1","params":{"type":"clear","args":[]}}"#,
            r#"{"method":"Runtime.executionContextCreated","sessionId":"S1","params":{}}"#,
            r#"{"method":"Runtime.executionContextDestroyed","sessionId":"S1","params":{}}"#,
            r#"{"method":"Runtime.executionContextsCleared","sessionId":"S1","params":{}}"#,
            r#"{"method":"Runtime.exceptionRevoked","sessionId":"S1","params":{}}"#,
        ];
        for message in consumed {
            assert!(recorder.intercept(&json(message), &wire), "{message}");
        }
        assert_eq!(recorder.entries("S1").len(), 3);
        assert_eq!(recorder.words(Some("S1")).as_deref(), Some("3 errors"));

        let through = [
            r#"{"method":"Page.frameNavigated","sessionId":"S1","params":{"frame":{"id":"F","url":"https://a.example/"}}}"#,
            r#"{"method":"Page.frameNavigated","sessionId":"S1","params":{"frame":{"id":"G","parentId":"F","url":"https://ad.example/"}}}"#,
            r#"{"method":"Page.loadEventFired","sessionId":"S1","params":{}}"#,
            r#"{"id":4,"result":{}}"#,
        ];
        for message in through {
            assert!(!recorder.intercept(&json(message), &wire), "{message}");
        }
        let entries = recorder.entries("S1");
        assert_eq!(entries.len(), 4, "one separator, for the main frame only");
        assert_eq!(entries[3].text, "navigated to https://a.example/");

        let detached = json(
            r#"{"method":"Target.detachedFromTarget","params":{"sessionId":"S1","targetId":"T"}}"#,
        );
        assert!(!recorder.intercept(&detached, &wire));
        assert!(recorder.entries("S1").is_empty());
        drop(browser);
        exchange.shutdown();
    }
}
