//! The real program in a real pseudoterminal (#83): what a person running
//! `blinkterm` in two terminal panes on one profile gets, with nothing
//! faked between the program and the screen.
//!
//! Each test opens a pty ([`pty`]), starts this build's binary on it with
//! `--no-probe --frames raw --tmux off`, and reads what it draws through
//! `tos_term::Terminal` — the compositor's own terminal, with its own file
//! reader for the `t=s` frames, as `tests/engine.rs` has it — answering
//! whatever the program asks the terminal. The frontend in the pty starts
//! its profile's backend the way it always does.
//!
//! Every test here runs only when `BLINKTERM_ENGINE` names the engine to use,
//! as `tests/engine.rs` does, and skips with a line saying so otherwise. Run
//! them one at a time (`--test-threads=1`).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use blinkterm::engine;

#[path = "support/pty.rs"]
mod pty;
use pty::Pty;

// tOS's `t=s` reader, as `tests/engine.rs` takes it.
#[path = "support/imagefile.rs"]
#[allow(clippy::undocumented_unsafe_blocks, dead_code)]
mod imagefile;
use imagefile::ImageFiles;

/// The pane: cells, and the pixels each one is.
const COLS: u16 = 60;
const ROWS: u16 = 16;
const CELL: (u16, u16) = (8, 16);

/// How long a test waits for something the engine has to do.
const PATIENCE: Duration = Duration::from_secs(30);

/// `ctrl+q`, as a terminal without the Kitty keyboard protocol sends it.
const CTRL_Q: &[u8] = b"\x11";

/// A page that moves, so that there are frames to draw.
const PAGE: &str = "<!doctype html><title>ready</title>\
<body style='margin:0;height:100vh;background:#fff'>\
<div id=b style='position:absolute;width:60px;height:30px;background:#c33'></div>\
<script>var b=document.getElementById('b'),n=0;\
function f(){n=n>300?0:n+3;b.style.left=n+'px';requestAnimationFrame(f)}f();\
</script></body>";

/// [`PAGE`], which also goes to `/key-<key>` on a key, so that the row's
/// url says which key reached it.
const KEYED: &str = "<!doctype html><title>keyed</title>\
<body style='margin:0;height:100vh;background:#fff'>\
<div id=b style='position:absolute;width:60px;height:30px;background:#c33'></div>\
<script>\
addEventListener('keydown',function(e){location.href='/key-'+e.key});\
var b=document.getElementById('b'),n=0;\
function f(){n=n>300?0:n+3;b.style.left=n+'px';requestAnimationFrame(f)}f();\
</script></body>";

/// A page that moves, and whose file input is clicked on a key — the key is
/// the activation a chooser needs — and which goes to `/pick-cancelled` when
/// the input hears `cancel`, so that the row's url says it did. As
/// `tests/hardening.rs` has it.
const PICK: &str = "<!doctype html><title>pick ready</title>\
<body style='margin:0;height:100vh;background:#fff'>\
<input id=f type=file style='position:absolute;left:0;top:40px'>\
<div id=b style='position:absolute;width:60px;height:30px;background:#33c'></div>\
<script>\
addEventListener('keydown',function(){f.click()});\
f.addEventListener('cancel',function(){location.href='/pick-cancelled'});\
var b=document.getElementById('b'),n=0;\
function g(){n=n>300?0:n+3;b.style.left=n+'px';requestAnimationFrame(g)}g();\
</script></body>";

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

/// A scratch `$XDG_DATA_HOME`, `$XDG_CONFIG_HOME` and `$HOME` of this test's
/// own, gone before and after.
struct Scratch(PathBuf);

impl Scratch {
    fn new(what: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "blinkterm-it-terminal-{what}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["data", "config", "home"] {
            std::fs::create_dir_all(dir.join(sub)).expect("a scratch directory");
        }
        Scratch(dir)
    }

    /// This build's binary with `args`, in the scratch environment, with
    /// its standard error kept in `stderr-<what>`.
    fn command(&self, args: &[&str], what: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_blinkterm"));
        let stderr = std::fs::File::create(self.0.join(format!("stderr-{what}"))).expect("a log");
        command
            .args(args)
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("HOME", self.0.join("home"))
            .env("TERM", "xterm-kitty")
            .env_remove("TMUX")
            .env_remove("STY")
            .env_remove("SSH_CONNECTION")
            .env_remove("SSH_CLIENT")
            .env_remove("SSH_TTY")
            .stderr(Stdio::from(stderr));
        if cfg!(target_os = "macos") {
            // A Mac's shared memory objects are not files under /dev/shm,
            // where the terminal's reader looks for them; over ssh the raw
            // frames go inline instead, which the terminal reads itself.
            command.env("SSH_CONNECTION", "127.0.0.1 1 127.0.0.1 2");
        }
        command
    }

    fn stderr(&self, what: &str) -> String {
        std::fs::read_to_string(self.0.join(format!("stderr-{what}"))).unwrap_or_default()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A local server: `/pick…` is [`PICK`], `/cut…` and `/key…` are
/// [`KEYED`], and anything else is [`PAGE`], which hears no keys.
fn serve() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to serve on");
    let address = listener.local_addr().expect("an address");
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut head = [0u8; 2048];
            let read = stream.read(&mut head).unwrap_or(0);
            let request = &head[..read];
            let body = if request.starts_with(b"GET /pick") {
                PICK
            } else if request.starts_with(b"GET /cut") || request.starts_with(b"GET /key") {
                KEYED
            } else {
                PAGE
            };
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(answer.as_bytes());
        }
    });
    format!("http://{address}/")
}

