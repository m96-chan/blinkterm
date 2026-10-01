//! The ways several terminals on one profile can go wrong (#83, step 5):
//! two starting at once, the first one leaving, a frontend or the backend or
//! the engine killed outright, an attach while the backend is on its way
//! out, a setting that cannot be shared, what a crash leaves in the profile,
//! a path too long for a socket, a terminal that stops reading beside one
//! that keeps up, and the cookie jar at the end of it all.
//!
//! As in `tests/windows.rs`, the backend is the real one, started by
//! [`blinkterm::frontend::attach`] the way the program starts it, and the
//! frontends are fake — a connection each, with made-up terminal sizes and
//! no pty. `tests/terminal.rs` runs the real binary in a pty.
//!
//! Every test here but the long path's runs only when `BLINKTERM_ENGINE`
//! names the engine to use, as `tests/engine.rs` does, and skips with a line
//! saying so otherwise. Run them one at a time (`--test-threads=1`): each
//! starts an engine of its own, and two of them are about time.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use blinkterm::backend;
use blinkterm::engine;
use blinkterm::fit::Metrics;
use blinkterm::frontend::{self, Link, Spawn};
use blinkterm::input::{Input, Key, KeyAction, KeyInput, Mods};
use blinkterm::ipc::{
    self, BrowserSettings, CloseWhy, FrameKind, Open, RouteFlags, ToBackend, ToFrontend,
    WindowSettings,
};
use blinkterm::options::{self, Invocation, Options};

/// A page that moves, so that the engine has frames to send, and that goes
/// to `/key-<key>` on a key, so that the row's url says which key it got.
const PAGE: &str = "<!doctype html><title>ready</title>\
<body style='margin:0;height:100vh;background:#fff'>\
<div id=b style='position:absolute;width:60px;height:30px;background:#c33'></div>\
<script>\
addEventListener('keydown',function(e){location.href='/key-'+e.key});\
var b=document.getElementById('b'),n=0;\
function f(){n=n>300?0:n+3;b.style.left=n+'px';requestAnimationFrame(f)}f();\
</script></body>";

/// Sets a cookie that lasts, and says so in its title.
const SET_COOKIE: &str = "<!doctype html><title>cookie set</title><body>set</body>";

/// Says in its title which cookie it was sent.
const SHOW_COOKIE: &str = "<!doctype html><title>x</title>\
<script>document.title='cookie='+(document.cookie||'none')</script>";

/// How long a test waits for something the engine has to do.
const PATIENCE: Duration = Duration::from_secs(20);

fn engine_named() -> bool {
    if std::env::var_os(engine::ENGINE_ENV).is_none() {
        eprintln!(
            "skipped: {} is not set; name a Chromium to run this against",
            engine::ENGINE_ENV
        );
        return false;
    }
    true
}

/// A scratch directory of this test's own, gone before and after.
struct Scratch(PathBuf);

impl Scratch {
    fn new(what: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "blinkterm-it-hardening-{what}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A local server for the pages: `/set` sets a cookie, `/show` shows it,
/// anything else is [`PAGE`].
fn serve() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to serve on");
    let address = listener.local_addr().expect("an address");
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut head = [0u8; 2048];
            let read = stream.read(&mut head).unwrap_or(0);
            let request = String::from_utf8_lossy(&head[..read]).to_string();
            let (body, extra) = if request.starts_with("GET /set") {
                (
                    SET_COOKIE,
                    "Set-Cookie: kept=yes; Max-Age=86400; Path=/\r\n",
                )
            } else if request.starts_with("GET /show") {
                (SHOW_COOKIE, "")
            } else {
                (PAGE, "")
            };
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n{extra}\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(answer.as_bytes());
        }
    });
    format!("http://{address}/")
}

/// The options a frontend started with `--no-config` and `extra` has.
fn options(extra: &[&str]) -> Options {
    let mut args = vec!["--no-config".to_string()];
    args.extend(extra.iter().map(|a| a.to_string()));
    match options::invocation(&args).expect("options") {
        Invocation::Run(options) => options,
        other => panic!("not a run: {other:?}"),
    }
}

/// The launcher the program uses, starting this build's binary, with the
/// grace a vanished terminal gets.
fn launcher(grace: Duration) -> Spawn {
    Spawn {
        exe: PathBuf::from(env!("CARGO_BIN_EXE_blinkterm")),
        args: vec![
            "--no-config".to_string(),
            "--grace-ms".to_string(),
            grace.as_millis().to_string(),
        ],
        label: None,
        children: Vec::new(),
    }
}

fn metrics(cols: u32, rows: u32) -> Metrics {
    Metrics {
        cols,
        rows,
        cell: (8, 16),
    }
}

/// The `open` a fake frontend sends.
fn an_open(size: (u32, u32), urls: &[String], browser: &Options, paced: bool) -> Open {
    let options = options(&[]);
    Open {
        nonce: ipc::new_nonce(),
        window: WindowSettings::from(&options),
        browser: BrowserSettings::from(browser),
        metrics: metrics(size.0, size.1),
        route: RouteFlags {
            paced,
            png: false,
            keyed: false,
            alpha: None,
            every_nth: 1,
        },
        pixel_mouse: false,
        urls: urls.to_vec(),
        restore: false,
        problems: Vec::new(),
        cwd: std::env::temp_dir(),
        home_dir: None,
    }
}

