//! Several terminals on one profile (#83): the real backend, started by
//! [`blinkterm::frontend::attach`] as the program starts it, driven by
//! fake frontends — a connection each, with made-up terminal sizes and no
//! pty — against a real engine.
//!
//! Every test here runs only when `BLINKTERM_ENGINE` names the engine to use,
//! as `tests/engine.rs` does, and skips with a line saying so otherwise. Run
//! them one at a time (`--test-threads=1`): each starts an engine of its own,
//! and two at once on a small machine is a test of the machine.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use blinkterm::engine;
use blinkterm::fit::Metrics;
use blinkterm::frontend::{self, Link, Spawn};
use blinkterm::input::{Input, Key, KeyAction, KeyInput, Mods, MouseInput, MouseKind};
use blinkterm::ipc::{
    self, BrowserSettings, CloseWhy, FrameKind, Open, RouteFlags, ToBackend, ToFrontend,
    WindowSettings,
};
use blinkterm::options::{self, Invocation, Options};
use blinkterm::remote::{self, Delivered};

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

/// One link over the whole page, to `/opened-<from>`, where `<from>` is
/// the page's own query: a middle click anywhere opens it behind.
const LINKS: &str = "<!doctype html><title>links</title>\
<body style='margin:0'><a id=l style='display:block;height:100vh' href='#'>go</a>\
<script>document.getElementById('l').href='/opened-'+location.search.slice(1)</script>";

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

/// A scratch profile of this test's own, gone before and after.
struct Scratch(PathBuf);

impl Scratch {
    fn new(what: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "blinkterm-it-windows-{what}-{}",
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
            } else if request.starts_with("GET /links") {
                (LINKS, "")
            } else if let Some(rest) = request.strip_prefix("GET /opened-") {
                let from: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect();
                let page = format!("<!doctype html><title>opened by {from}</title>");
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
                    page.len()
                );
                let _ = stream.write_all(answer.as_bytes());
                continue;
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

/// The options a frontend started with `--no-config` has, which are the
/// backend's too.
fn options() -> Options {
    match options::invocation(&["--no-config".to_string()]).expect("options") {
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

/// One fake frontend: its link, and what the backend has said to it.
struct Front {
    link: Link,
    metrics: Metrics,
    text: Vec<u8>,
    frames: Vec<(u32, u32)>,
    closed: Option<(String, u8)>,
}

impl Front {
    /// Attach to the profile at `dir`, starting its backend if need be, and
    /// open a window at `size` on `urls`.
    fn open(spawn: &mut Spawn, dir: &Path, size: (u32, u32), urls: &[String]) -> Front {
        Front::open_with(spawn, dir, size, urls, false)
    }

    fn open_with(
        spawn: &mut Spawn,
        dir: &Path,
        size: (u32, u32),
        urls: &[String],
        restore: bool,
    ) -> Front {
        let mut link = frontend::attach(spawn, Some(dir)).expect("attached");
        let options = options();
        let metrics = metrics(size.0, size.1);
        link.open(Open {
            nonce: ipc::new_nonce(),
            window: WindowSettings::from(&options),
            browser: BrowserSettings::from(&options),
            metrics,
            route: RouteFlags {
                paced: false,
                png: false,
                keyed: false,
                alpha: None,
                every_nth: 1,
            },
            pixel_mouse: false,
            urls: urls.to_vec(),
            restore,
            problems: Vec::new(),
            cwd: std::env::temp_dir(),
            home_dir: None,
        })
        .expect("a window");
        Front {
            link,
            metrics,
            text: Vec::new(),
            frames: Vec::new(),
            closed: None,
        }
    }

    /// Read for `within`, acknowledging every frame as a frontend does, or
    /// until `done` says so.
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
                Err(why) => {
                    self.closed = Some((why, 255));
                }
            }
        }
    }

