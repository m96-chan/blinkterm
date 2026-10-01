//! `blinkterm profiles` and the profile selectors, run as a person runs
//! them: the binary itself, in a scratch `$XDG_DATA_HOME` and `$HOME`, with
//! no settings file and stdin from nowhere, so that nothing can wait on a
//! picker. No engine and no terminal: everything here is decided before
//! either would be wanted, and `--remote` is answered by a socket this test
//! listens on in the running blinkterm's place, as in `tests/remote.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use blinkterm::profile::{Choice, Profile};
use blinkterm::registry::Registry;
use blinkterm::remote::Listener;

/// A scratch `$XDG_DATA_HOME` and `$HOME` of this test's own, gone again
/// before.
fn scratch(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blinkterm-it-profiles-{what}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).expect("a scratch data directory");
    std::fs::create_dir_all(dir.join("home")).expect("a scratch home");
    dir
}

/// Where the registry and the profiles are, under `scratch`.
fn data(scratch: &Path) -> PathBuf {
    scratch.join("data").join("blinkterm")
}

fn command(scratch: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_blinkterm"));
    command
        .args(args)
        .env("XDG_DATA_HOME", scratch.join("data"))
        .env("HOME", scratch.join("home"))
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// The binary with `args`, run to the end.
fn blinkterm(scratch: &Path, args: &[&str]) -> Output {
    command(scratch, args).output().expect("the binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `args`, which must succeed, and what it printed.
fn ok(scratch: &Path, args: &[&str]) -> String {
    let out = blinkterm(scratch, args);
    assert!(
        out.status.success(),
        "{args:?}: {}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    text(&out.stdout)
}

/// `args`, which must fail with `code`, and what it said on stderr.
fn fails(scratch: &Path, args: &[&str], code: i32) -> String {
    let out = blinkterm(scratch, args);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(code), "{args:?}: {stderr}");
    stderr
}

#[test]
fn the_profiles_command_makes_renames_defaults_lists_and_removes() {
    let dir = scratch("round-trip");
    let data = data(&dir);
    // What an earlier blinkterm left: the profile, with a login in it.
    std::fs::create_dir_all(data.join("profile").join("Default")).expect("a profile");
    std::fs::write(data.join("profile").join("Default").join("Cookies"), b"x").expect("a login");

    let list = ok(&dir, &["profiles"]);
    assert_eq!(
        list,
        format!("* Default   {}\n", data.join("profile").display())
    );
    let json = std::fs::read_to_string(data.join("profiles.json")).expect("migrated");
    assert!(json.contains("\"version\": 1"), "{json}");
    assert!(json.contains("\"dir\": \"profile\""), "{json}");
    assert!(
        data.join("profile")
            .join("Default")
            .join("Cookies")
            .is_file(),
        "nothing moved"
    );

    let made = ok(&dir, &["profiles", "create", "Work"]);
    assert!(made.starts_with("created Work at "), "{made}");
    let registry = Registry::load(&data).expect("read").expect("there");
    let work = registry.by_name("Work").expect("made");
    let work_dir = registry.dir_of(work);
    assert!(made.contains(&work_dir.display().to_string()), "{made}");
    assert!(work_dir.is_dir());

    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).expect("a directory");
    let outside = std::fs::canonicalize(&outside).expect("there");
    let registered = ok(
        &dir,
        &[
            "profiles",
            "create",
            "Testing",
            "--dir",
            outside.to_str().unwrap(),
        ],
    );
    assert!(
        registered.contains("not managed by blinkterm"),
        "{registered}"
    );

    assert_eq!(
        ok(&dir, &["profiles", "rename", "work", "Office"]),
        "renamed Work to Office\n"
    );
    assert_eq!(
        ok(&dir, &["profiles", "default", "office"]),
        "Office is the default profile\n"
    );
    let list = ok(&dir, &["profiles", "list"]);
    let lines: Vec<&str> = list.lines().collect();
    assert_eq!(lines.len(), 3, "{list}");
    assert!(lines[0].starts_with("  Default   /"), "{list}");
    assert!(lines[1].starts_with("* Office    /"), "{list}");
    assert!(
        lines[2].starts_with("  Testing   /") && lines[2].ends_with("not managed by blinkterm"),
        "{list}"
    );

    let removed = ok(&dir, &["profiles", "remove", "Office"]);
    assert!(
        removed.starts_with("removed Office; its cookies and logins are in "),
        "{removed}"
    );
    assert!(
        removed.contains("there is no default profile now"),
        "{removed}"
    );
    assert!(!work_dir.exists());
    assert!(data.join("trash").is_dir());
    let forgot = ok(&dir, &["profiles", "remove", "Testing"]);
    assert_eq!(
        forgot,
        format!(
            "forgot Testing; {} is left where it is\n",
            outside.display()
        )
    );
    assert!(outside.is_dir());

    let why = fails(&dir, &["profiles", "remove", "Nope"], 1);
    assert!(
        why.starts_with("blinkterm: profiles: no profile named \"Nope\""),
        "{why}"
    );
    let why = fails(&dir, &["profiles", "create", "Default"], 1);
    assert!(
        why.contains("there is already a profile named \"Default\""),
        "{why}"
    );
    let why = fails(&dir, &["profiles", "frob"], 2);
    assert!(why.contains("unknown command \"frob\""), "{why}");
    let why = fails(&dir, &["profiles", "create"], 2);
    assert!(why.contains("profiles create needs a name"), "{why}");
    let help = ok(&dir, &["profiles", "--help"]);
    assert!(help.starts_with("usage: blinkterm profiles"), "{help}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_unknown_name_and_two_selectors_are_refused_before_anything_starts() {
    let dir = scratch("selectors");
    let why = fails(&dir, &["--no-config", "--profile-name", "Nope"], 1);
    assert!(why.contains("no profile named \"Nope\""), "{why}");
    assert!(why.contains("blinkterm profiles create Nope"), "{why}");
    let why = fails(
        &dir,
        &["--no-config", "--profile-name", "x", "--profile", "/y"],
        2,
    );
    assert!(why.contains("one profile at a time"), "{why}");
    let why = fails(&dir, &["--no-config", "--profile-name", "a/b"], 2);
    assert!(
        why.contains("--profile-name: a profile name cannot contain"),
        "{why}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn with_no_default_and_no_terminal_a_start_says_how_to_choose_and_does_not_wait() {
    let dir = scratch("no-default");
    ok(&dir, &["profiles", "remove", "Default"]);
    let why = fails(&dir, &["--no-config", "example.com"], 1);
    assert!(why.contains("no default profile"), "{why}");
    assert!(why.contains("not a terminal"), "{why}");
    assert!(why.contains("--profile-name <name>"), "{why}");
    let why = fails(&dir, &["--no-config", "--choose-profile"], 1);
    assert!(why.contains("--choose-profile needs a terminal"), "{why}");
    let why = fails(
        &dir,
        &["--no-config", "--remote", "--choose-profile", "a.example"],
        2,
    );
    assert!(why.contains("contradiction"), "{why}");
    let why = fails(&dir, &["--no-config", "--remote", "a.example"], 1);
    assert!(
        why.contains("no default profile, and --remote never asks"),
        "{why}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn remote_with_a_profile_name_reaches_that_profile_s_blinkterm() {
    let dir = scratch("remote");
    ok(&dir, &["profiles", "create", "Work"]);
    let data = data(&dir);
    let registry = Registry::load(&data).expect("read").expect("there");
    let work = registry.dir_of(registry.by_name("Work").expect("made"));
    let mut listener = Listener::bind(&work).expect("listening");
    let child = command(
        &dir,
        &[
            "--no-config",
            "--remote",
            "--profile-name",
            "Work",
            "example.com",
        ],
    )
    .spawn()
    .expect("the binary runs");
    let deadline = Instant::now() + Duration::from_secs(10);
    let delivery = loop {
        let mut deliveries = listener.accept_ready().expect("accepting");
        if !deliveries.is_empty() {
            break deliveries.remove(0);
        }
        assert!(Instant::now() < deadline, "nobody connected");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(delivery.lines, [Ok("https://example.com".to_string())]);
    let lines = delivery.lines.clone();
    delivery.answer(&lines);
    let out = child.wait_with_output().expect("it exits");
    assert!(out.status.success(), "{}", text(&out.stderr));
    drop(listener);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_profile_in_use_is_not_removed() {
    let dir = scratch("in-use");
    ok(&dir, &["profiles", "create", "Work"]);
    let data = data(&dir);
    let registry = Registry::load(&data).expect("read").expect("there");
    let work = registry.dir_of(registry.by_name("Work").expect("made"));
    let held = Profile::take(Choice::At(work.clone())).expect("taken");
    let list = ok(&dir, &["profiles", "list"]);
    assert!(
        list.contains(&format!("in use (pid {})", std::process::id())),
        "{list}"
    );
    let why = fails(&dir, &["profiles", "remove", "Work"], 1);
    assert!(why.contains("is in use by another blinkterm"), "{why}");
    assert!(work.is_dir());
    drop(held);
    ok(&dir, &["profiles", "remove", "Work"]);
    std::fs::remove_dir_all(&dir).ok();
}
