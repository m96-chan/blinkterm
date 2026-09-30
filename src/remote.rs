//! `blinkterm --remote <url>…`: a url from another program, opened as a tab
//! in the blinkterm that is already running.
//!
//! Terminal programs open a link through `$BROWSER` or `xdg-open` — `gh
//! browse`, `git web--browse`, `man -H`, a mail client — and before this each
//! of those would have started a second blinkterm on a profile the first one
//! holds, and been refused. With `export BROWSER='blinkterm --remote'` they
//! reach the one that is running instead, and the url becomes its tab in
//! front.
//!
//! # Explicit, never implied
//!
//! `blinkterm <url>` on a profile that is in use stays the refusal
//! [`crate::profile`] explains; only `--remote` forwards. Guessing from
//! `$BROWSER` would make the same command line mean two things depending on
//! the environment, and `$BROWSER` can carry the flag itself.
//!
//! # The socket
//!
//! A `SOCK_STREAM` Unix socket, [`SOCKET_FILE`] inside the profile, next to
//! [`crate::profile::LOCK_FILE`], made 0600 inside the 0700 profile. The
//! profile resolves exactly as it does for a start — the command line over
//! the settings file — so `--remote --profile X` reaches the blinkterm on X
//! and nothing else. A temporary profile never has one: it is that run's
//! alone and nobody else knows its name.
//!
//! It is bound only once the profile lock is held, and the first thing the
//! binding does is remove whatever is at the path. Under the lock anything
//! there is stale — a `SIGKILL` or a `panic = "abort"` leaves the file behind
//! but never the lock, which the kernel lets go however the process ends — so
//! nothing a crash left is trusted or needs detecting. A sender that finds the
//! file with nobody listening gets `ECONNREFUSED`, which it reads the same as
//! no file at all: nobody there, start as usual.
//!
//! A socket's path must fit `sun_path`, which is 108 bytes on Linux and 104
//! on a Mac, and a profile can be deeper than that, or on a filesystem that
//! cannot hold a socket. Rather than a per-system constant, a bind that fails
//! for any reason is tried again in a fresh 0700 directory `mkdtemp(3)` makes
//! under the temporary directory, and a symlink at the usual place points at
//! it; a sender reads the link first. The profile stays the one place anyone
//! looks.
//!
//! # What is said on it
//!
//! ```text
//! sender   -> running  one url per line, UTF-8, then shutdown(Write)
//! running  -> sender   one line per url, in order, then close:
//!                        ok <the url as opened>
//!                        no <why not>
//! ```
//!
//! Urls and nothing else: no greeting, no version, no verbs. Each line goes
//! through [`crate::app::normalise`], what the url bar does to what is typed,
//! so `example.com`, `localhost:3000` and the `/tmp/…/index.html` that
//! `man -H` sends all mean what they would there; and the result must be an
//! `http`, `https`, `file` or `about` url ([`SCHEMES`]). `javascript:`,
//! `data:`, `chrome://` and the rest are answered `no`. So what a sender can
//! do is what ctrl+t can: open a page. The limits — [`MAX_LINE_BYTES`],
//! [`MAX_URLS`], [`MAX_REQUEST_BYTES`] and [`REQUEST_TIMEOUT`] — exist
//! because the reading happens on the loop that draws the page, and a sender
//! that never finishes must not be able to hold it for longer than a blink.

use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The socket's name inside the profile, next to the lock.
pub const SOCKET_FILE: &str = "blinkterm.sock";

/// The longest url line taken. A url is rarely more than a couple of
/// kilobytes; sixteen is generous and still small enough to read on the loop.
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// The most one sender may say in one connection, all lines together.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// The most urls one sender may hand over in one connection. Nobody clicks
/// sixty-four links at once; a sender that tries is not a person.
pub const MAX_URLS: usize = 64;

