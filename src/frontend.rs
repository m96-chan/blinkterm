//! A terminal's half of a run: the frontend that attaches to its profile's
//! backend, or starts one, and draws the window the backend opens for it.
//!
//! Since #83 every start is two processes. This one keeps the terminal —
//! raw mode, the probe and the route, the parser, the painter and the
//! decoding beside it, the cell size, and the helper programs a person sees
//! (file pickers, password commands, a desktop browser) — and the profile's
//! backend ([`crate::backend`]) keeps everything else: the profile, the
//! engine, and the window itself, its tabs and its row. What crosses between
//! them is [`crate::ipc`].
//!
//! # Connect, or start
//!
//! [`attach`] tries the backend socket in the profile first. Somebody there
//! answers `hello` with `welcome`, and the window is opened on that
//! connection. Nobody there, and a candidate backend is started: this same
//! program with `--serve-fd 3`, the frontend's own arguments less the ones
//! that choose a profile or say what to open ([`argv_for_backend`]), in a
//! session of its own (`setsid`, so that no signal for this terminal reaches
//! it), with a socket pair at its descriptor 3 and its stderr in
//! `<profile>/backend.log`. It takes the profile lock first; if another
//! candidate took it first, it says `busy` and exits, and this frontend tries
//! the socket again until the winner listens. A backend on its way out says
//! so (`refused`, try again), and once it has gone, a fresh one is started.
//! All of it within [`ATTACH_TIMEOUT`].
//!
//! A temporary profile is never looked for: its backend is started for this
//! terminal alone and reached only through the pair.
//!
//! # The window
//!
//! Once attached the frontend sends `open` — its settings, its terminal's
//! size and route, the urls — and then loops: the terminal's bytes are
//! parsed here and sent as input, except a cell size, which is this side's;
//! a resize is sent as one; the backend's bytes are written as they come,
//! its frames painted and acknowledged, its helpers run. `ctrl+q` is the
//! backend's to see, and closes this window only: the backend says `closed`
//! and this process exits, leaving the other windows on the profile as they
//! are. A frontend never touches the engine — not from a signal, not from
//! its panic hook — because it is not this process's.
//!
//! # A dropped link
//!
//! The connection can go without the backend's `closed`: the backend killed
//! or crashed, or a connection it gave up on — one that stopped taking what
//! it was sent ([`ipc`]'s outbox). The backend that is still there keeps
//! the window, suspended, for [`crate::backend::GRACE`], and gives it back
//! to an `open` with the same nonce (`opened`, `resumed`), clearing the
//! screen and laying it out again. So a drop — a read that finds the
//! connection's end, or a write that fails, [`Link::dropped`] — is not the
//! end at once. The frontend closes its side, so that the backend has seen
//! it go, and tries the socket again for [`RESUME_WITHIN`], the same
//! fifteen seconds, with [`reattach`]; once welcomed, it sends `open` again
//! with its first nonce and settings and the pane's size now. Its frames
//! owed and resize pending are forgotten; the backend's relayout is what
//! comes next.
//!
//! [`reattach`] never starts a backend: a fresh one would be a fresh window,
//! and the window is what is being taken back. Nobody listening while
//! nobody holds the profile's lock is a backend gone for good, and the end
//! at once; nobody listening while the lock is held, or a backend that does
//! not answer, is tried until the fifteen seconds are out. Either way the
//! run ends with the sentence it always ended with, `stopped unexpectedly`
//! and where the log is. A temporary profile's backend listens on no socket,
//! so a drop there is the end as before.
//!
//! What the terminal sends in the gap is read, so that the terminal is not
//! held up, and dropped: a person typing at a frozen screen cannot see
//! where it is going, and the backend never saw it to say what it did, so
//! it is not sent to the window that comes back. A cell size is kept, being
//! this side's. What was sent before the drop is not sent again either:
//! whether the backend acted on it is not known. A `closed` the backend
//! said before it hung up is read before anything is tried, so that a
//! window the person closed is not taken back.
//!
//! A window the backend no longer has — another frontend started a fresh
//! backend after this one's died, or its grace was shorter than this side's
//! patience — is opened new, `resumed` false, on the home page and with no
//! urls, where it offers the lost tabs back as any start after a crash
//! does.

use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::app;
use crate::appearance::Appearance;
use crate::external;
use crate::fit::Metrics;
use crate::graphics::{Canvas, Painter};
use crate::input::Input;
use crate::ipc::{
    self, BrowserSettings, CloseWhy, Fetched, FrameKind, Job, Open, Outcome, Picked, RouteFlags,
    ToBackend, ToFrontend, WindowSettings,
};
use crate::login;
use crate::options::Options;
use crate::picker;
use crate::registry;
use crate::route::{self, Payload, Wrap};
use crate::screen::{self, Pane};
use crate::terminal::{Encoded, FrameOut, Helper, HelperOutcome, LocalTerminal, Started, Terminal};
use crate::tty::{self, ReadOutcome};
use crate::upload;

/// How long [`attach`] tries before it gives up with the sentence.
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a candidate backend may take to say whether it is up: as long
/// as the engine and its first page may take, and a little more.
pub const CANDIDATE_TIMEOUT: Duration = Duration::from_secs(40);

/// How long a backend has to answer `hello`, and `open`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long after starting a candidate another may be started, while the
/// socket says nobody is there: room for the one started to listen.
const SPAWN_EVERY: Duration = Duration::from_secs(2);

/// The first and the longest wait between tries.
const BACKOFF_FIRST: Duration = Duration::from_millis(100);
const BACKOFF_MOST: Duration = Duration::from_millis(500);

/// How long a closing window waits for the backend's `closed`.
const CLOSE_WAIT: Duration = Duration::from_secs(1);

/// How long a paste that has been opened and not closed may go without a
/// byte before it is given up on.
///
/// A terminal that sent `CSI 200 ~` and then nothing is a terminal that will
/// never send the end marker, and until it is given up on every key typed is
/// more paste. Longer than any stall of an ssh connection a person would sit
/// through with a paste half-arrived; and it is a silence, not a total, so a
/// 64 KiB paste that trickles in over a slow line for longer than this is
/// not cut while it is still coming.
const PASTE_IDLE: Duration = Duration::from_secs(2);

/// The longest and the shortest wait of a pass: the second while a frame
/// is owed its acknowledgement, which nothing wakes the poll for.
const POLL_MS: i32 = 50;
const FRAME_POLL_MS: i32 = 4;

/// The backend's log, in the profile: its standard error.
pub const LOG_FILE: &str = "backend.log";

// ---------------------------------------------------------------------------
// The link.
// ---------------------------------------------------------------------------

/// A connection to a backend that has welcomed this frontend.
pub struct Link {
    stream: UnixStream,
    decoder: ipc::Decoder,
    /// Messages read and not yet asked for.
    queued: std::collections::VecDeque<ToFrontend>,
    /// The backend's pid, its run's number, the profile directory and its
    /// name, as it said in `welcome`.
    pub backend_pid: u32,
    pub generation: u64,
    pub dir: PathBuf,
    pub label: String,
    /// Whether the connection itself has gone: the backend hung up, or
    /// would not take what was written. See [`Link::dropped`].
    broken: bool,
}

/// What a backend answered `open` with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opened {
    /// The window's number.
    pub window: u64,
    /// Whether it is a window this frontend had before, taken back by its
    /// nonce, rather than a new one.
    pub resumed: bool,
}

