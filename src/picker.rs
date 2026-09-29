//! A file for a page's `<input type=file>`, chosen by a program the settings
//! name.
//!
//! [`crate::upload`] answers a file input with a path typed on the row. That
//! works, and it is also easy to miss that anything happened at all, and it
//! shows nothing of the files being chosen from — for an image upload the
//! person cannot see which image they are picking (#57, #58). What people
//! want instead differs: a window of the desktop's own (Finder, the GTK or
//! KDE dialog), or something that stays in the terminal (yazi, fzf, Kitty's
//! `choose-files`). So this program does not pick one. The settings name a
//! program, and with none named the row prompt is what it always was.
//!
//! # Two settings, because the two need opposite handling
//!
//! `file-picker` is a program that opens a window of its own and leaves the
//! terminal alone: this program keeps drawing and reading keys while it is
//! open, and reads its answer when it exits. `file-picker-terminal` is one
//! that needs this terminal: this program lets go of it — raw mode, mouse
//! reporting, keyboard flags, bracketed paste, the alternate screen — runs
//! the picker on it, waits, and takes it back. Nothing can tell the two
//! kinds apart from outside, and treating one as the other either leaves a
//! TUI fighting this program for the keys or stops a browser for as long as a
//! dialog in another window is open, so the person says which.
//!
//! With both set, the window is used where there is a display to open it on
//! ([`has_display`]) and the terminal elsewhere, which is what makes one
//! settings file right both at the desk and over ssh. `file-picker-multiple`
//! and `file-picker-terminal-multiple` are used instead for an input that
//! takes several files, because no flag says "several" to every picker
//! (`zenity --multiple`, `fzf -m`, `kitten choose-files --mode files`,
//! osascript's `with multiple selections allowed`, and nothing at all for
//! yazi), and a placeholder that expanded to one or another would need a
//! little language of conditionals inside a value — which the settings file
//! is careful not to be ([`crate::options`]). See [`Pickers::choose`].
//!
//! # A command line, split like a shell's, run without one
//!
//! The value is split into words the way a shell splits them — spaces
//! separate, quotes and backslashes keep them together ([`split_words`]) —
//! and run directly, with no shell. `{dir}` in a word is the directory the
//! picker should start in, by the same rule the row prompt starts by
//! ([`crate::upload::start_dir`]), and `{out}` a file it may write its answer
//! to ([`Command::expand`]); the values are put inside the words they are in
//! and never split again, so a directory with a space in it is still one
//! argument. There is no `$VAR`, no glob and no `~`, except a `~/` at the
//! start of a command written in the file, which is made `$HOME`'s as every
//! other path in the file is. Somebody who wants a shell writes
//! `sh -c '...'`, and then it is theirs.
//!
//! # The answer is paths, one per line, and checked as a typed one is
//!
//! From `{out}` when the command has one, else from its standard output
//! ([`parse_output`]). A trailing newline and blank lines are nothing; a
//! `file://` url is taken too, since some pickers print those
//! ([`decode_file_uri`]); a relative line is under the start directory, which
//! is also the picker's working directory, because that is what fzf prints.
//! A non-zero exit, or no path at all, is the person cancelling: osascript
//! exits 1 on its -128, zenity 1, fzf 130, and yazi 0 with nothing written.
//! Then every path goes through the same checks a typed one does
//! ([`accept`]) — absolute, there, a regular file, readable — because the
//! engine checks nothing and a directory or a missing name reaches the page
//! as a file that is not one (see [`crate::upload`]). One that fails sends
//! nothing, and the row says why.
//!
//! # A window runs beside the loop; a terminal program runs instead of it
//!
//! A [`Gui`] is started and left: in a process group of its own, as the
//! engine is and for the engine's reason — `sh -c '…'` around a dialog is
//! a wrapper, and killing the wrapper's pid would leave the dialog up — with
//! nothing on its standard input and its errors thrown away rather than
//! printed over the pane. The loop polls its output with everything else
//! and reaps it when it exits ([`Gui::pump`]), so the page keeps painting
//! and the keys keep going to it while the dialog is open, Escape included:
//! the dialog has its own Cancel. One runs at a time; a click while it is
//! open is told `cancel` at once. Dropping a [`Gui`] ends it — the group
//! asked with `SIGTERM`, then told with `SIGKILL` half a second later, as
//! the engine is — which is what happens when its tab closes or goes
//! somewhere else, when the engine dies, and when the program quits. Not
//! when it panics: a release build aborts, nothing is dropped, and a dialog
//! left open is one the person closes, which is not worth a second panic
//! hook for.
//!
//! A terminal picker is run to completion ([`run_terminal`]) once the loop
//! has given the terminal back (`Pane::release`), and
//! nothing else happens meanwhile: nothing could be drawn anyway. It is left
//! in this program's own process group — the terminal's foreground group —
//! so that it can read the terminal without being stopped for it, and while
//! it runs this program ignores `SIGINT` and `SIGQUIT`, so that a `ctrl+c`
//! that the picker's terminal mode turns into a signal ends the picker and
//! not the browser. The picker has both put back to their defaults before
//! it starts.

use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::engine;
use crate::text;
use crate::tty::{self, ReadOutcome};
use crate::upload::{self, Chooser, Dir, Refusal};

/// What the row and the strip say about a tab while its picker is open, in
/// place of its title: the click did something, and the something is in
/// another window or about to take the terminal.
pub const WORDS: &str = "choosing a file";

/// The most a picker may print before it is taken as broken and stopped: it
/// is paths, not data, and 64 KiB is hundreds of them.
pub const MAX_OUTPUT: usize = 64 * 1024;

/// A picker's command, as the settings gave it: words, not yet expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The program, then its arguments, each with its placeholders still in
    /// it. Never empty.
    pub words: Vec<String>,
}

