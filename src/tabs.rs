//! The list of pages, and what the engine's `Target.*` events do to it.
//!
//! A tab here is a CDP *page target*: the engine's own unit of "a page", with
//! its own history and its own renderer, reached as a flattened session on the
//! one pipe the engine was started with — `Target.attachToTarget` with
//! `flatten: true`, and the session's id on every message to and from it.
//!
//! This used to say the opposite: that multiplexing was for a browser with a
//! hundred tabs, and that one WebSocket per tab — a thread and a pipe each —
//! kept [`crate::cdp::Client`] from having to become a router. What changed is
//! that the WebSocket went, and it went because it was a port every process on
//! the machine could drive the browser through. A pipe is one connection, so
//! the router was no longer optional; it is one reader thread for every tab
//! instead of one each, and [`crate::cdp::Client`] still shows the loop what it
//! always did — `call`, a queue of events, a pipe to `poll`.
//!
//! What follows from a session per tab is that a tab *is* its connection:
//! closing the tab drops it, which detaches the session, which is all the
//! tidying there is. Nothing here holds a client of its own, so this module is
//! generic over what a tab is connected by and its tests use numbers.
//!
//! # Only the active tab costs anything
//!
//! A screencast is a frame every sixteen milliseconds, and a background tab
//! that sent them would be a pane's worth of PNG encoded for nobody. So the
//! screencast is started on activation and stopped on deactivation, and a
//! background tab is a session sitting idle. What it still does is *exist*:
//! the page goes on running, and its `Page` events go on arriving in its own
//! mailbox, which is what keeps the strip's titles true without anything being
//! polled.
//!
//! # Where a title comes from, which is not where it looks like it should
//!
//! `Target.targetInfoChanged` on the browser connection carries a `title`, and
//! the obvious design is to take it: one event, every tab, no page asked
//! anything. It does not work, and the measurement is in this crate's engine
//! tests. Against `chromium-shell` the event fires on *navigation* and carries
//! a title derived from the url — `127.0.0.1:33403/second` — and a page that
//! sets `document.title` afterwards produces no further event at all. A strip
//! built on it shows urls where titles should be, and shows them forever.
//!
//! So the url comes from the browser connection, where it is right, and the
//! title is asked of the page, on the page's own connection, when the page
//! says it has finished loading. That is one `Runtime.evaluate` per load per
//! tab — event-driven rather than polled, and a background tab that never
//! loads anything costs nothing at all.

use std::borrow::Cow;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::cdp::Event;
use crate::dialog::Dialog;
use crate::json::Json;
use crate::load::{self, Landing, Loaded, Problem, Trust};
use crate::text;
use crate::upload::{Chooser, Upload};

/// One page target, and what the row says about it.
pub struct Tab<C> {
    /// The engine's id for the target. Every `Target.*` event names a tab by
    /// this, and it is what closes and activates one.
    pub target: String,
    /// The connection to that target: in the program a [`crate::cdp::Client`],
    /// in the tests whatever is cheap.
    pub connection: C,
    /// Plain text; see [`crate::text`]. Set from the parsers in
    /// [`crate::load`], which sanitize; a test that writes it directly is
    /// still caught by the row.
    pub title: String,
    /// Plain text; see [`crate::text`]. Set from the parsers in
    /// [`crate::load`] and [`change`], which sanitize; a test that writes it
    /// directly is still caught by the row.
    pub url: String,
    /// Whether the page is between a navigation and its load event.
    pub loading: bool,
    /// A sentence that stands in for the title until the page says something
    /// else: what it is loading, why a navigation failed, what happened to the
    /// tab that is no longer here.
    pub note: Option<String>,
    /// What is wrong with the page, when something is: it did not come, or it
    /// came with an error status.
    ///
    /// Not a [`Tab::note`], for two reasons. A 404 page has a title and a url
    /// that are both worth keeping on the row, and a note stands in for the
    /// title rather than beside it. And a note is wiped by the browser
    /// connection's rename of the tab, which arrives a millisecond after a
    /// failed page lands and, for a url the engine spells differently from the
    /// one that was typed — a trailing slash is enough — would clear the
    /// failure the moment it was said. [`Tabs::take`] does not touch this;
    /// only the tab's own `Page` events do. See [`crate::load`].
    pub problem: Option<Problem>,
    /// The dialog this page has open and nobody has answered.
    ///
    /// The page is stopped until somebody does: its renderer is inside the
    /// `alert()` that opened it, so it paints nothing and answers no
    /// `Runtime.evaluate`. It is kept on the tab rather than on the loop
    /// because a tab that is not in front can open one too, and it has to be
    /// there — marked in the strip — when the person goes to look.
    pub dialog: Option<Dialog>,
    /// The file input this page asked about and nobody has answered: a path
    /// being typed on the row. See [`crate::upload`].
    ///
    /// Beside [`Tab::dialog`] rather than in the same slot, because the two
    /// are not exclusive. A chooser does not stop the page, so a script can
    /// open an `alert()` while the path is half typed; the alert has the row
    /// then, since the page is stopped behind it, and the path is still here
    /// when it has been answered. Per tab, like a dialog, because a tab that
    /// is not in front gets the event too (measured), and the answer goes to
    /// that page's input.
    pub upload: Option<Upload>,
    /// The main frame's id, from `Page.getFrameTree` at attach and from every
    /// landing since: what tells this tab's own `Page.frameStartedLoading`
    /// and `Page.frameStoppedLoading` from an iframe's, which the engine
    /// sends in the same shape under the child's id. `None` until known, and
    /// then every frame's news is taken as the page's — see
    /// [`crate::load::is_main`].
    pub frame: Option<String>,
    /// When the load in progress was asked for, typed or clicked; the seconds
    /// on the row count from here. `None` when nothing is loading.
    pub since: Option<Instant>,
    /// Whether what is on the page is the document the last navigation asked
    /// for: `false` from the moment one leaves until its `Page.frameNavigated`.
    /// Until then the page's renderer answers nothing — the engine holds every
    /// command meant for it while a navigation that will swap it is pending,
    /// measured in [`crate::hover`] — so nothing is asked of it.
    pub committed: bool,
    /// Whether the document came over a transport the row should warn about.
    /// Set at each landing, from the landing; see [`crate::load::trust`].
    pub trust: Trust,
}