impl Link {
    /// The descriptor to poll.
    pub fn fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Say `message`, waiting for the backend to take it.
    pub fn send(&mut self, message: &ToBackend) -> Result<(), String> {
        self.stream.write_all(&ipc::encode(message)).map_err(|e| {
            // A write that failed, or timed out halfway, leaves nothing on
            // this connection worth saying more on.
            self.broken = true;
            format!(
                "cannot reach the blinkterm serving {}: {e}",
                self.dir.display()
            )
        })
    }

    /// Whether the connection itself has gone — the backend hung up, or
    /// stopped taking what was written — rather than said something this
    /// side cannot read. A dropped link is one the window's loop tries to
    /// take the window back from; see the module's section on it.
    pub fn dropped(&self) -> bool {
        self.broken
    }

    /// The next message, waiting up to `within`; `None` when none came.
    /// `Err` is the backend gone, or saying something that makes no sense.
    pub fn recv(&mut self, within: Duration) -> Result<Option<ToFrontend>, String> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(message) = self.queued.pop_front() {
                return Ok(Some(message));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let ms = left.as_millis().min(i32::MAX as u128) as i32;
            let ready = tty::poll_readable(&[self.fd()], ms).map_err(|e| e.to_string())?;
            if ready.is_empty() {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                continue;
            }
            self.read_once()?;
        }
    }

    /// Everything that has arrived, without waiting. The end of the
    /// connection is an `Err` only once everything said before it has been
    /// handed over: a backend says `closed` and hangs up.
    pub fn read_available(&mut self) -> Result<Vec<ToFrontend>, String> {
        for _ in 0..64 {
            let ready = tty::poll_readable(&[self.fd()], 0).map_err(|e| e.to_string())?;
            if ready.is_empty() {
                break;
            }
            if let Err(why) = self.read_once() {
                if self.queued.is_empty() {
                    return Err(why);
                }
                break;
            }
        }
        Ok(self.queued.drain(..).collect())
    }

    /// One read of what is there, which the caller knows is something.
    fn read_once(&mut self) -> Result<(), String> {
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    self.broken = true;
                    return Err(self.gone());
                }
                Ok(n) => {
                    let messages = self.decoder.feed::<ToFrontend>(&buf[..n])?;
                    self.queued.extend(messages);
                    return Ok(());
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(_) => {
                    self.broken = true;
                    return Err(self.gone());
                }
            }
        }
    }

    fn gone(&self) -> String {
        format!(
            "the blinkterm serving {} stopped unexpectedly; what it said is in {}",
            self.dir.display(),
            self.dir.join(LOG_FILE).display()
        )
    }

    /// Open a window: `open` sent, and the backend's answer — the window,
    /// or the refusal's sentence.
    pub fn open(&mut self, open: Open) -> Result<Opened, String> {
        self.send(&ToBackend::Open(Box::new(open)))?;
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT * 3;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.recv(left)? {
                Some(ToFrontend::Opened { window, resumed }) => {
                    return Ok(Opened { window, resumed })
                }
                Some(ToFrontend::Refused { why, .. }) => return Err(why),
                Some(ToFrontend::Closed { why, .. }) => return Err(why),
                Some(_) => {}
                None => {
                    return Err(format!(
                        "the blinkterm serving {} did not open a window",
                        self.dir.display()
                    ))
                }
            }
        }
    }
}

/// `hello` on `stream`, and what the backend says to it.
fn handshake(stream: UnixStream, dir: Option<&Path>) -> Result<Reached<Link>, String> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
    let mut link = Link {
        stream,
        decoder: ipc::Decoder::new(),
        queued: Default::default(),
        backend_pid: 0,
        generation: 0,
        dir: dir.map(Path::to_path_buf).unwrap_or_default(),
        label: String::new(),
        broken: false,
    };
    let answer = link
        .send(&ToBackend::Hello {
            protocol: ipc::PROTOCOL,
            version: ipc::VERSION.to_string(),
            dir: dir.map(Path::to_path_buf),
        })
        .and_then(|()| link.recv(HANDSHAKE_TIMEOUT));
    // A backend that hangs up before it answers is one on its way out: the
    // connection waited in its socket's queue while it stopped its engine,
    // and went when the socket did. That is tried again, as a backend that
    // says it is shutting down is, within the attach's deadline.
    let answer = match answer {
        Ok(answer) => answer,
        Err(why) => return Ok(Reached::Refused { why, retry: true }),
    };
    match answer {
        Some(ToFrontend::Welcome {
            pid,
            generation,
            dir,
            label,
            ..
        }) => {
            link.backend_pid = pid;
            link.generation = generation;
            link.dir = dir;
            link.label = label;
            Ok(Reached::Link(link))
        }
        Some(ToFrontend::Refused { why, retry }) => Ok(Reached::Refused { why, retry }),
        Some(other) => Err(format!("the backend answered hello with {other:?}")),
        None => Ok(Reached::Refused {
            why: format!(
                "the blinkterm serving {} did not answer",
                link.dir.display()
            ),
            retry: true,
        }),
    }
}

// ---------------------------------------------------------------------------
// Connect, or start.
// ---------------------------------------------------------------------------

/// What trying a profile's backend socket found.
pub enum Reached<L> {
    /// A backend, which welcomed this frontend.
    Link(L),
    /// Nobody listening.
    NobodyThere,
    /// A backend that will not take this frontend: now (`retry`), or ever.
    Refused { why: String, retry: bool },
}

/// What starting a candidate backend came to.
pub enum Spawned<L> {
    /// It holds the profile, its engine is up, and it welcomed this
    /// frontend on the pair.
    Ready(L),
    /// Somebody else holds the profile: the pid it wrote in the lock.
    Busy(Option<u32>),
    /// It could not start, and why.
    Failed(String),
}

/// How [`attach`] reaches backends: the real one starts this program, a
/// test's pretends.
pub trait Launcher {
    type Link;
    /// Try the backend socket of the profile at `dir`.
    fn connect(&mut self, dir: &Path) -> Result<Reached<Self::Link>, String>;
    /// Start a candidate backend on `dir`, or on a temporary profile.
    fn spawn(&mut self, dir: Option<&Path>) -> Result<Spawned<Self::Link>, String>;
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&mut self, how_long: Duration) {
        std::thread::sleep(how_long);
    }
    /// Whether nobody holds the lock on the profile at `dir`: its backend is
    /// gone, not just not listening. Asking takes the lock for the moment
    /// it takes to ask; a candidate backend that tries for it in that
    /// moment says `busy`, and its frontend tries the socket again, which
    /// is what it does for any other holder.
    fn lock_free(&self, dir: &Path) -> bool {
        matches!(
            crate::profile::try_lock(dir),
            Ok(crate::profile::Tried::Taken(_))
        )
    }
}

/// The sentence for a profile somebody holds who never starts taking
/// windows.
pub fn not_taking_windows(dir: &Path, holder: Option<u32>) -> String {
    let pid = holder.map_or_else(|| "unknown".to_string(), |pid| pid.to_string());
    format!(
        "the profile at {} is in use by pid {pid}, which is not taking windows — an older \
         blinkterm, or one on its way out; quit it, open the url there with blinkterm --remote \
         <url>, or run this one with --temp-profile or --profile <dir>",
        dir.display()
    )
}

