//! The Chromium that does the rendering, as a child process.
//!
//! tOS does not ship a browser engine and this crate does not contain one: the
//! person installs a Chromium and `blinkterm` drives it. So the first thing
//! this program does is find one, and the second is start it in a way that
//! cannot leave it running after the pane is gone.
//!
//! # The flags are not decoration
//!
//! `--headless --disable-gpu --ozone-platform=headless` is the combination
//! that was measured to work. The third is the one that looks redundant and is
//! not: a Chromium started headless *without* an ozone platform opens its
//! debugging endpoint, accepts a connection, and then never answers on it — no
//! error, no output, no exit. That failure is why every wait in this program
//! has a deadline, and why the flags are written here as a constant rather
//! than assembled from options.
//!
//! # The group, and why not the pid
//!
//! What is started here is usually not a browser. On Debian — which is what a
//! tOS rootfs is — `/usr/bin/chromium-shell` and `/usr/bin/chromium` are shell
//! scripts that run the real binary under `/usr/lib/chromium/` as a child and
//! wait for it, and `/usr/bin/google-chrome` is a wrapper too. So the pid this
//! program gets back from `spawn` is `/bin/sh`, and a `kill(2)` on it takes
//! the shell and leaves the browser: reparented to init, still holding its
//! debugging endpoint, still painting the page it had. Seven sessions of that on
//! an installed machine left seven engines nobody was looking at, between them
//! keeping two processors busy.
//!
//! So the engine is started in a process group of its own and every kill here
//! signals the *group*: the wrapper, the browser it ran, and the zygote, gpu
//! and renderer processes the browser forked, all of which inherit the group
//! and none of which this program otherwise knows the pid of.
//!
//! `--disable-dev-shm-usage` keeps the engine off the same `/dev/shm` the
//! frames go through. `--no-sandbox` only when this program is root, because
//! Chromium refuses to start as root without it and adding it as anyone else
//! would be turning off a protection that was working. Root also gets a
//! sentence on stderr about it: it is the one flag here that takes a defence
//! away, and it should not be added on the person's behalf in silence.
//!
//! # A pipe, because a port is everybody's
//!
//! `--remote-debugging-pipe` rather than `--remote-debugging-port`, and the
//! difference is who else can drive the browser. A port on 127.0.0.1 is open
//! to every process on the machine, whoever runs it, and a browser driven from
//! it can be made to read any page this one has open and type into any form.
//! This used to ask for `--remote-debugging-port=0` and
//! `--remote-allow-origins=*`, and said here that the second was needed
//! because a WebSocket handshake without an `Origin` is checked against a list
//! that is empty by default. That was never true. Measured against
//! `headless_shell` 141.0.7390.37 without the flag, a handshake with no
//! `Origin` at all — which is what this program sent — is answered `101`, and
//! only one naming an origin such as `http://evil.example` gets `403`. So the
//! flag was letting any web page in the browser that could guess the port
//! talk to it, and removing it would have closed that and left every local
//! process exactly as able to attach as before.
//!
//! With the pipe there is nothing to attach to. The engine reads commands on
//! its descriptor 3 and writes replies and events on its descriptor 4, which
//! it inherits from this program and nobody else has; the whole group — six
//! processes, measured — holds no listening socket at all, and there is no
//! `DevTools listening on` line to wait for. The engine is ready when it
//! answers `Browser.getVersion` on the pipe, which is asked the moment it has
//! been started and is the first thing [`Engine::launch`] waits for.
//!
//! `wire` is how the two descriptors get to be 3 and 4, and every step of it
//! is there because the obvious version was wrong.

use std::io::{BufRead, BufReader};
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cdp::{Client, Exchange};
use crate::json::Json;
use crate::profile::Profile;

/// The engines that are looked for, in the order they are looked for.
///
/// `chromium-shell` first: it is Debian's `headless_shell`, the one this was
/// measured against, and the one with no window system code in it at all.
pub const CANDIDATES: [&str; 4] = [
    "chromium-shell",
    "chromium",
    "chromium-browser",
    "google-chrome",
];

/// The environment variable that overrides the search.
pub const ENGINE_ENV: &str = "BLINKTERM_ENGINE";

