//! Talking to the engine: commands out, events in.
//!
//! The DevTools protocol is two streams sharing one pipe. Commands carry an
//! `id` and come back as a reply with the same `id`; events arrive whenever
//! the engine has something to say and are addressed to nobody. A client has
//! to serve both without one starving the other, and it has to do it while the
//! main thread is sitting in `poll` waiting for a key.
//!
//! So: a thread owns the pipe's reading end and sorts what arrives into
//! replies and events; the main thread takes commands and drains events. The
//! two meet at a mutex and a condition variable, and — this is the part that
//! matters for a program with a terminal — at a pipe of their own. The reader
//! writes one byte to it whenever something arrives, so the main loop can wait
//! on the terminal and on the engine in a single `poll` instead of choosing
//! which one to block on or spinning between them.
//!
//! Every command has a deadline. An engine that stops answering is the failure
//! this program was written around, and a client that waits forever for a
//! reply turns it into a pane that cannot even be quit.
//!
//! The mailbox keeps a reply only while somebody is coming back for it. Most
//! of what this program says to the engine is said with [`Client::notify`] —
//! the acknowledgement of a screencast frame, sixty times a second, a
//! `mouseWheel` fourteen times a notch, a key as it is pressed — and Chromium
//! answers every one of them whether or not the answer is worth anything. A
//! mailbox that filed all of those would grow for as long as the pane is open:
//! an hour of reading is hundreds of thousands of replies nobody will ever
//! ask for. So the id of a command whose reply *will* be collected is
//! registered before the command goes out, and the reader thread keeps a reply
//! only if it finds its id there — a hash lookup on the hot path, and nothing
//! kept for the rest. What registers an id gives it back: a [`Pending`] that
//! is dropped without its reply, and a call that gives up waiting, both leave
//! the mailbox as they found it.
//!
//! # A pipe, and no port
//!
//! This used to be a WebSocket per target on the engine's
//! `--remote-debugging-port`, and that port was the bug: any process on the
//! machine could connect to it and drive the browser — read every page, type
//! into every form — and with `--remote-allow-origins=*` so could any web
//! page that guessed the number. Taking the flag away would have fixed only
//! the second half. Measured against `headless_shell` 141.0.7390.37 without
//! it, a handshake with no `Origin` header at all, which is what a local
//! process sends, is still answered `101`; only one that names an origin gets
//! `403`. A port on loopback is a port for everybody on the machine.
//!
//! So the engine is started with `--remote-debugging-pipe` and this module
//! speaks to it on two descriptors it inherits, 3 to read and 4 to write, which
//! nobody else has and nothing else can open. The framing on it was measured
//! rather than read about: one JSON message, then a single NUL, in both
//! directions, with no length in front. A screenshot's reply is a megabyte and
//! spans several `read(2)`s, so the NUL is the only boundary there is and
//! [`split_messages`] is the whole of the framing.
//!
//! # One pipe, many pages: sessions
//!
//! A pipe is one connection, and a browser has several pages. CDP's answer is
//! *flattened sessions*: `Target.attachToTarget` with `flatten: true` answers
//! with a `sessionId`, and from then on every command for that page carries it
//! at the top level and every reply and event from that page comes back with
//! it. Messages with no `sessionId` are the browser's own. That was measured
//! too: the `Target.attachedToTarget` event arrives before the reply to the
//! attach, and a `Target.closeTarget` is followed at browser level by
//! `Target.targetInfoChanged` with `attached: false` and then
//! `Target.detachedFromTarget` naming the session.
//!
//! So what used to be a socket and a reader thread per client is one
//! [`Exchange`] — one reader thread, one writing end, and a mailbox per
//! session — and a [`Client`] is a session on it. The surface a client shows
//! the rest of the program did not change: `call`, `send`, `notify`, `events`,
//! a wake pipe of its own to `poll`. The wake pipe staying per client is
//! deliberate. The loop polls the tab in front and the browser and nothing
//! else, so a background tab's events must not wake it, and the loop drains
//! "the pipe that was readable" by which client owns it.
//!
//! Watch the name. `Page.screencastFrame` carries a `sessionId` too, in its
//! `params`, and that one is an *integer*: the screencast's own session, which
//! the loop hands back in `Page.screencastFrameAck`. The one the router reads
//! is a *string* at the top level of the message, beside `id` and `method`.
//! They have nothing to do with each other and sit one level apart.

use std::collections::{HashMap, HashSet, VecDeque};
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tos_platform::tty::{self, ReadOutcome};

use crate::json::Json;

/// How long a command may take before it is a failure rather than a wait.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// The largest message that will be assembled.
///
/// A screencast frame at a pane's size is tens of kilobytes; a megabyte is a
/// very large page screenshot. Sixteen is room for anything CDP sends and a
/// ceiling on what a confused peer can make this program allocate — which on
/// a pipe with no length prefix means sixteen megabytes with no NUL in them.
pub const MAX_MESSAGE: usize = 16 << 20;

/// How long a write may wait for the engine to make room in the pipe.
///
/// A pipe holds 64 KiB on Linux and a command is a few hundred bytes, so a
/// write that has to wait at all is an engine that has stopped reading. Five
/// seconds is long enough for one that is merely busy and short enough that a
/// key pressed at a wedged engine comes back as a sentence.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How much the reader asks for at a time. A screenshot's reply is a
/// megabyte, so this is sixteen reads for one, which is nothing.
const CHUNK: usize = 64 << 10;

/// How often the reader looks up from the pipe to see whether it is being
/// stopped. It is also the most [`Exchange::shutdown`] waits for it.
const READ_POLL_MS: i32 = 200;

/// A mailbox, as the reader and the client share it.
type Slot = Arc<(Mutex<Mailbox>, Condvar)>;

/// A command that has gone out and whose reply has not been taken yet.
///
/// The method travels with the id so that a reply collected long after the
/// fact says what it was a reply to, exactly as [`Client::call`]'s errors do.
///
/// It carries the mailbox too, because a command that is given up on is the
/// ordinary case — a tab switched away from while the engine was drawing, a
/// still that timed out — and the reply to it must not be kept. Dropping this
/// is what says so: the id stops being wanted, and a reply that arrived in the
/// meantime goes with it. That is also why it is neither `Clone` nor `Eq`;
/// two of these for one id would be two claims on one reply, and the first
/// drop would cancel the second.
pub struct Pending {
    id: i64,
    method: String,
    mailbox: Slot,
}

impl Drop for Pending {
    fn drop(&mut self) {
        let (lock, _) = &*self.mailbox;
        if let Ok(mut mailbox) = lock.lock() {
            mailbox.forget(self.id);
        }
    }
}

impl std::fmt::Debug for Pending {
    /// The command, not the mailbox behind it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending")
            .field("id", &self.id)
            .field("method", &self.method)
            .finish()
    }
}

/// An event as it arrived: the method and its parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub method: String,
    pub params: Json,
}

/// The pipe the reader knocks on to wake the main loop: one per client.
///
/// It lives in the mailbox rather than in the client because the reader is
/// the one that writes to it, and the reader holds the mailbox — not the
/// client — while it does. So the descriptors close when the last holder of
/// the mailbox lets go, which cannot be while a knock is being written.
struct Wake {
    read: RawFd,
    write: RawFd,
}

