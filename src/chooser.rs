//! Choosing a profile by name: the picker a start shows when nothing chose
//! one, and `blinkterm profiles`, which does the same things from a command
//! line. The registry itself — the file, the lock, the rules for a name — is
//! [`crate::registry`]; this is the part that talks to a person.
//!
//! # The picker is lines, on the ordinary screen
//!
//! It runs first of all in [`crate::app::run`], before the terminal is asked
//! what it can draw, before the signal handlers and the panic hook, before
//! any engine: so it is a numbered list and a `> ` prompt in cooked mode,
//! read a line at a time with the terminal's own line editing, and nothing
//! is put back afterwards because nothing was taken. `ctrl+c` ends the
//! process as it would end `cat`, with nothing to clean up. No raw mode, no
//! alternate screen and no keyboard protocol means no escape sequence to
//! parse, and it works in a terminal the probe would go on to refuse — the
//! refusal comes after, as it always has.
//!
//! ```text
//! blinkterm profiles
//!
//!   1  Default   managed   (default)
//!   2  Work      managed   in use (pid 4242)
//!   3  Testing   ~/work-profile
//!
//! a number opens that profile; n makes a new one; d N uses N by default;
//! r N renames N; x N removes N; q quits
//! >
//! ```
//!
//! Every change is made through the registry, which reads the file again
//! under its lock first, and the list is read again before it is drawn
//! again, so a `blinkterm profiles create` typed in another terminal while
//! this one waits shows up rather than being written over. Opening a profile
//! another blinkterm is using is allowed: the profile's own lock, taken
//! afterwards, refuses it with the sentence that names the pid, and the
//! picker does not pretend to own that lock.
//!
//! # `blinkterm profiles`
//!
//! `list`, `create`, `rename`, `default` and `remove`, for scripts and for
//! people who know what they want. It is a word only as the first argument
//! — `blinkterm profiles` is this, and `blinkterm -- profiles` is still a
//! host of that name — and it reads no settings file, since the registry is
//! wherever `$XDG_DATA_HOME` says and nothing in the file changes that.
//! Paths in `list` are absolute and whole, for a script to read; the picker
//! shortens them for a person.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::profile::Profile;
use crate::registry::{self, Registry, Removed, Selected};
use crate::screen;
use crate::text;

/// `blinkterm profiles --help`.
pub const USAGE: &str = "\
usage: blinkterm profiles [list]
       blinkterm profiles create <name> [--dir <dir>]
       blinkterm profiles rename <old> <new>
       blinkterm profiles default <name>
       blinkterm profiles remove <name>

  list     the profiles, * before the default, each with its directory
  create   a new profile called <name>, in a directory blinkterm makes and
           looks after; with --dir, the existing directory <dir> instead,
           which blinkterm never deletes
  rename   call a profile something else; nothing on disk moves
  default  the profile a start opens when none is chosen
  remove   take a profile out of the list: one blinkterm made goes to
           $XDG_DATA_HOME/blinkterm/trash until you delete it, a --dir
           one is left where it is. Refused while it is in use

A start opens a profile with --profile-name <name>, asks with
--choose-profile, and with neither opens the default, or asks when there is
none. The list is $XDG_DATA_HOME/blinkterm/profiles.json
(~/.local/share/blinkterm/profiles.json).
";

/// What `blinkterm profiles …` asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    List,
    Create { name: String, dir: Option<PathBuf> },
    Rename { from: String, to: String },
    Default(String),
    Remove(String),
    Help,
}