/// One terminal pane: the program on its pty, and the terminal reading it.
struct Pane {
    pty: Pty,
    terminal: tos_term::Terminal,
    /// How many graphics commands it has written.
    pictures: usize,
    /// How many frames it has drawn: the first command of each, `a=T`,
    /// whether the rest of it comes in more commands or none
    /// (`src/graphics.rs`).
    frames: usize,
    /// The end of what was read last, so that a frame's first bytes split
    /// across two reads are counted once.
    tail: Vec<u8>,
}

impl Pane {
    fn start(command: Command) -> Pane {
        let pty = Pty::spawn(command, COLS, ROWS, CELL);
        let mut terminal = tos_term::Terminal::new(
            COLS as usize,
            ROWS as usize,
            tos_term::TerminalConfig::default(),
        );
        terminal.set_medium_reader(Box::new(ImageFiles::system()));
        Pane {
            pty,
            terminal,
            pictures: 0,
            frames: 0,
            tail: Vec::new(),
        }
    }

    /// Read and draw for up to `within`, answering what the program asks,
    /// until `done` says so.
    fn pump(&mut self, within: Duration, mut done: impl FnMut(&Pane) -> bool) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if done(self) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.pty.ended {
                return done(self);
            }
            let bytes = self.pty.read(left.min(Duration::from_millis(100)));
            self.pictures += bytes.windows(3).filter(|w| *w == b"\x1b_G").count();
            self.count_frames(&bytes);
            self.terminal.advance(&bytes);
            let answer = self.terminal.take_output();
            if !answer.is_empty() {
                self.pty.write(&answer);
            }
        }
    }

    /// Count the frames whose first command starts in `bytes`, or in the
    /// end of the read before and `bytes`.
    fn count_frames(&mut self, bytes: &[u8]) {
        const FRAME: &[u8] = b"\x1b_Ga=T";
        let keep = FRAME.len() - 1;
        // The seam: at most `keep` bytes each side, so that no frame wholly
        // inside `bytes` is counted here as well as below.
        let mut seam = std::mem::take(&mut self.tail);
        seam.extend_from_slice(&bytes[..bytes.len().min(keep)]);
        let count = |b: &[u8]| b.windows(FRAME.len()).filter(|w| *w == FRAME).count();
        self.frames += count(&seam) + count(bytes);
        let end = if bytes.len() >= keep {
            bytes
        } else {
            &seam[..]
        };
        self.tail = end[end.len().saturating_sub(keep)..].to_vec();
    }

    /// What the top row — the status row — reads.
    fn row(&self) -> String {
        self.terminal.grid().row(0).to_text()
    }
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

fn lock_free_within(dir: &Path, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while !lock_free(dir) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
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

/// The controlling terminal proc(5) gives a process (field 7), `0` for
/// none. Linux only.
fn tty_nr(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat[stat.rfind(')')? + 1..]
        .split_whitespace()
        .nth(4)
        .map(str::to_string)
}

/// Stop a backend a failed test left running.
fn stop(pid: Option<u32>) {
    if let Some(pid) = pid {
        // SAFETY: two integers, no memory; a pid the profile's lock named.
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
}

/// One terminal: the row comes up with the page's url on it, the page is
/// drawn, and `ctrl+q` ends the program with status 0 and — once the backend
/// it started has written the profile out — the lock let go. The backend
/// had no terminal while the frontend did.
#[test]
fn a_status_row_appears_and_ctrl_q_exits_0_and_releases_the_lock() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("one");
    let profile = scratch.0.join("profile");
    let page = serve();
    let port = page
        .trim_end_matches('/')
        .rsplit(':')
        .next()
        .unwrap()
        .to_string();
    let args = [
        "--no-config",
        "--no-probe",
        "--frames",
        "raw",
        "--tmux",
        "off",
        "--profile",
        profile.to_str().unwrap(),
        page.as_str(),
    ];
    let mut pane = Pane::start(scratch.command(&args, "one"));
    let up = pane.pump(PATIENCE, |p| p.row().contains(&port) && p.pictures >= 3);
    let backend = lock_holder(&profile);
    if !up {
        stop(backend);
    }
    assert!(
        up,
        "row {:?}, {} pictures; stderr: {}",
        pane.row(),
        pane.pictures,
        scratch.stderr("one")
    );
    eprintln!("row: {:?}, {} pictures", pane.row(), pane.pictures);
    let backend = backend.expect("a backend holds the profile");
    assert_ne!(backend, pane.pty.child.id(), "a process of its own");
    if cfg!(target_os = "linux") {
        assert_ne!(
            tty_nr(pane.pty.child.id()).as_deref(),
            Some("0"),
            "the frontend has the pty"
        );
        assert_eq!(
            tty_nr(backend).as_deref(),
            Some("0"),
            "the backend has none"
        );
    }

    // A paste whose end never comes is given up on by the frontend after two
    // quiet seconds, and the row — the backend's — says so.
    //
    // The wait is that idle and then room for the row's text to come down
    // the pane behind frames: a text the backend queues while a frame is
    // being written waits for it, and for at most one more (two are kept in
    // flight). Over ssh, which is how a Mac runs this (see
    // `Scratch::command`), a raw frame is some 470 KB of base64 inline;
    // read as `Pty::read` reads, with poll(2), that is tens of milliseconds
    // each, so the rest is slack for a runner that descheduled the program
    // or this test for a while (#114).
    let since = Instant::now();
    pane.pty.write(b"\x1b[200~half a paste");
    let cut = pane.pump(
        blinkterm::frontend::PASTE_IDLE + Duration::from_secs(8),
        |p| p.row().contains("paste cut short"),
    );
    let after = since.elapsed();
    eprintln!("paste cut said after {after:?}");
    assert!(cut, "row {:?}", pane.row());
    assert!(
        after >= blinkterm::frontend::PASTE_IDLE,
        "cut after {after:?}, before the paste had been quiet for long"
    );

    pane.pty.write(CTRL_Q);
    pane.pump(Duration::from_secs(5), |p| p.pty.ended);
    let status = pane.pty.exit_within(Duration::from_secs(5));
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(0),
        "stderr: {}",
        scratch.stderr("one")
    );
    let freed = lock_free_within(&profile, Duration::from_secs(10));
    if !freed {
        stop(Some(backend));
    }
    assert!(freed, "the lock was not let go");
}

