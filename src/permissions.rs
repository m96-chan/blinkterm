//! Every page told no, and the origins the person allowed, kept in the
//! profile.
//!
//! # What the engine does, and why this is a line and not a prompt
//!
//! Measured against `chrome-headless-shell` 153 with nothing set: every ask
//! is refused at once — `requestPermission()` answers `default`, a location
//! fails `User denied Geolocation`, the clipboard API `NotAllowedError` — but
//! `navigator.permissions.query` says `prompt`, which is the state a site
//! reads before deciding to keep its "turn on notifications" banner up and
//! to ask again. And nothing arrives on any session while a page asks:
//! there is no CDP event for a permission request, in this protocol or any
//! other this program knows of. Hearing an ask would take a script planted
//! in the page's own world, wrapping `Notification` and `getUserMedia` —
//! detectable (`Notification.toString()` loses its `native code`), fragile,
//! and on this engine asking a question whose "yes" buys nothing a person
//! could see.
//!
//! So the shape is *deny, and let the person allow an origin themselves*.
//! [`opening_commands`] tell a fresh engine `denied` for every name in
//! [`NAMES`] with no origin — browser-wide, which the engine applies to
//! every origin, every target made afterwards and a `data:` page alike —
//! and then `granted` for what the profile's file allows. `alt+p` opens a
//! line on the row that sets exactly the words typed for the page in
//! front's origin ([`origin_commands`]), and the file remembers it.
//!
//! `Browser.setPermission` is the one command used. It takes the W3C
//! `PermissionDescriptor` names (`camera`, `clipboard-read`, …) and a
//! `denied` state; `Browser.grantPermissions` takes the CDP spellings and
//! has no `denied`; `Browser.resetPermissions` is `prompt`, which is the
//! answer this module exists to replace. An origin's own entry outranks the
//! browser-wide one whichever was set last, so a word taken back is set
//! `denied` *for that origin* rather than left to the browser-wide deny,
//! which would not override a grant made earlier.
//!
//! What a grant does is the engine's business. On the headless shell it
//! makes the clipboard API work, into the engine's own clipboard, and makes
//! a site believe it may use a camera, a microphone and a location it then
//! cannot find; `Notification.permission` stays `denied` whatever is set,
//! because the service behind it is not in the shell. On a Chromium with a
//! webcam it is the switch a call page needs. Nothing here is a `call`: the
//! browser connection answers these at once and the answer says nothing.
//!
//! # The file
//!
//! `<profile>/permissions`, one line per change — the origin, a tab, a word,
//! a tab, `allow` or `deny` — appended, folded on load, and compacted when it
//! has grown to twice [`CAP`] lines or ends in a line cut short, exactly as
//! [`crate::zoom`] keeps its levels. It is a list of sites somebody visited,
//! so it is 0600 like the history beside it, and a temporary profile keeps
//! it in memory and writes nothing. The words and not the engine's names,
//! because the file is the person's to read and edit as much as the row's,
//! and `clipboard` is two engine names that have no business being separate
//! to a person.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::json::Json;
use crate::text;

/// A permission, as this program names it on the row and in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Permission {
    Camera,
    Microphone,
    Location,
    Notifications,
    Clipboard,
}

impl Permission {
    /// In the order the row lists them, which is also their order in a set.
    pub const ALL: [Permission; 5] = [
        Permission::Camera,
        Permission::Microphone,
        Permission::Location,
        Permission::Notifications,
        Permission::Clipboard,
    ];