/// Reach the backend of the profile at `dir`, starting one when there is
/// none: see the module's section. `None` is a temporary profile, whose
/// backend is always this frontend's own.
pub fn attach<L: Launcher>(launcher: &mut L, dir: Option<&Path>) -> Result<L::Link, String> {
    let Some(dir) = dir else {
        return match launcher.spawn(None)? {
            Spawned::Ready(link) => Ok(link),
            Spawned::Busy(_) => Err("a temporary profile was in use, which cannot be".to_string()),
            Spawned::Failed(why) => Err(why),
        };
    };
    let deadline = launcher.now() + ATTACH_TIMEOUT;
    let mut backoff = BACKOFF_FIRST;
    let mut spawned: Option<Instant> = None;
    let mut holder: Option<u32> = None;
    let mut last_refusal: Option<String> = None;
    loop {
        match launcher.connect(dir)? {
            Reached::Link(link) => return Ok(link),
            Reached::Refused { why, retry: false } => return Err(why),
            Reached::Refused { why, retry: true } => last_refusal = Some(why),
            Reached::NobodyThere => {
                let due = spawned
                    .is_none_or(|at| launcher.now().saturating_duration_since(at) > SPAWN_EVERY);
                if due {
                    spawned = Some(launcher.now());
                    match launcher.spawn(Some(dir))? {
                        Spawned::Ready(link) => return Ok(link),
                        Spawned::Busy(pid) => holder = pid.or(holder),
                        Spawned::Failed(why) => return Err(why),
                    }
                }
            }
        }
        if launcher.now() >= deadline {
            return Err(last_refusal.unwrap_or_else(|| not_taking_windows(dir, holder)));
        }
        launcher.sleep(backoff);
        backoff = (backoff * 2).min(BACKOFF_MOST);
    }
}

/// How long a frontend whose link dropped tries to take its window back:
/// as long as the backend keeps a window whose terminal went.
pub const RESUME_WITHIN: Duration = crate::backend::GRACE;

/// Why [`reattach`] did not reach the backend.
#[derive(Debug, PartialEq, Eq)]
pub enum NotTaken {
    /// It is gone — the profile's lock is free — or did not come back in
    /// time, or the caller stopped asking.
    Gone,
    /// It answered, and will not take this frontend: the sentence.
    Refused(String),
}

/// Reach the backend of the profile at `dir` again, after a link to it
/// dropped, until `deadline`: see the module's section on a dropped link.
/// Unlike [`attach`] this never starts a backend — a fresh one would be a
/// fresh window, and the window is what is being taken back — and nobody
/// listening while nobody holds the profile is the end at once.
/// `keep_going` is asked between tries, and `false` ends it as
/// [`NotTaken::Gone`].
pub fn reattach<L: Launcher>(
    launcher: &mut L,
    dir: &Path,
    deadline: Instant,
    mut keep_going: impl FnMut() -> bool,
) -> Result<L::Link, NotTaken> {
    let mut backoff = BACKOFF_FIRST;
    loop {
        match launcher.connect(dir) {
            Ok(Reached::Link(link)) => return Ok(link),
            Ok(Reached::Refused { why, retry: false }) => return Err(NotTaken::Refused(why)),
            // On its way out, or too busy to answer: it may yet come back,
            // and the deadline is what says it did not.
            Ok(Reached::Refused { retry: true, .. }) => {}
            Ok(Reached::NobodyThere) => {
                if launcher.lock_free(dir) {
                    return Err(NotTaken::Gone);
                }
            }
            Err(why) => return Err(NotTaken::Refused(why)),
        }
        if launcher.now() >= deadline || !keep_going() {
            return Err(NotTaken::Gone);
        }
        launcher.sleep(backoff);
        backoff = (backoff * 2).min(BACKOFF_MOST);
    }
}

/// The real [`Launcher`]: this program, started as a backend.
pub struct Spawn {
    /// The program to start: this one.
    pub exe: PathBuf,
    /// The frontend's own arguments, less `argv[0]`.
    pub args: Vec<String>,
    /// The profile's name, for the backend's row.
    pub label: Option<String>,
    /// The backends this frontend started, reaped as they exit.
    pub children: Vec<Child>,
}

impl Spawn {
    /// The backends this frontend started that have exited, waited for.
    pub fn reap(&mut self) {
        self.children
            .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
    }
}

impl Launcher for Spawn {
    type Link = Link;

    fn connect(&mut self, dir: &Path) -> Result<Reached<Link>, String> {
        match ipc::connect(dir)? {
            ipc::Connect::NobodyThere => Ok(Reached::NobodyThere),
            ipc::Connect::Stream(stream) => handshake(stream, Some(dir)),
        }
    }

    fn spawn(&mut self, dir: Option<&Path>) -> Result<Spawned<Link>, String> {
        let argv = argv_for_backend(&self.args, dir, self.label.as_deref());
        spawn_candidate(&self.exe, &argv, dir, &mut self.children)
    }
}

/// The options that take a value, as `--name value`, so that the value is
/// not taken for a url.
const VALUED: [&str; 28] = [
    "--download-dir",
    "--pdf-paper",
    "--search-url",
    "--scale",
    "--tmux",
    "--keymap",
    "--frames",
    "--fps",
    "--color-scheme",
    "--profile",
    "--profile-name",
    "--engine",
    "--engine-arg",
    "--block-list",
    "--sites-dir",
    "--user-agent",
    "--file-picker-terminal-multiple",
    "--file-picker-terminal",
    "--file-picker-multiple",
    "--file-picker",
    "--password-command-terminal",
    "--password-command",
    "--external-browser",
    "--proxy",
    "--home",
    "--config",
    "--serve-fd",
    "--grace-ms",
];

/// The arguments a candidate backend is started with: the frontend's own,
/// in order — so that it reads the same settings file and resolves every
/// browser-wide setting the same way — less what chooses the profile
/// (`--profile`, `--profile-name`, `--temp-profile`, `--choose-profile`),
/// what says what to open (`--remote`, `--restore`, the urls, everything
/// after `--`), and any `--serve-fd` or `--profile-label`; then the profile
/// pinned — `--profile <dir>`, or `--temp-profile` — its name for the row,
/// and `--serve-fd 3`.
pub fn argv_for_backend(args: &[String], dir: Option<&Path>, label: Option<&str>) -> Vec<String> {
    const DROPPED_FLAGS: [&str; 4] = [
        "--remote",
        "--restore",
        "--temp-profile",
        "--choose-profile",
    ];
    const DROPPED_VALUED: [&str; 4] = [
        "--profile",
        "--profile-name",
        "--serve-fd",
        "--profile-label",
    ];
    let mut out = Vec::new();
    let mut words = args.iter().peekable();
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        if DROPPED_FLAGS.contains(&word.as_str()) {
            continue;
        }
        let name = word.split_once('=').map_or(word.as_str(), |(name, _)| name);
        let joined = word.contains('=');
        if DROPPED_VALUED.contains(&name) {
            if !joined {
                words.next();
            }
            continue;
        }
        if VALUED.contains(&name) || name == "--profile-label" {
            out.push(word.clone());
            if !joined {
                if let Some(value) = words.next() {
                    out.push(value.clone());
                }
            }
            continue;
        }
        if word == "--alpha" {
            out.push(word.clone());
            let amount = words.peek().is_some_and(|next| {
                next.starts_with(|c: char| c.is_ascii_digit())
                    || next.as_str() == "true"
                    || next.as_str() == "false"
            });
            if amount {
                if let Some(value) = words.next() {
                    out.push(value.clone());
                }
            }
            continue;
        }
        if word.starts_with('-') && word.len() > 1 {
            out.push(word.clone());
            continue;
        }
        // A url: the window's, sent in `open`.
    }
    match dir {
        Some(dir) => {
            out.push("--profile".to_string());
            out.push(dir.display().to_string());
        }
        None => out.push("--temp-profile".to_string()),
    }
    if let Some(label) = label {
        out.push("--profile-label".to_string());
        out.push(label.to_string());
    }
    out.push("--serve-fd".to_string());
    out.push("3".to_string());
    out
}