/// Two terminals on one named profile: the second gets a window of its own
/// — its own row, with the profile's name on it and its own page's url —
/// from the backend the first started; `ctrl+q` in the first closes only
/// that window, and the second goes on drawing until it is closed too.
#[test]
fn a_second_terminal_on_the_profile_gets_its_own_row_and_outlives_the_first() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("two");
    // Two profiles, so that the row names the one in use.
    for (name, dir) in [("Work", "work"), ("Play", "play")] {
        let dir = scratch.0.join(dir);
        let out = scratch
            .command(
                &["profiles", "create", name, "--dir", dir.to_str().unwrap()],
                "create",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .expect("profiles create");
        assert!(out.success(), "{}", scratch.stderr("create"));
    }
    let profile = scratch.0.join("work");
    let page = serve();
    let start = |url: &str, what: &str| {
        let args = [
            "--no-config",
            "--no-probe",
            "--frames",
            "raw",
            "--tmux",
            "off",
            "--profile-name",
            "Work",
            url,
        ];
        Pane::start(scratch.command(&args, what))
    };

    let mut first = start(&format!("{page}first-pane"), "first");
    let up = first.pump(PATIENCE, |p| {
        p.row().contains("first-pane") && p.pictures >= 3
    });
    let backend = lock_holder(&profile);
    if !up {
        stop(backend);
    }
    assert!(
        up,
        "first row {:?}; stderr: {}",
        first.row(),
        scratch.stderr("first")
    );
    assert!(first.row().contains("Work"), "{:?}", first.row());

    let mut second = start(&format!("{page}second-pane"), "second");
    let up = second.pump(PATIENCE, |p| {
        p.row().contains("second-pane") && p.pictures >= 3
    });
    if !up {
        stop(backend);
    }
    assert!(
        up,
        "second row {:?}; stderr: {}",
        second.row(),
        scratch.stderr("second")
    );
    eprintln!("rows: {:?} / {:?}", first.row(), second.row());
    assert!(second.row().contains("Work"), "{:?}", second.row());
    assert!(!second.row().contains("first-pane"), "{:?}", second.row());
    assert_eq!(lock_holder(&profile), backend, "the same backend");
    // The first window is not disturbed by the second.
    first.pump(Duration::from_millis(500), |_| false);
    assert!(first.row().contains("first-pane"), "{:?}", first.row());
    assert!(!first.row().contains("second-pane"), "{:?}", first.row());

    first.pty.write(CTRL_Q);
    first.pump(Duration::from_secs(5), |p| p.pty.ended);
    let status = first.pty.exit_within(Duration::from_secs(5));
    assert_eq!(status.and_then(|s| s.code()), Some(0));

    let before = second.pictures;
    second.pump(Duration::from_secs(2), |_| false);
    assert!(
        second.pictures >= before + 10,
        "the second pane stopped drawing: {} pictures in 2 s",
        second.pictures - before
    );
    assert!(second.pty.child.try_wait().ok().flatten().is_none());
    assert!(!lock_free(&profile), "the profile was let go");
    assert_eq!(lock_holder(&profile), backend);

    second.pty.write(CTRL_Q);
    second.pump(Duration::from_secs(5), |p| p.pty.ended);
    let status = second.pty.exit_within(Duration::from_secs(5));
    assert_eq!(status.and_then(|s| s.code()), Some(0));
    let freed = lock_free_within(&profile, Duration::from_secs(10));
    if !freed {
        stop(backend);
    }
    assert!(freed, "the lock was not let go");
}