    /// The word typed and written: `camera`, `microphone`, `location`,
    /// `notifications`, `clipboard`.
    pub fn word(self) -> &'static str {
        match self {
            Permission::Camera => "camera",
            Permission::Microphone => "microphone",
            Permission::Location => "location",
            Permission::Notifications => "notifications",
            Permission::Clipboard => "clipboard",
        }
    }

    /// The word read, in any case; `None` for anything else.
    pub fn parse(word: &str) -> Option<Permission> {
        Permission::ALL
            .into_iter()
            .find(|permission| permission.word().eq_ignore_ascii_case(word))
    }

    /// The engine's `PermissionDescriptor` names this one stands for:
    /// `clipboard` is `clipboard-read` and `clipboard-write`, `location` is
    /// `geolocation`, the rest are themselves.
    pub fn names(self) -> &'static [&'static str] {
        match self {
            Permission::Camera => &["camera"],
            Permission::Microphone => &["microphone"],
            Permission::Location => &["geolocation"],
            Permission::Notifications => &["notifications"],
            Permission::Clipboard => &["clipboard-read", "clipboard-write"],
        }
    }
}

/// Every engine name this program sets, once each: the union of
/// [`Permission::names`], in [`Permission::ALL`]'s order.
pub const NAMES: [&str; 6] = [
    "camera",
    "microphone",
    "geolocation",
    "notifications",
    "clipboard-read",
    "clipboard-write",
];

/// `Browser.setPermission` parameters: `name` to `setting`, for `origin` or,
/// with `None`, for every origin.
pub fn set_params(name: &str, setting: &str, origin: Option<&str>) -> Json {
    let mut fields = vec![
        (
            "permission",
            Json::object(vec![("name", Json::string(name))]),
        ),
        ("setting", Json::string(setting)),
    ];
    if let Some(origin) = origin {
        fields.push(("origin", Json::string(origin)));
    }
    Json::object(fields)
}

/// The method every command here is.
const SET: &str = "Browser.setPermission";

/// The commands that tell a fresh engine no for every origin, then yes for
/// what `allowed` holds: `(method, params)` pairs in order, so that a test
/// can read them and `app::boot` can send them as notifications.
pub fn opening_commands(allowed: &Allowed) -> Vec<(&'static str, Json)> {
    let mut commands: Vec<(&'static str, Json)> = NAMES
        .iter()
        .map(|name| (SET, set_params(name, "denied", None)))
        .collect();
    for (origin, set) in allowed.iter() {
        for permission in set {
            for name in permission.names() {
                commands.push((SET, set_params(name, "granted", Some(origin))));
            }
        }
    }
    commands
}

/// The commands for one origin after the allow line: every name in
/// [`NAMES`] set `granted` if one of `set` covers it, else `denied` *with
/// the origin*, so that a word taken back falls back to no through an entry
/// the engine keeps per origin — a browser-wide deny sent later does not
/// override an origin's grant, so the origin's own entry has to say it.
pub fn origin_commands(origin: &str, set: &[Permission]) -> Vec<(&'static str, Json)> {
    NAMES
        .iter()
        .map(|name| {
            let granted = set
                .iter()
                .any(|permission| permission.names().contains(name));
            let setting = if granted { "granted" } else { "denied" };
            (SET, set_params(name, setting, Some(origin)))
        })
        .collect()
}

/// The origin a permission is kept under.
///
/// `scheme://host[:port]` for `http` and `https`: the scheme and host
/// lower-cased, an IPv6 host in its brackets, the port dropped when it is
/// the scheme's default, no user name — which is what the engine reads out
/// of an origin itself, and what a grant on one port not reaching another
/// (measured) says it keys by. And only when every character is plain
/// ([`text::is_plain`]) and not a space, so that it is one field of one line
/// of the file and safe on the row. `None` for everything else —
/// `about:blank`, `data:`, `file:`, the engine's `chrome-error:` — which the
/// engine calls opaque and refuses anyway.
pub fn origin_of(url: &str) -> Option<String> {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let scheme = ["http://", "https://"]
        .into_iter()
        .find(|scheme| lower.starts_with(scheme))?;
    let rest = &url[scheme.len()..];
    let authority = &rest[..rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len())];
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = if host_port.starts_with('[') {
        let end = host_port.find(']')?;
        (&host_port[..=end], host_port[end + 1..].strip_prefix(':'))
    } else {
        match host_port.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        }
    };
    let plain = |part: &str| {
        part.chars()
            .all(|c| text::is_plain(c) && !c.is_whitespace())
    };
    if host.is_empty() || !plain(host) {
        return None;
    }
    let default = if scheme == "https://" { 443 } else { 80 };
    let port = match port.filter(|port| !port.is_empty()) {
        None => None,
        Some(port) => {
            let number: u16 = port
                .parse()
                .ok()
                .filter(|_| port.bytes().all(|b| b.is_ascii_digit()))?;
            (number != default).then_some(number)
        }
    };
    let host = host.to_ascii_lowercase();
    Some(match port {
        Some(port) => format!("{scheme}{host}:{port}"),
        None => format!("{scheme}{host}"),
    })
}