/// Start `exe` with `argv` as a candidate backend on `dir` (a temporary
/// profile with none), and wait for what it says on the pair: see the
/// module's section. The child is kept in `children`, to be reaped once it
/// exits.
pub fn spawn_candidate(
    exe: &Path,
    argv: &[String],
    dir: Option<&Path>,
    children: &mut Vec<Child>,
) -> Result<Spawned<Link>, String> {
    let (ours, theirs) =
        UnixStream::pair().map_err(|e| format!("cannot make a socket pair: {e}"))?;
    let log = match dir {
        Some(dir) => {
            crate::profile::make_private_dir(dir)?;
            let path = dir.join(LOG_FILE);
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .map(Stdio::from)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?
        }
        None => Stdio::null(),
    };
    let given = theirs.as_raw_fd();
    let mut command = Command::new(exe);
    command
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    // SAFETY: the closure runs in the child between `fork` and `exec`, where
    // only async-signal-safe calls are allowed; `setsid(2)`, `dup2(2)`,
    // `fcntl(2)` and reading `errno` all are. `given` is an integer copied
    // in, and `theirs` stays open in the parent until `spawn` has returned,
    // so it is open in the child. `dup2` clears close-on-exec on the copy it
    // makes; when the pair's end already is 3 there is no copy, and the flag
    // is cleared by hand instead.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let placed = if given == 3 {
                libc::fcntl(3, libc::F_SETFD, 0)
            } else {
                libc::dup2(given, 3)
            };
            if placed < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    children.push(child);
    drop(theirs);
    let shown = dir.map_or_else(
        || "a temporary profile".to_string(),
        |d| d.display().to_string(),
    );
    let mut pair = Link {
        stream: ours,
        decoder: ipc::Decoder::new(),
        queued: Default::default(),
        backend_pid: 0,
        generation: 0,
        dir: dir.map(Path::to_path_buf).unwrap_or_default(),
        label: String::new(),
        broken: false,
    };
    let first = pair.recv(CANDIDATE_TIMEOUT).map_err(|_| {
        format!(
            "the blinkterm started to serve {shown} exited before it was ready{}",
            dir.map(|d| format!("; what it said is in {}", d.join(LOG_FILE).display()))
                .unwrap_or_default()
        )
    })?;
    match first {
        Some(ToFrontend::Ready { .. }) => {
            let stream = pair.stream;
            match handshake(stream, dir)? {
                Reached::Link(link) => Ok(Spawned::Ready(link)),
                Reached::Refused { why, .. } => Ok(Spawned::Failed(why)),
                Reached::NobodyThere => Ok(Spawned::Failed(format!(
                    "the blinkterm started to serve {shown} did not answer"
                ))),
            }
        }
        Some(ToFrontend::Busy { pid }) => Ok(Spawned::Busy((pid != 0).then_some(pid))),
        Some(ToFrontend::Failed { why }) => Ok(Spawned::Failed(why)),
        Some(other) => Ok(Spawned::Failed(format!(
            "the blinkterm started to serve {shown} said {other:?}"
        ))),
        None => Ok(Spawned::Failed(format!(
            "the blinkterm started to serve {shown} did not say it was ready in {} s",
            CANDIDATE_TIMEOUT.as_secs()
        ))),
    }
}

// ---------------------------------------------------------------------------
// The run.
// ---------------------------------------------------------------------------

/// A terminal's run: the terminal probed and the route chosen, the backend
/// reached or started, the terminal taken, a window opened, and the loop
/// until the backend says the window is closed.
pub fn run(options: Options, selected: registry::Selected) -> Result<(), String> {
    // The terminal is asked what it is before anything is started: a
    // terminal that cannot draw costs a sentence in the shell rather than a
    // Chromium start and a blank pane. See [`crate::doctor::probe`] and
    // [`route::choose`].
    let env = route::Env::current();
    let (verdict, heard) = if options.route.probe {
        crate::doctor::probe(0, env.tmux || env.screen, crate::doctor::TERMINAL_TIMEOUT)
            .map_err(|e| format!("cannot take the terminal: {e}"))?
    } else {
        (
            crate::doctor::Verdict::Skipped,
            crate::doctor::TerminalAnswer::default(),
        )
    };
    if let Some(why) = crate::doctor::refusal(verdict, &env) {
        return Err(why);
    }
    let route = route::choose(&env, options.route, verdict, Painter::shm_usable());
    // What the row says once the first page is up: a `key.` line on a chord
    // Kitty keeps, which would otherwise do nothing and say nothing. See
    // [`crate::taken`].
    let mut problems: Vec<String> = Vec::new();
    if env.kitty {
        problems.extend(crate::taken::conflicts(
            &options.bindings,
            cfg!(target_os = "macos"),
        ));
    }
    app::install_signals(app::Role::Frontend);
    std::panic::set_hook(Box::new(|info| {
        // A release build aborts here, so this is the only chance to put the
        // terminal back. The engine is the backend's, and other terminals'
        // windows are on it: it is not this process's to kill.
        screen::emergency();
        eprintln!("blinkterm: {info}");
    }));

    let mut launcher = Spawn {
        exe: std::env::current_exe()
            .map_err(|e| format!("cannot tell where this program is: {e}"))?,
        args: std::env::args().skip(1).collect(),
        label: selected.label.clone(),
        children: Vec::new(),
    };
    let mut link = attach(&mut launcher, selected.dir.as_deref())?;

    let mut appearance = Appearance::new(options.scheme, options.force_dark, options.alpha);
    appearance.keyed = route.payload == Payload::Raw;
    let pane = Pane::enter(0, 1, route.wrap == Wrap::None, route.wrap)
        .map_err(|e| format!("cannot take the terminal: {e}"))?;
    // The painter for the route chosen, beside what it takes out of a frame
    // under `--alpha`; and the cell size the probe heard, which over ssh is
    // what the page is sized for from the first frame rather than from the
    // second.
    let canvas = Canvas::new(
        Painter::with_route(route),
        appearance.keys(),
        appearance.alpha.scaling(),
    );
    let mut term = LocalTerminal::new(pane, canvas, heard.cell, &app::RESIZED);
    let outcome = term.metrics().and_then(|metrics| {
        let open = Open {
            nonce: ipc::new_nonce(),
            window: WindowSettings::from(&options),
            browser: BrowserSettings::from(&options),
            metrics,
            route: RouteFlags {
                paced: route.paced(),
                png: route.payload == Payload::Png,
                keyed: appearance.keyed,
                alpha: appearance.alpha.scaling(),
                every_nth: route.every_nth.max(1),
            },
            pixel_mouse: false,
            urls: options.urls.clone(),
            restore: options.restore,
            problems,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            home_dir: upload::home(),
        };
        link.open(open.clone())?;
        drive(
            &mut term,
            link,
            metrics,
            &mut launcher,
            selected.dir.as_deref(),
            &open,
        )
    });
    term.leave();
    outcome
}

