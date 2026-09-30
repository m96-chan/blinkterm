//! The page in front, opened in the desktop browser (#61).
//!
//! Some pages cannot be finished in a terminal. A reCAPTCHA that wants its
//! pictures clicked in a way the cell grid cannot quite give it (#57), a file
//! input in a frame of another origin that the picker cannot reach, a passkey
//! that wants the platform's authenticator, a video behind DRM that a headless
//! engine will not play. For all of them the honest answer is an escape
//! hatch: one key that says "take this page elsewhere". That is `alt+o`
//! (`open-external`), and it hands the url of the tab in front to the
//! desktop's own browser.
//!
//! # The url is the tab's own, and nothing is asked of the page
//!
//! The url opened is the one this program already has for the tab — the one
//! `copy-url` copies and the url bar shows. No CDP call is made. The pages
//! this key is for are exactly the ones where asking would fail: a renderer
//! that has crashed, a dialog holding the page, a challenge that has stopped
//! answering. So the key works on a crashed page and behind a dialog too.
//!
//! Only `http`, `https` and `file` pages go outside ([`openable`]). A blank
//! tab has nothing to open, and `about:`, `chrome:`, `data:`, `javascript:`,
//! `blob:` and the rest are either this engine's own, mean nothing in another
//! browser, or would run something there: they are refused on the row.
//!
//! # Which program: the setting, else the platform's own
//!
//! `external-browser = <command>` names it, split into words as the picker
//! commands are ([`crate::picker::split_words`]) and run with no shell.
//! `{url}` is where the url goes; a command without it gets the url as its
//! last argument, so `external-browser = firefox` does what it says. A named
//! command is run wherever it is — the person said so.
//!
//! With nothing named ([`plan`]): `open` on a Mac; on anything else,
//! `$BROWSER` when it is set, splits into words and is not this program (a
//! `%s` in it is where the url goes, as the old convention has it), and
//! `xdg-open` when it is not. The platform's own is only used where there is
//! a desktop to open it on, which is [`crate::picker::has_display`]'s answer:
//! an X or Wayland display in the environment, or a Mac not reached over ssh.
//! Without one nothing is run and the row says so, and points at `alt+u`,
//! which copies the url to the clipboard of the machine the terminal is on.
//!
//! # `$BROWSER` that is this program
//!
//! `blinkterm --remote` is a reasonable `$BROWSER` — it is how other programs
//! open links here ([`crate::remote`]) — and it is exactly the wrong thing
//! for this key: the url would come straight back as a new tab. So a
//! `$BROWSER` whose program's file name is `blinkterm` is not used, and is
//! taken out of the environment of whatever is run instead
//! ([`browser_to_pass`]), because `xdg-open` on a desktop it does not know
//! falls back to `$BROWSER` on its own.
//!
//! # Started and let go
//!
//! The program is started in a process group of its own, with nothing on its
//! standard input, output or error, in the home directory, and it is never
//! signalled and never waited on. That is the opposite of the file picker's
//! [`crate::picker::Running`], whose group is ended when it is dropped: a
//! picker belongs to the question it answers, and a browser belongs to the
//! person, who will not expect it to close because this program did. `open`
//! returns at once; `xdg-open` can stay as long as the browser it started, so
//! the children are asked once a pass whether they have exited ([`reap`]),
//! which is one `waitpid` each, and forgotten when they have, so none is left
//! a zombie. At quit they are simply dropped, which in the standard library
//! kills nothing.
//!
//! # What leaves, and what does not
//!
//! The url, query string included, is the one thing handed over — as an
//! argument of a program of the person's own, only when the key is pressed.
//! Nothing else of the page goes with it. In particular cookies and logins do
//! not travel: the other browser is another browser, on its own profile, and
//! opens the page afresh — signed in if it was already signed in there, and
//! otherwise not. A `file:` url on a Mac is opened by `open` with whatever the
//! desktop opens that file with, which for a PDF is Preview rather than a
//! browser; `external-browser = open -a Safari {url}` says otherwise.

use std::path::Path;
use std::process::{Child, Stdio};

use crate::engine;
use crate::picker;
use crate::remote;
use crate::text;

/// What is started, for the sentence when it cannot be.
pub const WHAT: &str = "the desktop browser";