/// The words after `profiles`, read. An `Err` is a mistake on the command
/// line, exit 2.
pub fn parse(args: &[String]) -> Result<Verb, String> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Ok(Verb::Help);
    }
    let Some((verb, rest)) = args.split_first() else {
        return Ok(Verb::List);
    };
    let one = |what: &str| -> Result<String, String> {
        match rest {
            [] => Err(format!("profiles {what} needs a name")),
            [name] => Ok(name.clone()),
            _ => Err(format!("profiles {what} takes one name")),
        }
    };
    match verb.as_str() {
        "help" => Ok(Verb::Help),
        "list" if rest.is_empty() => Ok(Verb::List),
        "list" => Err("profiles list takes nothing after it".to_string()),
        "create" => {
            let mut name = None;
            let mut dir = None;
            let mut words = rest.iter();
            while let Some(word) = words.next() {
                let value = if word == "--dir" {
                    Some(words.next().map(String::as_str).unwrap_or_default())
                } else {
                    word.strip_prefix("--dir=")
                };
                if let Some(value) = value {
                    if value.is_empty() {
                        return Err("profiles: --dir needs a directory".to_string());
                    }
                    if dir.replace(PathBuf::from(value)).is_some() {
                        return Err("profiles create takes one --dir".to_string());
                    }
                } else if word.starts_with('-') {
                    return Err(format!("profiles create: unknown option {word}"));
                } else if name.replace(word.clone()).is_some() {
                    return Err("profiles create takes one name, and --dir <dir>".to_string());
                }
            }
            let name = name.ok_or("profiles create needs a name")?;
            Ok(Verb::Create { name, dir })
        }
        "rename" => match rest {
            [from, to] => Ok(Verb::Rename {
                from: from.clone(),
                to: to.clone(),
            }),
            [_, _, _, ..] => {
                Err("profiles rename takes the old name and the new, and nothing else".to_string())
            }
            _ => Err("profiles rename needs the old name and the new".to_string()),
        },
        "default" => one("default").map(Verb::Default),
        "remove" => one("remove").map(Verb::Remove),
        other => Err(format!(
            "profiles: unknown command \"{}\"; list, create, rename, default or remove",
            text::sanitize(other)
        )),
    }
}

/// `blinkterm profiles …`, for `main`: 0 when it did what it was asked, 1
/// when the registry, a name or a profile in use stopped it, 2 for a
/// command line it could not read.
pub fn command(args: &[String]) -> ExitCode {
    let verb = match parse(args) {
        Ok(verb) => verb,
        Err(why) => {
            eprintln!("blinkterm: {}", text::sanitize(&why));
            eprintln!("try 'blinkterm profiles --help'");
            return ExitCode::from(2);
        }
    };
    if verb == Verb::Help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match Profile::data_dir().and_then(|data| run(&data, verb)) {
        Ok(lines) => {
            for line in lines {
                println!("{}", text::sanitize(&line));
            }
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("blinkterm: profiles: {}", text::sanitize(&why));
            ExitCode::FAILURE
        }
    }
}

/// One verb against the registry in `data`, and the lines that say what it
/// did.
fn run(data: &Path, verb: Verb) -> Result<Vec<String>, String> {
    match verb {
        Verb::Help => Ok(USAGE.lines().map(str::to_string).collect()),
        Verb::List => {
            let registry = Registry::open(data)?;
            Ok(list_lines(&registry, registry::in_use))
        }
        Verb::Create { name, dir } => {
            let entry = registry::create(data, &name, dir)?;
            let registry = Registry::open(data)?;
            let at = registry.dir_of(&entry);
            let mut lines = vec![if entry.is_managed() {
                format!("created {} at {}", entry.name, at.display())
            } else {
                format!(
                    "created {} at {} (not managed by blinkterm: remove forgets it \
                     and deletes nothing)",
                    entry.name,
                    at.display()
                )
            }];
            if registry.default.as_deref() == Some(entry.id.as_str()) {
                lines.push("it is the default profile".to_string());
            }
            Ok(lines)
        }
        Verb::Rename { from, to } => {
            let before = Registry::open(data)?
                .by_name(&from)
                .map(|e| e.name.clone())
                .unwrap_or(from.clone());
            let entry = registry::rename(data, &from, &to)?;
            Ok(vec![format!("renamed {before} to {}", entry.name)])
        }
        Verb::Default(name) => {
            let entry = registry::set_default(data, &name)?;
            Ok(vec![format!("{} is the default profile", entry.name)])
        }
        Verb::Remove(name) => Ok(removed_lines(&registry::remove(data, &name)?)),
    }
}