/// One fake frontend: its link, and what the backend has said to it.
struct Front {
    link: Link,
    text: Vec<u8>,
    frames: usize,
    /// Every frame's sequence number not acknowledged yet, when the
    /// frontend is not acknowledging.
    unacked: Vec<u64>,
    acking: bool,
    closed: Option<(String, u8)>,
}

impl Front {
    /// Attach to the profile at `dir`, starting its backend if need be, and
    /// open a window at `size` on `urls`.
    fn open(spawn: &mut Spawn, dir: &Path, size: (u32, u32), urls: &[String]) -> Front {
        Front::try_open(spawn, dir, an_open(size, urls, &options(&[]), false)).expect("a window")
    }

    fn try_open(spawn: &mut Spawn, dir: &Path, open: Open) -> Result<Front, String> {
        let mut link = frontend::attach(spawn, Some(dir))?;
        link.open(open)?;
        Ok(Front {
            link,
            text: Vec::new(),
            frames: 0,
            unacked: Vec::new(),
            acking: true,
            closed: None,
        })
    }

    /// Read for `within`, acknowledging every frame as a frontend does
    /// (unless it has been told not to), or until `done` says so.
    fn pump(&mut self, within: Duration, mut done: impl FnMut(&Front) -> bool) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if done(self) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.closed.is_some() {
                return done(self);
            }
            match self.link.recv(left.min(Duration::from_millis(100))) {
                Ok(Some(message)) => self.take(message),
                Ok(None) => {}
                Err(why) => self.closed = Some((why, 255)),
            }
        }
    }

    fn take(&mut self, message: ToFrontend) {
        match message {
            ToFrontend::Text(bytes) => self.text.extend(bytes),
            ToFrontend::Frame(frame) => {
                if frame.kind == FrameKind::Jpeg {
                    self.frames += 1;
                }
                if self.acking {
                    let _ = self.link.send(&ToBackend::Painted {
                        seq: frame.seq,
                        waited_ms: None,
                        viewport_gen: 0,
                    });
                } else {
                    self.unacked.push(frame.seq);
                }
            }
            ToFrontend::Closed { why, exit } => self.closed = Some((why, exit)),
            _ => {}
        }
    }

    /// What the row and the lists said, as text.
    fn said(&self) -> String {
        String::from_utf8_lossy(&self.text).into_owned()
    }

    fn says(&self, words: &str) -> bool {
        self.said().contains(words)
    }

    fn key(&mut self, ch: char) {
        let key = KeyInput {
            key: Key::Char(ch),
            mods: Mods::default(),
            action: KeyAction::Press,
            text: Some(ch),
        };
        self.link
            .send(&ToBackend::Input {
                input: Input::Key(key),
                pixel_mouse: false,
            })
            .expect("sent");
    }

    /// Close the window as `ctrl+q` does, and wait for the backend to say
    /// it has.
    fn quit(&mut self) -> (String, u8) {
        self.link
            .send(&ToBackend::Close {
                why: CloseWhy::Quit,
            })
            .expect("sent");
        assert!(
            self.pump(Duration::from_secs(5), |f| f.closed.is_some()),
            "the backend did not say the window closed"
        );
        self.closed.clone().expect("closed")
    }

    /// Frames per second over `within`, acknowledging as it goes.
    fn rate(&mut self, within: Duration) -> f64 {
        let before = self.frames;
        let start = Instant::now();
        self.pump(within, |_| false);
        (self.frames - before) as f64 / start.elapsed().as_secs_f64()
    }
}

/// `/proc/<pid>/stat`'s fields after the command name: the state first,
/// so that field `n` of proc(5) is at `n - 3`.
fn stat_fields(pid: u32) -> Option<Vec<String>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(
        stat[stat.rfind(')')? + 1..]
            .split_whitespace()
            .map(str::to_string)
            .collect(),
    )
}

/// Whether `pid` is a process that is running: a zombie — a backend this
/// test started that has exited and not been reaped — is not.
fn alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        return stat_fields(pid)
            .and_then(|f| f.first().cloned())
            .is_some_and(|state| state != "Z" && state != "X");
    }
    // Without /proc a zombie answers signal 0 like the living, so one of
    // this test's own children is reaped first if it has exited; for
    // anybody else's pid this does nothing.
    // SAFETY: `waitpid(2)` with a null status pointer writes nothing, and
    // `WNOHANG` does not wait; signal 0 sends nothing and only asks.
    unsafe {
        if libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) == pid as i32 {
            return false;
        }
        libc::kill(pid as i32, 0) == 0
    }
}

/// Whether to assert on a rate: not on GitHub's shared macOS VM, which can
/// be descheduled for over 100 ms (`tests/engine.rs` has the story), saying
/// so.
fn timing_asserted(what: &str) -> bool {
    if cfg!(target_os = "macos") && std::env::var_os("BLINKTERM_SHARED_RUNNER").is_some() {
        eprintln!("skipped on the shared macOS runner: {what} asserts on time");
        return false;
    }
    true
}

/// Wait for `pid` to have gone, up to `within`.
fn gone_within(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !alive(pid)
}

