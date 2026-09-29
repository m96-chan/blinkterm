//! A login form filled from a password manager's command, on a key (#65).
//!
//! This program stores no password and never will. Most people who live in
//! a terminal already have a password manager with a command line — `pass`,
//! `gopass`, `rbw`, `op`, `bw`, `keepassxc-cli` — so the settings name a
//! command, and `fill-login` (`alt+l`) runs it for the page in front and
//! puts what it printed into the page's password field and the user-name
//! field before it.
//!
//! # The same two kinds as the file picker
//!
//! `password-command` is a program that leaves the terminal alone — a
//! dialog, or a manager that answers without asking — and runs beside the
//! loop; `password-command-terminal` is one that needs this terminal (`fzf`
//! over `pass ls`, a manager asking for its master password) and is given it
//! until it exits. With both set, the window where there is a display and
//! the terminal elsewhere. Everything about starting, reading, bounding and
//! ending them is the file picker's ([`crate::picker::Running`],
//! [`crate::picker::run_in_terminal`]), and so is the splitting of the
//! command into words, run with no shell.
//!
//! # Placeholders: the host, a guess at the domain, the url without its query
//!
//! `{host}` is the page's hostname, lower-cased, with no port. `{domain}` is
//! the host with its subdomains dropped ([`domain`]) — `accounts.example.com`
//! is `example.com` — which is how most people name their entries; it is a
//! guess made without the public-suffix list, and says so. `{url}` is the
//! page's url up to its path: scheme, host, port and path, with the query
//! and fragment taken off, because a process's arguments are readable by
//! every local process and a query string can carry a session token. There
//! is no `{out}`: a file for the answer would put a password on the disk.
//!
//! The command is run once, with whatever placeholder was written. Trying
//! the parent domain after a miss would ask an interactive manager twice,
//! and "not found" cannot be told from "cancelled" by an exit status.
//!
//! # What it prints: the `pass` convention
//!
//! The first line is the password, as it is — only a `\r` before the
//! newline is taken off, since a password can begin or end in a space. Of
//! the lines after it, the first whose key is `login`, `username` or `user`
//! (any case, `login: me`) is the user name. That is what `pass`, `gopass`
//! and `rbw get --full` print. JSON is not read: every manager's differs,
//! and the parsed copies would be left in memory nobody can clear; a wrapper
//! with `jq` turns any of them into these two lines. A non-zero exit, or
//! nothing printed, is no login for the page — cancelled, or not found.
//!
//! # Where it may run, decided twice
//!
//! Before anything is started, from the url this program has for the tab
//! ([`site`]): `https`, or `http` on a host that is this machine
//! (`localhost`, `*.localhost`, `127.x.x.x`, `[::1]`). Anything else is
//! refused on the row with nothing run, so a secret is never fetched for a
//! page that could not be trusted with it.
//!
//! Then in the page, by the script that fills (`SCRIPT`): it touches only
//! a document whose own `location` passes the same rule and whose hostname
//! is the one the secret was fetched for — the top document and the frames
//! of the same origin it can reach. That is checked where the fields are,
//! so a page that went somewhere else while the command ran, or a frame of
//! another site, is not filled. A cross-origin frame is out of the script's
//! reach by the engine's own rule, and a `srcdoc` or `about:blank` frame has
//! no hostname and is skipped.
//!
//! # The secret is kept as briefly as it can be
//!
//! What the command printed is read from a pipe nobody else holds into a
//! buffer that never grows, copied once into a [`Secret`] — which is
//! overwritten with zeros when it is dropped, as is every buffer the output
//! passed through ([`crate::picker::scrub`]) — sent to the page as an
//! argument of one `Runtime.callFunctionOn`, never spliced into script
//! source, and dropped. It is never put on the row, the tab list, the
//! history, the session or any file. The one copy this program cannot
//! clear is the serialized DevTools message, which is built and freed
//! inside [`crate::cdp::Client::send`].

use std::os::fd::RawFd;
use std::path::Path;

use crate::permissions;
use crate::picker::{self, Command, Exit, Kind, Running};
use crate::text;

/// What the password command is called in the sentences it shares with the
/// file picker: "can't start the password command pass: no such program".
pub const WHAT: &str = "the password command";

/// The two settings, as the run has them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Programs {
    /// `password-command`: left to run while the browser carries on.
    pub gui: Option<Command>,
    /// `password-command-terminal`: given the terminal until it exits.
    pub terminal: Option<Command>,
}