impl Wake {
    fn new() -> Result<Wake, String> {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: `pipe(2)` writes exactly two `int`s through the pointer, and
        // `fds` is a live local of exactly that size and type.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(format!(
                "cannot make a pipe to wake the loop: {}",
                std::io::Error::last_os_error()
            ));
        }
        // Non-blocking on both ends: the reader must never block on a pipe the
        // main loop has not drained, and the main loop must never block
        // draining one that is already empty.
        tty::set_nonblocking(fds[0]).ok();
        tty::set_nonblocking(fds[1]).ok();
        Ok(Wake {
            read: fds[0],
            write: fds[1],
        })
    }

    /// One byte, best effort: a full pipe already means the main loop has
    /// been told.
    fn knock(&self) {
        let byte = b"\x01";
        // SAFETY: writes the single byte `byte` points at, and `byte` is alive
        // for the call. `self.write` is this `Wake`'s own descriptor and is
        // closed only in its `Drop`, which cannot run while `&self` is held.
        // The result is dropped on purpose: a full pipe means the main loop
        // has already been told.
        unsafe {
            libc::write(self.write, byte.as_ptr() as *const libc::c_void, 1);
        }
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        // SAFETY: both descriptors came from the `pipe(2)` in `Wake::new` and
        // are owned by this `Wake` alone; this is `Drop`, so nothing will use
        // or close them again.
        unsafe {
            libc::close(self.read);
            libc::close(self.write);
        }
    }
}

/// What has arrived, and who is still waiting for it.
#[derive(Default)]
struct Mailbox {
    /// The ids somebody has said they will come back for.
    ///
    /// A reply whose id is not here is a reply nobody asked to keep, and it is
    /// dropped where it is read. This is the whole of what bounds the map.
    wanted: HashSet<i64>,
    replies: HashMap<i64, Json>,
    events: VecDeque<Event>,
    /// Set once the session has gone, with the reason.
    ended: Option<String>,
    /// The client's wake pipe. `None` only in the tests, which sort into a
    /// mailbox with nobody polling it.
    wake: Option<Wake>,
}

impl Mailbox {
    /// Say that the reply to this id is going to be collected.
    ///
    /// Said before the command goes out, so that there is no window in which a
    /// reply could arrive at a mailbox not yet willing to keep it.
    fn want(&mut self, id: i64) {
        self.wanted.insert(id);
    }

    /// Stop waiting for this id, and throw away a reply that beat the giving
    /// up. Forgetting something already forgotten is nothing.
    fn forget(&mut self, id: i64) {
        self.wanted.remove(&id);
        self.replies.remove(&id);
    }

    /// The reply to this id if it has come; taking it ends the waiting.
    fn take(&mut self, id: i64) -> Option<Json> {
        let reply = self.replies.remove(&id)?;
        self.wanted.remove(&id);
        Some(reply)
    }

    /// Tell whoever polls this mailbox's pipe to look in it.
    fn knock(&self) {
        if let Some(wake) = &self.wake {
            wake.knock();
        }
    }
}

/// Say a mailbox's session is over, and wake everybody who would want to know.
fn end(slot: &Slot, why: &str) {
    let (lock, signal) = &**slot;
    if let Ok(mut mailbox) = lock.lock() {
        mailbox.ended.get_or_insert_with(|| why.to_string());
        mailbox.knock();
    }
    signal.notify_all();
}

/// Where the reader delivers: a mailbox per session, and whether the pipe has
/// ended.
///
/// Both behind one lock, so that a client made while the pipe is ending either
/// is in the map when the reader ends every mailbox in it, or finds the reason
/// already written here and is refused.
#[derive(Default)]
struct Routes {
    /// `None` is the browser's own mailbox, for the messages with no
    /// `sessionId`; every other key is a session a page was attached on.
    mailboxes: HashMap<Option<String>, Slot>,
    ended: Option<String>,
}

/// The one pipe to the engine, shared by every [`Client`] on it.
///
/// It is the engine's descriptors 3 and 4 seen from this side: a writing end
/// that every client puts its commands on, one at a time, and a reading end
/// that one thread reads and routes by `sessionId`. What the reader thread
/// shares with it is only the routing table and the stop flag — never the
/// `Exchange` itself — so the thread holds nothing that keeps the exchange
/// alive, and the exchange's own `Drop` can stop and join it.
pub struct Exchange {
    /// The end commands go out on. `None` once shut down, so that a write
    /// after the close is a sentence rather than a write to whatever
    /// descriptor the number has been given to since.
    write: Mutex<Option<RawFd>>,
    /// The end the reader reads, closed once the reader has been joined.
    read: Mutex<Option<RawFd>>,
    /// The id the last command went out under.
    ///
    /// One id space for every session on the pipe. The engine would accept
    /// the same id on two sessions, but nothing is gained by letting it, and
    /// an id that names one command whichever mailbox it lands in is one less
    /// thing to get wrong.
    next_id: AtomicI64,
    routes: Arc<Mutex<Routes>>,
    stop: Arc<AtomicBool>,
    reader: Mutex<Option<JoinHandle<()>>>,
}

impl Exchange {
    /// Take the two ends of a pipe to an engine and start reading.
    ///
    /// Both descriptors become the exchange's: it makes them non-blocking and
    /// closes them in [`Exchange::shutdown`]. Non-blocking is safe to set here
    /// even though the engine holds the other ends, because `O_NONBLOCK` lives
    /// on the open file description and a pipe's two ends are two of those —
    /// the engine's reads and writes stay blocking, as it expects them.
    pub fn over(read: RawFd, write: RawFd) -> Arc<Exchange> {
        tty::set_nonblocking(read).ok();
        tty::set_nonblocking(write).ok();
        let routes = Arc::new(Mutex::new(Routes::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_routes = Arc::clone(&routes);
        let thread_stop = Arc::clone(&stop);
        let reader = std::thread::spawn(move || read_loop(read, &thread_routes, &thread_stop));
        Arc::new(Exchange {
            write: Mutex::new(Some(write)),
            read: Mutex::new(Some(read)),
            next_id: AtomicI64::new(0),
            routes,
            stop,
            reader: Mutex::new(Some(reader)),
        })
    }

    /// Stop talking to the engine, and stop listening.
    ///
    /// The writing end is closed first, because that is what tells the engine
    /// to go: a browser whose fd 3 reaches end of file exits, and it was
    /// measured doing so with status 0 in 14 ms. Then the reader, which sees
    /// that exit as end of file on its own end, or the stop flag within
    /// 200 ms if something else in the engine's group is still
    /// holding the pipe open. Every client on the pipe is told why, and
    /// every call after this fails at once. Shutting down twice is once.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(mut write) = self.write.lock() {
            if let Some(fd) = write.take() {
                // SAFETY: `fd` was handed to `Exchange::over` to own, and
                // taking it out of the `Option` under the lock is what makes
                // this the one close of it: every writer looks in the same
                // `Option` under the same lock and finds `None` from now on.
                unsafe {
                    libc::close(fd);
                }
            }
        }
        let reader = self.reader.lock().ok().and_then(|mut reader| reader.take());
        if let Some(reader) = reader {
            let _ = reader.join();
        }
        if let Ok(mut read) = self.read.lock() {
            if let Some(fd) = read.take() {
                // SAFETY: the reader thread was the only user of this end and
                // it has been joined above, or had already been joined by an
                // earlier call, which is the only way `reader` is `None`; the
                // `take` makes this the one close.
                unsafe {
                    libc::close(fd);
                }
            }
        }
        end_all(&self.routes, "the pipe was closed from this end");
    }