impl<C> Tab<C> {
    pub fn new(target: impl Into<String>, connection: C, url: impl Into<String>) -> Tab<C> {
        Tab {
            target: target.into(),
            connection,
            title: String::new(),
            url: url.into(),
            loading: false,
            note: None,
            problem: None,
            dialog: None,
            upload: None,
            frame: None,
            since: None,
            committed: true,
            trust: Trust::Plain,
        }
    }

    /// `Page.frameStartedNavigating` for the main frame: the page is leaving
    /// for `url`, typed or clicked, and nothing has come back yet.
    ///
    /// This is what makes a link clicked into a slow host say so at once
    /// rather than when its document commits — for a host that never answers,
    /// never — and what tells `esc` there is something to stop. The page on
    /// the screen is still the last one, so its url and title stay; the note
    /// stands in front of them. A typed url has already put up the same note
    /// and started the clock, and the clock is kept: the seconds count from
    /// the Enter, not from the engine's echo of it three milliseconds later.
    pub fn started(&mut self, url: String, now: Instant) {
        let note = format!("loading {url}");
        if self.loading && self.note.as_deref() == Some(note.as_str()) {
            self.since.get_or_insert(now);
        } else {
            self.since = Some(now);
            self.note = Some(note);
        }
        self.loading = true;
        self.committed = false;
        self.problem = None;
    }

    /// `Page.frameStoppedLoading` for the main frame: the load is over,
    /// however it ended.
    ///
    /// It fires in the same millisecond as the load event for a load that
    /// finished, after `Page.stopLoading` for one that was stopped — when no
    /// load event is ever coming — and for a navigation that was abandoned
    /// before anything came back: a clicked link that turned out to be a
    /// download, one the page overtook with another. In that last case the
    /// page on the screen never left, and the note that said it was leaving
    /// comes down with the load; the url was never changed for a click. The
    /// title is still asked for on the load event, which is where it is
    /// known; this only clears.
    pub fn stopped_loading(&mut self) {
        self.loading = false;
        self.since = None;
        if !self.committed {
            if self
                .note
                .as_deref()
                .is_some_and(|note| note.starts_with("loading "))
            {
                self.note = None;
            }
            self.committed = true;
        }
    }

    /// How long the load in progress has been going, in whole seconds, once
    /// it has been going for one. Below a second a number would flicker past
    /// on every page that is merely quick.
    pub fn loading_for(&self, now: Instant) -> Option<u64> {
        if !self.loading {
            return None;
        }
        let going = now.saturating_duration_since(self.since?);
        (going >= Duration::from_secs(1)).then_some(going.as_secs())
    }

    /// The main frame committed: a document, or the error page for one.
    ///
    /// This is `Page.frameNavigated`, and it is the one signal of a failure
    /// that arrives however the navigation started — typed, clicked, reloaded
    /// or walked to through history. What it does not carry is why, which
    /// only a `Page.navigate` reply says, and that arrives first, through
    /// [`Tab::failed_to_reach`]. So the question here is whether a reason
    /// already on the tab belongs to this landing.
    ///
    /// The reason names the url that was asked for and the landing names where
    /// the engine ended up — the same place in the engine's spelling, or the
    /// end of a redirect — so the two cannot simply be compared. What can be
    /// relied on instead is that between the reply and the landing nothing
    /// else happens to this tab: a reason recorded while the tab is still
    /// loading is this landing's. A reason for the very url that has landed
    /// again is kept too, which is what reloading an error page looks like.
    /// Anything else is dropped, because a wrong reason is worse than none.
    /// [`crate::app`]'s `navigate` clears the problem before it sends, which
    /// is what keeps "still loading" honest against a `Page.navigate` that
    /// timed out here and failed in the engine afterwards.
    pub fn landed(&mut self, landing: Landing) {
        // Read before it is set below: it says whether a reason from
        // `failed_to_reach` belongs to this landing.
        let ours = self.loading;
        // A page that has gone somewhere has not got there yet, and the title
        // it had was the last page's.
        self.title.clear();
        self.note = None;
        self.loading = true;
        // The input a path was being typed for has gone with its document. An
        // answer sent to it now would be taken and do nothing (measured), but
        // a question about an input that is not there is a lie on the row.
        self.upload = None;
        // The document is here, so the renderer answers again; and the clock
        // goes on from the departure when there was one, which a load the
        // engine began by itself — a redirect's second landing, a history
        // step whose departure was missed — did not have.
        self.committed = true;
        self.since.get_or_insert_with(Instant::now);
        match landing {
            Landing::Document(url) => {
                self.url = url;
                self.problem = None;
            }
            Landing::Unreachable(url) => {
                let reason = match &self.problem {
                    Some(Problem::Unreachable { url: was, reason }) if ours || *was == url => {
                        reason.clone()
                    }
                    _ => None,
                };
                // The url that did not come, and never the error page's own
                // `chrome-error://chromewebdata/`: that is where the engine
                // put the page, not where anybody was going.
                self.url = url.clone();
                self.problem = Some(Problem::Unreachable { url, reason });
            }
        }
    }

    /// `Page.navigate` answered with an `errorText`: the landing is on its way,
    /// ten to sixty milliseconds behind, and this is why.
    ///
    /// Or it has already come. The reply is collected a pass after it is sent
    /// rather than waited for — a navigation away from a page with a "leave
    /// this page?" is held by the engine until somebody answers, and a loop
    /// that waited could not show the question — so the error page's own
    /// events can be read first. Then the tab already says where it ended up
    /// and has had its load event, and all this adds is the reason: the url
    /// stays the landing's, which is the truer one after a redirect, and
    /// `loading` is left alone, since the load event it would wait for has
    /// been and gone. An unreachable problem on the tab can only be this
    /// navigation's, because `navigate` cleared it before sending.
    pub fn failed_to_reach(&mut self, url: &str, code: &str) {
        self.note = None;
        if let Some(Problem::Unreachable { reason, .. }) = &mut self.problem {
            reason.get_or_insert_with(|| code.to_string());
            return;
        }
        self.loading = true;
        self.problem = Some(Problem::Unreachable {
            url: url.to_string(),
            reason: Some(code.to_string()),
        });
    }

    /// The page said it has loaded, and this is what it answered.
    ///
    /// An error status replaces a status and nothing else: a page that did not
    /// come keeps saying so, because the error page it is showing has a status
    /// of 0 and no title, and that is not the news that the problem is over.
    pub fn loaded(&mut self, loaded: Loaded) {
        self.title = loaded.title;
        match loaded.status {
            Some(status) => self.problem = Some(Problem::Status(status)),
            None => {
                if matches!(self.problem, Some(Problem::Status(_))) {
                    self.problem = None;
                }
            }
        }
    }