/// The row's sentence where there is no desktop to open anything on.
pub const NO_DESKTOP: &str = "no desktop here; alt+u copies the url";

/// The row's sentence for a tab with no page in it.
pub const NOTHING: &str = "nothing to open here";

/// What happens for the url: which command, or why none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// This command, its `{url}` still to fill ([`argv`]).
    Run(picker::Command),
    /// Nothing named and no desktop: nothing is run.
    NoDesktop,
}

/// Which page may go outside: `http`, `https` and `file`, in any case. The
/// sentence for the row otherwise, with the scheme in it.
pub fn openable(url: &str) -> Result<(), String> {
    if url.is_empty() || url == "about:blank" {
        return Err(NOTHING.to_string());
    }
    // `scheme_of` will not call `javascript:1` a scheme, since it reads the
    // text the way a person types an address and `host:1` is a port; here
    // anything before a colon is enough to name what is refused.
    let scheme = remote::scheme_of(url)
        .or_else(|| url.split_once(':').map(|(scheme, _)| scheme))
        .unwrap_or("");
    let lower = scheme.to_ascii_lowercase();
    if matches!(lower.as_str(), "http" | "https" | "file") {
        return Ok(());
    }
    if scheme.is_empty() {
        return Err(NOTHING.to_string());
    }
    Err(format!(
        "a {}: page is not opened outside",
        text::sanitize(scheme)
    ))
}

/// The command for this run: `configured` if set, wherever it is; else
/// [`Plan::NoDesktop`] without a `display`; else `open` on a Mac; else
/// `browser` (`$BROWSER`) when it is set, splits and is not this program,
/// with a `%s` in it made `{url}`; else `xdg-open`.
pub fn plan(
    configured: Option<&picker::Command>,
    mac: bool,
    browser: Option<&str>,
    display: bool,
) -> Plan {
    if let Some(command) = configured {
        return Plan::Run(command.clone());
    }
    if !display {
        return Plan::NoDesktop;
    }
    let fixed = |words: &[&str]| picker::Command {
        words: words.iter().map(|word| word.to_string()).collect(),
    };
    if mac {
        return Plan::Run(fixed(&["open", "{url}"]));
    }
    if let Some(mut command) = browser.and_then(usable_browser) {
        for word in &mut command.words {
            *word = word.replace("%s", "{url}");
        }
        return Plan::Run(command);
    }
    Plan::Run(fixed(&["xdg-open", "{url}"]))
}

/// `$BROWSER` as a command, when it is one and is not this program.
fn usable_browser(value: &str) -> Option<picker::Command> {
    picker::Command::parse("BROWSER", value)
        .ok()
        .filter(|command| !names_blinkterm(command))
}

/// What `BROWSER` is in the environment of the program started: the same
/// value when it is a command that is not this program, and nothing — the
/// variable taken out — otherwise, so that no fallback of `xdg-open`'s can
/// hand the url back here.
pub fn browser_to_pass(browser: Option<&str>) -> Option<String> {
    browser
        .filter(|value| usable_browser(value).is_some())
        .map(str::to_string)
}

/// Whether the command's program is this program: its file name is
/// `blinkterm`.
pub fn names_blinkterm(command: &picker::Command) -> bool {
    command
        .words
        .first()
        .and_then(|program| Path::new(program).file_name())
        .is_some_and(|name| name == "blinkterm")
}

/// The words to run: `{url}` filled in where it is written, inside a word or
/// as one ([`picker::Command::expand_with`]), or the url as the last word
/// when no word has it.
pub fn argv(command: &picker::Command, url: &str) -> Vec<String> {
    let mut words = command.expand_with(&[("url", url)]);
    if !command.has("{url}") {
        words.push(url.to_string());
    }
    words
}

/// The process to start: `argv`, standard input, output and error on
/// `/dev/null`, in `home` or else `/`, and `BROWSER` in its environment only
/// as `browser` says — `None` takes it out.
pub fn process(
    argv: &[String],
    browser: Option<&str>,
    home: Option<&Path>,
) -> std::process::Command {
    let mut process = std::process::Command::new(&argv[0]);
    process
        .args(&argv[1..])
        .current_dir(home.unwrap_or(Path::new("/")))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match browser {
        Some(value) => process.env("BROWSER", value),
        None => process.env_remove("BROWSER"),
    };
    process
}