/// The file in the profile the allowances are kept in.
pub const FILE: &str = "permissions";

/// Origins kept: more sites than anybody allows a camera, and a fold of
/// this many lines is instant.
pub const CAP: usize = 500;

/// Lines in the file at which it is compacted on load.
pub const COMPACT_AT: usize = 2 * CAP;

/// What is allowed, per origin, in `<profile>/permissions` — or nowhere, for
/// a temporary profile. Folded like [`crate::zoom::Zooms`].
#[derive(Debug)]
pub struct Allowed {
    /// Each origin's words, sorted, never empty.
    set: HashMap<String, Vec<Permission>>,
    /// The origins in `set`, oldest change first: what goes past [`CAP`].
    order: Vec<String>,
    /// The file, or `None` for a temporary profile.
    path: Option<PathBuf>,
}

impl Allowed {
    /// Allowances that are never written anywhere: a temporary profile's.
    pub fn in_memory() -> Allowed {
        Allowed {
            set: HashMap::new(),
            order: Vec::new(),
            path: None,
        }
    }

    /// The allowances kept in the profile at `dir`, and where to add to them.
    ///
    /// As [`crate::zoom::Zooms::load`]: a missing file is nothing allowed, a
    /// line that does not parse is skipped, as is a last line with no
    /// newline after it, and nothing here fails. Compacted when it has grown
    /// to [`COMPACT_AT`] lines or its last line was cut short.
    pub fn load(dir: &Path) -> Allowed {
        let path = dir.join(FILE);
        let file = std::fs::read(&path).unwrap_or_default();
        let file = String::from_utf8_lossy(&file);
        let mut allowed = Allowed::in_memory();
        let mut lines = 0;
        for line in file.split_inclusive('\n') {
            lines += 1;
            let Some(line) = line.strip_suffix('\n') else {
                continue;
            };
            if let Some((origin, permission, allow)) = Allowed::parse_line(line) {
                allowed.remember(origin, permission, allow);
            }
        }
        allowed.path = Some(path);
        if lines >= COMPACT_AT || !(file.is_empty() || file.ends_with('\n')) {
            let _ = allowed.compact();
        }
        allowed
    }

    /// One line of the file: the origin, a tab, a word, a tab, `allow` or
    /// `deny`; or `None` for a line that is not one. The origin has to be
    /// one [`origin_of`] would have made, character for character, so that
    /// a hand-edited line cannot put anything on the row that a url could
    /// not.
    pub fn parse_line(line: &str) -> Option<(String, Permission, bool)> {
        let mut fields = line.split('\t');
        let (origin, word, verdict) = (fields.next()?, fields.next()?, fields.next()?);
        if fields.next().is_some() || origin_of(origin).as_deref() != Some(origin) {
            return None;
        }
        let permission = Permission::parse(word).filter(|p| p.word() == word)?;
        let allow = match verdict {
            "allow" => true,
            "deny" => false,
            _ => return None,
        };
        Some((origin.to_string(), permission, allow))
    }