/// How many lines of the engine's stderr are kept to explain a death: the
/// first `HEAD` and the last `TAIL`, with whatever came between dropped.
///
/// Both ends, because Chromium says why it is dying at the top — one
/// `FATAL:` line, or "No usable sandbox!" — and then prints a stack trace and
/// a register dump some twenty lines long. A tail alone keeps the registers
/// and loses the sentence, which is exactly what happened the first time this
/// ran on a machine whose Chromium could not start.
const HEAD: usize = 8;
const TAIL: usize = 12;

/// What to kill to stop the engine, for the paths that cannot run a
/// destructor.
///
/// A panic in a release build aborts — the release profile sets
/// `panic = "abort"` —
/// so `Drop` is not a way to be sure the child dies. This is read by the panic
/// hook and by the signal path, both of which have to kill a process without
/// owning anything.
///
/// It is written the way `kill(2)` wants it: the engine's process group,
/// negated, or the wrapper's bare pid if a group of its own could not be made.
/// Zero when there is nothing to kill.
static TARGET: AtomicI32 = AtomicI32::new(0);

/// Kill the engine and everything it started, from anywhere, without a `&mut`
/// to it.
///
/// Signal-safe enough for what it is used for: one `kill(2)` on an integer
/// read out of an atomic. The panic hook and the signal path do not go through
/// [`Engine::kill`] and do not close the pipe the browser would take as its
/// cue to close, nor ask it to close, so they take the group with `SIGKILL`.
/// Whatever the engine had not flushed of the profile is lost with it — see
/// [`crate::profile`] for how much — and a temporary profile's directory is
/// [`crate::profile::remove_temp_profile`]'s to take away, not this.
pub fn kill_engine() {
    signal_all(TARGET.swap(0, Ordering::SeqCst), libc::SIGKILL);
}

/// Signal the engine: the whole group where there is one.
///
/// `target` is already in `kill(2)`'s own notation — negative for a group —
/// so this is one `kill(2)` and a check, which is all a signal handler may do.
fn signal_all(target: i32, signal: libc::c_int) {
    // 0 is "this program's own group" and -1 is "every process we are allowed
    // to signal". Either would be this program killing itself, so a target
    // that was never recorded kills nothing.
    if target == 0 || target == -1 {
        return;
    }
    // SAFETY: `kill(2)` takes two integers and reads no memory, so there is
    // nothing here to keep alive or to have got wrong. It is also
    // async-signal-safe, which is the property that matters: this runs from a
    // signal handler. `target` has just been checked for the two values that
    // would aim it at this program itself.
    unsafe {
        libc::kill(target, signal);
    }
}

/// Start `command` as the leader of a new process group, and say what to kill.
///
/// The group is asked for twice on purpose: in the child before `exec`, and
/// again in the parent. Either call alone is a race — the parent can reach the
/// kill before the child has reached `setpgid`, and the child can `exec`
/// before the parent has got round to it — and `setpgid(2)` on a process that
/// already leads its own group changes nothing, so doing both closes the
/// window. The parent's call failing means the child's has already run.
fn spawn_in_own_group(command: &mut Command) -> std::io::Result<(Child, i32)> {
    // SAFETY: `pre_exec` is unsafe because its closure runs in the child
    // between `fork` and `exec`, where a thread that held a lock in the parent
    // will never release it, so only async-signal-safe calls are allowed. This
    // closure calls `setpgid(2)` and reads `errno`, and both of those are.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    let pid = child.id() as i32;
    // SAFETY: two integers and no memory, as above. The result is dropped on
    // purpose rather than missed: failure here is the expected case, and means
    // the child reached its own `setpgid` first.
    unsafe {
        libc::setpgid(pid, pid);
    }
    Ok((child, group_target(pid)))
}

/// A descriptor with the same pipe behind it, numbered 5 or higher, and
/// close-on-exec. The one it was made from is closed as it drops.
///
/// The engine's ends of its two pipes go through this before [`wire`] puts
/// them on 3 and 4, for two reasons that both come from the numbers a fresh
/// process hands out. In a program that has opened nothing yet, the first pipe
/// *is* 3 and 4, and `dup2(3, 3)` is defined to do nothing at all — including
/// not clearing close-on-exec — so the engine would start with no descriptor 3
/// and say "remote debugging pipe file descriptors are not open". And with two
/// pipes the ends can be crossed: were the engine's writing end 3, the
/// `dup2(read, 3)` that comes first would close it before it had been copied
/// to 4. Above 4, neither can happen.
fn above(fd: OwnedFd) -> std::io::Result<OwnedFd> {
    // SAFETY: `fcntl(2)` with `F_DUPFD_CLOEXEC` takes a descriptor and an
    // integer and reads no memory; `fd` is open for the whole call because it
    // is an `OwnedFd` that is not dropped until this returns.
    let moved = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 5) };
    if moved < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `moved` is a descriptor `fcntl` has just made, so it is open and
    // nothing else in this program owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(moved) })
}