/// Whether `pid` is a process that is running, not a zombie.
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

/// A terminal window closed under its frontend hangs up on it, and the
/// frontend goes; the backend, in a session of its own, hears no hang-up,
/// and the other pane goes on drawing. Then the last frontend is killed
/// outright: its window waits out the grace, its tabs are saved as a lost
/// group, and with nothing left to serve the backend stops and lets go of
/// the profile.
#[test]
fn a_terminal_closed_or_killed_outright_costs_only_its_own_window() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("outright");
    let profile = scratch.0.join("profile");
    let page = serve();
    let start = |url: &str, what: &str| {
        let args = [
            "--no-config",
            "--no-probe",
            "--frames",
            "raw",
            "--tmux",
            "off",
            "--profile",
            profile.to_str().unwrap(),
            url,
        ];
        Pane::start(scratch.command(&args, what))
    };
    let mut first = start(&format!("{page}closed-pane"), "first");
    let up = first.pump(PATIENCE, |p| {
        p.row().contains("closed-pane") && p.pictures >= 3
    });
    let backend = lock_holder(&profile);
    if !up {
        stop(backend);
    }
    assert!(
        up,
        "first row {:?}: {}",
        first.row(),
        scratch.stderr("first")
    );
    let mut second = start(&format!("{page}killed-pane"), "second");
    let up = second.pump(PATIENCE, |p| {
        p.row().contains("killed-pane") && p.pictures >= 3
    });
    if !up {
        stop(backend);
    }
    assert!(
        up,
        "second row {:?}: {}",
        second.row(),
        scratch.stderr("second")
    );
    let backend = backend.expect("a backend");
    // Long enough for the session to have the second window's tab.
    second.pump(Duration::from_secs(1), |_| false);

    first.pty.hang_up();
    assert!(
        first.pty.exit_within(Duration::from_secs(5)).is_some(),
        "the frontend outlived its terminal"
    );
    let before = second.pictures;
    second.pump(Duration::from_secs(2), |_| false);
    assert!(
        second.pictures >= before + 10,
        "the other pane stopped drawing: {} pictures in 2 s",
        second.pictures - before
    );
    assert!(alive(backend), "the hang-up reached the backend");
    assert_eq!(lock_holder(&profile), Some(backend));

    // Before the kill: the grace starts no earlier than the kill does.
    let killed = Instant::now();
    let _ = second.pty.child.kill();
    let _ = second.pty.child.wait();
    let deadline = killed + blinkterm::backend::GRACE + Duration::from_secs(15);
    while alive(backend) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let stopped = !alive(backend);
    if !stopped {
        stop(Some(backend));
    }
    assert!(stopped, "the backend did not stop after the grace");
    assert!(
        killed.elapsed() >= blinkterm::backend::GRACE,
        "the backend stopped before the grace was out: {:?}; it said: {}",
        killed.elapsed(),
        std::fs::read_to_string(profile.join("backend.log")).unwrap_or_default()
    );
    assert!(lock_free_within(&profile, Duration::from_secs(5)));
    let session = std::fs::read_to_string(profile.join("session")).expect("a session");
    // Neither window quit: the one whose terminal hung up is as lost as the
    // one whose frontend was killed (#106).
    assert!(
        session.contains("/closed-pane\t")
            && session.contains("/killed-pane\t")
            && session.matches(" lost\n").count() == 2
            && !session.contains(" closed\n"),
        "{session}"
    );
}

/// Wait up to `within` for `pid` to be gone, and say whether it is.
fn gone_within(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    !alive(pid)
}