    /// What an origin may use; empty for one never allowed.
    pub fn get(&self, origin: &str) -> &[Permission] {
        self.set.get(origin).map_or(&[], Vec::as_slice)
    }

    /// Set exactly `set` for `origin`: one `allow` line per word gained, one
    /// `deny` line per word lost, appended in one write if there is a file.
    /// An empty set forgets the origin.
    ///
    /// The error is for the row to say and a test to see: an allowance that
    /// cannot be written is still the one the engine was told, for this run.
    pub fn set(&mut self, origin: &str, set: &[Permission]) -> Result<(), String> {
        let before = self.get(origin).to_vec();
        let mut lines = String::new();
        for permission in Permission::ALL {
            let now = set.contains(&permission);
            if now == before.contains(&permission) {
                continue;
            }
            self.remember(origin.to_string(), permission, now);
            lines.push_str(&line(origin, permission, now));
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        if lines.is_empty() {
            return Ok(());
        }
        OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut file| file.write_all(lines.as_bytes()))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// Every origin with something allowed, oldest change first, with what.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[Permission])> {
        self.order
            .iter()
            .map(|origin| (origin.as_str(), self.get(origin)))
    }

    /// The map's half of [`Allowed::set`] and of every line read.
    fn remember(&mut self, origin: String, permission: Permission, allow: bool) {
        self.order.retain(|known| *known != origin);
        let mut words = self.set.remove(&origin).unwrap_or_default();
        words.retain(|known| *known != permission);
        if allow {
            words.push(permission);
            words.sort();
        }
        if words.is_empty() {
            return;
        }
        self.set.insert(origin.clone(), words);
        self.order.push(origin);
        if self.order.len() > CAP {
            let gone = self.order.remove(0);
            self.set.remove(&gone);
        }
    }

    /// Write the allowances as a fresh file, oldest first so that appending
    /// goes on in order, to a file beside it that is then renamed over it.
    fn compact(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let fresh = path.with_extension("tmp");
        let file: String = self
            .iter()
            .flat_map(|(origin, set)| set.iter().map(move |p| line(origin, *p, true)))
            .collect();
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&fresh)
            .and_then(|mut out| out.write_all(file.as_bytes()))
            .and_then(|()| std::fs::rename(&fresh, path))
            .map_err(|e| format!("cannot compact {}: {e}", path.display()))
    }
}

/// One line of the file, newline included.
fn line(origin: &str, permission: Permission, allow: bool) -> String {
    let verdict = if allow { "allow" } else { "deny" };
    format!("{origin}\t{}\t{verdict}\n", permission.word())
}

/// The words on the allow line, read: the permissions named, each once and
/// in [`Permission::ALL`]'s order, or the first word that is not one, for
/// the sentence. Spaces or commas between them, since the row's own
/// sentence ([`applied`]) writes them with commas and a person copies what
/// they see.
pub fn parse_words(text: &str) -> Result<Vec<Permission>, String> {
    let mut set = Vec::new();
    for word in text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty())
    {
        let permission = Permission::parse(word).ok_or_else(|| word.to_string())?;
        if !set.contains(&permission) {
            set.push(permission);
        }
    }
    set.sort();
    Ok(set)
}

