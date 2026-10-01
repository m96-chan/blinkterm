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
//! at most once every [`WRITE_EVERY`]: the loop hands
//! [`Session::record_window`] a [`Snapshot`] once a pass, which is a few
//! string clones, and a write — 137 µs for ten tabs, measured — happens only
//! when that differs from the last one it was handed. A page that rewrites
//! its url every frame costs two writes a second, not sixty, and an unclean
//! exit loses at most the last half second.
//!
//! # A group of tabs a window
//!
//! The file is groups of tabs, one for each window that had any, each
//! headed by its state:
//!
//! ```text
//! # blinkterm session: open
//! # format: 2
//! # window 1 live
//! *<TAB>https://a.example/<TAB>A
//! -<TAB>https://b.example/<TAB>B
//! # window 2 closed
//! *<TAB>https://c.example/<TAB>C
//! ```
//!
//! A window today is the one terminal a run has, always [`WindowId`] 1; the
//! groups are there so that one profile can serve several windows and each
//! comes back as itself rather than as one long row of everybody's tabs. A
//! group is [`GroupState::Live`] while its window is open,
//! [`GroupState::Closed`] once it was closed by a quit, and
//! [`GroupState::Lost`] when it went any other way. A `live` group read at
//! the start was written by a run that never said it was done, and is read
//! as lost. The number after `window` is only the group's place in the file,
//! for the person reading it; nothing reads it back.
//!
//! Each new window continues one group: it is offered or given one, oldest
//! first ([`Session::take_group`], [`Session::offer_group`]), or, when there
//! was nothing to restore, the first tabs it records take the place of the
//! oldest group nobody has claimed — which, for one window, is exactly the
//! old rule that this run's tabs replace the last run's. Groups nobody has
//! claimed are written back as they were read, so that a group no window has
//! taken yet is still there for the next `--restore`. So the file holds no
//! more groups than there were windows open at once, and of those at most
//! [`MAX_GROUPS`] are read back, the first in the file going first, with at
//! most [`RESTORE_CAP`] tabs in each.
//!
//! A file without `# window` lines — every file a blinkterm before this one
//! wrote — is one group, lost if its header says `open` and closed
//! otherwise. The other way round, every line this version adds starts with
//! `#`, which the older reader passes over: an older blinkterm reads a file
//! of several groups as one window with all of their tabs, which is the
//! most it could do with it.
//!
//! # Knowing that the last run did not quit
//!
//! The first line is the marker: `# blinkterm session: open` while a run has
//! the profile, `# blinkterm session: closed` once it has quit. A run that
//! ends by a quit — `ctrl+q`, the last tab closing, a `SIGTERM` or a `SIGHUP`
//! — writes `closed` ([`Session::finish`]), and marks its window's group
//! closed with it; every other ending leaves `open` and the group `live`: a
//! panic (nothing runs under `panic = "abort"`, and nothing needs to — the
//! file is already right), a `SIGKILL`, the engine dying. So the next start
//! reads a lost group and offers the tabs back ([`plan_for_window`],
//! [`Offer`]). The header stays `open` while any group in the file would be
//! offered, so that an older blinkterm reading it offers it too. There is no
//! marker file to clean up and no lock to consult; the one false positive —
//! the last run still running on this profile — cannot happen, because the
//! profile's lock would have refused this one first. While the offer is up
//! the window it is in records nothing ([`Session::offer_group`]), so a
//! second crash before it is answered still finds the group it was going to
//! offer.
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

/// The most tabs a restore will open, in one group. A hand-written or
/// runaway file of a thousand lines would be a thousand renderers; lines past
/// this in a group are not read.
pub const RESTORE_CAP: usize = 100;

/// The most groups read from the file; past this the first ones in it are
/// not. A run writes no more groups than it had windows open at once — each
/// new window takes the place of one nobody claimed — so this is a bound on
/// a hand-written or runaway file, as [`RESTORE_CAP`] is on its tabs.
pub const MAX_GROUPS: usize = 8;

/// Closed tabs remembered for `ctrl+shift+t`.
pub const CLOSED_KEEP: usize = 20;

/// How long a change may wait before it is on disk: a page that pushes state
/// every frame is one write per half second, and a crash loses at most that
/// much.
pub const WRITE_EVERY: Duration = Duration::from_millis(500);

/// The header's first words; the state is the word after them.
const HEADER: &str = "# blinkterm session:";

/// The line after the header, for the person reading the file: the format
/// is told by the `# window` lines, not by this.
const FORMAT: &str = "# format: 2";

/// A group's first words; its place and its state are the words after them.
const WINDOW: &str = "# window";

/// One window, for as long as it is open: what its tabs are recorded under.
///
/// Today a run has one window, `WindowId(1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u64);

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

/// Whether the run that wrote the file finished with a quit: the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// A run had the profile and never said it was done — or one did, and
    /// left a group in the file that the next start would offer.
    Open,
    /// The run quit.
    Closed,
}

/// How one window's group ended, or that it has not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupState {
    /// Its window is open. Never read from a file: a `live` group there is
    /// from a run that did not finish, and reads as [`GroupState::Lost`].
    Live,
    /// Its window was closed by a quit; `--restore` brings it back.
    Closed,
    /// Its window went without a quit; the next window is offered it.
    Lost,
}