/// Whether nobody holds the profile's lock.
fn lock_free(dir: &Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("blinkterm.lock"))
    else {
        return true;
    };
    use std::os::fd::AsRawFd;
    // SAFETY: `flock(2)` takes a descriptor and a flag word and reads no
    // memory; the descriptor is `file`'s, open for the whole call. The lock,
    // if taken, goes with the file at the end of this function.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// Wait for the profile's lock to be let go, up to `within`.
fn lock_free_within(dir: &Path, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while !lock_free(dir) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    lock_free(dir)
}

/// The pid written into the profile's lock.
fn lock_holder(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(dir.join("blinkterm.lock"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Whether a command line, as `/proc/<pid>/cmdline` has it, says
/// `--user-data-dir=<dir>`: as one of its words, or inside the one string
/// a browser that has rewritten its title makes of them, with spaces.
fn names_profile(cmdline: &[u8], dir: &Path) -> bool {
    let wanted = format!("--user-data-dir={}", dir.display());
    let wanted = wanted.as_bytes();
    if cmdline.len() < wanted.len() {
        return false;
    }
    (0..=cmdline.len() - wanted.len()).any(|at| {
        let starts = at == 0 || matches!(cmdline[at - 1], 0 | b' ');
        let ends = matches!(cmdline.get(at + wanted.len()), None | Some(0 | b' '));
        starts && ends && &cmdline[at..at + wanted.len()] == wanted
    })
}

/// The process groups with a living member whose command line names the
/// profile: the engines running on it. Linux only; elsewhere, nothing to ask.
fn engine_groups(dir: &Path) -> Vec<i32> {
    let mut groups = Vec::new();
    if !cfg!(target_os = "linux") {
        return groups;
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return groups;
    };
    for entry in entries.flatten() {
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if !names_profile(&cmdline, dir) {
            continue;
        }
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Some(rest) = stat_fields(pid) else {
            continue;
        };
        if rest.first().map(String::as_str) == Some("Z") {
            continue;
        }
        if let Some(group) = rest.get(2).and_then(|g| g.parse().ok()) {
            if !groups.contains(&group) {
                groups.push(group);
            }
        }
    }
    groups
}

/// The engine's process group as the backend wrote it into the profile.
fn marked_group(dir: &Path) -> Option<i32> {
    let text = std::fs::read_to_string(dir.join(engine::PGID_FILE)).ok()?;
    engine::parse_pgid_marker(&text).map(|(group, _)| group)
}

/// Whether process group `group` has a member that is not a zombie.
fn group_alive(group: i32) -> bool {
    if cfg!(target_os = "linux") {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return false;
        };
        return entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .parse::<u32>()
                .ok()
                .and_then(stat_fields)
                .is_some_and(|f| {
                    f.first().map(String::as_str) != Some("Z")
                        && f.get(2).and_then(|g| g.parse::<i32>().ok()) == Some(group)
                })
        });
    }
    // SAFETY: signal 0 to a group sends nothing and only asks.
    unsafe { libc::kill(-group, 0) == 0 }
}

fn signal(pid: i32, signal: libc::c_int) {
    // SAFETY: two integers, no memory; a pid or group this test was told.
    unsafe {
        libc::kill(pid, signal);
    }
}

/// Stop whatever a failed test left behind.
fn stop_backend(pid: u32) {
    if alive(pid) {
        signal(pid as i32, libc::SIGTERM);
        gone_within(pid, Duration::from_secs(10));
    }
}

/// Two terminals started on a profile nobody holds, at the same moment:
/// both candidates race for the lock, one wins, the other says busy, and
/// its frontend attaches to the winner. One backend, one engine, two
/// windows.
#[test]
fn two_frontends_starting_at_once_get_one_backend() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("at-once");
    let page = serve();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let starts: Vec<_> = (0..2)
        .map(|n| {
            let dir = scratch.0.clone();
            let page = page.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut spawn = launcher(Duration::from_secs(15));
                barrier.wait();
                let front = Front::try_open(
                    &mut spawn,
                    &dir,
                    an_open((60 + n * 10, 20), &[page], &options(&[]), false),
                );
                (front, spawn)
            })
        })
        .collect();
    let mut fronts: Vec<(Front, Spawn)> = starts
        .into_iter()
        .map(|t| {
            let (front, spawn) = t.join().expect("the thread");
            (front.expect("a window for both"), spawn)
        })
        .collect();
    let backend = fronts[0].0.link.backend_pid;
    assert_eq!(fronts[1].0.link.backend_pid, backend, "one backend");
    assert_eq!(fronts[1].0.link.generation, fronts[0].0.link.generation);
    assert_eq!(lock_holder(&scratch.0), Some(backend));
    // The candidate that lost has gone, having said busy.
    let started: Vec<u32> = fronts
        .iter()
        .flat_map(|(_, spawn)| spawn.children.iter().map(Child::id))
        .collect();
    for pid in started.iter().filter(|pid| **pid != backend) {
        assert!(
            gone_within(*pid, Duration::from_secs(5)),
            "a losing candidate {pid} is still running"
        );
    }
    for (front, _) in &mut fronts {
        assert!(
            front.pump(PATIENCE, |f| f.frames >= 3),
            "frames on both windows"
        );
    }
    if cfg!(target_os = "linux") {
        assert_eq!(engine_groups(&scratch.0).len(), 1, "one engine");
    }
    for (front, _) in &mut fronts {
        front.quit();
    }
    assert!(gone_within(backend, Duration::from_secs(10)));
    stop_backend(backend);
}

