//! A page's `<input type=file>`, asked on the row.
//!
//! A file input is the browser's, like a dialog is: the page asks, and the
//! browser shows a picker the page cannot draw or read. Headless has no
//! picker, so a click on one did nothing at all (issue #11) — and the page was
//! told so at once, since the input's `cancel` event fires in the same turn,
//! measured against `chrome-headless-shell` 153. What this module is instead
//! is the picker as a line on the status row: a path, typed, with Tab to
//! complete names the way a shell does, and Enter to hand the file over.
//!
//! The engine hands the question over only when it is asked to. With
//! `Page.setInterceptFileChooserDialog` on — per session, and after
//! `Page.enable` on that same session, because before it the command is
//! accepted and does nothing — a click on an input is a
//! `Page.fileChooserOpened` carrying `{frameId, mode, backendNodeId}` and
//! nothing else: no url, no `accept`, no name. The page's only word in the
//! event is a number, so nothing this module draws is the page's.
//!
//! **It is not a dialog.** A page with an `alert()` up is stopped; a page with
//! a chooser open is not — `Runtime.evaluate` answers, the screencast keeps
//! coming, and a second click on the same input is a second event with the
//! same `backendNodeId`. So a chooser can wait for as long as the person
//! likes while the page carries on underneath, and it does not get a dialog's
//! claim on the mouse. It does keep a dialog's claim on the keys, because the
//! keys are what type the path.
//!
//! **The engine checks nothing, so this does.** `DOM.setFileInputFiles` is
//! the answer, and what it does with a path was measured rather than assumed.
//! A relative one — `hello.txt`, `~/hello.txt` — kills the renderer: the
//! engine writes `Terminating renderer for bad IPC message, reason 2` and the
//! tab is dead. One that does not exist is taken, and the page is handed a
//! 0-byte file of that name. A directory is a 4096-byte "file" to a plain
//! input and everything under it to a `webkitdirectory` one. So every path
//! that goes out goes through [`absolute`], which always makes one absolute,
//! and through [`Dir::check`], which refuses anything that is not a readable
//! regular file — directories included, always.
//!
//! **`[]` is not a cancel.** `setFileInputFiles` with no files is answered
//! `{}` and does nothing on the page: no `change`, no `cancel`, the files it
//! had kept. The protocol has no cancel. So Escape sends nothing, and tells
//! the page what a real chooser would — Chromium fires `cancel` on dismiss
//! since 113 — by dispatching that event on the input itself
//! ([`Upload::CANCEL_FUNCTION`]).
//!
//! Nothing here talks to the engine or draws anything, as with
//! [`crate::dialog`]: it reads the event, decides what a key means, and says
//! what the engine is to be told. The filesystem is behind [`Dir`] so that
//! what completes and what is refused are decided in one place, and the
//! tests run against directories of their own rather than the machine's.

use std::path::{Component, Path, PathBuf};

use crate::input::{Key, KeyAction, KeyInput};
use crate::json::Json;
use crate::line::{Edit, Line};
use crate::text;

/// What the page asked for: read off `Page.fileChooserOpened`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chooser {
    /// The input, as `DOM.setFileInputFiles` wants it back; the one field
    /// that matters.
    pub backend_node_id: i64,
    /// `selectMultiple`: one file per Enter until an Enter with nothing
    /// typed. A `webkitdirectory` input says `selectSingle`, measured, so the
    /// mode does not say directory, and a directory is never sent anyway.
    pub multiple: bool,
    /// The frame the input is in. Kept for the day the row says so, and not
    /// drawn today — the same reasoning as [`crate::dialog::Dialog::url`].
    pub frame_id: String,
}

impl Chooser {
    /// Read a `Page.fileChooserOpened`.
    ///
    /// `None` for an event with no `backendNodeId`. `showOpenFilePicker()`
    /// fires one of those, and the page's promise is rejected with an
    /// `AbortError` before anything could be sent, whatever anyone does —
    /// there is no input to give files to, so there is no question to ask.
    pub fn opening(params: &Json) -> Option<Chooser> {
        let backend_node_id = params.get("backendNodeId").and_then(Json::as_i64)?;
        let multiple = params.get("mode").and_then(Json::as_str) == Some("selectMultiple");
        let frame_id = params
            .get("frameId")
            .and_then(Json::as_str)
            .unwrap_or_default();
        Some(Chooser {
            backend_node_id,
            multiple,
            frame_id: text::sanitize(frame_id).into_owned(),
        })
    }
}

/// The prompt while it is open: the chooser, the line, the files taken so
/// far, and the one-key message in the prompt's place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub chooser: Chooser,
    pub line: Line,
    /// Absolute paths accepted with Enter and checked, in the order they
    /// were: one for a plain input once it is sent, as many as were entered
    /// for a `multiple` one.
    pub taken: Vec<PathBuf>,
    /// Where a relative name is looked for, and where the line started.
    /// Absolute.
    pub base: PathBuf,
    /// `$HOME`, if there is one, for `~` both ways.
    pub home: Option<PathBuf>,
    /// Said in place of the prompt for exactly one key: the names Tab found,
    /// or why Enter was refused.
    pub message: Option<String>,
    /// What the line was set to when it was last put back, so that an Enter
    /// on it untouched is an Enter with nothing typed.
    offered: String,
}