/// What a helper the backend asked for is answering, for the answer.
enum Running {
    Picker { tab: String, node: i64 },
    Login { tab: String, url: String },
}

/// What the window's loop keeps from one pass to the next, and across a
/// link taken back.
struct Driving {
    buf: [u8; 8192],
    metrics: Metrics,
    // When the terminal last sent a byte of a paste that is still open.
    paste_heard: Option<Instant>,
    // The newest frame painted or dropped and not yet acknowledged.
    owed: Option<u64>,
    waited: Option<Duration>,
    // A resize has been sent and the backend has not laid the window out
    // again yet: a frame meanwhile is of the old size, and dropped.
    relayout_due: bool,
    viewport_gen: u32,
    helpers: Vec<(u64, Running)>,
}

/// Whether the window goes on after a pass.
enum Flow {
    Going,
    /// The window is over, and how the run ends.
    Ended(Result<(), String>),
}

/// The window, until the backend says it is closed. A link that drops
/// under it is taken back if it can be — see the module's section on a
/// dropped link — on the profile at `dir` (`None`, a temporary profile,
/// cannot be), with `open` sent again.
fn drive(
    term: &mut LocalTerminal,
    mut link: Link,
    metrics: Metrics,
    launcher: &mut Spawn,
    dir: Option<&Path>,
    open: &Open,
) -> Result<(), String> {
    let mut st = Driving {
        buf: [0u8; 8192],
        metrics,
        paste_heard: None,
        owed: None,
        waited: None,
        relayout_due: false,
        viewport_gen: 0,
        helpers: Vec::new(),
    };
    loop {
        match pass(term, &mut link, &mut st, launcher) {
            Ok(Flow::Going) => {}
            Ok(Flow::Ended(outcome)) => return outcome,
            Err(_) if link.dropped() => match resume(term, launcher, dir, open, &mut st, link)? {
                Some(again) => link = again,
                None => return Ok(()),
            },
            Err(why) => return Err(why),
        }
    }
}

/// One pass of the window's loop. `Err` with [`Link::dropped`] is the link
/// gone under it; any other `Err` is the end.
fn pass(
    term: &mut LocalTerminal,
    link: &mut Link,
    st: &mut Driving,
    launcher: &mut Spawn,
) -> Result<Flow, String> {
    let Driving {
        buf,
        metrics,
        paste_heard,
        owed,
        waited,
        relayout_due,
        viewport_gen,
        helpers,
    } = st;
    if app::quit_requested() {
        close(link, CloseWhy::Hangup)?;
        return Ok(Flow::Ended(Ok(())));
    }
    if let Some(now) = term.resized()? {
        *metrics = now;
        *viewport_gen = viewport_gen.wrapping_add(1);
        *relayout_due = true;
        link.send(&ToBackend::Resize {
            metrics: *metrics,
            viewport_gen: *viewport_gen,
        })?;
    }
    let mut watching = vec![term.input_fd(), link.fd()];
    watching.extend(term.helper_fds());
    let wait = if owed.is_some() {
        FRAME_POLL_MS
    } else {
        POLL_MS
    };
    let ready =
        tty::poll_readable(&watching, wait).map_err(|e| format!("cannot wait for input: {e}"))?;

    if ready.contains(&term.input_fd()) {
        match tty::read_available(term.input_fd(), buf) {
            Ok(ReadOutcome::Data(n)) => {
                let inputs = term.parse(&buf[..n]);
                *paste_heard = term.pasting().then(Instant::now);
                let pixel_mouse = term.pixel_coordinates();
                for input in inputs {
                    take_input(term, link, input, pixel_mouse, *metrics)?;
                }
            }
            Ok(ReadOutcome::Eof) => {
                close(link, CloseWhy::Terminal)?;
                return Ok(Flow::Ended(Ok(())));
            }
            Ok(ReadOutcome::WouldBlock) => {}
            Err(err) => return Err(format!("cannot read the terminal: {err}")),
        }
    } else if paste_heard.is_some_and(|at| at.elapsed() > PASTE_IDLE) {
        // A paste that was opened and has gone quiet: the end marker is
        // not coming, and what arrived is half of something.
        *paste_heard = None;
        if term.abandon_paste() {
            // The row is the backend's, so it is told to say so.
            let pixel_mouse = term.pixel_coordinates();
            take_input(term, link, Input::PasteCut, pixel_mouse, *metrics)?;
        }
    } else if let Some(input) = term.flush() {
        // Nothing arrived, so a held escape was the Escape key after all.
        let pixel_mouse = term.pixel_coordinates();
        take_input(term, link, input, pixel_mouse, *metrics)?;
    }
    term.reap();
    launcher.reap();

    if ready.contains(&link.fd()) {
        for message in link.read_available()? {
            match message {
                ToFrontend::Text(bytes) => term.write(&bytes)?,
                ToFrontend::ClearPicture => term.clear_picture()?,
                ToFrontend::ClearScreen => {
                    term.clear_screen()?;
                    *relayout_due = false;
                }
                ToFrontend::Frame(frame) => {
                    *owed = Some(owed.map_or(frame.seq, |seq| seq.max(frame.seq)));
                    if !*relayout_due {
                        let pixels = (
                            frame.cells.cols.saturating_mul(metrics.cell.0).max(1),
                            frame.cells.rows.saturating_mul(metrics.cell.1).max(1),
                        );
                        let payload = match frame.kind {
                            FrameKind::Jpeg => Encoded::Jpeg(&frame.image),
                            FrameKind::Png => Encoded::Png(&frame.image),
                        };
                        term.frame(FrameOut {
                            payload,
                            cells: frame.cells,
                            row: frame.row,
                            pixels,
                            viewport_gen: frame.viewport_gen,
                        })?;
                    }
                }
                ToFrontend::Helper { id, job } => {
                    if let Some(answer) = start_job(term, helpers, id, job)? {
                        link.send(&answer)?;
                    }
                }
                ToFrontend::Closed { why, exit } => {
                    return Ok(Flow::Ended(if exit == 0 { Ok(()) } else { Err(why) }));
                }
                ToFrontend::Refused { why, .. } => return Ok(Flow::Ended(Err(why))),
                _ => {}
            }
        }
    }
    // The helpers' answers, each once.
    let mut answered = Vec::new();
    for (index, (id, _)) in helpers.iter().enumerate() {
        if let Some(outcome) = term.poll_helper(*id, &ready) {
            answered.push((index, outcome));
        }
    }
    for (index, outcome) in answered.into_iter().rev() {
        let (id, running) = helpers.remove(index);
        link.send(&ToBackend::HelperDone {
            id,
            outcome: outcome_of(running, outcome),
        })?;
    }
    // The frames painted, acknowledged once they have all gone out.
    let painted = term.painted();
    if painted.waited.is_some() {
        *waited = painted.waited;
    }
    if let (true, Some(seq)) = (painted.all, *owed) {
        *owed = None;
        link.send(&ToBackend::Painted {
            seq,
            waited_ms: waited.take().map(|d| d.as_millis() as u64),
            viewport_gen: *viewport_gen,
        })?;
    }
    Ok(Flow::Going)
}