impl Programs {
    /// Whether neither is set, and `fill-login` has nothing to run.
    pub fn is_empty(&self) -> bool {
        self.gui.is_none() && self.terminal.is_none()
    }

    /// The command to run, by the file picker's rule
    /// ([`crate::picker::choose_kind`]): `display` is whether a window can
    /// be opened ([`crate::picker::has_display`]).
    pub fn choose(&self, display: bool) -> Option<(Kind, &Command)> {
        picker::choose_kind(self.gui.as_ref(), self.terminal.as_ref(), display)
    }
}

/// The page a login is for, as the command's placeholders need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// `{host}`: the hostname, lower-cased, with no port; `[::1]` with its
    /// brackets, as `location.hostname` has it.
    pub host: String,
    /// `{domain}`: [`domain`] of the host.
    pub domain: String,
    /// `{url}`: scheme, host, a port that is not the scheme's own, and the
    /// path; no user, no query, no fragment.
    pub url: String,
}

/// Why a page's login is not looked up at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Plain `http` to a host that is not this machine: anybody on the way
    /// could read what was typed.
    NotSecure,
    /// Not a web page with a host: `about:blank`, `data:`, `file:`, an
    /// error page.
    NoHost,
}

impl Refused {
    /// What the row says.
    pub fn sentence(self) -> &'static str {
        match self {
            Refused::NotSecure => "fill-login needs https, or localhost",
            Refused::NoHost => "this page has no host to look up",
        }
    }
}

/// The page at `url` as a [`Site`], or why its login is not to be looked
/// up. The host is read as [`crate::permissions::origin_of`] reads one, so a
/// user and password in the url are not taken for the host, and a host with
/// anything but plain text in it is none.
pub fn site(url: &str) -> Result<Site, Refused> {
    let origin = permissions::origin_of(url).ok_or(Refused::NoHost)?;
    let (scheme, authority) = origin.split_once("://").ok_or(Refused::NoHost)?;
    let host = match authority.strip_prefix('[') {
        Some(_) => &authority[..=authority.find(']').ok_or(Refused::NoHost)?],
        None => authority.split(':').next().unwrap_or(authority),
    };
    if scheme != "https" && !is_local(host) {
        return Err(Refused::NotSecure);
    }
    // The path is what follows the authority in the url as it was written,
    // up to a query or a fragment. `origin_of` has already found where the
    // authority ends by the same four characters.
    let rest = &url[scheme.len() + 3..];
    let rest = &rest[rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len())..];
    let path = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    let path = if path.is_empty() { "/" } else { path };
    Ok(Site {
        host: host.to_string(),
        domain: domain(host),
        url: format!("{origin}{path}"),
    })
}