/// What a key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Still typing: a move, a letter, a completion, a refusal said.
    Waiting,
    /// Enter on a good path, or an Enter with nothing typed once a `multiple`
    /// input has files: send [`Upload::reply`] and close the prompt.
    Send,
    /// Escape: close, send nothing, tell the page `cancel`.
    Cancel,
    /// `ctrl+q`, which quits from here as from everywhere.
    Quit,
}

impl Upload {
    /// The `Runtime.callFunctionOn` `functionDeclaration` that fires `cancel`
    /// on the input, for after [`Outcome::Cancel`]. The loop gets the
    /// `objectId` from `DOM.resolveNode {backendNodeId}` first; measured, the
    /// page's listener runs, and nothing of the person's is in it.
    pub const CANCEL_FUNCTION: &'static str =
        "function(){this.dispatchEvent(new Event('cancel',{bubbles:true}))}";

    /// A prompt for `chooser` starting in `base`: the line holds the
    /// directory, `~`-shortened, with a `/` after it and the cursor at the
    /// end, not selected. It says where a relative name will be looked for,
    /// it is the first thing Tab completes from, and `ctrl+u` empties it.
    pub fn new(chooser: Chooser, base: PathBuf, home: Option<PathBuf>) -> Upload {
        let mut upload = Upload {
            chooser,
            line: Line::empty(),
            taken: Vec::new(),
            base,
            home,
            message: None,
            offered: String::new(),
        };
        let start = upload.base.clone();
        upload.restart_in(&start);
        upload
    }

    /// Put the line back to `dir`, as [`Upload::new`] starts it.
    fn restart_in(&mut self, dir: &Path) {
        let mut shown = tilde(dir, self.home.as_deref());
        if !shown.ends_with('/') {
            shown.push('/');
        }
        self.line = Line::empty();
        self.line.set_text(shown);
        self.offered = self.line.text().to_string();
    }

    /// One key. `fs` is the directory reader: [`Disk`] in the loop.
    ///
    /// A key let go is nothing, and neither is a modifier on its own — it
    /// clears nothing anywhere else on the row either. Every other key clears
    /// the message first, whatever it then does.
    ///
    /// A plain Tab is looked at before the line sees it. If the line is
    /// showing a suggestion, the line takes it, as the url bar's does;
    /// otherwise it is completion ([`complete`]). So the line only ever sees
    /// Tab when there is a suggestion to take, and the suggestion is always
    /// one completion made. After anything goes in — a letter, a
    /// suggestion, a completion — the one name that matches is offered dim
    /// ([`hint`]). Up and Down do nothing: no history of uploads is kept,
    /// because the paths a person uploaded are a record, and a record is a
    /// thing `SECURITY.md` would have to list.
    pub fn step(&mut self, key: &KeyInput, fs: &dyn Dir) -> Outcome {
        if key.action == KeyAction::Release || matches!(key.key, Key::Other(_)) {
            return Outcome::Waiting;
        }
        self.message = None;
        let plain = !key.mods.ctrl() && !key.mods.alt() && !key.mods.shift();
        if key.key == Key::Tab && plain && self.line.hint().is_empty() {
            self.complete(fs);
            return Outcome::Waiting;
        }
        match self.line.step(key) {
            Edit::Inserted => {
                self.offer(fs);
                Outcome::Waiting
            }
            Edit::Typing | Edit::Previous | Edit::Next => Outcome::Waiting,
            Edit::Go => self.enter(fs),
            Edit::Cancel => Outcome::Cancel,
            Edit::Quit => Outcome::Quit,
        }
    }

    /// A paste, into the line at the cursor, and then the suggestion asked
    /// for again. The caller makes it one line first, as every paste into a
    /// line is ([`crate::clipboard::one_line`]); a pasted path is the common
    /// way a long one arrives. True when anything went in.
    pub fn paste(&mut self, text: &str, fs: &dyn Dir) -> bool {
        self.message = None;
        if !self.line.insert_str(text) {
            return false;
        }
        self.offer(fs);
        true
    }

    /// The prompt, as a caption for [`crate::screen::dialog_prompt`], which
    /// clips it to half the pane so that the path stays in sight and puts the
    /// space after it: `upload:`, `upload (2 added, enter to send):`, or the
    /// message and a `>` for the key after a Tab that listed names or an
    /// Enter that was refused.
    ///
    /// The `>` is ASCII for the reason the strip's `!` is: the arrows a font
    /// draws better are ambiguous-width in East Asian terminals, and a prompt
    /// one cell out is a cursor one cell out.
    pub fn prompt(&self) -> String {
        if let Some(message) = &self.message {
            return format!("{message} >");
        }
        match self.taken.len() {
            0 => "upload:".to_string(),
            n => format!("upload ({n} added, enter to send):"),
        }
    }

    /// The `DOM.setFileInputFiles` params: the input and the files taken,
    /// absolute, in the order they were entered. Only meaningful after
    /// [`Outcome::Send`].
    pub fn reply(&self) -> Json {
        let files = self
            .taken
            .iter()
            .map(|path| Json::string(path.to_string_lossy()))
            .collect();
        Json::object(vec![
            (
                "backendNodeId",
                Json::number(self.chooser.backend_node_id as f64),
            ),
            ("files", Json::Array(files)),
        ])
    }