/// What a removal says: where the data went, and that there is no default
/// now when there is not.
fn removed_lines(removed: &Removed) -> Vec<String> {
    let (mut lines, was_default) = match removed {
        Removed::Trashed {
            name,
            to: Some(to),
            was_default,
        } => (
            vec![format!(
                "removed {name}; its cookies and logins are in {} until you delete that directory",
                to.display()
            )],
            *was_default,
        ),
        Removed::Trashed {
            name,
            to: None,
            was_default,
        } => (
            vec![format!(
                "removed {name}; it had never been used, so nothing was kept"
            )],
            *was_default,
        ),
        Removed::Forgotten {
            name,
            dir,
            was_default,
        } => (
            vec![format!(
                "forgot {name}; {} is left where it is",
                dir.display()
            )],
            *was_default,
        ),
    };
    if was_default {
        lines.push(
            "there is no default profile now; blinkterm profiles default <name> sets one, \
             and until then a start asks"
                .to_string(),
        );
    }
    lines
}

/// `blinkterm profiles list`: one line a profile, `*` before the default,
/// the names padded to the longest, then the whole directory and what else
/// is worth knowing. `in_use` is [`registry::in_use`], or a stand-in for a
/// test.
pub fn list_lines(
    registry: &Registry,
    in_use: impl Fn(&Path) -> Option<Option<u32>>,
) -> Vec<String> {
    if registry.profiles.is_empty() {
        return vec!["no profiles; blinkterm profiles create <name> makes one".to_string()];
    }
    let widest = registry
        .profiles
        .iter()
        .map(|e| screen::width(&text::sanitize(&e.name)))
        .max()
        .unwrap_or(0);
    registry
        .profiles
        .iter()
        .map(|entry| {
            let dir = registry.dir_of(entry);
            let mark = if registry.default.as_deref() == Some(entry.id.as_str()) {
                '*'
            } else {
                ' '
            };
            let mut line = format!("{mark} {}   {}", padded(&entry.name, widest), dir.display());
            if !entry.is_managed() {
                line.push_str("   not managed by blinkterm");
            }
            if let Some(holder) = in_use(&dir) {
                line.push_str("   ");
                line.push_str(&in_use_words(holder));
            }
            line
        })
        .collect()
}

/// `name`, plain, with spaces after it to `cells`.
fn padded(name: &str, cells: usize) -> String {
    let name = text::sanitize(name);
    let gap = cells.saturating_sub(screen::width(&name));
    format!("{name}{}", " ".repeat(gap))
}

fn in_use_words(holder: Option<u32>) -> String {
    match holder {
        Some(pid) => format!("in use (pid {pid})"),
        None => "in use".to_string(),
    }
}

/// The picker, on this process's stdin and stdout: see [`pick_with`].
pub fn pick(registry: Registry) -> Result<Selected, String> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    pick_with(registry, &mut input, &mut out)
}