/// Take the window back after `lost` dropped: see the module's section on
/// a dropped link. `Some` is a link with the window open on it again;
/// `None` is the run over as it would have been with the link up — the
/// terminal gone, a signal to stop, or a `closed` the backend said before
/// it hung up; `Err` is the sentence to end with.
fn resume(
    term: &mut LocalTerminal,
    launcher: &mut Spawn,
    dir: Option<&Path>,
    open: &Open,
    st: &mut Driving,
    mut lost: Link,
) -> Result<Option<Link>, String> {
    // What the backend said before it hung up comes first: a window it
    // closed is closed, and a write that failed because it had already
    // said so and gone is not a drop. Taking it back would open a window
    // the person had just closed.
    for message in lost.read_available().unwrap_or_default() {
        match message {
            ToFrontend::Closed { why, exit } => {
                return if exit == 0 { Ok(None) } else { Err(why) };
            }
            ToFrontend::Refused { why, .. } => return Err(why),
            _ => {}
        }
    }
    let sentence = lost.gone();
    // Closed before anything is tried, so that the backend has seen this
    // connection go — and suspended the window — by the time another one
    // asks for it.
    drop(lost);
    // A temporary profile's backend listens on no socket: the pair it was
    // started with was the only way to it.
    let Some(dir) = dir else {
        return Err(sentence);
    };
    let deadline = Instant::now() + RESUME_WITHIN;
    let mut ended: Option<Result<(), String>> = None;
    let mut link = loop {
        let cell = st.metrics.cell;
        let reached = reattach(launcher, dir, deadline, || {
            gap(term, &mut st.buf, cell, &mut ended)
        });
        if let Some(outcome) = ended.take() {
            return outcome.map(|()| None);
        }
        let mut link = match reached {
            Ok(link) => link,
            Err(NotTaken::Gone) => return Err(sentence),
            Err(NotTaken::Refused(why)) => return Err(why),
        };
        // The window as it is now, since the pane may have changed size in
        // the gap; the same nonce, so that the backend takes the window
        // back rather than opening another. No urls: a window the backend
        // no longer has — its grace ran out first — opens on the home page
        // and offers its tabs back, rather than opening the urls this run
        // started with a second time.
        term.resized()?;
        st.metrics = term.metrics()?;
        let mut again = open.clone();
        again.metrics = st.metrics;
        again.pixel_mouse = term.pixel_coordinates();
        again.urls.clear();
        again.restore = false;
        again.problems.clear();
        match link.open(again) {
            Ok(_) => break link,
            Err(_) if link.dropped() && Instant::now() < deadline => {}
            Err(why) => return Err(why),
        }
    };
    // Up to the moment the window is back, what was typed was typed at a
    // window that was not there.
    if !gap(term, &mut st.buf, st.metrics.cell, &mut ended) {
        let why = if app::quit_requested() {
            CloseWhy::Hangup
        } else {
            CloseWhy::Terminal
        };
        let _ = close(&mut link, why);
        return ended.unwrap_or(Ok(())).map(|()| None);
    }
    // The backend clears the screen and lays the window out again; nothing
    // this side was waiting for on the old link is coming. A paste still
    // arriving is the person's one paste, and goes when it ends, or is cut
    // as any other.
    st.paste_heard = term.pasting().then(Instant::now);
    st.owed = None;
    st.waited = None;
    st.relayout_due = false;
    Ok(Some(link))
}

/// The terminal's bytes while there is no window to send them to: read and
/// dropped, but for a cell size, which is this side's (`cell` is the one
/// the window was laid out for). What was typed at a frozen screen is not
/// sent to the window that comes back: the person typing could not see
/// where it was going, and the backend never saw it to say what it did. An
/// escape held for its next byte is let go of with the rest. `false`, with
/// `ended` set, is the terminal gone or a signal to stop.
fn gap(
    term: &mut LocalTerminal,
    buf: &mut [u8],
    cell: (u32, u32),
    ended: &mut Option<Result<(), String>>,
) -> bool {
    if app::quit_requested() {
        *ended = Some(Ok(()));
        return false;
    }
    loop {
        match tty::poll_readable(&[term.input_fd()], 0) {
            Ok(ready) if ready.is_empty() => break,
            Ok(_) => {}
            Err(e) => {
                *ended = Some(Err(format!("cannot wait for input: {e}")));
                return false;
            }
        }
        match tty::read_available(term.input_fd(), buf) {
            Ok(ReadOutcome::Data(n)) => {
                for input in term.parse(&buf[..n]) {
                    if let Input::CellSize { width, height } = input {
                        term.cell_size(width, height, cell);
                    }
                }
            }
            Ok(ReadOutcome::WouldBlock) => break,
            Ok(ReadOutcome::Eof) => {
                *ended = Some(Ok(()));
                return false;
            }
            Err(err) => {
                *ended = Some(Err(format!("cannot read the terminal: {err}")));
                return false;
            }
        }
    }
    let _ = term.flush();
    true
}

/// One thing the terminal said: a cell size is this side's, a mode report
/// nobody's, and everything else the window's.
fn take_input(
    term: &mut LocalTerminal,
    link: &mut Link,
    input: Input,
    pixel_mouse: bool,
    metrics: Metrics,
) -> Result<(), String> {
    match input {
        Input::CellSize { width, height } => {
            term.cell_size(width, height, metrics.cell);
            Ok(())
        }
        Input::Mode { .. } => Ok(()),
        input => link.send(&ToBackend::Input { input, pixel_mouse }),
    }
}

/// Close the window and wait, briefly, for the backend to say it has.
fn close(link: &mut Link, why: CloseWhy) -> Result<(), String> {
    if link.send(&ToBackend::Close { why }).is_err() {
        return Ok(());
    }
    let deadline = Instant::now() + CLOSE_WAIT;
    while let Ok(Some(message)) = link.recv(deadline.saturating_duration_since(Instant::now())) {
        if let ToFrontend::Closed { .. } = message {
            break;
        }
    }
    Ok(())
}

/// Start what the backend asked for: `Some` is an answer to send at once,
/// for a helper that could not be started.
fn start_job(
    term: &mut LocalTerminal,
    helpers: &mut Vec<(u64, Running)>,
    id: u64,
    job: Job,
) -> Result<Option<ToBackend>, String> {
    let kind = |terminal: bool| {
        if terminal {
            picker::Kind::Terminal
        } else {
            picker::Kind::Gui
        }
    };
    match job {
        Job::Picker {
            tab,
            node,
            session,
            command,
            terminal,
            dir,
            multiple,
        } => {
            let helper = Helper::Picker {
                tab: tab.clone(),
                chooser: upload::Chooser {
                    backend_node_id: node,
                    multiple,
                    frame_id: String::new(),
                    session,
                },
                command,
                kind: kind(terminal),
                dir,
            };
            match term.start_helper(id, helper)? {
                Started::Running => {
                    helpers.push((id, Running::Picker { tab, node }));
                    Ok(None)
                }
                Started::Failed(why) => Ok(Some(ToBackend::HelperDone {
                    id,
                    outcome: Outcome::Picker {
                        tab,
                        node,
                        result: Picked::Failed(why),
                    },
                })),
            }
        }
        Job::Login {
            tab,
            url,
            command,
            terminal,
            site,
            dir,
        } => {
            let helper = Helper::Login {
                tab: tab.clone(),
                url: url.clone(),
                site,
                command,
                kind: kind(terminal),
                dir,
            };
            match term.start_helper(id, helper)? {
                Started::Running => {
                    helpers.push((id, Running::Login { tab, url }));
                    Ok(None)
                }
                Started::Failed(why) => Ok(Some(ToBackend::HelperDone {
                    id,
                    outcome: Outcome::Login {
                        tab,
                        url,
                        result: Fetched::Failed(why),
                    },
                })),
            }
        }
        Job::External { command, url } => {
            // Decided here, with this terminal's environment: its `BROWSER`
            // and whether it has a desktop.
            let browser = std::env::var("BROWSER").ok();
            let display = picker::has_display(|name| std::env::var(name).ok());
            let plan = external::plan(
                command.as_ref(),
                cfg!(target_os = "macos"),
                browser.as_deref(),
                display,
            );
            let failed = |why: String| {
                Some(ToBackend::HelperDone {
                    id,
                    outcome: Outcome::External(Err(why)),
                })
            };
            let chosen = match plan {
                external::Plan::Run(chosen) => chosen,
                external::Plan::NoDesktop => return Ok(failed(external::NO_DESKTOP.to_string())),
            };
            let helper = Helper::External {
                argv: external::argv(&chosen, &url),
                browser: external::browser_to_pass(browser.as_deref()),
                home: upload::home(),
                url,
                configured: command,
            };
            match term.start_helper(id, helper)? {
                Started::Running => Ok(None),
                Started::Failed(why) => Ok(failed(why)),
            }
        }
    }
}