    /// The id the next command goes out under. Every one is spent once,
    /// whichever client and whichever thread spends it.
    fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Put one message on the pipe, NUL and all.
    ///
    /// The lock is what keeps two messages from interleaving: the main loop
    /// and [`crate::scroll`]'s animator both write, and half a `mouseWheel`
    /// inside a `Page.navigate` is two broken commands.
    fn transmit(&self, message: &str) -> Result<(), String> {
        let mut guard = self
            .write
            .lock()
            .map_err(|_| "the pipe is poisoned".to_string())?;
        let Some(fd) = *guard else {
            return Err("the pipe is closed".to_string());
        };
        let mut bytes = Vec::with_capacity(message.len() + 1);
        bytes.extend_from_slice(message.as_bytes());
        bytes.push(0);
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let mut at = 0;
        while at < bytes.len() {
            let rest = &bytes[at..];
            // SAFETY: writes at most `rest.len()` bytes from `rest`, a live
            // slice of exactly that length. `fd` is the exchange's writing end
            // and cannot be closed under this call, because closing it takes
            // the lock this function is holding.
            let n = unsafe { libc::write(fd, rest.as_ptr() as *const libc::c_void, rest.len()) };
            if n >= 0 {
                at += n as usize;
                continue;
            }
            let err = std::io::Error::last_os_error();
            let why = match err.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EAGAIN) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if !left.is_zero() && wait_writable(fd, left) {
                        continue;
                    }
                    "the engine is not reading its pipe".to_string()
                }
                // The Rust runtime ignores SIGPIPE, and so does
                // `app::install_signals`, so a reader that has gone is this
                // and not the end of the program.
                Some(libc::EPIPE) => "the engine closed its end of the pipe".to_string(),
                _ => format!("cannot write to the engine's pipe: {err}"),
            };
            if at > 0 {
                // Half a message is on the pipe, and with no length prefix the
                // engine will read the next one as the rest of it. Nothing
                // said on this pipe can be understood again, so it is closed
                // rather than left to say garbage.
                // SAFETY: `fd` is the exchange's writing end, held under the
                // lock, and taking it out of the `Option` here makes this the
                // one close.
                unsafe {
                    libc::close(fd);
                }
                *guard = None;
            }
            return Err(why);
        }
        Ok(())
    }

    /// Give a session a mailbox, or say why it cannot have one.
    fn register(&self, session: Option<String>, mailbox: Mailbox) -> Result<Slot, String> {
        let mut routes = self
            .routes
            .lock()
            .map_err(|_| "the pipe is poisoned".to_string())?;
        if let Some(ended) = &routes.ended {
            return Err(ended.clone());
        }
        if routes.mailboxes.contains_key(&session) {
            return Err(match &session {
                None => "the browser already has a client on this pipe".to_string(),
                Some(session) => format!("session {session} already has a client"),
            });
        }
        let slot: Slot = Arc::new((Mutex::new(mailbox), Condvar::new()));
        routes.mailboxes.insert(session, Arc::clone(&slot));
        Ok(slot)
    }

    /// Stop delivering to a mailbox — this one, and not one registered since
    /// under the same session.
    fn deregister(&self, session: &Option<String>, slot: &Slot) {
        if let Ok(mut routes) = self.routes.lock() {
            if routes
                .mailboxes
                .get(session)
                .is_some_and(|held| Arc::ptr_eq(held, slot))
            {
                routes.mailboxes.remove(session);
            }
        }
    }
}

impl Drop for Exchange {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl std::fmt::Debug for Exchange {
    /// What it is, not the descriptors behind it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Exchange")
    }
}

/// Wait for room in the pipe. `false` is no room in the time.
fn wait_writable(fd: RawFd, timeout: Duration) -> bool {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    let ms = timeout.as_millis().clamp(1, i32::MAX as u128) as i32;
    // SAFETY: `poll(2)` reads and writes exactly one `pollfd` through the
    // pointer, and `poll_fd` is a live local of that type; the count says one.
    let n = unsafe { libc::poll(&mut poll_fd, 1, ms) };
    // An interrupted wait is a wait to go round again, which the caller does
    // by writing and finding `EAGAIN` once more; an error or a hang-up is
    // answered by that write too.
    n != 0
}

/// The reader thread: read, split, route, until the pipe ends or is stopped.
fn read_loop(read: RawFd, routes: &Mutex<Routes>, stop: &AtomicBool) {
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; CHUNK];
    let why = loop {
        if stop.load(Ordering::SeqCst) {
            break "the pipe was closed from this end".to_string();
        }
        match tty::poll_readable(&[read], READ_POLL_MS) {
            Ok(ready) if ready.is_empty() => continue,
            Ok(_) => {}
            Err(err) => break format!("cannot wait on the engine's pipe: {err}"),
        }
        match tty::read_available(read, &mut chunk) {
            Ok(ReadOutcome::Data(n)) => buf.extend_from_slice(&chunk[..n]),
            Ok(ReadOutcome::WouldBlock) => continue,
            Ok(ReadOutcome::Eof) => break "the engine closed its end of the pipe".to_string(),
            Err(err) => break format!("cannot read the engine's pipe: {err}"),
        }
        let messages = match split_messages(&mut buf) {
            Ok(messages) => messages,
            Err(why) => break why,
        };
        if messages.is_empty() {
            continue;
        }
        if let Ok(routes) = routes.lock() {
            for message in &messages {
                route(&routes.mailboxes, message);
            }
        }
    };
    end_all(routes, &why);
}

/// The pipe is over: every mailbox on it is told why, and so is every client
/// that asks for one afterwards.
fn end_all(routes: &Mutex<Routes>, why: &str) {
    let slots: Vec<Slot> = match routes.lock() {
        Ok(mut routes) => {
            routes.ended.get_or_insert_with(|| why.to_string());
            routes.mailboxes.values().cloned().collect()
        }
        Err(_) => return,
    };
    for slot in &slots {
        end(slot, why);
    }
}

/// Cut whole messages off the front of what has been read.
///
/// A message is everything up to a NUL, and the NUL is not part of it. What
/// is left after the last NUL is the start of a message still arriving and
/// stays in `buf` for the next read. An empty message — two NULs in a row —
/// is nothing and is skipped, and a message that is not UTF-8 is dropped
/// rather than ending the pipe over: CDP is JSON, and a message that is not
/// even text is one nobody could have been waiting for.
///
/// The one thing that is an error is [`MAX_MESSAGE`] bytes with no NUL in
/// them, because with no length prefix there is no other way to tell a very
/// large message from a peer that is never going to finish one.
pub fn split_messages(buf: &mut Vec<u8>) -> Result<Vec<String>, String> {
    let mut messages = Vec::new();
    let mut start = 0;
    while let Some(at) = buf[start..].iter().position(|&byte| byte == 0) {
        let end = start + at;
        if end > start {
            if let Ok(text) = std::str::from_utf8(&buf[start..end]) {
                messages.push(text.to_string());
            }
        }
        start = end + 1;
    }
    buf.drain(..start);
    if buf.len() > MAX_MESSAGE {
        return Err(format!(
            "the engine sent {} bytes without ending a message",
            buf.len()
        ));
    }
    Ok(messages)
}