impl GroupState {
    /// The word on the group's line.
    fn word(self) -> &'static str {
        match self {
            GroupState::Live => "live",
            GroupState::Closed => "closed",
            GroupState::Lost => "lost",
        }
    }

    /// The word on a group's line, read at the start: `live` and `lost` are
    /// both a window that went without a quit, and anything else — a word
    /// from a newer blinkterm, a typo — is closed, so that it restores with
    /// `--restore` and never prompts an offer, as a file without a header
    /// does.
    fn read(word: &str) -> GroupState {
        match word {
            "live" | "lost" => GroupState::Lost,
            _ => GroupState::Closed,
        }
    }
}

/// One window's tabs, and how that window ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub state: GroupState,
    pub snapshot: Snapshot,
}

/// What the file said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// Every group with a tab in it, in file order, at most [`MAX_GROUPS`].
    pub groups: Vec<Group>,
    /// The header.
    pub state: State,
}

/// Who a group in the file is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holder {
    /// Nobody yet: read from the file, or left by a window that went.
    Unclaimed,
    /// The window whose row is offering it, until the offer is answered.
    Offered(WindowId),
    /// The open window recording into it.
    Window(WindowId),
}

/// One group of the file as this run keeps it.
#[derive(Debug, Clone)]
struct Slot {
    holder: Holder,
    group: Group,
}