/// Start `process` in a group of its own and let it go. The sentence for
/// the row when it cannot start; `program` is its name for that sentence.
pub fn launch(process: &mut std::process::Command, program: &str) -> Result<Child, String> {
    // A group of its own so that a ctrl+c or a job-control signal meant for
    // this program's group does not reach it. The group's kill target that
    // comes back is dropped on purpose: nothing here ever ends a browser the
    // person has opened.
    engine::spawn_in_own_group(process)
        .map(|(child, _target)| child)
        .map_err(|error| picker::cannot_start(WHAT, program, &error))
}

/// Drop the children that have exited; the rest stay, to be asked again
/// next pass. One `waitpid` each, which never blocks.
pub fn reap(children: &mut Vec<Child>) {
    children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
}

/// The row's sentence once it has started: `sent to the desktop browser`, or
/// `sent to <program>` for a command the settings named — its file name, as
/// plain text since it is the person's own words from a file.
pub fn sent(configured: Option<&picker::Command>) -> String {
    let program = configured.and_then(|command| command.words.first());
    match program {
        Some(program) => {
            let name = Path::new(program)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| program.clone());
            format!("sent to {}", text::sanitize(&name))
        }
        None => format!("sent to {WHAT}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn command(text: &str) -> picker::Command {
        picker::Command::parse("external-browser", text).expect("a command")
    }

    fn words(plan: Plan) -> Vec<String> {
        match plan {
            Plan::Run(command) => command.words,
            Plan::NoDesktop => panic!("nothing to run"),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "blinkterm-external-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn a_configured_command_runs_wherever_it_is_and_the_url_fills_its_placeholder_or_goes_last() {
        let url = "https://example.com/a b?q=1";
        let tab = command("firefox --new-tab --url={url}");
        assert_eq!(
            argv(&tab, url),
            ["firefox", "--new-tab", "--url=https://example.com/a b?q=1"]
        );
        assert_eq!(argv(&command("firefox"), url), ["firefox", url]);
        assert_eq!(
            argv(&command("open -a Safari {url}"), url),
            ["open", "-a", "Safari", url]
        );
        // A `{url}` in the url is not filled again.
        assert_eq!(
            argv(&command("x {url}"), "https://x/{url}"),
            ["x", "https://x/{url}"]
        );
        for (mac, display) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(
                plan(Some(&tab), mac, Some("chromium %s"), display),
                Plan::Run(tab.clone()),
                "mac {mac}, display {display}"
            );
        }
    }

    #[test]
    fn with_nothing_set_a_mac_opens_with_open_and_linux_with_xdg_open() {
        assert_eq!(words(plan(None, true, None, true)), ["open", "{url}"]);
        assert_eq!(words(plan(None, false, None, true)), ["xdg-open", "{url}"]);
        // `$BROWSER` is for the platforms without their own opener.
        assert_eq!(
            words(plan(None, true, Some("firefox %s"), true)),
            ["open", "{url}"]
        );
    }

    #[test]
    fn on_linux_browser_is_used_unless_it_names_blinkterm() {
        assert_eq!(
            words(plan(None, false, Some("firefox %s"), true)),
            ["firefox", "{url}"]
        );
        assert_eq!(words(plan(None, false, Some("firefox"), true)), ["firefox"]);
        for browser in [
            "blinkterm --remote",
            "/usr/local/bin/blinkterm",
            "",
            "firefox 'unclosed",
        ] {
            assert_eq!(
                words(plan(None, false, Some(browser), true)),
                ["xdg-open", "{url}"],
                "{browser:?}"
            );
        }
        assert!(names_blinkterm(&command("~/bin/blinkterm --remote")));
        assert!(!names_blinkterm(&command("blinkterm-open")));
        assert_eq!(browser_to_pass(Some("firefox")).as_deref(), Some("firefox"));
        assert_eq!(browser_to_pass(Some("blinkterm --remote")), None);
        assert_eq!(browser_to_pass(Some("")), None);
        assert_eq!(browser_to_pass(None), None);
    }

    #[test]
    fn with_no_display_and_nothing_set_nothing_runs_and_the_row_says_so() {
        assert_eq!(plan(None, true, None, false), Plan::NoDesktop);
        assert_eq!(
            plan(None, false, Some("firefox %s"), false),
            Plan::NoDesktop
        );
        assert!(NO_DESKTOP.contains("alt+u"));
    }

    #[test]
    fn only_http_https_and_file_pages_go_outside() {
        for url in [
            "http://example.com/",
            "https://example.com/?q=1",
            "HTTPS://X",
            "file:///tmp/a.pdf",
        ] {
            assert_eq!(openable(url), Ok(()), "{url}");
        }
        assert_eq!(openable(""), Err(NOTHING.to_string()));
        assert_eq!(openable("about:blank"), Err(NOTHING.to_string()));
        for (url, scheme) in [
            ("chrome://version", "chrome"),
            ("data:text/html,x", "data"),
            ("javascript:1", "javascript"),
            ("blob:https://x/y", "blob"),
            ("about:version", "about"),
        ] {
            assert_eq!(
                openable(url),
                Err(format!("a {scheme}: page is not opened outside")),
                "{url}"
            );
        }
    }

    #[test]
    fn browser_reaches_the_child_only_when_it_is_not_blinkterm() {
        for (browser, expected) in [(None, "unset"), (Some("firefox"), "firefox")] {
            let file = scratch("browser");
            let argv = strings(&[
                "sh",
                "-c",
                "printf %s \"${BROWSER-unset}\" > \"$1\"",
                "sh",
                &file.to_string_lossy(),
            ]);
            let mut child = launch(&mut process(&argv, browser, None), "sh").expect("sh starts");
            assert!(child.wait().expect("waited").success());
            let written = std::fs::read_to_string(&file).expect("written");
            std::fs::remove_file(&file).ok();
            assert_eq!(written, expected);
        }
        // What is passed is decided from the environment's value.
        assert_eq!(browser_to_pass(Some("blinkterm --remote")), None);
    }

    #[test]
    fn the_browser_is_started_in_a_group_of_its_own_with_nothing_on_its_stdio_and_reaped_when_it_exits(
    ) {
        let home = std::env::temp_dir();
        let argv = strings(&["sh", "-c", "sleep 0.3"]);
        let child = launch(&mut process(&argv, None, Some(&home)), "sh").expect("sh starts");
        let pid = child.id() as i32;
        // SAFETY: `getpgid(2)` takes a pid and touches no memory.
        assert_eq!(unsafe { libc::getpgid(pid) }, pid, "a group of its own");
        let mut children = vec![child];
        reap(&mut children);
        assert_eq!(children.len(), 1, "still running, so kept");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !children.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            reap(&mut children);
        }
        assert!(children.is_empty(), "reaped once it exited");

        // Standard input is `/dev/null`: a read ends at once rather than
        // waiting on this program's terminal.
        let argv = strings(&["sh", "-c", "read x; exit 0"]);
        let mut child = launch(&mut process(&argv, None, None), "sh").expect("sh starts");
        assert!(child.wait().expect("waited").success());

        // A failure is reaped like a success, and nothing is said of it.
        let argv = strings(&["sh", "-c", "exit 7"]);
        let child = launch(&mut process(&argv, None, None), "sh").expect("sh starts");
        let mut children = vec![child];
        let deadline = Instant::now() + Duration::from_secs(10);
        while !children.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            reap(&mut children);
        }
        assert!(children.is_empty());
    }

    #[test]
    fn a_program_that_is_not_there_says_so() {
        let argv = strings(&["blinkterm-no-such-browser", "https://example.com/"]);
        let error = launch(&mut process(&argv, None, None), &argv[0]).expect_err("not there");
        assert_eq!(
            error,
            "can't start the desktop browser blinkterm-no-such-browser: no such program"
        );
    }

    #[test]
    fn the_row_names_what_was_run() {
        assert_eq!(sent(None), "sent to the desktop browser");
        assert_eq!(
            sent(Some(&command(
                "/Applications/Firefox.app/x/firefox --new-tab"
            ))),
            "sent to firefox"
        );
        assert_eq!(sent(Some(&command("chromium {url}"))), "sent to chromium");
    }
}