/// A helper's answer as the backend is told it. A login crosses as what
/// the command printed would have been, rebuilt from what was read of it —
/// the password line, and a `user:` line — in a buffer made the right size
/// once, so that no copy is left behind by a reallocation; it is overwritten
/// when the message is dropped.
fn outcome_of(running: Running, outcome: HelperOutcome) -> Outcome {
    match (running, outcome) {
        (Running::Picker { tab, node }, HelperOutcome::Picker(outcome)) => Outcome::Picker {
            tab,
            node,
            result: match outcome {
                picker::Outcome::Files(files) => Picked::Files(files),
                picker::Outcome::Cancel => Picked::Cancel,
                picker::Outcome::Failed(why) => Picked::Failed(why),
            },
        },
        (Running::Login { tab, url }, HelperOutcome::Login(outcome)) => Outcome::Login {
            tab,
            url,
            result: match outcome {
                login::Outcome::Found(found) => Fetched::Found(ipc::Secret::new(printed(&found))),
                login::Outcome::None => Fetched::None,
                login::Outcome::Failed(why) => Fetched::Failed(why),
            },
        },
        (Running::Picker { tab, node }, HelperOutcome::Login(_)) => Outcome::Picker {
            tab,
            node,
            result: Picked::Failed("the helper answered something else".to_string()),
        },
        (Running::Login { tab, url }, HelperOutcome::Picker(_)) => Outcome::Login {
            tab,
            url,
            result: Fetched::Failed("the helper answered something else".to_string()),
        },
    }
}

