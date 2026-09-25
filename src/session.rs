//! The tabs that were open, and the ones just closed.
//!
//! The session is the tabs, in order, with their titles and which one is in
//! front — and nothing else. Not the scroll position, not what was typed into
//! a form, not each tab's back and forward: that is the renderer's state, and
//! nothing on the pipe offers it short of asking every page with a
//! `Runtime.evaluate` twenty times a second. What is kept is what a person
//! would write down to get back to where they were.
//!
//! # Where, and when
//!
//! It is `<profile>/session`, in the profile rather than beside it, because
//! the tabs open under the work profile are not the tabs open under the
//! personal one, and two profiles running at once would otherwise overwrite
//! one file. It is readable by its owner alone (0600), like the history — it
//! is a list of pages. A temporary profile keeps it in memory
//! ([`Session::in_memory`]) and writes nothing, which is what
//! `--temp-profile` promises; `ctrl+shift+t` still works there.
//!
//! It is a snapshot written whole — to `session.tmp`, renamed over `session`
//! — rather than a log, because a session is a state and not a series of
//! events, and a crash in the middle of a write must leave the last whole
//! one; a rename is atomic against this process dying, which is the case this
//! defends against, so there is no `fsync`. It is written when it changes,
//! at most once every [`WRITE_EVERY`]: the loop hands [`Session::record`] a
//! [`Snapshot`] once a pass, which is a few string clones, and a write —
//! 137 µs for ten tabs, measured — happens only when that differs from the
//! last one written. A page that rewrites its url every frame costs two
//! writes a second, not sixty, and an unclean exit loses at most the last
//! half second.
//!
//! # Knowing that the last run did not quit
//!
//! The first line is the marker: `# blinkterm session: open` while a run has
//! the profile, `# blinkterm session: closed` once it has quit. A run that
//! ends by a quit — `ctrl+q`, the last tab closing, a `SIGTERM` or a `SIGHUP`
//! — writes `closed` ([`Session::finish`]); every other ending leaves `open`:
//! a panic (nothing runs under `panic = "abort"`, and nothing needs to — the
//! file is already right), a `SIGKILL`, the engine dying. So the next start
//! reads `open` and offers the tabs back ([`plan`], [`Offer`]). There is no
//! marker file to clean up and no lock to consult; the one false positive —
//! the last run still running on this profile — cannot happen, because the
//! profile's lock would have refused this one first. While the offer is up
//! nothing is written ([`Session::hold`]), so a second crash before it is
//! answered still finds the session it was going to offer.
//!
//! # The closed tabs
//!
//! `ctrl+shift+t` (and `alt+t`, for a terminal that cannot send it) reopens
//! the last tab closed, then the one before, up to [`CLOSED_KEEP`]. That
//! stack is memory only, for this run: a stack that outlived the run would be
//! a second history.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::history::History;
use crate::input::{Key, KeyAction, KeyInput};
use crate::tabs::Tabs;
use crate::text;

/// The file in the profile.
pub const FILE: &str = "session";

/// The most tabs a restore will open. A hand-written or runaway file of a
/// thousand lines would be a thousand renderers; lines past this are not
/// read.
pub const RESTORE_CAP: usize = 100;

/// Closed tabs remembered for `ctrl+shift+t`.
pub const CLOSED_KEEP: usize = 20;

/// How long a change may wait before it is on disk: a page that pushes state
/// every frame is one write per half second, and a crash loses at most that
/// much.
pub const WRITE_EVERY: Duration = Duration::from_millis(500);

/// The header's first words; the state is the word after them.
const HEADER: &str = "# blinkterm session:";

/// One tab, as the session keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub url: String,
    /// Plain text, one line; may be empty.
    pub title: String,
}

/// The tabs as they are, or were.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub tabs: Vec<Entry>,
    /// Which of `tabs` was in front.
    pub active: usize,
}

