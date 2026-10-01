//! A profile's backend: the one process that holds the profile, its engine
//! and every window on it, for however many terminals are attached.
//!
//! Issue #83 asked for a second terminal on a profile that is in use to open
//! a window of its own rather than be refused. A profile directory takes one
//! engine — two Chromiums writing one cookie jar is the corruption the lock
//! has always prevented — so the second terminal has to reach the first
//! one's engine. That is this process: started by the first terminal's
//! frontend ([`crate::frontend`]) as this same program with the hidden
//! `--serve-fd`, in a session of its own (`setsid`) with no terminal, and
//! reached by every other frontend on the profile through `backend.sock`
//! ([`crate::ipc`]).
//!
//! # A window per terminal
//!
//! Each frontend that attaches opens one *window*: a [`crate::app`] window,
//! its tabs, its row and prompts, its motion policy and its layout, exactly
//! the window a run used to have, drawn through a `RemoteTerminal` that
//! turns everything the window asks of a terminal into a message. The first
//! window adopts the engine's first page; every later one is a page in an
//! engine window of its own (`Target.createTarget {newWindow: true}`), which
//! casts, takes input and is sized independently of the others — measured,
//! see [`crate::tabs`]. What a window does not own — the history, the
//! bookmarks, the session, the downloads, the blocker — is the profile's
//! [`crate::app`] `Shared`, kept once here for all of them.
//!
//! One loop drives every window, on the one CDP connection the engine has:
//! each pass runs every window's half before the poll, polls everything at
//! once — the backend socket, every frontend, the browser's connection,
//! every window's page in front, the `--remote` socket — then reads every
//! frontend, routes the browser's news to the windows, and runs every
//! window's half after the poll.
//!
//! # Which window a new page belongs to
//!
//! The browser's connection announces every page target once, and a page
//! must land in exactly one window: never broadcast, never handed to
//! whichever terminal typed last. In order: a target a window already has
//! (one it asked for) is that window's; one whose opener a window has (a
//! `target=_blank`, a `window.open`) is the opener's window's. A link opened
//! with a modifier names no opener at all, and there the opener's own
//! session has said `Page.frameRequestedNavigation` with a `newTab`
//! disposition and the url a moment before ([`crate::tabs`] has the
//! measurements): such a target is held, unrouted, until its url is known,
//! and given to the one window with a disposition for that url in the last
//! two seconds. A target nobody can be shown to have asked for, or that two
//! windows asked for at once, is closed rather than guessed. With one window
//! there is nobody to confuse it with, and it is that window's at once.
//!
//! # Frames
//!
//! A frame goes to its frontend still encoded, numbered, and the frontend
//! says when it has painted it ([`crate::ipc::ToBackend::Painted`]). At most
//! [`IN_FLIGHT`] are out at once per window and a newer frame replaces one
//! that is waiting (`FrameQueue`), so a slow terminal — an ssh link, a
//! suspended `tmux` client — costs its own window frames and nothing else.
//! On a paced route the engine's acknowledgement waits for the paint, as it
//! did in one process. A window whose oldest frame has been out for
//! [`STALL`] stops casting until its terminal catches up.
//!
//! A terminal that has handed itself to a helper — a `file-picker-terminal`
//! picker, a password command run in the terminal — is not painting on
//! purpose, and is not a stalled one (issue #105). The helper runs inside
//! the frontend's loop, so nothing is acknowledged until it exits, and five
//! seconds of a picker is nothing unusual. So the backend holds that
//! window's cast itself from the moment it sends the job until the
//! frontend's answer: the frames out are given up and none is sent, and the
//! answer lays the window out again — at the size the frontend says just
//! before it, since the terminal may have been resized meanwhile — and
//! starts the cast with a fresh frame. The frontend's half is in
//! [`crate::frontend`]; no message was added for it, since the job and its
//! answer already say when the terminal went and when it came back.
//!
//! # Lifecycle
//!
//! A frontend whose connection ends without a word suspends its window: the
//! page stops casting and the window waits [`GRACE`] for a frontend that
//! resumes it by its nonce; after that its tabs are the session's lost group,
//! offered to the next window, and the window goes. A frontend that says
//! `close` — `ctrl+q`, a hang-up — closes its window at once and its tabs
//! become a closed group for a later `--restore`. When no window is left and
//! nobody is attaching, the backend stops the way a run always stopped:
//! the session written, the downloads cancelled, `Browser.close` and a wait
//! for the cookie jar to be written, the sockets removed, and the profile
//! lock released last. A frontend that tries to attach during those seconds
//! is told to wait, and then starts a fresh backend.
//!
//! The engine dying is every window's business at once: it is started again
//! on the same profile without the lock ever being let go, and each window's
//! tabs come back in it (`app::relaunch_all`); a second death in a
//! minute stops the backend, and every terminal is told why.
//!
//! A backend that is itself killed outright leaves its engine's group
//! behind it, and nothing in the lock says so. So it writes the group into
//! the profile ([`crate::engine::PGID_FILE`]), and the next backend kills
//! whatever is left of it before starting another engine
//! ([`crate::engine::reap_orphan`]).

use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::app::{self, Live, Pass, Shared, Window};
use crate::appearance::Appearance;
use crate::cdp::Client;
use crate::cdp::Event;
use crate::engine;
use crate::fit::Metrics;
use crate::hover::Shape;
use crate::ipc::{
    self, BrowserSettings, CloseWhy, FrameKind, Job, Outcome, Picked, RouteFlags, ToBackend,
    ToFrontend,
};
use crate::json::Json;
use crate::login;
use crate::options::Options;
use crate::picker;
use crate::profile::{self, Choice, Profile, TakeAt};
use crate::screen;
use crate::session::WindowId;
use crate::tabs::{self, Change, Tabs};
use crate::terminal::{Encoded, FrameOut, Helper, HelperOutcome, Painted, Started, Terminal};
use crate::tty;

/// How long a window whose frontend vanished waits for it to come back.
pub const GRACE: Duration = Duration::from_secs(15);

/// The most frames out with a frontend at once, per window.
pub const IN_FLIGHT: usize = 2;

/// How long a frame may be out before the window stops casting.
pub const STALL: Duration = Duration::from_secs(5);

/// How long a connection may take to say `hello` and `open`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a page nobody can be shown to have asked for is held before it
/// is closed: as long as a disposition is kept.
const HELD_FOR: Duration = app::DISPOSITION_WITHIN;

/// The longest a pass waits with nothing to do, and the shortest, while a
/// frame is owed or a frontend has not taken everything it was sent.
const POLL_MS: i32 = 50;
const BUSY_POLL_MS: i32 = 4;

/// How often the engine is asked whether it is alive with no window to do
/// it.
const CHECK_EVERY: Duration = Duration::from_millis(500);

/// The bytes read from a connection in one go.
const READ_CHUNK: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// Frames.
// ---------------------------------------------------------------------------

/// One window's frames on their way to its terminal: numbered, at most
/// [`IN_FLIGHT`] out, the newest waiting one replacing an older one, and the
/// window stalled when its terminal has stopped saying it painted — or
/// paused, when the terminal is with a helper and is not painting on
/// purpose (see the module's section on frames).
#[derive(Debug, Default)]
pub(crate) struct FrameQueue {
    next_seq: u64,
    in_flight: VecDeque<(u64, Instant)>,
    pending: Option<ipc::Frame>,
    stalled: bool,
    /// How long the last frame the terminal painted waited to be written,
    /// for the window's throttle; taken once.
    waited: Option<Duration>,
    /// The terminal is with a helper: every frame is dropped, nothing is
    /// out, and nothing is late.
    paused: bool,
    /// The last frame numbered before the pause. The terminal still says
    /// it painted those, after the helper; how long they waited is the
    /// helper's time, not the link's, and is no news for the throttle.
    paused_through: u64,
}

impl FrameQueue {
    /// A frame the window wants painted: numbered and returned to send now,
    /// or held — replacing a frame already held — while [`IN_FLIGHT`] are
    /// out.
    /// Paused, it is dropped: not numbered, not held, not sent.
    pub(crate) fn offer(&mut self, mut frame: ipc::Frame, now: Instant) -> Option<ipc::Frame> {
        if self.paused {
            return None;
        }
        self.next_seq += 1;
        frame.seq = self.next_seq;
        if self.in_flight.len() < IN_FLIGHT {
            self.in_flight.push_back((frame.seq, now));
            Some(frame)
        } else {
            self.pending = Some(frame);
            None
        }
    }

    /// The terminal painted (or dropped) every frame up to `seq`: they are
    /// no longer out, and the frame held, if one is, goes now.
    pub(crate) fn painted(
        &mut self,
        seq: u64,
        waited: Option<Duration>,
        now: Instant,
    ) -> Option<ipc::Frame> {
        self.in_flight.retain(|(out, _)| *out > seq);
        if waited.is_some() && seq > self.paused_through {
            self.waited = waited;
        }
        if self.in_flight.len() < IN_FLIGHT {
            if let Some(frame) = self.pending.take() {
                self.in_flight.push_back((frame.seq, now));
                return Some(frame);
            }
        }
        None
    }

    /// Whether every frame handed over has been painted.
    pub(crate) fn all_painted(&self) -> bool {
        self.in_flight.is_empty() && self.pending.is_none()
    }