/// The frontend that started the backend leaving is one window closing:
/// the backend is not its child in any way that matters — a session of its
/// own, no controlling terminal — and the other window goes on.
#[test]
fn the_first_frontend_exiting_leaves_the_backend_and_its_other_window() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("first-exits");
    let page = serve();
    let mut first_spawn = launcher(Duration::from_secs(15));
    let mut a = Front::open(
        &mut first_spawn,
        &scratch.0,
        (80, 24),
        std::slice::from_ref(&page),
    );
    let backend = a.link.backend_pid;
    assert_eq!(
        first_spawn.children.first().map(Child::id),
        Some(backend),
        "the first frontend started the backend"
    );
    let mut b = Front::open(
        &mut launcher(Duration::from_secs(15)),
        &scratch.0,
        (60, 20),
        std::slice::from_ref(&page),
    );
    if cfg!(target_os = "linux") {
        let fields = stat_fields(backend).expect("the backend's stat");
        // proc(5): field 6 is the session, field 7 the controlling tty.
        assert_eq!(fields[3], backend.to_string(), "a session of its own");
        assert_eq!(fields[4], "0", "no controlling terminal");
    }
    assert!(b.pump(PATIENCE, |f| f.frames >= 3));

    a.quit();
    drop(a);
    drop(first_spawn);
    let before = b.frames;
    b.pump(Duration::from_secs(2), |_| false);
    assert!(
        b.frames >= before + 20
            || (b.frames > before && !timing_asserted("the other window's rate")),
        "the other window kept painting: {} frames in 2 s",
        b.frames - before
    );
    assert!(alive(backend), "the backend is still there");
    assert_eq!(lock_holder(&scratch.0), Some(backend));
    b.key('k');
    assert!(b.pump(PATIENCE, |f| f.says("key-k")), "and taking input");
    b.quit();
    assert!(gone_within(backend, Duration::from_secs(10)));
    stop_backend(backend);
}

/// A frontend killed outright says nothing; its connection just ends. Its
/// window waits out the grace, then its tabs are a lost group and its page
/// is closed in the engine, and nothing else closes: not the other window,
/// not the backend.
#[test]
fn a_frontend_killed_outright_is_a_window_lost_after_grace_and_nothing_else_closes() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("killed-frontend");
    let page = serve();
    let grace = Duration::from_millis(1500);
    let mut spawn = launcher(grace);
    let mut a = Front::open(&mut spawn, &scratch.0, (80, 24), &[format!("{page}a")]);
    let mut b = Front::open(&mut spawn, &scratch.0, (80, 24), &[format!("{page}gone")]);
    let backend = a.link.backend_pid;
    assert!(b.pump(PATIENCE, |f| f.frames >= 3));
    b.pump(Duration::from_secs(1), |_| false);
    // What a SIGKILL of the frontend leaves its backend: the end of the
    // connection, without a `close`.
    drop(b);
    let vanished = Instant::now();

    // Within the grace, the window is still there: nothing is saved lost.
    a.pump(grace / 2, |_| false);
    let early = std::fs::read_to_string(scratch.0.join("session")).unwrap_or_default();
    assert!(!early.contains(" lost\n"), "lost before the grace: {early}");

    a.pump(grace + Duration::from_secs(2), |_| false);
    let session = std::fs::read_to_string(scratch.0.join("session")).expect("a session");
    assert!(
        session.contains(" lost\n") && session.contains("/gone\t"),
        "{session}"
    );
    assert!(vanished.elapsed() >= grace);
    assert!(a.closed.is_none(), "the other window was closed");
    assert!(alive(backend), "the backend stopped");
    let before = a.frames;
    a.pump(Duration::from_secs(1), |_| false);
    assert!(a.frames > before, "the other window kept painting");
    a.key('x');
    assert!(a.pump(PATIENCE, |f| f.says("key-x")));
    a.quit();
    assert!(gone_within(backend, Duration::from_secs(10)));
    stop_backend(backend);
}