/// The rule for what is offered back (#106). The only window's terminal
/// hangs up: that is not a quit, so the backend, with nothing left to serve,
/// stops with the window's tabs a lost group, and the next window on the
/// profile is offered them. Declined, and that window quit with `ctrl+q`:
/// the start after it offers nothing, and `--restore` brings the quit
/// window's tabs back.
#[test]
fn a_terminal_hung_up_is_offered_back_and_one_quit_with_ctrl_q_waits_for_restore() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("hangup");
    let profile = scratch.0.join("profile");
    let page = serve();
    let start = |url: Option<&str>, restore: bool, what: &str| {
        let mut args = vec![
            "--no-config",
            "--no-probe",
            "--frames",
            "raw",
            "--tmux",
            "off",
            "--profile",
            profile.to_str().unwrap(),
        ];
        if restore {
            args.push("--restore");
        }
        args.extend(url);
        Pane::start(scratch.command(&args, what))
    };
    // A pane quit with `ctrl+q`: status 0, and its backend — the only
    // window was its — gone with the profile let go.
    let quit = |pane: &mut Pane, backend: Option<u32>, what: &str| {
        pane.pty.write(CTRL_Q);
        pane.pump(Duration::from_secs(5), |p| p.pty.ended);
        let status = pane.pty.exit_within(Duration::from_secs(5));
        if status.and_then(|s| s.code()) != Some(0) {
            stop(backend);
        }
        assert_eq!(
            status.and_then(|s| s.code()),
            Some(0),
            "stderr: {}",
            scratch.stderr(what)
        );
        let freed = lock_free_within(&profile, Duration::from_secs(15));
        if !freed {
            stop(backend);
        }
        assert!(freed, "the lock was not let go after {what}");
    };

    let mut hung = start(Some(&format!("{page}hung-pane")), false, "hung");
    let up = hung.pump(PATIENCE, |p| {
        p.row().contains("hung-pane") && p.pictures >= 3
    });
    let backend = lock_holder(&profile);
    if !up {
        stop(backend);
    }
    assert!(up, "row {:?}: {}", hung.row(), scratch.stderr("hung"));
    let backend = backend.expect("a backend");
    // Long enough for the session to have the window's tab.
    hung.pump(Duration::from_secs(1), |_| false);

    hung.pty.hang_up();
    assert!(
        hung.pty.exit_within(Duration::from_secs(5)).is_some(),
        "the frontend outlived its terminal"
    );
    let stopped = gone_within(backend, Duration::from_secs(20));
    if !stopped {
        stop(Some(backend));
    }
    assert!(stopped, "the backend did not stop with no window left");
    assert!(lock_free_within(&profile, Duration::from_secs(5)));
    let session = std::fs::read_to_string(profile.join("session")).expect("a session");
    assert!(
        session.starts_with("# blinkterm session: open\n")
            && session.contains(" lost\n")
            && session.contains("/hung-pane\t")
            && !session.contains(" closed\n"),
        "a hang-up is not a quit: {session}"
    );

    // Offered back, and declined.
    let mut next = start(Some(&format!("{page}next-pane")), false, "next");
    let offered = next.pump(PATIENCE, |p| {
        p.row().contains("restore 1 tab from last time?")
    });
    let backend = lock_holder(&profile);
    if !offered {
        stop(backend);
    }
    assert!(
        offered,
        "the hung-up window's tab was not offered: row {:?}: {}",
        next.row(),
        scratch.stderr("next")
    );
    next.pty.write(b"n");
    let up = next.pump(PATIENCE, |p| {
        p.row().contains("next-pane") && p.pictures >= 3
    });
    if !up {
        stop(backend);
    }
    assert!(up, "row {:?}: {}", next.row(), scratch.stderr("next"));
    next.pump(Duration::from_secs(1), |_| false);
    quit(&mut next, backend, "next");

    // A quit is not offered back.
    let mut third = start(Some(&format!("{page}third-pane")), false, "third");
    let mut asked = false;
    let up = third.pump(PATIENCE, |p| {
        asked |= p.row().contains("restore");
        p.row().contains("third-pane") && p.pictures >= 3
    });
    third.pump(Duration::from_secs(1), |p| {
        asked |= p.row().contains("restore");
        false
    });
    let backend = lock_holder(&profile);
    if !up || asked {
        stop(backend);
    }
    assert!(up, "row {:?}: {}", third.row(), scratch.stderr("third"));
    assert!(!asked, "a window quit with ctrl+q was offered back");
    quit(&mut third, backend, "third");

    // `--restore` brings the window that quit back.
    let mut restored = start(None, true, "restored");
    let up = restored.pump(PATIENCE, |p| p.row().contains("third-pane"));
    let backend = lock_holder(&profile);
    if !up {
        stop(backend);
    }
    assert!(
        up,
        "--restore did not reopen the quit window: row {:?}: {}",
        restored.row(),
        scratch.stderr("restored")
    );
    quit(&mut restored, backend, "restored");
}

/// A backend killed outright under a pane (#103). The frontend sees its
/// link drop and would take its window back from a backend still there,
/// but nobody listens and nobody holds the profile: the backend is gone for
/// good, and the pane ends at once — not after the fifteen seconds it would
/// wait for one that is coming back — with the sentence saying where its
/// log is, and a status that is not success.
///
/// The other half, a link dropped under a backend that is still there and
/// the window taken back, is
/// [`a_link_cut_under_a_pane_takes_the_window_back_and_drops_what_was_typed_meanwhile`].
#[test]
fn a_backend_that_dies_under_a_pane_ends_it_with_the_sentence_within_bounds() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("died");
    let profile = scratch.0.join("profile");
    let page = serve();
    let args = [
        "--no-config",
        "--no-probe",
        "--frames",
        "raw",
        "--tmux",
        "off",
        "--profile",
        profile.to_str().unwrap(),
        page.as_str(),
    ];
    let mut pane = Pane::start(scratch.command(&args, "died"));
    let up = pane.pump(PATIENCE, |p| p.pictures >= 3);
    let backend = lock_holder(&profile);
    if !up {
        stop(backend);
    }
    assert!(up, "row {:?}: {}", pane.row(), scratch.stderr("died"));
    let backend = backend.expect("a backend holds the profile");
    let group = std::fs::read_to_string(profile.join(engine::PGID_FILE))
        .ok()
        .and_then(|text| engine::parse_pgid_marker(&text))
        .map(|(group, _)| group);

    // SAFETY: two integers, no memory; the pid the profile's lock named.
    unsafe {
        libc::kill(backend as i32, libc::SIGKILL);
    }
    let killed = Instant::now();
    pane.pump(Duration::from_secs(10), |p| p.pty.ended);
    let status = pane.pty.exit_within(Duration::from_secs(5));
    let took = killed.elapsed();
    if let Some(group) = group {
        // What the kill left of the engine, not left for the next test.
        // SAFETY: two integers, no memory; the group the backend wrote.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    let said = scratch.stderr("died");
    assert!(
        status.is_some_and(|s| s.code().is_some_and(|code| code != 0)),
        "{status:?}: {said}"
    );
    assert!(
        took < Duration::from_secs(5),
        "took {took:?}, as if it waited for a backend that is not coming"
    );
    assert!(
        said.contains("stopped unexpectedly") && said.contains("backend.log"),
        "{said}"
    );
    assert!(lock_free_within(&profile, Duration::from_secs(5)));
}