impl Snapshot {
    /// What the list is now: every tab whose url [`Session::keeps`] — a blank
    /// tab is not a place — with dormant ones by the url and title they were
    /// restored with, and `active` moved to the nearest kept tab at or before
    /// the one in front (the first, when there is none before it). Empty
    /// when no tab is a place.
    pub fn of<C>(tabs: &Tabs<C>) -> Snapshot {
        let mut snapshot = Snapshot::default();
        for (index, tab) in tabs.iter().enumerate() {
            if !Session::keeps(&tab.url) {
                continue;
            }
            if index <= tabs.active_index() {
                snapshot.active = snapshot.tabs.len();
            }
            snapshot.tabs.push(Entry {
                url: tab.url.clone(),
                title: tab.title.clone(),
            });
        }
        snapshot
    }
}

/// Whether the run that wrote the file finished with a quit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// A run had the profile and never said it was done.
    Open,
    /// The run quit.
    Closed,
}

/// What the file said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub snapshot: Snapshot,
    pub state: State,
}

/// The session file, the closed-tab stack, and what the last run left.
#[derive(Debug)]
pub struct Session {
    /// The file, or `None` for a temporary profile.
    path: Option<PathBuf>,
    /// What the file said when this run began, until a restore takes it.
    saved: Option<Saved>,
    /// The last snapshot on disk (or, in memory, the last recorded), so a
    /// pass that changed nothing writes nothing.
    written: Option<Snapshot>,
    wrote_at: Option<Instant>,
    /// A change waiting for [`WRITE_EVERY`] to pass.
    pending: Option<Snapshot>,
    /// While an offer to restore is on the row nothing is written: the file
    /// still says what the last run had, and that is what is being offered.
    held: bool,
    /// Why the last write failed, until [`Session::flush`] says it.
    failed: Option<String>,
    /// Said once, on the first write that fails.
    warned: bool,
    /// Closed tabs, oldest first.
    closed: Vec<Entry>,
}

impl Session {
    /// A session kept nowhere: a temporary profile's. The closed-tab stack
    /// works; nothing is written.
    pub fn in_memory() -> Session {
        Session {
            path: None,
            saved: None,
            written: None,
            wrote_at: None,
            pending: None,
            held: false,
            failed: None,
            warned: false,
            closed: Vec::new(),
        }
    }

    /// `<dir>/session`, read now. A file that is not there or does not parse
    /// is no session, never an error.
    pub fn load(dir: &Path) -> Session {
        let path = dir.join(FILE);
        let saved = std::fs::read(&path)
            .ok()
            .and_then(|bytes| Session::parse(&String::from_utf8_lossy(&bytes)));
        Session {
            path: Some(path),
            saved,
            ..Session::in_memory()
        }
    }

    /// The pure half of [`Session::load`]: the header's state, and one tab a
    /// line — a mark (`*` in front, `-` not), a tab, the url, a tab, the
    /// title. A line that is not that, or whose url is not one
    /// [`Session::keeps`], is skipped; a missing or unrecognised header is
    /// [`State::Closed`], so that a file written by hand restores with
    /// `--restore` and never prompts an offer. No `*` is the first tab in
    /// front; two is the first of them. `None` when no line is a tab.
    pub fn parse(text: &str) -> Option<Saved> {
        let mut lines = text.lines().peekable();
        let mut state = State::Closed;
        if let Some(header) = lines.peek().and_then(|line| line.strip_prefix(HEADER)) {
            if header.trim() == "open" {
                state = State::Open;
            }
            lines.next();
        }
        let mut snapshot = Snapshot::default();
        let mut front = None;
        for line in lines {
            if snapshot.tabs.len() >= RESTORE_CAP {
                break;
            }
            let mut fields = line.splitn(3, '\t');
            let mark = fields.next().unwrap_or_default();
            if mark != "*" && mark != "-" {
                continue;
            }
            let Some(url) = fields.next().map(|url| url.trim_matches([' ', '\r'])) else {
                continue;
            };
            if !Session::keeps(url) {
                continue;
            }
            if mark == "*" && front.is_none() {
                front = Some(snapshot.tabs.len());
            }
            snapshot.tabs.push(Entry {
                url: url.to_string(),
                title: text::sanitize(fields.next().unwrap_or_default())
                    .trim()
                    .to_string(),
            });
        }
        if snapshot.tabs.is_empty() {
            return None;
        }
        snapshot.active = front.unwrap_or(0);
        Some(Saved { snapshot, state })
    }