/// Deliver one message to the mailbox it is for.
///
/// Addressed by the string `sessionId` at the top level of the message, and
/// by nothing else: no `sessionId` is the browser's own mailbox, and a
/// session nobody has a mailbox for — a client that has closed, a target
/// attached by something else — is dropped here. Not the `sessionId` in a
/// `Page.screencastFrame`'s `params`, which is an integer and belongs to the
/// screencast; see the module documentation.
///
/// `Target.detachedFromTarget` is the one message read twice. It arrives at
/// browser level and goes to the browser's mailbox like any other, but it is
/// also the end of the session it names: that mailbox is ended, so a client
/// on it hears that its page is gone the way it used to hear a socket close.
/// That is what keeps the tab list's two ways of hearing about a closed tab —
/// its client ending, and `Target.targetDestroyed` — two ways rather than one.
fn route(mailboxes: &HashMap<Option<String>, Slot>, text: &str) {
    let Ok(value) = Json::parse(text) else {
        return;
    };
    let session = value
        .get("sessionId")
        .and_then(Json::as_str)
        .map(str::to_string);
    if value.get("method").and_then(Json::as_str) == Some("Target.detachedFromTarget") {
        let detached = value
            .path(&["params", "sessionId"])
            .and_then(Json::as_str)
            .map(str::to_string);
        if let Some(slot) = detached.and_then(|detached| mailboxes.get(&Some(detached))) {
            end(slot, "the engine detached from the target");
        }
    }
    let Some(slot) = mailboxes.get(&session) else {
        return;
    };
    let (lock, signal) = &**slot;
    if let Ok(mut mailbox) = lock.lock() {
        sort(&mut mailbox, &value);
        // Under the mailbox's lock, like everything else that touches the
        // wake pipe, so a knock and a close of the pipe cannot cross.
        mailbox.knock();
    }
    signal.notify_all();
}

/// The right to put a notification on a client's session, from any thread.
///
/// [`Client`] is the main loop's and stays the main loop's: the mailbox, the
/// wake pipe and every command that expects a reply are its. This is the one
/// thing another thread needs — [`crate::scroll`]'s animator sends a
/// `mouseWheel` every 16 ms on a clock of its own, and having to ask the loop
/// to put it on the wire is exactly the dependency that made the scrolling
/// lurch.
///
/// It is the smallest thing that works, because both pieces underneath were
/// already shared: the pipe's writing end is behind the exchange's mutex, and
/// the command counter is the exchange's atomic, so an id is still spent
/// exactly once. Nothing here touches the mailbox or the reader, and a
/// notification's id is never registered, so the reply Chromium sends all the
/// same is dropped where it is read, exactly as before.
#[derive(Clone)]
pub struct Notifier {
    exchange: Arc<Exchange>,
    session: Option<String>,
}

impl Notifier {
    /// Send a command and do not wait for its reply. This is
    /// [`Client::notify`] without the `&mut`.
    pub fn notify(&self, method: &str, params: Json) -> Result<(), String> {
        let id = self.exchange.next_id();
        let message = message(id, self.session.as_deref(), method, params);
        self.exchange
            .transmit(&message)
            .map_err(|why| format!("cannot send {method}: {why}"))
    }
}

impl std::fmt::Debug for Notifier {
    /// What it is for, not the pipe behind it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Notifier")
    }
}

/// A session on the pipe: the browser's own, or one page target's.
pub struct Client {
    exchange: Arc<Exchange>,
    /// `None` for the browser; the page's `sessionId` otherwise.
    session: Option<String>,
    mailbox: Slot,
    /// Read end of the pipe the reader thread knocks on. It belongs to the
    /// mailbox, which this client holds for as long as it holds the number.
    wake_read: RawFd,
    closed: bool,
}

impl Client {
    /// The browser's own client: the messages with no `sessionId`.
    ///
    /// One at a time. Two would be two mailboxes for one stream of replies
    /// and events, and whichever was registered second would hear nothing, so
    /// asking for a second while the first is alive is refused.
    pub fn browser(exchange: &Arc<Exchange>) -> Result<Client, String> {
        Client::on(exchange, None)
    }

    fn on(exchange: &Arc<Exchange>, session: Option<String>) -> Result<Client, String> {
        let wake = Wake::new()?;
        let wake_read = wake.read;
        let mailbox = Mailbox {
            wake: Some(wake),
            ..Mailbox::default()
        };
        let mailbox = exchange.register(session.clone(), mailbox)?;
        Ok(Client {
            exchange: Arc::clone(exchange),
            session,
            mailbox,
            wake_read,
            closed: false,
        })
    }