    /// What a `Page.javascriptDialog*` event does to this tab. True when the
    /// row is now out of date.
    ///
    /// Opening replaces whatever was there, because a page has one dialog at
    /// a time: a second can only open once the first has been answered, and
    /// if the close of the first was lost the second is the truth. Closing
    /// clears it whoever answered — this program, or the engine itself when
    /// the page navigated or its renderer went away — so a dialog that was
    /// answered somewhere else is not left on the row as a question with
    /// nobody to ask.
    pub fn dialog_event(&mut self, event: &Event) -> bool {
        match event.method.as_str() {
            "Page.javascriptDialogOpening" => {
                self.dialog = Dialog::opening(&event.params);
                self.dialog.is_some()
            }
            "Page.javascriptDialogClosed" => self.dialog.take().is_some(),
            _ => false,
        }
    }

    /// What a `Page.fileChooserOpened` does to this tab: a prompt for a path,
    /// starting in `base`. True when the row is now out of date.
    ///
    /// A second click on the input a prompt is already up for comes with the
    /// same `backendNodeId` (measured), and it is the person clicking again,
    /// not a new question: the half-typed path is kept. A different input
    /// replaces it, since that is the one the person clicked last. An event
    /// with no input to give files to — `showOpenFilePicker()` — is said once
    /// on the note, so that the click did not silently do nothing.
    pub fn chooser_event(&mut self, event: &Event, base: &Path, home: Option<&Path>) -> bool {
        if event.method != "Page.fileChooserOpened" {
            return false;
        }
        let Some(chooser) = Chooser::opening(&event.params) else {
            self.note = Some("this page's file picker isn't supported".to_string());
            return true;
        };
        let same = self
            .upload
            .as_ref()
            .is_some_and(|upload| upload.chooser.backend_node_id == chooser.backend_node_id);
        if same {
            return false;
        }
        self.upload = Some(Upload::new(
            chooser,
            base.to_path_buf(),
            home.map(Path::to_path_buf),
        ));
        true
    }

    /// Whether the page is waiting on the person — a dialog, or a file
    /// input's path — which is what the strip marks with a `!`.
    pub fn asks(&self) -> bool {
        self.dialog.is_some() || self.upload.is_some()
    }

    /// The whole row, when this is the only tab there is.
    ///
    /// Unchanged from the browser that had no tabs, deliberately: one page in
    /// a pane is still the common case and it should look like it always did.
    pub fn line(&self) -> String {
        if let Some(note) = &self.note {
            return note.clone();
        }
        // A status is said beside the title and the url rather than instead
        // of them: a 404 page has both, and the site's own words for what went
        // wrong are usually in the title.
        let status = match &self.problem {
            Some(problem @ Problem::Unreachable { .. }) => return load::sentence(problem),
            Some(Problem::Status(status)) => Some(load::status_phrase(*status)),
            None => None,
        };
        // And `not secure` before the title, where it is read before the
        // page's own words for itself — which, on a page anybody on the way
        // could have rewritten, are exactly what it qualifies.
        let parts: Vec<&str> = [
            status.as_deref(),
            self.trust.words(),
            Some(&self.title),
            Some(&self.url),
        ]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect();
        if parts.is_empty() {
            return "blinkterm".to_string();
        }
        parts.join("  —  ")
    }

    /// The name this tab goes by in the strip, before it is clipped.
    ///
    /// A page that did not come is named for that, since the host and the
    /// reason are all there is to know about it. A status is not: a strip is
    /// narrow, and the title of a 404 page is usually the site saying so.
    pub fn label(&self) -> Cow<'_, str> {
        if let Some(note) = &self.note {
            return Cow::Borrowed(note);
        }
        if let Some(problem @ Problem::Unreachable { .. }) = &self.problem {
            return Cow::Owned(load::sentence(problem));
        }
        if !self.title.is_empty() {
            return Cow::Borrowed(&self.title);
        }
        // A tab that was just opened has the url it was opened with and no
        // title yet, and "about:blank" is not a name for anything.
        if self.url.is_empty() || self.url == "about:blank" {
            return Cow::Borrowed("new tab");
        }
        Cow::Borrowed(&self.url)
    }
}

/// The tabs, in the order they are shown, and which one is in front.
pub struct Tabs<C> {
    tabs: Vec<Tab<C>>,
    active: usize,
}