/// The picker: the list, a prompt, and a line read; again until a profile
/// is chosen, or `q` or the end of the input says none will be.
///
/// `registry` is what the caller has already read; after every change the
/// file is read again, so the list drawn is always what is there.
pub fn pick_with(
    registry: Registry,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Selected, String> {
    let data = registry.data().to_path_buf();
    let mut registry = registry;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut redraw = true;
    loop {
        if redraw {
            write_out(out, &menu(&registry, home.as_deref(), registry::in_use))?;
        }
        redraw = true;
        write_out(out, "> ")?;
        let Some(line) = read_line(input)? else {
            write_out(out, "\n")?;
            return Err("no profile chosen".to_string());
        };
        let count = registry.profiles.len();
        let (word, rest) = match line.split_once(char::is_whitespace) {
            Some((word, rest)) => (word, rest.trim()),
            None => (line.as_str(), ""),
        };
        let numbered = |rest: &str| -> Result<usize, String> {
            match rest.parse::<usize>() {
                Ok(n) if (1..=count).contains(&n) => Ok(n - 1),
                _ if count == 0 => Err("? there are no profiles yet; n makes one".to_string()),
                _ => Err(format!("? a profile number, 1 to {count}")),
            }
        };
        let said: Result<(), String> = match (word, rest) {
            ("q", "") => return Err("no profile chosen".to_string()),
            ("n", "") => {
                let Some(name) = ask(input, out, "name: ")? else {
                    return Err("no profile chosen".to_string());
                };
                match registry::create(&data, &name, None) {
                    Ok(entry) => {
                        let yes = ask(
                            input,
                            out,
                            &format!("use {} by default? [y/N] ", entry.name),
                        )?;
                        if yes.as_deref().is_some_and(is_yes) {
                            registry::set_default(&data, &entry.name)?;
                        }
                        let registry = Registry::open(&data)?;
                        let entry = registry
                            .by_id(&entry.id)
                            .ok_or_else(|| format!("{} was removed meanwhile", entry.name))?;
                        return Ok(registry.selected(entry));
                    }
                    Err(why) => Err(why),
                }
            }
            ("d", rest) if !rest.is_empty() => numbered(rest).and_then(|at| {
                registry::set_default(&data, &registry.profiles[at].name).map(|_| ())
            }),
            ("r", rest) if !rest.is_empty() => match numbered(rest) {
                Ok(at) => {
                    let old = registry.profiles[at].name.clone();
                    match ask(
                        input,
                        out,
                        &format!("new name for {}: ", text::sanitize(&old)),
                    )? {
                        None => return Err("no profile chosen".to_string()),
                        Some(new) => registry::rename(&data, &old, &new).map(|_| ()),
                    }
                }
                Err(why) => Err(why),
            },
            ("x", rest) if !rest.is_empty() => match numbered(rest) {
                Ok(at) => {
                    let entry = registry.profiles[at].clone();
                    let name = text::sanitize(&entry.name).into_owned();
                    let question = if entry.is_managed() {
                        format!(
                            "remove {name}? its cookies and logins go to {} until you delete \
                             them [y/N] ",
                            shown(&registry.trash_dir(), home.as_deref())
                        )
                    } else {
                        format!(
                            "forget {name}? {} is left where it is [y/N] ",
                            shown(&entry.dir, home.as_deref())
                        )
                    };
                    match ask(input, out, &question)? {
                        Some(yes) if is_yes(&yes) => {
                            registry::remove(&data, &entry.name).map(|_| ())
                        }
                        Some(_) => Ok(()),
                        None => return Err("no profile chosen".to_string()),
                    }
                }
                Err(why) => Err(why),
            },
            (number, "") if number.parse::<usize>().is_ok() => match numbered(number) {
                Ok(at) => {
                    // What was drawn may be stale; the entry is looked up
                    // again by id so that the one opened is the one there.
                    let id = registry.profiles[at].id.clone();
                    let fresh = Registry::open(&data)?;
                    match fresh.by_id(&id) {
                        Some(entry) => return Ok(fresh.selected(entry)),
                        None => Err("? that profile was removed meanwhile".to_string()),
                    }
                }
                Err(why) => Err(why),
            },
            ("", "") => {
                redraw = false;
                Ok(())
            }
            _ if count == 0 => Err("? n or q".to_string()),
            _ => Err("? a number, n, d N, r N, x N or q".to_string()),
        };
        if let Err(why) = said {
            write_out(out, &format!("{}\n", text::sanitize(&why)))?;
            redraw = false;
        }
        registry = Registry::open(&data)?;
    }
}

/// The picker's screen: a title, the numbered list, and the keys.
fn menu(
    registry: &Registry,
    home: Option<&Path>,
    in_use: impl Fn(&Path) -> Option<Option<u32>>,
) -> String {
    let mut out = String::from("\nblinkterm profiles\n\n");
    if registry.profiles.is_empty() {
        out.push_str("  no profiles yet\n\nn makes a new one; q quits\n");
        return out;
    }
    let widest = registry
        .profiles
        .iter()
        .map(|e| screen::width(&text::sanitize(&e.name)))
        .max()
        .unwrap_or(0);
    let digits = registry.profiles.len().to_string().len();
    for (index, entry) in registry.profiles.iter().enumerate() {
        let place = if entry.is_managed() {
            "managed".to_string()
        } else {
            shown(&entry.dir, home)
        };
        let mut line = format!(
            "  {:>digits$}  {}   {place}",
            index + 1,
            padded(&entry.name, widest)
        );
        if registry.default.as_deref() == Some(entry.id.as_str()) {
            line.push_str("   (default)");
        }
        if let Some(holder) = in_use(&registry.dir_of(entry)) {
            line.push_str("   ");
            line.push_str(&in_use_words(holder));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str(
        "\na number opens that profile; n makes a new one; d N uses N by default;\n\
         r N renames N; x N removes N; q quits\n",
    );
    out
}

/// A path for a person: `$HOME` as `~`, plain text.
fn shown(path: &Path, home: Option<&Path>) -> String {
    let shortened = match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    };
    text::sanitize(&shortened).into_owned()
}

fn is_yes(answer: &str) -> bool {
    matches!(answer, "y" | "Y" | "yes" | "Yes" | "YES")
}

/// `prompt`, and the line answered, trimmed; `None` at the end of the input.
fn ask(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    prompt: &str,
) -> Result<Option<String>, String> {
    write_out(out, prompt)?;
    let line = read_line(input)?;
    if line.is_none() {
        write_out(out, "\n")?;
    }
    Ok(line)
}

/// One line, trimmed; `None` at the end of the input.
fn read_line(input: &mut dyn BufRead) -> Result<Option<String>, String> {
    let mut line = String::new();
    match input.read_line(&mut line) {
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(line.trim().to_string())),
        Err(e) => Err(format!("cannot read the answer: {e}")),
    }
}

fn write_out(out: &mut dyn Write, text: &str) -> Result<(), String> {
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|e| format!("cannot write the profile list: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    fn parsed(args: &[&str]) -> Result<Verb, String> {
        parse(&words(args))
    }

    /// A scratch data directory of this test's own, gone again before.
    fn scratch(what: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "blinkterm-unit-chooser-{what}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir.join("blinkterm")
    }

    /// The picker over `typed`, on a registry of Default and Work.
    fn picked(what: &str, typed: &str) -> (PathBuf, Result<Selected, String>, String) {
        let data = scratch(what);
        registry::create(&data, "Work", None).expect("made");
        let registry = Registry::open(&data).expect("read");
        let mut out = Vec::new();
        let chosen = pick_with(registry, &mut Cursor::new(typed.as_bytes()), &mut out);
        (data, chosen, String::from_utf8(out).expect("text"))
    }

    #[test]
    fn every_verb_is_read_and_every_mistake_named() {
        assert_eq!(parsed(&[]), Ok(Verb::List));
        assert_eq!(parsed(&["list"]), Ok(Verb::List));
        assert_eq!(parsed(&["help"]), Ok(Verb::Help));
        assert_eq!(parsed(&["-h"]), Ok(Verb::Help));
        assert_eq!(parsed(&["create", "--help"]), Ok(Verb::Help));
        assert_eq!(
            parsed(&["create", "Work"]),
            Ok(Verb::Create {
                name: "Work".into(),
                dir: None
            })
        );
        for args in [
            &["create", "Testing", "--dir", "/w"][..],
            &["create", "--dir=/w", "Testing"][..],
        ] {
            assert_eq!(
                parsed(args),
                Ok(Verb::Create {
                    name: "Testing".into(),
                    dir: Some("/w".into())
                }),
                "{args:?}"
            );
        }
        assert_eq!(
            parsed(&["rename", "Work", "Office"]),
            Ok(Verb::Rename {
                from: "Work".into(),
                to: "Office".into()
            })
        );
        assert_eq!(
            parsed(&["default", "Work"]),
            Ok(Verb::Default("Work".into()))
        );
        assert_eq!(parsed(&["remove", "Work"]), Ok(Verb::Remove("Work".into())));

        let refused = [
            (&["list", "x"][..], "profiles list takes nothing after it"),
            (&["create"][..], "profiles create needs a name"),
            (&["create", "a", "b"][..], "profiles create takes one name"),
            (
                &["create", "a", "--dir"][..],
                "profiles: --dir needs a directory",
            ),
            (
                &["create", "a", "--dir="][..],
                "profiles: --dir needs a directory",
            ),
            (
                &["create", "a", "--x"][..],
                "profiles create: unknown option --x",
            ),
            (
                &["rename", "a"][..],
                "profiles rename needs the old name and the new",
            ),
            (
                &["rename", "a", "b", "c"][..],
                "profiles rename takes the old name",
            ),
            (&["default"][..], "profiles default needs a name"),
            (&["remove", "a", "b"][..], "profiles remove takes one name"),
            (
                &["foo"][..],
                "profiles: unknown command \"foo\"; list, create",
            ),
        ];
        for (args, why) in refused {
            let said = parsed(args).unwrap_err();
            assert!(said.starts_with(why), "{args:?}: {said}");
        }
    }

    #[test]
    fn the_list_lines_up_the_names_and_marks_the_default_and_what_is_in_use() {
        let data = Path::new("/d");
        let text = "{\"version\": 1, \"default\": \"b\", \"profiles\": [\
            {\"id\": \"a\", \"name\": \"Default\", \"dir\": \"profile\"},\
            {\"id\": \"b\", \"name\": \"仕事\", \"dir\": \"profiles/b\"},\
            {\"id\": \"c\", \"name\": \"Testing\", \"dir\": \"/w\"}]}";
        let registry = Registry::parse(data, text).expect("read");
        let lines = list_lines(&registry, |dir| {
            (dir == Path::new("/w")).then_some(Some(42))
        });
        assert_eq!(
            lines,
            [
                "  Default   /d/profile",
                "* 仕事      /d/profiles/b",
                "  Testing   /w   not managed by blinkterm   in use (pid 42)",
            ]
        );
        let empty = Registry::parse(data, "{\"version\": 1, \"profiles\": []}").expect("read");
        assert_eq!(
            list_lines(&empty, |_| None),
            ["no profiles; blinkterm profiles create <name> makes one"]
        );
        let menu = menu(&registry, Some(Path::new("/")), |_| Some(None));
        assert!(menu.contains("  1  Default   managed   in use\n"), "{menu}");
        assert!(
            menu.contains("  2  仕事      managed   (default)   in use\n"),
            "{menu}"
        );
        assert!(menu.contains("  3  Testing   ~/w   in use\n"), "{menu}");
    }

    #[test]
    fn a_number_opens_that_profile() {
        let (data, chosen, out) = picked("number", "2\n");
        let chosen = chosen.expect("chosen");
        let registry = Registry::open(&data).expect("read");
        let work = registry.by_name("Work").expect("there");
        assert_eq!(chosen.dir, Some(registry.dir_of(work)));
        assert_eq!(chosen.label.as_deref(), Some("Work"));
        assert!(out.contains("  2  Work      managed\n"), "{out}");
        std::fs::remove_dir_all(data.parent().unwrap()).ok();
    }

    #[test]
    fn n_makes_a_profile_offers_it_as_the_default_and_opens_it() {
        let (data, chosen, out) = picked("new", "n\nTesting\ny\n");
        let chosen = chosen.expect("chosen");
        let registry = Registry::open(&data).expect("read");
        let testing = registry.by_name("Testing").expect("made");
        assert_eq!(registry.default.as_deref(), Some(testing.id.as_str()));
        assert_eq!(chosen.id.as_deref(), Some(testing.id.as_str()));
        assert!(out.contains("use Testing by default? [y/N] "), "{out}");
        std::fs::remove_dir_all(data.parent().unwrap()).ok();
    }

    #[test]
    fn a_bad_name_is_said_and_the_menu_goes_on() {
        let (data, chosen, out) = picked("bad-name", "n\na/b\n1\n");
        assert!(chosen.expect("chosen").dir.unwrap().ends_with("profile"));
        assert!(out.contains("cannot contain / or \\"), "{out}");
        std::fs::remove_dir_all(data.parent().unwrap()).ok();
    }

    #[test]
    fn d_sets_the_default_then_a_number_opens_one() {
        let (data, chosen, _) = picked("default", "d 2\n1\n");
        let registry = Registry::open(&data).expect("read");
        assert_eq!(
            registry.default_entry().map(|e| e.name.as_str()),
            Some("Work")
        );
        assert_eq!(chosen.expect("chosen").id.as_deref(), Some("default"));
        std::fs::remove_dir_all(data.parent().unwrap()).ok();
    }

    #[test]
    fn x_removes_after_asking_and_r_renames() {
        let (data, chosen, out) = picked("remove", "x 2\ny\n1\n");
        let registry = Registry::open(&data).expect("read");
        assert!(registry.by_name("Work").is_none());
        assert!(data.join(registry::TRASH_DIR).is_dir());
        let chosen = chosen.expect("chosen");
        assert_eq!(chosen.label, None, "one profile left: no name on the row");
        assert!(
            out.contains("remove Work? its cookies and logins go to"),
            "{out}"
        );
        std::fs::remove_dir_all(data.parent().unwrap()).ok();

        let (data, chosen, _) = picked("rename", "r 2\nOffice\n2\n");
        assert_eq!(chosen.expect("chosen").label.as_deref(), Some("Office"));
        std::fs::remove_dir_all(data.parent().unwrap()).ok();
    }

    #[test]
    fn q_the_end_of_the_input_and_nonsense_choose_nothing() {
        for typed in ["q\n", "", "what\n9\nq\n"] {
            let (data, chosen, out) = picked("quit", typed);
            assert_eq!(chosen, Err("no profile chosen".to_string()), "{typed:?}");
            if typed.starts_with("what") {
                assert!(out.contains("? a number, n, d N, r N, x N or q"), "{out}");
                assert!(out.contains("? a profile number, 1 to 2"), "{out}");
            }
            std::fs::remove_dir_all(data.parent().unwrap()).ok();
        }
    }

    #[test]
    fn with_every_profile_removed_only_n_and_q_are_offered() {
        let data = scratch("empty");
        Registry::open(&data).expect("migrated");
        registry::remove(&data, "Default").expect("removed");
        let registry = Registry::open(&data).expect("read");
        let mut out = Vec::new();
        let chosen = pick_with(
            registry,
            &mut Cursor::new(&b"1\nn\nFresh\n\n"[..]),
            &mut out,
        );
        let out = String::from_utf8(out).expect("text");
        assert!(out.contains("no profiles yet"), "{out}");
        assert!(
            out.contains("? there are no profiles yet; n makes one"),
            "{out}"
        );
        let chosen = chosen.expect("chosen");
        assert_eq!(chosen.label, None);
        let registry = Registry::open(&data).expect("read");
        assert_eq!(
            registry.default_entry().map(|e| e.name.as_str()),
            Some("Fresh"),
            "the first profile of a registry with no default becomes it"
        );
        std::fs::remove_dir_all(data.parent().unwrap()).ok();
    }
}