    /// Once a pass: `Some(true)` when the oldest frame out has been out
    /// longer than [`STALL`] and the window should stop casting,
    /// `Some(false)` when a stalled terminal has caught up and it should
    /// start again, `None` for no change.
    ///
    /// Paused, never: the cast is held for another reason, and by
    /// [`RemoteTerminal::hold`].
    pub(crate) fn stall(&mut self, now: Instant) -> Option<bool> {
        if self.paused {
            return None;
        }
        let late = self
            .in_flight
            .front()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) > STALL);
        if !self.stalled && late {
            self.stalled = true;
            return Some(true);
        }
        if self.stalled && self.in_flight.is_empty() {
            self.stalled = false;
            self.pending = None;
            return Some(false);
        }
        None
    }

    /// Nothing out any more: the terminal went, or came back.
    pub(crate) fn clear(&mut self) {
        self.in_flight.clear();
        self.pending = None;
        self.stalled = false;
        self.waited = None;
        self.paused = false;
    }

    /// The terminal has gone to a helper. What is out is given up — the
    /// terminal paints nothing until the helper exits, and the window is
    /// laid out again after — so that [`FrameQueue::all_painted`] holds
    /// and the engine's frame owed an acknowledgement is acknowledged; and
    /// every frame from here is dropped, until [`FrameQueue::resume`].
    pub(crate) fn pause(&mut self) {
        self.paused_through = self.next_seq;
        self.in_flight.clear();
        self.pending = None;
        self.stalled = false;
        self.waited = None;
        self.paused = true;
    }

    /// The terminal is back from its helper: frames are sent again,
    /// numbered on from where they were.
    pub(crate) fn resume(&mut self) {
        self.paused = false;
    }
}

// ---------------------------------------------------------------------------
// A connection.
// ---------------------------------------------------------------------------

/// One frontend's connection: the socket, non-blocking, what has been read
/// of it, and what is on its way out.
pub(crate) struct Conn {
    stream: UnixStream,
    decoder: ipc::Decoder,
    outbox: ipc::Outbox,
    /// Why it is no good any more, once it is not: a write that failed, an
    /// outbox past its cap, a message that made no sense, the end.
    broken: Option<String>,
}

impl Conn {
    pub(crate) fn new(stream: UnixStream) -> Conn {
        let _ = stream.set_nonblocking(true);
        Conn {
            stream,
            decoder: ipc::Decoder::new(),
            outbox: ipc::Outbox::new(),
            broken: None,
        }
    }

    fn fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// `message`, after everything sent before it. A connection that cannot
    /// take it is broken, and dropped by the loop.
    fn send(&mut self, message: &ToFrontend) {
        if self.broken.is_some() {
            return;
        }
        if let Err(why) = self.outbox.send(message) {
            self.broken = Some(why);
        }
    }

    /// As much of the outbox as the socket takes now.
    fn flush(&mut self) {
        if self.broken.is_some() || self.outbox.is_empty() {
            return;
        }
        if let Err(why) = self.outbox.flush(&mut self.stream) {
            self.broken = Some(why);
        }
    }

    /// Everything it has to say that has arrived, whole messages only. The
    /// end, or a message that makes no sense, breaks it.
    fn read(&mut self, buf: &mut [u8]) -> Vec<ToBackend> {
        let mut messages = Vec::new();
        if self.broken.is_some() {
            return messages;
        }
        // A bounded number of reads, so that one chatty frontend cannot keep
        // the loop from the others.
        for _ in 0..16 {
            match self.stream.read(buf) {
                Ok(0) => {
                    self.broken = Some("the frontend hung up".to_string());
                    break;
                }
                Ok(n) => match self.decoder.feed::<ToBackend>(&buf[..n]) {
                    Ok(more) => messages.extend(more),
                    Err(why) => {
                        self.broken = Some(why);
                        break;
                    }
                },
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => {
                    self.broken = Some(e.to_string());
                    break;
                }
            }
        }
        messages
    }