    fn take(&mut self, message: ToFrontend) {
        match message {
            ToFrontend::Text(bytes) => self.text.extend(bytes),
            ToFrontend::Frame(frame) => {
                if frame.kind == FrameKind::Jpeg {
                    if let Ok(size) = blinkterm::jpeg::dimensions(&frame.image) {
                        self.frames.push(size);
                    }
                }
                let _ = self.link.send(&ToBackend::Painted {
                    seq: frame.seq,
                    waited_ms: None,
                    viewport_gen: 0,
                });
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

    /// The size a frame of this window is, at a scale of 1: the pane less
    /// the row.
    fn frame_size(&self) -> (u32, u32) {
        (
            self.metrics.cols * self.metrics.cell.0,
            (self.metrics.rows - 1) * self.metrics.cell.1,
        )
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

    /// A middle click at a cell of the page, pressed and let go.
    fn middle_click(&mut self, x: u32, y: u32) {
        for kind in [MouseKind::Press, MouseKind::Release] {
            self.link
                .send(&ToBackend::Input {
                    input: Input::Mouse(MouseInput {
                        kind,
                        button: Some(1),
                        mods: Mods::default(),
                        x,
                        y,
                        wheel: (0, 0),
                    }),
                    pixel_mouse: false,
                })
                .expect("sent");
        }
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
}

/// Whether `pid` is a process that is running: a zombie — a backend this
/// test started that has exited and not been reaped — is not.
fn alive(pid: u32) -> bool {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let state = stat[stat.rfind(')').unwrap_or(0) + 1..]
            .split_whitespace()
            .next()
            .unwrap_or("");
        return state != "Z" && state != "X";
    }
    // SAFETY: signal 0 sends nothing and only asks; reads no memory.
    unsafe { libc::kill(pid as i32, 0) == 0 }
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

/// The pid written into the profile's lock.
fn lock_holder(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(dir.join("blinkterm.lock"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The process groups with a member whose command line names the profile:
/// the engines running on it. Linux only; elsewhere, nothing to ask. The
/// browser rewrites its own command line into one string with spaces, so
/// the flag is looked for inside it.
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
        if !engine::names_profile(&cmdline, dir) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let rest: Vec<&str> = stat[stat.rfind(')').unwrap_or(0) + 1..]
            .split_whitespace()
            .collect();
        if rest.first() == Some(&"Z") {
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

/// Stop whatever a failed test left on `dir`.
fn stop_backend(pid: u32) {
    if alive(pid) {
        // SAFETY: two integers, no memory; a pid this test was told.
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        gone_within(pid, Duration::from_secs(10));
    }
}

#[test]
fn two_links_on_one_profile_get_two_windows_and_one_engine() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("two");
    let page = serve();
    let mut spawn = launcher(Duration::from_secs(15));
    let mut a = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        std::slice::from_ref(&page),
    );
    let mut b = Front::open(
        &mut spawn,
        &scratch.0,
        (50, 20),
        std::slice::from_ref(&page),
    );
    let backend = a.link.backend_pid;
    assert_eq!(b.link.backend_pid, backend, "both reached one backend");
    assert_eq!(b.link.generation, a.link.generation);
    assert_eq!(
        lock_holder(&scratch.0),
        Some(backend),
        "the backend holds the lock"
    );
    assert!(!lock_free(&scratch.0));

    // Frames on both, each at its own window's size.
    for front in [&mut a, &mut b] {
        let want = front.frame_size();
        assert!(
            front.pump(PATIENCE, |f| f
                .frames
                .iter()
                .filter(|s| **s == want)
                .count()
                >= 5),
            "frames at {want:?}: {:?}",
            &front.frames[front.frames.len().saturating_sub(5)..]
        );
    }
    assert_ne!(a.frame_size(), b.frame_size());
    if cfg!(target_os = "linux") {
        assert_eq!(engine_groups(&scratch.0).len(), 1, "one engine");
    }

    // A key to one is that window's page's alone.
    a.key('q');
    assert!(a.pump(PATIENCE, |f| f.says("key-q")), "{}", a.said());
    b.pump(Duration::from_millis(500), |_| false);
    assert!(!b.says("key-q"), "the other window saw it");
    b.key('z');
    assert!(b.pump(PATIENCE, |f| f.says("key-z")));
    a.pump(Duration::from_millis(500), |_| false);
    assert!(!a.says("key-z"));

    a.quit();
    b.quit();
    assert!(
        gone_within(backend, Duration::from_secs(10)),
        "the backend stopped"
    );
    stop_backend(backend);
}

#[test]
fn closing_one_link_leaves_the_other_painting_and_the_backend_running() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("close-one");
    let page = serve();
    let mut spawn = launcher(Duration::from_secs(15));
    let mut a = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        std::slice::from_ref(&page),
    );
    let mut b = Front::open(
        &mut spawn,
        &scratch.0,
        (60, 20),
        std::slice::from_ref(&page),
    );
    let backend = a.link.backend_pid;
    assert!(b.pump(PATIENCE, |f| f.frames.len() >= 3));

    let (why, exit) = a.quit();
    assert_eq!((why.as_str(), exit), ("", 0), "a quit is a clean close");
    let before = b.frames.len();
    b.pump(Duration::from_secs(2), |_| false);
    assert!(
        b.frames.len() >= before + 20,
        "the other window kept painting: {} frames in 2 s",
        b.frames.len() - before
    );
    assert!(alive(backend), "the backend is still there");
    assert!(!lock_free(&scratch.0));
    b.key('k');
    assert!(b.pump(PATIENCE, |f| f.says("key-k")), "and taking input");

    b.quit();
    assert!(gone_within(backend, Duration::from_secs(10)));
    stop_backend(backend);
}

/// The last window closing stops the backend the way a run stopped:
/// `Browser.close`, the cookie jar written, the lock let go. A cookie set
/// before is there when the profile is started again.
#[test]
fn the_last_link_leaving_stops_the_engine_and_releases_the_lock_within_close_timeout() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("last");
    let page = serve();
    let mut spawn = launcher(Duration::from_secs(15));
    let mut a = Front::open(&mut spawn, &scratch.0, (80, 24), &[format!("{page}set")]);
    assert!(a.pump(PATIENCE, |f| f.says("cookie set")), "{}", a.said());
    let backend = a.link.backend_pid;
    let generation = a.link.generation;
    let groups = engine_groups(&scratch.0);

    a.quit();
    let stopped = Instant::now();
    assert!(
        gone_within(backend, Duration::from_secs(8)),
        "the backend did not stop"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while !lock_free(&scratch.0) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(lock_free(&scratch.0), "the lock was let go");
    eprintln!("stopped in {:?}", stopped.elapsed());
    assert!(
        !scratch.0.join(ipc::SOCKET_FILE).exists(),
        "the backend socket went with it"
    );
    assert!(
        !scratch.0.join(engine::PGID_FILE).exists(),
        "and the marker"
    );
    for group in groups {
        // SAFETY: signal 0 sends nothing and only asks; reads no memory.
        let there = unsafe { libc::kill(-group, 0) == 0 };
        assert!(
            !there || engine_groups(&scratch.0).is_empty(),
            "the engine's group went"
        );
    }

    let mut again = Front::open(&mut spawn, &scratch.0, (80, 24), &[format!("{page}show")]);
    assert_ne!(again.link.generation, generation, "a fresh backend");
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

#[test]
fn remote_reaches_the_most_recently_used_window() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("remote");
    let page = serve();
    let mut spawn = launcher(Duration::from_secs(15));
    let mut a = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        std::slice::from_ref(&page),
    );
    let mut b = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        std::slice::from_ref(&page),
    );
    let backend = a.link.backend_pid;
    a.pump(Duration::from_secs(1), |_| false);
    b.pump(Duration::from_secs(1), |_| false);

    let deliver = |url: String| {
        let dir = scratch.0.clone();
        std::thread::spawn(move || remote::deliver(&dir, &[url]))
    };
    // B typed last.
    b.key('b');
    assert!(b.pump(PATIENCE, |f| f.says("key-b")));
    let sent = deliver(format!("{page}to-b"));
    assert!(b.pump(PATIENCE, |f| f.says("to-b")), "{}", b.said());
    assert_eq!(sent.join().expect("sender"), Ok(Delivered::Opened));
    a.pump(Duration::from_millis(500), |_| false);
    assert!(!a.says("to-b"), "the other window got it");

    // Then A.
    a.key('a');
    assert!(a.pump(PATIENCE, |f| f.says("key-a")));
    let sent = deliver(format!("{page}to-a"));
    assert!(a.pump(PATIENCE, |f| f.says("to-a")), "{}", a.said());
    assert_eq!(sent.join().expect("sender"), Ok(Delivered::Opened));
    b.pump(Duration::from_millis(500), |_| false);
    assert!(!b.says("to-a"));

    a.quit();
    b.quit();
    assert!(gone_within(backend, Duration::from_secs(10)));
    stop_backend(backend);
}

#[test]
fn a_second_profile_runs_its_own_engine_and_shares_no_cookie() {
    if !engine_named() {
        return;
    }
    let one = Scratch::new("iso-one");
    let two = Scratch::new("iso-two");
    let page = serve();
    let mut spawn = launcher(Duration::from_secs(15));
    let mut a = Front::open(&mut spawn, &one.0, (80, 24), &[format!("{page}set")]);
    assert!(a.pump(PATIENCE, |f| f.says("cookie set")));
    let mut b = Front::open(&mut spawn, &two.0, (80, 24), &[format!("{page}show")]);
    assert_ne!(a.link.backend_pid, b.link.backend_pid, "a backend each");
    assert!(b.pump(PATIENCE, |f| f.says("cookie=none")), "{}", b.said());
    if cfg!(target_os = "linux") {
        let (first, second) = (engine_groups(&one.0), engine_groups(&two.0));
        assert_eq!((first.len(), second.len()), (1, 1));
        assert_ne!(first, second, "an engine each");
    }
    let pids = [a.link.backend_pid, b.link.backend_pid];
    a.quit();
    b.quit();
    for pid in pids {
        assert!(gone_within(pid, Duration::from_secs(10)));
        stop_backend(pid);
    }
}

/// A frontend that vanishes without a word: its window waits out the
/// grace, then its tabs are a lost group, and the next window opened is
/// offered them. The other window never notices.
#[test]
fn a_link_that_vanishes_is_offered_its_tabs_to_the_next_window_after_grace() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("vanish");
    let page = serve();
    let grace = Duration::from_millis(800);
    let mut spawn = launcher(grace);
    let mut a = Front::open(&mut spawn, &scratch.0, (80, 24), &[format!("{page}a")]);
    let mut b = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        &[format!("{page}b1"), format!("{page}b2")],
    );
    let backend = a.link.backend_pid;
    assert!(b.pump(PATIENCE, |f| f.frames.len() >= 3));
    // Long enough for the session to have recorded B's tabs.
    b.pump(Duration::from_secs(1), |_| false);
    drop(b);

    a.pump(grace + Duration::from_secs(2), |_| false);
    let session = std::fs::read_to_string(scratch.0.join("session")).expect("a session");
    assert!(session.contains(" lost\n"), "{session}");
    assert!(
        session.contains("/b1\t") && session.contains("/b2\t"),
        "{session}"
    );
    assert!(alive(backend));
    let before = a.frames.len();
    a.pump(Duration::from_secs(1), |_| false);
    assert!(a.frames.len() > before, "the other window kept painting");

    let mut c = Front::open(&mut spawn, &scratch.0, (80, 24), &[]);
    assert!(
        c.pump(PATIENCE, |f| f.says("restore 2 tabs from last time?")),
        "{}",
        c.said()
    );
    a.quit();
    c.quit();
    assert!(gone_within(backend, Duration::from_secs(10)));
    stop_backend(backend);
}

/// The hard case for routing: a link opened with a modifier names no
/// opener, and two windows open one in the same moment. Each page lands
/// in the window it was clicked in — attributed by the disposition the
/// clicking page's own session announced — and in no other.
#[test]
fn a_link_opened_behind_lands_in_the_window_it_was_clicked_in() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("route");
    let page = serve();
    let mut spawn = launcher(Duration::from_secs(15));
    let mut a = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        &[format!("{page}links?a")],
    );
    let mut b = Front::open(
        &mut spawn,
        &scratch.0,
        (80, 24),
        &[format!("{page}links?b")],
    );
    let backend = a.link.backend_pid;
    for front in [&mut a, &mut b] {
        assert!(
            front.pump(PATIENCE, |f| f.says("links")),
            "{}",
            front.said()
        );
        front.pump(Duration::from_millis(500), |_| false);
    }
    a.middle_click(10, 6);
    b.middle_click(10, 6);
    // A tab behind shows its url on the strip until it is looked at.
    assert!(a.pump(PATIENCE, |f| f.says("2 http")), "{}", a.said());
    assert!(b.pump(PATIENCE, |f| f.says("2 http")), "{}", b.said());
    a.pump(Duration::from_secs(1), |_| false);
    b.pump(Duration::from_secs(1), |_| false);
    assert!(a.says("2 http://") && a.says("/opened-a"), "{}", a.said());
    assert!(b.says("/opened-b"), "{}", b.said());
    assert!(!a.says("/opened-b"), "a's window got b's page");
    assert!(!b.says("/opened-a"), "b's window got a's page");
    assert!(
        !a.says("3 http") && !b.says("3 http"),
        "a page landed twice"
    );
    a.quit();
    b.quit();
    assert!(gone_within(backend, Duration::from_secs(10)));
    stop_backend(backend);
}