/// Whether to assert on a time: not on GitHub's shared macOS VM, which can
/// be descheduled for over 100 ms (`tests/engine.rs` has the story), saying
/// so.
fn timing_asserted(what: &str) -> bool {
    if cfg!(target_os = "macos") && std::env::var_os("BLINKTERM_SHARED_RUNNER").is_some() {
        eprintln!("skipped on the shared macOS runner: {what} asserts on time");
        return false;
    }
    true
}

/// Send `pid` the signal `sig`.
fn signal(pid: u32, sig: libc::c_int) {
    // SAFETY: two integers, no memory; a pid the profile's lock named.
    unsafe {
        libc::kill(pid as i32, sig);
    }
}

/// Cut the link between the frontend `pid` and the backend `peer` from
/// outside, killing neither: every Unix socket of `pid`'s whose other end
/// is `peer`'s, or a pair `pid` made itself, is shut down both ways, and
/// how many is the answer.
///
/// Nothing else does it. A frontend stopped with `SIGSTOP` does not make
/// the backend give up on it: a window's frames replace each other in the
/// outbox rather than queueing up to its cap, and a stalled window stops
/// being cast to after five seconds, so the connection stays up however
/// long the frontend is stopped. And a way into the backend to drop a
/// window's connection would be a feature in the program for the sake of a
/// test. So the test reaches into the frontend: `pidfd_getfd(2)` gives it a
/// copy of the frontend's descriptor — the same socket, not a new one — and
/// `shutdown(2)` acts on the socket rather than the descriptor, so both
/// ends read their end exactly as when the link drops. `SO_PEERCRED` says
/// whose the other end is: the backend's for a connection to its socket,
/// and the frontend's own for the pair it started the backend with (a
/// pair's credentials are its maker's).
///
/// `pidfd_getfd` needs Linux 5.6, the same user, and leave to ptrace `pid`,
/// which Yama's scope 1 gives a test over its own descendants and scope 2
/// or 3 does not; a container's default seccomp profile allows it only with
/// `CAP_SYS_PTRACE`. Refused, it is `EPERM`, or `ENOSYS` where the kernel
/// has no such call.
#[cfg(target_os = "linux")]
fn cut_link(pid: u32, peer: u32) -> io::Result<usize> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    // SAFETY: `pidfd_open(2)` takes a pid and a flag word and reads no
    // memory.
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if pidfd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a descriptor the call above just opened, which nothing else
    // has, closed once by the `OwnedFd`.
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd as i32) };
    let mut cut = 0;
    for entry in std::fs::read_dir(format!("/proc/{pid}/fd"))? {
        let entry = entry?;
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        if !target.to_string_lossy().starts_with("socket:") {
            continue;
        }
        let Some(fd) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        // SAFETY: `pidfd_getfd(2)` takes a pidfd, a descriptor number in
        // that process and a flag word, and reads no memory of ours; the
        // pidfd is open for the call.
        let copy = unsafe { libc::syscall(libc::SYS_pidfd_getfd, pidfd.as_raw_fd(), fd, 0) };
        if copy < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EBADF) {
                // Closed since the directory was read.
                continue;
            }
            return Err(err);
        }
        // SAFETY: a descriptor the call above just made in this process,
        // which nothing else has, closed once by the `OwnedFd`.
        let copy = unsafe { OwnedFd::from_raw_fd(copy as i32) };
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: `getsockopt(2)` writes at most `len` bytes through the
        // pointer, a live local of exactly that size, and the length it used
        // through `len`, another live local; the descriptor is `copy`'s,
        // open for the call.
        let read = unsafe {
            libc::getsockopt(
                copy.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::addr_of_mut!(cred).cast(),
                &mut len,
            )
        };
        if read != 0 || cred.pid <= 0 {
            // Not a Unix socket, or not a connected one.
            continue;
        }
        if cred.pid as u32 == peer || cred.pid as u32 == pid {
            // SAFETY: `shutdown(2)` takes a descriptor and a flag word and
            // reads no memory; the descriptor is `copy`'s, open for the call.
            if unsafe { libc::shutdown(copy.as_raw_fd(), libc::SHUT_RDWR) } != 0 {
                return Err(io::Error::last_os_error());
            }
            cut += 1;
        }
    }
    Ok(cut)
}

/// Elsewhere there is no taking a descriptor out of another process.
#[cfg(not(target_os = "linux"))]
fn cut_link(_pid: u32, _peer: u32) -> io::Result<usize> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "pidfd_getfd(2) is Linux's",
    ))
}