/// `camera microphone`: the line's starting text for `set`.
pub fn words(set: &[Permission]) -> String {
    set.iter()
        .map(|permission| permission.word())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `allowed https://meet.example: camera, microphone`, or `nothing allowed
/// for https://meet.example` for an empty set.
pub fn applied(origin: &str, set: &[Permission]) -> String {
    let origin = text::sanitize(origin);
    if set.is_empty() {
        return format!("nothing allowed for {origin}");
    }
    let words: Vec<&str> = set.iter().map(|permission| permission.word()).collect();
    format!("allowed {origin}: {}", words.join(", "))
}

/// `not a permission: mic; they are camera, microphone, location,
/// notifications, clipboard`. The word is the person's own, but it may have
/// come in a paste, which can carry anything, so it is made plain here.
pub fn refused(word: &str) -> String {
    let all: Vec<&str> = Permission::ALL.iter().map(|p| p.word()).collect();
    format!(
        "not a permission: {}; they are {}",
        text::sanitize(word),
        all.join(", ")
    )
}

/// What `alt+p` says on a page that has no origin to allow.
pub const NO_ORIGIN: &str = "this page has no origin to allow";

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use Permission::*;

    /// A scratch directory of this test's own, gone again before and after.
    fn scratch(what: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "blinkterm-permissions-{what}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    /// `(name, setting, origin)` of each command, to compare.
    fn read(commands: &[(&str, Json)]) -> Vec<(String, String, Option<String>)> {
        commands
            .iter()
            .map(|(method, params)| {
                assert_eq!(*method, "Browser.setPermission");
                (
                    params
                        .path(&["permission", "name"])
                        .and_then(Json::as_str)
                        .expect("a name")
                        .to_string(),
                    params
                        .get("setting")
                        .and_then(Json::as_str)
                        .expect("a setting")
                        .to_string(),
                    params
                        .get("origin")
                        .and_then(Json::as_str)
                        .map(str::to_string),
                )
            })
            .collect()
    }

    #[test]
    fn an_origin_is_the_scheme_host_and_port_of_an_http_url_and_nothing_else() {
        for (url, origin) in [
            ("https://A.example:443/x", "https://a.example"),
            ("HTTPS://meet.example/room?x#y", "https://meet.example"),
            ("http://h:8080/", "http://h:8080"),
            ("http://h:80/", "http://h"),
            ("https://h:80/", "https://h:80"),
            ("http://u@h/", "http://h"),
            ("http://u:pw@h:81/", "http://h:81"),
            ("http://[::1]:3000/", "http://[::1]:3000"),
            ("http://[::1]/", "http://[::1]"),
            ("http://127.0.0.1:38535", "http://127.0.0.1:38535"),
        ] {
            assert_eq!(origin_of(url).as_deref(), Some(origin), "{url}");
        }
        for url in [
            "file:///etc/passwd",
            "data:text/html,x",
            "about:blank",
            "chrome-error://chromewebdata/",
            "http://",
            "http://h\tx/",
            "http://h x/",
            "http://h\u{1b}/",
            "http://h:port/",
            "http://h:99999/",
            "http://[::1/",
        ] {
            assert_eq!(origin_of(url), None, "{url:?}");
        }
    }

    #[test]
    fn each_word_names_its_engine_permissions_and_clipboard_names_two() {
        let mut names: Vec<&str> = Vec::new();
        for permission in Permission::ALL {
            assert_eq!(Permission::parse(permission.word()), Some(permission));
            names.extend(permission.names());
        }
        assert_eq!(names, NAMES, "the union, once each, in order");
        assert_eq!(Clipboard.names(), ["clipboard-read", "clipboard-write"]);
        assert_eq!(Location.names(), ["geolocation"]);
        assert_eq!(Permission::parse("CAMERA"), Some(Camera));
        assert_eq!(Permission::parse("geolocation"), None, "the person's words");
    }

    #[test]
    fn the_words_on_the_line_are_read_once_each_and_a_stranger_is_refused_with_the_list() {
        assert_eq!(
            parse_words("  location camera location "),
            Ok(vec![Camera, Location])
        );
        assert_eq!(
            parse_words("camera, microphone,clipboard"),
            Ok(vec![Camera, Microphone, Clipboard])
        );
        assert_eq!(parse_words(""), Ok(vec![]));
        assert_eq!(parse_words("camera mic location"), Err("mic".to_string()));
        assert_eq!(
            refused("mic"),
            "not a permission: mic; they are camera, microphone, location, notifications, clipboard"
        );
        assert_eq!(words(&[Camera, Microphone]), "camera microphone");
        assert_eq!(words(&[]), "");
    }

    #[test]
    fn the_file_folds_to_the_last_line_per_origin_and_word_and_a_deny_forgets_the_origin_when_it_was_its_last(
    ) {
        let dir = scratch("fold");
        std::fs::write(
            dir.join(FILE),
            "https://meet.example\tcamera\tallow\n\
             https://meet.example\tmicrophone\tallow\n\
             http://wiki.corp:8080\tclipboard\tallow\n\
             https://meet.example\tmicrophone\tdeny\n\
             http://wiki.corp:8080\tclipboard\tdeny\n",
        )
        .expect("written");
        let allowed = Allowed::load(&dir);
        assert_eq!(allowed.get("https://meet.example"), [Camera]);
        assert_eq!(allowed.get("http://wiki.corp:8080"), []);
        let origins: Vec<&str> = allowed.iter().map(|(origin, _)| origin).collect();
        assert_eq!(origins, ["https://meet.example"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_with_a_control_character_an_unknown_word_or_a_third_field_that_is_neither_is_skipped()
    {
        assert_eq!(
            Allowed::parse_line("https://meet.example\tcamera\tallow"),
            Some(("https://meet.example".to_string(), Camera, true))
        );
        for broken in [
            "https://meet.\u{1b}example\tcamera\tallow",
            "https://meet.example\tmic\tallow",
            "https://meet.example\tCAMERA\tallow",
            "https://meet.example\tcamera\tmaybe",
            "https://meet.example\tcamera",
            "https://meet.example\tcamera\tallow\textra",
            "https://Meet.example\tcamera\tallow",
            "https://meet.example/\tcamera\tallow",
            "meet.example\tcamera\tallow",
            "",
        ] {
            assert_eq!(Allowed::parse_line(broken), None, "{broken:?}");
        }
    }

    #[test]
    fn a_set_appends_one_line_per_word_gained_and_lost_and_the_file_is_0600() {
        let dir = scratch("set");
        let mut allowed = Allowed::load(&dir);
        allowed
            .set("https://meet.example", &[Camera, Microphone])
            .expect("written");
        allowed
            .set("https://meet.example", &[Camera, Location])
            .expect("written");
        allowed
            .set("https://meet.example", &[Camera, Location])
            .expect("nothing to write");
        let file = std::fs::read_to_string(dir.join(FILE)).expect("a file");
        assert_eq!(
            file,
            "https://meet.example\tcamera\tallow\n\
             https://meet.example\tmicrophone\tallow\n\
             https://meet.example\tmicrophone\tdeny\n\
             https://meet.example\tlocation\tallow\n"
        );
        let mode = std::fs::metadata(dir.join(FILE))
            .expect("the file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "a list of sites visited nobody else may read");
        assert_eq!(
            Allowed::load(&dir).get("https://meet.example"),
            [Camera, Location]
        );
        allowed.set("https://meet.example", &[]).expect("written");
        assert!(allowed.iter().next().is_none(), "an empty set forgets");
        assert!(Allowed::load(&dir).iter().next().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_is_compacted_at_twice_the_cap_and_after_a_line_cut_short() {
        let dir = scratch("compact");
        let mut allowed = Allowed::load(&dir);
        allowed
            .set("https://a.example", &[Camera])
            .expect("written");
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join(FILE))
            .expect("the file");
        file.write_all(b"junk\nhttps://b.example\tcam")
            .expect("written");
        drop(file);
        let again = Allowed::load(&dir);
        assert_eq!(again.get("https://b.example"), []);
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE)).expect("a file"),
            "https://a.example\tcamera\tallow\n",
            "compacted to what is allowed"
        );

        let long: String = (0..COMPACT_AT)
            .map(|n| line(&format!("https://{}.example", n % 3), Camera, n % 2 == 0))
            .collect();
        std::fs::write(dir.join(FILE), long).expect("written");
        let loaded = Allowed::load(&dir);
        let file = std::fs::read_to_string(dir.join(FILE)).expect("a file");
        assert!(file.lines().count() <= 3, "{file:?}");
        assert!(!dir.join("permissions.tmp").exists());
        assert_eq!(Allowed::load(&dir).set, loaded.set);

        // And the oldest origin goes past the cap.
        let mut many = Allowed::in_memory();
        for n in 0..=CAP {
            many.set(&format!("https://{n}.example"), &[Clipboard])
                .expect("kept");
        }
        assert_eq!(many.get("https://0.example"), []);
        assert_eq!(many.set.len(), CAP);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_opening_commands_deny_every_name_for_every_origin_then_grant_what_the_file_allows() {
        let mut allowed = Allowed::in_memory();
        allowed
            .set("http://127.0.0.1:8080", &[Camera, Clipboard])
            .expect("kept");
        let commands = read(&opening_commands(&allowed));
        let mut wanted: Vec<(String, String, Option<String>)> = NAMES
            .iter()
            .map(|name| (name.to_string(), "denied".to_string(), None))
            .collect();
        for name in ["camera", "clipboard-read", "clipboard-write"] {
            wanted.push((
                name.to_string(),
                "granted".to_string(),
                Some("http://127.0.0.1:8080".to_string()),
            ));
        }
        assert_eq!(commands, wanted);
        assert_eq!(
            read(&opening_commands(&Allowed::in_memory())).len(),
            NAMES.len()
        );
    }

    #[test]
    fn the_origin_commands_grant_the_words_and_deny_the_rest_by_origin() {
        let origin = "https://meet.example";
        let commands = read(&origin_commands(origin, &[Microphone, Location]));
        let settings: Vec<(&str, &str)> = commands
            .iter()
            .map(|(name, setting, at)| {
                assert_eq!(at.as_deref(), Some(origin), "never browser-wide");
                (name.as_str(), setting.as_str())
            })
            .collect();
        assert_eq!(
            settings,
            [
                ("camera", "denied"),
                ("microphone", "granted"),
                ("geolocation", "granted"),
                ("notifications", "denied"),
                ("clipboard-read", "denied"),
                ("clipboard-write", "denied"),
            ]
        );
        assert!(read(&origin_commands(origin, &[]))
            .iter()
            .all(|(_, setting, _)| setting == "denied"));
    }

    #[test]
    fn a_temporary_profile_s_allowances_are_in_memory_and_write_nothing() {
        let mut allowed = Allowed::in_memory();
        allowed
            .set("https://meet.example", &[Notifications])
            .expect("kept");
        assert!(allowed.path.is_none());
        assert_eq!(allowed.get("https://meet.example"), [Notifications]);
    }

    #[test]
    fn the_sentences_are_the_origin_and_the_words_and_carry_nothing_a_terminal_executes() {
        assert_eq!(
            applied("https://meet.example", &[Camera, Microphone, Location]),
            "allowed https://meet.example: camera, microphone, location"
        );
        assert_eq!(
            applied("https://meet.example", &[]),
            "nothing allowed for https://meet.example"
        );
        let pasted = refused("mic\x1b]52;c;aGk=\x07");
        assert!(!pasted.chars().any(char::is_control), "{pasted:?}");
        assert!(pasted.starts_with("not a permission: mic]52;c;aGk=;"));
        assert_eq!(NO_ORIGIN, "this page has no origin to allow");
    }

    #[test]
    fn the_parameters_are_the_descriptor_the_setting_and_the_origin_when_there_is_one() {
        assert_eq!(
            set_params("camera", "denied", None).to_string(),
            r#"{"permission":{"name":"camera"},"setting":"denied"}"#
        );
        assert_eq!(
            set_params("geolocation", "granted", Some("https://a.example")).to_string(),
            r#"{"permission":{"name":"geolocation"},"setting":"granted","origin":"https://a.example"}"#
        );
    }
}