/// Whether `host` — lower-cased, as [`site`] has it — is this machine:
/// `localhost` and any name under it, which the engine resolves to loopback
/// itself, an IPv4 address in `127.0.0.0/8`, or `[::1]`. Plain `http` to one
/// of these goes nowhere another machine can see it.
pub fn is_local(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "[::1]"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The labels two-letter country domains put under themselves before
/// selling names: `example.co.uk`, `example.com.au`, `example.ne.jp`.
pub const SECOND_LEVEL: [&str; 11] = [
    "co", "com", "net", "org", "ac", "gov", "edu", "ne", "or", "go", "mil",
];

/// `host` without its subdomains, for `{domain}`: its last two labels, or
/// its last three when the last is a two-letter country and the one before
/// it is one of [`SECOND_LEVEL`]. An address, `localhost`, and a host of so
/// few labels that there is nothing to drop are themselves.
///
/// A guess: the real answer is the public-suffix list, thousands of lines
/// that change every week, which a program with one dependency does not
/// carry. It is right for the names people keep passwords for, and a
/// command that needs the exact host uses `{host}`.
pub fn domain(host: &str) -> String {
    let address = host.starts_with('[') || host.parse::<std::net::Ipv4Addr>().is_ok();
    let labels: Vec<&str> = host.split('.').collect();
    if address || host == "localhost" || labels.len() < 3 {
        return host.to_string();
    }
    let n = labels.len();
    let country = labels[n - 1].len() == 2;
    let keep = if country && SECOND_LEVEL.contains(&labels[n - 2]) {
        3
    } else {
        2
    };
    labels[n - keep..].join(".")
}

/// The words to run for `site`: `{host}`, `{domain}` and `{url}` replaced
/// inside the words they are in, nothing split again, and anything else in
/// braces — `{out}` included — left as it is written
/// ([`crate::picker::Command::expand_with`]).
pub fn argv(command: &Command, site: &Site) -> Vec<String> {
    command.expand_with(&[
        ("host", &site.host),
        ("domain", &site.domain),
        ("url", &site.url),
    ])
}

/// A password, or a user name, held no longer than it is needed and
/// overwritten with zeros when it is dropped.
///
/// Not `Clone`, so that there is one copy to clear, and its `Debug` prints
/// nothing of it, so that no `{:?}` can put it on a screen. Always UTF-8:
/// it is only made from a `&str`.
pub struct Secret(Vec<u8>);

impl Secret {
    /// A copy of `text`, in an allocation of exactly its size, which is
    /// never grown and so never leaves a copy behind.
    pub fn new(text: &str) -> Secret {
        let mut bytes = Vec::with_capacity(text.len());
        bytes.extend_from_slice(text.as_bytes());
        Secret(bytes)
    }

    /// The text, to hand to the page.
    pub fn as_str(&self) -> &str {
        // Only ever made from a `&str`, so this is never the empty fallback.
        std::str::from_utf8(&self.0).unwrap_or_default()
    }

    /// Its length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether it is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        picker::scrub(&mut self.0);
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// What the command printed, as the page is to be given it.
#[derive(Debug)]
pub struct Login {
    /// The first line.
    pub password: Secret,
    /// The first `login:`, `username:` or `user:` line after it, trimmed;
    /// treated exactly as the password is.
    pub user: Option<Secret>,
}

/// The sentence for output that is not UTF-8: a password is text, and what
/// is not is some other program's output.
const NOT_TEXT: &str = "the password command printed something that is not text";

/// The sentence for output whose first line is empty.
const NO_PASSWORD: &str = "the password command printed no password";

/// The command's output as a [`Login`]: see the module for the format. The
/// sentence for the row when it is not one.
pub fn parse_output(bytes: &[u8]) -> Result<Login, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| NOT_TEXT.to_string())?;
    let mut lines = text.split('\n');
    let first = lines.next().unwrap_or_default();
    let first = first.strip_suffix('\r').unwrap_or(first);
    if first.is_empty() {
        return Err(NO_PASSWORD.to_string());
    }
    let user = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| {
            let key = key.trim();
            ["login", "username", "user"]
                .iter()
                .any(|name| key.eq_ignore_ascii_case(name))
        })
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
        .map(Secret::new);
    Ok(Login {
        password: Secret::new(first),
        user,
    })
}

/// How a password command ended.
#[derive(Debug)]
pub enum Outcome {
    /// The login it printed.
    Found(Login),
    /// It was cancelled, found nothing, or printed nothing: the row says
    /// there is no login for the host, and nothing is sent.
    None,
    /// It could not be run, broke, or printed something that is not a
    /// login: the sentence for the row, which never holds what it printed.
    Failed(String),
}

/// What a command that has exited, or could not be run, amounts to. A
/// non-zero exit or nothing but blank space printed is [`Outcome::None`],
/// as it is a cancel for the file picker.
pub fn outcome(exit: Result<Exit, String>) -> Outcome {
    let exit = match exit {
        Ok(exit) => exit,
        Err(why) => return Outcome::Failed(why),
    };
    if !exit.success || exit.answer.iter().all(u8::is_ascii_whitespace) {
        return Outcome::None;
    }
    match parse_output(&exit.answer) {
        Ok(login) => Outcome::Found(login),
        Err(why) => Outcome::Failed(why),
    }
}

/// A password command with a window of its own, or none, while it runs
/// beside the loop. Dropping it ends it, as a picker is ended.
pub struct Gui {
    running: Running,
    /// The tab it is for, by target id.
    pub tab: String,
    /// The url the tab was at when it was started: the login is only for
    /// that page, and a tab that has gone somewhere else is not filled.
    pub url: String,
    /// What it was started for.
    pub site: Site,
}

impl Gui {
    /// Start `command` for `site`, in `dir`, for the tab `tab` at `url`.
    /// The sentence for the row when it cannot be.
    pub fn spawn(
        command: &Command,
        site: &Site,
        tab: &str,
        url: &str,
        dir: &Path,
    ) -> Result<Gui, String> {
        let running = Running::spawn(&argv(command, site), dir, None, WHAT)?;
        Ok(Gui {
            running,
            tab: tab.to_string(),
            url: url.to_string(),
            site: site.clone(),
        })
    }

    /// The descriptor to poll, while there is output still to come.
    pub fn fd(&self) -> Option<RawFd> {
        self.running.fd()
    }