    /// The sentence for the tab's note once it is sent: `uploading
    /// report.pdf`, or `uploading 3 files`. The name is the one typed, which
    /// is plain text because the line is.
    pub fn sentence(&self) -> String {
        match self.taken.as_slice() {
            [one] => {
                let name = one.file_name().unwrap_or_default().to_string_lossy();
                format!("uploading {name}")
            }
            many => format!("uploading {} files", many.len()),
        }
    }

    /// The directory the first file sent was in, for the next prompt to
    /// start in. See [`start_dir`].
    pub fn last_dir(&self) -> Option<PathBuf> {
        self.taken
            .first()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf)
    }

    /// What is typed before the cursor, split where completion splits it,
    /// and the entries of the directory it names.
    fn listing(&self, fs: &dyn Dir) -> (String, Vec<Entry>) {
        let typed = &self.line.text()[..self.line.cursor()];
        let (dir, prefix) = split(typed);
        let entries = fs.entries(&absolute(dir, &self.base, self.home.as_deref()));
        (prefix.to_string(), entries)
    }

    /// Tab, with no suggestion showing: see [`complete`].
    fn complete(&mut self, fs: &dyn Dir) {
        let (prefix, entries) = self.listing(fs);
        match complete(&prefix, &entries) {
            Completion::None => {}
            Completion::Extend(more) => {
                self.line.insert_str(&more);
                self.offer(fs);
            }
            Completion::Choices(names) => self.message = Some(choices_sentence(&names)),
        }
    }

    /// Offer the one name that matches, dim, if the cursor is at the end
    /// where a suggestion can be; otherwise nothing, without listing
    /// anything.
    fn offer(&mut self, fs: &dyn Dir) {
        if self.line.cursor() != self.line.text().len() {
            self.line.suggest(None);
            return;
        }
        let (prefix, entries) = self.listing(fs);
        self.line.suggest(hint(&prefix, &entries));
    }

    /// Enter: the line, checked, as a file to send.
    fn enter(&mut self, fs: &dyn Dir) -> Outcome {
        let typed = self.line.text();
        if typed.trim().is_empty() || typed == self.offered {
            if self.taken.is_empty() {
                self.message = Some("type a path".to_string());
                return Outcome::Waiting;
            }
            return Outcome::Send;
        }
        let path = absolute(typed, &self.base, self.home.as_deref());
        match fs.check(&path) {
            Ok(Kind::File) => {
                let parent = path.parent().map(Path::to_path_buf);
                self.taken.push(path);
                if !self.chooser.multiple {
                    return Outcome::Send;
                }
                if let Some(parent) = parent {
                    self.restart_in(&parent);
                }
                Outcome::Waiting
            }
            // A directory is refused whatever the input is: see the module.
            Ok(Kind::Directory) => {
                self.message = Some(Refusal::IsDirectory.sentence());
                Outcome::Waiting
            }
            Err(refusal) => {
                self.message = Some(refusal.sentence());
                Outcome::Waiting
            }
        }
    }
}

/// A directory, as completion and Enter need it. One trait, two methods, so
/// that what the loop reads and what a test reads are the same questions.
pub trait Dir {
    /// The entries of `dir`, or none if it cannot be read: a directory that
    /// is not there completes to nothing rather than to an error.
    fn entries(&self, dir: &Path) -> Vec<Entry>;
    /// Whether `path` is something Enter may send: a file or a directory,
    /// or why not.
    fn check(&self, path: &Path) -> Result<Kind, Refusal>;
}

/// One name in a directory, and whether it is a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
}

/// What a path that can be read is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}

/// Why a path cannot be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Missing,
    IsDirectory,
    /// A pipe, a socket, a device: not a file a page can be given, and a
    /// FIFO is one an `open` to check it would wait on for ever.
    NotAFile,
    /// What the system said, in its own words.
    Unreadable(String),
}

impl Refusal {
    /// The words said on the row in the prompt's place.
    pub fn sentence(&self) -> String {
        match self {
            Refusal::Missing => "no such file".to_string(),
            Refusal::IsDirectory => "that's a directory".to_string(),
            Refusal::NotAFile => "that's not a regular file".to_string(),
            Refusal::Unreadable(why) => format!("can't read it: {why}"),
        }
    }
}

/// The real filesystem.
pub struct Disk;

impl Dir for Disk {
    /// `read_dir` and each entry's own file type, which on Linux comes with
    /// the listing (`d_type`) rather than a `stat` per name — 0.4 ms for the
    /// 1065 names of `/usr/bin`, measured, against 10 ms with a `stat` each,
    /// and this is asked for on every letter typed. A symlink is the one
    /// entry that costs a `metadata`, so that a link to a directory completes
    /// as one. A name that is not UTF-8 is skipped: it cannot be typed on the
    /// row, so it cannot be sent either.
    fn entries(&self, dir: &Path) -> Vec<Entry> {
        let Ok(listing) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        listing
            .flatten()
            .filter_map(|entry| {
                let kind = entry.file_type().ok()?;
                let dir = if kind.is_symlink() {
                    std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir())
                } else {
                    kind.is_dir()
                };
                let name = entry.file_name().into_string().ok()?;
                Some(Entry { name, dir })
            })
            .collect()
    }

    /// `metadata`, following a link, then an `open` for reading, because read
    /// access is what the engine will need and a file it cannot read would
    /// reach the page as nothing. The `open` is only tried on a regular file:
    /// on a FIFO it would wait for a writer, with the loop inside it.
    fn check(&self, path: &Path) -> Result<Kind, Refusal> {
        let meta = std::fs::metadata(path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Refusal::Missing,
            kind => Refusal::Unreadable(kind.to_string()),
        })?;
        if meta.is_dir() {
            return Ok(Kind::Directory);
        }
        if !meta.is_file() {
            return Err(Refusal::NotAFile);
        }
        std::fs::File::open(path).map_err(|error| Refusal::Unreadable(error.kind().to_string()))?;
        Ok(Kind::File)
    }
}