/// A backend killed outright: every frontend is told in a sentence where to
/// look, and the next start leaves no engine of the old one's on the
/// profile — whatever the kill left of its group is killed before another
/// engine starts — and offers the tabs back.
///
/// The old engine cannot be kept alive for the next start to find: stopped
/// (`SIGSTOP`) before the kill, its group is orphaned by the kill with a
/// stopped member, and the kernel sends such a group `SIGHUP` and `SIGCONT`,
/// which ends it. That `reap_orphan` kills a group that did survive is
/// shown with a stand-in in
/// `a_stale_backend_sock_and_a_stale_engine_pgid_are_cleaned_under_the_lock`.
#[test]
fn a_backend_killed_outright_leaves_frontends_with_a_sentence_and_the_next_start_reaps_its_engine()
{
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("killed-backend");
    let page = serve();
    let mut spawn = launcher(Duration::from_secs(15));
    let mut a = Front::open(&mut spawn, &scratch.0, (80, 24), &[format!("{page}one")]);
    let mut b = Front::open(&mut spawn, &scratch.0, (80, 24), &[format!("{page}two")]);
    let backend = a.link.backend_pid;
    assert!(a.pump(PATIENCE, |f| f.frames >= 3));
    assert!(b.pump(PATIENCE, |f| f.frames >= 3));
    // Long enough for the session to have recorded both windows' tabs.
    a.pump(Duration::from_secs(1), |_| false);
    let group = marked_group(&scratch.0).expect("the engine's group is in the profile");
    assert!(group_alive(group));

    signal(backend as i32, libc::SIGKILL);
    assert!(gone_within(backend, Duration::from_secs(5)));
    for front in [&mut a, &mut b] {
        assert!(
            front.pump(Duration::from_secs(5), |f| f.closed.is_some()),
            "a frontend was not told"
        );
        let (why, _) = front.closed.clone().expect("closed");
        assert!(
            why.contains("stopped unexpectedly") && why.contains(frontend::LOG_FILE),
            "{why}"
        );
    }
    assert!(
        scratch.0.join(engine::PGID_FILE).exists(),
        "the marker is what the kill left"
    );
    let session = std::fs::read_to_string(scratch.0.join("session")).expect("a session");
    assert!(
        session.starts_with("# blinkterm session: open"),
        "{session}"
    );

    let mut c = Front::open(&mut spawn, &scratch.0, (80, 24), &[]);
    assert_ne!(c.link.backend_pid, backend);
    let reaped = Instant::now() + Duration::from_secs(3);
    while group_alive(group) && Instant::now() < reaped {
        std::thread::sleep(Duration::from_millis(20));
    }
    let left = group_alive(group);
    if left {
        // Not left for the next test to trip on.
        signal(-group, libc::SIGKILL);
    }
    assert!(!left, "the old engine's group {group} was not reaped");
    if cfg!(target_os = "linux") {
        assert_eq!(engine_groups(&scratch.0).len(), 1, "one engine, the new");
    }
    assert!(
        c.pump(PATIENCE, |f| f.says("restore")),
        "no offer: {}",
        c.said()
    );
    // Each window the dead backend had is a group of its own, offered to a
    // window each.
    let mut d = Front::open(&mut spawn, &scratch.0, (80, 24), &[]);
    assert!(
        d.pump(PATIENCE, |f| f.says("restore")),
        "no second offer: {}",
        d.said()
    );
    let pid = c.link.backend_pid;
    d.quit();
    c.quit();
    assert!(gone_within(pid, Duration::from_secs(10)));
    stop_backend(pid);
}

/// The engine dying under two windows: started again once, on the same
/// profile and lock, and each window gets its own tabs back.
#[test]
fn an_engine_killed_under_two_windows_comes_back_with_both_windows_tabs() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("killed-engine");
    let page = serve();
    let mut spawn = launcher(Duration::from_secs(15));
    let mut a = Front::open(&mut spawn, &scratch.0, (80, 24), &[format!("{page}alpha")]);
    let mut b = Front::open(&mut spawn, &scratch.0, (70, 20), &[format!("{page}beta")]);
    let backend = a.link.backend_pid;
    assert!(a.pump(PATIENCE, |f| f.says("alpha") && f.frames >= 3));
    assert!(b.pump(PATIENCE, |f| f.says("beta") && f.frames >= 3));
    let group = marked_group(&scratch.0).expect("the engine's group");

    signal(-group, libc::SIGKILL);
    for front in [&mut a, &mut b] {
        assert!(
            front.pump(PATIENCE, |f| f.says("starting it again")),
            "not told: {}",
            front.said()
        );
        front.text.clear();
        front.frames = 0;
    }
    assert!(
        a.pump(PATIENCE, |f| f.says("alpha") && f.frames >= 3),
        "window a: {}",
        a.said()
    );
    assert!(
        b.pump(PATIENCE, |f| f.says("beta") && f.frames >= 3),
        "window b: {}",
        b.said()
    );
    assert!(!a.says("beta") && !b.says("alpha"), "the tabs swapped");
    assert!(a.closed.is_none() && b.closed.is_none());
    assert!(alive(backend), "the same backend");
    assert_eq!(lock_holder(&scratch.0), Some(backend));
    let new_group = marked_group(&scratch.0).expect("the new engine's group");
    assert_ne!(new_group, group, "the marker names the new engine");
    if cfg!(target_os = "linux") {
        assert_eq!(engine_groups(&scratch.0), vec![new_group]);
    }
    a.quit();
    b.quit();
    assert!(gone_within(backend, Duration::from_secs(10)));
    stop_backend(backend);
}

/// What a backend answers a `hello` on the profile at `dir` with, read off
/// the socket by hand; `None` when nobody is listening.
fn hello(dir: &Path) -> Option<ToFrontend> {
    let ipc::Connect::Stream(mut stream) = ipc::connect(dir).ok()? else {
        return None;
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("a timeout");
    let said = ipc::encode(&ToBackend::Hello {
        protocol: ipc::PROTOCOL,
        version: ipc::VERSION.to_string(),
        dir: Some(dir.to_path_buf()),
    });
    stream.write_all(&said).ok()?;
    let mut decoder = ipc::Decoder::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).ok().filter(|n| *n > 0)?;
        let got: Vec<ToFrontend> = decoder.feed(&buf[..n]).ok()?;
        if let Some(first) = got.into_iter().next() {
            return Some(first);
        }
    }
}