/// What a password command would have printed for `found`, in the form
/// [`login::parse_output`] reads.
fn printed(found: &login::Login) -> Vec<u8> {
    let password = found.password.as_str().as_bytes();
    let user = found.user.as_ref().map(|user| user.as_str().as_bytes());
    let size = password.len() + 1 + user.map_or(0, |user| user.len() + 7);
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(password);
    out.push(b'\n');
    if let Some(user) = user {
        out.extend_from_slice(b"user: ");
        out.extend_from_slice(user);
        out.push(b'\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn the_backend_gets_the_frontends_settings_and_its_profile_pinned() {
        let args = words(&[
            "--profile-name",
            "Work",
            "--engine-arg",
            "--accept-lang=ja",
            "example.com",
            "--remote",
            "--download-dir",
            "/d",
            "--engine-arg=--mute-audio",
            "--alpha",
            "40",
            "--restore",
            "b.example",
            "--",
            "-c.example",
        ]);
        let argv = argv_for_backend(&args, Some(Path::new("/p")), Some("Work"));
        assert_eq!(
            argv,
            words(&[
                "--engine-arg",
                "--accept-lang=ja",
                "--download-dir",
                "/d",
                "--engine-arg=--mute-audio",
                "--alpha",
                "40",
                "--profile",
                "/p",
                "--profile-label",
                "Work",
                "--serve-fd",
                "3",
            ])
        );
        let temp = argv_for_backend(&words(&["--temp-profile", "--no-probe", "x"]), None, None);
        assert_eq!(
            temp,
            words(&["--no-probe", "--temp-profile", "--serve-fd", "3"])
        );
        let pinned = argv_for_backend(
            &words(&[
                "--profile=/q",
                "--choose-profile",
                "--serve-fd",
                "9",
                "--grace-ms",
                "5",
            ]),
            Some(Path::new("/q")),
            None,
        );
        assert_eq!(
            pinned,
            words(&["--grace-ms", "5", "--profile", "/q", "--serve-fd", "3"])
        );
    }

    /// A launcher that answers from a script, on a clock it moves itself.
    struct Fake {
        connects: Vec<Reached<u32>>,
        spawns: Vec<Spawned<u32>>,
        clock: Instant,
        spawned: usize,
        slept: Duration,
        /// What the profile's lock is asked to say: held, unless a test
        /// says otherwise.
        lock_free: bool,
    }

    impl Fake {
        fn new(connects: Vec<Reached<u32>>, spawns: Vec<Spawned<u32>>) -> Fake {
            Fake {
                connects,
                spawns,
                clock: Instant::now(),
                spawned: 0,
                slept: Duration::ZERO,
                lock_free: false,
            }
        }
    }

    impl Launcher for Fake {
        type Link = u32;
        fn connect(&mut self, _dir: &Path) -> Result<Reached<u32>, String> {
            Ok(if self.connects.is_empty() {
                Reached::NobodyThere
            } else {
                self.connects.remove(0)
            })
        }
        fn spawn(&mut self, _dir: Option<&Path>) -> Result<Spawned<u32>, String> {
            self.spawned += 1;
            Ok(if self.spawns.is_empty() {
                Spawned::Busy(Some(77))
            } else {
                self.spawns.remove(0)
            })
        }
        fn now(&self) -> Instant {
            self.clock + self.slept
        }
        fn sleep(&mut self, how_long: Duration) {
            self.slept += how_long;
        }
        fn lock_free(&self, _dir: &Path) -> bool {
            self.lock_free
        }
    }

    #[test]
    fn nobody_there_starts_one_and_a_busy_one_is_waited_for() {
        let dir = Path::new("/p");
        // Nobody, a candidate that lost the lock, then the winner listening.
        let mut fake = Fake::new(
            vec![Reached::NobodyThere, Reached::NobodyThere, Reached::Link(5)],
            vec![Spawned::Busy(Some(9))],
        );
        assert_eq!(attach(&mut fake, Some(dir)).ok(), Some(5));
        assert_eq!(fake.spawned, 1, "not started again within two seconds");

        // A candidate that is ready answers at once.
        let mut fake = Fake::new(vec![], vec![Spawned::Ready(6)]);
        assert_eq!(attach(&mut fake, Some(dir)).ok(), Some(6));

        // A temporary profile is always started, never looked for.
        let mut fake = Fake::new(vec![Reached::Link(1)], vec![Spawned::Ready(2)]);
        assert_eq!(attach(&mut fake, None).ok(), Some(2));
    }

    #[test]
    fn a_refusal_is_final_unless_it_says_to_try_again() {
        let dir = Path::new("/p");
        let mut fake = Fake::new(
            vec![Reached::Refused {
                why: "versions".to_string(),
                retry: false,
            }],
            vec![],
        );
        assert_eq!(
            attach(&mut fake, Some(dir)).err().as_deref(),
            Some("versions")
        );
        let mut fake = Fake::new(
            vec![
                Reached::Refused {
                    why: "shutting down".to_string(),
                    retry: true,
                },
                Reached::NobodyThere,
            ],
            vec![Spawned::Ready(3)],
        );
        assert_eq!(attach(&mut fake, Some(dir)).ok(), Some(3));
        let mut fake = Fake::new(vec![], vec![Spawned::Failed("no engine".to_string())]);
        assert_eq!(
            attach(&mut fake, Some(dir)).err().as_deref(),
            Some("no engine")
        );
    }

    #[test]
    fn a_holder_that_never_takes_windows_is_named_at_the_deadline() {
        let mut fake = Fake::new(vec![], vec![]);
        let why = attach(&mut fake, Some(Path::new("/p"))).expect_err("gave up");
        assert_eq!(why, not_taking_windows(Path::new("/p"), Some(77)));
        assert!(why.starts_with(
            "the profile at /p is in use by pid 77, which is not taking windows — an older \
             blinkterm, or one on its way out; quit it"
        ));
        assert!(fake.slept >= ATTACH_TIMEOUT);
        assert!(fake.spawned > 1, "tried again every two seconds");
    }

    /// A backend that hangs up on a hello without a word — one whose socket
    /// went while the connection waited in its queue — is tried again, not
    /// the end of the attach.
    #[test]
    fn a_backend_that_hangs_up_on_hello_is_tried_again() {
        let (ours, theirs) = UnixStream::pair().expect("a pair");
        drop(theirs);
        match handshake(ours, Some(Path::new("/p"))) {
            Ok(Reached::Refused { retry: true, .. }) => {}
            Ok(Reached::Refused { why, retry: false }) => panic!("final: {why}"),
            Ok(_) => panic!("not refused"),
            Err(why) => panic!("the attach would end: {why}"),
        }
    }

    /// A backend that is still there — the lock held — and not listening
    /// for a moment is waited for, and nothing is started meanwhile.
    #[test]
    fn a_dropped_link_is_taken_back_without_starting_a_backend() {
        let dir = Path::new("/p");
        let mut fake = Fake::new(
            vec![Reached::NobodyThere, Reached::NobodyThere, Reached::Link(5)],
            vec![Spawned::Ready(9)],
        );
        let deadline = fake.now() + RESUME_WITHIN;
        assert_eq!(reattach(&mut fake, dir, deadline, || true), Ok(5));
        assert_eq!(fake.spawned, 0, "a backend was started");
    }

    /// Nobody listening and nobody holding the profile: the backend is
    /// gone, and there is nothing to wait for.
    #[test]
    fn a_backend_gone_for_good_is_given_up_on_at_once() {
        let mut fake = Fake::new(vec![], vec![Spawned::Ready(9)]);
        fake.lock_free = true;
        let deadline = fake.now() + RESUME_WITHIN;
        assert_eq!(
            reattach(&mut fake, Path::new("/p"), deadline, || true),
            Err(NotTaken::Gone)
        );
        assert!(fake.slept < BACKOFF_MOST, "waited {:?}", fake.slept);
        assert_eq!(fake.spawned, 0);
    }

    /// Somebody holding the profile who never listens again is waited for
    /// as long as the backend would keep the window, and no longer; and a
    /// caller that stops asking ends it at once.
    #[test]
    fn a_backend_that_holds_the_lock_but_never_listens_is_given_up_on_at_the_deadline() {
        let mut fake = Fake::new(vec![], vec![]);
        let deadline = fake.now() + RESUME_WITHIN;
        assert_eq!(
            reattach(&mut fake, Path::new("/p"), deadline, || true),
            Err(NotTaken::Gone)
        );
        assert!(
            fake.slept >= RESUME_WITHIN,
            "gave up after {:?}",
            fake.slept
        );
        assert!(fake.slept < RESUME_WITHIN + BACKOFF_MOST * 2);
        assert_eq!(fake.spawned, 0);

        let mut fake = Fake::new(vec![], vec![]);
        let deadline = fake.now() + RESUME_WITHIN;
        let mut asked = 0;
        let stopped = reattach(&mut fake, Path::new("/p"), deadline, || {
            asked += 1;
            false
        });
        assert_eq!(stopped, Err(NotTaken::Gone));
        assert_eq!((asked, fake.slept), (1, Duration::ZERO), "one try");
    }

    #[test]
    fn a_final_refusal_ends_the_resume_with_its_sentence() {
        let dir = Path::new("/p");
        let mut fake = Fake::new(
            vec![Reached::Refused {
                why: "versions".to_string(),
                retry: false,
            }],
            vec![],
        );
        let deadline = fake.now() + RESUME_WITHIN;
        assert_eq!(
            reattach(&mut fake, dir, deadline, || true),
            Err(NotTaken::Refused("versions".to_string()))
        );
        let mut fake = Fake::new(
            vec![
                Reached::Refused {
                    why: "did not answer".to_string(),
                    retry: true,
                },
                Reached::Link(3),
            ],
            vec![],
        );
        let deadline = fake.now() + RESUME_WITHIN;
        assert_eq!(reattach(&mut fake, dir, deadline, || true), Ok(3));
    }

    /// A link over one end of a pair, as [`handshake`] makes one.
    fn a_link(stream: UnixStream) -> Link {
        Link {
            stream,
            decoder: ipc::Decoder::new(),
            queued: Default::default(),
            backend_pid: 0,
            generation: 0,
            dir: PathBuf::from("/p"),
            label: String::new(),
            broken: false,
        }
    }

    /// The connection's end, read or written, is a drop; and what the
    /// backend said before it hung up is handed over first, so that a
    /// `closed` is never taken for one.
    #[test]
    fn a_link_whose_peer_went_says_so() {
        let (ours, theirs) = UnixStream::pair().expect("a pair");
        let mut link = a_link(ours);
        assert!(!link.dropped());
        drop(theirs);
        let why = link.read_available().expect_err("the end");
        assert!(why.contains("stopped unexpectedly"), "{why}");
        assert!(link.dropped());

        let (ours, theirs) = UnixStream::pair().expect("a pair");
        let mut link = a_link(ours);
        drop(theirs);
        assert!(link.send(&ToBackend::Ping).is_err());
        assert!(link.dropped());

        let (ours, mut theirs) = UnixStream::pair().expect("a pair");
        let mut link = a_link(ours);
        theirs
            .write_all(&ipc::encode(&ToFrontend::Closed {
                why: String::new(),
                exit: 0,
            }))
            .expect("written");
        drop(theirs);
        let said = link.read_available().expect("what was said");
        assert!(
            matches!(said.as_slice(), [ToFrontend::Closed { exit: 0, .. }]),
            "{said:?}"
        );
        assert!(link.read_available().is_err(), "then the end");
    }

    #[test]
    fn a_login_crosses_as_the_lines_it_was_read_from() {
        let found = login::parse_output(b"hunter2\nuser: me\n").expect("a login");
        let again = login::parse_output(&printed(&found)).expect("read again");
        assert_eq!(again.password.as_str(), "hunter2");
        assert_eq!(again.user.as_ref().map(|u| u.as_str()), Some("me"));
        let bare = login::parse_output(b"pw").expect("a login");
        assert_eq!(printed(&bare), b"pw\n");
    }
}