/// Where a prompt starts: the directory the last file was sent from in this
/// run, else the working directory `blinkterm` was started in, else `$HOME`,
/// else `/` — whichever is the first to be absolute.
///
/// Not `~/Downloads`, and not home first. Somebody who ran `blinkterm` from
/// `~/work/report/` to attach the PDF in it is the common case in a terminal,
/// and the working directory is the one directory the person has already
/// chosen. The last one used is the program's, not a tab's — a second tab's
/// upload is the same person's afternoon — and it is not kept between runs or
/// written anywhere. The working directory is read when the prompt opens, so
/// one that has since been removed falls through to `$HOME`.
pub fn start_dir(last: Option<&Path>, cwd: Option<PathBuf>, home: Option<&Path>) -> PathBuf {
    last.map(Path::to_path_buf)
        .into_iter()
        .chain(cwd)
        .chain(home.map(Path::to_path_buf))
        .find(|dir| dir.is_absolute())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `$HOME`, if it is set and absolute; `~` means nothing otherwise.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

/// `path` as the row shows it: under `home`, as `~` and the rest.
///
/// A home of `/` is no home to shorten to — every path would be `~` — and is
/// left alone.
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home.filter(|home| home.parent().is_some()) {
        if let Ok(rest) = path.strip_prefix(home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// `~` and `~/x` against `home`; anything else as typed.
///
/// `~user` is not expanded — that is a password-database lookup, in a
/// program with no dependencies — and says so by staying as typed, a name
/// under the start directory that [`Dir::check`] then refuses as missing.
pub fn expand(typed: &str, home: Option<&Path>) -> PathBuf {
    match home {
        Some(home) if typed == "~" => home.to_path_buf(),
        Some(home) if typed.starts_with("~/") => home.join(&typed[2..]),
        _ => PathBuf::from(typed),
    }
}

/// What is typed, as the path the engine is sent: [`expand`], then joined
/// onto `base` if it is relative, then with `.` and `..` folded.
///
/// Always absolute, whatever it is given: this is the one place a path
/// becomes what the engine is sent, and a relative one kills the renderer.
/// Folded by its spelling and not by the filesystem — no symlink is
/// resolved, no `canonicalize` — because the person typed a name and that
/// name is what the page's `File.name` should carry. A `..` at the root stays
/// at the root, as it does in the kernel.
pub fn absolute(typed: &str, base: &Path, home: Option<&Path>) -> PathBuf {
    let expanded = expand(typed, home);
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    let mut out = PathBuf::from("/");
    for part in joined.components() {
        match part {
            Component::Normal(name) => out.push(name),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// What is typed, split after its last `/`: the directory as typed, with the
/// `/` (empty when there is none, which is the start directory), and the
/// start of the name after it.
pub fn split(typed: &str) -> (&str, &str) {
    match typed.rfind('/') {
        Some(at) => (&typed[..=at], &typed[at + 1..]),
        None => ("", typed),
    }
}

/// What Tab does to what is typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    /// Nothing matched, or the one match is already typed: the row does not
    /// change, and no message says so — a Tab that did nothing says so by
    /// doing nothing.
    None,
    /// Put this after what was typed: the rest of the one name that matches,
    /// with a `/` if it is a directory, or what several of them share.
    Extend(String),
    /// Nothing more is shared: these are the matches, sorted, each with a
    /// `/` if it is a directory, for the row to say.
    Choices(Vec<String>),
}

/// The names in `entries` that `prefix` could be the start of.
///
/// Byte for byte and case and all, because this is a filesystem and not a
/// search box. A name starting with `.` only when a `.` is typed, as a shell
/// hides them. And only names that are plain text ([`text::is_plain`]): a
/// filename can hold a newline or an escape, and one that the row would show
/// as something other than itself is left out rather than offered as a name
/// that is not on the disk. Sorted by name, because `read_dir` gives inode
/// order and that would shuffle between machines.
fn candidates<'a>(prefix: &str, entries: &'a [Entry]) -> Vec<&'a Entry> {
    let mut found: Vec<&Entry> = entries
        .iter()
        .filter(|entry| entry.name.starts_with(prefix))
        .filter(|entry| prefix.starts_with('.') || !entry.name.starts_with('.'))
        .filter(|entry| entry.name.chars().all(text::is_plain))
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// The rest of `entry`'s name after `prefix`, with a `/` for a directory.
fn rest(prefix: &str, entry: &Entry) -> String {
    let mut rest = entry.name[prefix.len()..].to_string();
    if entry.dir {
        rest.push('/');
    }
    rest
}

/// What Tab does to `prefix`, given the entries of its directory.
///
/// No match is nothing. One is the rest of it, with a `/` for a directory, so
/// that the next Tab lists inside it. Several are what they share beyond what
/// was typed, when they share anything, and the list of them when they do
/// not — which is a shell's first and second Tab, as one key each.
pub fn complete(prefix: &str, entries: &[Entry]) -> Completion {
    let found = candidates(prefix, entries);
    match found.as_slice() {
        [] => Completion::None,
        [one] => match rest(prefix, one) {
            more if more.is_empty() => Completion::None,
            more => Completion::Extend(more),
        },
        several => {
            let first = &several[0].name;
            let mut end = first.len();
            for other in &several[1..] {
                let same = first
                    .bytes()
                    .zip(other.name.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                end = end.min(same);
            }
            while !first.is_char_boundary(end) {
                end -= 1;
            }
            if end > prefix.len() {
                return Completion::Extend(first[prefix.len()..end].to_string());
            }
            Completion::Choices(
                several
                    .iter()
                    .map(|entry| format!("{}{}", entry.name, if entry.dir { "/" } else { "" }))
                    .collect(),
            )
        }
    }
}

/// The suggestion after something went in: the rest of the one name that
/// matches, and nothing when several do — a ghost of one of them would be a
/// guess.
pub fn hint(prefix: &str, entries: &[Entry]) -> Option<String> {
    match candidates(prefix, entries).as_slice() {
        [one] => Some(rest(prefix, one)).filter(|more| !more.is_empty()),
        _ => None,
    }
}

/// The row's sentence for [`Completion::Choices`]: `3 matches: a b c`, clipped
/// by the caller. A name with a space in it is quoted, so that the list still
/// reads as a list.
pub fn choices_sentence(choices: &[String]) -> String {
    let names: Vec<String> = choices
        .iter()
        .map(|name| {
            if name.contains(' ') {
                format!("\"{name}\"")
            } else {
                name.clone()
            }
        })
        .collect();
    format!("{} matches: {}", choices.len(), names.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Mods;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A directory of the test's own, gone when the test is.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(files: &[&str], dirs: &[&str]) -> Scratch {
            static COUNT: AtomicUsize = AtomicUsize::new(0);
            let n = COUNT.fetch_add(1, Ordering::SeqCst);
            let path =
                std::env::temp_dir().join(format!("blinkterm-upload-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("a scratch directory");
            for dir in dirs {
                std::fs::create_dir_all(path.join(dir)).expect("a directory");
            }
            for file in files {
                std::fs::write(path.join(file), b"twelve bytes").expect("a file");
            }
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn chooser(multiple: bool) -> Chooser {
        Chooser {
            backend_node_id: 3,
            multiple,
            frame_id: "F".to_string(),
        }
    }

    fn upload_in(dir: &Path, multiple: bool) -> Upload {
        Upload::new(chooser(multiple), dir.to_path_buf(), None)
    }

    fn typed(c: char) -> KeyInput {
        KeyInput {
            key: Key::Char(c),
            mods: Mods::default(),
            action: KeyAction::Press,
            text: Some(c),
        }
    }

    fn key(k: Key) -> KeyInput {
        KeyInput::press(k)
    }

    fn type_in(upload: &mut Upload, text: &str) {
        for c in text.chars() {
            assert_eq!(upload.step(&typed(c), &Disk), Outcome::Waiting);
        }
    }

    fn entries(names: &[(&str, bool)]) -> Vec<Entry> {
        names
            .iter()
            .map(|&(name, dir)| Entry {
                name: name.to_string(),
                dir,
            })
            .collect()
    }

    #[test]
    fn the_opening_event_says_which_node_and_whether_several() {
        let read = |params: &str| Chooser::opening(&Json::parse(params).expect("JSON"));
        // As `chrome-headless-shell` 153 sends it, and nothing else.
        assert_eq!(
            read(r#"{"frameId":"F1","mode":"selectSingle","backendNodeId":3}"#),
            Some(Chooser {
                backend_node_id: 3,
                multiple: false,
                frame_id: "F1".to_string(),
            })
        );
        let several = read(r#"{"frameId":"F1","mode":"selectMultiple","backendNodeId":9}"#);
        assert!(several.expect("a chooser").multiple);
        // `showOpenFilePicker()` has no input to give files to.
        assert_eq!(read(r#"{"frameId":"F1","mode":"selectSingle"}"#), None);
        assert_eq!(Chooser::opening(&Json::empty()), None);
    }

    #[test]
    fn the_line_starts_at_the_base_directory_with_a_tilde_and_a_slash() {
        let home = Path::new("/home/someone");
        let start = |base: &str, home: Option<&Path>| {
            Upload::new(
                chooser(false),
                PathBuf::from(base),
                home.map(Path::to_path_buf),
            )
        };
        let upload = start("/home/someone/Documents", Some(home));
        assert_eq!(upload.line.text(), "~/Documents/");
        assert_eq!(upload.line.cursor(), upload.line.text().len());
        assert!(!upload.line.whole(), "the first key adds to it");
        assert_eq!(start("/srv", Some(home)).line.text(), "/srv/");
        assert_eq!(start("/home/someone", Some(home)).line.text(), "~/");
        assert_eq!(
            start("/home/someoneelse", Some(home)).line.text(),
            "/home/someoneelse/"
        );
        assert_eq!(
            start("/home/someone/x", None).line.text(),
            "/home/someone/x/"
        );
        assert_eq!(start("/", Some(home)).line.text(), "/");
        assert_eq!(start("/etc", Some(Path::new("/"))).line.text(), "/etc/");
        assert_eq!(upload.prompt(), "upload:");
    }

    #[test]
    fn the_start_directory_is_the_last_used_then_the_working_one_then_home() {
        let last = Path::new("/tmp/last");
        let home = Path::new("/home/someone");
        let cwd = || Some(PathBuf::from("/work/report"));
        assert_eq!(start_dir(Some(last), cwd(), Some(home)), last);
        assert_eq!(
            start_dir(None, cwd(), Some(home)),
            Path::new("/work/report")
        );
        assert_eq!(start_dir(None, None, Some(home)), home);
        assert_eq!(start_dir(None, None, None), Path::new("/"));
        // Only an absolute one is a place to start.
        assert_eq!(
            start_dir(
                None,
                Some(PathBuf::from("relative")),
                Some(Path::new("rel"))
            ),
            Path::new("/")
        );
    }

    #[test]
    fn tab_completes_the_one_name_that_matches() {
        let scratch = Scratch::new(&["report.pdf", "notes.txt"], &["music"]);
        let mut upload = upload_in(&scratch.0, false);
        type_in(&mut upload, "rep");
        // The letters already offered it; Tab takes that.
        assert_eq!(upload.line.hint(), "ort.pdf");
        upload.step(&key(Key::Tab), &Disk);
        assert!(upload.line.text().ends_with("/report.pdf"));

        // A directory completes with its slash, and the next Tab lists
        // inside it.
        let mut upload = upload_in(&scratch.0, false);
        type_in(&mut upload, "mu");
        upload.step(&key(Key::Right), &Disk);
        assert!(
            upload.line.text().ends_with("/music/"),
            "{}",
            upload.line.text()
        );

        // And with no suggestion showing, Tab completes by itself.
        assert_eq!(
            complete(
                "rep",
                &entries(&[("report.pdf", false), ("notes.txt", false)])
            ),
            Completion::Extend("ort.pdf".to_string())
        );
        assert_eq!(
            complete("rep", &entries(&[("reports", true)])),
            Completion::Extend("orts/".to_string())
        );
        assert_eq!(
            complete("zz", &entries(&[("reports", true)])),
            Completion::None
        );
        assert_eq!(
            complete("report.pdf", &entries(&[("report.pdf", false)])),
            Completion::None,
            "already typed"
        );
    }

    #[test]
    fn tab_extends_to_what_several_matches_share_and_then_lists_them() {
        let scratch = Scratch::new(&["report.pdf", "repo.tar"], &["reports"]);
        let mut upload = upload_in(&scratch.0, false);
        type_in(&mut upload, "re");
        assert_eq!(upload.line.hint(), "", "several match, so none is offered");
        upload.step(&key(Key::Tab), &Disk);
        // All three start `repo`: `repo.tar`, `report.pdf`, `reports`.
        assert!(
            upload.line.text().ends_with("/repo"),
            "{}",
            upload.line.text()
        );
        assert_eq!(upload.message, None);

        upload.step(&key(Key::Tab), &Disk);
        assert!(upload.line.text().ends_with("/repo"), "the line is kept");
        assert_eq!(
            upload.message.as_deref(),
            Some("3 matches: repo.tar report.pdf reports/")
        );
        assert_eq!(upload.prompt(), "3 matches: repo.tar report.pdf reports/ >");
        assert_eq!(
            complete(
                "repo",
                &entries(&[
                    ("report.pdf", false),
                    ("reports", true),
                    ("repo.tar", false)
                ])
            ),
            Completion::Choices(vec![
                "repo.tar".to_string(),
                "report.pdf".to_string(),
                "reports/".to_string()
            ])
        );
    }

    #[test]
    fn hidden_names_are_offered_only_when_a_dot_is_typed() {
        let listing = entries(&[(".git", true), ("src", true)]);
        assert_eq!(
            complete("", &listing),
            Completion::Extend("src/".to_string())
        );
        assert_eq!(
            complete(".", &listing),
            Completion::Extend("git/".to_string())
        );
        assert_eq!(hint("", &listing), Some("src/".to_string()));
    }

    #[test]
    fn a_tilde_is_home_and_a_relative_name_is_under_the_base() {
        let home = Some(Path::new("/home/someone"));
        let base = Path::new("/work/report");
        for (typed, wanted) in [
            ("~", "/home/someone"),
            ("~/a.txt", "/home/someone/a.txt"),
            ("~/", "/home/someone"),
            ("a.txt", "/work/report/a.txt"),
            ("./a.txt", "/work/report/a.txt"),
            ("../a.txt", "/work/a.txt"),
            ("sub/../a.txt", "/work/report/a.txt"),
            ("/etc/../etc/./hosts", "/etc/hosts"),
            ("/../../x", "/x"),
            ("", "/work/report"),
            // `~user` is a name like any other: under the base, and missing.
            ("~root/x", "/work/report/~root/x"),
            ("my notes.md", "/work/report/my notes.md"),
        ] {
            assert_eq!(absolute(typed, base, home), Path::new(wanted), "{typed:?}");
        }
        // With no home, a tilde is a name too.
        assert_eq!(absolute("~/x", base, None), Path::new("/work/report/~/x"));
        // And the result is absolute whatever it is given, a relative base
        // included: a relative path is a renderer killed.
        for typed in ["a", "~/a", "..", "../../..", ""] {
            assert!(
                absolute(typed, Path::new("rel"), None).is_absolute(),
                "{typed:?}"
            );
        }
        assert_eq!(expand("~", home), Path::new("/home/someone"));
        assert_eq!(expand("~x", home), Path::new("~x"));
        assert_eq!(split("~/Documents/rep"), ("~/Documents/", "rep"));
        assert_eq!(split("/rep"), ("/", "rep"));
        assert_eq!(split("rep"), ("", "rep"));
        assert_eq!(split("dir/"), ("dir/", ""));
    }

    #[test]
    fn a_letter_offers_the_unique_match_dim_and_a_backspace_takes_it_back() {
        let scratch = Scratch::new(&["report.pdf", "readme.md"], &[]);
        let mut upload = upload_in(&scratch.0, false);
        type_in(&mut upload, "r");
        assert_eq!(upload.line.hint(), "", "two names start with r");
        type_in(&mut upload, "ep");
        assert_eq!(upload.line.hint(), "ort.pdf");
        upload.step(&key(Key::Backspace), &Disk);
        assert_eq!(upload.line.hint(), "", "a deletion does not bring it back");
        type_in(&mut upload, "p");
        assert_eq!(upload.line.hint(), "ort.pdf", "a letter does");
        // Not with the cursor anywhere but the end.
        upload.step(&key(Key::Left), &Disk);
        assert_eq!(upload.line.hint(), "");
    }

    #[test]
    fn enter_sends_one_file_that_exists_and_says_why_not_otherwise() {
        let scratch = Scratch::new(&["report.pdf"], &["reports"]);
        let mut upload = upload_in(&scratch.0, false);
        type_in(&mut upload, "report.pdf");
        assert_eq!(upload.step(&key(Key::Enter), &Disk), Outcome::Send);
        let path = scratch.0.join("report.pdf");
        assert_eq!(
            upload.reply().to_string(),
            format!(r#"{{"backendNodeId":3,"files":["{}"]}}"#, path.display())
        );
        assert!(path.is_absolute());
        assert_eq!(upload.last_dir(), Some(scratch.0.clone()));

        for (name, why) in [
            ("reprot.pdf", "no such file"),
            ("reports", "that's a directory"),
            ("reports/", "that's a directory"),
        ] {
            let mut upload = upload_in(&scratch.0, false);
            type_in(&mut upload, name);
            let before = upload.line.text().to_string();
            assert_eq!(
                upload.step(&key(Key::Enter), &Disk),
                Outcome::Waiting,
                "{name}"
            );
            assert_eq!(upload.message.as_deref(), Some(why), "{name}");
            assert_eq!(upload.line.text(), before, "the line is kept");
            assert!(upload.taken.is_empty());
        }
        assert_eq!(
            Disk.check(Path::new("/dev/null")),
            Err(Refusal::NotAFile),
            "a device is not a file"
        );
    }

    #[test]
    fn a_multiple_input_takes_one_path_per_enter_and_an_empty_enter_sends() {
        let scratch = Scratch::new(&["one.txt", "two.txt"], &[]);
        let mut upload = upload_in(&scratch.0, true);
        // Nothing typed and nothing taken is not an answer.
        assert_eq!(upload.step(&key(Key::Enter), &Disk), Outcome::Waiting);
        assert_eq!(upload.message.as_deref(), Some("type a path"));

        type_in(&mut upload, "one.txt");
        assert_eq!(upload.step(&key(Key::Enter), &Disk), Outcome::Waiting);
        assert_eq!(upload.prompt(), "upload (1 added, enter to send):");
        assert_eq!(
            upload.line.text(),
            format!("{}/", scratch.0.display()),
            "back to the directory it came from"
        );
        type_in(&mut upload, "two.txt");
        upload.step(&key(Key::Enter), &Disk);
        assert_eq!(upload.prompt(), "upload (2 added, enter to send):");
        assert_eq!(upload.step(&key(Key::Enter), &Disk), Outcome::Send);
        assert_eq!(
            upload.taken,
            [scratch.0.join("one.txt"), scratch.0.join("two.txt")]
        );
        assert_eq!(upload.sentence(), "uploading 2 files");

        // An emptied line is nothing typed too.
        let mut upload = upload_in(&scratch.0, true);
        type_in(&mut upload, "one.txt");
        upload.step(&key(Key::Enter), &Disk);
        upload.step(
            &KeyInput {
                mods: Mods(Mods::CTRL),
                ..key(Key::Char('u'))
            },
            &Disk,
        );
        assert_eq!(upload.line.text(), "");
        assert_eq!(upload.step(&key(Key::Enter), &Disk), Outcome::Send);
    }

    #[test]
    fn escape_sends_nothing_however_much_was_taken() {
        let scratch = Scratch::new(&["one.txt"], &[]);
        let mut upload = upload_in(&scratch.0, true);
        type_in(&mut upload, "one.txt");
        upload.step(&key(Key::Enter), &Disk);
        assert_eq!(upload.step(&key(Key::Escape), &Disk), Outcome::Cancel);
        assert_eq!(
            Upload::CANCEL_FUNCTION,
            "function(){this.dispatchEvent(new Event('cancel',{bubbles:true}))}"
        );
    }

    #[test]
    fn a_name_with_a_space_is_completed_and_sent_as_it_is() {
        let scratch = Scratch::new(&["my notes.md", "other.txt"], &[]);
        let mut upload = upload_in(&scratch.0, false);
        type_in(&mut upload, "my");
        upload.step(&key(Key::Tab), &Disk);
        assert!(upload.line.text().ends_with("/my notes.md"));
        assert_eq!(upload.step(&key(Key::Enter), &Disk), Outcome::Send);
        assert_eq!(upload.taken, [scratch.0.join("my notes.md")]);
        assert_eq!(upload.sentence(), "uploading my notes.md");
        assert_eq!(
            choices_sentence(&["my notes.md".to_string(), "my.txt".to_string()]),
            "2 matches: \"my notes.md\" my.txt"
        );
    }

    #[test]
    fn a_name_that_is_not_plain_text_is_not_offered() {
        let listing = entries(&[("evil\x1b]0;x\x07", false), ("even.txt", false)]);
        assert_eq!(
            complete("ev", &listing),
            Completion::Extend("en.txt".to_string())
        );
        assert_eq!(hint("ev", &listing), Some("en.txt".to_string()));
        let listing = entries(&[("a\u{202e}b", false), ("a\nc", false), ("ab", false)]);
        assert_eq!(complete("a", &listing), Completion::Extend("b".to_string()));
        // And one on the disk is left out the same way.
        let scratch = Scratch::new(&["evil\x1b[2J", "ev.txt"], &[]);
        let mut upload = upload_in(&scratch.0, false);
        type_in(&mut upload, "ev");
        assert_eq!(upload.line.hint(), ".txt");
    }

    #[test]
    fn a_message_lasts_exactly_one_key() {
        let scratch = Scratch::new(&["ab", "ac"], &[]);
        let mut upload = upload_in(&scratch.0, false);
        type_in(&mut upload, "a");
        upload.step(&key(Key::Tab), &Disk);
        assert!(upload.message.is_some());
        // A key let go, or a modifier on its own, is not a key pressed.
        let mut released = typed('x');
        released.action = KeyAction::Release;
        upload.step(&released, &Disk);
        upload.step(&key(Key::Other(57441)), &Disk);
        assert!(upload.message.is_some());
        upload.step(&key(Key::Left), &Disk);
        assert_eq!(upload.message, None);
        assert_eq!(upload.prompt(), "upload:");
    }

    #[test]
    fn ctrl_q_quits_and_up_and_down_do_nothing() {
        let scratch = Scratch::new(&[], &[]);
        let mut upload = upload_in(&scratch.0, false);
        let ctrl = |c| KeyInput {
            mods: Mods(Mods::CTRL),
            ..key(Key::Char(c))
        };
        let before = upload.line.clone();
        for k in [key(Key::Up), key(Key::Down), ctrl('p'), ctrl('n')] {
            assert_eq!(upload.step(&k, &Disk), Outcome::Waiting, "{k:?}");
            assert_eq!(upload.line, before, "{k:?}");
        }
        assert_eq!(upload.step(&ctrl('q'), &Disk), Outcome::Quit);
    }

    #[test]
    fn a_paste_goes_in_at_the_cursor_and_offers_what_it_completes() {
        let scratch = Scratch::new(&["report.pdf"], &[]);
        let mut upload = upload_in(&scratch.0, false);
        assert!(upload.paste("rep", &Disk));
        assert_eq!(upload.line.hint(), "ort.pdf");
        assert!(!upload.paste("\u{200b}", &Disk), "nothing plain in it");
        let mut upload = upload_in(&scratch.0, false);
        upload.step(
            &KeyInput {
                mods: Mods(Mods::CTRL),
                ..key(Key::Char('u'))
            },
            &Disk,
        );
        let path = scratch.0.join("report.pdf");
        assert!(upload.paste(&path.to_string_lossy(), &Disk));
        assert_eq!(upload.step(&key(Key::Enter), &Disk), Outcome::Send);
        assert_eq!(upload.taken, [path]);
    }

    #[test]
    fn sentence_says_one_name_or_a_count() {
        let mut upload = Upload::new(chooser(true), PathBuf::from("/"), None);
        upload.taken = vec![PathBuf::from("/home/someone/Documents/report.pdf")];
        assert_eq!(upload.sentence(), "uploading report.pdf");
        upload.taken = vec![
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/c"),
        ];
        assert_eq!(upload.sentence(), "uploading 3 files");
        assert_eq!(upload.last_dir(), Some(PathBuf::from("/")));
    }

    #[test]
    fn refusals_say_why_in_a_few_words() {
        assert_eq!(Refusal::Missing.sentence(), "no such file");
        assert_eq!(Refusal::IsDirectory.sentence(), "that's a directory");
        assert_eq!(
            Refusal::Unreadable(std::io::ErrorKind::PermissionDenied.to_string()).sentence(),
            "can't read it: permission denied"
        );
        assert_eq!(
            tilde(Path::new("/home/a/b"), Some(Path::new("/home/a"))),
            "~/b"
        );
        assert_eq!(
            tilde(Path::new("/home/ab"), Some(Path::new("/home/a"))),
            "/home/ab"
        );
    }
}