/// How long the running blinkterm waits for a sender to finish saying what it
/// has to say, and for its answer to be taken, before giving up on it. It
/// reads on the loop that draws the page, so this is how long a sender that
/// connects and says nothing can hold that loop still.
pub const REQUEST_TIMEOUT: Duration = Duration::from_millis(500);

/// How long a sender waits for the answer. Longer than [`REQUEST_TIMEOUT`] by
/// far, because the running blinkterm may be busy — in a terminal file picker
/// for a while, say — with the urls already in the socket's buffer, where they
/// wait and are opened once it is back.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// The schemes a sender may open: the ones a page from a link can be.
pub const SCHEMES: [&str; 4] = ["http", "https", "file", "about"];

/// The name the fallback directory starts with, so that the next bind can
/// tell one it made from anything else a link could point into.
const FALLBACK_PREFIX: &str = "blinkterm-sock-";

/// Where the socket (or the link to it) is, in the profile at `profile`.
pub fn socket_path(profile: &Path) -> PathBuf {
    profile.join(SOCKET_FILE)
}

/// One line as the running blinkterm read it: the url to open, or why not.
pub type Checked = Result<String, String>;

/// What a line from a sender says to open, or why it is refused.
///
/// In order: it must be UTF-8, not longer than [`MAX_LINE_BYTES`], and not
/// empty once the control characters and the spaces round it are gone. A
/// scheme it spells out must be one of [`SCHEMES`] — this is looked at before
/// the url bar's rules, because those take `javascript:x` for a host and a
/// port and would make it `https://javascript:x`, which is harmless but not
/// what anybody sent. Then [`crate::app::normalise`], and the scheme of what
/// that made is checked once more.
pub fn check(line: &[u8]) -> Checked {
    let text = std::str::from_utf8(line).map_err(|_| "a url is text, and this is not UTF-8")?;
    if line.len() > MAX_LINE_BYTES {
        return Err(format!(
            "a url of more than {} KiB is not one",
            MAX_LINE_BYTES / 1024
        ));
    }
    let text = crate::text::sanitize(text);
    let text = text.trim();
    if text.is_empty() {
        return Err("an empty line is not a url".to_string());
    }
    if let Some(scheme) = scheme_of(text) {
        if !SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()) {
            return Err(refused(scheme));
        }
    }
    let url = crate::app::normalise(text);
    match scheme_of(&url) {
        Some(scheme) if SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()) => Ok(url),
        Some(scheme) => Err(refused(scheme)),
        None => Err(format!("{url} is not a url")),
    }
}

/// Whether `url`, as it is to be opened, has a scheme a sender may open.
pub fn accepted(url: &str) -> bool {
    scheme_of(url).is_some_and(|scheme| SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()))
}

/// The sentence for a scheme that is not taken.
fn refused(scheme: &str) -> String {
    let (last, rest) = SCHEMES.split_last().expect("there are schemes");
    format!(
        "a {scheme}: url is not opened from another program; only {} and {last} urls are",
        rest.join(", ")
    )
}

/// The scheme `text` spells out, if it spells one out.
///
/// RFC 3986's shape — a letter, then letters, digits, `+`, `-` and `.`, then a
/// colon — with one exception: `localhost:3000` and `example.com:8080/x` have
/// that shape too, and are a host and a port. A colon followed by `//` is
/// always a scheme; one followed by a digit is a port; anything else after
/// the colon (`javascript:x`, `mailto:a@b`, `about:blank`) is a scheme.
pub(crate) fn scheme_of(text: &str) -> Option<&str> {
    let (scheme, rest) = text.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    if !rest.starts_with("//") && rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(scheme)
}

/// The line that answers `checked`.
///
/// The reason is made plain text: it goes back to a sender that prints it on
/// a terminal, and it can quote what the sender said.
fn reply_line(checked: &Checked) -> String {
    match checked {
        Ok(url) => format!("ok {}\n", crate::text::sanitize(url)),
        Err(why) => format!("no {}\n", crate::text::sanitize(why)),
    }
}