    /// Write what is queued, waiting up to `within` for the socket to take
    /// it: for the last words before a connection is dropped.
    fn drain(&mut self, within: Duration) {
        let deadline = Instant::now() + within;
        while self.broken.is_none() && !self.outbox.is_empty() && Instant::now() < deadline {
            self.flush();
            if !self.outbox.is_empty() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The terminal a window is drawn on, in another process.
// ---------------------------------------------------------------------------

/// A window's terminal as the backend has it: everything
/// [`Terminal`] asks for, said as a message to the frontend that holds it,
/// and that frontend's answers kept for the window to ask for. With no
/// connection — the frontend vanished and the window is waiting for it —
/// what is written goes nowhere.
pub(crate) struct RemoteTerminal {
    conn: Option<Conn>,
    frames: FrameQueue,
    route: RouteFlags,
    /// The size the frontend said last, until the window asks.
    resized: Option<Metrics>,
    /// The helpers started and not yet answered or ended.
    waiting: Vec<u64>,
    /// The answers that have come and not been asked for.
    answers: HashMap<u64, HelperOutcome>,
    /// The helpers given the terminal itself and not yet said done by the
    /// frontend: while there is one, the window is not painting on purpose,
    /// and its cast is held rather than stalled. Not the same list as
    /// `waiting`, which forgets a helper the window has stopped wanting
    /// while the frontend still has it running.
    in_terminal: Vec<u64>,
    /// Whether the window's cast is held for them: what
    /// [`RemoteTerminal::hold`] said last.
    held: bool,
}

impl RemoteTerminal {
    pub(crate) fn new(conn: Conn, route: RouteFlags) -> RemoteTerminal {
        RemoteTerminal {
            conn: Some(conn),
            frames: FrameQueue::default(),
            route,
            resized: None,
            waiting: Vec::new(),
            answers: HashMap::new(),
            in_terminal: Vec::new(),
            held: false,
        }
    }

    fn send(&mut self, message: &ToFrontend) {
        if let Some(conn) = self.conn.as_mut() {
            conn.send(message);
        }
    }

    /// The frontend painted up to `seq`.
    fn painted_up_to(&mut self, seq: u64, waited: Option<Duration>) {
        if let Some(frame) = self.frames.painted(seq, waited, Instant::now()) {
            self.send(&ToFrontend::Frame(frame));
        }
    }

    /// A helper's answer from the frontend, for the window's next ask; one
    /// for a helper nobody waits for any more is dropped (and a password
    /// with it, overwritten).
    fn answer(&mut self, id: u64, outcome: HelperOutcome) {
        if self.waiting.contains(&id) {
            self.answers.insert(id, outcome);
        }
    }

    /// The frontend said helper `id` is done, whatever its answer and
    /// whether or not anybody still wants it: if it had the terminal, the
    /// terminal is back, and with the last of them the frames go again.
    pub(crate) fn terminal_back(&mut self, id: u64) {
        let had = !self.in_terminal.is_empty();
        self.in_terminal.retain(|running| *running != id);
        if had && self.in_terminal.is_empty() {
            self.frames.resume();
        }
    }

    /// Once a pass, after the window's own half: `Some(true)` when a helper
    /// has just taken the terminal and the window's cast is to be held,
    /// `Some(false)` when the terminal has just come back from the last one
    /// and the window is to be laid out and cast again, `None` for no
    /// change — and then the [`FrameQueue`]'s own stall decides.
    pub(crate) fn hold(&mut self) -> Option<bool> {
        let wanted = !self.in_terminal.is_empty();
        if wanted == self.held {
            return None;
        }
        self.held = wanted;
        Some(wanted)
    }

    /// The frontend is gone: nothing out with it any more, and no helper
    /// holding the cast — a window without a terminal holds its own
    /// ([`app::suspend_window`]), and one taken back is cast again
    /// ([`app::resume_window`]).
    fn detach(&mut self) -> Option<Conn> {
        self.frames.clear();
        self.waiting.clear();
        self.answers.clear();
        self.in_terminal.clear();
        self.held = false;
        self.resized = None;
        self.conn.take()
    }

    /// A frontend took the window back.
    fn attach(&mut self, conn: Conn, route: RouteFlags) {
        self.frames.clear();
        self.in_terminal.clear();
        self.held = false;
        self.route = route;
        self.conn = Some(conn);
    }

    fn linked(&self) -> bool {
        self.conn.is_some()
    }
}

impl Terminal for RemoteTerminal {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.send(&ToFrontend::Text(bytes.to_vec()));
        Ok(())
    }

    fn clear_picture(&mut self) -> Result<(), String> {
        self.send(&ToFrontend::ClearPicture);
        Ok(())
    }

    fn clear_screen(&mut self) -> Result<(), String> {
        self.send(&ToFrontend::ClearScreen);
        Ok(())
    }

    /// Encoded as it came; numbered and sent, or held, by the
    /// [`FrameQueue`]. Whether it decodes is the frontend's to find out, so
    /// this always says it did.
    fn frame(&mut self, frame: FrameOut<'_>) -> Result<bool, String> {
        if self.conn.is_none() {
            return Ok(true);
        }
        let (kind, image) = match frame.payload {
            Encoded::Jpeg(bytes) => (FrameKind::Jpeg, bytes.to_vec()),
            Encoded::Png(bytes) => (FrameKind::Png, bytes.to_vec()),
        };
        let out = ipc::Frame {
            seq: 0,
            viewport_gen: frame.viewport_gen,
            cells: frame.cells,
            row: frame.row,
            kind,
            image,
        };
        if let Some(out) = self.frames.offer(out, Instant::now()) {
            self.send(&ToFrontend::Frame(out));
        }
        Ok(true)
    }

    fn painted(&mut self) -> Painted {
        Painted {
            all: self.frames.all_painted(),
            waited: self.frames.waited.take(),
        }
    }

    fn paced(&self) -> bool {
        self.route.paced
    }

    fn png_route(&self) -> bool {
        self.route.png
    }

    fn shape(&mut self, shape: Shape) -> Result<(), String> {
        self.write(&screen::pointer_shape(shape.name()))
    }

    fn resized(&mut self) -> Result<Option<Metrics>, String> {
        Ok(self.resized.take())
    }

    fn start_helper(&mut self, id: u64, job: Helper) -> Result<Started, String> {
        if self.conn.is_none() {
            return Ok(Started::Failed("the terminal is not attached".to_string()));
        }
        let in_terminal = job.in_terminal();
        self.waiting.push(id);
        self.send(&ToFrontend::Helper {
            id,
            job: job_for(job),
        });
        // From here the frontend is running it and painting nothing, until
        // it says the helper is done.
        if in_terminal {
            self.in_terminal.push(id);
            self.frames.pause();
        }
        Ok(Started::Running)
    }

    fn helper_fds(&self) -> Vec<RawFd> {
        Vec::new()
    }

    fn poll_helper(&mut self, id: u64, _ready: &[RawFd]) -> Option<HelperOutcome> {
        let outcome = self.answers.remove(&id)?;
        self.waiting.retain(|waiting| *waiting != id);
        Some(outcome)
    }

    /// The window has stopped wanting it, which does not give the terminal
    /// back: a helper that has the terminal runs to its end in the
    /// frontend whatever the window wants, and frames sent before then
    /// would only be late again. Its id stays in `in_terminal` until the
    /// frontend's [`ToBackend::HelperDone`].
    fn end_helper(&mut self, id: u64) {
        self.waiting.retain(|waiting| *waiting != id);
        self.answers.remove(&id);
    }
}

/// What a frontend is asked to run, for a helper the window decided on.
pub(crate) fn job_for(helper: Helper) -> Job {
    match helper {
        Helper::Picker {
            tab,
            chooser,
            command,
            kind,
            dir,
        } => Job::Picker {
            tab,
            node: chooser.backend_node_id,
            session: chooser.session,
            command,
            terminal: kind == picker::Kind::Terminal,
            dir,
            multiple: chooser.multiple,
        },
        Helper::Login {
            tab,
            url,
            site,
            command,
            kind,
            dir,
        } => Job::Login {
            tab,
            url,
            command,
            terminal: kind == picker::Kind::Terminal,
            site,
            dir,
        },
        Helper::External {
            url, configured, ..
        } => Job::External {
            command: configured,
            url,
        },
    }
}

/// What a frontend's answer is to the window: a picker's files, a password
/// command's login — read here from what it printed, which is then dropped
/// and overwritten — or, for a desktop browser that would not start, the
/// sentence for the row (`Err`).
pub(crate) fn outcome_for(outcome: Outcome) -> Result<Option<HelperOutcome>, String> {
    Ok(Some(match outcome {
        Outcome::Picker { result, .. } => HelperOutcome::Picker(match result {
            Picked::Files(files) => picker::Outcome::Files(files),
            Picked::Cancel => picker::Outcome::Cancel,
            Picked::Failed(why) => picker::Outcome::Failed(why),
        }),
        Outcome::Login { result, .. } => HelperOutcome::Login(match result {
            ipc::Fetched::Found(secret) => match login::parse_output(secret.bytes()) {
                Ok(found) => login::Outcome::Found(found),
                Err(why) => login::Outcome::Failed(why),
            },
            ipc::Fetched::None => login::Outcome::None,
            ipc::Fetched::Failed(why) => login::Outcome::Failed(why),
        }),
        Outcome::External(Ok(())) => return Ok(None),
        Outcome::External(Err(why)) => return Err(why),
    }))
}

// ---------------------------------------------------------------------------
// Sentences.
// ---------------------------------------------------------------------------

/// The refusal for a frontend of another version.
pub fn version_sentence(dir: &str, pid: u32, theirs: &str, ours: &str) -> String {
    format!(
        "the blinkterm serving {dir} (pid {pid}) is version {theirs} and this is {ours}; \
         quit its windows before starting another on this profile"
    )
}

/// The refusal for a frontend whose browser-wide settings differ from the
/// backend's: the first key that differs, named.
pub fn settings_sentence(
    dir: &str,
    pid: u32,
    ours: &BrowserSettings,
    asked: &BrowserSettings,
) -> Option<String> {
    let (key, theirs, wanted) = ours.differences(asked).into_iter().next()?;
    Some(format!(
        "the blinkterm serving {dir} (pid {pid}) runs the engine with {key} = {theirs}, \
         and this start asked for {wanted}; browser-wide settings are shared by every window \
         on a profile: quit its windows first, or use --temp-profile or --profile <dir>"
    ))
}

/// The answer to a frontend that comes while the backend is stopping.
pub fn shutting_down_sentence(dir: &str, secs: u64) -> String {
    format!(
        "the blinkterm serving {dir} has been shutting down for {secs} s; try again in a moment"
    )
}

/// What a window's frontend is told when the backend stops under it.
pub fn stopped_sentence(dir: &str, why: &str) -> String {
    format!("the blinkterm serving {dir} stopped: {why}")
}

// ---------------------------------------------------------------------------
// Routing.
// ---------------------------------------------------------------------------

/// Which window a held page goes to, by the dispositions seen: exactly one
/// window asked for `url`, two or more did, or none did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Match {
    One(WindowId),
    Ambiguous,
    Nobody,
}

/// See [`Match`]. `dispositions` are (window, url, when).
pub(crate) fn match_disposition(url: &str, dispositions: &[(WindowId, String, Instant)]) -> Match {
    let mut found: Option<WindowId> = None;
    for (window, wanted, _) in dispositions {
        if wanted != url {
            continue;
        }
        match found {
            None => found = Some(*window),
            Some(other) if other != *window => return Match::Ambiguous,
            Some(_) => {}
        }
    }
    found.map_or(Match::Nobody, Match::One)
}

/// Whether a target's url says where it is going yet: the headless shell
/// announces a clicked link's page with none, and gives it later.
fn says_where(url: &str) -> bool {
    !url.is_empty() && url != "about:blank"
}

/// A page target no window has been shown to have asked for yet.
struct Held {
    target: String,
    /// Its announcement, and its latest change of url, to hand to the
    /// window it turns out to be for.
    created: Event,
    renamed: Option<Event>,
    url: String,
    since: Instant,
}

// ---------------------------------------------------------------------------
// The backend.
// ---------------------------------------------------------------------------

/// Whether the backend is taking windows, or on its way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    Serving,
    ShuttingDown { since: Instant },
}

/// The windows whose frontend has been gone longer than `grace`, by index:
/// `suspended` is when each window's frontend went, `None` for one that is
/// attached.
pub(crate) fn expired(suspended: &[Option<Instant>], grace: Duration, now: Instant) -> Vec<usize> {
    suspended
        .iter()
        .enumerate()
        .filter(|(_, since)| since.is_some_and(|at| now.saturating_duration_since(at) >= grace))
        .map(|(index, _)| index)
        .collect()
}

/// Whether a backend with `windows` windows and `attaching` connections that
/// have not opened one yet has nothing left to serve.
pub(crate) fn idle(windows: usize, attaching: usize) -> bool {
    windows == 0 && attaching == 0
}

/// One window and the terminal it is drawn on.
struct Slot {
    win: Window,
    term: RemoteTerminal,
    nonce: String,
    /// When its frontend went, while it waits for one to come back.
    suspended_since: Option<Instant>,
    /// When its terminal last sent input: `--remote` goes to the window used
    /// last.
    last_input: Instant,
    /// The browser's news routed to it after its pass this time round, for
    /// its next.
    routed: Vec<Event>,
}

/// A connection that has not opened a window yet.
struct Attaching {
    conn: Conn,
    hello: bool,
    since: Instant,
}

/// Why a window is going.
enum Going {
    /// Its frontend asked: `ctrl+q`, a hang-up; or its last tab closed. The
    /// group is closed.
    Quit,
    /// Something went wrong for it alone; its frontend is told why and its
    /// group is lost, to be offered again.
    Failed(String),
}

struct Backend {
    shared: Shared,
    /// `None` only between a relaunch that failed and the stop it leads to.
    live: Option<Live>,
    /// The engine's first page until a window adopts it.
    spare: Option<Tabs<Client>>,
    windows: Vec<Slot>,
    attaching: Vec<Attaching>,
    endpoint: Option<ipc::Endpoint>,
    state: Lifecycle,
    next_window: u64,
    grace: Duration,
    /// The profile's directory, canonical; `None` for a temporary one.
    dir: Option<PathBuf>,
    /// The profile's directory as frontends are told it.
    shown: String,
    label: String,
    generation: u64,
    browser_settings: BrowserSettings,
    /// What the engine's first page is told when no window says.
    appearance: Appearance,
    /// What the first window's row says about the start, once.
    startup: Option<(Vec<String>, Option<String>)>,
    held: Vec<Held>,
    dispositions: Vec<(WindowId, String, Instant)>,
    last_check: Instant,
    buf: Vec<u8>,
}

