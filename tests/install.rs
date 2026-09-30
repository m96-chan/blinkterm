//! `blinkterm --install-engine`, run as a person runs it: the binary itself,
//! against Chrome for Testing, into a scratch `$XDG_DATA_HOME` and `$HOME`
//! so that nothing lands in the real ones.
//!
//! Not in `tests/engine.rs`: those tests drive the library against the
//! engine `$BLINKTERM_ENGINE` names, and this one must run the binary with
//! that variable removed, so that the search can be satisfied by nothing but
//! what `--install-engine` put there.
//!
//! It downloads about 100 MB, so it runs only when `BLINKTERM_NETWORK` is
//! set, on the same principle as naming an engine is the consent to start
//! one: CI sets it in the two engine jobs. Without it every test here says
//! it was skipped and passes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use blinkterm::install;

/// Whether this run may reach the network, with a line when it may not.
fn network_allowed() -> bool {
    if std::env::var_os("BLINKTERM_NETWORK").is_some() {
        return true;
    }
    eprintln!(
        "skipped: BLINKTERM_NETWORK is not set; this test downloads ~100 MB from Chrome for Testing"
    );
    false
}

/// A scratch directory of this test's own, gone again before.
fn scratch(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blinkterm-it-install-{what}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).expect("a scratch data directory");
    std::fs::create_dir_all(dir.join("home")).expect("a scratch home");
    dir
}

/// The binary with `args`, `$XDG_DATA_HOME` and `$HOME` under `scratch`,
/// and no `$BLINKTERM_ENGINE` unless `engine` names one.
fn blinkterm(scratch: &Path, args: &[&str], engine: Option<&str>, path: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_blinkterm"));
    command
        .args(args)
        .env("XDG_DATA_HOME", scratch.join("data"))
        .env("HOME", scratch.join("home"))
        .env_remove("BLINKTERM_ENGINE")
        .stdin(Stdio::null());
    if let Some(engine) = engine {
        command.env("BLINKTERM_ENGINE", engine);
    }
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command.output().expect("the binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The fact after `name:` on the first line that has it.
fn fact<'a>(report: &'a str, name: &str) -> Option<&'a str> {
    report
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}:")))
        .map(str::trim)
}

/// `--install-engine`, once more if the first try failed: Chrome for
/// Testing's bucket is a network away, and one bad minute there is not a
/// fault in this program.
fn install(scratch: &Path) -> Output {
    let first = blinkterm(scratch, &["--no-config", "--install-engine"], None, None);
    if first.status.success() {
        return first;
    }
    eprintln!(
        "first try failed, trying once more:\n{}{}",
        text(&first.stdout),
        text(&first.stderr)
    );
    blinkterm(scratch, &["--no-config", "--install-engine"], None, None)
}

#[test]
fn the_pinned_engine_is_fetched_verified_unpacked_and_then_found_without_any_setting() {
    if !network_allowed() {
        return;
    }
    let platform = install::platform().expect("a platform Chrome for Testing builds for");
    let shell = install::shell_for(platform).expect("a checksum for this platform");
    let dir = scratch("fetch");

    let out = install(&dir);
    let report = text(&out.stdout);
    assert!(out.status.success(), "{report}{}", text(&out.stderr));
    let sha = fact(&report, "sha-256").expect("a sha-256 line");
    assert!(sha.starts_with(shell.sha256), "{report}");
    assert!(fact(&report, "from").is_some(), "{report}");
    let answered = fact(&report, "answered").expect("an answered line");
    assert!(answered.starts_with("on its pipe"), "{report}");
    let installed = PathBuf::from(fact(&report, "installed").expect("an installed line"));
    assert_eq!(
        installed,
        install::executable_in(
            &dir.join("data").join("blinkterm"),
            install::SHELL_VERSION,
            platform
        ),
        "{report}"
    );
    assert!(installed.is_file(), "{}", installed.display());

    // With nothing on PATH, the only engine there is to find is that one.
    let found = blinkterm(&dir, &["--no-config", "--print-engine"], None, Some(""));
    assert!(found.status.success(), "{}", text(&found.stderr));
    assert_eq!(text(&found.stdout).trim(), installed.display().to_string());

    // A person who names an engine still gets theirs.
    let named = blinkterm(
        &dir,
        &["--no-config", "--print-engine"],
        Some("/bin/sh"),
        Some(""),
    );
    assert_eq!(text(&named.stdout).trim(), "/bin/sh");

    // A second run fetches nothing.
    let again = blinkterm(&dir, &["--no-config", "--install-engine"], None, None);
    let report = text(&again.stdout);
    assert!(again.status.success(), "{report}{}", text(&again.stderr));
    assert!(
        fact(&report, "installed").is_some_and(|line| line.starts_with("already installed at")),
        "{report}"
    );
    assert_eq!(fact(&report, "from"), None, "{report}");
    assert_eq!(fact(&report, "sha-256"), None, "{report}");

    std::fs::remove_dir_all(&dir).ok();
}