/// What an answer line says, read back; a line that is neither is a refusal
/// that quotes it.
fn parse_reply(line: &str) -> Checked {
    let line = crate::text::sanitize(line.trim_end_matches(['\n', '\r']));
    if let Some(url) = line.strip_prefix("ok ") {
        return Ok(url.to_string());
    }
    if let Some(why) = line.strip_prefix("no ") {
        return Err(why.to_string());
    }
    Err(format!("an answer that makes no sense: {line}"))
}

/// A running blinkterm's socket, bound under the profile lock and gone again
/// when this is dropped.
#[derive(Debug)]
pub struct Listener {
    socket: UnixListener,
    /// Where a sender looks: the socket itself, or the symlink to it.
    link: PathBuf,
    /// The fresh directory the socket is in when the profile could not hold
    /// it, removed with it.
    fallback: Option<PathBuf>,
}

impl Listener {
    /// Listen on the profile at `profile`, which the caller holds the lock
    /// on. Whatever is at the socket's path is removed first: under the lock,
    /// it can only be what a crash left.
    pub fn bind(profile: &Path) -> Result<Listener, String> {
        let link = socket_path(profile);
        clear(&link);
        let (socket, fallback) = match UnixListener::bind(&link) {
            Ok(socket) => (socket, None),
            Err(first) => {
                let dir = crate::profile::make_temp_dir(FALLBACK_PREFIX).map_err(|second| {
                    format!(
                        "cannot make a socket in {} ({first}) nor a directory for one in {second}",
                        profile.display()
                    )
                })?;
                let target = dir.join("sock");
                let bound = UnixListener::bind(&target)
                    .and_then(|socket| std::os::unix::fs::symlink(&target, &link).map(|()| socket));
                match bound {
                    Ok(socket) => (socket, Some(dir)),
                    Err(second) => {
                        let _ = std::fs::remove_dir_all(&dir);
                        return Err(format!(
                            "cannot make a socket in {} ({first}) nor a link there to one \
                             in {} ({second})",
                            profile.display(),
                            dir.display()
                        ));
                    }
                }
            }
        };
        let listener = Listener {
            socket,
            link,
            fallback,
        };
        // Between the bind and this the socket has the umask's mode, which is
        // why it is only ever made inside a 0700 directory: the profile, or
        // the fallback `mkdtemp` made. On a Mac the socket's own mode is not
        // consulted by `connect` at all, and the directory is the whole of
        // the access control there — which it is here too, in the end.
        let bound = listener
            .fallback
            .as_ref()
            .map_or(listener.link.clone(), |dir| dir.join("sock"));
        std::fs::set_permissions(&bound, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot make {} private: {e}", bound.display()))?;
        listener
            .socket
            .set_nonblocking(true)
            .map_err(|e| format!("cannot listen on {}: {e}", listener.link.display()))?;
        Ok(listener)
    }

    /// The descriptor to wait on: readable when a sender has connected.
    pub fn fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }

    /// Where a sender looks for this socket.
    pub fn path(&self) -> &Path {
        &self.link
    }