/// Run as a profile's backend, with the frontend that started this process
/// at descriptor `first`: take the profile — or tell that frontend who has
/// it — make sure no engine of a previous backend is still running on it,
/// listen, start the engine, say ready, and serve until there is nothing
/// left to serve.
///
/// Every way a start can fail before ready is told to the frontend as
/// `failed` with the sentence, which it prints; the backend then exits 1.
pub fn serve(options: Options, first: RawFd) -> Result<(), String> {
    // SAFETY: descriptor `first` was put in place by the frontend that
    // started this process, before `exec`, for this process alone; nothing
    // else in it owns or closes it. It is taken over once, here.
    let given = unsafe { OwnedFd::from_raw_fd(first) };
    // Moved off its fixed number, close-on-exec, so that no child — the
    // engine above all, whose own 3 and 4 are its pipe — inherits it.
    let moved = given
        .try_clone()
        .map_err(|e| format!("cannot take descriptor {first}: {e}"))?;
    drop(given);
    let mut pair = UnixStream::from(moved);
    app::install_signals(app::Role::Backend);
    std::panic::set_hook(Box::new(|info| {
        // A release build aborts here, so this is the only chance to stop
        // the engine. There is no terminal to put back.
        engine::kill_engine();
        profile::remove_temp_profile();
        eprintln!("blinkterm: {info}");
    }));
    let started = start(&options, &mut pair);
    let (profile_up, endpoint) = match started {
        Ok(Some(up)) => up,
        Ok(None) => return Ok(()),
        Err(why) => {
            let _ = pair.write_all(&ipc::encode(&ToFrontend::Failed { why: why.clone() }));
            return Err(why);
        }
    };
    let app::ProfileUp {
        shared,
        live,
        first: spare,
        problems,
        copied,
    } = profile_up;
    let dir =
        (!live.engine.profile().is_temporary()).then(|| live.engine.profile().dir().to_path_buf());
    let shown = live.engine.profile().dir().display().to_string();
    if let Some(dir) = &dir {
        engine::write_pgid_marker(dir, &live.engine);
    }
    let ready = ToFrontend::Ready {
        dir: live.engine.profile().dir().to_path_buf(),
        pid: std::process::id(),
    };
    if pair.write_all(&ipc::encode(&ready)).is_err() {
        // The frontend that started this went while it started: nobody is
        // there to serve, and nobody waiting either.
        eprintln!("blinkterm: the frontend that started this backend went before it was ready");
    }
    let backend = Backend {
        shared,
        live: Some(live),
        spare: Some(spare),
        windows: Vec::new(),
        attaching: vec![Attaching {
            conn: Conn::new(pair),
            hello: false,
            since: Instant::now(),
        }],
        endpoint,
        state: Lifecycle::Serving,
        next_window: 1,
        grace: options.grace.unwrap_or(GRACE),
        dir,
        shown,
        label: options.profile_label.clone().unwrap_or_default(),
        generation: generation(),
        browser_settings: BrowserSettings::from(&options),
        appearance: Appearance::new(options.scheme, options.force_dark, options.alpha),
        startup: Some((problems, copied)),
        held: Vec::new(),
        dispositions: Vec::new(),
        last_check: Instant::now(),
        buf: vec![0u8; READ_CHUNK],
    };
    backend.run()
}

/// The start up to ready: the profile, the old engine reaped, the backend
/// socket, the engine. `Ok(None)` is a profile somebody else holds, which
/// the frontend has been told.
fn start(
    options: &Options,
    pair: &mut UnixStream,
) -> Result<Option<(app::ProfileUp, Option<ipc::Endpoint>)>, String> {
    let profile = match &options.profile {
        Choice::Temporary => Profile::temporary()?,
        Choice::At(dir) => {
            match Profile::try_take_at(dir.clone(), options.profile_label.clone())? {
                TakeAt::Taken(profile) => profile,
                TakeAt::Held(pid) => {
                    let _ = pair.write_all(&ipc::encode(&ToFrontend::Busy {
                        pid: pid.unwrap_or(0),
                    }));
                    return Ok(None);
                }
            }
        }
        _ => return Err("a backend is started on a directory or a temporary profile".to_string()),
    };
    let endpoint = if profile.is_temporary() {
        None
    } else {
        engine::reap_orphan(profile.dir())?;
        Some(ipc::bind(profile.dir())?)
    };
    let up = app::start_profile(options, profile)?;
    Ok(Some((up, endpoint)))
}

/// A number for this backend's run, different from the last one's: the
/// clock in microseconds, which a JSON number carries exactly.
fn generation() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or_default()
        & ((1 << 53) - 1)
}

impl Backend {
    fn run(mut self) -> Result<(), String> {
        loop {
            if app::quit_requested() {
                return self.shutdown(None);
            }
            let now = Instant::now();
            self.expire_grace(now);
            self.attaching
                .retain(|a| now.saturating_duration_since(a.since) < HANDSHAKE_TIMEOUT);
            if idle(self.windows.len(), self.attaching.len()) {
                return self.shutdown(None);
            }
            if let Some(why) = self.prepare() {
                if let Err(why) = self.relaunch(why) {
                    return self.shutdown(Some(why));
                }
                continue;
            }
            let ready = self.poll()?;
            self.accept();
            self.read_attaching(&ready);
            if let Some(why) = self.read_windows(&ready) {
                if let Err(why) = self.relaunch(why) {
                    return self.shutdown(Some(why));
                }
                continue;
            }
            if let Some(why) = self.pass(&ready) {
                if let Err(why) = self.relaunch(why) {
                    return self.shutdown(Some(why));
                }
                continue;
            }
            self.flush();
        }
    }

    /// Every window's half before the poll; the engine asked after when no
    /// window does it. `Some` is the engine's death.
    fn prepare(&mut self) -> Option<String> {
        let Some(live) = self.live.as_mut() else {
            return Some("the engine is not running".to_string());
        };
        if self.windows.is_empty() && self.last_check.elapsed() > CHECK_EVERY {
            self.last_check = Instant::now();
            if let Err(why) = live.engine.check() {
                return Some(why);
            }
        }
        let mut going = Vec::new();
        for (index, slot) in self.windows.iter_mut().enumerate() {
            let pass = app::prepare_window(
                &mut slot.term,
                &mut slot.win,
                &mut self.shared,
                &mut live.browser,
                &mut live.engine,
                &mut self.last_check,
            );
            match pass {
                Ok(Pass::Continue) => {}
                Ok(Pass::Quit) => going.push((index, Going::Quit)),
                Ok(Pass::EngineDied(why)) => return Some(why),
                Err(why) => match app::ended_by_engine(why, &mut live.engine, &live.browser) {
                    Ok(death) => return Some(death),
                    Err(why) => going.push((index, Going::Failed(why))),
                },
            }
        }
        self.close_windows(going);
        None
    }

    /// Wait for anything to happen: a frontend, the browser, a page in
    /// front, `--remote`, a connection.
    fn poll(&mut self) -> Result<Vec<RawFd>, String> {
        let mut fds: Vec<RawFd> = Vec::new();
        let mut busy = false;
        if let Some(endpoint) = &self.endpoint {
            fds.push(endpoint.fd());
        }
        if let Some(remote) = &self.shared.remote {
            fds.push(remote.fd());
        }
        if let Some(live) = &self.live {
            fds.push(live.browser.wake_fd());
        }
        for attaching in &self.attaching {
            fds.push(attaching.conn.fd());
            busy |= !attaching.conn.outbox.is_empty();
        }
        for slot in &self.windows {
            fds.extend(app::window_fds(&slot.win, &slot.term));
            if let Some(conn) = &slot.term.conn {
                fds.push(conn.fd());
                busy |= !conn.outbox.is_empty();
            }
            busy |= app::poll_wait(&slot.win) < POLL_MS || !slot.routed.is_empty();
        }
        busy |= !self.held.is_empty();
        let wait = if busy { BUSY_POLL_MS } else { POLL_MS };
        tty::poll_readable(&fds, wait).map_err(|e| format!("cannot wait for input: {e}"))
    }

    /// Every connection waiting on the backend socket, taken.
    fn accept(&mut self) {
        let Some(endpoint) = &self.endpoint else {
            return;
        };
        loop {
            match ipc::accept(endpoint) {
                Ok(Some(stream)) => self.attaching.push(Attaching {
                    conn: Conn::new(stream),
                    hello: false,
                    since: Instant::now(),
                }),
                Ok(None) => break,
                Err(why) => {
                    eprintln!("blinkterm: stopped taking windows: {why}");
                    self.endpoint = None;
                    break;
                }
            }
        }
    }