    /// One pass: read what it printed if `readable`, and see whether it has
    /// exited. `Some` once it is over, and then only once.
    pub fn pump(&mut self, readable: bool) -> Option<Outcome> {
        self.running.pump(readable).map(outcome)
    }
}

/// Run a terminal password command to the end, on this terminal, for
/// `site`, in `dir`. The caller has given the terminal back first and takes
/// it again after, as for a terminal file picker.
pub fn run_terminal(command: &Command, site: &Site, dir: &Path) -> Outcome {
    outcome(picker::run_in_terminal(
        &argv(command, site),
        dir,
        None,
        WHAT,
    ))
}

/// What the row says while a command with a window is asked.
pub fn asking(host: &str) -> String {
    format!("asking for the login for {}", text::sanitize(host))
}

/// What the row says when the command had no login for the page.
pub fn no_login(host: &str) -> String {
    format!("no login for {}", text::sanitize(host))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn command(text: &str) -> Command {
        Command::parse("password-command", text).expect("a command")
    }

    fn exit(success: bool, answer: &[u8]) -> Result<Exit, String> {
        Ok(Exit {
            success,
            answer: answer.to_vec(),
        })
    }

    /// The password and the user name of a login, for comparing.
    fn found(outcome: &Outcome) -> Option<(&str, Option<&str>)> {
        match outcome {
            Outcome::Found(login) => Some((
                login.password.as_str(),
                login.user.as_ref().map(Secret::as_str),
            )),
            _ => None,
        }
    }

    fn parsed(text: &str) -> (String, Option<String>) {
        let login = parse_output(text.as_bytes()).expect("a login");
        (
            login.password.as_str().to_string(),
            login.user.as_ref().map(|user| user.as_str().to_string()),
        )
    }

    #[test]
    fn the_first_line_is_the_password_and_a_login_line_the_user_name() {
        let pair =
            |password: &str, user: Option<&str>| (password.to_string(), user.map(str::to_string));
        assert_eq!(parsed("secret\nlogin: me\n"), pair("secret", Some("me")));
        assert_eq!(parsed("s\r\nUsername:me\n"), pair("s", Some("me")));
        assert_eq!(
            parsed("s\nurl: x\nuser:  a b \n"),
            pair("s", Some("a b")),
            "the user name is trimmed"
        );
        assert_eq!(parsed("s\n"), pair("s", None));
        assert_eq!(parsed("s"), pair("s", None));
        assert_eq!(
            parsed("s\nlogin: first\nusername: second\n"),
            pair("s", Some("first")),
            "only the first counts"
        );
        assert_eq!(
            parsed(" pass word \nLOGIN : me"),
            pair(" pass word ", Some("me")),
            "the password is kept as it is"
        );
        assert_eq!(
            parsed("s\nlogin:\n"),
            pair("s", None),
            "an empty one is none"
        );
        assert_eq!(
            parsed("s\nloginx: no\n"),
            pair("s", None),
            "the key is the whole key"
        );
    }

    #[test]
    fn no_password_line_or_bytes_that_are_not_text_are_refused() {
        let refused = |bytes: &[u8]| parse_output(bytes).err();
        assert_eq!(refused(b"").as_deref(), Some(NO_PASSWORD));
        assert_eq!(refused(b"\n").as_deref(), Some(NO_PASSWORD));
        assert_eq!(refused(b"\r\nlogin: me\n").as_deref(), Some(NO_PASSWORD));
        assert_eq!(refused(b"\xff\n").as_deref(), Some(NOT_TEXT));
        assert_eq!(
            NOT_TEXT,
            "the password command printed something that is not text"
        );
        assert_eq!(NO_PASSWORD, "the password command printed no password");
    }

    #[test]
    fn a_site_is_https_or_a_local_http_host_with_a_plain_host() {
        let ok = |url: &str| site(url).unwrap_or_else(|why| panic!("{url}: {why:?}"));
        let example = ok("https://Example.com/a?b#c");
        assert_eq!(example.host, "example.com");
        assert_eq!(example.domain, "example.com");
        assert_eq!(example.url, "https://example.com/a");
        assert_eq!(ok("https://example.com").url, "https://example.com/");
        assert_eq!(ok("https://example.com?q").url, "https://example.com/");
        assert_eq!(ok("https://example.com:443/x").url, "https://example.com/x");

        let local = ok("http://localhost:8080/login?next=/");
        assert_eq!(local.host, "localhost");
        assert_eq!(local.url, "http://localhost:8080/login", "the port is kept");
        assert_eq!(ok("http://127.0.0.1/").host, "127.0.0.1");
        assert_eq!(ok("http://127.1.2.3:9/").host, "127.1.2.3");
        assert_eq!(ok("http://[::1]:8000/a").host, "[::1]");
        assert_eq!(ok("http://[::1]:8000/a").url, "http://[::1]:8000/a");
        assert_eq!(ok("http://foo.localhost/").host, "foo.localhost");
        assert_eq!(ok("https://u:p@h.example/").host, "h.example");
        assert_eq!(ok("https://u:p@h.example/").url, "https://h.example/");

        assert_eq!(site("http://example.com/"), Err(Refused::NotSecure));
        assert_eq!(
            site("http://127.evil.example/"),
            Err(Refused::NotSecure),
            "a name that begins like an address is a name"
        );
        assert_eq!(site("http://localhost.example/"), Err(Refused::NotSecure));
        assert_eq!(site("http://[::2]/"), Err(Refused::NotSecure));
        for url in [
            "about:blank",
            "data:text/html,<form>",
            "file:///etc/passwd",
            "chrome-error://chromewebdata/",
            "",
        ] {
            assert_eq!(site(url), Err(Refused::NoHost), "{url}");
        }
        assert_eq!(
            Refused::NotSecure.sentence(),
            "fill-login needs https, or localhost"
        );
        assert_eq!(
            Refused::NoHost.sentence(),
            "this page has no host to look up"
        );
    }

    #[test]
    fn the_domain_is_the_host_without_its_subdomains() {
        assert_eq!(domain("accounts.example.com"), "example.com");
        assert_eq!(domain("a.b.example.com"), "example.com");
        assert_eq!(domain("example.com"), "example.com");
        assert_eq!(domain("www.amazon.co.uk"), "amazon.co.uk");
        assert_eq!(domain("amazon.co.uk"), "amazon.co.uk");
        assert_eq!(domain("id.example.com.au"), "example.com.au");
        assert_eq!(domain("www.example.ne.jp"), "example.ne.jp");
        assert_eq!(domain("www.example.de"), "example.de");
        assert_eq!(domain("a.b.example.io"), "example.io");
        assert_eq!(domain("localhost"), "localhost");
        assert_eq!(domain("app.localhost"), "app.localhost");
        assert_eq!(domain("127.0.0.1"), "127.0.0.1");
        assert_eq!(domain("[::1]"), "[::1]");
    }

    #[test]
    fn the_placeholders_are_the_host_the_domain_and_the_url_and_nothing_is_split() {
        let site = site("https://login.example.co.uk/a%20b/{host}?token=x").expect("a site");
        assert_eq!(
            argv(
                &command("pass show 'web/{domain}' --for={host} {url} {out} {dir}"),
                &site
            ),
            [
                "pass",
                "show",
                "web/example.co.uk",
                "--for=login.example.co.uk",
                "https://login.example.co.uk/a%20b/{host}",
                "{out}",
                "{dir}",
            ],
            "a value is never read again, and the query is gone"
        );
    }

    #[test]
    fn a_nonzero_exit_or_nothing_printed_is_no_login() {
        assert!(matches!(outcome(exit(false, b"secret\n")), Outcome::None));
        assert!(matches!(outcome(exit(true, b"")), Outcome::None));
        assert!(matches!(outcome(exit(true, b"\n \n")), Outcome::None));
        assert!(matches!(
            outcome(Err("lost the password command: x".to_string())),
            Outcome::Failed(why) if why == "lost the password command: x"
        ));
        assert!(matches!(
            outcome(exit(true, b"\nlogin: me\n")),
            Outcome::Failed(why) if why == NO_PASSWORD
        ));
        assert_eq!(
            found(&outcome(exit(true, b"pw\nuser: me\n"))),
            Some(("pw", Some("me")))
        );
    }

    #[test]
    fn a_secret_never_shows_itself_and_a_buffer_is_zero_after_scrubbing() {
        let secret = Secret::new("hunter2");
        assert_eq!(secret.as_str(), "hunter2");
        assert_eq!(secret.len(), 7);
        assert!(!secret.is_empty());
        assert_eq!(format!("{secret:?}"), "Secret(..)");
        let login = parse_output(b"hunter2\nlogin: me\n").expect("a login");
        assert!(!format!("{login:?}").contains("hunter2"));
        assert!(!format!("{:?}", outcome(exit(true, b"hunter2\n"))).contains("hunter2"));
        assert!(!format!("{:?}", exit(true, b"hunter2\n")).contains("hunter2"));

        let mut bytes = b"hunter2".to_vec();
        picker::scrub(&mut bytes);
        assert_eq!(bytes, [0; 7]);
    }

    #[test]
    fn the_window_command_and_the_terminal_one_are_chosen_as_the_pickers_are() {
        let gui = command("rbw get --full {host}");
        let tui = command("sh -c 'pass ls | fzf'");
        let both = Programs {
            gui: Some(gui.clone()),
            terminal: Some(tui.clone()),
        };
        assert_eq!(both.choose(true), Some((Kind::Gui, &gui)));
        assert_eq!(both.choose(false), Some((Kind::Terminal, &tui)));
        let only_gui = Programs {
            gui: Some(gui.clone()),
            terminal: None,
        };
        assert_eq!(only_gui.choose(false), Some((Kind::Gui, &gui)));
        let only_tui = Programs {
            gui: None,
            terminal: Some(tui.clone()),
        };
        assert_eq!(only_tui.choose(true), Some((Kind::Terminal, &tui)));
        assert!(Programs::default().is_empty());
        assert!(!both.is_empty());
        assert_eq!(Programs::default().choose(true), None);
    }

    fn localhost() -> Site {
        site("http://localhost:8080/login").expect("a site")
    }

    /// Pump `gui` as the loop does until it is over, or panic after a few
    /// seconds.
    fn finish(gui: &mut Gui) -> Outcome {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let readable = match gui.fd() {
                Some(fd) => crate::tty::poll_readable(&[fd], 50)
                    .expect("poll")
                    .contains(&fd),
                None => {
                    std::thread::sleep(Duration::from_millis(20));
                    false
                }
            };
            if let Some(outcome) = gui.pump(readable) {
                return outcome;
            }
        }
        panic!("the password command did not finish");
    }

    fn gui(text: &str) -> Result<Gui, String> {
        Gui::spawn(
            &command(text),
            &localhost(),
            "T",
            "http://localhost:8080/login",
            &std::env::temp_dir(),
        )
    }

    #[test]
    fn a_window_command_is_read_from_what_it_prints() {
        let mut command = gui("sh -c 'printf \"secret\\nlogin: me\\n\"'").expect("started");
        assert_eq!(found(&finish(&mut command)), Some(("secret", Some("me"))));
        assert!(command.pump(true).is_none(), "said once");
        assert_eq!(command.tab, "T");
        assert_eq!(command.url, "http://localhost:8080/login");
        assert_eq!(command.site, localhost());

        // The placeholders reach it.
        let mut command = gui("sh -c 'printf \"%s\\n\" \"$1\"' sh {host}").expect("started");
        assert_eq!(found(&finish(&mut command)), Some(("localhost", None)));

        let mut command = gui("sh -c 'echo secret; exit 1'").expect("started");
        assert!(matches!(finish(&mut command), Outcome::None));

        assert_eq!(
            gui("blinkterm-no-such-command --x").err().as_deref(),
            Some("can't start the password command blinkterm-no-such-command: no such program")
        );

        let mut command = gui("yes").expect("started");
        assert!(matches!(
            finish(&mut command),
            Outcome::Failed(why) if why == "the password command printed more than 64 KiB"
        ));
    }

    #[test]
    fn a_terminal_command_is_read_from_its_output() {
        let _turn = picker::SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir();
        let run = |text: &str| run_terminal(&command(text), &localhost(), &dir);
        assert_eq!(
            found(&run("sh -c 'printf \"pw\\nusername: someone\\n\"'")),
            Some(("pw", Some("someone")))
        );
        assert!(matches!(run("sh -c 'echo pw; exit 130'"), Outcome::None));
        assert!(matches!(run("true"), Outcome::None));
        assert!(matches!(
            run("sh -c 'head -c 70000 /dev/zero | tr \"\\0\" a'"),
            Outcome::Failed(why) if why == "the password command printed more than 64 KiB"
        ));
        assert!(matches!(
            run("blinkterm-no-such-command"),
            Outcome::Failed(why)
                if why == "can't start the password command blinkterm-no-such-command: no such program"
        ));
    }

    #[test]
    fn the_row_names_the_host_and_nothing_else() {
        assert_eq!(
            asking("example.com"),
            "asking for the login for example.com"
        );
        assert_eq!(no_login("example.com"), "no login for example.com");
    }
}
