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

use std::path::{Path, PathBuf};

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
}