/// A terminal that attaches just as the last window closes is told to wait
/// while the old backend stops its engine, and then gets a fresh backend.
///
/// The engine is stopped (`SIGSTOP`) before the close so that it cannot
/// answer `Browser.close`: the backend's stop then takes its whole close
/// timeout (five seconds) instead of the few milliseconds an idle headless
/// shell needs, and the attach is sure to land inside it.
#[test]
fn attaching_during_shutdown_waits_and_then_starts_a_fresh_backend() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("during-shutdown");
    let page = serve();
    let mut spawn = launcher(Duration::from_secs(15));
    let mut a = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        std::slice::from_ref(&page),
    );
    assert!(a.pump(PATIENCE, |f| f.frames >= 3));
    let (old_pid, old_generation) = (a.link.backend_pid, a.link.generation);
    let group = marked_group(&scratch.0).expect("the engine's group");

    signal(-group, libc::SIGSTOP);
    let (why, exit) = a.quit();
    assert_eq!((why.as_str(), exit), ("", 0));
    let closed = Instant::now();

    // While it waits for the engine: a hello is told to try again, and why.
    match hello(&scratch.0) {
        Some(ToFrontend::Refused { why, retry: true }) => {
            assert!(why.contains("shutting down"), "{why}");
        }
        other => panic!("not told to wait: {other:?}"),
    }
    assert!(alive(old_pid));

    let mut b = Front::try_open(
        &mut launcher(Duration::from_secs(15)),
        &scratch.0,
        an_open((80, 24), &[page], &options(&[]), false),
    )
    .expect("a window after the shutdown");
    eprintln!("attached {:?} after the close", closed.elapsed());
    assert_ne!(b.link.backend_pid, old_pid, "a fresh backend");
    assert_ne!(b.link.generation, old_generation);
    assert!(!alive(old_pid), "the old one had gone first");
    assert!(!group_alive(group), "and its engine");
    assert!(b.pump(PATIENCE, |f| f.frames >= 3));
    let pid = b.link.backend_pid;
    b.quit();
    assert!(gone_within(pid, Duration::from_secs(10)));
    stop_backend(pid);
    stop_backend(old_pid);
    if group_alive(group) {
        signal(-group, libc::SIGKILL);
    }
}

/// A browser-wide setting the running engine does not have — a proxy — is
/// refused with the key named, and the window already open is untouched.
#[test]
fn a_conflicting_proxy_is_refused_with_the_key_named() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("proxy");
    let page = serve();
    let mut spawn = launcher(Duration::from_secs(15));
    let mut a = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        std::slice::from_ref(&page),
    );
    let backend = a.link.backend_pid;
    assert!(a.pump(PATIENCE, |f| f.frames >= 3));

    let proxied = options(&["--proxy", "127.0.0.1:9"]);
    let refused = Front::try_open(
        &mut spawn,
        &scratch.0,
        an_open((80, 24), &[page], &proxied, false),
    );
    let why = refused.err().expect("refused");
    assert!(
        why.contains(&format!("(pid {backend}) runs the engine with proxy = ")),
        "{why}"
    );
    assert!(why.contains("127.0.0.1:9"), "{why}");
    assert!(why.contains("--temp-profile or --profile <dir>"), "{why}");

    let before = a.frames;
    a.pump(Duration::from_secs(1), |_| false);
    assert!(a.frames > before, "the open window kept painting");
    assert!(a.closed.is_none());
    a.quit();
    assert!(gone_within(backend, Duration::from_secs(10)));
    stop_backend(backend);
}

/// A process group of its own that does, or does not (`engine`), look like
/// an engine on `dir` to a backend reading a marker, and the executable to
/// write in that marker.
///
/// On Linux the backend looks for `--user-data-dir=<dir>` as a word of a
/// member's command line: a shell with it, waiting on a sleep in the same
/// group. On a Mac it compares the group leader's executable with the
/// marker's: a sleep, and its own path or another.
fn a_fake_engine(dir: &Path, engine: bool) -> (Child, &'static str) {
    let (mut command, exe) = if cfg!(target_os = "linux") {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("sleep 600; :");
        if engine {
            command.arg(format!("--user-data-dir={}", dir.display()));
        }
        (command, "/bin/sh")
    } else {
        let mut command = Command::new("/bin/sleep");
        command.arg("600");
        (command, if engine { "/bin/sleep" } else { "/bin/sh" })
    };
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("a stand-in");
    (child, exe)
}

/// What a crashed backend left on `dir`: a socket file nobody listens on, a
/// lock naming a pid that has gone, and a marker naming `group` and `exe`.
fn a_crash_left(dir: &Path, group: u32, exe: &str) {
    std::fs::create_dir_all(dir).expect("the profile");
    let socket = dir.join(ipc::SOCKET_FILE);
    let _ = std::fs::remove_file(&socket);
    drop(std::os::unix::net::UnixListener::bind(&socket).expect("a socket"));
    assert!(socket.exists(), "a socket file nobody listens on");
    std::fs::write(dir.join("blinkterm.lock"), "999999999\n").expect("a lock");
    std::fs::write(
        dir.join(engine::PGID_FILE),
        format!("{group}\t1700000000\t{exe}\n"),
    )
    .expect("a marker");
}