/// Whether a refusal from [`cut_link`] is a machine that cannot do it,
/// which skips the test saying so, rather than a failure. Not on GitHub's
/// Linux runner, which is set up to allow it (`.github/workflows/ci.yml`):
/// there a refusal fails, so that the test is never skipped there unseen.
fn cut_refused(err: &io::Error) -> bool {
    let refused = err.kind() == io::ErrorKind::Unsupported
        || err.raw_os_error() == Some(libc::EPERM)
        || err.raw_os_error() == Some(libc::ENOSYS);
    if !refused || (cfg!(target_os = "linux") && std::env::var_os("GITHUB_ACTIONS").is_some()) {
        return false;
    }
    eprintln!("skipped: pidfd_getfd refused ({err}); the link cannot be cut from outside");
    true
}

/// A link dropped under a backend that is still there (#103, #117): the
/// frontend takes its window back — the same window, on the same backend,
/// with no second one opened and nothing saved as lost — and what was typed
/// while it had none is not sent to the page.
///
/// The backend is stopped before the cut and let go a second later, so that
/// there is a gap to type into: the frontend reads its link's end, connects
/// again — the socket's backlog takes it — and waits for the backend to say
/// hello, while `x` sits in the pty. Let go, the backend reads the old
/// connection's end and suspends the window, then answers the new one,
/// which asks for it back by its nonce; the frontend drops what it read in
/// the gap, `x` among it. `y`, typed once the window is back, reaches the
/// page.
#[test]
fn a_link_cut_under_a_pane_takes_the_window_back_and_drops_what_was_typed_meanwhile() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("cut");
    let profile = scratch.0.join("profile");
    let page = serve();
    let url = format!("{page}cut-pane");
    let args = [
        "--no-config",
        "--no-probe",
        "--frames",
        "raw",
        "--tmux",
        "off",
        "--profile",
        profile.to_str().unwrap(),
        url.as_str(),
    ];
    let mut pane = Pane::start(scratch.command(&args, "cut"));
    let up = pane.pump(PATIENCE, |p| p.row().contains("cut-pane") && p.frames >= 3);
    let backend = lock_holder(&profile);
    if !up {
        stop(backend);
    }
    assert!(up, "row {:?}: {}", pane.row(), scratch.stderr("cut"));
    let backend = backend.expect("a backend holds the profile");
    let frontend = pane.pty.child.id();
    // Long enough for the session to have the window's tab.
    pane.pump(Duration::from_secs(1), |_| false);

    signal(backend, libc::SIGSTOP);
    let cut = match cut_link(frontend, backend) {
        Ok(cut) => cut,
        Err(err) => {
            signal(backend, libc::SIGCONT);
            if cut_refused(&err) {
                pane.pty.write(CTRL_Q);
                pane.pump(Duration::from_secs(5), |p| p.pty.ended);
                let _ = pane.pty.exit_within(Duration::from_secs(5));
                if !lock_free_within(&profile, Duration::from_secs(10)) {
                    stop(Some(backend));
                }
                return;
            }
            stop(Some(backend));
            panic!("cannot cut the link: {err}");
        }
    };
    if cut != 1 {
        signal(backend, libc::SIGCONT);
        stop(Some(backend));
    }
    assert_eq!(cut, 1, "the frontend's links to the backend");

    // Typed while the window is gone and the backend not yet answering.
    pane.pump(Duration::from_millis(300), |_| false);
    pane.pty.write(b"x");
    pane.pump(Duration::from_millis(700), |_| false);
    signal(backend, libc::SIGCONT);

    let before = pane.frames;
    let mut saw_x = false;
    let back = pane.pump(PATIENCE, |p| {
        saw_x |= p.row().contains("key-x");
        saw_x || (p.frames >= before + 3 && p.row().contains("cut-pane"))
    });
    if !back || saw_x {
        stop(Some(backend));
    }
    assert!(!saw_x, "a key typed in the gap reached the page");
    assert!(
        back,
        "the window did not come back: row {:?}, {} frames since: {}",
        pane.row(),
        pane.frames - before,
        scratch.stderr("cut")
    );
    pane.pty.write(b"y");
    let heard = pane.pump(PATIENCE, |p| {
        saw_x |= p.row().contains("key-x");
        p.row().contains("key-y")
    });
    if !heard {
        stop(Some(backend));
    }
    assert!(heard, "a key after the gap was not heard: {:?}", pane.row());
    // Long enough for an `x` let through after all to have arrived, and for
    // the session to have the tab's url.
    pane.pump(Duration::from_secs(1), |p| {
        saw_x |= p.row().contains("key-x");
        false
    });
    if saw_x {
        stop(Some(backend));
    }
    assert!(!saw_x, "a key typed in the gap reached the page");
    assert_eq!(lock_holder(&profile), Some(backend), "the same backend");
    assert!(
        pane.pty.child.try_wait().ok().flatten().is_none(),
        "the frontend ended: {}",
        scratch.stderr("cut")
    );
    let log = std::fs::read_to_string(profile.join("backend.log")).unwrap_or_default();
    assert!(log.contains("lost its terminal"), "{log}");
    let said = scratch.stderr("cut");
    assert!(!said.contains("stopped unexpectedly"), "{said}");

    pane.pty.write(CTRL_Q);
    pane.pump(Duration::from_secs(5), |p| p.pty.ended);
    let status = pane.pty.exit_within(Duration::from_secs(5));
    if status.and_then(|s| s.code()) != Some(0) {
        stop(Some(backend));
    }
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(0),
        "stderr: {}",
        scratch.stderr("cut")
    );
    let freed = lock_free_within(&profile, Duration::from_secs(10));
    if !freed {
        stop(Some(backend));
    }
    assert!(freed, "the lock was not let go");
    // One window, taken back rather than opened again, and quit.
    let session = std::fs::read_to_string(profile.join("session")).expect("a session");
    assert!(
        session.contains("/key-y\t")
            && !session.contains("/key-x\t")
            && !session.contains(" lost\n")
            && session.matches("# window ").count() == 1,
        "{session}"
    );
}