    /// The pure half of a write: the header and a line a tab, titles plain
    /// text on one line.
    pub fn render(snapshot: &Snapshot, state: State) -> String {
        let word = match state {
            State::Open => "open",
            State::Closed => "closed",
        };
        let mut out = format!("{HEADER} {word}\n");
        for (index, entry) in snapshot.tabs.iter().enumerate() {
            let mark = if index == snapshot.active { '*' } else { '-' };
            let title = text::sanitize(&entry.title);
            out.push_str(&format!("{mark}\t{}\t{}\n", entry.url, title.trim()));
        }
        out
    }

    /// What the file said when this run began, until a restore takes it.
    pub fn saved(&self) -> Option<&Saved> {
        self.saved.as_ref()
    }

    /// The saved session, for a restore: it is this run's now.
    pub fn take_saved(&mut self) -> Option<Saved> {
        self.saved.take()
    }

    /// Hold every write while an offer to restore is on the row, or let them
    /// go again once it has been answered.
    pub fn hold(&mut self, held: bool) {
        self.held = held;
        if held {
            self.pending = None;
        }
    }

    /// The tabs are now this. Written at once if the last write was more
    /// than [`WRITE_EVERY`] ago, else kept for [`Session::flush`]; nothing if
    /// unchanged, or empty — a session with no tabs is not one to reopen, so
    /// the file keeps the last that had any — or while held. Never an error:
    /// what fails is said by `flush`.
    pub fn record(&mut self, snapshot: Snapshot, now: Instant) {
        if self.held || snapshot.tabs.is_empty() {
            return;
        }
        if self.written.as_ref() == Some(&snapshot) {
            self.pending = None;
            return;
        }
        self.pending = Some(snapshot);
        if self.due(now) {
            self.write_pending(now);
        }
    }

    /// Write what is pending once its time has come. `Err` once, the first
    /// time a write fails, for the row; after that, silence — the row has
    /// better things to say than the same failure twice a second.
    pub fn flush(&mut self, now: Instant) -> Result<(), String> {
        if self.pending.is_some() && self.due(now) {
            self.write_pending(now);
        }
        match self.failed.take() {
            Some(why) if !self.warned => {
                self.warned = true;
                Err(why)
            }
            _ => Ok(()),
        }
    }

    /// The run is ending. A clean end writes the last snapshot marked
    /// closed; an unclean one — the engine died — writes whatever is still
    /// pending marked open, so that its last half second is not lost with
    /// it. A clean end with nothing of its own to write, after an offer that
    /// was declined, marks the session it was offered closed, so the next
    /// start does not offer it again; one that quit with the offer still up
    /// leaves it, since the question was never answered.
    pub fn finish(&mut self, clean: bool) {
        if self.held {
            return;
        }
        let now = Instant::now();
        if !clean {
            if self.pending.is_some() {
                self.write_pending(now);
            }
            return;
        }
        let last = self
            .pending
            .take()
            .or_else(|| self.written.clone())
            .or_else(|| self.saved.take().map(|saved| saved.snapshot));
        if let Some(last) = last {
            let _ = self.write(&last, State::Closed);
        }
    }

    /// A tab went: remembered for [`Session::reopen`] if its url is one
    /// [`Session::keeps`], and the oldest forgotten past [`CLOSED_KEEP`].
    pub fn closed(&mut self, entry: Entry) {
        if !Session::keeps(&entry.url) {
            return;
        }
        self.closed.push(entry);
        if self.closed.len() > CLOSED_KEEP {
            self.closed.remove(0);
        }
    }

    /// The tab closed last, which is then forgotten.
    pub fn reopen(&mut self) -> Option<Entry> {
        self.closed.pop()
    }

    /// Whether a url is a place worth coming back to: the history's rule
    /// ([`History::records`]), which is also what makes it one field of one
    /// line.
    pub fn keeps(url: &str) -> bool {
        History::records(url)
    }

    /// How many tabs the last recorded snapshot had, for the sentence after
    /// the engine dies; `None` for a session kept nowhere, which has nothing
    /// to point at.
    pub fn saved_tabs(&self) -> Option<usize> {
        self.path.as_ref()?;
        self.pending
            .as_ref()
            .or(self.written.as_ref())
            .map(|snapshot| snapshot.tabs.len())
            .filter(|&tabs| tabs > 0)
    }