/// A crash leaves a socket file and a marker. The next backend takes the
/// lock first, then kills the group the marker names only when it is an
/// engine on this profile, and binds over the dead socket. A marker whose
/// group is somebody else's is left alone.
#[test]
fn a_stale_backend_sock_and_a_stale_engine_pgid_are_cleaned_under_the_lock() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("stale");
    let page = serve();
    let mut spawn = launcher(Duration::from_secs(15));

    // A marker that names a group which is not an engine on this profile.
    let (mut decoy, exe) = a_fake_engine(&scratch.0, false);
    a_crash_left(&scratch.0, decoy.id(), exe);
    assert!(matches!(
        ipc::connect(&scratch.0),
        Ok(ipc::Connect::NobodyThere)
    ));
    let mut a = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        std::slice::from_ref(&page),
    );
    assert!(a.pump(PATIENCE, |f| f.frames >= 3));
    assert!(
        matches!(decoy.try_wait(), Ok(None)),
        "a group that is not an engine was killed"
    );
    assert_ne!(marked_group(&scratch.0), Some(decoy.id() as i32));
    let pid = a.link.backend_pid;
    a.quit();
    assert!(gone_within(pid, Duration::from_secs(10)));
    let _ = decoy.kill();
    let _ = decoy.wait();

    // And one that is.
    let (mut orphan, exe) = a_fake_engine(&scratch.0, true);
    let orphan_group = orphan.id() as i32;
    std::thread::sleep(Duration::from_millis(100));
    a_crash_left(&scratch.0, orphan.id(), exe);
    let mut b = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        std::slice::from_ref(&page),
    );
    let status = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match orphan.try_wait() {
                Ok(Some(status)) => break Some(status),
                _ if Instant::now() >= deadline => break None,
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    };
    if status.is_none() {
        signal(-orphan_group, libc::SIGKILL);
        let _ = orphan.wait();
    }
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        status.and_then(|s| s.signal()),
        Some(libc::SIGKILL),
        "the orphaned engine was not killed"
    );
    assert!(!group_alive(orphan_group), "its whole group");
    assert!(b.pump(PATIENCE, |f| f.frames >= 3));
    let pid = b.link.backend_pid;
    assert_eq!(lock_holder(&scratch.0), Some(pid));
    assert!(matches!(
        ipc::connect(&scratch.0),
        Ok(ipc::Connect::Stream(_))
    ));
    b.quit();
    assert!(gone_within(pid, Duration::from_secs(10)));
    stop_backend(pid);
}

/// A profile whose path is longer than a socket address holds (104 bytes on
/// a Mac, 108 on Linux) gets both of its sockets — the backend's and the
/// `--remote` one — in private fallback directories of their own, behind
/// links in the profile, and both are reachable through those links. No
/// engine: this runs everywhere.
#[test]
fn a_profile_path_longer_than_sun_path_gets_a_private_fallback_for_both_sockets() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("long");
    let dir = scratch.0.join("p".repeat(120));
    std::fs::create_dir_all(&dir).expect("a deep profile");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("0700");
    assert!(dir.join(ipc::SOCKET_FILE).as_os_str().len() > 108);

    let endpoint = ipc::bind(&dir).expect("the backend socket");
    let mut listener = blinkterm::remote::Listener::bind(&dir).expect("the remote socket");
    let mut fallbacks = Vec::new();
    for name in [ipc::SOCKET_FILE, blinkterm::remote::SOCKET_FILE] {
        let link = dir.join(name);
        let meta = std::fs::symlink_metadata(&link).expect("something in the profile");
        assert!(meta.file_type().is_symlink(), "{name} is not a link");
        let target = std::fs::read_link(&link).expect("a link");
        assert!(target.as_os_str().len() < 100, "{target:?} is short enough");
        let fallback = target.parent().expect("a directory").to_path_buf();
        let mode = std::fs::metadata(&fallback).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{fallback:?}");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{target:?}");
        fallbacks.push(fallback);
    }
    assert_ne!(fallbacks[0], fallbacks[1], "a directory each");

    // The backend socket, through its link.
    let ipc::Connect::Stream(mut stream) = ipc::connect(&dir).expect("connected") else {
        panic!("nobody there");
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut accepted = loop {
        if let Some(peer) = ipc::accept(&endpoint).expect("accept") {
            break peer;
        }
        assert!(Instant::now() < deadline, "never accepted");
        std::thread::sleep(Duration::from_millis(10));
    };
    stream
        .write_all(&ipc::encode(&ToBackend::Ping))
        .expect("written");
    accepted.set_nonblocking(false).expect("blocking");
    accepted
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("a timeout");
    let mut chunk = [0u8; 64];
    let n = accepted.read(&mut chunk).expect("read");
    let got: Vec<ToBackend> = ipc::Decoder::new().feed(&chunk[..n]).expect("decoded");
    assert_eq!(got, vec![ToBackend::Ping]);

    // The --remote socket, through its own.
    let sender = {
        let dir = dir.clone();
        std::thread::spawn(move || {
            blinkterm::remote::deliver(&dir, &["https://example.com/".to_string()])
        })
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let deliveries = listener.accept_ready().expect("accept");
        if let Some(delivery) = deliveries.into_iter().next() {
            assert_eq!(delivery.lines, vec![Ok("https://example.com/".to_string())]);
            delivery.nowhere();
            break;
        }
        assert!(Instant::now() < deadline, "the sender never came");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        sender.join().expect("the sender"),
        Ok(blinkterm::remote::Delivered::NobodyThere),
        "the sender heard nowhere"
    );

    drop((stream, accepted, endpoint, listener));
    for fallback in fallbacks {
        assert!(!fallback.exists(), "{fallback:?} outlived its socket");
    }
    assert!(std::fs::symlink_metadata(dir.join(ipc::SOCKET_FILE)).is_err());
}