    /// What the connections that have not opened a window say: `hello`,
    /// then `open`.
    fn read_attaching(&mut self, ready: &[RawFd]) {
        let mut index = 0;
        while index < self.attaching.len() {
            if !ready.contains(&self.attaching[index].conn.fd()) {
                index += 1;
                continue;
            }
            let messages = self.attaching[index].conn.read(&mut self.buf);
            let mut opened = None;
            for message in messages {
                let attaching = &mut self.attaching[index];
                match message {
                    ToBackend::Hello {
                        protocol,
                        version,
                        dir,
                    } => {
                        let answer = self.welcome(protocol, &version, dir.as_deref());
                        let attaching = &mut self.attaching[index];
                        attaching.hello = matches!(answer, ToFrontend::Welcome { .. });
                        attaching.conn.send(&answer);
                    }
                    ToBackend::Open(open) if attaching.hello => {
                        opened = Some(open);
                        break;
                    }
                    ToBackend::Ping => attaching.conn.send(&ToFrontend::Pong),
                    _ => {
                        attaching.conn.broken = Some("not a hello".to_string());
                    }
                }
            }
            if let Some(open) = opened {
                let attaching = self.attaching.remove(index);
                self.open(attaching.conn, *open);
                continue;
            }
            index += 1;
        }
    }

    /// The answer to a `hello`.
    fn welcome(&self, protocol: u32, version: &str, dir: Option<&Path>) -> ToFrontend {
        let pid = std::process::id();
        if protocol != ipc::PROTOCOL || version != ipc::VERSION {
            return ToFrontend::Refused {
                why: version_sentence(&self.shown, pid, ipc::VERSION, version),
                retry: false,
            };
        }
        let same = match (dir, &self.dir) {
            (None, None) => true,
            (Some(asked), Some(ours)) => {
                std::fs::canonicalize(asked).ok().as_deref()
                    == std::fs::canonicalize(ours).ok().as_deref()
            }
            _ => false,
        };
        if !same {
            return ToFrontend::Refused {
                why: format!(
                    "this is the blinkterm serving {}, not {}",
                    self.shown,
                    dir.map_or_else(
                        || "a temporary profile".to_string(),
                        |d| d.display().to_string()
                    )
                ),
                retry: false,
            };
        }
        if let Lifecycle::ShuttingDown { since } = self.state {
            return ToFrontend::Refused {
                why: shutting_down_sentence(&self.shown, since.elapsed().as_secs()),
                retry: true,
            };
        }
        ToFrontend::Welcome {
            protocol: ipc::PROTOCOL,
            version: ipc::VERSION.to_string(),
            pid,
            generation: self.generation,
            dir: self
                .dir
                .clone()
                .unwrap_or_else(|| PathBuf::from(&self.shown)),
            label: self.label.clone(),
        }
    }