/// A picker that takes the terminal (#105, #117): the window paints
/// nothing while it runs — longer than the five seconds after which a
/// window that has stopped acknowledging is no longer cast to — and has a
/// fresh frame within a second of its exit: the terminal's size is said
/// before the answer, so the backend lays the window out and casts again at
/// once rather than leaving it blank. The picker exits 0 with nothing
/// written, which is the person cancelling, and the page hears `cancel`.
#[test]
fn a_terminal_picker_gives_the_pane_a_fresh_frame_within_a_second_of_exiting() {
    if !engine_named() {
        return;
    }
    let scratch = Scratch::new("picker");
    let profile = scratch.0.join("profile");
    let started = scratch.0.join("START");
    let done = scratch.0.join("DONE");
    let page = serve();
    // Six seconds: longer than the backend's stall.
    let picker = format!(
        "sh -c 'touch {}; sleep 6; touch {}'",
        started.display(),
        done.display()
    );
    let url = format!("{page}pick");
    let args = [
        "--no-config",
        "--no-probe",
        "--frames",
        "raw",
        "--tmux",
        "off",
        "--profile",
        profile.to_str().unwrap(),
        "--file-picker-terminal",
        picker.as_str(),
        url.as_str(),
    ];
    let mut command = scratch.command(&args, "picker");
    command.env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY");
    let mut pane = Pane::start(command);
    let up = pane.pump(PATIENCE, |p| p.row().contains("/pick") && p.frames >= 3);
    let backend = lock_holder(&profile);
    if !up {
        stop(backend);
    }
    assert!(up, "row {:?}: {}", pane.row(), scratch.stderr("picker"));

    // The key clicks the input, and the picker has the terminal.
    pane.pty.write(b"p");
    let asked = pane.pump(PATIENCE, |_| started.exists());
    if !asked {
        stop(backend);
    }
    assert!(
        asked,
        "the picker did not start: row {:?}: {}",
        pane.row(),
        scratch.stderr("picker")
    );
    // What was written before the picker started and is still on its way.
    pane.pump(Duration::from_millis(300), |_| false);
    let at_start = pane.frames;
    let mut painted = 0;
    let exited = pane.pump(Duration::from_secs(10), |p| {
        painted = painted.max(p.frames - at_start);
        done.exists()
    });
    if !exited {
        stop(backend);
    }
    assert!(exited, "the picker did not finish");
    assert_eq!(painted, 0, "frames were drawn on a terminal a picker had");

    let exited_at = std::fs::metadata(&done)
        .and_then(|m| m.modified())
        .expect("when the picker finished");
    let fresh = pane.pump(Duration::from_secs(5), |p| p.frames > at_start);
    let took = exited_at.elapsed().unwrap_or_default();
    if !fresh {
        stop(backend);
    }
    assert!(
        fresh,
        "no frame after the picker: {}",
        scratch.stderr("picker")
    );
    eprintln!("the first frame after the picker: {took:?}");
    if timing_asserted("the first frame after the picker") {
        assert!(
            took < Duration::from_secs(1),
            "the first frame after the picker took {took:?}"
        );
    }

    // Nothing written is a cancel, and the window goes on painting.
    let cancelled = pane.pump(PATIENCE, |p| p.row().contains("/pick-cancelled"));
    if !cancelled {
        stop(backend);
    }
    assert!(cancelled, "the page heard no cancel: {:?}", pane.row());
    let before = pane.frames;
    pane.pump(Duration::from_secs(2), |_| false);
    if pane.frames < before + 10 {
        stop(backend);
    }
    assert!(
        pane.frames >= before + 10,
        "the pane stopped drawing: {} frames in 2 s",
        pane.frames - before
    );

    pane.pty.write(CTRL_Q);
    pane.pump(Duration::from_secs(5), |p| p.pty.ended);
    let status = pane.pty.exit_within(Duration::from_secs(5));
    if status.and_then(|s| s.code()) != Some(0) {
        stop(backend);
    }
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(0),
        "stderr: {}",
        scratch.stderr("picker")
    );
    let freed = lock_free_within(&profile, Duration::from_secs(10));
    if !freed {
        stop(backend);
    }
    assert!(freed, "the lock was not let go");
}