/// A terminal that stops reading — an ssh link that stalls, a suspended
/// tmux client — beside one that keeps up: the slow one's window has at
/// most [`backend::IN_FLIGHT`] frames out and stops casting after
/// [`backend::STALL`], the fast one keeps its rate the whole time, and the
/// slow one picks up again once it reads and acknowledges.
#[test]
fn a_slow_frontend_bounds_its_own_frames_and_the_fast_one_keeps_its_rate() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("slow");
    let page = serve();
    let mut spawn = launcher(Duration::from_secs(15));
    let mut fast = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        std::slice::from_ref(&page),
    );
    // Paced, so that the engine's own acknowledgement waits for this
    // terminal too: the worst case for everybody else.
    let mut slow = Front::try_open(
        &mut spawn,
        &scratch.0,
        an_open((80, 24), std::slice::from_ref(&page), &options(&[]), true),
    )
    .expect("a window");
    let backend = fast.link.backend_pid;
    assert!(slow.pump(PATIENCE, |f| f.frames >= 3));
    assert!(fast.pump(PATIENCE, |f| f.frames >= 3));

    // From here the slow one reads nothing at all, and so acknowledges
    // nothing either.
    let before = fast.rate(Duration::from_secs(2));
    eprintln!("fast window alone: {before:.1} fps");
    let during = fast.rate(backend::STALL + Duration::from_secs(1));
    eprintln!("fast window beside a stalled one: {during:.1} fps");
    let after = fast.rate(Duration::from_secs(2));
    eprintln!("fast window once the other stopped casting: {after:.1} fps");
    let timed = timing_asserted("the fast window's rate");
    for rate in [during, after] {
        assert!(
            rate >= 20.0 || (!timed && rate > 0.0),
            "the fast window slowed to {rate:.1} fps"
        );
    }

    // What waited in the slow one's socket all that time: at most the
    // frames that were out, and no more.
    slow.acking = false;
    slow.pump(Duration::from_millis(500), |_| false);
    assert!(
        slow.unacked.len() <= backend::IN_FLIGHT,
        "{} frames were sent to a terminal that painted none",
        slow.unacked.len()
    );
    assert!(slow.closed.is_none(), "the slow terminal was dropped");

    // It catches up, and its window casts again.
    slow.acking = true;
    let seq = slow.unacked.iter().copied().max().unwrap_or(0);
    slow.link
        .send(&ToBackend::Painted {
            seq,
            waited_ms: None,
            viewport_gen: 0,
        })
        .expect("sent");
    slow.frames = 0;
    assert!(
        slow.pump(PATIENCE, |f| f.frames >= 10),
        "the slow window did not cast again: {} frames",
        slow.frames
    );
    let rate = fast.rate(Duration::from_secs(1));
    assert!(rate >= 20.0 || (!timed && rate > 0.0), "{rate:.1} fps");
    fast.quit();
    slow.quit();
    assert!(gone_within(backend, Duration::from_secs(10)));
    stop_backend(backend);
}

/// A cookie set in a window that is not the first, and a last window that
/// goes without a word: the stop at the end of the grace is the same
/// `Browser.close` and wait as a quit's, and the cookie is there at the
/// next start.
#[test]
fn cookies_set_before_the_last_window_goes_are_there_at_the_next_start() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("cookie");
    let page = serve();
    let grace = Duration::from_millis(500);
    let mut spawn = launcher(grace);
    let mut a = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        std::slice::from_ref(&page),
    );
    let mut b = Front::open(&mut spawn, &scratch.0, (80, 24), &[format!("{page}set")]);
    let backend = a.link.backend_pid;
    assert!(b.pump(PATIENCE, |f| f.says("cookie set")), "{}", b.said());
    a.quit();
    drop(b);
    assert!(
        gone_within(backend, grace + Duration::from_secs(10)),
        "the backend did not stop after the grace"
    );
    assert!(lock_free_within(&scratch.0, Duration::from_secs(2)));
    assert!(!scratch.0.join(engine::PGID_FILE).exists());

    let mut again = Front::open(&mut spawn, &scratch.0, (80, 24), &[format!("{page}show")]);
    // The window that went without a word is offered back, ahead of the one
    // that quit; declined, the row shows the page.
    assert!(
        again.pump(PATIENCE, |f| f.says("restore 1 tab from last time?")),
        "{}",
        again.said()
    );
    again.key('n');
    assert!(
        again.pump(PATIENCE, |f| f.says("cookie=kept=yes")),
        "the cookie was not kept: {}",
        again.said()
    );
    let pid = again.link.backend_pid;
    again.quit();
    assert!(gone_within(pid, Duration::from_secs(10)));
    stop_backend(pid);
}