    fn due(&self, now: Instant) -> bool {
        self.wrote_at
            .is_none_or(|at| now.saturating_duration_since(at) >= WRITE_EVERY)
    }

    fn write_pending(&mut self, now: Instant) {
        let Some(snapshot) = self.pending.take() else {
            return;
        };
        self.wrote_at = Some(now);
        if let Err(why) = self.write(&snapshot, State::Open) {
            self.failed = Some(why);
        }
        self.written = Some(snapshot);
    }

    /// The snapshot, whole, to `session.tmp` and renamed over the file.
    fn write(&mut self, snapshot: &Snapshot, state: State) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let fresh = path.with_extension("tmp");
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&fresh)
            .and_then(|mut file| file.write_all(Session::render(snapshot, state).as_bytes()))
            .and_then(|()| std::fs::rename(&fresh, path))
            .map_err(|e| format!("cannot save the tabs to {}: {e}", path.display()))
    }
}

/// What a start does with the session, decided before anything is opened.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Tabs to open before anything else, dormant.
    pub restore: Option<Snapshot>,
    /// A question to put on the row instead.
    pub offer: Option<Offer>,
}

/// What to do with `saved` at the start: `restore` is `--restore`, which
/// restores whatever the file says, quit or not — that is what the flag is
/// for. Without it, a session left open by a run that did not quit is
/// offered. A url on the command line is not the plan's business: it opens
/// after the plan, in a tab of its own.
pub fn plan(saved: Option<&Saved>, restore: bool) -> Plan {
    let Some(saved) = saved else {
        return Plan::default();
    };
    if restore {
        return Plan {
            restore: Some(saved.snapshot.clone()),
            offer: None,
        };
    }
    if saved.state == State::Open && !saved.snapshot.tabs.is_empty() {
        return Plan {
            restore: None,
            offer: Some(Offer {
                tabs: saved.snapshot.tabs.len(),
            }),
        };
    }
    Plan::default()
}

/// The question on the row after an unclean exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    pub tabs: usize,
}

/// What a key says to an [`Offer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// `y`, `Y`, Enter.
    Yes,
    /// `n`, `N`, Escape: declined, and the key is spent.
    No,
    /// A release or a bare modifier: nothing yet.
    Waiting,
    /// Any other key: declined, and the key goes on to whatever it was for.
    /// The question is not urgent, and somebody who has started typing has
    /// answered it.
    Pass,
}

impl Offer {
    /// `restore 3 tabs from last time?`
    pub fn caption(&self) -> String {
        let noun = if self.tabs == 1 { "tab" } else { "tabs" };
        format!("restore {} {noun} from last time?", self.tabs)
    }