/// The session file, the closed-tab stack, and what the last run left.
#[derive(Debug)]
pub struct Session {
    /// The file, or `None` for a temporary profile.
    path: Option<PathBuf>,
    /// The groups as the next write puts them, in file order: the last
    /// run's that nobody has claimed, and this run's windows.
    slots: Vec<Slot>,
    /// A change not yet on disk.
    dirty: bool,
    wrote_at: Option<Instant>,
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
            slots: Vec::new(),
            dirty: false,
            wrote_at: None,
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
            slots: saved
                .map(|saved| saved.groups)
                .unwrap_or_default()
                .into_iter()
                .map(|group| Slot {
                    holder: Holder::Unclaimed,
                    group,
                })
                .collect(),
            ..Session::in_memory()
        }
    }

    /// The pure half of [`Session::load`]: the header's state, and the
    /// groups.
    ///
    /// A group starts at a `# window <n> <state>` line and has one tab a line
    /// — a mark (`*` in front, `-` not), a tab, the url, a tab, the title.
    /// Tabs before any `# window` line, which is every tab of a file written
    /// before there were groups, are a group of their own whose state is the
    /// header's: lost after `open`, closed otherwise. A line that is not a
    /// tab, or whose url is not one [`Session::keeps`], is skipped; a missing
    /// or unrecognised header is [`State::Closed`], so that a file written by
    /// hand restores with `--restore` and never prompts an offer. No `*` is
    /// the group's first tab in front; two is the first of them. A group with
    /// no tab is no group, past [`RESTORE_CAP`] tabs the rest of a group is
    /// not read, and past [`MAX_GROUPS`] the first groups are dropped. `None`
    /// when no group is left.
    pub fn parse(text: &str) -> Option<Saved> {
        let mut lines = text.lines().peekable();
        let mut state = State::Closed;
        if let Some(header) = lines.peek().and_then(|line| line.strip_prefix(HEADER)) {
            if header.trim() == "open" {
                state = State::Open;
            }
            lines.next();
        }
        let mut groups = Vec::new();
        let mut group = Group {
            state: match state {
                State::Open => GroupState::Lost,
                State::Closed => GroupState::Closed,
            },
            snapshot: Snapshot::default(),
        };
        let mut front = None;
        for line in lines {
            if let Some(rest) = line.strip_prefix(WINDOW) {
                if rest.is_empty() || rest.starts_with([' ', '\t']) {
                    end_group(&mut groups, group, front);
                    group = Group {
                        state: GroupState::read(rest.split_whitespace().nth(1).unwrap_or("")),
                        snapshot: Snapshot::default(),
                    };
                    front = None;
                    continue;
                }
            }
            if group.snapshot.tabs.len() >= RESTORE_CAP {
                continue;
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
                front = Some(group.snapshot.tabs.len());
            }
            group.snapshot.tabs.push(Entry {
                url: url.to_string(),
                title: text::sanitize(fields.next().unwrap_or_default())
                    .trim()
                    .to_string(),
            });
        }
        end_group(&mut groups, group, front);
        if groups.len() > MAX_GROUPS {
            groups.drain(..groups.len() - MAX_GROUPS);
        }
        if groups.is_empty() {
            return None;
        }
        Some(Saved { groups, state })
    }

    /// The pure half of a write: the header, then each group with a tab in
    /// it under its `# window` line, a line a tab, titles plain text on one
    /// line. The header is `open` while any group would be offered — one
    /// live or lost — and `closed` when none would.
    pub fn render(groups: &[Group]) -> String {
        let open = groups
            .iter()
            .any(|group| group.state != GroupState::Closed && !group.snapshot.tabs.is_empty());
        let word = if open { "open" } else { "closed" };
        let mut out = format!("{HEADER} {word}\n{FORMAT}\n");
        let written = groups
            .iter()
            .filter(|group| !group.snapshot.tabs.is_empty());
        for (place, group) in written.enumerate() {
            out.push_str(&format!("{WINDOW} {} {}\n", place + 1, group.state.word()));
            let snapshot = &group.snapshot;
            for (index, entry) in snapshot.tabs.iter().enumerate() {
                let mark = if index == snapshot.active { '*' } else { '-' };
                let title = text::sanitize(&entry.title);
                out.push_str(&format!("{mark}\t{}\t{}\n", entry.url, title.trim()));
            }
        }
        out
    }

    /// The group the next window would be given or offered: the oldest that
    /// nobody has claimed. What [`plan_for_window`] decides on.
    pub fn next_group(&self) -> Option<&Group> {
        self.slots
            .iter()
            .find(|slot| slot.holder == Holder::Unclaimed)
            .map(|slot| &slot.group)
    }

    /// The oldest unclaimed group, for a restore into `window`: it is that
    /// window's now, and stays where it is in the file, live, with the tabs
    /// it had until the window records its own. `None` when there is none.
    ///
    /// For a window that has not recorded anything yet, once; a window that
    /// already continues a group continues that one.
    pub fn take_group(&mut self, window: WindowId) -> Option<Group> {
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.holder == Holder::Unclaimed)?;
        Some(claim(slot, window, &mut self.dirty))
    }

    /// Put the oldest unclaimed group to `window` as a question: it is
    /// promised to that window, no other is offered it, and the window
    /// records nothing until [`Session::take_offered`] — the file keeps
    /// saying what is being offered. Nothing when there is no group.
    pub fn offer_group(&mut self, window: WindowId) {
        if let Some(slot) = self
            .slots
            .iter_mut()
            .find(|slot| slot.holder == Holder::Unclaimed)
        {
            slot.holder = Holder::Offered(window);
        }
    }

    /// The offer to `window`, answered either way: the group is the
    /// window's, as [`Session::take_group`] makes it, and returned for the
    /// caller to restore on a yes. A no consumes it all the same — it is not
    /// offered again — and the window's first tabs of its own replace it.
    /// `None` when nothing was offered to `window`.
    pub fn take_offered(&mut self, window: WindowId) -> Option<Group> {
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.holder == Holder::Offered(window))?;
        Some(claim(slot, window, &mut self.dirty))
    }

    /// `window`'s tabs are now this. Written at once if the last write was
    /// more than [`WRITE_EVERY`] ago, else kept for [`Session::flush`];
    /// nothing if unchanged, or empty — a window with no tabs is not one to
    /// reopen, so the file keeps the last it had — or while an offer is up
    /// in it. A window recording for the first time without having been
    /// given a group takes the place of the oldest unclaimed one. Never an
    /// error: what fails is said by `flush`.
    pub fn record_window(&mut self, window: WindowId, snapshot: Snapshot, now: Instant) {
        if snapshot.tabs.is_empty() || self.holds(Holder::Offered(window)) {
            return;
        }
        let at = self
            .slots
            .iter()
            .position(|slot| slot.holder == Holder::Window(window))
            .or_else(|| {
                self.slots
                    .iter()
                    .position(|slot| slot.holder == Holder::Unclaimed)
            });
        let group = Group {
            state: GroupState::Live,
            snapshot,
        };
        match at {
            Some(at) => {
                let slot = &mut self.slots[at];
                if slot.holder == Holder::Window(window) && slot.group == group {
                    return;
                }
                *slot = Slot {
                    holder: Holder::Window(window),
                    group,
                };
            }
            None => self.slots.push(Slot {
                holder: Holder::Window(window),
                group,
            }),
        }
        self.dirty = true;
        if self.due(now) {
            self.write_pending(now);
        }
    }

    /// `window` was closed by a quit: its group is closed, for a later
    /// `--restore`, and on disk at once. An offer still up in it was never
    /// answered, and is left for the next window.
    pub fn window_closed(&mut self, window: WindowId) {
        self.window_went(window, GroupState::Closed);
        self.write_now();
    }

    /// `window` went without a quit — its terminal vanished: its group is
    /// lost, to be offered to the next window, and on disk at once.
    pub fn window_lost(&mut self, window: WindowId) {
        self.window_went(window, GroupState::Lost);
        self.write_now();
    }

    /// Write what is pending once its time has come. `Err` once, the first
    /// time a write fails, for the row; after that, silence — the row has
    /// better things to say than the same failure twice a second.
    pub fn flush(&mut self, now: Instant) -> Result<(), String> {
        if self.dirty && self.due(now) {
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

    /// The run is ending. A clean end closes every window's group — and the
    /// group of a window whose offer was declined and which had nothing of
    /// its own, so the next start does not offer it again — and writes the
    /// file at once; an offer still up was never answered and its group is
    /// left as it was. An unclean one — the engine died — writes whatever
    /// is still pending, the windows still live, so that its last half
    /// second is not lost with it and the next start offers it. Nothing is
    /// written when nothing changed.
    pub fn finish(&mut self, clean: bool) {
        if clean {
            let windows: Vec<WindowId> = self
                .slots
                .iter()
                .filter_map(|slot| match slot.holder {
                    Holder::Window(window) | Holder::Offered(window) => Some(window),
                    Holder::Unclaimed => None,
                })
                .collect();
            for window in windows {
                self.window_went(window, GroupState::Closed);
            }
        }
        self.write_now();
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

    /// How many tabs `window`'s group has, for the sentence after the engine
    /// dies; `None` for a session kept nowhere, which has nothing to point
    /// at, and for a window with no tabs kept.
    pub fn saved_tabs(&self, window: WindowId) -> Option<usize> {
        self.path.as_ref()?;
        self.slots
            .iter()
            .find(|slot| slot.holder == Holder::Window(window))
            .map(|slot| slot.group.snapshot.tabs.len())
            .filter(|&tabs| tabs > 0)
    }

    /// Whether any group is held by `holder`.
    fn holds(&self, holder: Holder) -> bool {
        self.slots.iter().any(|slot| slot.holder == holder)
    }

    /// `window` is gone: its group, if it has one, is unclaimed and `how`;
    /// a group offered to it is unclaimed and as it was.
    fn window_went(&mut self, window: WindowId, how: GroupState) {
        for slot in &mut self.slots {
            if slot.holder == Holder::Window(window) {
                slot.holder = Holder::Unclaimed;
                slot.group.state = how;
                self.dirty = true;
            } else if slot.holder == Holder::Offered(window) {
                slot.holder = Holder::Unclaimed;
            }
        }
    }

    fn due(&self, now: Instant) -> bool {
        self.wrote_at
            .is_none_or(|at| now.saturating_duration_since(at) >= WRITE_EVERY)
    }

    /// Whatever is pending, now, whenever the last write was.
    fn write_now(&mut self) {
        if self.dirty {
            self.write_pending(Instant::now());
        }
    }

    fn write_pending(&mut self, now: Instant) {
        self.dirty = false;
        self.wrote_at = Some(now);
        if let Err(why) = self.write() {
            self.failed = Some(why);
        }
    }

    /// Every group, whole, to `session.tmp` and renamed over the file.
    fn write(&mut self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let groups: Vec<Group> = self.slots.iter().map(|slot| slot.group.clone()).collect();
        let fresh = path.with_extension("tmp");
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&fresh)
            .and_then(|mut file| file.write_all(Session::render(&groups).as_bytes()))
            .and_then(|()| std::fs::rename(&fresh, path))
            .map_err(|e| format!("cannot save the tabs to {}: {e}", path.display()))
    }
}

/// A group read, onto the list if it has a tab, its front tab the first
/// marked one.
fn end_group(groups: &mut Vec<Group>, mut group: Group, front: Option<usize>) {
    if group.snapshot.tabs.is_empty() {
        return;
    }
    group.snapshot.active = front.unwrap_or(0);
    groups.push(group);
}

/// `slot` is `window`'s, live, and its group as it was is returned.
fn claim(slot: &mut Slot, window: WindowId, dirty: &mut bool) -> Group {
    let was = slot.group.clone();
    slot.holder = Holder::Window(window);
    slot.group.state = GroupState::Live;
    // Written, even with the tabs unchanged: the group is live now, and a
    // crash from here on is this window's to be offered back.
    *dirty = true;
    was
}

/// What a start does with the session, decided before anything is opened.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Tabs to open before anything else, dormant.
    pub restore: Option<Snapshot>,
    /// A question to put on the row instead.
    pub offer: Option<Offer>,
}