impl Command {
    /// A setting's value, split into words ([`split_words`]). `name` is the
    /// setting or the option, for the sentence when it is refused.
    pub fn parse(name: &str, text: &str) -> Result<Command, String> {
        let words = split_words(text).map_err(|why| format!("{name} {why}"))?;
        Ok(Command { words })
    }

    /// A `~/` at the start of the program made `$HOME`'s: for a command
    /// written in the settings file, where no shell did it, as
    /// [`crate::options::expand_home`] does for the file's paths. Only the
    /// program: an argument with a `~` in it is the picker's to read.
    pub fn expand_home(mut self, home: Option<&Path>) -> Command {
        if let (Some(first), Some(home)) = (self.words.first_mut(), home) {
            if let Some(rest) = first.strip_prefix("~/") {
                *first = home.join(rest).to_string_lossy().into_owned();
            }
        }
        self
    }

    /// Whether the command asks for a file to write its answer to, and so
    /// is not read from its standard output.
    pub fn has_out(&self) -> bool {
        self.words.iter().any(|word| word.contains("{out}"))
    }

    /// The words to run: `{dir}` and `{out}` replaced inside each word, and
    /// nothing split again. Any other `{…}` is left as it is written — it is
    /// the picker's, or a typo the picker will say something about. `{out}`
    /// stays too when there is no file, which only a command without one
    /// asks for.
    pub fn expand(&self, dir: &Path, out: Option<&Path>) -> Vec<String> {
        let dir = dir.to_string_lossy();
        let out = out.map(|out| out.to_string_lossy());
        self.words
            .iter()
            .map(|word| {
                let word = word.replace("{dir}", &dir);
                match &out {
                    Some(out) => word.replace("{out}", out),
                    None => word,
                }
            })
            .collect()
    }
}

/// A command line split into words, the way a shell splits one and no
/// further.
///
/// Spaces and tabs separate words. `'…'` keeps everything up to the next `'`
/// as it is. `"…"` does too, except that `\"` and `\\` in it are a quote and
/// a backslash. Outside quotes a backslash keeps the character after it,
/// a space included. Quoted text next to plain text is one word
/// (`--x='a b'` is `--x=a b`), and `''` is an empty one. Nothing else is
/// special: no `$`, no glob, no `~`, no `;`.
///
/// Refused when a quote is never closed, and when there are no words.
pub fn split_words(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    // Whether a word has begun, which an empty `''` is.
    let mut in_word = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("has a quote that is never closed".to_string()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("has a quote that is never closed".to_string()),
                        },
                        Some(c) => word.push(c),
                        None => return Err("has a quote that is never closed".to_string()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                // A backslash at the very end keeps nothing, and is kept
                // itself rather than refused: there is nothing it could have
                // meant that is worth a sentence.
                word.push(chars.next().unwrap_or('\\'));
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    if words.is_empty() {
        return Err("needs a command".to_string());
    }
    Ok(words)
}

/// Which kind of picker: one with a window of its own, or one that runs in
/// this terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `file-picker`: left to run while the browser carries on.
    Gui,
    /// `file-picker-terminal`: given the terminal until it exits.
    Terminal,
}

/// The four settings, as the run has them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pickers {
    /// `file-picker`.
    pub gui: Option<Command>,
    /// `file-picker-multiple`.
    pub gui_multiple: Option<Command>,
    /// `file-picker-terminal`.
    pub terminal: Option<Command>,
    /// `file-picker-terminal-multiple`.
    pub terminal_multiple: Option<Command>,
}

impl Pickers {
    /// Whether none is set, which is the row prompt as it always was.
    pub fn is_empty(&self) -> bool {
        self.gui.is_none()
            && self.gui_multiple.is_none()
            && self.terminal.is_none()
            && self.terminal_multiple.is_none()
    }

    /// The picker for an input: `multiple` says whether it takes several
    /// files, `display` whether a window can be opened ([`has_display`]).
    ///
    /// Of each kind, the `-multiple` one for a `multiple` input and the
    /// plain one otherwise, each falling back to the other when it is the
    /// only one of its kind that is set: a picker that can choose several is
    /// still a picker for one (the first of what it prints is taken), and one
    /// that chooses one still gives a `multiple` input a file. Then the
    /// window where there is a display and the terminal where there is not,
    /// when both kinds are set; the one kind there is, when only one is.
    /// `None` only when nothing is set.
    pub fn choose(&self, multiple: bool, display: bool) -> Option<(Kind, &Command)> {
        let gui = of_kind(&self.gui, &self.gui_multiple, multiple);
        let terminal = of_kind(&self.terminal, &self.terminal_multiple, multiple);
        match (gui, terminal) {
            (Some(gui), Some(_)) if display => Some((Kind::Gui, gui)),
            (_, Some(terminal)) => Some((Kind::Terminal, terminal)),
            (Some(gui), None) => Some((Kind::Gui, gui)),
            (None, None) => None,
        }
    }
}

/// The one of a kind for an input: see [`Pickers::choose`].
fn of_kind<'a>(
    plain: &'a Option<Command>,
    several: &'a Option<Command>,
    multiple: bool,
) -> Option<&'a Command> {
    let (first, second) = if multiple {
        (several, plain)
    } else {
        (plain, several)
    };
    first.as_ref().or(second.as_ref())
}

/// Whether a window can be opened from here: an X or Wayland display named
/// in the environment, or a Mac that is not being reached over ssh — a Mac
/// has no variable for its display, and a Finder dialog opened from an ssh
/// session opens on a screen the person is not in front of, if on any.
///
/// Pure over its getter, like [`crate::route::Env::read`].
pub fn has_display(var: impl Fn(&str) -> Option<String>) -> bool {
    has_display_on(cfg!(target_os = "macos"), var)
}