    /// An `open`: the window its nonce names taken back, or a new one.
    fn open(&mut self, mut conn: Conn, open: ipc::Open) {
        let pid = std::process::id();
        if let Some(why) =
            settings_sentence(&self.shown, pid, &self.browser_settings, &open.browser)
        {
            conn.send(&ToFrontend::Refused { why, retry: false });
            conn.drain(Duration::from_millis(200));
            return;
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if let Some(slot) = self
            .windows
            .iter_mut()
            .find(|slot| slot.nonce == open.nonce)
        {
            if slot.term.linked() {
                conn.send(&ToFrontend::Refused {
                    why: "that window is open in another terminal".to_string(),
                    retry: false,
                });
                conn.drain(Duration::from_millis(200));
                return;
            }
            let id = app::window_id(&slot.win);
            conn.send(&ToFrontend::Opened {
                window: id.0,
                resumed: true,
            });
            slot.term.attach(conn, open.route);
            slot.suspended_since = None;
            slot.last_input = Instant::now();
            if let Err(why) = app::resume_window(
                &mut slot.term,
                &mut slot.win,
                &mut self.shared,
                open.metrics,
            ) {
                eprintln!("blinkterm: resuming window {}: {why}", id.0);
            }
            return;
        }
        let id = WindowId(self.next_window);
        self.next_window += 1;
        let first = match self.spare.take() {
            Some(tabs) => Ok(tabs),
            None => app::window_target(&mut live.browser, &self.shared, &self.appearance),
        };
        let first = match first {
            Ok(first) => first,
            Err(why) => {
                conn.send(&ToFrontend::Refused {
                    why: format!("the engine would not open a window: {why}"),
                    retry: false,
                });
                conn.drain(Duration::from_millis(200));
                return;
            }
        };
        conn.send(&ToFrontend::Opened {
            window: id.0,
            resumed: false,
        });
        let win = app::new_window(id, first, open.metrics, &open, &self.shared);
        let mut slot = Slot {
            win,
            term: RemoteTerminal::new(conn, open.route),
            nonce: open.nonce.clone(),
            suspended_since: None,
            last_input: Instant::now(),
            routed: Vec::new(),
        };
        let (mut problems, copied) = match self.startup.take() {
            Some((problems, copied)) => (problems, copied),
            None => (Vec::new(), None),
        };
        problems.extend(open.problems.iter().cloned());
        let opened = app::open_window(
            &mut slot.term,
            &mut slot.win,
            &mut live.browser,
            &mut self.shared,
            &open,
            &problems,
            copied,
        );
        self.windows.push(slot);
        if let Err(why) = opened {
            eprintln!("blinkterm: opening window {}: {why}", id.0);
            let index = self.windows.len() - 1;
            if let Some(live) = self.live.as_mut() {
                if app::ended_by_engine(why.clone(), &mut live.engine, &live.browser).is_err() {
                    self.close_windows(vec![(index, Going::Failed(why))]);
                }
            }
        }
    }

    /// What every attached frontend said. `Some` is the engine's death.
    fn read_windows(&mut self, ready: &[RawFd]) -> Option<String> {
        let mut going = Vec::new();
        let mut death = None;
        for index in 0..self.windows.len() {
            let Some(fd) = self.windows[index].term.conn.as_ref().map(Conn::fd) else {
                continue;
            };
            if !ready.contains(&fd) {
                continue;
            }
            let messages = match self.windows[index].term.conn.as_mut() {
                Some(conn) => conn.read(&mut self.buf),
                None => continue,
            };
            for message in messages {
                let live = self.live.as_mut()?;
                let slot = &mut self.windows[index];
                match message {
                    ToBackend::Input { input, pixel_mouse } => {
                        slot.last_input = Instant::now();
                        let handled = app::window_input(
                            &mut slot.term,
                            &mut slot.win,
                            &mut self.shared,
                            &mut live.browser,
                            input,
                            pixel_mouse,
                        );
                        match handled {
                            Ok(true) => {}
                            Ok(false) => {
                                going.push((index, Going::Quit));
                                break;
                            }
                            Err(why) => {
                                match app::ended_by_engine(why, &mut live.engine, &live.browser) {
                                    Ok(dead) => {
                                        death = Some(dead);
                                        break;
                                    }
                                    Err(why) => {
                                        going.push((index, Going::Failed(why)));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    ToBackend::Resize { metrics, .. } => slot.term.resized = Some(metrics),
                    ToBackend::Painted { seq, waited_ms, .. } => {
                        slot.term
                            .painted_up_to(seq, waited_ms.map(Duration::from_millis));
                    }
                    ToBackend::HelperDone { id, outcome } => {
                        slot.term.terminal_back(id);
                        match outcome_for(outcome) {
                            Ok(Some(outcome)) => slot.term.answer(id, outcome),
                            Ok(None) => {}
                            Err(why) => {
                                let _ = app::window_note(
                                    &mut slot.term,
                                    &mut slot.win,
                                    &self.shared,
                                    why,
                                );
                            }
                        }
                    }
                    ToBackend::Close { why } => {
                        let _ = why == CloseWhy::Quit;
                        going.push((index, Going::Quit));
                        break;
                    }
                    ToBackend::Ping => slot.term.send(&ToFrontend::Pong),
                    ToBackend::Hello { .. } | ToBackend::Open(_) => {
                        if let Some(conn) = slot.term.conn.as_mut() {
                            conn.broken = Some("a second hello".to_string());
                        }
                    }
                }
            }
            if death.is_some() {
                break;
            }
        }
        self.close_windows(going);
        death
    }

    /// The browser's news routed, `--remote` delivered, and every window's
    /// half after the poll. `Some` is the engine's death.
    fn pass(&mut self, ready: &[RawFd]) -> Option<String> {
        let now = Instant::now();
        self.pump_remote(ready);
        let events = {
            let live = self.live.as_mut()?;
            if ready.contains(&live.browser.wake_fd()) {
                live.browser.drain_wake();
            }
            live.browser.events()
        };
        let mut routed: Vec<Vec<Event>> = self
            .windows
            .iter_mut()
            .map(|slot| std::mem::take(&mut slot.routed))
            .collect();
        let mut downloads_moved = false;
        for event in events {
            // A download's news comes on this connection and is nobody's
            // tab's: the profile's, and every window's row says it.
            if self.shared.downloads.take(&event, now) {
                downloads_moved = true;
            }
            self.route(event, &mut routed, now);
        }
        let live = self.live.as_mut()?;
        let mut going = Vec::new();
        let mut death = None;
        for (index, (slot, events)) in self.windows.iter_mut().zip(routed).enumerate() {
            let pass = app::pass_window(
                &mut slot.term,
                &mut slot.win,
                &mut self.shared,
                &mut live.browser,
                ready,
                events,
                downloads_moved,
            );
            match pass {
                Ok(Pass::Continue) => {}
                Ok(Pass::Quit) => going.push((index, Going::Quit)),
                Ok(Pass::EngineDied(why)) => {
                    death = Some(why);
                    break;
                }
                Err(why) => match app::ended_by_engine(why, &mut live.engine, &live.browser) {
                    Ok(dead) => {
                        death = Some(dead);
                        break;
                    }
                    Err(why) => going.push((index, Going::Failed(why))),
                },
            }
            if !slot.term.linked() {
                continue;
            }
            // After the window's half, which is where a picker is started
            // (and a password command, from the input read before it): a
            // helper given the terminal holds the cast from the pass it
            // starts, and the pass its answer comes lays the window out
            // again — at the size the frontend said just before it, taken
            // here so that the next pass does not lay it out a second time.
            match slot.term.hold() {
                Some(true) => app::hold_cast(&mut slot.win, true),
                Some(false) => {
                    let metrics = slot
                        .term
                        .resized
                        .take()
                        .unwrap_or_else(|| app::window_metrics(&slot.win));
                    let resumed = app::resume_window(
                        &mut slot.term,
                        &mut slot.win,
                        &mut self.shared,
                        metrics,
                    );
                    if let Err(why) = resumed {
                        match app::ended_by_engine(why, &mut live.engine, &live.browser) {
                            Ok(dead) => {
                                death = Some(dead);
                                break;
                            }
                            Err(why) => going.push((index, Going::Failed(why))),
                        }
                    }
                }
                None => {
                    if let Some(held) = slot.term.frames.stall(now) {
                        app::hold_cast(&mut slot.win, held);
                    }
                }
            }
        }
        if death.is_some() {
            return death;
        }
        self.close_windows(going);
        self.settle_held(now);
        None
    }

    /// One event from the browser's connection, to the window it is about —
    /// now into `routed`, or held. See the module's section on routing.
    fn route(&mut self, event: Event, routed: &mut [Vec<Event>], now: Instant) {
        let holder = |windows: &[Slot], target: &str| {
            windows
                .iter()
                .position(|slot| app::window_holds(&slot.win, target))
        };
        let Some(change) = tabs::change(&event) else {
            return;
        };
        match change {
            Change::Opened {
                target,
                opener,
                url,
                ..
            } => {
                if let Some(index) = holder(&self.windows, &target) {
                    routed[index].push(event);
                    return;
                }
                if self
                    .spare
                    .as_ref()
                    .is_some_and(|spare| spare.index_of(&target).is_some())
                {
                    return;
                }
                if let Some(index) = opener
                    .as_deref()
                    .and_then(|opener| holder(&self.windows, opener))
                {
                    routed[index].push(event);
                    return;
                }
                if self.windows.len() == 1 {
                    routed[0].push(event);
                    return;
                }
                self.held.push(Held {
                    target,
                    created: event,
                    renamed: None,
                    url,
                    since: now,
                });
            }
            Change::Renamed { target, url } => {
                if let Some(index) = holder(&self.windows, &target) {
                    routed[index].push(event);
                } else if let Some(held) = self.held.iter_mut().find(|held| held.target == target) {
                    held.url = url;
                    held.renamed = Some(event);
                }
            }
            Change::Closed { target } | Change::Crashed { target } => {
                if let Some(index) = holder(&self.windows, &target) {
                    routed[index].push(event);
                } else {
                    self.held.retain(|held| held.target != target);
                }
            }
        }
    }

    /// The held pages given to the window that asked for each, closed when
    /// two did, and closed when nobody has after [`HELD_FOR`].
    fn settle_held(&mut self, now: Instant) {
        for slot in &mut self.windows {
            let id = app::window_id(&slot.win);
            for (url, at) in app::take_dispositions(&mut slot.win, now) {
                self.dispositions.push((id, url, at));
            }
        }
        self.dispositions
            .retain(|(_, _, at)| now.saturating_duration_since(*at) < HELD_FOR);
        if self.held.is_empty() {
            return;
        }
        let mut keep = Vec::new();
        for held in std::mem::take(&mut self.held) {
            let decided = if says_where(&held.url) {
                match_disposition(&held.url, &self.dispositions)
            } else {
                Match::Nobody
            };
            match decided {
                Match::One(window) => {
                    let url = held.url.clone();
                    self.dispositions.retain(|(_, wanted, _)| *wanted != url);
                    if let Some(slot) = self
                        .windows
                        .iter_mut()
                        .find(|slot| app::window_id(&slot.win) == window)
                    {
                        slot.routed.push(held.created);
                        slot.routed.extend(held.renamed);
                    }
                }
                Match::Ambiguous => self.close_target(&held.target, "two windows asked for it"),
                Match::Nobody if now.saturating_duration_since(held.since) >= HELD_FOR => {
                    self.close_target(&held.target, "no window asked for it");
                }
                Match::Nobody => keep.push(held),
            }
        }
        self.held = keep;
    }

    /// A page no window can take, closed, and said in the log.
    fn close_target(&mut self, target: &str, why: &str) {
        eprintln!("blinkterm: closed a page the engine opened ({target}): {why}");
        if let Some(live) = self.live.as_mut() {
            let _ = live.browser.notify(
                "Target.closeTarget",
                Json::object(vec![("targetId", Json::string(target))]),
            );
        }
    }

    /// `blinkterm --remote` senders: their urls opened in the window used
    /// last, or — with no window attached — `nowhere`, so that the sender
    /// starts a terminal of its own.
    fn pump_remote(&mut self, ready: &[RawFd]) {
        let Some(listener) = self.shared.remote.as_mut() else {
            return;
        };
        if !ready.contains(&listener.fd()) {
            return;
        }
        let deliveries = match listener.accept_ready() {
            Ok(deliveries) => deliveries,
            Err(why) => {
                eprintln!("blinkterm: stopped listening for --remote: {why}");
                self.shared.remote = None;
                return;
            }
        };
        if deliveries.is_empty() {
            return;
        }
        let target = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.term.linked())
            .max_by_key(|(_, slot)| slot.last_input)
            .map(|(index, _)| index);
        let (Some(index), Some(live)) = (target, self.live.as_mut()) else {
            for delivery in deliveries {
                delivery.nowhere();
            }
            return;
        };
        let slot = &mut self.windows[index];
        if let Err(why) = app::deliver_remote(
            &mut slot.term,
            &mut slot.win,
            &mut self.shared,
            &mut live.browser,
            deliveries,
        ) {
            eprintln!("blinkterm: --remote: {why}");
        }
    }

    /// Every outbox written as far as its socket takes it; a frontend that
    /// is gone, or stopped reading, suspends its window.
    fn flush(&mut self) {
        let now = Instant::now();
        for attaching in &mut self.attaching {
            attaching.conn.flush();
        }
        self.attaching.retain(|a| a.conn.broken.is_none());
        for slot in &mut self.windows {
            let Some(conn) = slot.term.conn.as_mut() else {
                continue;
            };
            conn.flush();
            if let Some(why) = conn.broken.clone() {
                let id = app::window_id(&slot.win);
                eprintln!("blinkterm: window {} lost its terminal: {why}", id.0);
                drop(slot.term.detach());
                slot.suspended_since = Some(now);
                app::suspend_window(&mut slot.term, &mut slot.win);
            }
        }
    }

    /// Windows whose frontend has been gone [`GRACE`]: their tabs a lost
    /// group in the session, offered to the next window, and gone.
    fn expire_grace(&mut self, now: Instant) {
        let suspended: Vec<Option<Instant>> = self
            .windows
            .iter()
            .map(|slot| slot.suspended_since)
            .collect();
        let mut gone = expired(&suspended, self.grace, now);
        gone.reverse();
        for index in gone {
            let mut slot = self.windows.remove(index);
            let id = app::window_id(&slot.win);
            eprintln!(
                "blinkterm: window {} was not taken back; its tabs are saved",
                id.0
            );
            self.shared.session.window_lost(id);
            if let Some(live) = self.live.as_mut() {
                app::close_window(&mut slot.term, &mut slot.win, &mut live.browser);
            }
        }
    }

    /// The windows at `going`, each told why and gone: a quit closes its
    /// group, a failure leaves it lost.
    fn close_windows(&mut self, mut going: Vec<(usize, Going)>) {
        going.sort_by_key(|(index, _)| std::cmp::Reverse(*index));
        going.dedup_by_key(|(index, _)| *index);
        for (index, why) in going {
            if index >= self.windows.len() {
                continue;
            }
            let mut slot = self.windows.remove(index);
            let id = app::window_id(&slot.win);
            let (sentence, exit) = match why {
                Going::Quit => {
                    self.shared.session.window_closed(id);
                    (String::new(), 0)
                }
                Going::Failed(why) => {
                    self.shared.session.window_lost(id);
                    (app::died(why, &self.shared.session, id), 1)
                }
            };
            if let Some(live) = self.live.as_mut() {
                app::close_window(&mut slot.term, &mut slot.win, &mut live.browser);
            }
            if let Some(mut conn) = slot.term.detach() {
                conn.send(&ToFrontend::Closed {
                    why: sentence,
                    exit,
                });
                conn.drain(Duration::from_millis(500));
            }
        }
    }

    /// The engine died: started again under every window, or — the second
    /// death in a minute, or an engine that will not start — `Err`, the
    /// reason the backend stops with.
    fn relaunch(&mut self, why: String) -> Result<(), String> {
        let now = Instant::now();
        if !self.shared.relaunches.allows(now) {
            return Err(app::gave_up(why));
        }
        let Some(dead) = self.live.take() else {
            return Err(why);
        };
        eprintln!("blinkterm: {why}; starting it again");
        self.held.clear();
        self.dispositions.clear();
        self.spare = None;
        let mut pairs: Vec<(&mut dyn Terminal, &mut Window)> = self
            .windows
            .iter_mut()
            .map(|slot| {
                slot.routed.clear();
                (&mut slot.term as &mut dyn Terminal, &mut slot.win)
            })
            .collect();
        let (live, spare) =
            app::relaunch_all(&mut pairs, &mut self.shared, dead, why, &self.appearance)?;
        drop(pairs);
        if let Some(dir) = &self.dir {
            engine::write_pgid_marker(dir, &live.engine);
        }
        self.live = Some(live);
        self.spare = spare;
        self.shared.relaunches.relaunched(Instant::now());
        self.flush();
        Ok(())
    }

    /// Stop: no more windows taken, every window still attached told why
    /// (`why` is `None` for the last window gone, or a `SIGTERM`), the
    /// session written, the downloads cancelled, the engine asked to close
    /// and waited for, the sockets removed, and the profile let go last.
    fn shutdown(mut self, why: Option<String>) -> Result<(), String> {
        self.state = Lifecycle::ShuttingDown {
            since: Instant::now(),
        };
        for attaching in &mut self.attaching {
            attaching.conn.send(&ToFrontend::Refused {
                why: shutting_down_sentence(&self.shown, 0),
                retry: true,
            });
            attaching.conn.drain(Duration::from_millis(100));
        }
        self.attaching.clear();
        let clean = why.is_none();
        for slot in &mut self.windows {
            let id = app::window_id(&slot.win);
            let (sentence, exit) = match &why {
                None => (String::new(), 0),
                Some(why) => (
                    stopped_sentence(
                        &self.shown,
                        &app::died(why.clone(), &self.shared.session, id),
                    ),
                    1,
                ),
            };
            if let Some(mut conn) = slot.term.detach() {
                conn.send(&ToFrontend::Closed {
                    why: sentence,
                    exit,
                });
                conn.drain(Duration::from_millis(500));
            }
        }
        // A quit says the session is closed; anything else leaves it open,
        // with what was still waiting to be written, so that the next start
        // offers it back. See [`crate::session`].
        self.shared.session.finish(clean);
        let windows = std::mem::take(&mut self.windows);
        let spare = self.spare.take();
        if let Some(Live {
            mut engine,
            mut browser,
        }) = self.live.take()
        {
            // Whatever is still coming is cancelled, whichever way the engine
            // is about to stop: `Browser.close` would cancel it too, but the
            // temporary profile's way out is a kill, and a kill leaves the
            // engine's partial file behind. Cancelled, the engine removes it.
            self.shared.downloads.cancel_all(&mut browser);
            // Dropping the tabs closes every page's session, which is all a
            // tab is once the engine is about to be asked to close.
            drop(windows);
            drop(spare);
            if !engine.profile().is_temporary() {
                // `Browser.close` is the only stop that writes the cookie jar
                // — a `SIGTERM` loses it; `crate::profile` has the
                // measurements — and the writing happens after the reply, in
                // the two seconds before the process ends, so the engine is
                // waited for rather than just asked. A frontend that comes
                // meanwhile is told to wait.
                let _ = browser.notify("Browser.close", Json::empty());
                let deadline = Instant::now() + app::CLOSE_TIMEOUT;
                while !engine.wait_for_exit(Duration::from_millis(20)) {
                    self.refuse_while_closing();
                    if Instant::now() >= deadline {
                        break;
                    }
                }
            }
            browser.close();
            engine.kill();
            // The partial files of every download this backend saw begin and
            // not save, now that nothing can be writing them.
            for partial in self.shared.downloads.partials() {
                let _ = std::fs::remove_file(partial);
            }
            // The sockets and the marker before the lock: a frontend that
            // comes now finds nobody and starts a backend of its own, which
            // waits for the lock.
            self.shared.remote = None;
            self.endpoint = None;
            if let Some(dir) = &self.dir {
                engine::remove_pgid_marker(dir);
            }
            // And the profile, the lock with it, last.
            drop(engine);
        }
        self.shared.remote = None;
        self.endpoint = None;
        match why {
            None => Ok(()),
            Some(why) => Err(why),
        }
    }

    /// While the engine writes the profile on its way out: anybody who
    /// connects is told to try again shortly.
    fn refuse_while_closing(&mut self) {
        let Some(endpoint) = &self.endpoint else {
            return;
        };
        let since = match self.state {
            Lifecycle::ShuttingDown { since } => since,
            Lifecycle::Serving => Instant::now(),
        };
        while let Ok(Some(stream)) = ipc::accept(endpoint) {
            let mut conn = Conn::new(stream);
            conn.send(&ToFrontend::Refused {
                why: shutting_down_sentence(&self.shown, since.elapsed().as_secs()),
                retry: true,
            });
            conn.drain(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::Cells;

    fn frame() -> ipc::Frame {
        ipc::Frame {
            seq: 0,
            viewport_gen: 1,
            cells: Cells { cols: 2, rows: 2 },
            row: 2,
            kind: FrameKind::Jpeg,
            image: vec![1, 2, 3],
        }
    }

    #[test]
    fn at_most_two_frames_are_out_and_a_newer_one_replaces_the_one_waiting() {
        let now = Instant::now();
        let mut queue = FrameQueue::default();
        assert_eq!(queue.offer(frame(), now).map(|f| f.seq), Some(1));
        assert_eq!(queue.offer(frame(), now).map(|f| f.seq), Some(2));
        assert!(queue.offer(frame(), now).is_none(), "the third waits");
        assert!(
            queue.offer(frame(), now).is_none(),
            "the fourth replaces it"
        );
        assert!(!queue.all_painted());
        // One painted: the waiting one goes, and it is the newest.
        let next = queue.painted(1, Some(Duration::from_millis(7)), now);
        assert_eq!(next.map(|f| f.seq), Some(4));
        assert_eq!(queue.waited.take(), Some(Duration::from_millis(7)));
        assert!(queue.painted(4, None, now).is_none());
        assert!(queue.all_painted(), "everything up to 4 is painted");
    }

    #[test]
    fn a_terminal_that_stops_painting_stalls_its_window_until_it_catches_up() {
        let start = Instant::now();
        let mut queue = FrameQueue::default();
        queue.offer(frame(), start);
        assert_eq!(queue.stall(start + Duration::from_secs(1)), None);
        assert_eq!(
            queue.stall(start + STALL + Duration::from_millis(1)),
            Some(true)
        );
        assert_eq!(queue.stall(start + STALL * 2), None, "said once");
        queue.painted(1, None, start + STALL * 2);
        assert_eq!(queue.stall(start + STALL * 2), Some(false));
        assert_eq!(queue.stall(start + STALL * 3), None);
    }

    #[test]
    fn a_window_waits_its_grace_and_the_backend_stops_with_nothing_to_serve() {
        let start = Instant::now();
        let grace = Duration::from_millis(300);
        let suspended = [None, Some(start), Some(start + Duration::from_millis(200))];
        assert!(expired(&suspended, grace, start + Duration::from_millis(100)).is_empty());
        assert_eq!(expired(&suspended, grace, start + grace), vec![1]);
        assert_eq!(expired(&suspended, grace, start + grace * 2), vec![1, 2]);
        assert!(!idle(1, 0));
        assert!(!idle(0, 1), "somebody is attaching");
        assert!(idle(0, 0));
        let state = Lifecycle::ShuttingDown { since: start };
        assert_ne!(state, Lifecycle::Serving);
    }

    #[test]
    fn a_page_goes_to_the_one_window_that_asked_for_its_url_and_to_nobody_on_a_tie() {
        let now = Instant::now();
        let asked = vec![
            (WindowId(1), "https://a.example/".to_string(), now),
            (WindowId(2), "https://b.example/".to_string(), now),
            (WindowId(2), "https://b.example/".to_string(), now),
        ];
        assert_eq!(
            match_disposition("https://a.example/", &asked),
            Match::One(WindowId(1))
        );
        assert_eq!(
            match_disposition("https://b.example/", &asked),
            Match::One(WindowId(2))
        );
        assert_eq!(
            match_disposition("https://c.example/", &asked),
            Match::Nobody
        );
        let mut tie = asked.clone();
        tie.push((WindowId(1), "https://b.example/".to_string(), now));
        assert_eq!(
            match_disposition("https://b.example/", &tie),
            Match::Ambiguous
        );
        assert!(!says_where(""));
        assert!(!says_where("about:blank"));
        assert!(says_where("https://a.example/"));
    }

    #[test]
    fn the_refusals_say_what_differs_and_what_to_do() {
        let options = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            let cli = crate::options::parse_args(&args).expect("args");
            crate::options::resolve(cli, Default::default(), Default::default()).expect("options")
        };
        let ours = BrowserSettings::from(&options(&[]));
        let asked = BrowserSettings::from(&options(&["--proxy", "localhost:8080"]));
        let said = settings_sentence("/p", 42, &ours, &asked).expect("a difference");
        assert!(
            said.starts_with("the blinkterm serving /p (pid 42) runs the engine with proxy = "),
            "{said}"
        );
        assert!(said.contains("localhost:8080"), "{said}");
        assert!(said.ends_with(
            "browser-wide settings are shared by every window on a profile: quit its windows \
             first, or use --temp-profile or --profile <dir>"
        ));
        assert!(settings_sentence("/p", 42, &ours, &ours.clone()).is_none());
        assert_eq!(
            version_sentence("/p", 7, "0.5.0", "0.4.0"),
            "the blinkterm serving /p (pid 7) is version 0.5.0 and this is 0.4.0; quit its \
             windows before starting another on this profile"
        );
        assert_eq!(
            shutting_down_sentence("/p", 2),
            "the blinkterm serving /p has been shutting down for 2 s; try again in a moment"
        );
        assert_eq!(
            stopped_sentence("/p", "it died"),
            "the blinkterm serving /p stopped: it died"
        );
    }

    /// A [`RemoteTerminal`] says everything as a message, in order, and
    /// keeps a helper's answer until the window asks.
    #[test]
    fn a_remote_terminal_is_messages_on_the_socket() {
        let (ours, theirs) = UnixStream::pair().expect("a pair");
        let route = RouteFlags {
            paced: true,
            png: false,
            keyed: true,
            alpha: None,
            every_nth: 1,
        };
        let mut term = RemoteTerminal::new(Conn::new(ours), route);
        term.write(b"row").unwrap();
        term.clear_screen().unwrap();
        let cells = Cells { cols: 2, rows: 1 };
        for _ in 0..3 {
            term.frame(FrameOut {
                payload: Encoded::Jpeg(&[9, 9]),
                cells,
                row: 2,
                pixels: (16, 16),
                viewport_gen: 3,
            })
            .unwrap();
        }
        assert!(term.paced());
        assert!(!term.painted().all, "frames are out");
        let job = Helper::External {
            argv: vec!["x".to_string()],
            browser: None,
            home: None,
            url: "https://a.example/".to_string(),
            configured: None,
        };
        assert_eq!(term.start_helper(5, job).unwrap(), Started::Running);
        term.conn.as_mut().unwrap().drain(Duration::from_secs(1));
        theirs
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut decoder = ipc::Decoder::new();
        let mut got: Vec<ToFrontend> = Vec::new();
        let mut buf = [0u8; 4096];
        let mut theirs = theirs;
        while got.len() < 5 {
            let n = theirs.read(&mut buf).expect("read");
            got.extend(decoder.feed::<ToFrontend>(&buf[..n]).expect("decode"));
        }
        assert_eq!(got[0], ToFrontend::Text(b"row".to_vec()));
        assert_eq!(got[1], ToFrontend::ClearScreen);
        assert!(matches!(&got[2], ToFrontend::Frame(f) if f.seq == 1 && f.viewport_gen == 3));
        assert!(matches!(&got[3], ToFrontend::Frame(f) if f.seq == 2));
        assert!(
            matches!(&got[4], ToFrontend::Helper { id: 5, job: Job::External { url, command: None } } if url == "https://a.example/")
        );
        // The third frame waits for a paint.
        term.painted_up_to(2, None);
        assert!(!term.painted().all);
        term.painted_up_to(3, None);
        assert!(term.painted().all);
        // An answer nobody waits for is dropped; one somebody does is kept.
        term.answer(9, HelperOutcome::Picker(picker::Outcome::Cancel));
        assert!(term.poll_helper(9, &[]).is_none());
        let answer = outcome_for(Outcome::Picker {
            tab: "t".to_string(),
            node: 1,
            result: Picked::Cancel,
        })
        .unwrap()
        .unwrap();
        term.answer(5, answer);
        assert!(matches!(
            term.poll_helper(5, &[]),
            Some(HelperOutcome::Picker(picker::Outcome::Cancel))
        ));
        assert!(term.poll_helper(5, &[]).is_none(), "once");
    }

    #[test]
    fn a_held_queue_drops_frames_and_never_stalls() {
        let now = Instant::now();
        let mut queue = FrameQueue::default();
        assert_eq!(queue.offer(frame(), now).map(|f| f.seq), Some(1));
        queue.pause();
        // What was out is given up, so the engine's frame is acknowledged.
        assert!(queue.all_painted());
        assert!(queue.offer(frame(), now).is_none());
        assert!(queue.offer(frame(), now).is_none());
        assert!(queue.all_painted(), "nothing held while paused");
        assert_eq!(queue.stall(now + STALL * 10), None, "never late");
        // The frame out before the pause is said painted after the helper:
        // no news, and its wait is the helper's.
        assert!(queue
            .painted(1, Some(Duration::from_millis(7)), now)
            .is_none());
        assert_eq!(queue.waited.take(), None);
        queue.resume();
        assert_eq!(
            queue.offer(frame(), now).map(|f| f.seq),
            Some(2),
            "numbered on, the dropped ones never numbered"
        );
        assert_eq!(queue.stall(now), None);
    }

    #[test]
    fn a_terminal_helper_holds_the_cast_until_the_frontend_says_it_is_back() {
        let (ours, theirs) = UnixStream::pair().expect("a pair");
        let route = RouteFlags {
            paced: true,
            png: false,
            keyed: true,
            alpha: None,
            every_nth: 1,
        };
        let mut term = RemoteTerminal::new(Conn::new(ours), route);
        let offer = |term: &mut RemoteTerminal| {
            term.frame(FrameOut {
                payload: Encoded::Jpeg(&[9, 9]),
                cells: Cells { cols: 2, rows: 1 },
                row: 2,
                pixels: (16, 16),
                viewport_gen: 1,
            })
            .unwrap();
        };
        let picker = |kind| Helper::Picker {
            tab: "t".to_string(),
            chooser: crate::upload::Chooser {
                backend_node_id: 4,
                multiple: false,
                frame_id: String::new(),
                session: None,
            },
            command: picker::Command {
                words: vec!["pick".to_string()],
            },
            kind,
            dir: PathBuf::from("/"),
        };
        let login = |kind| Helper::Login {
            tab: "t".to_string(),
            url: "https://a.example/".to_string(),
            site: login::site("https://a.example/").expect("a site"),
            command: picker::Command {
                words: vec!["pass".to_string()],
            },
            kind,
            dir: PathBuf::from("/"),
        };
        assert_eq!(term.hold(), None, "nothing to hold for");
        offer(&mut term);
        assert!(!term.painted().all, "a frame is out");

        // A picker that takes the terminal: held at once, the frame out
        // given up, and nothing sent until the frontend says it is back.
        let started = term.start_helper(1, picker(picker::Kind::Terminal));
        assert_eq!(started.unwrap(), Started::Running);
        assert_eq!(term.hold(), Some(true));
        assert_eq!(term.hold(), None, "said once");
        assert!(term.painted().all);
        offer(&mut term);
        offer(&mut term);
        assert!(term.painted().all, "dropped, not out");
        assert_eq!(term.frames.stall(Instant::now() + STALL * 10), None);
        // The window losing interest is not the terminal coming back.
        term.end_helper(1);
        assert_eq!(term.hold(), None);
        // A helper that never had the terminal changes nothing.
        term.terminal_back(7);
        assert_eq!(term.hold(), None);
        term.terminal_back(1);
        assert_eq!(term.hold(), Some(false));
        offer(&mut term);
        assert!(!term.painted().all, "frames go again");

        // Two at once (a picker, then a password command): held until the
        // last is back.
        term.painted_up_to(u64::MAX, None);
        term.start_helper(2, picker(picker::Kind::Terminal))
            .unwrap();
        term.start_helper(3, login(picker::Kind::Terminal)).unwrap();
        assert_eq!(term.hold(), Some(true));
        term.terminal_back(2);
        assert_eq!(term.hold(), None);
        offer(&mut term);
        assert!(term.painted().all, "still held");
        term.terminal_back(3);
        assert_eq!(term.hold(), Some(false));

        // Ones that leave the terminal alone never hold it.
        term.start_helper(4, picker(picker::Kind::Gui)).unwrap();
        term.start_helper(5, login(picker::Kind::Gui)).unwrap();
        let external = Helper::External {
            argv: vec!["x".to_string()],
            browser: None,
            home: None,
            url: "https://a.example/".to_string(),
            configured: None,
        };
        term.start_helper(6, external).unwrap();
        assert_eq!(term.hold(), None);

        // The frontend going takes the hold with it: the window's own
        // suspension holds its cast, and its resumption starts it.
        term.start_helper(8, picker(picker::Kind::Terminal))
            .unwrap();
        assert_eq!(term.hold(), Some(true));
        let conn = term.detach().expect("attached");
        assert_eq!(term.hold(), None);
        term.attach(conn, route);
        assert_eq!(term.hold(), None);
        offer(&mut term);
        assert!(!term.painted().all, "frames go to the frontend back");
        drop(theirs);
    }

    #[test]
    fn a_login_crosses_as_what_the_command_printed_and_is_read_here() {
        let outcome = outcome_for(Outcome::Login {
            tab: "t".to_string(),
            url: "https://a.example/".to_string(),
            result: ipc::Fetched::Found(ipc::Secret::new(b"hunter2\nuser: me\n".to_vec())),
        })
        .unwrap();
        match outcome {
            Some(HelperOutcome::Login(login::Outcome::Found(found))) => {
                assert_eq!(found.password.as_str(), "hunter2");
                assert_eq!(found.user.as_ref().map(|u| u.as_str()), Some("me"));
            }
            other => panic!("not a login: {other:?}"),
        }
        assert_eq!(
            outcome_for(Outcome::External(Err("no".to_string()))).err(),
            Some("no".to_string())
        );
    }
}