/// What a new window does with `saved_next`, the group it would continue
/// ([`Session::next_group`]): `restore` is `--restore`, which restores it
/// whatever way it ended — that is what the flag is for — and the caller
/// then takes it ([`Session::take_group`]). Without it, a group lost by a
/// window that did not quit is offered ([`Session::offer_group`]), and a
/// closed one is left alone: the window's own tabs take its place once it
/// has any. A url on the command line is not the plan's business: it opens
/// after the plan, in a tab of its own.
pub fn plan_for_window(saved_next: Option<&Group>, restore: bool) -> Plan {
    let Some(group) = saved_next.filter(|group| !group.snapshot.tabs.is_empty()) else {
        return Plan::default();
    };
    if restore {
        return Plan {
            restore: Some(group.snapshot.clone()),
            offer: None,
        };
    }
    if group.state != GroupState::Closed {
        return Plan {
            restore: None,
            offer: Some(Offer {
                tabs: group.snapshot.tabs.len(),
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

    const ONE: WindowId = WindowId(1);
    const TWO: WindowId = WindowId(2);

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

    fn group(state: GroupState, urls: &[&str]) -> Group {
        Group {
            state,
            snapshot: snapshot(urls, 0),
        }
    }

    fn on_disk(dir: &Path) -> Option<Saved> {
        Session::parse(&std::fs::read_to_string(dir.join(FILE)).ok()?)
    }

    /// The one group a one-window file has.
    fn only(saved: &Saved) -> &Group {
        assert_eq!(saved.groups.len(), 1, "{saved:?}");
        &saved.groups[0]
    }

    fn states(saved: &Saved) -> Vec<GroupState> {
        saved.groups.iter().map(|group| group.state).collect()
    }

    fn first_urls(saved: &Saved) -> Vec<&str> {
        saved
            .groups
            .iter()
            .map(|group| group.snapshot.tabs[0].url.as_str())
            .collect()
    }

    #[test]
    fn a_file_round_trips_and_says_how_each_window_ended() {
        let snapshot = Snapshot {
            tabs: vec![
                entry("https://example.com/", "Example Domain"),
                entry("https://docs.rs/", ""),
            ],
            active: 1,
        };
        let groups = vec![
            Group {
                state: GroupState::Closed,
                snapshot: snapshot.clone(),
            },
            group(GroupState::Lost, &["https://c.example/"]),
        ];
        let text = Session::render(&groups);
        assert_eq!(
            text,
            "# blinkterm session: open\n\
             # format: 2\n\
             # window 1 closed\n\
             -\thttps://example.com/\tExample Domain\n\
             *\thttps://docs.rs/\t\n\
             # window 2 lost\n\
             *\thttps://c.example/\t\n"
        );
        assert_eq!(
            Session::parse(&text),
            Some(Saved {
                groups,
                state: State::Open
            })
        );

        let closed = vec![Group {
            state: GroupState::Closed,
            snapshot,
        }];
        let text = Session::render(&closed);
        assert!(text.starts_with("# blinkterm session: closed\n"), "{text}");
        assert_eq!(
            Session::parse(&text),
            Some(Saved {
                groups: closed,
                state: State::Closed
            })
        );
    }

    #[test]
    fn a_live_group_reads_as_lost_and_a_word_nobody_knows_as_closed() {
        let text = Session::render(&[
            group(GroupState::Live, &["https://a.example/"]),
            group(GroupState::Closed, &["https://b.example/"]),
        ]);
        assert!(text.contains("# window 1 live\n"), "{text}");
        let saved = Session::parse(&text).expect("a session");
        assert_eq!(saved.state, State::Open);
        assert_eq!(states(&saved), [GroupState::Lost, GroupState::Closed]);

        let saved = Session::parse(
            "# blinkterm session: open\n# window 7 sideways\n-\thttps://a.example/\n\
             # window\n-\thttps://b.example/\n# windows 3 live\n-\thttps://c.example/\n",
        )
        .expect("a session");
        assert_eq!(states(&saved), [GroupState::Closed, GroupState::Closed]);
        assert_eq!(
            saved.groups[1].snapshot.tabs.len(),
            2,
            "`# windows` is a comment, not a group"
        );
    }

    #[test]
    fn a_file_without_groups_is_one_group_lost_if_it_was_left_open() {
        let v1 = |header: &str| {
            Session::parse(&format!(
                "{header}-\thttps://a.example/\tA\n*\thttps://b.example/\tB\n"
            ))
            .expect("a session")
        };
        let open = v1("# blinkterm session: open\n");
        assert_eq!(open.state, State::Open);
        assert_eq!(only(&open).state, GroupState::Lost);
        assert_eq!(only(&open).snapshot.active, 1);
        let closed = v1("# blinkterm session: closed\n");
        assert_eq!(closed.state, State::Closed);
        assert_eq!(only(&closed).state, GroupState::Closed);
        let bare = v1("");
        assert_eq!(only(&bare).state, GroupState::Closed, "written by hand");
    }

    #[test]
    fn an_older_blinkterm_reads_every_group_as_one_window() {
        // What the reader before groups did: the header, then every line
        // that is a mark, a tab and a url — anything starting with `#` is
        // not one.
        let text = Session::render(&[
            group(
                GroupState::Live,
                &["https://a.example/", "https://b.example/"],
            ),
            group(GroupState::Lost, &["https://c.example/"]),
        ]);
        let tabs: Vec<&str> = text
            .lines()
            .skip(1)
            .filter(|line| !line.starts_with('#'))
            .collect();
        assert_eq!(
            tabs,
            [
                "*\thttps://a.example/\t",
                "-\thttps://b.example/\t",
                "*\thttps://c.example/\t"
            ]
        );
        assert!(text.starts_with("# blinkterm session: open\n"), "offered");
        assert!(text
            .lines()
            .skip(1)
            .all(|line| line.starts_with('#') || line.starts_with(['*', '-'])));
    }

    #[test]
    fn a_broken_line_is_skipped_and_a_missing_header_is_a_closed_session() {
        let text = "junk\n-\tabout:blank\tBlank\n*\thttps://a.example/\tA\tand\x1b]0;x\x07\n\
                    ?\thttps://b.example/\n-\n*\thttps://c.example/\r\n# comment\n";
        let saved = Session::parse(text).expect("a session");
        assert_eq!(saved.state, State::Closed, "no header");
        let group = only(&saved);
        assert_eq!(
            group.snapshot.tabs,
            [
                entry("https://a.example/", "A and]0;x"),
                entry("https://c.example/", "")
            ]
        );
        assert_eq!(group.snapshot.active, 0, "the first of two *");

        let saved = Session::parse("# blinkterm session: open\n-\thttps://a.example/\n")
            .expect("a session");
        assert_eq!(
            (saved.state, only(&saved).snapshot.active),
            (State::Open, 0)
        );
        assert_eq!(
            Session::parse("# blinkterm session: sideways\n-\thttps://a.example/\n")
                .map(|saved| saved.state),
            Some(State::Closed)
        );
        assert_eq!(Session::parse("# blinkterm session: open\n"), None);
        assert_eq!(
            Session::parse("# blinkterm session: open\n# window 1 live\n# window 2 lost\n"),
            None,
            "groups with no tabs are no groups"
        );
        assert_eq!(Session::parse(""), None);

        let many: String = (0..RESTORE_CAP + 5)
            .map(|n| format!("-\thttps://example.com/{n}\n"))
            .collect();
        let twice = format!("{many}# window 2 closed\n{many}");
        let saved = Session::parse(&twice).expect("a session");
        assert_eq!(saved.groups.len(), 2);
        for group in &saved.groups {
            assert_eq!(group.snapshot.tabs.len(), RESTORE_CAP, "per group");
        }
    }

    #[test]
    fn past_the_cap_the_oldest_groups_go_and_groups_are_taken_oldest_first() {
        let text: String = (0..MAX_GROUPS + 2)
            .map(|n| format!("# window {n} closed\n-\thttps://example.com/{n}\n"))
            .collect();
        let saved = Session::parse(&text).expect("a session");
        assert_eq!(saved.groups.len(), MAX_GROUPS);
        assert_eq!(
            saved.groups[0].snapshot.tabs[0].url,
            "https://example.com/2"
        );

        let dir = scratch("fifo");
        std::fs::write(dir.join(FILE), &text).expect("a file");
        let mut session = Session::load(&dir);
        assert_eq!(
            session
                .next_group()
                .map(|g| g.snapshot.tabs[0].url.as_str()),
            Some("https://example.com/2")
        );
        let taken: Vec<String> = (1..=3)
            .map(|n| {
                let group = session.take_group(WindowId(n)).expect("a group");
                assert_eq!(group.state, GroupState::Closed, "as it was");
                group.snapshot.tabs[0].url.clone()
            })
            .collect();
        assert_eq!(
            taken,
            [
                "https://example.com/2",
                "https://example.com/3",
                "https://example.com/4"
            ]
        );

        // A window with nothing to take, recording, takes the place of the
        // oldest unclaimed group; the taken ones stay where they were.
        let t = Instant::now();
        session.record_window(WindowId(9), snapshot(&["https://new.example/"], 0), t);
        session.finish(true);
        let saved = on_disk(&dir).expect("written");
        assert_eq!(saved.groups.len(), MAX_GROUPS);
        assert_eq!(
            first_urls(&saved),
            [
                "https://example.com/2",
                "https://example.com/3",
                "https://example.com/4",
                "https://new.example/",
                "https://example.com/6",
                "https://example.com/7",
                "https://example.com/8",
                "https://example.com/9",
            ],
            "the new window took the 5th's place"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_windows_are_never_dropped_and_a_new_one_reuses_a_closed_ones_place() {
        let mut session = Session::in_memory();
        let t = Instant::now();
        let record = |session: &mut Session, n: u64| {
            session.record_window(
                WindowId(n),
                snapshot(&[&format!("https://example.com/{n}")], 0),
                t,
            )
        };
        for n in 1..=MAX_GROUPS as u64 + 1 {
            record(&mut session, n);
        }
        assert_eq!(session.slots.len(), MAX_GROUPS + 1, "no open window goes");
        session.window_closed(WindowId(3));
        session.window_closed(WindowId(5));
        // A new window takes the oldest unclaimed group's place.
        record(&mut session, 50);
        assert_eq!(session.slots.len(), MAX_GROUPS + 1);
        assert_eq!(session.slots[2].holder, Holder::Window(WindowId(50)));
        record(&mut session, 51);
        assert_eq!(session.slots.len(), MAX_GROUPS + 1);
        assert_eq!(session.slots[4].holder, Holder::Window(WindowId(51)));
        assert!(session
            .slots
            .iter()
            .all(|slot| matches!(slot.holder, Holder::Window(_))));
        // Read again, the file is held to the cap, the oldest going.
        session.finish(true);
        let groups: Vec<Group> = session.slots.iter().map(|s| s.group.clone()).collect();
        let saved = Session::parse(&Session::render(&groups)).expect("a session");
        assert_eq!(saved.groups.len(), MAX_GROUPS);
        assert_eq!(
            saved.groups[0].snapshot.tabs[0].url,
            "https://example.com/2"
        );
    }

    #[test]
    fn a_live_group_written_and_read_again_is_lost() {
        let dir = scratch("live");
        let mut session = Session::load(&dir);
        let t = Instant::now();
        session.record_window(ONE, snapshot(&["https://a.example/"], 0), t);
        assert!(session.flush(t).is_ok());
        let text = std::fs::read_to_string(dir.join(FILE)).expect("written");
        assert!(text.contains("# window 1 live\n"), "{text}");
        let saved = on_disk(&dir).expect("a session");
        assert_eq!(saved.state, State::Open);
        assert_eq!(only(&saved).state, GroupState::Lost);
        let next = Session::load(&dir);
        assert_eq!(
            plan_for_window(next.next_group(), false).offer,
            Some(Offer { tabs: 1 })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unclaimed_groups_are_written_back_and_a_new_window_takes_the_oldest_ones_place() {
        let dir = scratch("unclaimed");
        let before = Session::render(&[
            group(GroupState::Closed, &["https://old1.example/"]),
            group(GroupState::Closed, &["https://old2.example/"]),
        ]);
        std::fs::write(dir.join(FILE), &before).expect("the last run's");
        let mut session = Session::load(&dir);
        assert_eq!(
            plan_for_window(session.next_group(), false),
            Plan::default()
        );
        let t = Instant::now();
        session.record_window(ONE, snapshot(&["https://mine.example/"], 0), t);
        let saved = on_disk(&dir).expect("written");
        assert_eq!(
            first_urls(&saved),
            ["https://mine.example/", "https://old2.example/"]
        );
        assert_eq!(states(&saved), [GroupState::Lost, GroupState::Closed]);
        session.window_closed(ONE);
        let saved = on_disk(&dir).expect("written");
        assert_eq!(saved.state, State::Closed);
        assert_eq!(states(&saved), [GroupState::Closed, GroupState::Closed]);
        // The next --restore brings back the newest window first.
        let mut next = Session::load(&dir);
        assert_eq!(
            plan_for_window(next.next_group(), true).restore,
            Some(snapshot(&["https://mine.example/"], 0))
        );
        assert!(next.take_group(ONE).is_some());
        assert_eq!(
            next.next_group().map(|g| g.snapshot.tabs[0].url.as_str()),
            Some("https://old2.example/")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_offered_group_is_promised_to_one_window_and_a_lost_window_is_offered_next() {
        let dir = scratch("offers");
        std::fs::write(
            dir.join(FILE),
            Session::render(&[
                group(GroupState::Lost, &["https://g1.example/"]),
                group(GroupState::Lost, &["https://g2.example/"]),
            ]),
        )
        .expect("a crashed run's");
        let mut session = Session::load(&dir);
        session.offer_group(ONE);
        let next = session.next_group().expect("another");
        assert_eq!(next.snapshot.tabs[0].url, "https://g2.example/");
        session.offer_group(TWO);
        assert_eq!(session.next_group(), None);
        let t = Instant::now();
        session.record_window(ONE, snapshot(&["https://x.example/"], 0), t);
        assert_eq!(
            first_urls(&on_disk(&dir).expect("unchanged")),
            ["https://g1.example/", "https://g2.example/"],
            "a window with an offer up records nothing"
        );
        let yes = session.take_offered(TWO).expect("offered to two");
        assert_eq!(yes.snapshot.tabs[0].url, "https://g2.example/");
        assert_eq!(session.take_offered(TWO), None, "once");
        // One goes without answering: its offer goes back, unanswered.
        session.window_lost(ONE);
        assert_eq!(
            session
                .next_group()
                .map(|g| g.snapshot.tabs[0].url.as_str()),
            Some("https://g1.example/")
        );
        // Two recorded its own, then its terminal vanished.
        session.record_window(
            TWO,
            snapshot(&["https://y.example/"], 0),
            t + Duration::from_secs(1),
        );
        session.window_lost(TWO);
        let saved = on_disk(&dir).expect("written");
        assert_eq!(saved.state, State::Open);
        assert_eq!(states(&saved), [GroupState::Lost, GroupState::Lost]);
        assert_eq!(
            first_urls(&saved),
            ["https://g1.example/", "https://y.example/"]
        );
        let _ = std::fs::remove_dir_all(&dir);
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
        session.record_window(ONE, snapshot(&["https://a.example/"], 0), start);
        assert!(session.flush(start).is_ok());
        let saved = on_disk(&dir).expect("written at once");
        assert_eq!(saved.state, State::Open);
        std::fs::remove_file(dir.join(FILE)).expect("removed");
        let later = start + Duration::from_secs(5);
        session.record_window(ONE, snapshot(&["https://a.example/"], 0), later);
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
        session.record_window(ONE, snapshot(&["https://a.example/"], 0), t);
        session.record_window(
            ONE,
            snapshot(&["https://a.example/", "https://b.example/"], 1),
            t + Duration::from_millis(100),
        );
        session.record_window(
            ONE,
            snapshot(&["https://a.example/", "https://c.example/"], 1),
            t + Duration::from_millis(200),
        );
        assert!(session.flush(t + Duration::from_millis(300)).is_ok());
        assert_eq!(
            only(&on_disk(&dir).expect("the first")).snapshot.tabs.len(),
            1,
            "only the first is on disk before half a second"
        );
        assert!(session.flush(t + Duration::from_millis(600)).is_ok());
        assert_eq!(
            only(&on_disk(&dir).expect("the last")).snapshot,
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
        session.record_window(ONE, snapshot(&["https://a.example/"], 0), t);
        session.record_window(ONE, Snapshot::default(), t + Duration::from_secs(1));
        assert!(session.flush(t + Duration::from_secs(2)).is_ok());
        assert_eq!(only(&on_disk(&dir).expect("kept")).snapshot.tabs.len(), 1);
        session.finish(true);
        let saved = on_disk(&dir).expect("kept");
        assert_eq!(saved.state, State::Closed);
        assert_eq!(
            (only(&saved).state, only(&saved).snapshot.tabs.len()),
            (GroupState::Closed, 1)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clean_finish_says_closed_and_an_unclean_one_writes_what_was_pending_open() {
        let dir = scratch("finish");
        let mut session = Session::load(&dir);
        let t = Instant::now();
        session.record_window(ONE, snapshot(&["https://a.example/"], 0), t);
        session.record_window(
            ONE,
            snapshot(&["https://a.example/", "https://b.example/"], 1),
            t + Duration::from_millis(10),
        );
        assert_eq!(only(&on_disk(&dir).expect("first")).snapshot.tabs.len(), 1);
        session.finish(false);
        let saved = on_disk(&dir).expect("pending written");
        assert_eq!(saved.state, State::Open);
        assert_eq!(
            (only(&saved).state, only(&saved).snapshot.tabs.len()),
            (GroupState::Lost, 2)
        );
        session.finish(true);
        let saved = on_disk(&dir).expect("closed");
        assert_eq!(saved.state, State::Closed);
        assert_eq!(
            (only(&saved).state, only(&saved).snapshot.tabs.len()),
            (GroupState::Closed, 2)
        );

        // A declined offer is not offered again after a clean quit.
        std::fs::write(
            dir.join(FILE),
            "# blinkterm session: open\n*\thttps://a.example/\t\n",
        )
        .expect("an open session, as an older blinkterm left it");
        let mut next = Session::load(&dir);
        next.offer_group(ONE);
        assert!(next.take_offered(ONE).is_some(), "declined");
        next.finish(true);
        let saved = on_disk(&dir).expect("kept");
        assert_eq!(saved.state, State::Closed);
        assert_eq!(only(&saved).state, GroupState::Closed);
        assert_eq!(
            plan_for_window(Session::load(&dir).next_group(), true).restore,
            Some(snapshot(&["https://a.example/"], 0)),
            "and --restore still has it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_written_while_the_offer_is_up() {
        let dir = scratch("held");
        let before = "# blinkterm session: open\n*\thttps://old.example/\t\n";
        std::fs::write(dir.join(FILE), before).expect("the last run's");
        let mut session = Session::load(&dir);
        session.offer_group(ONE);
        let t = Instant::now();
        session.record_window(ONE, snapshot(&["https://new.example/"], 0), t);
        assert!(session.flush(t + Duration::from_secs(1)).is_ok());
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE)).expect("kept"),
            before
        );
        session.take_offered(ONE);
        session.record_window(
            ONE,
            snapshot(&["https://new.example/"], 0),
            t + Duration::from_secs(2),
        );
        assert_eq!(
            only(&on_disk(&dir).expect("written")).snapshot.tabs[0].url,
            "https://new.example/"
        );

        // A quit with the offer still up leaves the file as it was: the
        // question was never answered.
        std::fs::write(dir.join(FILE), before).expect("the last run's");
        let mut session = Session::load(&dir);
        session.offer_group(ONE);
        session.record_window(ONE, snapshot(&["https://new.example/"], 0), t);
        session.finish(true);
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE)).expect("kept"),
            before
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restore_makes_the_group_live_at_once() {
        let dir = scratch("restore");
        std::fs::write(
            dir.join(FILE),
            "# blinkterm session: closed\n*\thttps://a.example/\t\n",
        )
        .expect("a quit run's");
        let mut session = Session::load(&dir);
        let plan = plan_for_window(session.next_group(), true);
        let group = session.take_group(ONE).expect("the group");
        assert_eq!(plan.restore, Some(group.snapshot.clone()));
        assert_eq!(session.saved_tabs(ONE), Some(1));
        // The restored tabs are what the window records, and still the
        // group is written live: a crash from here is offered back.
        let t = Instant::now();
        session.record_window(ONE, group.snapshot, t);
        assert!(session.flush(t).is_ok());
        let saved = on_disk(&dir).expect("written");
        assert_eq!(saved.state, State::Open);
        assert_eq!(only(&saved).state, GroupState::Lost);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_first_write_that_fails_is_said_once() {
        let dir = scratch("fails");
        let file = dir.join("not-a-dir");
        std::fs::write(&file, b"a file").expect("a file");
        let mut session = Session::load(&file);
        let t = Instant::now();
        session.record_window(ONE, snapshot(&["https://a.example/"], 0), t);
        let why = session.flush(t).unwrap_err();
        assert!(why.contains("session"), "{why}");
        session.record_window(
            ONE,
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
    fn the_plan_restores_on_the_flag_offers_a_lost_group_and_leaves_a_closed_one() {
        let urls = ["https://a.example/", "https://b.example/"];
        let lost = group(GroupState::Lost, &urls);
        let closed = group(GroupState::Closed, &urls);
        let live = group(GroupState::Live, &urls);
        for any in [&lost, &closed, &live] {
            assert_eq!(
                plan_for_window(Some(any), true),
                Plan {
                    restore: Some(any.snapshot.clone()),
                    offer: None
                },
                "{any:?}"
            );
        }
        for offered in [&lost, &live] {
            assert_eq!(
                plan_for_window(Some(offered), false),
                Plan {
                    restore: None,
                    offer: Some(Offer { tabs: 2 })
                }
            );
        }
        assert_eq!(plan_for_window(Some(&closed), false), Plan::default());
        assert_eq!(plan_for_window(None, true), Plan::default());
        assert_eq!(plan_for_window(None, false), Plan::default());
        let empty = group(GroupState::Lost, &[]);
        assert_eq!(plan_for_window(Some(&empty), false), Plan::default());
        assert_eq!(plan_for_window(Some(&empty), true), Plan::default());
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
        session.record_window(ONE, snapshot(&["https://a.example/"], 0), t);
        assert!(session.flush(t).is_ok());
        session.finish(true);
        assert_eq!(session.saved_tabs(ONE), None, "nothing to point at");
        session.closed(entry("https://a.example/", "A"));
        assert_eq!(session.reopen(), Some(entry("https://a.example/", "A")));
    }
}