/// [`has_display`], with whether this is a Mac said rather than compiled
/// in, so that both answers can be tested on either.
pub fn has_display_on(mac: bool, var: impl Fn(&str) -> Option<String>) -> bool {
    let set = |name: &str| var(name).is_some_and(|value| !value.is_empty());
    if set("DISPLAY") || set("WAYLAND_DISPLAY") {
        return true;
    }
    mac && !["SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"]
        .iter()
        .any(|name| set(name))
}

/// A tab's file input while a picker is to answer it, kept on the tab
/// beside where [`crate::upload::Upload`] would be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picking {
    /// What the page asked.
    pub chooser: Chooser,
    /// Whether the picker has been started: `false` from the click until the
    /// loop gets to it, which is the same pass.
    pub started: bool,
}

/// How a picker ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The paths it chose, absolute, not yet checked: see [`accept`].
    Files(Vec<PathBuf>),
    /// It was cancelled, or chose nothing: the page is told `cancel`.
    Cancel,
    /// It could not be run, or broke: the sentence goes on the row, and the
    /// page is told `cancel` so that it is not left waiting.
    Failed(String),
}

/// What a picker that has exited said: its exit, and the bytes it answered
/// with — its output, or the `{out}` file.
pub fn outcome(success: bool, answer: &[u8], dir: &Path, home: Option<&Path>) -> Outcome {
    if !success {
        return Outcome::Cancel;
    }
    let paths = parse_output(answer, dir, home);
    if paths.is_empty() {
        return Outcome::Cancel;
    }
    Outcome::Files(paths)
}

/// A picker's answer as paths: one per line, a `\r` before the newline
/// dropped, blank lines skipped, a `file://` url decoded
/// ([`decode_file_uri`]) — and one that cannot be is skipped — and each made
/// absolute against `dir` the way a typed one is
/// ([`crate::upload::absolute`]).
///
/// Not trimmed otherwise: a name can end in a space, and a picker prints it
/// as it is.
pub fn parse_output(bytes: &[u8], dir: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    String::from_utf8_lossy(bytes)
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            if line.starts_with("file://") {
                decode_file_uri(line)
            } else {
                Some(line.to_string())
            }
        })
        .map(|line| upload::absolute(&line, dir, home))
        .collect()
}

/// A `file://` url as the path it names: `file:///a%20b` is `/a b`, and
/// `file://localhost/x` is `/x`. `None` for any other host — a file on
/// another machine is not one this program can hand over — for a `%` that
/// is not followed by two hex digits, and for escapes that do not make
/// UTF-8.
pub fn decode_file_uri(line: &str) -> Option<String> {
    let rest = line.strip_prefix("file://")?;
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(at) if &rest[..at] == "localhost" => &rest[at..],
        _ => return None,
    };
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The paths a picker chose, as the files to send, checked as the row
/// prompt checks a typed one ([`crate::upload::Dir::check`]).
///
/// The first only, for an input that takes one: what a browser's own dialog
/// hands a plain input. All or nothing: one path refused refuses the lot,
/// since sending the rest would be a choice the person did not make, and a
/// directory is refused whatever the input is, for the reason the row
/// prompt refuses one.
pub fn accept(paths: Vec<PathBuf>, multiple: bool, fs: &dyn Dir) -> Result<Vec<PathBuf>, Refusal> {
    let take = if multiple { paths.len() } else { 1 };
    let mut files = Vec::new();
    for path in paths.into_iter().take(take) {
        match fs.check(&path)? {
            upload::Kind::File => files.push(path),
            upload::Kind::Directory => return Err(Refusal::IsDirectory),
        }
    }
    Ok(files)
}

/// The file a picker writes its answer to, for a command with `{out}`:
/// made empty before the picker starts, readable by this user alone, and
/// removed when it is dropped.
///
/// In the temporary directory under a name of this program's and this
/// process's, and made with `O_EXCL` so that a name somebody else put there
/// first is never taken over — the next number is tried instead. Made empty
/// rather than left for the picker to create, because a picker cancelled
/// before it writes anything (yazi on `q`) leaves the file as it was, and a
/// file that is there and empty is a cancel with no question about whose
/// file it was.
pub struct OutFile {
    path: PathBuf,
}