    /// Every sender that has connected, each read to the end of what it said.
    ///
    /// An error is one that will not go away by trying again — out of
    /// descriptors, the socket gone bad — and the caller stops listening
    /// rather than wake up for it on every turn of the loop.
    pub fn accept_ready(&mut self) -> Result<Vec<Delivery>, String> {
        let mut deliveries = Vec::new();
        loop {
            match self.socket.accept() {
                Ok((stream, _)) => deliveries.push(Delivery::read(stream)),
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(deliveries),
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::Interrupted | ErrorKind::ConnectionAborted
                    ) => {}
                Err(e) if deliveries.is_empty() => {
                    return Err(format!("{}: {e}", self.link.display()))
                }
                // Answer the ones already taken; the error comes back on the
                // next turn if it is still there.
                Err(_) => return Ok(deliveries),
            }
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.link);
        if let Some(dir) = &self.fallback {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Remove what is at the socket's path, and the fallback directory a link
/// there points into if it is one this program made: what a crash left.
fn clear(link: &Path) {
    if let Ok(target) = std::fs::read_link(link) {
        if let Some(dir) = target.parent() {
            let ours = dir
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(FALLBACK_PREFIX));
            if ours && dir.starts_with(std::env::temp_dir()) {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }
    let _ = std::fs::remove_file(link);
}

/// One sender, read: the urls it asked for, and the connection to answer on.
#[derive(Debug)]
pub struct Delivery {
    stream: UnixStream,
    /// Each line it sent, checked, in the order it sent them.
    pub lines: Vec<Checked>,
    /// Why the rest of what it said was not read, when it said too much.
    overflow: Option<String>,
}

impl Delivery {
    /// Read what `stream` has to say, for up to [`REQUEST_TIMEOUT`].
    pub fn read(mut stream: UnixStream) -> Delivery {
        // On a Mac an accepted socket inherits the listener's O_NONBLOCK, and
        // a read timeout on a non-blocking socket is no timeout at all.
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_write_timeout(Some(REQUEST_TIMEOUT));
        let (lines, overflow) = read_lines(&mut stream);
        Delivery {
            stream,
            lines: lines.iter().map(|line| check(line)).collect(),
            overflow,
        }
    }

    /// Say what became of each line — `opened` is [`Delivery::lines`] after
    /// the opening, one for one — and hang up. A sender that has gone is not
    /// an error: the tabs are open whether it hears so or not.
    pub fn answer(mut self, opened: &[Checked]) {
        let mut reply: String = opened.iter().map(reply_line).collect();
        if let Some(why) = &self.overflow {
            reply.push_str(&reply_line(&Err(why.clone())));
        }
        let _ = self.stream.write_all(reply.as_bytes());
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// The lines a sender wrote, up to the end of what it said, the limits, or
/// [`REQUEST_TIMEOUT`], whichever is first; and why the rest was left, if it
/// was.
///
/// A last line with no newline counts when the sender hung up after it, and
/// not when the time ran out in the middle of it: then it may be half a url.
fn read_lines(stream: &mut UnixStream) -> (Vec<Vec<u8>>, Option<String>) {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut said = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut ended = false;
    let mut overflow = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                ended = true;
                break;
            }
            Ok(n) => said.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
        if said.len() > MAX_REQUEST_BYTES {
            said.truncate(MAX_REQUEST_BYTES);
            overflow = Some(too_much());
            break;
        }
    }
    let complete = ended && overflow.is_none();
    let mut lines: Vec<Vec<u8>> = said.split(|&b| b == b'\n').map(<[u8]>::to_vec).collect();
    // `split` leaves one piece after the last newline: empty when the text
    // ended in one, part of a line otherwise.
    let last = lines.pop().unwrap_or_default();
    if complete && !last.is_empty() {
        lines.push(last);
    }
    if lines.len() > MAX_URLS {
        lines.truncate(MAX_URLS);
        overflow = Some(too_much());
    }
    (lines, overflow)
}

/// The answer for a sender that said more than one connection may.
fn too_much() -> String {
    format!(
        "more than {MAX_URLS} urls or {} KiB at once is not a list of links; the rest was not read",
        MAX_REQUEST_BYTES / 1024
    )
}

/// What became of a [`deliver`].
#[derive(Debug, PartialEq, Eq)]
pub enum Delivered {
    /// Every url was opened.
    Opened,
    /// Some were not, and these are the reasons, one per url; the others were
    /// opened.
    Refused(Vec<String>),
    /// No blinkterm is listening on the profile: the caller starts one.
    NobodyThere,
    /// One took the urls and has not answered in [`REPLY_TIMEOUT`].
    NoAnswer,
}

/// Hand `urls` to the blinkterm running on the profile at `profile`.
pub fn deliver(profile: &Path, urls: &[String]) -> Result<Delivered, String> {
    let link = socket_path(profile);
    let target = match std::fs::read_link(&link) {
        Ok(target) => profile.join(target),
        Err(_) => link,
    };
    let mut stream = match UnixStream::connect(&target) {
        Ok(stream) => stream,
        // No file, a file nobody listens on (what a crash leaves), or a path
        // too long to be one with no link to a shorter one: nobody there.
        Err(e)
            if matches!(
                e.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::InvalidInput
            ) =>
        {
            return Ok(Delivered::NobodyThere)
        }
        Err(e) => {
            return Err(format!(
                "cannot reach the blinkterm on {}: {e}",
                profile.display()
            ))
        }
    };
    let request: String = urls
        .iter()
        .map(|url| format!("{}\n", url.replace(['\n', '\r'], " ")))
        .collect();
    let _ = stream.set_write_timeout(Some(REPLY_TIMEOUT));
    let wrote = stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.shutdown(Shutdown::Write));
    if let Err(e) = wrote {
        match e.kind() {
            // It hung up without reading: it was on its way out. What it did
            // answer, if anything, is still read below.
            ErrorKind::BrokenPipe | ErrorKind::ConnectionReset => {}
            ErrorKind::WouldBlock | ErrorKind::TimedOut => return Ok(Delivered::NoAnswer),
            _ => {
                return Err(format!(
                    "cannot talk to the blinkterm on {}: {e}",
                    profile.display()
                ))
            }
        }
    }
    let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT));
    let mut answer = Vec::new();
    if let Err(e) = stream.read_to_end(&mut answer) {
        if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
            return Ok(Delivered::NoAnswer);
        }
    }
    let replies: Vec<Checked> = String::from_utf8_lossy(&answer)
        .lines()
        .filter(|line| !line.is_empty())
        .map(parse_reply)
        .collect();
    if replies.is_empty() && !urls.is_empty() {
        return Ok(Delivered::NobodyThere);
    }
    let mut reasons: Vec<String> = replies.iter().filter_map(|r| r.clone().err()).collect();
    if replies.len() < urls.len() {
        reasons.push(format!(
            "the blinkterm on {} answered {} of {} urls",
            profile.display(),
            replies.len(),
            urls.len()
        ));
    }
    if reasons.is_empty() {
        Ok(Delivered::Opened)
    } else {
        Ok(Delivered::Refused(reasons))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of this test's own, gone again before and after.
    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blinkterm-remote-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    /// Wait until `fd` is readable, for up to two seconds.
    fn readable(fd: RawFd) -> bool {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `poll` is one valid `pollfd` on this stack frame, the count
        // says one, and `poll(2)` writes only its `revents`.
        let n = unsafe { libc::poll(&mut poll, 1, 2000) };
        n == 1 && poll.revents & libc::POLLIN != 0
    }

    /// Answer every line as it was checked, as the program does when every
    /// tab opened.
    fn serve_once(listener: &mut Listener) -> Vec<Checked> {
        assert!(readable(listener.fd()), "nobody connected");
        let deliveries = listener.accept_ready().expect("accepted");
        let mut all = Vec::new();
        for delivery in deliveries {
            let lines = delivery.lines.clone();
            delivery.answer(&lines);
            all.extend(lines);
        }
        all
    }

    fn urls(list: &[&str]) -> Vec<String> {
        list.iter().map(|u| u.to_string()).collect()
    }

    #[test]
    fn a_request_line_is_checked_as_the_url_bar_checks_what_is_typed() {
        assert_eq!(check(b"example.com"), Ok("https://example.com".into()));
        assert_eq!(check(b"/tmp/x.html"), Ok("file:///tmp/x.html".into()));
        assert_eq!(check(b"localhost:3000"), Ok("http://localhost:3000".into()));
        assert_eq!(check(b"about:blank"), Ok("about:blank".into()));
        assert_eq!(
            check(b"  https://example.com/a?b#c \r"),
            Ok("https://example.com/a?b#c".into())
        );
        assert_eq!(
            check(b"example.com:8080/x"),
            Ok("https://example.com:8080/x".into())
        );
        assert!(accepted("https://example.com"));
        assert!(!accepted("javascript:x"));
    }

    #[test]
    fn only_http_https_file_and_about_are_opened() {
        for (line, scheme) in [
            ("javascript:alert(1)", "javascript"),
            ("data:text/html,<b>x</b>", "data"),
            ("chrome://settings", "chrome"),
            ("mailto:a@example.com", "mailto"),
            ("ftp://example.com/x", "ftp"),
            ("JavaScript:x", "JavaScript"),
        ] {
            let why = check(line.as_bytes()).unwrap_err();
            assert!(why.contains(&format!("{scheme}:")), "{line}: {why}");
        }
    }

    #[test]
    fn an_empty_line_bad_utf8_or_a_line_too_long_is_refused() {
        assert!(check(b"").is_err());
        assert!(check(b"   \x07 ").is_err());
        assert!(check(b"\xff\xfeexample.com").unwrap_err().contains("UTF-8"));
        let long = format!("https://example.com/{}", "a".repeat(MAX_LINE_BYTES));
        assert!(check(long.as_bytes()).unwrap_err().contains("KiB"));
    }

    #[test]
    fn a_reply_line_is_read_back_as_it_was_written() {
        let opened: Checked = Ok("https://example.com/".into());
        assert_eq!(parse_reply(&reply_line(&opened)), opened);
        let refused: Checked = Err("no good".into());
        assert_eq!(parse_reply(&reply_line(&refused)), refused);
        let sly: Checked = Err("a \x1b]52;c;x\x07 b".into());
        let line = reply_line(&sly);
        assert!(!line.contains('\x1b'), "{line:?}");
        assert_eq!(line.matches('\n').count(), 1, "{line:?}");
        let back = parse_reply(&line).unwrap_err();
        assert!(!back.contains('\x1b') && back.starts_with('a'), "{back:?}");
        assert!(parse_reply("what").is_err());
    }

    #[test]
    fn a_listener_makes_a_private_socket_next_to_the_lock() {
        use std::os::unix::fs::FileTypeExt;
        let dir = scratch("private");
        let listener = Listener::bind(&dir).expect("bound");
        assert_eq!(listener.path(), dir.join(SOCKET_FILE));
        let meta = std::fs::symlink_metadata(listener.path()).expect("there");
        assert!(meta.file_type().is_socket());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        // SAFETY: `F_GETFD` reads a descriptor's flags and no memory; the
        // descriptor is the listener's, open for the whole call.
        let flags = unsafe { libc::fcntl(listener.fd(), libc::F_GETFD) };
        assert!(
            flags >= 0 && flags & libc::FD_CLOEXEC != 0,
            "the engine or a picker would inherit the socket"
        );
        drop(listener);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn whatever_a_crash_left_at_the_socket_path_is_replaced() {
        let dir = scratch("stale");
        std::fs::write(socket_path(&dir), b"left").expect("a plain file");
        let listener = Listener::bind(&dir).expect("bound over a file");
        drop(listener);

        let left = crate::profile::make_temp_dir(FALLBACK_PREFIX).expect("a fallback dir");
        std::os::unix::fs::symlink(left.join("sock"), socket_path(&dir)).expect("a link");
        let listener = Listener::bind(&dir).expect("bound over a dangling link");
        assert!(
            !left.exists(),
            "the crash's fallback directory is still there"
        );
        let meta = std::fs::symlink_metadata(listener.path()).expect("there");
        assert!(!meta.file_type().is_symlink());
        drop(listener);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_delivery_reaches_a_listener_and_every_line_is_answered_in_order() {
        let dir = scratch("deliver");
        let mut listener = Listener::bind(&dir).expect("bound");
        let sender = {
            let dir = dir.clone();
            std::thread::spawn(move || {
                deliver(
                    &dir,
                    &urls(&["example.com", "javascript:x", "http://a.example/"]),
                )
            })
        };
        let lines = serve_once(&mut listener);
        assert_eq!(
            lines,
            vec![
                Ok("https://example.com".to_string()),
                Err(refused("javascript")),
                Ok("http://a.example/".to_string()),
            ]
        );
        let delivered = sender.join().expect("the sender").expect("delivered");
        assert_eq!(delivered, Delivered::Refused(vec![refused("javascript")]));

        let sender = {
            let dir = dir.clone();
            std::thread::spawn(move || deliver(&dir, &urls(&["example.com"])))
        };
        serve_once(&mut listener);
        assert_eq!(sender.join().unwrap(), Ok(Delivered::Opened));
        drop(listener);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn with_nobody_listening_deliver_says_so() {
        let dir = scratch("nobody");
        assert_eq!(
            deliver(&dir, &urls(&["example.com"])),
            Ok(Delivered::NobodyThere)
        );
        // The standard library's listener does not remove its file when it
        // goes, which is exactly what a crash leaves: a socket nobody is on.
        drop(UnixListener::bind(socket_path(&dir)).expect("bound"));
        assert!(socket_path(&dir).exists());
        assert_eq!(
            deliver(&dir, &urls(&["example.com"])),
            Ok(Delivered::NobodyThere)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_profile_too_long_for_a_socket_gets_a_short_one_behind_a_symlink() {
        let root = scratch("long");
        let dir = root.join("p".repeat(120));
        std::fs::create_dir_all(&dir).expect("a deep profile");
        let mut listener = Listener::bind(&dir).expect("bound somewhere");
        let link = socket_path(&dir);
        let target = std::fs::read_link(&link).expect("a link");
        let fallback = target.parent().expect("a directory").to_path_buf();
        assert!(fallback
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(FALLBACK_PREFIX));
        let mode = std::fs::metadata(&fallback).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let sender = {
            let dir = dir.clone();
            std::thread::spawn(move || deliver(&dir, &urls(&["example.com"])))
        };
        serve_once(&mut listener);
        assert_eq!(sender.join().unwrap(), Ok(Delivered::Opened));
        drop(listener);
        assert!(std::fs::symlink_metadata(&link).is_err(), "the link stayed");
        assert!(!fallback.exists(), "the fallback directory stayed");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_client_that_never_writes_holds_the_loop_for_half_a_second_at_most() {
        let dir = scratch("silent");
        let mut listener = Listener::bind(&dir).expect("bound");
        let silent = UnixStream::connect(listener.path()).expect("connected");
        assert!(readable(listener.fd()));
        let started = Instant::now();
        let deliveries = listener.accept_ready().expect("accepted");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(deliveries.len(), 1);
        assert!(deliveries[0].lines.is_empty());
        drop(silent);
        drop(listener);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_sender_that_says_too_much_is_told_so_after_the_first_ones() {
        let dir = scratch("much");
        let mut listener = Listener::bind(&dir).expect("bound");
        let many: Vec<String> = (0..MAX_URLS + 3)
            .map(|n| format!("https://example.com/{n}"))
            .collect();
        let sender = {
            let dir = dir.clone();
            std::thread::spawn(move || deliver(&dir, &many))
        };
        let lines = serve_once(&mut listener);
        assert_eq!(lines.len(), MAX_URLS);
        match sender.join().unwrap() {
            Ok(Delivered::Refused(reasons)) => {
                assert!(
                    reasons.iter().any(|why| why.contains("more than")),
                    "{reasons:?}"
                );
            }
            other => panic!("{other:?}"),
        }
        drop(listener);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_listener_is_gone_with_its_owner() {
        let dir = scratch("gone");
        let listener = Listener::bind(&dir).expect("bound");
        assert!(socket_path(&dir).exists());
        drop(listener);
        assert!(std::fs::symlink_metadata(socket_path(&dir)).is_err());
        assert_eq!(
            deliver(&dir, &urls(&["example.com"])),
            Ok(Delivered::NobodyThere)
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
