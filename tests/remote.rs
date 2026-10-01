//! `blinkterm --remote`, run as the program other programs run: the binary
//! itself, against a socket this test listens on in the running blinkterm's
//! place. No engine and no terminal — a sender needs neither when somebody
//! answers, and says so when nobody does.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use blinkterm::remote::{Delivery, Listener};

/// A scratch profile of this test's own, gone again before and after.
fn scratch(what: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("blinkterm-it-remote-{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// The binary with `args`, its stdout and stderr piped: not a terminal.
fn blinkterm(args: &[&str]) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_blinkterm"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs")
}

/// Wait for the first sender, for up to ten seconds.
fn first_delivery(listener: &mut Listener) -> Delivery {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut deliveries = listener.accept_ready().expect("accepting");
        if !deliveries.is_empty() {
            return deliveries.remove(0);
        }
        assert!(Instant::now() < deadline, "nobody connected");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn sender(dir: &Path, urls: &[&str]) -> std::process::Child {
    let dir = dir.to_str().expect("a scratch path is text");
    let mut args = vec!["--no-config", "--remote", "--profile", dir];
    args.extend(urls);
    blinkterm(&args)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_sender_that_is_answered_exits_at_once_and_says_nothing() {
    let dir = scratch("opened");
    let mut listener = Listener::bind(&dir).expect("listening");
    let child = sender(&dir, &["example.com", "/tmp/x.html"]);
    let delivery = first_delivery(&mut listener);
    assert_eq!(
        delivery.lines,
        [
            Ok("https://example.com".to_string()),
            Ok("file:///tmp/x.html".to_string()),
        ]
    );
    let lines = delivery.lines.clone();
    delivery.answer(&lines);
    let Output {
        status,
        stdout,
        stderr,
    } = child.wait_with_output().expect("it exits");
    assert!(status.success(), "{status}: {}", text(&stderr));
    assert!(stdout.is_empty() && stderr.is_empty(), "{}", text(&stderr));
    drop(listener);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_url_that_is_refused_is_a_line_on_stderr_and_exit_one() {
    let dir = scratch("refused");
    let mut listener = Listener::bind(&dir).expect("listening");
    let child = sender(&dir, &["https://a.example", "javascript:x"]);
    let delivery = first_delivery(&mut listener);
    assert_eq!(delivery.lines.len(), 2);
    assert!(delivery.lines[0].is_ok() && delivery.lines[1].is_err());
    let lines = delivery.lines.clone();
    delivery.answer(&lines);
    let output = child.wait_with_output().expect("it exits");
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(output.stdout.is_empty());
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.starts_with("blinkterm: ") && stderr.contains("javascript"),
        "{stderr}"
    );
    drop(listener);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn with_nobody_running_and_no_terminal_a_sender_says_it_cannot_start_one() {
    let dir = scratch("nobody");
    // What a crash leaves: the file, and nobody on it.
    drop(std::os::unix::net::UnixListener::bind(dir.join("blinkterm.sock")).expect("bound"));
    let output = sender(&dir, &["example.com"])
        .wait_with_output()
        .expect("it exits");
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("no blinkterm is running"), "{stderr}");
    assert!(stderr.contains("not a terminal"), "{stderr}");
    assert!(stderr.contains(&dir.display().to_string()), "{stderr}");
    std::fs::remove_dir_all(&dir).ok();
}

/// A backend whose terminals have all gone — it is waiting out their grace —
/// has nowhere to put a page, and says `nowhere`; the sender then starts a
/// terminal of its own, as it does with nobody listening, and with no
/// terminal to start one in says so.
#[test]
fn a_backend_with_no_window_answers_nowhere_and_the_sender_starts_one() {
    let dir = scratch("nowhere");
    let mut listener = Listener::bind(&dir).expect("listening");
    let child = sender(&dir, &["example.com"]);
    let delivery = first_delivery(&mut listener);
    assert_eq!(delivery.lines, [Ok("https://example.com".to_string())]);
    delivery.nowhere();
    let output = child.wait_with_output().expect("it exits");
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("not a terminal"), "{stderr}");
    assert!(stderr.contains(&dir.display().to_string()), "{stderr}");
    drop(listener);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn remote_with_no_url_is_a_mistake_on_the_command_line() {
    let output = blinkterm(&["--no-config", "--remote"])
        .wait_with_output()
        .expect("it exits");
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("needs a url"), "{stderr}");
}