impl OutFile {
    /// A new empty file, `0600`.
    pub fn create() -> std::io::Result<OutFile> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir();
        let mut tries = 0;
        loop {
            let n = NEXT.fetch_add(1, Ordering::SeqCst);
            let path = dir.join(format!("blinkterm-pick-{}-{n}", std::process::id()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(_) => return Ok(OutFile { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 16 => {
                    tries += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Where it is, for `{out}`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What the picker wrote, up to one byte past [`MAX_OUTPUT`] so that a
    /// file too big is seen to be; nothing if it is gone.
    pub fn read(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        if let Ok(file) = std::fs::File::open(&self.path) {
            let _ = file.take(MAX_OUTPUT as u64 + 1).read_to_end(&mut bytes);
        }
        bytes
    }
}

impl Drop for OutFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The sentence for a picker that printed more than [`MAX_OUTPUT`].
const TOO_MUCH: &str = "the file picker printed more than 64 KiB";

/// Why a picker could not be started, for the row: the program's name, as
/// plain text since it is the person's own words from a file, and the
/// system's reason in a few of its own.
fn cannot_start(program: &str, error: &std::io::Error) -> String {
    let why = match error.kind() {
        std::io::ErrorKind::NotFound => "no such program".to_string(),
        kind => kind.to_string(),
    };
    format!(
        "can't start the file picker {}: {why}",
        text::sanitize(program)
    )
}

/// A picker with a window of its own, while it runs. See the module for
/// why it is left to run and how it is ended.
pub struct Gui {
    child: Child,
    /// `kill(2)`'s argument for its group, as [`crate::engine`] keeps one.
    target: i32,
    /// Its standard output, non-blocking, until end of file; `None` from
    /// the start for a command with `{out}`.
    stdout: Option<OwnedFd>,
    /// What it has printed so far.
    read: Vec<u8>,
    out: Option<OutFile>,
    /// Where it started, which a relative line is under.
    dir: PathBuf,
    home: Option<PathBuf>,
    /// Whether it has been waited for, and so has nothing left to kill.
    reaped: bool,
    /// The tab whose input it is answering, by target id.
    pub tab: String,
    /// The input it is answering.
    pub chooser: Chooser,
}

impl Gui {
    /// Start `command` in `dir` for `chooser`, on the tab `tab`. The
    /// sentence for the row when it cannot be.
    pub fn spawn(
        command: &Command,
        dir: &Path,
        tab: &str,
        chooser: &Chooser,
    ) -> Result<Gui, String> {
        let out = if command.has_out() {
            Some(OutFile::create().map_err(|e| {
                format!(
                    "can't make a file for the file picker to write to: {}",
                    e.kind()
                )
            })?)
        } else {
            None
        };
        let argv = command.expand(dir, out.as_ref().map(OutFile::path));
        let mut process = std::process::Command::new(&argv[0]);
        process
            .args(&argv[1..])
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(if out.is_some() {
                Stdio::null()
            } else {
                Stdio::piped()
            })
            .stderr(Stdio::null());
        let (mut child, target) =
            engine::spawn_in_own_group(&mut process).map_err(|e| cannot_start(&argv[0], &e))?;
        let stdout: Option<OwnedFd> = child.stdout.take().map(OwnedFd::from);
        if let Some(fd) = &stdout {
            tty::set_nonblocking(fd.as_raw_fd()).ok();
        }
        Ok(Gui {
            child,
            target,
            stdout,
            read: Vec::new(),
            out,
            dir: dir.to_path_buf(),
            home: upload::home(),
            reaped: false,
            tab: tab.to_string(),
            chooser: chooser.clone(),
        })
    }

    /// The descriptor to poll, while there is output still to come.
    pub fn fd(&self) -> Option<RawFd> {
        self.stdout.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// One pass: read what it printed if `readable`, and see whether it has
    /// exited. `Some` once it is over, and then only once.
    ///
    /// The exit is asked every pass whether the output was readable or not,
    /// because a picker with `{out}` has no output, and one whose output is
    /// at end of file is no longer polled; a pass is at most the loop's
    /// poll interval, and `try_wait` is one `waitpid`.
    pub fn pump(&mut self, readable: bool) -> Option<Outcome> {
        if self.reaped {
            return None;
        }
        if readable && self.drain() {
            self.kill_now();
            return Some(Outcome::Failed(TOO_MUCH.to_string()));
        }
        let status = match self.child.try_wait() {
            Ok(None) => return None,
            Ok(Some(status)) => status,
            Err(e) => {
                self.kill_now();
                return Some(Outcome::Failed(format!(
                    "lost the file picker: {}",
                    e.kind()
                )));
            }
        };
        self.reaped = true;
        // What it printed just before it went and was not read yet: once,
        // without waiting, since something it left behind may still hold the
        // pipe open and would keep a blocking read here for ever.
        let over = self.drain();
        self.stdout = None;
        let answer = match &self.out {
            Some(out) => out.read(),
            None => std::mem::take(&mut self.read),
        };
        if over || answer.len() > MAX_OUTPUT {
            return Some(Outcome::Failed(TOO_MUCH.to_string()));
        }
        Some(outcome(
            status.success(),
            &answer,
            &self.dir,
            self.home.as_deref(),
        ))
    }

    /// Read whatever is waiting on its output. True when it has now printed
    /// more than [`MAX_OUTPUT`]. At end of file the descriptor is let go: a
    /// pipe whose writer has gone is readable for ever, and polling it would
    /// spin the loop.
    fn drain(&mut self) -> bool {
        let Some(fd) = self.fd() else {
            return false;
        };
        let mut buf = [0u8; 8192];
        loop {
            match tty::read_available(fd, &mut buf) {
                Ok(ReadOutcome::Data(n)) => {
                    self.read.extend_from_slice(&buf[..n]);
                    if self.read.len() > MAX_OUTPUT {
                        return true;
                    }
                }
                Ok(ReadOutcome::WouldBlock) => return false,
                Ok(ReadOutcome::Eof) | Err(_) => {
                    self.stdout = None;
                    return false;
                }
            }
        }
    }

    /// End it now, with no half second of grace: it is misbehaving.
    fn kill_now(&mut self) {
        engine::signal_all(self.target, libc::SIGKILL);
        let _ = self.child.wait();
        self.reaped = true;
        self.stdout = None;
    }
}

impl Drop for Gui {
    /// Asked, then told: `SIGTERM` to the group, half a second for it to go,
    /// then `SIGKILL`, as [`crate::engine::Engine::kill`] does. A dialog
    /// has nothing to save and goes at once; the grace is for a wrapper
    /// that tidies up.
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        engine::signal_all(self.target, libc::SIGTERM);
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                // The wrapper went. Anything it started that is still in
                // its group did not take the hint, and is not waited for.
                engine::signal_all(self.target, libc::SIGKILL);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        engine::signal_all(self.target, libc::SIGKILL);
        let _ = self.child.wait();
    }
}

/// `SIGINT` and `SIGQUIT` ignored for as long as it is held, and put back
/// as they were — with `sigaction`, so that `SA_RESTART` stays off as
/// [`crate::app`] set it, which `signal(3)` would not promise.
struct QuietSignals {
    int: libc::sigaction,
    quit: libc::sigaction,
}

impl QuietSignals {
    fn new() -> QuietSignals {
        // SAFETY: `sigaction` is a C struct of a handler address, a mask
        // and flags; all-zero is a valid value of each, and both are
        // overwritten by `ignore` below before they are read.
        let mut quiet: QuietSignals = unsafe { std::mem::zeroed() };
        ignore(libc::SIGINT, &mut quiet.int);
        ignore(libc::SIGQUIT, &mut quiet.quit);
        quiet
    }
}

/// Ignore `signal`, and keep what it was in `old`.
fn ignore(signal: libc::c_int, old: &mut libc::sigaction) {
    // SAFETY: `sigaction(2)` reads the new action through the first pointer
    // and writes the old through the second, and both point at live values
    // of exactly that type for the whole call; `sigemptyset(3)` writes the
    // mask in place. `SIG_IGN` is not a function and is never called.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_IGN;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(signal, &action, old);
    }
}

impl Drop for QuietSignals {
    fn drop(&mut self) {
        // SAFETY: each is read through the pointer and copied by the kernel;
        // both are what `sigaction(2)` itself wrote in `new`, so each names
        // the handler, mask and flags that were in place before.
        unsafe {
            libc::sigaction(libc::SIGINT, &self.int, std::ptr::null_mut());
            libc::sigaction(libc::SIGQUIT, &self.quit, std::ptr::null_mut());
        }
    }
}

/// Run a terminal picker to the end, on this terminal, and say how it
/// ended. The caller has given the terminal back first and takes it again
/// after; see the module for the process group and the signals.
///
/// Its output, when it has no `{out}`, is read on a thread of its own while
/// this waits, as [`crate::engine`] reads the engine's errors: a picker that
/// printed more than a pipe holds with nobody reading would never exit.
/// What is past [`MAX_OUTPUT`] is read and thrown away for the same reason,
/// and the picker is then taken as broken.
pub fn run_terminal(command: &Command, dir: &Path, home: Option<&Path>) -> Outcome {
    let out = if command.has_out() {
        match OutFile::create() {
            Ok(out) => Some(out),
            Err(e) => {
                return Outcome::Failed(format!(
                    "can't make a file for the file picker to write to: {}",
                    e.kind()
                ))
            }
        }
    } else {
        None
    };
    let argv = command.expand(dir, out.as_ref().map(OutFile::path));
    let mut process = std::process::Command::new(&argv[0]);
    process
        .args(&argv[1..])
        .current_dir(dir)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdout(if out.is_some() {
            Stdio::inherit()
        } else {
            Stdio::piped()
        });
    // SAFETY: the closure runs in the child between `fork` and `exec`, where
    // only async-signal-safe calls are allowed; `signal(2)` is one, and the
    // closure makes two calls to it and touches nothing else. An ignored
    // signal stays ignored across `exec`, which is why they are put back.
    unsafe {
        process.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            Ok(())
        });
    }
    let quiet = QuietSignals::new();
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(e) => return Outcome::Failed(cannot_start(&argv[0], &e)),
    };
    let reader = child.stdout.take().map(|mut stdout| {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let mut buf = [0u8; 8192];
            let mut over = false;
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let room = (MAX_OUTPUT + 1).saturating_sub(kept.len());
                        kept.extend_from_slice(&buf[..n.min(room)]);
                        over |= kept.len() > MAX_OUTPUT;
                    }
                }
            }
            (kept, over)
        })
    });
    let status = child.wait();
    let printed = reader.and_then(|reader| reader.join().ok());
    drop(quiet);
    let status = match status {
        Ok(status) => status,
        Err(e) => return Outcome::Failed(format!("lost the file picker: {}", e.kind())),
    };
    let answer = match (&out, printed) {
        (Some(out), _) => out.read(),
        (None, Some((_, true))) => return Outcome::Failed(TOO_MUCH.to_string()),
        (None, Some((kept, false))) => kept,
        (None, None) => Vec::new(),
    };
    if answer.len() > MAX_OUTPUT {
        return Outcome::Failed(TOO_MUCH.to_string());
    }
    outcome(status.success(), &answer, dir, home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upload::{Disk, Entry};

    fn words(text: &str) -> Vec<String> {
        split_words(text).unwrap_or_else(|why| panic!("{text:?}: {why}"))
    }

    fn command(text: &str) -> Command {
        Command::parse("file-picker", text).expect("a command")
    }

    #[test]
    fn a_command_is_split_into_words_like_a_shell_splits_it() {
        assert_eq!(
            words("zenity --file-selection"),
            ["zenity", "--file-selection"]
        );
        assert_eq!(
            words("osascript -e 'POSIX path of (choose file)'"),
            ["osascript", "-e", "POSIX path of (choose file)"]
        );
        assert_eq!(
            words(r#"sh -c "echo \"a b\" \\ \$x""#),
            ["sh", "-c", r#"echo "a b" \ \$x"#],
            "inside double quotes only \\\" and \\\\ are escapes"
        );
        assert_eq!(words("pick --x='a b'"), ["pick", "--x=a b"]);
        assert_eq!(words(r"pick my\ file"), ["pick", "my file"]);
        assert_eq!(words("pick '' x"), ["pick", "", "x"]);
        assert_eq!(words("\tpick \t  x  "), ["pick", "x"]);
        assert_eq!(words("pick $HOME ~ *"), ["pick", "$HOME", "~", "*"]);
        assert_eq!(words(r"pick x\"), ["pick", r"x\"]);
    }

    #[test]
    fn an_unclosed_quote_or_no_command_is_refused_by_name() {
        assert_eq!(
            Command::parse("file-picker", "zenity 'x"),
            Err("file-picker has a quote that is never closed".to_string())
        );
        assert_eq!(
            Command::parse("--file-picker", r#"zenity "x\""#),
            Err("--file-picker has a quote that is never closed".to_string())
        );
        assert_eq!(
            Command::parse("file-picker", "  \t "),
            Err("file-picker needs a command".to_string())
        );
    }

    #[test]
    fn placeholders_are_replaced_inside_their_words_and_never_split() {
        let dir = Path::new("/home/someone/My Documents");
        let out = Path::new("/tmp/blinkterm-pick-1-0");
        let yazi = command("yazi --chooser-file={out} {dir}");
        assert!(yazi.has_out());
        assert_eq!(
            yazi.expand(dir, Some(out)),
            [
                "yazi",
                "--chooser-file=/tmp/blinkterm-pick-1-0",
                "/home/someone/My Documents"
            ]
        );
        let kdialog = command("kdialog --getopenfilename {dir} '{foo}' {dir}/{dir}");
        assert!(!kdialog.has_out());
        assert_eq!(
            kdialog.expand(dir, None),
            [
                "kdialog",
                "--getopenfilename",
                "/home/someone/My Documents",
                "{foo}",
                "/home/someone/My Documents//home/someone/My Documents"
            ]
        );
        // Quoted, a placeholder is still one.
        assert_eq!(
            command("pick '{dir}'").expand(Path::new("/a b"), None),
            ["pick", "/a b"]
        );
    }

    #[test]
    fn a_tilde_is_made_home_only_at_the_start_of_the_program() {
        let home = Some(Path::new("/home/someone"));
        assert_eq!(
            command("~/bin/pick ~/x").expand_home(home).words,
            ["/home/someone/bin/pick", "~/x"]
        );
        assert_eq!(
            command("~/bin/pick").expand_home(None).words,
            ["~/bin/pick"]
        );
        assert_eq!(command("pick").expand_home(home).words, ["pick"]);
    }

    #[test]
    fn the_window_is_used_where_there_is_a_display_and_the_terminal_elsewhere() {
        let gui = command("zenity --file-selection");
        let gui_many = command("zenity --file-selection --multiple");
        let tui = command("fzf");
        let tui_many = command("fzf -m");
        let all = Pickers {
            gui: Some(gui.clone()),
            gui_multiple: Some(gui_many.clone()),
            terminal: Some(tui.clone()),
            terminal_multiple: Some(tui_many.clone()),
        };
        assert_eq!(all.choose(false, true), Some((Kind::Gui, &gui)));
        assert_eq!(all.choose(true, true), Some((Kind::Gui, &gui_many)));
        assert_eq!(all.choose(false, false), Some((Kind::Terminal, &tui)));
        assert_eq!(all.choose(true, false), Some((Kind::Terminal, &tui_many)));

        // Only one kind: that one, display or not.
        let only_gui = Pickers {
            gui: Some(gui.clone()),
            ..Pickers::default()
        };
        assert_eq!(only_gui.choose(false, false), Some((Kind::Gui, &gui)));
        assert_eq!(
            only_gui.choose(true, true),
            Some((Kind::Gui, &gui)),
            "the plain one stands in for several"
        );
        let only_tui = Pickers {
            terminal: Some(tui.clone()),
            ..Pickers::default()
        };
        assert_eq!(only_tui.choose(false, true), Some((Kind::Terminal, &tui)));

        // A `-multiple` one alone stands in for one file too.
        let only_many = Pickers {
            terminal_multiple: Some(tui_many.clone()),
            ..Pickers::default()
        };
        assert!(!only_many.is_empty());
        assert_eq!(
            only_many.choose(false, true),
            Some((Kind::Terminal, &tui_many))
        );

        assert!(Pickers::default().is_empty());
        assert_eq!(Pickers::default().choose(false, true), None);
    }

    #[test]
    fn a_display_is_a_named_one_or_a_mac_not_reached_over_ssh() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        };
        assert!(has_display_on(false, env(&[("DISPLAY", ":0")])));
        assert!(has_display_on(
            false,
            env(&[("WAYLAND_DISPLAY", "wayland-0")])
        ));
        assert!(!has_display_on(false, env(&[])));
        assert!(!has_display_on(false, env(&[("DISPLAY", "")])));
        assert!(has_display_on(true, env(&[])));
        assert!(!has_display_on(true, env(&[("SSH_TTY", "/dev/ttys001")])));
        assert!(!has_display_on(true, env(&[("SSH_CONNECTION", "a b c d")])));
        assert!(has_display_on(
            true,
            env(&[("SSH_CLIENT", "x"), ("DISPLAY", ":0")])
        ));
        assert_eq!(
            has_display(env(&[])),
            cfg!(target_os = "macos"),
            "with nothing named, only a Mac has one"
        );
    }

    #[test]
    fn the_answer_is_one_absolute_path_per_line() {
        let dir = Path::new("/work");
        let home = Some(Path::new("/home/someone"));
        let parse = |text: &str| parse_output(text.as_bytes(), dir, home);
        assert_eq!(parse("/a/report.pdf\n"), [PathBuf::from("/a/report.pdf")]);
        assert_eq!(
            parse("\n/a/one.txt\r\n\n  \n/a/two.txt"),
            [PathBuf::from("/a/one.txt"), PathBuf::from("/a/two.txt")]
        );
        assert_eq!(
            parse("notes.txt\n~/x.png\n../up.txt\n"),
            [
                PathBuf::from("/work/notes.txt"),
                PathBuf::from("/home/someone/x.png"),
                PathBuf::from("/up.txt")
            ],
            "relative to the start directory, which is the picker's own"
        );
        assert_eq!(
            parse("file:///a/my%20notes.md\nfile://localhost/b\nfile://nas/c\nfile:///%zz\n"),
            [PathBuf::from("/a/my notes.md"), PathBuf::from("/b")]
        );
        assert_eq!(
            parse("name ends in a space \n"),
            [PathBuf::from("/work/name ends in a space ")]
        );
        assert!(parse("").is_empty());
        assert!(parse("\n\n").is_empty());
    }

    #[test]
    fn a_file_url_is_decoded_or_skipped() {
        assert_eq!(decode_file_uri("file:///a%20b").as_deref(), Some("/a b"));
        assert_eq!(
            decode_file_uri("file:///%E2%9C%93").as_deref(),
            Some("/\u{2713}")
        );
        assert_eq!(decode_file_uri("file://localhost/x").as_deref(), Some("/x"));
        assert_eq!(decode_file_uri("file://host/x"), None);
        assert_eq!(decode_file_uri("file://localhost"), None);
        assert_eq!(decode_file_uri("file:///a%2"), None);
        assert_eq!(decode_file_uri("file:///a%zz"), None);
        assert_eq!(decode_file_uri("file:///%FF"), None, "not UTF-8");
        assert_eq!(decode_file_uri("/plain"), None);
    }

    #[test]
    fn nonzero_or_nothing_is_a_cancel() {
        let dir = Path::new("/work");
        assert_eq!(outcome(false, b"/a\n", dir, None), Outcome::Cancel);
        assert_eq!(outcome(true, b"", dir, None), Outcome::Cancel);
        assert_eq!(outcome(true, b"\n \n", dir, None), Outcome::Cancel);
        assert_eq!(outcome(true, b"file://nas/a\n", dir, None), Outcome::Cancel);
        assert_eq!(
            outcome(true, b"/a\n", dir, None),
            Outcome::Files(vec![PathBuf::from("/a")])
        );
    }

    /// A filesystem of the test's own: a list of what is what.
    struct Fake(Vec<(&'static str, Result<upload::Kind, Refusal>)>);

    impl Dir for Fake {
        fn entries(&self, _dir: &Path) -> Vec<Entry> {
            Vec::new()
        }
        fn check(&self, path: &Path) -> Result<upload::Kind, Refusal> {
            self.0
                .iter()
                .find(|(name, _)| Path::new(name) == path)
                .map_or(Err(Refusal::Missing), |(_, kind)| kind.clone())
        }
    }

    #[test]
    fn what_a_picker_chose_is_checked_as_a_typed_path_is() {
        let fs = Fake(vec![
            ("/a/one.txt", Ok(upload::Kind::File)),
            ("/a/two.txt", Ok(upload::Kind::File)),
            ("/a/dir", Ok(upload::Kind::Directory)),
            ("/a/fifo", Err(Refusal::NotAFile)),
        ]);
        let paths = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            accept(paths(&["/a/one.txt", "/a/two.txt"]), false, &fs),
            Ok(paths(&["/a/one.txt"])),
            "one input, one file: the first"
        );
        assert_eq!(
            accept(paths(&["/a/one.txt", "/a/nothing"]), false, &fs),
            Ok(paths(&["/a/one.txt"])),
            "and what is not taken is not checked"
        );
        assert_eq!(
            accept(paths(&["/a/one.txt", "/a/two.txt"]), true, &fs),
            Ok(paths(&["/a/one.txt", "/a/two.txt"]))
        );
        assert_eq!(
            accept(paths(&["/a/dir"]), false, &fs),
            Err(Refusal::IsDirectory)
        );
        assert_eq!(
            accept(paths(&["/a/one.txt", "/a/missing"]), true, &fs),
            Err(Refusal::Missing),
            "all or nothing"
        );
        assert_eq!(
            accept(paths(&["/a/fifo"]), true, &fs),
            Err(Refusal::NotAFile)
        );
        // And against the disk, the check is the row prompt's own.
        assert_eq!(
            accept(paths(&["/dev/null"]), false, &Disk),
            Err(Refusal::NotAFile)
        );
    }

    // Running them.

    /// The signal dispositions are the process's, and the tests run on
    /// threads of one process: the tests that change them take turns.
    static SIGNALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn chooser(multiple: bool) -> Chooser {
        Chooser {
            backend_node_id: 3,
            multiple,
            frame_id: "F".to_string(),
        }
    }

    /// Pump `gui` as the loop does until it is over, or panic after a few
    /// seconds.
    fn finish(gui: &mut Gui) -> Outcome {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let readable = match gui.fd() {
                Some(fd) => tty::poll_readable(&[fd], 50).expect("poll").contains(&fd),
                None => {
                    std::thread::sleep(Duration::from_millis(20));
                    false
                }
            };
            if let Some(outcome) = gui.pump(readable) {
                return outcome;
            }
        }
        panic!("the picker did not finish");
    }

    fn gui(text: &str, dir: &Path, multiple: bool) -> Gui {
        Gui::spawn(&command(text), dir, "T", &chooser(multiple)).expect("started")
    }

    #[test]
    fn a_window_picker_is_read_from_what_it_prints() {
        let dir = std::env::temp_dir();
        let mut picker = gui("sh -c 'printf \"%s\\n\" /a/one.txt two.txt'", &dir, true);
        assert!(picker.fd().is_some());
        assert_eq!(
            finish(&mut picker),
            Outcome::Files(vec![PathBuf::from("/a/one.txt"), dir.join("two.txt")])
        );
        assert_eq!(picker.pump(true), None, "said once");
        assert_eq!(picker.tab, "T");

        // Its working directory is the start directory, which is what a
        // relative line is under.
        let mut picker = gui("sh -c pwd", Path::new("/"), false);
        assert_eq!(
            finish(&mut picker),
            Outcome::Files(vec![PathBuf::from("/")])
        );
    }

    #[test]
    fn a_window_picker_that_fails_or_says_nothing_is_a_cancel() {
        let dir = std::env::temp_dir();
        assert_eq!(
            finish(&mut gui("sh -c 'echo /a; exit 1'", &dir, false)),
            Outcome::Cancel
        );
        assert_eq!(finish(&mut gui("true", &dir, false)), Outcome::Cancel);
    }

    #[test]
    fn a_window_picker_with_out_is_read_from_the_file() {
        let dir = std::env::temp_dir();
        let mut picker = gui(
            "sh -c 'echo ignored; printf \"/b/x.txt\\n\" > \"$1\"' sh {out}",
            &dir,
            false,
        );
        assert_eq!(picker.fd(), None, "its output is not read");
        let out = picker.out.as_ref().expect("a file").path().to_path_buf();
        assert_eq!(
            finish(&mut picker),
            Outcome::Files(vec![PathBuf::from("/b/x.txt")])
        );
        drop(picker);
        assert!(!out.exists(), "the file goes with the picker");

        // Nothing written is nothing chosen.
        assert_eq!(finish(&mut gui("true {out}", &dir, false)), Outcome::Cancel);
    }

    #[test]
    fn a_window_picker_that_cannot_start_says_why() {
        let started = Gui::spawn(
            &command("blinkterm-no-such-picker --x"),
            &std::env::temp_dir(),
            "T",
            &chooser(false),
        );
        assert_eq!(
            started.err().as_deref(),
            Some("can't start the file picker blinkterm-no-such-picker: no such program")
        );
    }

    #[test]
    fn a_window_picker_that_prints_too_much_is_stopped() {
        let mut picker = gui("yes /a/path/that/goes/on", &std::env::temp_dir(), false);
        assert_eq!(finish(&mut picker), Outcome::Failed(TOO_MUCH.to_string()));
        assert!(picker.reaped);
    }

    /// Whether anything is left in a process group.
    fn group_gone(target: i32) -> bool {
        // SAFETY: signal 0 sends nothing and `kill(2)` reads no memory.
        let alive = unsafe { libc::kill(target, 0) } == 0;
        !alive && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    #[test]
    fn dropping_a_window_picker_ends_it_and_what_it_started() {
        // A wrapper and the program it runs, as `sh -c` around a dialog is.
        let picker = gui("sh -c 'sleep 30; true'", &std::env::temp_dir(), false);
        let target = picker.target;
        assert!(target < 0, "a group of its own");
        std::thread::sleep(Duration::from_millis(100));
        assert!(!group_gone(target));
        let started = Instant::now();
        drop(picker);
        let deadline = Instant::now() + Duration::from_secs(1);
        while !group_gone(target) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(group_gone(target), "something of the picker is left");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn the_out_file_is_new_empty_private_and_removed() {
        use std::os::unix::fs::PermissionsExt;
        let out = OutFile::create().expect("a file");
        let other = OutFile::create().expect("another");
        assert_ne!(out.path(), other.path());
        let meta = std::fs::metadata(out.path()).expect("there");
        assert_eq!(meta.len(), 0);
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert!(out.read().is_empty());
        let path = out.path().to_path_buf();
        drop(out);
        assert!(!path.exists());
    }

    /// Where `signal`'s handler is now.
    fn handler(signal: libc::c_int) -> libc::sighandler_t {
        // SAFETY: zeroed is a valid `sigaction`; `sigaction(2)` with a null
        // new action only writes the current one through the pointer, which
        // points at a live local.
        unsafe {
            let mut now: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, std::ptr::null(), &mut now);
            now.sa_sigaction
        }
    }

    #[test]
    fn interrupts_are_ignored_while_held_and_put_back_after() {
        let _turn = SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        let before = (handler(libc::SIGINT), handler(libc::SIGQUIT));
        let quiet = QuietSignals::new();
        assert_eq!(handler(libc::SIGINT), libc::SIG_IGN);
        assert_eq!(handler(libc::SIGQUIT), libc::SIG_IGN);
        drop(quiet);
        assert_eq!((handler(libc::SIGINT), handler(libc::SIGQUIT)), before);
    }

    #[test]
    fn a_terminal_picker_is_read_from_its_output_or_its_file() {
        let _turn = SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir();
        let run = |text: &str| run_terminal(&command(text), &dir, None);
        assert_eq!(
            run("sh -c 'printf \"%s\\n\" /a/one.txt'"),
            Outcome::Files(vec![PathBuf::from("/a/one.txt")])
        );
        assert_eq!(
            run("sh -c 'printf \"file:///a/b%%20c\\n\" > \"$1\"' sh {out}"),
            Outcome::Files(vec![PathBuf::from("/a/b c")])
        );
        assert_eq!(run("sh -c 'echo /a; exit 130'"), Outcome::Cancel);
        assert_eq!(run("true {out}"), Outcome::Cancel);
        assert_eq!(
            run("sh -c 'head -c 70000 /dev/zero | tr \"\\0\" a'"),
            Outcome::Failed(TOO_MUCH.to_string())
        );
        assert_eq!(
            run("blinkterm-no-such-picker"),
            Outcome::Failed(
                "can't start the file picker blinkterm-no-such-picker: no such program".to_string()
            )
        );
        // And the picker gets the interrupt back that this program
        // ignores while it runs.
        assert_eq!(
            run("sh -c 'kill -INT $$; echo /not/reached'"),
            Outcome::Cancel,
            "killed by it, as a picker's own ctrl+c would"
        );
    }
}