    /// What the keys are.
    pub fn hint(&self) -> &'static str {
        "y/n"
    }

    /// What `key` says to the question.
    pub fn reply(&self, key: &KeyInput) -> Reply {
        if key.action == KeyAction::Release {
            return Reply::Waiting;
        }
        let plain = !key.mods.ctrl() && !key.mods.alt() && !key.mods.meta();
        match key.key {
            Key::Other(_) => Reply::Waiting,
            Key::Enter if plain => Reply::Yes,
            Key::Char('y' | 'Y') if plain => Reply::Yes,
            Key::Escape => Reply::No,
            Key::Char('n' | 'N') if plain => Reply::No,
            _ => Reply::Pass,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Mods;
    use crate::tabs::Tab;
    use std::os::unix::fs::PermissionsExt;

    /// A scratch directory of this test's own, gone again before and after.
    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blinkterm-session-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn entry(url: &str, title: &str) -> Entry {
        Entry {
            url: url.to_string(),
            title: title.to_string(),
        }
    }

    fn snapshot(urls: &[&str], active: usize) -> Snapshot {
        Snapshot {
            tabs: urls.iter().map(|url| entry(url, "")).collect(),
            active,
        }
    }

    fn on_disk(dir: &Path) -> Option<Saved> {
        Session::parse(&std::fs::read_to_string(dir.join(FILE)).ok()?)
    }

    #[test]
    fn a_file_round_trips_and_says_whether_the_run_that_wrote_it_quit() {
        let snapshot = Snapshot {
            tabs: vec![
                entry("https://example.com/", "Example Domain"),
                entry("https://docs.rs/", ""),
            ],
            active: 1,
        };
        for state in [State::Open, State::Closed] {
            let text = Session::render(&snapshot, state);
            assert_eq!(
                Session::parse(&text),
                Some(Saved {
                    snapshot: snapshot.clone(),
                    state
                }),
                "{text}"
            );
        }
        assert_eq!(
            Session::render(&snapshot, State::Open),
            "# blinkterm session: open\n\
             -\thttps://example.com/\tExample Domain\n\
             *\thttps://docs.rs/\t\n"
        );
    }

    #[test]
    fn a_broken_line_is_skipped_and_a_missing_header_is_a_closed_session() {
        let text = "junk\n-\tabout:blank\tBlank\n*\thttps://a.example/\tA\tand\x1b]0;x\x07\n\
                    ?\thttps://b.example/\n-\n*\thttps://c.example/\r\n# comment\n";
        let saved = Session::parse(text).expect("a session");
        assert_eq!(saved.state, State::Closed, "no header");
        assert_eq!(
            saved.snapshot.tabs,
            [
                entry("https://a.example/", "A and]0;x"),
                entry("https://c.example/", "")
            ]
        );
        assert_eq!(saved.snapshot.active, 0, "the first of two *");

        let saved = Session::parse("# blinkterm session: open\n-\thttps://a.example/\n")
            .expect("a session");
        assert_eq!((saved.state, saved.snapshot.active), (State::Open, 0));
        assert_eq!(
            Session::parse("# blinkterm session: sideways\n-\thttps://a.example/\n")
                .map(|saved| saved.state),
            Some(State::Closed)
        );
        assert_eq!(Session::parse("# blinkterm session: open\n"), None);
        assert_eq!(Session::parse(""), None);

        let many: String = (0..RESTORE_CAP + 5)
            .map(|n| format!("-\thttps://example.com/{n}\n"))
            .collect();
        assert_eq!(
            Session::parse(&many)
                .expect("a session")
                .snapshot
                .tabs
                .len(),
            RESTORE_CAP
        );
    }

    #[test]
    fn a_snapshot_is_every_tab_that_is_a_place_with_the_front_one_marked() {
        let mut tabs = Tabs::new(Tab::new("a", 0u32, "https://a.example/"));
        tabs.get_mut(0).expect("a").title = "A".to_string();
        tabs.open(Tab::new("b", 1, "about:blank"));
        let mut dormant = Tab::new("c", 2, "about:blank");
        dormant.url = "https://c.example/".to_string();
        dormant.title = "Saved C".to_string();
        dormant.dormant = true;
        tabs.open(dormant);
        tabs.select(2);
        // The blank tab is in front: the nearest place before it is marked.
        assert_eq!(
            Snapshot::of(&tabs),
            Snapshot {
                tabs: vec![
                    entry("https://a.example/", "A"),
                    entry("https://c.example/", "Saved C")
                ],
                active: 0,
            }
        );
        tabs.select(3);
        assert_eq!(Snapshot::of(&tabs).active, 1);

        let blank = Tabs::new(Tab::new("a", 0u32, "about:blank"));
        assert_eq!(Snapshot::of(&blank), Snapshot::default());
    }

    #[test]
    fn a_change_is_written_once_and_an_unchanged_pass_writes_nothing() {
        let dir = scratch("once");
        let mut session = Session::load(&dir);
        let start = Instant::now();
        session.record(snapshot(&["https://a.example/"], 0), start);
        assert!(session.flush(start).is_ok());
        let saved = on_disk(&dir).expect("written at once");
        assert_eq!(saved.state, State::Open);
        std::fs::remove_file(dir.join(FILE)).expect("removed");
        let later = start + Duration::from_secs(5);
        session.record(snapshot(&["https://a.example/"], 0), later);
        assert!(session.flush(later).is_ok());
        assert!(!dir.join(FILE).exists(), "nothing changed, nothing written");
        assert!(!dir.join("session.tmp").exists(), "no temporary file left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changes_within_half_a_second_are_one_write_and_a_flush_writes_the_last() {
        let dir = scratch("window");
        let mut session = Session::load(&dir);
        let t = Instant::now();
        session.record(snapshot(&["https://a.example/"], 0), t);
        session.record(
            snapshot(&["https://a.example/", "https://b.example/"], 1),
            t + Duration::from_millis(100),
        );
        session.record(
            snapshot(&["https://a.example/", "https://c.example/"], 1),
            t + Duration::from_millis(200),
        );
        assert!(session.flush(t + Duration::from_millis(300)).is_ok());
        assert_eq!(
            on_disk(&dir).expect("the first").snapshot.tabs.len(),
            1,
            "only the first is on disk before half a second"
        );
        assert!(session.flush(t + Duration::from_millis(600)).is_ok());
        assert_eq!(
            on_disk(&dir).expect("the last").snapshot,
            snapshot(&["https://a.example/", "https://c.example/"], 1)
        );
        let mode = std::fs::metadata(dir.join(FILE))
            .expect("the file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "a list of pages nobody else may read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_snapshot_never_overwrites_the_last_session() {
        let dir = scratch("empty");
        let mut session = Session::load(&dir);
        let t = Instant::now();
        session.record(snapshot(&["https://a.example/"], 0), t);
        session.record(Snapshot::default(), t + Duration::from_secs(1));
        assert!(session.flush(t + Duration::from_secs(2)).is_ok());
        assert_eq!(on_disk(&dir).expect("kept").snapshot.tabs.len(), 1);
        session.finish(true);
        let saved = on_disk(&dir).expect("kept");
        assert_eq!((saved.state, saved.snapshot.tabs.len()), (State::Closed, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clean_finish_says_closed_and_an_unclean_one_writes_what_was_pending_open() {
        let dir = scratch("finish");
        let mut session = Session::load(&dir);
        let t = Instant::now();
        session.record(snapshot(&["https://a.example/"], 0), t);
        session.record(
            snapshot(&["https://a.example/", "https://b.example/"], 1),
            t + Duration::from_millis(10),
        );
        assert_eq!(on_disk(&dir).expect("first").snapshot.tabs.len(), 1);
        session.finish(false);
        let saved = on_disk(&dir).expect("pending written");
        assert_eq!((saved.state, saved.snapshot.tabs.len()), (State::Open, 2));
        session.finish(true);
        let saved = on_disk(&dir).expect("closed");
        assert_eq!((saved.state, saved.snapshot.tabs.len()), (State::Closed, 2));

        // A declined offer is not offered again after a clean quit.
        std::fs::write(
            dir.join(FILE),
            Session::render(&snapshot(&["https://a.example/"], 0), State::Open),
        )
        .expect("an open session");
        let mut next = Session::load(&dir);
        next.hold(true);
        next.hold(false);
        next.finish(true);
        assert_eq!(on_disk(&dir).expect("kept").state, State::Closed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_written_while_the_offer_is_up() {
        let dir = scratch("held");
        let before = Session::render(&snapshot(&["https://old.example/"], 0), State::Open);
        std::fs::write(dir.join(FILE), &before).expect("the last run's");
        let mut session = Session::load(&dir);
        session.hold(true);
        let t = Instant::now();
        session.record(snapshot(&["https://new.example/"], 0), t);
        assert!(session.flush(t + Duration::from_secs(1)).is_ok());
        session.finish(true);
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE)).expect("kept"),
            before
        );
        session.hold(false);
        session.record(
            snapshot(&["https://new.example/"], 0),
            t + Duration::from_secs(2),
        );
        assert_eq!(
            on_disk(&dir).expect("written").snapshot.tabs[0].url,
            "https://new.example/"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_first_write_that_fails_is_said_once() {
        let dir = scratch("fails");
        let file = dir.join("not-a-dir");
        std::fs::write(&file, b"a file").expect("a file");
        let mut session = Session::load(&file);
        let t = Instant::now();
        session.record(snapshot(&["https://a.example/"], 0), t);
        let why = session.flush(t).unwrap_err();
        assert!(why.contains("session"), "{why}");
        session.record(
            snapshot(&["https://b.example/"], 0),
            t + Duration::from_secs(1),
        );
        assert!(
            session.flush(t + Duration::from_secs(1)).is_ok(),
            "said once"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closed_tabs_reopen_newest_first_up_to_the_cap_and_blank_ones_are_not_kept() {
        let mut session = Session::in_memory();
        session.closed(entry("about:blank", ""));
        assert_eq!(session.reopen(), None);
        for n in 0..CLOSED_KEEP + 3 {
            session.closed(entry(&format!("https://example.com/{n}"), ""));
        }
        let first = session.reopen().expect("the newest");
        assert_eq!(
            first.url,
            format!("https://example.com/{}", CLOSED_KEEP + 2)
        );
        let mut rest = 1;
        let mut last = first;
        while let Some(entry) = session.reopen() {
            rest += 1;
            last = entry;
        }
        assert_eq!(rest, CLOSED_KEEP);
        assert_eq!(last.url, "https://example.com/3", "the oldest went");
    }

    #[test]
    fn the_plan_restores_on_the_flag_and_offers_after_an_open_session_otherwise() {
        let saved = |state| Saved {
            snapshot: snapshot(&["https://a.example/", "https://b.example/"], 1),
            state,
        };
        let open = saved(State::Open);
        let closed = saved(State::Closed);
        assert_eq!(plan(Some(&open), true).restore, Some(open.snapshot.clone()));
        assert_eq!(plan(Some(&open), true).offer, None);
        assert_eq!(
            plan(Some(&closed), true).restore,
            Some(closed.snapshot.clone())
        );
        assert_eq!(
            plan(Some(&open), false),
            Plan {
                restore: None,
                offer: Some(Offer { tabs: 2 })
            }
        );
        assert_eq!(plan(Some(&closed), false), Plan::default());
        assert_eq!(plan(None, true), Plan::default());
        let empty = Saved {
            snapshot: Snapshot::default(),
            state: State::Open,
        };
        assert_eq!(plan(Some(&empty), false), Plan::default());
    }

    #[test]
    fn an_offer_takes_y_and_enter_as_yes_n_and_escape_as_no_and_passes_the_rest_on() {
        let offer = Offer { tabs: 3 };
        assert_eq!(offer.caption(), "restore 3 tabs from last time?");
        assert_eq!(Offer { tabs: 1 }.caption(), "restore 1 tab from last time?");
        assert_eq!(offer.hint(), "y/n");
        let press = KeyInput::press;
        for key in [Key::Char('y'), Key::Char('Y'), Key::Enter] {
            assert_eq!(offer.reply(&press(key)), Reply::Yes, "{key:?}");
        }
        for key in [Key::Char('n'), Key::Char('N'), Key::Escape] {
            assert_eq!(offer.reply(&press(key)), Reply::No, "{key:?}");
        }
        let mut release = press(Key::Char('x'));
        release.action = KeyAction::Release;
        assert_eq!(offer.reply(&release), Reply::Waiting);
        assert_eq!(offer.reply(&press(Key::Other(57441))), Reply::Waiting);
        let mut ctrl_l = press(Key::Char('l'));
        ctrl_l.mods = Mods(Mods::CTRL);
        let mut ctrl_y = press(Key::Char('y'));
        ctrl_y.mods = Mods(Mods::CTRL);
        for key in [ctrl_l, ctrl_y, press(Key::Char('x')), press(Key::Char('1'))] {
            assert_eq!(offer.reply(&key), Reply::Pass, "{key:?}");
        }
    }

    #[test]
    fn an_in_memory_session_keeps_the_closed_stack_and_writes_nothing() {
        let mut session = Session::in_memory();
        let t = Instant::now();
        session.record(snapshot(&["https://a.example/"], 0), t);
        assert!(session.flush(t).is_ok());
        session.finish(true);
        assert_eq!(session.saved_tabs(), None, "nothing to point at");
        session.closed(entry("https://a.example/", "A"));
        assert_eq!(session.reopen(), Some(entry("https://a.example/", "A")));
    }
}