/// Give the engine `engine_read` as its descriptor 3 and `engine_write` as 4.
///
/// Both pipes are made close-on-exec (`std::io::pipe` is `pipe2(2)` with
/// `O_CLOEXEC`), so that nothing of them leaks into the engine except the two
/// copies made here: `dup2(2)` clears close-on-exec on the descriptor it
/// makes, which is the whole mechanism — the originals, at 5 and above, close
/// at `exec`, and 3 and 4 survive it. The parent has to close its own copies
/// of the engine's two ends once the engine is started, or it holds the
/// writing end of its own reading pipe and never sees end of file.
///
/// Both have to be above 4 before this runs; see [`above`].
///
/// Rust runs `pre_exec` closures after it has put stdin, stdout and stderr in
/// place, and in the order they were registered, so this is registered before
/// [`spawn_in_own_group`] registers its own. One consequence is worth
/// knowing: the pipe the standard library uses to hear that `exec` failed may
/// itself be numbered 3 or 4 in the child, and a `dup2` onto it closes it —
/// so an `exec` that fails reads here as an engine that started and then did
/// not answer, which [`Engine::launch`] reports all the same. [`locate`]
/// checks that the file is executable first, so that is a rare way to fail.
fn wire(command: &mut Command, engine_read: RawFd, engine_write: RawFd) {
    // SAFETY: the closure runs in the child between `fork` and `exec`, where
    // only async-signal-safe calls are allowed; it makes two `dup2(2)` calls
    // and reads `errno`, all of which are. The two descriptors are integers
    // copied into the closure, and the parent keeps them open until `spawn`
    // has returned, so they are open in the child when it runs.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(engine_read, 3) < 0 || libc::dup2(engine_write, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// `kill(2)`'s argument for everything `pid` leads: `-pid` once `pid` is a
/// group of its own, and `pid` alone if it somehow is not.
///
/// The second case should not happen and is not an error: a program that is
/// running is better stopped by its pid than not at all. It is checked rather
/// than assumed because the number is about to be handed to `kill(2)` with a
/// minus in front of it, and the group this program is in is one of the things
/// that could be on the other end of that.
fn group_target(pid: i32) -> i32 {
    // SAFETY: `getpgid(2)` takes an integer, reads no memory and only reports.
    // Answering -1 for a child that has already gone is handled by the
    // comparison below, which then keeps `pid` rather than negating it.
    let group = unsafe { libc::getpgid(pid) };
    // SAFETY: `getpgrp(2)` takes nothing, reads no memory and cannot fail.
    let ours = unsafe { libc::getpgrp() };
    if group == pid && group != ours {
        -pid
    } else {
        pid
    }
}

/// Whether anything is left of the engine's group.
///
/// Signal 0 asks `kill(2)` whether it could send rather than sending, and
/// `ESRCH` is the answer that the group is empty. A process nobody has waited
/// for is still a member of it, which is why the wrapper is reaped before this
/// is believed.
fn group_alive(target: i32) -> bool {
    if target >= 0 {
        // No group of its own; the child's own exit status is the whole
        // answer, and the caller has it.
        return false;
    }
    // SAFETY: signal 0 sends nothing and only asks whether it could, and
    // `kill(2)` reads no memory. `target` is negative here, checked above.
    if unsafe { libc::kill(target, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Where the engine is, or a sentence about why there is none.
pub fn locate() -> Result<PathBuf, String> {
    if let Some(named) = std::env::var_os(ENGINE_ENV) {
        let path = PathBuf::from(&named);
        if is_executable(&path) {
            return Ok(path);
        }
        if let Some(found) = search_path(&path.to_string_lossy()) {
            return Ok(found);
        }
        return Err(format!(
            "{ENGINE_ENV} names {}, which is not an executable",
            path.display()
        ));
    }
    for candidate in CANDIDATES {
        if let Some(found) = search_path(candidate) {
            return Ok(found);
        }
    }
    Err(format!(
        "no browser engine on PATH: looked for {}; set {ENGINE_ENV} to one",
        CANDIDATES.join(", ")
    ))
}

fn search_path(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let path = PathBuf::from(name);
        return is_executable(&path).then_some(path);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The command line, which is fixed apart from the sandbox.
pub fn flags(as_root: bool) -> Vec<&'static str> {
    let mut flags = vec![
        "--headless",
        "--disable-gpu",
        "--disable-dev-shm-usage",
        "--ozone-platform=headless",
        "--remote-debugging-pipe",
    ];
    if as_root {
        flags.push("--no-sandbox");
    }
    flags.push("about:blank");
    flags
}

/// A running engine, killed when this is dropped and when the program dies.
pub struct Engine {
    child: Child,
    /// What to kill, in `kill(2)`'s notation; see [`TARGET`].
    target: i32,
    /// The pipe, from this side. Every [`Client`] holds it too, and this is
    /// the holder that shuts it.
    exchange: Arc<Exchange>,
    tail: Arc<Mutex<Vec<String>>>,
    /// The `--user-data-dir` it was given. Declared last so that it is dropped
    /// last: [`Engine`]'s own `Drop` kills the engine first, and only then is a
    /// kept profile's lock let go or a temporary one's directory removed.
    profile: Profile,
}

impl Engine {
    /// Start one and wait, for no longer than `timeout`, for it to answer on
    /// its pipe.
    ///
    /// `profile` is where it keeps what it keeps, and is held for as long as
    /// the engine is: see [`crate::profile`] for why the directory is always
    /// named, and why the lock on it is this program's.
    pub fn launch(profile: Profile, timeout: Duration) -> Result<Engine, String> {
        let path = locate()?;
        // SAFETY: `geteuid(2)` takes nothing, reads no memory and cannot fail.
        let as_root = unsafe { libc::geteuid() } == 0;
        if as_root {
            // Said out loud, because `--no-sandbox` is added silently below and
            // it is the one flag here that takes a protection away rather than
            // adding one. The renderer is the part of Chromium that parses
            // what a page sends; the sandbox is what stops a bug in it from
            // being the machine. This runs before `Pane::enter`, so it lands
            // on the ordinary screen rather than under the alternate one.
            eprintln!(
                "blinkterm: running as root, so the engine gets --no-sandbox: \
                 Chromium will not start as root without it."
            );
            eprintln!(
                "blinkterm: that is the sandbox off, on the program that renders \
                 untrusted pages. Run as an ordinary user if you can."
            );
        }

        let piped = |e: std::io::Error| format!("cannot make the engine's pipe: {e}");
        // Commands go down the first; replies and events come up the second.
        let (engine_read, ours_write) = std::io::pipe().map_err(piped)?;
        let (ours_read, engine_write) = std::io::pipe().map_err(piped)?;
        let engine_read = above(engine_read.into()).map_err(piped)?;
        let engine_write = above(engine_write.into()).map_err(piped)?;

        let mut command = Command::new(&path);
        command
            .arg(format!("--user-data-dir={}", profile.dir().display()))
            .args(flags(as_root))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        wire(
            &mut command,
            engine_read.as_raw_fd(),
            engine_write.as_raw_fd(),
        );
        let (mut child, target) = spawn_in_own_group(&mut command)
            .map_err(|e| format!("cannot start {}: {e}", path.display()))?;
        TARGET.store(target, Ordering::SeqCst);
        // The engine has its own copies now. Keeping these would be holding the
        // writing end of the pipe this program reads, which is the difference
        // between an engine that dies being heard as end of file and not.
        drop(engine_read);
        drop(engine_write);

        let stderr = child.stderr.take().ok_or("the engine has no stderr")?;
        let tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let keep = Arc::clone(&tail);
        // A thread rather than a poll, because the lines have to be read as
        // they arrive: a pipe nobody reads fills, and an engine whose stderr is
        // full stops. Nothing in them is waited for any more — the pipe says
        // when the engine is ready — but they are what explains a death.
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Ok(mut tail) = keep.lock() {
                    tail.push(line);
                    if tail.len() > HEAD + TAIL {
                        // The first lines stay; the ring is the rest.
                        tail.remove(HEAD);
                    }
                }
            }
        });

        let exchange = Exchange::over(ours_read.into_raw_fd(), ours_write.into_raw_fd());
        let mut engine = Engine {
            child,
            target,
            exchange,
            tail,
            profile,
        };
        // The first thing asked, and the readiness signal: an engine that is
        // up answers it, and one that died on the way up closes the pipe,
        // which fails the call at once rather than at the deadline.
        let ready = Client::browser(&engine.exchange).and_then(|mut browser| {
            browser.call_within("Browser.getVersion", Json::empty(), timeout)
        });
        if let Err(err) = ready {
            engine.kill();
            // After the kill, so that whatever the engine said on its way out
            // has been read by the time it is quoted.
            let why = describe_tail(&engine.tail);
            return Err(format!(
                "{} did not answer on its debugging pipe within {} seconds ({err}){why}",
                path.display(),
                timeout.as_secs()
            ));
        }
        Ok(engine)
    }

    /// A client for the browser's own messages: the one that opens, closes,
    /// raises and attaches to pages. One at a time; see [`Client::browser`].
    pub fn browser(&self) -> Result<Client, String> {
        Client::browser(&self.exchange)
    }

    /// The profile it was started with.
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// The engine's process group, once it has one of its own.
    ///
    /// `None` means the group could not be made and the wrapper's pid is all
    /// there is to kill, which is the case this module exists to avoid.
    pub fn group(&self) -> Option<i32> {
        (self.target < 0).then_some(-self.target)
    }

    /// The last lines the engine wrote, for an error message.
    pub fn tail(&self) -> Vec<String> {
        self.tail.lock().map(|t| t.clone()).unwrap_or_default()
    }

    /// `Ok` while the engine is running; the reason, with its own last words,
    /// once it is not.
    pub fn check(&mut self) -> Result<(), String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Err(format!(
                "the browser engine exited ({status}){}",
                describe_tail(&self.tail)
            )),
            Ok(None) => Ok(()),
            Err(err) => Err(format!("cannot tell whether the engine is running: {err}")),
        }
    }

    /// Wait, for no longer than `timeout`, for the engine to finish on its
    /// own — after `Browser.close`, which is the only stop that writes the
    /// profile — and say whether it did.
    ///
    /// Nothing is signalled. `false` means it is still running, and
    /// [`Engine::kill`] is what comes next.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        self.gone_by(Instant::now() + timeout)
    }

    /// Stop it, politely and then not — and the group, not the pid.
    ///
    /// This is not how the profile gets written, and it used to say it was:
    /// that Chromium flushes its profile and takes its `SingletonLock` with it
    /// when it is asked to stop. Measured against 141, `SIGTERM` ends the
    /// group in about two seconds and in that time neither writes the cookie
    /// jar nor removes the lock — full Chromium exits 0 while losing the
    /// cookie. The stop that keeps a login is `Browser.close`, and that is
    /// [`crate::app::run`]'s, before this is called; [`crate::profile`] has
    /// the table. On that path the engine has gone by the time this runs and
    /// there is nothing left here to signal.
    ///
    /// So the `SIGTERM` stays for what it is still worth — half a second to
    /// close its files is fewer half-written ones — and the `SIGKILL` after it
    /// is the part that is relied on, because an engine that is only ever
    /// asked can take as long as it likes. Then a temporary profile is
    /// removed, since nothing is left that could write into it.
    ///
    /// Before any signal, the pipe is closed: a browser whose descriptor 3
    /// reaches end of file exits on its own, measured at 14 ms with status 0.
    /// It does not take the whole group with it — two of the six were still
    /// running at that point — so the signals to the group stay, and they are
    /// what this is sure of.
    pub fn kill(&mut self) {
        TARGET.store(0, Ordering::SeqCst);
        self.exchange.shutdown();
        signal_all(self.target, libc::SIGTERM);
        if !self.gone_by(Instant::now() + Duration::from_millis(500)) {
            signal_all(self.target, libc::SIGKILL);
            // The wrapper is this program's child and has to be waited for.
            // The rest of the group are init's children by the time the
            // signal lands, and die on it with nothing here left to reap.
            let _ = self.child.wait();
            self.target = 0;
        }
        self.profile.remove();
    }

    /// Whether the engine and everything it started are gone, waiting until
    /// `deadline` for it.
    ///
    /// The wrapper first — a process nobody has waited for is still a member
    /// of its own group — and then the group it led, which is where the
    /// browser and its renderers are. Once both are gone the target is
    /// forgotten, here and in [`TARGET`], so that a kill after
    /// [`Engine::wait_for_exit`] — or the one in `Drop` after an explicit one
    /// — does not signal a group id the kernel may since have given to
    /// somebody else. [`Engine::group`] reads `None` from then on.
    fn gone_by(&mut self, deadline: Instant) -> bool {
        loop {
            let waited = !matches!(self.child.try_wait(), Ok(None));
            if waited && !group_alive(self.target) {
                let _ = TARGET.compare_exchange(self.target, 0, Ordering::SeqCst, Ordering::SeqCst);
                self.target = 0;
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.kill();
    }
}

fn describe_tail(tail: &Arc<Mutex<Vec<String>>>) -> String {
    let lines = tail.lock().map(|t| t.clone()).unwrap_or_default();
    if lines.is_empty() {
        return String::new();
    }
    format!("; it said: {}", lines.join(" / "))
}

/// The first page target's id, once the engine has one.
///
/// The engine starts with the `about:blank` it was given on its command line,
/// but a browser that has only just answered `Browser.getVersion` may not
/// have made its page yet, so the list is asked for again every 50 ms until
/// there is a page in it or the time is up. It is also the first command that
/// needs the engine to have done anything, so an engine that answers the pipe
/// and then does nothing is caught here, by the deadline.
pub fn first_page_target(browser: &mut Client, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "the engine has no page to drive{}",
                if last.is_empty() {
                    String::new()
                } else {
                    format!(": {last}")
                }
            ));
        }
        match browser.call_within(
            "Target.getTargets",
            Json::empty(),
            left.min(Duration::from_secs(2)),
        ) {
            Ok(reply) => match first_page(&reply) {
                Ok(target) => return Ok(target),
                Err(why) => last = why,
            },
            Err(why) => last = why,
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The first page target's id in a `Target.getTargets` answer.
pub fn first_page(reply: &Json) -> Result<String, String> {
    let targets = reply
        .get("targetInfos")
        .and_then(Json::as_array)
        .ok_or_else(|| "the target list is not a list".to_string())?;
    targets
        .iter()
        .find(|target| target.get("type").and_then(Json::as_str) == Some("page"))
        .and_then(|target| target.get("targetId"))
        .and_then(Json::as_str)
        .map(|id| id.to_string())
        .ok_or_else(|| format!("no page among {} targets", targets.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flags_are_the_ones_that_were_measured() {
        let plain = flags(false);
        assert_eq!(
            plain,
            vec![
                "--headless",
                "--disable-gpu",
                "--disable-dev-shm-usage",
                "--ozone-platform=headless",
                "--remote-debugging-pipe",
                "about:blank",
            ]
        );
        for flags in [flags(false), flags(true)] {
            assert!(
                !flags
                    .iter()
                    .any(|f| f.starts_with("--remote-debugging-port")),
                "a port is open to every process on the machine"
            );
            assert!(
                !flags
                    .iter()
                    .any(|f| f.starts_with("--remote-allow-origins")),
                "and origins are a question only a port asks"
            );
        }
        assert!(!plain.contains(&"--no-sandbox"), "not unless we are root");
        assert!(flags(true).contains(&"--no-sandbox"));
        assert_eq!(
            flags(true).last(),
            Some(&"about:blank"),
            "the url goes last"
        );
    }

    #[test]
    fn a_page_is_picked_out_of_the_target_list() {
        let reply = Json::parse(
            r#"{"targetInfos":[
              {"targetId":"B","type":"browser","title":"","url":""},
              {"targetId":"AB","type":"page","title":"about:blank","url":"about:blank"}
            ]}"#,
        )
        .expect("the test's own JSON");
        assert_eq!(first_page(&reply), Ok("AB".into()));
        let none = Json::parse(r#"{"targetInfos":[]}"#).expect("JSON");
        assert!(first_page(&none).is_err());
        let browser = Json::parse(r#"{"targetInfos":[{"type":"browser"}]}"#).expect("JSON");
        assert!(first_page(&browser).is_err());
        assert!(first_page(&Json::Null).is_err());
    }

    #[test]
    fn a_descriptor_moved_above_four_is_above_four_and_closes_on_exec() {
        let (read, write) = std::io::pipe().expect("a pipe");
        let moved = above(read.into()).expect("a move");
        assert!(moved.as_raw_fd() >= 5, "moved to {}", moved.as_raw_fd());
        // SAFETY: `F_GETFD` reads a descriptor's flags and no memory; `moved`
        // is open, owned by this test, for the whole call.
        let flags = unsafe { libc::fcntl(moved.as_raw_fd(), libc::F_GETFD) };
        assert!(
            flags >= 0 && flags & libc::FD_CLOEXEC != 0,
            "close-on-exec is off"
        );
        drop(write);
    }

    #[test]
    fn a_shell_on_fds_3_and_4_is_reached_through_them() {
        use std::io::{Read, Write};
        // What the engine is to this program, with `cat` standing in for it:
        // commands in on 3, answers out on 4, nothing else inherited.
        let (engine_read, mut ours_write) = std::io::pipe().expect("a pipe");
        let (mut ours_read, engine_write) = std::io::pipe().expect("a pipe");
        let engine_read = above(engine_read.into()).expect("a move");
        let engine_write = above(engine_write.into()).expect("a move");
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("exec cat <&3 >&4")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        wire(
            &mut command,
            engine_read.as_raw_fd(),
            engine_write.as_raw_fd(),
        );
        let (mut child, _) = spawn_in_own_group(&mut command).expect("a shell starts");
        drop(engine_read);
        drop(engine_write);

        ours_write.write_all(b"hello\0").expect("the write");
        drop(ours_write);
        let mut echoed = Vec::new();
        ours_read.read_to_end(&mut echoed).expect("the read");
        assert_eq!(echoed, b"hello\0");
        let status = child.wait().expect("the shell is reaped");
        assert!(status.success(), "{status}");
    }

    #[test]
    fn a_child_leads_a_group_that_is_not_the_one_this_program_is_in() {
        // The shape of the Debian wrapper: a shell that runs something else
        // and waits for it, rather than exec-ing it. The trailing `:` is what
        // stops the shell optimising the wait away and becoming the sleep.
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 30; :")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (mut child, target) = spawn_in_own_group(&mut command).expect("a shell starts");
        let pid = child.id() as i32;
        // SAFETY: reports this program's own group; no arguments, no memory.
        let ours = unsafe { libc::getpgrp() };

        assert_eq!(
            target, -pid,
            "the target is the child's group, kill(2)'s way"
        );
        // SAFETY: as above, and `pid` is the child this test has not yet reaped.
        assert_eq!(unsafe { libc::getpgid(pid) }, pid, "and the child leads it");
        assert_ne!(-target, ours, "a group of its own, not the one we are in");
        assert!(
            group_alive(target),
            "the group has the shell in it at least"
        );

        signal_all(target, libc::SIGKILL);
        let status = child.wait().expect("the shell is reaped");
        assert!(!status.success(), "it was killed, not asked: {status}");
    }

    #[test]
    fn a_target_that_was_never_recorded_kills_nothing() {
        // 0 is this program's own group and -1 is every process it may signal,
        // so either of those reaching `kill(2)` would end this test process
        // and every other test with it. Getting to the end is the assertion.
        signal_all(0, libc::SIGKILL);
        signal_all(-1, libc::SIGKILL);
        assert!(
            !group_alive(0),
            "a pid with no group of its own is not a group"
        );
    }

    #[test]
    fn a_missing_engine_is_a_sentence_that_names_what_was_looked_for() {
        // An override that names nothing, which is the case a person hits
        // after a typo, and the message has to say which variable.
        let failed = with_engine_env(Some("/nonexistent/chromium"), locate).unwrap_err();
        assert!(failed.contains("/nonexistent/chromium"), "{failed}");
        assert!(failed.contains(ENGINE_ENV), "{failed}");
    }

    #[test]
    fn an_override_that_names_a_real_program_is_taken() {
        let found = with_engine_env(Some("/bin/sh"), locate);
        assert_eq!(found.as_deref().map(|p| p.to_str().unwrap()), Ok("/bin/sh"));
    }

    /// Set the variable, run, put it back. The tests that use it are in one
    /// module and the environment is per process, so they are serialised by a
    /// mutex rather than left to race.
    fn with_engine_env<T>(value: Option<&str>, run: impl FnOnce() -> T) -> T {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = std::env::var_os(ENGINE_ENV);
        match value {
            Some(value) => std::env::set_var(ENGINE_ENV, value),
            None => std::env::remove_var(ENGINE_ENV),
        }
        let out = run();
        match before {
            Some(before) => std::env::set_var(ENGINE_ENV, before),
            None => std::env::remove_var(ENGINE_ENV),
        }
        out
    }
}