    /// Attach to a page target and hand back a client on its session.
    ///
    /// Only the browser's client may: attaching is a browser-level command,
    /// and a page asked to attach to a page would be asked in the wrong
    /// place. The new session has no domain enabled, so nothing is said on it
    /// before its first command and nothing is lost in the moment between the
    /// engine's reply and the mailbox being registered; enabling `Page` is
    /// the caller's first command.
    pub fn attach(&mut self, target: &str, timeout: Duration) -> Result<Client, String> {
        if self.session.is_some() {
            return Err("only the browser's own client can attach to a page".to_string());
        }
        let reply = self.call_within(
            "Target.attachToTarget",
            Json::object(vec![
                ("targetId", Json::string(target)),
                ("flatten", Json::Bool(true)),
            ]),
            timeout,
        )?;
        let session = reply
            .get("sessionId")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("the engine attached to {target} and gave no session"))?
            .to_string();
        Client::on(&self.exchange, Some(session.clone())).inspect_err(|_| {
            // A session nobody will ever read is a page the engine keeps a
            // debugger attached to for nothing.
            let _ = self.notify(
                "Target.detachFromTarget",
                Json::object(vec![("sessionId", Json::string(&session))]),
            );
        })
    }

    /// The session this client speaks on; `None` for the browser.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// A handle another thread may send notifications on. See [`Notifier`].
    pub fn notifier(&self) -> Notifier {
        Notifier {
            exchange: Arc::clone(&self.exchange),
            session: self.session.clone(),
        }
    }

    /// The descriptor to poll alongside the terminal.
    pub fn wake_fd(&self) -> RawFd {
        self.wake_read
    }

    /// Empty the wake pipe. What it held is only ever "look in the mailbox".
    pub fn drain_wake(&self) {
        let mut buf = [0u8; 256];
        // SAFETY: reads at most `buf.len()` bytes into `buf`, a live local of
        // exactly that size. The descriptor is non-blocking, so the loop ends
        // on `EAGAIN` rather than waiting for a byte that is not coming, and
        // it stays open while this client holds the mailbox that owns it.
        while unsafe {
            libc::read(
                self.wake_read,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        } > 0
        {}
    }

    /// Send a command and wait for its reply.
    pub fn call(&mut self, method: &str, params: Json) -> Result<Json, String> {
        self.call_within(method, params, CALL_TIMEOUT)
    }

    /// The same, with a deadline of the caller's choosing.
    pub fn call_within(
        &mut self,
        method: &str,
        params: Json,
        timeout: Duration,
    ) -> Result<Json, String> {
        let id = self.exchange.next_id();
        let message = message(id, self.session.as_deref(), method, params);

        self.want(id)?;
        if let Err(why) = self.exchange.transmit(&message) {
            self.forget(id);
            return Err(format!("cannot send {method}: {why}"));
        }

        let deadline = Instant::now() + timeout;
        let (lock, signal) = &*self.mailbox;
        let mut mailbox = lock
            .lock()
            .map_err(|_| "the connection is poisoned".to_string())?;
        loop {
            if let Some(reply) = mailbox.take(id) {
                return outcome(method, &reply);
            }
            if let Some(ended) = mailbox.ended.clone() {
                mailbox.forget(id);
                return Err(format!("{method}: {ended}"));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // Nothing is coming that anybody will read. A reply after this
                // is the engine answering a question that has been withdrawn.
                mailbox.forget(id);
                return Err(format!(
                    "{method}: no answer in {} seconds",
                    timeout.as_secs()
                ));
            }
            let (next, _) = signal
                .wait_timeout(mailbox, left)
                .map_err(|_| "the connection is poisoned".to_string())?;
            mailbox = next;
        }
    }

    /// Send a command and come back for the reply later.
    ///
    /// [`Client::call`] is the right shape for most of a person's critical
    /// path — a history entry, a reload — because there is nothing useful to
    /// do until the engine has answered. It is the wrong shape for the
    /// lossless still: that is tens of milliseconds of engine at a pane's
    /// size, and a loop that sits in `call` for them is a loop that is not
    /// reading the terminal. And it is the wrong shape for `Page.navigate`,
    /// whose reply the engine holds until a "leave this page?" is answered —
    /// a loop waiting for it could never show the question. So those go out
    /// here and the reply is collected by [`Client::take_reply`] on whichever
    /// pass it has arrived on.
    ///
    /// Nothing new is needed underneath: a reply is filed in the mailbox under
    /// its own id by the same reader thread, and the same wake pipe knocks for
    /// it, so a `poll` that came back for an event comes back for this too.
    ///
    /// The returned [`Pending`] is the claim on that reply, and dropping it
    /// withdraws the claim: a caller that stops caring — the tab was switched
    /// away from, the still took too long — has only to let it go.
    pub fn send(&mut self, method: &str, params: Json) -> Result<Pending, String> {
        let id = self.exchange.next_id();
        let message = message(id, self.session.as_deref(), method, params);
        self.want(id)?;
        if let Err(why) = self.exchange.transmit(&message) {
            self.forget(id);
            return Err(format!("cannot send {method}: {why}"));
        }
        Ok(Pending {
            id,
            method: method.to_string(),
            mailbox: Arc::clone(&self.mailbox),
        })
    }

    /// The reply to a [`Client::send`], if it has come.
    ///
    /// `None` is "not yet, ask again" and nothing else: the caller keeps the
    /// [`Pending`] and its own deadline, because how long a command is worth
    /// waiting for is the caller's decision rather than this module's. A
    /// session that has ended answers straight away, with why, so that a
    /// caller is never left asking a page that is gone.
    pub fn take_reply(&self, pending: &Pending) -> Option<Result<Json, String>> {
        let (lock, _) = &*self.mailbox;
        let mut mailbox = lock.lock().ok()?;
        if let Some(reply) = mailbox.take(pending.id) {
            return Some(outcome(&pending.method, &reply));
        }
        let ended = mailbox.ended.clone()?;
        // An ended session answers nothing further, so the wait ends here
        // rather than at the drop.
        mailbox.forget(pending.id);
        Some(Err(format!("{}: {ended}", pending.method)))
    }

    /// Send a command and do not wait for its reply.
    ///
    /// For the acknowledgement of a screencast frame, which has to happen
    /// sixty times a second and whose reply says nothing: waiting for it would
    /// put a round trip between every pair of frames.
    ///
    /// The id is spent and never registered, so the reply Chromium sends all
    /// the same is read off the pipe and dropped. This is the command that
    /// runs all day, and nothing it does is kept.
    pub fn notify(&mut self, method: &str, params: Json) -> Result<(), String> {
        let message = message(
            self.exchange.next_id(),
            self.session.as_deref(),
            method,
            params,
        );
        self.exchange
            .transmit(&message)
            .map_err(|why| format!("cannot send {method}: {why}"))
    }

    /// Register an id as one whose reply is going to be collected.
    fn want(&self, id: i64) -> Result<(), String> {
        let (lock, _) = &*self.mailbox;
        let mut mailbox = lock
            .lock()
            .map_err(|_| "the connection is poisoned".to_string())?;
        mailbox.want(id);
        Ok(())
    }

    /// Give an id back: nobody is coming for its reply after all.
    fn forget(&self, id: i64) {
        let (lock, _) = &*self.mailbox;
        if let Ok(mut mailbox) = lock.lock() {
            mailbox.forget(id);
        }
    }

    /// How many replies the mailbox is holding for somebody to collect.
    ///
    /// A client with nothing in flight holds none, however long it has been
    /// running and however many frames it has acknowledged. That is the whole
    /// claim this module makes about its memory, so it is worth being able to
    /// ask.
    pub fn replies_held(&self) -> usize {
        let (lock, _) = &*self.mailbox;
        lock.lock()
            .map(|mailbox| mailbox.replies.len())
            .unwrap_or(0)
    }

    /// How many commands are still expecting a reply to be kept for them.
    pub fn replies_wanted(&self) -> usize {
        let (lock, _) = &*self.mailbox;
        lock.lock().map(|mailbox| mailbox.wanted.len()).unwrap_or(0)
    }

    /// Take every event that has arrived since the last time.
    pub fn events(&self) -> Vec<Event> {
        let (lock, _) = &*self.mailbox;
        match lock.lock() {
            Ok(mut mailbox) => mailbox.events.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The reason the session ended, if it has.
    pub fn ended(&self) -> Option<String> {
        let (lock, _) = &*self.mailbox;
        lock.lock().ok().and_then(|mailbox| mailbox.ended.clone())
    }

    /// Let go of the session.
    ///
    /// A page's client detaches from its target first, unless the engine has
    /// already said it is detached, so that the engine is not left keeping a
    /// debugger's view of a page nobody is reading. The detach is said at
    /// browser level — it is the browser's command about a session, not the
    /// session's about itself — and its reply is not waited for. The browser's
    /// own client has nothing to detach from.
    ///
    /// Either way the mailbox stops being delivered to, and the client is
    /// ended, so a call made after this fails at once rather than waiting for
    /// a reply nothing will file. Closing twice is closing once.
    pub fn close(&mut self) {
        if std::mem::replace(&mut self.closed, true) {
            return;
        }
        if let Some(session) = &self.session {
            if self.ended().is_none() {
                let message = message(
                    self.exchange.next_id(),
                    None,
                    "Target.detachFromTarget",
                    Json::object(vec![("sessionId", Json::string(session))]),
                );
                let _ = self.exchange.transmit(&message);
            }
        }
        self.exchange.deregister(&self.session, &self.mailbox);
        end(&self.mailbox, "the session was closed from this end");
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}

/// One command on the wire: the id it will be answered under, the session it
/// is for, and what it asks. The browser's own commands carry no session.
fn message(id: i64, session: Option<&str>, method: &str, params: Json) -> String {
    let mut fields = vec![("id", Json::number(id as f64))];
    if let Some(session) = session {
        fields.push(("sessionId", Json::string(session)));
    }
    fields.push(("method", Json::string(method)));
    fields.push(("params", params));
    Json::object(fields).to_string()
}

/// What one reply means: the result, or the engine's refusal in words.
///
/// The words are plain text ([`crate::text::sanitize`]): a refusal can quote
/// what it refused — a url, a selector — and it ends up as a note on the row
/// or a line printed into the shell. The method name is this program's own.
fn outcome(method: &str, reply: &Json) -> Result<Json, String> {
    if let Some(error) = reply.get("error") {
        let what = crate::text::sanitize(
            error
                .get("message")
                .and_then(Json::as_str)
                .unwrap_or("the engine refused it"),
        );
        return Err(format!("{method}: {what}"));
    }
    Ok(reply.get("result").cloned().unwrap_or(Json::Null))
}

/// Put one message where it belongs in its session's mailbox.
///
/// A message that is neither — an object with no `id` and no `method` — is
/// dropped. There is nothing useful to do with it and a pipe is not worth
/// ending over one. (Text that is not JSON never gets this far: [`route`]
/// parses once, for the session and for this.)
fn sort(mailbox: &mut Mailbox, value: &Json) {
    if let Some(id) = value.get("id").and_then(Json::as_i64) {
        // The one lookup that stands between an hour of browsing and a map of
        // hundreds of thousands of answers to questions nobody asked.
        if mailbox.wanted.contains(&id) {
            mailbox.replies.insert(id, value.clone());
        }
        return;
    }
    if let Some(method) = value.get("method").and_then(Json::as_str) {
        let event = Event {
            method: method.to_string(),
            params: value.get("params").cloned().unwrap_or(Json::Null),
        };
        // A page that is repainting can produce events faster than a pane can
        // draw them. The queue is bounded so that a slow frame cannot become
        // unbounded memory; what goes is the oldest, because the newest frame
        // is the one worth having.
        if mailbox.events.len() >= 512 {
            mailbox.events.pop_front();
        }
        mailbox.events.push_back(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::io::IntoRawFd;

    /// Sort a message the test wrote as text, the way [`route`] would once it
    /// had parsed it.
    fn sort_text(mailbox: &mut Mailbox, text: &str) {
        if let Ok(value) = Json::parse(text) {
            sort(mailbox, &value);
        }
    }

    /// A mailbox and a [`Pending`] on it, as `Client::send` would leave them.
    fn awaiting(id: i64, method: &str) -> (Slot, Pending) {
        let mailbox: Slot = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
        mailbox.0.lock().expect("a mailbox").want(id);
        let pending = Pending {
            id,
            method: method.to_string(),
            mailbox: Arc::clone(&mailbox),
        };
        (mailbox, pending)
    }

    /// How much the mailbox is holding on to: replies, and claims on them.
    fn held(mailbox: &Slot) -> (usize, usize) {
        let mailbox = mailbox.0.lock().expect("a mailbox");
        (mailbox.replies.len(), mailbox.wanted.len())
    }

    #[test]
    fn a_reply_goes_to_the_call_that_asked_and_an_event_to_the_queue() {
        let mut mailbox = Mailbox::default();
        mailbox.want(4);
        sort_text(&mut mailbox, r#"{"id":4,"result":{"frameId":"A"}}"#);
        sort_text(
            &mut mailbox,
            r#"{"method":"Page.screencastFrame","params":{"data":"x","sessionId":9}}"#,
        );
        sort_text(&mut mailbox, r#"{"neither":true}"#);

        assert_eq!(
            mailbox.take(4).and_then(|r| r
                .path(&["result", "frameId"])
                .and_then(Json::as_str)
                .map(str::to_string)),
            Some("A".to_string())
        );
        assert_eq!(mailbox.events.len(), 1);
        assert_eq!(mailbox.events[0].method, "Page.screencastFrame");
        assert_eq!(
            mailbox.events[0]
                .params
                .get("sessionId")
                .and_then(Json::as_i64),
            Some(9)
        );
    }

    #[test]
    fn the_event_queue_drops_the_oldest_rather_than_growing_forever() {
        let mut mailbox = Mailbox::default();
        for i in 0..600 {
            sort_text(
                &mut mailbox,
                &format!(r#"{{"method":"Page.screencastFrame","params":{{"n":{i}}}}}"#),
            );
        }
        assert_eq!(mailbox.events.len(), 512);
        assert_eq!(
            mailbox.events[0].params.get("n").and_then(Json::as_i64),
            Some(88),
            "the newest frames are the ones kept"
        );
    }

    #[test]
    fn an_error_reply_is_an_error_and_not_a_result() {
        let mut mailbox = Mailbox::default();
        mailbox.want(1);
        sort_text(
            &mut mailbox,
            r#"{"id":1,"error":{"code":-32000,"message":"Cannot navigate to invalid URL"}}"#,
        );
        let reply = mailbox.take(1).expect("a reply");
        assert_eq!(
            reply.path(&["error", "message"]).and_then(Json::as_str),
            Some("Cannot navigate to invalid URL")
        );
    }

    #[test]
    fn an_engines_refusal_is_plain_text() {
        let reply = Json::parse(
            r#"{"id":1,"error":{"code":-32000,"message":"Cannot \u001b[2J navigate"}}"#,
        )
        .expect("the test's own JSON");
        assert_eq!(
            outcome("Page.navigate", &reply),
            Err("Page.navigate: Cannot [2J navigate".to_string())
        );
        let bare = Json::parse(r#"{"id":1,"error":{}}"#).expect("the test's own JSON");
        assert_eq!(
            outcome("Page.navigate", &bare),
            Err("Page.navigate: the engine refused it".to_string())
        );
    }

    #[test]
    fn nothing_is_kept_for_a_command_nobody_will_come_back_for() {
        let mut mailbox = Mailbox::default();
        // What `Client::notify` leaves behind: an id spent and never claimed.
        sort_text(&mut mailbox, r#"{"id":7,"result":{}}"#);
        assert!(
            mailbox.replies.is_empty(),
            "the reply to a notification was filed"
        );
        assert!(mailbox.wanted.is_empty());
    }

    #[test]
    fn a_call_that_asked_still_gets_its_reply() {
        let mut mailbox = Mailbox::default();
        mailbox.want(3);
        sort_text(&mut mailbox, r#"{"id":3,"result":{"data":"png"}}"#);
        let reply = mailbox.take(3).expect("the reply the call asked for");
        assert_eq!(
            reply.path(&["result", "data"]).and_then(Json::as_str),
            Some("png")
        );
        assert_eq!(
            (mailbox.replies.len(), mailbox.wanted.len()),
            (0, 0),
            "taking a reply ends the waiting for it"
        );
    }

    #[test]
    fn a_pending_dropped_before_its_reply_leaves_nothing_behind() {
        let (mailbox, pending) = awaiting(11, "Page.captureScreenshot");
        drop(pending);
        // The tab was switched away from; the engine answers all the same.
        {
            let mut inner = mailbox.0.lock().expect("a mailbox");
            sort_text(&mut inner, r#"{"id":11,"result":{"data":"a megabyte"}}"#);
        }
        assert_eq!(held(&mailbox), (0, 0));
    }

    #[test]
    fn a_pending_dropped_after_its_reply_takes_the_reply_with_it() {
        let (mailbox, pending) = awaiting(12, "Page.captureScreenshot");
        {
            let mut inner = mailbox.0.lock().expect("a mailbox");
            sort_text(&mut inner, r#"{"id":12,"result":{"data":"a megabyte"}}"#);
        }
        assert_eq!(held(&mailbox), (1, 1), "the reply was wanted when it came");
        drop(pending);
        assert_eq!(held(&mailbox), (0, 0));
    }

    #[test]
    fn ten_thousand_acknowledgements_leave_an_empty_mailbox() {
        // Three minutes of screencast at sixty frames a second.
        let mut mailbox = Mailbox::default();
        for id in 1..=10_000 {
            sort_text(&mut mailbox, &format!(r#"{{"id":{id},"result":{{}}}}"#));
        }
        assert_eq!(mailbox.replies.len(), 0);
        assert_eq!(mailbox.wanted.len(), 0);
    }

    #[test]
    fn a_still_among_the_acknowledgements_is_the_one_thing_kept() {
        let (mailbox, pending) = awaiting(5_000, "Page.captureScreenshot");
        {
            let mut inner = mailbox.0.lock().expect("a mailbox");
            for id in 1..=10_000 {
                sort_text(
                    &mut inner,
                    &format!(r#"{{"id":{id},"result":{{"n":{id}}}}}"#),
                );
            }
            assert_eq!(
                inner.replies.len(),
                1,
                "only the one reply somebody asked for"
            );
            assert_eq!(
                inner
                    .take(5_000)
                    .and_then(|r| r.path(&["result", "n"]).and_then(Json::as_i64)),
                Some(5_000)
            );
        }
        drop(pending);
        assert_eq!(held(&mailbox), (0, 0));
    }

    // ---------------------------------------------------------------------
    // The framing
    // ---------------------------------------------------------------------

    #[test]
    fn a_message_that_arrives_in_two_reads_is_one_message() {
        let mut buf = br#"{"id":1,"res"#.to_vec();
        assert_eq!(split_messages(&mut buf), Ok(vec![]), "not ended yet");
        assert_eq!(buf, br#"{"id":1,"res"#, "and kept for the next read");
        buf.extend_from_slice(b"ult\":{}}\0{\"id\":2");
        assert_eq!(
            split_messages(&mut buf),
            Ok(vec![r#"{"id":1,"result":{}}"#.to_string()])
        );
        assert_eq!(buf, br#"{"id":2"#, "the start of the next one stays");
    }

    #[test]
    fn two_messages_in_one_read_are_two_messages() {
        let mut buf = b"{\"a\":1}\0{\"b\":2}\0".to_vec();
        assert_eq!(
            split_messages(&mut buf),
            Ok(vec![r#"{"a":1}"#.to_string(), r#"{"b":2}"#.to_string()])
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn an_empty_message_is_skipped_and_a_message_that_is_not_text_is_dropped() {
        let mut buf = b"\0\0{\"a\":1}\0\xff\xfe\0{\"b\":2}\0".to_vec();
        assert_eq!(
            split_messages(&mut buf),
            Ok(vec![r#"{"a":1}"#.to_string(), r#"{"b":2}"#.to_string()])
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn sixteen_megabytes_with_no_end_is_an_error_and_not_an_allocation() {
        let mut buf = vec![b'x'; MAX_MESSAGE + 1];
        assert!(split_messages(&mut buf).is_err());
        let mut buf = vec![b'x'; MAX_MESSAGE];
        assert_eq!(
            split_messages(&mut buf),
            Ok(vec![]),
            "exactly the ceiling is still a message arriving"
        );
    }

    // ---------------------------------------------------------------------
    // The routing
    // ---------------------------------------------------------------------

    fn slot() -> Slot {
        Arc::new((Mutex::new(Mailbox::default()), Condvar::new()))
    }

    fn methods(slot: &Slot) -> Vec<String> {
        let mailbox = slot.0.lock().expect("a mailbox");
        mailbox.events.iter().map(|e| e.method.clone()).collect()
    }

    fn ended_of(slot: &Slot) -> Option<String> {
        slot.0.lock().expect("a mailbox").ended.clone()
    }

    #[test]
    fn a_sessions_message_goes_to_that_session_and_nowhere_else() {
        let (browser, page, other) = (slot(), slot(), slot());
        let mut mailboxes = HashMap::new();
        mailboxes.insert(None, Arc::clone(&browser));
        mailboxes.insert(Some("S1".to_string()), Arc::clone(&page));
        mailboxes.insert(Some("S2".to_string()), Arc::clone(&other));
        page.0.lock().expect("a mailbox").want(5);

        route(
            &mailboxes,
            r#"{"id":5,"sessionId":"S1","result":{"ok":true}}"#,
        );
        route(
            &mailboxes,
            r#"{"method":"Page.loadEventFired","sessionId":"S1","params":{}}"#,
        );
        route(
            &mailboxes,
            r#"{"method":"Target.targetCreated","params":{"targetInfo":{}}}"#,
        );
        // A session nobody holds, and something that is not JSON at all.
        route(
            &mailboxes,
            r#"{"method":"Page.loadEventFired","sessionId":"S9","params":{}}"#,
        );
        route(&mailboxes, "not json at all");

        assert!(page.0.lock().expect("a mailbox").take(5).is_some());
        assert_eq!(methods(&page), ["Page.loadEventFired"]);
        assert_eq!(methods(&browser), ["Target.targetCreated"]);
        assert!(methods(&other).is_empty(), "S2 heard somebody else's news");
    }

    #[test]
    fn the_screencasts_own_session_number_is_not_a_route() {
        // `params.sessionId` is the screencast's integer; the route is the
        // string at the top. A frame with no top-level session is the browser's.
        let (browser, page) = (slot(), slot());
        let mut mailboxes = HashMap::new();
        mailboxes.insert(None, Arc::clone(&browser));
        mailboxes.insert(Some("S1".to_string()), Arc::clone(&page));
        route(
            &mailboxes,
            r#"{"method":"Page.screencastFrame","sessionId":"S1","params":{"sessionId":3,"data":"x"}}"#,
        );
        assert_eq!(methods(&page), ["Page.screencastFrame"]);
        assert!(methods(&browser).is_empty());
    }

    #[test]
    fn a_detach_ends_the_session_it_names_and_is_still_the_browsers_news() {
        let (browser, page, other) = (slot(), slot(), slot());
        let mut mailboxes = HashMap::new();
        mailboxes.insert(None, Arc::clone(&browser));
        mailboxes.insert(Some("S1".to_string()), Arc::clone(&page));
        mailboxes.insert(Some("S2".to_string()), Arc::clone(&other));
        route(
            &mailboxes,
            r#"{"method":"Target.detachedFromTarget","params":{"sessionId":"S1","targetId":"T1"}}"#,
        );
        assert_eq!(
            ended_of(&page).as_deref(),
            Some("the engine detached from the target")
        );
        assert_eq!(ended_of(&other), None);
        assert_eq!(ended_of(&browser), None);
        assert_eq!(methods(&browser), ["Target.detachedFromTarget"]);
    }

    #[test]
    fn a_command_names_its_session_and_the_browsers_names_none() {
        let page = message(3, Some("S1"), "Page.enable", Json::empty());
        let parsed = Json::parse(&page).expect("JSON");
        assert_eq!(parsed.get("sessionId").and_then(Json::as_str), Some("S1"));
        assert_eq!(parsed.get("id").and_then(Json::as_i64), Some(3));
        assert_eq!(
            parsed.get("method").and_then(Json::as_str),
            Some("Page.enable")
        );

        let browser = message(4, None, "Browser.getVersion", Json::empty());
        let parsed = Json::parse(&browser).expect("JSON");
        assert_eq!(parsed.get("sessionId"), None);
        assert_eq!(parsed.get("id").and_then(Json::as_i64), Some(4));
    }

    // ---------------------------------------------------------------------
    // An exchange over a pipe the test is the other end of
    // ---------------------------------------------------------------------

    /// An exchange, and the engine's two ends for the test to play it with:
    /// where the commands arrive, and where the replies go.
    fn exchange() -> (Arc<Exchange>, std::io::PipeReader, std::io::PipeWriter) {
        let (commands, ours_write) = std::io::pipe().expect("a pipe");
        let (ours_read, replies) = std::io::pipe().expect("a pipe");
        let exchange = Exchange::over(ours_read.into_raw_fd(), ours_write.into_raw_fd());
        (exchange, commands, replies)
    }

    /// One command off the pipe, as the engine would read it.
    fn next_command(commands: &mut std::io::PipeReader) -> Json {
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

    fn poll_one(fd: RawFd, ms: i32) -> bool {
        tty::poll_readable(&[fd], ms).is_ok_and(|ready| ready.contains(&fd))
    }

    #[test]
    fn a_call_on_the_pipe_is_answered_by_the_reply_with_its_id() {
        let (exchange, mut commands, mut replies) = exchange();
        let engine = std::thread::spawn(move || {
            let command = next_command(&mut commands);
            let id = command.get("id").and_then(Json::as_i64).expect("an id");
            assert_eq!(
                command.get("method").and_then(Json::as_str),
                Some("Browser.getVersion")
            );
            assert_eq!(command.get("sessionId"), None, "the browser's own");
            replies
                .write_all(format!("{{\"id\":{id},\"result\":{{\"ok\":true}}}}\0").as_bytes())
                .expect("the reply goes");
            (commands, replies)
        });
        let mut browser = Client::browser(&exchange).expect("a browser client");
        let answer = browser
            .call_within("Browser.getVersion", Json::empty(), Duration::from_secs(5))
            .expect("an answer");
        assert_eq!(answer.get("ok").and_then(Json::as_bool), Some(true));
        let _ends = engine.join().expect("the engine side");
        drop(browser);
        exchange.shutdown();
    }

    #[test]
    fn an_event_knocks_on_the_wake_pipe_and_draining_empties_it() {
        let (exchange, _commands, mut replies) = exchange();
        let browser = Client::browser(&exchange).expect("a browser client");
        assert!(!poll_one(browser.wake_fd(), 0), "nothing has happened yet");
        replies
            .write_all(b"{\"method\":\"Target.targetCreated\",\"params\":{}}\0")
            .expect("the event goes");
        assert!(poll_one(browser.wake_fd(), 2000), "the loop was not woken");
        browser.drain_wake();
        assert!(!poll_one(browser.wake_fd(), 0), "the pipe was not emptied");
        assert_eq!(browser.events().len(), 1);
        drop(browser);
        exchange.shutdown();
    }

    #[test]
    fn when_the_engine_closes_its_end_every_client_hears_and_nothing_waits() {
        let (exchange, commands, replies) = exchange();
        let mut browser = Client::browser(&exchange).expect("a browser client");
        let page = Client::on(&exchange, Some("S1".to_string())).expect("a page client");
        drop(replies);

        let deadline = Instant::now() + Duration::from_secs(2);
        while (browser.ended().is_none() || page.ended().is_none()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(browser.ended().is_some(), "the browser was not told");
        assert!(page.ended().is_some(), "the page was not told");
        assert!(poll_one(page.wake_fd(), 0), "and nobody was woken to look");

        let started = Instant::now();
        assert!(browser
            .call_within("Browser.getVersion", Json::empty(), Duration::from_secs(5))
            .is_err());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a call on an ended pipe waited {:?}",
            started.elapsed()
        );
        assert!(
            Client::browser(&exchange).is_err(),
            "a client made after the end is told at once"
        );

        drop(page);
        drop(browser);
        drop(commands);
        exchange.shutdown();
    }

    #[test]
    fn a_notification_after_the_shutdown_is_a_sentence_and_not_a_write() {
        let (exchange, _commands, _replies) = exchange();
        let browser = Client::browser(&exchange).expect("a browser client");
        let notifier = browser.notifier();
        exchange.shutdown();
        let sent = notifier.notify("Input.dispatchMouseEvent", Json::empty());
        assert!(
            sent.as_ref().is_err_and(|why| why.contains("closed")),
            "{sent:?}"
        );
    }

    #[test]
    fn only_the_browser_attaches_and_only_one_browser_at_a_time() {
        let (exchange, _commands, _replies) = exchange();
        let browser = Client::browser(&exchange).expect("a browser client");
        assert!(
            Client::browser(&exchange).is_err(),
            "two clients for the browser's messages"
        );
        let mut page = Client::on(&exchange, Some("S1".to_string())).expect("a page client");
        assert_eq!(page.session(), Some("S1"));
        let refused = page.attach("T2", Duration::from_secs(1));
        assert!(refused.is_err(), "a page attached to a page");

        // The browser's client going is what lets the next one be made.
        drop(browser);
        let again = Client::browser(&exchange).expect("a browser client after the first");
        drop(again);
        drop(page);
        exchange.shutdown();
    }

    #[test]
    fn shutting_down_joins_the_reader_within_a_second() {
        let (exchange, _commands, _replies) = exchange();
        // The test still holds the engine's writing end, so the reader sees no
        // end of file: it has to notice the stop on its own.
        let started = Instant::now();
        exchange.shutdown();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the reader took {:?} to stop",
            started.elapsed()
        );
        exchange.shutdown();
    }
}