impl<C> Tabs<C> {
    /// The list a browser starts with: the page the engine already had.
    pub fn new(first: Tab<C>) -> Tabs<C> {
        Tabs {
            tabs: vec![first],
            active: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// True once the last tab has gone, which is when the program is over.
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> Option<&Tab<C>> {
        self.tabs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Tab<C>> {
        self.tabs.get_mut(self.active)
    }

    /// The active tab's target id, which is what a switch is measured against.
    pub fn active_target(&self) -> Option<&str> {
        self.active().map(|tab| tab.target.as_str())
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Tab<C>> {
        self.tabs.iter()
    }

    pub fn index_of(&self, target: &str) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.target == target)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut Tab<C>> {
        self.tabs.get_mut(index)
    }

    /// Add a tab at the end and make it the one in front.
    ///
    /// Which is what a desktop browser does for an open the person asked for,
    /// and a target with an opener is always one of those: a link they clicked
    /// or a `window.open` the click ran.
    pub fn open(&mut self, tab: Tab<C>) -> usize {
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.active
    }

    /// Take a tab out of the list and hand it back, so that the caller can
    /// close its connection where a failure can be reported.
    ///
    /// The tab that takes its place in front is the one to its right, or the
    /// new last one — the rule every browser uses, and the one that makes
    /// closing several tabs in a row feel like closing several tabs in a row.
    pub fn close(&mut self, index: usize) -> Option<Tab<C>> {
        if index >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        Some(tab)
    }

    /// Forwards, wrapping. `false` when there is nowhere else to be.
    pub fn select_next(&mut self) -> bool {
        if self.tabs.len() < 2 {
            return false;
        }
        self.active = (self.active + 1) % self.tabs.len();
        true
    }

    /// Backwards, wrapping.
    pub fn select_previous(&mut self) -> bool {
        if self.tabs.len() < 2 {
            return false;
        }
        self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        true
    }

    /// The nth tab, counted from one the way the strip numbers them.
    ///
    /// A number nobody has a tab for does nothing at all rather than choosing
    /// the nearest: `alt+7` with three tabs open is a typo, and moving to the
    /// third would hide that.
    pub fn select(&mut self, number: usize) -> bool {
        if number == 0 || number > self.tabs.len() {
            return false;
        }
        let wanted = number - 1;
        let moved = wanted != self.active;
        self.active = wanted;
        moved
    }

    /// Make `index` the tab in front.
    pub fn switch_to(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    /// What one event from the browser connection does to the list.
    ///
    /// `open` is asked for a connection only for a target that is becoming a
    /// tab, and may fail: an engine that will not attach is a sentence on the
    /// row, not a reason to stop.
    pub fn take(
        &mut self,
        event: &Event,
        open: impl FnOnce(&str) -> Result<C, String>,
    ) -> Outcome<C> {
        let Some(change) = change(event) else {
            return Outcome::Ignored;
        };
        match change {
            Change::Opened { target, url } => {
                if let Some(index) = self.index_of(&target) {
                    // Already ours — the engine says so twice when a target is
                    // created and then attached.
                    return if self.switch_to(index) {
                        Outcome::Opened
                    } else {
                        Outcome::Ignored
                    };
                }
                match open(&target) {
                    Ok(connection) => {
                        self.open(Tab::new(target, connection, url));
                        Outcome::Opened
                    }
                    Err(why) => Outcome::Failed(format!("that link wanted a new tab: {why}")),
                }
            }
            Change::Renamed { target, url } => {
                let Some(index) = self.index_of(&target) else {
                    return Outcome::Ignored;
                };
                let tab = &mut self.tabs[index];
                // An empty url in a target's information means the engine has
                // not decided yet, not that the page has no address.
                if url.is_empty() || tab.url == url {
                    return Outcome::Ignored;
                }
                tab.url = url;
                // A page that has gone somewhere has not got there yet, and
                // the title it had was the last page's. Both are filled in
                // again by that tab's own `Page` events.
                tab.title.clear();
                tab.note = None;
                Outcome::Renamed
            }
            Change::Closed { target } => self.gone(&target, None),
            Change::Crashed { target } => self.gone(
                &target,
                Some("the page in that tab stopped answering, so the tab is gone".to_string()),
            ),
        }
    }

    fn gone(&mut self, target: &str, why: Option<String>) -> Outcome<C> {
        match self.index_of(target) {
            Some(index) => match self.close(index) {
                Some(tab) => Outcome::Gone {
                    tab: Box::new(tab),
                    why,
                },
                None => Outcome::Ignored,
            },
            None => Outcome::Ignored,
        }
    }
}

/// What [`Tabs::take`] did.
pub enum Outcome<C> {
    /// Nothing the list cares about.
    Ignored,
    /// A tab was added and is now in front.
    Opened,
    /// A tab's title or url moved, so the row is out of date.
    Renamed,
    /// A tab is no longer in the list. Its connection comes back with it, to
    /// be closed by the caller, and `why` is the sentence to put on the row
    /// when the page did not simply close itself.
    Gone {
        tab: Box<Tab<C>>,
        why: Option<String>,
    },
    /// A target that should have become a tab could not be connected to.
    Failed(String),
}

/// What a `Target.*` event means, with nothing said about tabs.
///
/// Split out from [`Tabs::take`] so that the reading of the engine's JSON can
/// be tested against the JSON the engine sends, and the list's behaviour
/// against a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A page target somebody else opened: a link with `target=_blank`, or the
    /// `window.open` a click ran.
    Opened { target: String, url: String },
    /// A target's url is now this.
    ///
    /// The event carries a title as well and it is deliberately not read: see
    /// the module documentation for what the engine puts in it.
    Renamed { target: String, url: String },
    /// The target is gone, because the page closed itself or because something
    /// else closed it.
    Closed { target: String },
    /// The renderer behind the target died.
    Crashed { target: String },
}

/// Read one event, or decide it says nothing about the tabs.
///
/// A url comes out as plain text ([`crate::text::sanitize`]): it is the
/// engine's spelling of wherever the page went, the page chose where that
/// was, and it is about to be on the row.
pub fn change(event: &Event) -> Option<Change> {
    match event.method.as_str() {
        "Target.targetCreated" => {
            let info = event.params.get("targetInfo")?;
            if info.get("type").and_then(Json::as_str) != Some("page") {
                return None;
            }
            // Only a target with an opener. A target this program asked for
            // has none, and neither has the `about:blank` the engine started
            // with — both are already tabs by the time the event arrives, and
            // acting on it again would open the same page twice.
            //
            // All five ways a page can ask for a window were checked against
            // `chromium-shell` before this rule was trusted, because one of
            // them not carrying an opener would be a click that does nothing:
            // a `target=_blank` link, the same link with `rel=noopener`, a
            // `window.open` from a click handler, the same with `noopener`, and
            // a `window.open` from a script with no user gesture at all. Every
            // one of them arrives with `openerId` set. What differs between
            // them is `canAccessOpener`, which is about what the *page* may
            // reach and is none of this program's business. The url, on the
            // other hand, is empty in this event for all five: the target is
            // announced before it has an address, and the address comes along
            // afterwards as `Target.targetInfoChanged`.
            let opener = info.get("openerId").and_then(Json::as_str)?;
            if opener.is_empty() {
                return None;
            }
            Some(Change::Opened {
                target: info.get("targetId").and_then(Json::as_str)?.to_string(),
                url: text::sanitize(
                    info.get("url")
                        .and_then(Json::as_str)
                        .unwrap_or("about:blank"),
                )
                .into_owned(),
            })
        }
        "Target.targetInfoChanged" => {
            let info = event.params.get("targetInfo")?;
            if info.get("type").and_then(Json::as_str) != Some("page") {
                return None;
            }
            Some(Change::Renamed {
                target: info.get("targetId").and_then(Json::as_str)?.to_string(),
                url: text::sanitize(info.get("url").and_then(Json::as_str).unwrap_or_default())
                    .into_owned(),
            })
        }
        "Target.targetDestroyed" => Some(Change::Closed {
            target: event
                .params
                .get("targetId")
                .and_then(Json::as_str)?
                .to_string(),
        }),
        "Target.targetCrashed" => Some(Change::Crashed {
            target: event
                .params
                .get("targetId")
                .and_then(Json::as_str)?
                .to_string(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tab whose connection is a number, which is as much as the list needs
    /// to know about one.
    fn tab(target: &str, title: &str) -> Tab<u32> {
        let mut tab = Tab::new(target, 0, format!("https://{target}.example"));
        tab.title = title.to_string();
        tab
    }

    fn three() -> Tabs<u32> {
        let mut tabs = Tabs::new(tab("a", "A"));
        tabs.open(tab("b", "B"));
        tabs.open(tab("c", "C"));
        tabs.select(1);
        tabs
    }

    fn event(method: &str, params: &str) -> Event {
        Event {
            method: method.to_string(),
            params: Json::parse(params).expect("the test's own JSON"),
        }
    }

    fn titles(tabs: &Tabs<u32>) -> Vec<&str> {
        tabs.iter().map(|tab| tab.title.as_str()).collect()
    }

    #[test]
    fn opening_a_tab_puts_it_at_the_end_and_in_front() {
        let mut tabs = Tabs::new(tab("a", "A"));
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs.active_index(), 0);
        tabs.open(tab("b", "B"));
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active_index(), 1, "a new tab is switched to");
        assert_eq!(tabs.active_target(), Some("b"));
    }

    #[test]
    fn closing_the_active_tab_shows_the_one_to_its_right() {
        let mut tabs = three();
        assert_eq!(tabs.active_index(), 0);
        tabs.select(2);
        assert_eq!(tabs.close(1).map(|tab| tab.target), Some("b".to_string()));
        assert_eq!(titles(&tabs), ["A", "C"]);
        assert_eq!(tabs.active_target(), Some("c"), "the one to the right");

        // And the last tab in the list falls back to the new last one.
        let mut tabs = three();
        tabs.select(3);
        tabs.close(2);
        assert_eq!(tabs.active_target(), Some("b"));
    }

    #[test]
    fn closing_a_tab_before_the_active_one_keeps_the_active_one() {
        let mut tabs = three();
        tabs.select(3);
        tabs.close(0);
        assert_eq!(titles(&tabs), ["B", "C"]);
        assert_eq!(tabs.active_target(), Some("c"), "still the same page");
    }

    #[test]
    fn closing_the_last_tab_leaves_nothing() {
        let mut tabs = Tabs::new(tab("a", "A"));
        assert!(tabs.close(0).is_some());
        assert!(tabs.is_empty());
        assert!(tabs.active().is_none());
        assert_eq!(tabs.active_target(), None);
        assert!(
            tabs.close(0).is_none(),
            "and there is nothing left to close"
        );
    }

    #[test]
    fn next_and_previous_wrap_in_both_directions() {
        let mut tabs = three();
        assert_eq!(tabs.active_index(), 0);
        assert!(tabs.select_next());
        assert_eq!(tabs.active_index(), 1);
        tabs.select_next();
        assert_eq!(tabs.active_index(), 2);
        assert!(tabs.select_next());
        assert_eq!(tabs.active_index(), 0, "round the end");
        assert!(tabs.select_previous());
        assert_eq!(tabs.active_index(), 2, "and round the beginning");

        // With one tab there is nowhere to go, and saying so is what stops the
        // row being redrawn for nothing.
        let mut one = Tabs::new(tab("a", "A"));
        assert!(!one.select_next());
        assert!(!one.select_previous());
    }

    #[test]
    fn a_number_with_no_tab_behind_it_does_nothing() {
        let mut tabs = three();
        assert!(tabs.select(3));
        assert_eq!(tabs.active_index(), 2);
        assert!(!tabs.select(4), "there is no fourth tab");
        assert_eq!(tabs.active_index(), 2, "and nothing moved");
        assert!(!tabs.select(0), "and no zeroth one");
        assert_eq!(tabs.active_index(), 2);
        assert!(!tabs.select(3), "already there");
    }

    #[test]
    fn a_label_is_the_best_name_the_tab_has() {
        let mut tab: Tab<u32> = Tab::new("a", 0, "about:blank");
        assert_eq!(tab.label(), "new tab");
        tab.url = "https://example.com/a".to_string();
        assert_eq!(tab.label(), "https://example.com/a");
        tab.title = "Example".to_string();
        assert_eq!(tab.label(), "Example");
        tab.note = Some("loading".to_string());
        assert_eq!(tab.label(), "loading");
    }

    #[test]
    fn a_page_that_opens_a_page_becomes_a_tab_and_one_that_does_not_does_not() {
        let mut tabs = Tabs::new(tab("a", "A"));

        // The engine announcing the target this program asked for: no opener,
        // so it is already a tab and the event says nothing.
        let mine = event(
            "Target.targetCreated",
            r#"{"targetInfo":{"targetId":"b","type":"page","url":"about:blank",
                "title":"","attached":false}}"#,
        );
        assert!(matches!(tabs.take(&mine, |_| Ok(1)), Outcome::Ignored));
        assert_eq!(tabs.len(), 1);

        // Something that is not a page: a service worker, an iframe with a
        // process of its own.
        let worker = event(
            "Target.targetCreated",
            r#"{"targetInfo":{"targetId":"w","type":"service_worker","url":"x",
                "openerId":"a","title":""}}"#,
        );
        assert!(matches!(tabs.take(&worker, |_| Ok(2)), Outcome::Ignored));
        assert_eq!(tabs.len(), 1);

        // A link with target=_blank, which is the one that counts.
        let opened = event(
            "Target.targetCreated",
            r#"{"targetInfo":{"targetId":"b","type":"page","openerId":"a",
                "url":"https://example.com/second","title":""}}"#,
        );
        assert!(matches!(tabs.take(&opened, |_| Ok(3)), Outcome::Opened));
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active_target(), Some("b"), "and it is switched to");
        assert_eq!(
            tabs.active().map(|t| t.url.as_str()),
            Some("https://example.com/second")
        );
        assert_eq!(tabs.active().map(|t| t.connection), Some(3));

        // The same target announced again is not a second tab.
        assert!(matches!(tabs.take(&opened, |_| Ok(4)), Outcome::Ignored));
        assert_eq!(tabs.len(), 2);
    }

    #[test]
    fn a_url_from_the_browser_connection_is_plain_text() {
        let renamed = event(
            "Target.targetInfoChanged",
            r#"{"targetInfo":{"targetId":"a","type":"page","title":"",
                "url":"https://example.com/\u001b]0;x\u0007"}}"#,
        );
        assert_eq!(
            change(&renamed),
            Some(Change::Renamed {
                target: "a".to_string(),
                url: "https://example.com/]0;x".to_string()
            })
        );
        let opened = event(
            "Target.targetCreated",
            r#"{"targetInfo":{"targetId":"b","type":"page","openerId":"a","title":"",
                "url":"https://evil.example/\u202emoc.knab"}}"#,
        );
        assert_eq!(
            change(&opened),
            Some(Change::Opened {
                target: "b".to_string(),
                url: "https://evil.example/moc.knab".to_string()
            })
        );
    }

    #[test]
    fn a_tab_that_cannot_be_connected_to_is_a_sentence() {
        let mut tabs = Tabs::new(tab("a", "A"));
        let opened = event(
            "Target.targetCreated",
            r#"{"targetInfo":{"targetId":"b","type":"page","openerId":"a","url":"x","title":""}}"#,
        );
        let outcome = tabs.take(&opened, |_| Err("connection refused".to_string()));
        match outcome {
            Outcome::Failed(why) => assert!(why.contains("connection refused"), "{why}"),
            _ => panic!("a refused connection is a failure"),
        }
        assert_eq!(tabs.len(), 1, "and no half a tab was left behind");
    }

    #[test]
    fn a_tab_that_goes_somewhere_says_so_and_loses_the_last_pages_name() {
        let mut tabs = three();
        // The title in this event is the one the engine derives from the url,
        // and taking it would put a url in the strip where a title belongs.
        let renamed = event(
            "Target.targetInfoChanged",
            r#"{"targetInfo":{"targetId":"c","type":"page","title":"example.com/third",
                "url":"https://example.com/third"}}"#,
        );
        assert!(matches!(tabs.take(&renamed, |_| Ok(0)), Outcome::Renamed));
        assert_eq!(titles(&tabs), ["A", "B", ""], "the title is the page's job");
        assert_eq!(
            tabs.iter().nth(2).map(|t| t.url.as_str()),
            Some("https://example.com/third")
        );

        // The same url twice does not redraw the row.
        assert!(matches!(tabs.take(&renamed, |_| Ok(0)), Outcome::Ignored));

        // And a target that is not a tab of ours is not an error either.
        let stranger = event(
            "Target.targetInfoChanged",
            r#"{"targetInfo":{"targetId":"zz","type":"page","title":"x","url":"y"}}"#,
        );
        assert!(matches!(tabs.take(&stranger, |_| Ok(0)), Outcome::Ignored));
        assert_eq!(tabs.len(), 3);
    }

    #[test]
    fn a_page_that_closes_itself_takes_its_tab_with_it() {
        let mut tabs = three();
        tabs.select(2);
        let destroyed = event("Target.targetDestroyed", r#"{"targetId":"b"}"#);
        match tabs.take(&destroyed, |_| Ok(0)) {
            Outcome::Gone { tab, why } => {
                assert_eq!(tab.target, "b");
                assert_eq!(why, None, "window.close() needs no explanation");
            }
            _ => panic!("the tab should be gone"),
        }
        assert_eq!(titles(&tabs), ["A", "C"]);

        // A target nobody here has is somebody else's business.
        let stranger = event("Target.targetDestroyed", r#"{"targetId":"zz"}"#);
        assert!(matches!(tabs.take(&stranger, |_| Ok(0)), Outcome::Ignored));
        assert_eq!(tabs.len(), 2);
    }

    #[test]
    fn a_renderer_that_dies_is_a_sentence_and_not_a_panic() {
        let mut tabs = three();
        let crashed = event("Target.targetCrashed", r#"{"targetId":"a","errorCode":5}"#);
        match tabs.take(&crashed, |_| Ok(0)) {
            Outcome::Gone { tab, why } => {
                assert_eq!(tab.target, "a");
                let why = why.expect("a crash says why");
                assert!(why.contains("tab"), "{why}");
            }
            _ => panic!("a crashed tab is gone"),
        }
        assert_eq!(titles(&tabs), ["B", "C"]);
    }

    #[test]
    fn a_dialog_stays_with_the_tab_it_opened_on() {
        let mut tabs = three();
        let opening = event(
            "Page.javascriptDialogOpening",
            r#"{"url":"https://c.example","message":"sure?","type":"confirm",
                "hasBrowserHandler":false,"defaultPrompt":""}"#,
        );
        // The third tab asks while the first is in front.
        assert!(tabs.get_mut(2).expect("c").dialog_event(&opening));
        assert_eq!(tabs.active_target(), Some("a"), "a dialog does not switch");
        assert!(tabs.active().expect("a").dialog.is_none());

        // Going to it and away again leaves the question where it was asked.
        tabs.select(3);
        assert!(tabs.active().expect("c").dialog.is_some());
        tabs.select(2);
        assert!(tabs.active().expect("b").dialog.is_none());
        let asking: Vec<bool> = tabs.iter().map(|tab| tab.dialog.is_some()).collect();
        assert_eq!(asking, [false, false, true]);

        // Something else about the page is not about the dialog.
        let loaded = event("Page.loadEventFired", r#"{"timestamp":1}"#);
        assert!(!tabs.get_mut(2).expect("c").dialog_event(&loaded));
        assert!(tabs.iter().nth(2).expect("c").dialog.is_some());

        // Closed, by whoever answered it; and closed again is nothing new.
        let closed = event(
            "Page.javascriptDialogClosed",
            r#"{"result":true,"userInput":""}"#,
        );
        assert!(tabs.get_mut(2).expect("c").dialog_event(&closed));
        assert!(tabs.iter().nth(2).expect("c").dialog.is_none());
        assert!(!tabs.get_mut(2).expect("c").dialog_event(&closed));

        // And closing the tab takes its dialog with it.
        tabs.get_mut(1).expect("b").dialog_event(&opening);
        let gone = tabs.close(1).expect("b");
        assert!(gone.dialog.is_some());
        assert!(tabs.iter().all(|tab| tab.dialog.is_none()));
    }

    #[test]
    fn a_file_input_waits_on_its_tab_and_goes_with_its_page() {
        let mut tabs = three();
        let base = Path::new("/work");
        let opened = |node: u32| {
            event(
                "Page.fileChooserOpened",
                &format!(r#"{{"frameId":"F","mode":"selectSingle","backendNodeId":{node}}}"#),
            )
        };
        // The third tab asks while the first is in front.
        let tab = tabs.get_mut(2).expect("c");
        assert!(tab.chooser_event(&opened(3), base, None));
        assert!(tab.asks(), "marked in the strip");
        assert_eq!(tab.upload.as_ref().expect("a prompt").line.text(), "/work/");
        assert!(tabs.active().expect("a").upload.is_none());

        // The same input clicked again keeps what was typed.
        let tab = tabs.get_mut(2).expect("c");
        tab.upload
            .as_mut()
            .expect("a prompt")
            .line
            .insert_str("rep");
        assert!(!tab.chooser_event(&opened(3), base, None));
        assert_eq!(tab.upload.as_ref().expect("kept").line.text(), "/work/rep");
        // Another input is another question.
        assert!(tab.chooser_event(&opened(7), base, None));
        assert_eq!(tab.upload.as_ref().expect("new").chooser.backend_node_id, 7);
        // Something else about the page is not about the input.
        let loaded = event("Page.loadEventFired", r#"{"timestamp":1}"#);
        assert!(!tab.chooser_event(&loaded, base, None));
        assert!(tab.upload.is_some());

        // The page going somewhere takes the input, and the prompt, with it.
        tab.landed(Landing::Document("https://c.example/next".to_string()));
        assert!(tab.upload.is_none());
        assert!(!tab.asks());

        // A picker with no input to fill is said, not asked.
        let picker = event(
            "Page.fileChooserOpened",
            r#"{"frameId":"F","mode":"selectSingle"}"#,
        );
        assert!(tab.chooser_event(&picker, base, None));
        assert!(tab.upload.is_none());
        assert_eq!(
            tab.note.as_deref(),
            Some("this page's file picker isn't supported")
        );
    }

    #[test]
    fn an_event_about_something_else_entirely() {
        let frame = event("Page.screencastFrame", r#"{"data":"x","sessionId":1}"#);
        assert_eq!(change(&frame), None);
        let empty = event("Target.targetCreated", r#"{}"#);
        assert_eq!(change(&empty), None);
    }

    /// What `app::handle_page_events` does with a load event, which is not a
    /// method of the tab's because the title is asked of the page in between.
    fn load_finished(tab: &mut Tab<u32>, title: &str, status: Option<u16>) {
        tab.loading = false;
        tab.loaded(Loaded {
            title: title.to_string(),
            status,
        });
    }

    #[test]
    fn a_failed_navigation_is_a_sentence_until_the_page_goes_somewhere_else() {
        let mut tab: Tab<u32> = Tab::new("a", 0, "about:blank");
        // What `navigate` does: the url typed, a note while it goes, and then
        // a reply with an `errorText` in it.
        tab.url = "https://example.cmo".to_string();
        tab.note = Some("loading https://example.cmo".to_string());
        tab.loading = true;
        tab.failed_to_reach("https://example.cmo", "net::ERR_NAME_NOT_RESOLVED");
        assert_eq!(tab.line(), "can't reach example.cmo: name not resolved");

        // The error page lands under the url in the engine's spelling, and
        // the reason stays with it.
        tab.landed(Landing::Unreachable("https://example.cmo/".to_string()));
        assert_eq!(tab.url, "https://example.cmo/", "never chrome-error://");
        assert_eq!(tab.line(), "can't reach example.cmo: name not resolved");
        // The error page loads like any page, with no title and no status,
        // and that is not the news that the problem is over.
        load_finished(&mut tab, "", None);
        assert_eq!(tab.line(), "can't reach example.cmo: name not resolved");
        assert_eq!(tab.label(), "can't reach example.cmo: name not resolved");

        // Going somewhere that works is.
        tab.landed(Landing::Document("https://example.com/".to_string()));
        load_finished(&mut tab, "Example", None);
        assert_eq!(tab.problem, None);
        assert_eq!(tab.line(), "Example  —  https://example.com/");
    }

    #[test]
    fn a_reason_that_comes_after_its_landing_is_added_to_it() {
        // The reply read a pass late: the error page has landed and loaded
        // already, under the url the redirect ended at.
        let mut tab: Tab<u32> = Tab::new("a", 0, "about:blank");
        tab.loading = true;
        tab.landed(Landing::Unreachable("http://127.0.0.1:1/".to_string()));
        load_finished(&mut tab, "", None);
        assert!(!tab.loading);
        assert_eq!(tab.line(), "can't reach 127.0.0.1:1");

        tab.failed_to_reach("http://127.0.0.1:2/redir", "net::ERR_CONNECTION_REFUSED");
        assert_eq!(tab.line(), "can't reach 127.0.0.1:1: connection refused");
        assert_eq!(tab.url, "http://127.0.0.1:1/", "the landing's url stays");
        assert!(!tab.loading, "its load event has been and gone");
    }

    #[test]
    fn a_failure_the_page_found_on_its_own_names_the_host() {
        // A link clicked on a loaded page: no `Page.navigate`, so no reason,
        // only the landing.
        let mut tab = tab("a", "A");
        tab.landed(Landing::Unreachable("http://127.0.0.1:9/x".to_string()));
        assert_eq!(tab.line(), "can't reach 127.0.0.1:9");
        assert_eq!(tab.label(), "can't reach 127.0.0.1:9");
        assert_eq!(tab.title, "", "the last page's title went with it");

        // And reloading it lands at the same url again, still with nothing
        // to say why.
        load_finished(&mut tab, "", None);
        tab.landed(Landing::Unreachable("http://127.0.0.1:9/x".to_string()));
        assert_eq!(tab.line(), "can't reach 127.0.0.1:9");
    }

    #[test]
    fn a_reason_that_timed_out_is_not_pinned_on_the_next_failure() {
        let mut tab: Tab<u32> = Tab::new("a", 0, "about:blank");
        tab.failed_to_reach("https://first.invalid/", "net::ERR_NAME_NOT_RESOLVED");
        tab.landed(Landing::Unreachable("https://first.invalid/".to_string()));
        load_finished(&mut tab, "", None);

        // Reloading the same failure keeps its reason: nothing else could
        // have happened to the same url.
        tab.landed(Landing::Unreachable("https://first.invalid/".to_string()));
        assert_eq!(tab.line(), "can't reach first.invalid: name not resolved");
        load_finished(&mut tab, "", None);

        // A different url failing from a page at rest is not the first one's
        // reason again: nothing has said why this one failed.
        tab.landed(Landing::Unreachable("http://127.0.0.1:9/".to_string()));
        assert_eq!(tab.line(), "can't reach 127.0.0.1:9");
    }

    #[test]
    fn a_rename_from_the_browser_leaves_the_failure_alone() {
        let mut tabs = Tabs::new(Tab::new("a", 0u32, "about:blank"));
        let tab = tabs.active_mut().expect("a tab");
        tab.url = "http://127.0.0.1:9".to_string();
        tab.loading = true;
        tab.failed_to_reach("http://127.0.0.1:9", "net::ERR_CONNECTION_REFUSED");

        // A millisecond later the browser connection renames the tab, in the
        // engine's spelling — which is a different url, so the rename is
        // taken, and a note would have gone with it.
        let renamed = event(
            "Target.targetInfoChanged",
            r#"{"targetInfo":{"targetId":"a","type":"page","title":"127.0.0.1:9/",
                "url":"http://127.0.0.1:9/"}}"#,
        );
        assert!(matches!(tabs.take(&renamed, |_| Ok(0)), Outcome::Renamed));
        let tab = tabs.active_mut().expect("a tab");
        assert_eq!(tab.line(), "can't reach 127.0.0.1:9: connection refused");

        tab.landed(Landing::Unreachable("http://127.0.0.1:9/".to_string()));
        assert_eq!(tab.line(), "can't reach 127.0.0.1:9: connection refused");
    }

    #[test]
    fn a_link_clicked_into_a_slow_host_says_loading_before_it_lands() {
        let mut tab = tab("a", "A");
        let url_before = tab.url.clone();
        let now = Instant::now();
        tab.started("http://127.0.0.1:1/hang".to_string(), now);
        assert!(tab.loading);
        assert!(!tab.committed, "nothing has come back yet");
        assert_eq!(tab.note.as_deref(), Some("loading http://127.0.0.1:1/hang"));
        assert_eq!(tab.line(), "loading http://127.0.0.1:1/hang");
        assert_eq!(tab.url, url_before, "the page on screen is still the last");

        tab.landed(Landing::Document("http://127.0.0.1:1/hang".to_string()));
        assert!(tab.committed);
        assert_eq!(tab.note, None);
        assert_eq!(tab.since, Some(now), "the clock runs from the click");

        tab.stopped_loading();
        assert!(!tab.loading);
        assert_eq!(tab.since, None);
        assert_eq!(tab.loading_for(now + Duration::from_secs(9)), None);
    }

    #[test]
    fn a_click_abandoned_before_it_lands_leaves_the_page_saying_what_it_was() {
        // A link that turned out to be a download, or was overtaken: the
        // departure, and then the stop with no landing in between.
        let mut tab = tab("a", "A");
        tab.started("https://a.example/file.pdf".to_string(), Instant::now());
        tab.stopped_loading();
        assert!(tab.committed);
        assert_eq!(tab.note, None);
        assert_eq!(tab.line(), "A  —  https://a.example");

        // A note that is not about loading is not the stop's to take down.
        tab.started("https://a.example/x".to_string(), Instant::now());
        tab.note = Some("copied url".to_string());
        tab.stopped_loading();
        assert_eq!(tab.note.as_deref(), Some("copied url"));
    }

    #[test]
    fn a_started_load_over_a_typed_one_keeps_the_clock() {
        let mut tab: Tab<u32> = Tab::new("a", 0, "about:blank");
        let typed = Instant::now();
        // What `edit_url` and `navigate` do.
        tab.url = "http://127.0.0.1:1/".to_string();
        tab.note = Some("loading http://127.0.0.1:1/".to_string());
        tab.loading = true;
        tab.since = Some(typed);
        tab.started(
            "http://127.0.0.1:1/".to_string(),
            typed + Duration::from_millis(3),
        );
        assert_eq!(tab.since, Some(typed));
        assert_eq!(tab.note.as_deref(), Some("loading http://127.0.0.1:1/"));

        // The engine's spelling of it is a different note and a new clock,
        // three milliseconds on, which nobody can see.
        let later = typed + Duration::from_millis(5);
        tab.started("http://127.0.0.1:1/x".to_string(), later);
        assert_eq!(tab.since, Some(later));
    }

    #[test]
    fn loading_for_counts_whole_seconds_from_the_start_and_only_after_one() {
        let mut tab = tab("a", "A");
        let start = Instant::now();
        assert_eq!(tab.loading_for(start), None, "not loading");
        tab.started("https://a.example/".to_string(), start);
        assert_eq!(tab.loading_for(start), None);
        assert_eq!(tab.loading_for(start + Duration::from_millis(999)), None);
        assert_eq!(
            tab.loading_for(start + Duration::from_millis(1000)),
            Some(1)
        );
        assert_eq!(
            tab.loading_for(start + Duration::from_millis(4700)),
            Some(4)
        );
    }

    #[test]
    fn a_plain_http_page_says_not_secure_beside_the_title_and_localhost_does_not() {
        let mut tab: Tab<u32> = Tab::new("a", 0, "about:blank");
        tab.landed(Landing::Document("http://wiki.corp/".to_string()));
        tab.trust = Trust::Insecure;
        load_finished(&mut tab, "Wiki", None);
        assert_eq!(tab.line(), "not secure  —  Wiki  —  http://wiki.corp/");
        assert_eq!(tab.label(), "Wiki", "the strip keeps the page's name");

        load_finished(&mut tab, "nope", Some(404));
        assert_eq!(
            tab.line(),
            "404 not found  —  not secure  —  nope  —  http://wiki.corp/"
        );

        tab.landed(Landing::Document("http://127.0.0.1:1/".to_string()));
        tab.trust = Trust::Plain;
        load_finished(&mut tab, "fine", None);
        assert_eq!(tab.line(), "fine  —  http://127.0.0.1:1/");
    }

    #[test]
    fn an_error_status_is_said_beside_the_title_and_not_instead_of_it() {
        let mut tab: Tab<u32> = Tab::new("a", 0, "about:blank");
        tab.landed(Landing::Document("http://127.0.0.1:1/404".to_string()));
        load_finished(&mut tab, "nope", Some(404));
        assert_eq!(
            tab.line(),
            "404 not found  —  nope  —  http://127.0.0.1:1/404"
        );
        assert_eq!(tab.label(), "nope", "the strip keeps the page's own name");

        // No title: the status and the url.
        tab.title.clear();
        assert_eq!(tab.line(), "404 not found  —  http://127.0.0.1:1/404");

        // A note still stands in for the lot while something is loading.
        tab.note = Some("loading".to_string());
        assert_eq!(tab.line(), "loading");
        tab.note = None;

        // The next page is fine, and says nothing about a status.
        tab.landed(Landing::Document("http://127.0.0.1:1/".to_string()));
        load_finished(&mut tab, "fine", None);
        assert_eq!(tab.problem, None);
        assert_eq!(tab.line(), "fine  —  http://127.0.0.1:1/");
    }
}
