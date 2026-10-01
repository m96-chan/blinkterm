//! The pages the person asked to keep.
//!
//! A bookmark here is a url the person asked, with `ctrl+d`, to keep, and the
//! title the page had when they asked. It is offered back by the url bar
//! before any page merely visited: the dim suggestion after what is typed is
//! a bookmark's when one starts the same way, and Up walks the bookmarks that
//! match before the history ([`crate::history::Walk`]).
//!
//! # A file a profile
//!
//! The file is `<profile>/bookmarks`, beside the history and the saved tabs:
//! the work profile's bookmarks are the work profile's, and a profile made
//! to keep two identities apart does not hand one's pages to the other. A
//! temporary profile keeps them in memory ([`Bookmarks::in_memory`]) — a
//! `ctrl+d` there works for the run, the row says it is for the run only,
//! and nothing is written, which is what `--temp-profile` promises.
//!
//! # The file from before profiles
//!
//! Every blinkterm before this one kept one file for every profile,
//! `$XDG_DATA_HOME/blinkterm/bookmarks` (or
//! `~/.local/share/blinkterm/bookmarks`), and the first start on the
//! registry's `Default` profile — the directory those versions used,
//! `<data>/profile` — copies it in ([`migrate_legacy`]). Once: the copy
//! leaves `bookmarks.migrated` beside the old file, and a start that finds
//! it, or finds the profile already has bookmarks, copies nothing. The old
//! file is left where it is, so that an older blinkterm run afterwards still
//! has its bookmarks; what either adds after that is its own. The copy is
//! made under the old file's lock, `<data>/bookmarks.lock`, which an older
//! blinkterm takes for every change it makes, so the copy is never of a file
//! half rewritten. Any other profile starts with none: a `--profile <dir>`
//! or a named profile that should have them gets them with a `cp` of the old
//! file into its directory.
//!
//! # A line per bookmark
//!
//! ```text
//! https://example.com/<TAB>Example Domain
//! # a comment, kept
//! https://a.example/notitle
//! ```
//!
//! `url<TAB>title`, the title optional, readable by its owner alone (0600: a
//! list of pages, like the history). It is a plain file on purpose: `grep`
//! over it shows what is bookmarked, an editor is the bookmark manager, and a
//! hand edit is the whole truth — there are no tombstones to read past. A
//! line that is not a bookmark — a `# comment`, a blank, a mistake — is kept
//! exactly as it is across every change this program makes, as are the
//! bookmarks it does not touch. A url is matched exactly, which is Chrome's
//! rule too: `https://example.com/` and `https://example.com` are two pages.
//!
//! Titles are plain text on one line: [`crate::text::sanitize`] on the way in
//! from the page and again on the way in from the file, since a hand-edited
//! or hostile file reaches the row by the same path a title does.
//!
//! # Two writers
//!
//! The profile's lock keeps a second blinkterm off the profile, and so off
//! its bookmarks; but a person's editor, a script, or the one-time copy
//! above can write the file while a run has it, and an append racing a
//! rename would land on the inode the rename just replaced, and be lost. So
//! every change holds `bookmarks.lock` ([`crate::profile::hold`], a blocking
//! `flock`) and reads the file again under it before changing it, which also
//! means a removal never drops a line somebody else added since this run
//! read it. The lock is on a separate file that is never renamed over, since
//! a lock on the bookmarks file itself would be a lock on an inode the next
//! rename replaces. A hold lasts one read and one write, tens of
//! microseconds.
//!
//! What is not done is watching the file for another writer's additions:
//! they appear in this run's completions after the next `ctrl+d` here, which
//! reads the file again, or at the next start.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::history::{self, History};
use crate::profile::Profile;
use crate::{profile, registry, text};

/// The file, in the profile: `<profile>/bookmarks`. The file every profile
/// shared before has the same name, in the data directory.
pub const FILE: &str = "bookmarks";

/// The lock file beside it, never renamed over — see [`Bookmarks::toggle`].
pub const LOCK: &str = "bookmarks.lock";

/// Beside the old shared file once it has been copied into the `Default`
/// profile: see [`migrate_legacy`].
pub const MIGRATED: &str = "bookmarks.migrated";

/// One page kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    /// Plain text, one line; may be empty.
    pub title: String,
}

/// One line of the file, as it is kept across rewrites.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    /// A bookmark, and the line it was read from, which is what is written
    /// back: a change to one bookmark leaves every other line byte for byte.
    Mark { mark: Bookmark, line: String },
    /// A line that is not a bookmark — a comment, a blank, a mistake —
    /// written back exactly as it was read.
    Other(String),
}

/// The bookmarks, in file order, and where they are kept.
#[derive(Debug)]
pub struct Bookmarks {
    items: Vec<Item>,
    /// The file, or `None` for a temporary profile, in which case `ctrl+d`
    /// works for the run and the row says so.
    path: Option<PathBuf>,
}

/// What [`Bookmarks::toggle`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggled {
    Added,
    Removed,
}

impl Bookmarks {
    /// Bookmarks kept nowhere but in this run.
    pub fn in_memory() -> Bookmarks {
        Bookmarks {
            items: Vec::new(),
            path: None,
        }
    }

    /// The bookmarks of `profile`: its file, read now, or for a temporary
    /// profile none, kept in memory.
    pub fn for_profile(profile: &Profile) -> Bookmarks {
        if profile.is_temporary() {
            Bookmarks::in_memory()
        } else {
            Bookmarks::load(profile.dir())
        }
    }

    /// `<dir>/bookmarks`, read now. A missing file is no bookmarks, and so
    /// is a file that cannot be read: never an error, since a list that
    /// cannot be read is not a reason not to browse.
    pub fn load(dir: &Path) -> Bookmarks {
        let path = dir.join(FILE);
        Bookmarks {
            items: read(&path).1,
            path: Some(path),
        }
    }

    /// Where the bookmarks are kept, or `None` for this run only.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// One line, into a bookmark, or `None` for a line that is not one:
    /// the pure half of [`Bookmarks::load`].
    ///
    /// Split on the first tab. The url, trimmed of spaces and a `\r` — so
    /// that a file written on another system reads the same — must be one
    /// [`Bookmarks::keeps`]; the title is sanitized and trimmed, and may be
    /// empty or absent.
    pub fn parse_line(line: &str) -> Option<Bookmark> {
        let (url, title) = line.split_once('\t').unwrap_or((line, ""));
        let url = url.trim_matches([' ', '\r']);
        if !Bookmarks::keeps(url) {
            return None;
        }
        Some(Bookmark {
            url: url.to_string(),
            title: text::sanitize(title).trim().to_string(),
        })
    }

    /// Whether `url` is one that can be kept at all: the history's rule
    /// ([`History::records`]), which is also what makes a url one field of
    /// one line.
    pub fn keeps(url: &str) -> bool {
        History::records(url)
    }

    /// Every bookmark, in file order; a url that appears twice appears once,
    /// where its last line is and with that line's title.
    pub fn all(&self) -> Vec<&Bookmark> {
        let marks: Vec<&Bookmark> = self
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Mark { mark, .. } => Some(mark),
                Item::Other(_) => None,
            })
            .collect();
        marks
            .iter()
            .enumerate()
            .filter(|(at, mark)| !marks[at + 1..].iter().any(|later| later.url == mark.url))
            .map(|(_, mark)| *mark)
            .collect()
    }

    /// Whether `url`, exactly, is bookmarked.
    pub fn has(&self, url: &str) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, Item::Mark { mark, .. } if mark.url == url))
    }

    /// Add the page, or remove it if it is there, and say which.
    ///
    /// Under [`LOCK`] the file is read again first, so that what is added or
    /// removed is added to or removed from what is *there*, not from what
    /// this run last saw; afterwards `self` is what the file now says. An add
    /// appends one line; a removal writes the file again without that url's
    /// lines, to `bookmarks.tmp` renamed over it, every other line as it was.
    /// `Err` is the lock or the write failing, with the change made in memory
    /// all the same: the caller says so on the row.
    pub fn toggle(&mut self, url: &str, title: &str) -> Result<Toggled, String> {
        let Some(path) = self.path.clone() else {
            return Ok(self.apply(url, title));
        };
        let held = match profile::hold(&path.with_file_name(LOCK)) {
            Ok(held) => held,
            Err(why) => {
                self.apply(url, title);
                return Err(why);
            }
        };
        let (was, items) = read(&path);
        self.items = items;
        let toggled = self.apply(url, title);
        let written = match toggled {
            Toggled::Added => {
                let Some(Item::Mark { line, .. }) = self.items.last() else {
                    unreachable!("an add pushes a bookmark")
                };
                // A file that ends in a line with no newline — a hand edit —
                // gets one first, or the bookmark would join that line.
                let gap = if was.is_empty() || was.ends_with('\n') {
                    ""
                } else {
                    "\n"
                };
                OpenOptions::new()
                    .append(true)
                    .create(true)
                    .mode(0o600)
                    .open(&path)
                    .and_then(|mut file| file.write_all(format!("{gap}{line}\n").as_bytes()))
            }
            Toggled::Removed => {
                let fresh = path.with_extension("tmp");
                let whole: String = self
                    .items
                    .iter()
                    .map(|item| match item {
                        Item::Mark { line, .. } | Item::Other(line) => format!("{line}\n"),
                    })
                    .collect();
                OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&fresh)
                    .and_then(|mut file| file.write_all(whole.as_bytes()))
                    .and_then(|()| std::fs::rename(&fresh, &path))
            }
        };
        drop(held);
        written
            .map(|()| toggled)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// The toggle itself, on the list in memory.
    fn apply(&mut self, url: &str, title: &str) -> Toggled {
        if self.has(url) {
            self.items
                .retain(|item| !matches!(item, Item::Mark { mark, .. } if mark.url == url));
            return Toggled::Removed;
        }
        let title = text::sanitize(title).trim().to_string();
        let line = if title.is_empty() {
            url.to_string()
        } else {
            format!("{url}\t{title}")
        };
        self.items.push(Item::Mark {
            mark: Bookmark {
                url: url.to_string(),
                title,
            },
            line,
        });
        Toggled::Added
    }

    /// The rest of the newest bookmark whose [`history::key`] starts with what
    /// was typed and is longer, to be offered after it; or `None`.
    ///
    /// The same rule as [`History::complete`], with "newest" for "most
    /// visited", since a bookmark has no count: `exa` finds
    /// `https://www.example.com/`, the rest is the url's own characters, and
    /// an `http://` bookmark is not offered for a bare host, which would go
    /// to it over `https://`.
    pub fn complete(&self, typed: &str) -> Option<String> {
        if typed.is_empty() || typed.chars().any(char::is_whitespace) {
            return None;
        }
        let wanted = history::key(typed);
        if wanted.is_empty() {
            return None;
        }
        let best = self.all().into_iter().rev().find(|mark| {
            let known = history::key(&mark.url);
            known.len() > wanted.len() && known.starts_with(&wanted)
        })?;
        let from = history::prefix_len(&best.url) + wanted.len();
        best.url.get(from..).map(str::to_string)
    }

    /// The bookmarks worth walking with Up for what has been typed:
    /// [`history::tiers`], newest first within a tier.
    pub fn matches(&self, typed: &str) -> Vec<&Bookmark> {
        let newest_first: Vec<&Bookmark> = self.all().into_iter().rev().collect();
        history::tiers(&newest_first, |m| &m.url, |m| &m.title, typed)
    }
}

/// Copy the bookmarks file every profile shared before profiles had their
/// own into the profile at `profile`, once, and say so; see the module's
/// doc. `data` is the data directory, `$XDG_DATA_HOME/blinkterm`.
///
/// Only for the registry's `Default` profile at its old place,
/// `<data>/profile` ([`registry::LEGACY_DIR`]) — the profile whose
/// bookmarks the old file was as far as anybody using one profile could
/// tell — and only when that profile has no bookmarks file of its own, the
/// old file is there, and no `bookmarks.migrated` says it was copied
/// before. The checks are made again under the old file's lock, which is
/// held for the copy; the copy is written to `bookmarks.tmp` and renamed
/// into place, 0600, so a crash leaves no half a file for the next start
/// to take as the profile's own; then the marker is written. The old file
/// is not touched.
///
/// `None` when there was nothing to do, and quietly when the old file holds
/// no bookmark (its comments are copied all the same); else the sentence for
/// the row — what was copied, or why it could not be, in which case nothing
/// is marked and the next start tries again.
pub fn migrate_legacy(data: &Path, profile: &Path) -> Option<String> {
    let default = data.join(registry::LEGACY_DIR);
    let same = profile == default
        || matches!(
            (profile.canonicalize(), default.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        );
    let legacy = data.join(FILE);
    let marker = data.join(MIGRATED);
    let target = profile.join(FILE);
    let wanted = || !marker.exists() && !target.exists() && legacy.exists();
    if !same || !wanted() {
        return None;
    }
    let failed = |why: String| {
        Some(format!(
            "the bookmarks were not copied into this profile: {why}"
        ))
    };
    let held = match profile::hold(&data.join(LOCK)) {
        Ok(held) => held,
        Err(why) => return failed(why),
    };
    if !wanted() {
        return None;
    }
    let bytes = match std::fs::read(&legacy) {
        Ok(bytes) => bytes,
        Err(e) => return failed(format!("cannot read {}: {e}", legacy.display())),
    };
    let fresh = target.with_extension("tmp");
    let copied = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&fresh)
        .and_then(|mut file| file.write_all(&bytes))
        .and_then(|()| std::fs::rename(&fresh, &target));
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&fresh);
        return failed(format!("cannot write {}: {e}", target.display()));
    }
    // The copy is the profile's own file now, which is what stops a second
    // copy; the marker is what stops one after the person empties or
    // removes it. A marker that cannot be written is not worth a word.
    let _ = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&marker)
        .and_then(|mut file| file.write_all(format!("1\t{}\n", profile.display()).as_bytes()));
    drop(held);
    let marks = read(&target)
        .1
        .iter()
        .filter(|item| matches!(item, Item::Mark { .. }))
        .count();
    match marks {
        0 => None,
        1 => Some(format!(
            "1 bookmark copied into this profile from {}",
            legacy.display()
        )),
        n => Some(format!(
            "{n} bookmarks copied into this profile from {}",
            legacy.display()
        )),
    }
}

/// The file at `path` as text, and as the lines it is kept as. Nothing there
/// and nothing readable are both no lines.
fn read(path: &Path) -> (String, Vec<Item>) {
    let bytes = std::fs::read(path).unwrap_or_default();
    let whole = String::from_utf8_lossy(&bytes).into_owned();
    let items = whole
        .split_terminator('\n')
        .map(|line| match Bookmarks::parse_line(line) {
            Some(mark) => Item::Mark {
                mark,
                line: line.to_string(),
            },
            None => Item::Other(line.to_string()),
        })
        .collect();
    (whole, items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    /// A scratch directory of this test's own, gone again before and after.
    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blinkterm-bookmarks-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn urls(marks: &Bookmarks) -> Vec<&str> {
        marks.all().into_iter().map(|m| m.url.as_str()).collect()
    }

    fn mark(url: &str, title: &str) -> Option<Bookmark> {
        Some(Bookmark {
            url: url.to_string(),
            title: title.to_string(),
        })
    }

    #[test]
    fn a_line_is_a_url_and_maybe_a_title_and_anything_else_is_kept_as_it_is() {
        assert_eq!(
            Bookmarks::parse_line("https://example.com/\tExample Domain"),
            mark("https://example.com/", "Example Domain")
        );
        assert_eq!(
            Bookmarks::parse_line("https://a.example/notitle"),
            mark("https://a.example/notitle", "")
        );
        assert_eq!(
            Bookmarks::parse_line("https://a.example/\r"),
            mark("https://a.example/", ""),
            "a CRLF line with no title"
        );
        assert_eq!(
            Bookmarks::parse_line("https://a.example/\tA\r"),
            mark("https://a.example/", "A"),
            "a CRLF line with a title"
        );
        assert_eq!(
            Bookmarks::parse_line("https://a.example/\t\x1b]0;x\x07 a\tb "),
            mark("https://a.example/", "]0;x a b"),
            "a title is plain text on one line"
        );
        for other in ["# a comment", "", "about:blank\tx", "not a url", "  "] {
            assert_eq!(Bookmarks::parse_line(other), None, "{other:?}");
        }

        let dir = scratch("parse");
        let file = "# mine\r\nhttps://a.example/\tA\r\n\nnonsense\nhttps://b.example/";
        std::fs::write(dir.join(FILE), file).expect("a file");
        let marks = Bookmarks::load(&dir);
        assert_eq!(urls(&marks), ["https://a.example/", "https://b.example/"]);
        assert_eq!(
            marks
                .items
                .iter()
                .filter(|i| matches!(i, Item::Other(_)))
                .count(),
            3
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bookmark_toggles_on_and_off_and_the_row_is_told_which() {
        let dir = scratch("toggle");
        let mut marks = Bookmarks::load(&dir);
        assert_eq!(
            marks.toggle("https://example.com/", "Example"),
            Ok(Toggled::Added)
        );
        assert!(marks.has("https://example.com/"));
        assert_eq!(
            marks.toggle("https://example.com/", "Example"),
            Ok(Toggled::Removed)
        );
        assert!(!marks.has("https://example.com/"));
        assert!(Bookmarks::load(&dir).all().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adding_appends_one_line_and_removing_rewrites_without_it_and_keeps_the_comments() {
        let dir = scratch("rewrite");
        let before = "# mine\r\nhttps://a.example/\tA\r\n\njunk here\nhttps://b.example/";
        std::fs::write(dir.join(FILE), before).expect("a file");
        let mut marks = Bookmarks::load(&dir);
        marks.toggle("https://c.example/", "C\tsee").expect("added");
        let file = std::fs::read_to_string(dir.join(FILE)).expect("the file");
        assert_eq!(file, format!("{before}\nhttps://c.example/\tC see\n"));

        marks.toggle("https://b.example/", "").expect("removed");
        let file = std::fs::read_to_string(dir.join(FILE)).expect("the file");
        assert_eq!(
            file,
            "# mine\r\nhttps://a.example/\tA\r\n\njunk here\nhttps://c.example/\tC see\n"
        );
        assert!(!dir.join("bookmarks.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_url_is_matched_exactly_so_a_trailing_slash_is_another_page() {
        let mut marks = Bookmarks::in_memory();
        marks.toggle("https://example.com/", "").expect("added");
        assert!(!marks.has("https://example.com"));
        assert_eq!(
            marks.toggle("https://example.com", ""),
            Ok(Toggled::Added),
            "another page"
        );
        assert_eq!(marks.all().len(), 2);
    }

    #[test]
    fn the_file_is_readable_by_its_owner_alone() {
        let dir = scratch("mode");
        let mut marks = Bookmarks::load(&dir);
        marks.toggle("https://example.com/", "").expect("added");
        for name in [FILE, LOCK] {
            let mode = std::fs::metadata(dir.join(name))
                .expect("the file")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{name}");
        }
        marks.toggle("https://other.example/", "").expect("added");
        marks.toggle("https://example.com/", "").expect("removed");
        let mode = std::fs::metadata(dir.join(FILE))
            .expect("the file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "and after a rewrite");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_change_reads_the_file_again_first_so_two_runs_do_not_lose_each_others_bookmarks() {
        let dir = scratch("two");
        std::fs::write(dir.join(FILE), "https://y.example/\tY\n").expect("a file");
        let mut a = Bookmarks::load(&dir);
        let mut b = Bookmarks::load(&dir);
        a.toggle("https://x.example/", "X").expect("A adds X");
        // B never saw X, and removes Y.
        assert!(!b.has("https://x.example/"));
        assert_eq!(b.toggle("https://y.example/", ""), Ok(Toggled::Removed));
        let file = std::fs::read_to_string(dir.join(FILE)).expect("the file");
        assert_eq!(file, "https://x.example/\tX\n");
        assert_eq!(urls(&b), ["https://x.example/"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_bookmarks_holds_the_lock_only_for_the_change() {
        let dir = scratch("lock");
        let mut marks = Bookmarks::load(&dir);
        let held = profile::hold(&dir.join(LOCK)).expect("the test holds it");
        let (tx, rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            tx.send(Instant::now()).expect("listening");
            drop(held);
        });
        let started = Instant::now();
        marks.toggle("https://example.com/", "").expect("added");
        let done = Instant::now();
        let let_go = rx.recv().expect("the holder let go");
        holder.join().expect("the holder finishes");
        assert!(done >= let_go, "the toggle did not wait for the lock");
        assert!(done - started < Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_profile_has_its_own_bookmarks_and_a_temporary_one_keeps_them_for_the_run() {
        let root = scratch("profiles");
        let work = Profile::take_at(root.join("work"), None).expect("work");
        let home = Profile::take_at(root.join("home"), None).expect("home");
        let mut marks = Bookmarks::for_profile(&work);
        assert_eq!(marks.path(), Some(root.join("work").join(FILE).as_path()));
        marks.toggle("https://work.example/", "W").expect("added");
        assert_eq!(
            urls(&Bookmarks::for_profile(&work)),
            ["https://work.example/"],
            "read again from the profile"
        );
        assert!(
            Bookmarks::for_profile(&home).all().is_empty(),
            "another profile has none of them"
        );

        let temporary = Profile::temporary().expect("a temporary profile");
        let mut marks = Bookmarks::for_profile(&temporary);
        assert_eq!(marks.path(), None, "the row says it is for this run");
        assert_eq!(marks.toggle("https://t.example/", ""), Ok(Toggled::Added));
        assert!(!temporary.dir().join(FILE).exists(), "nothing written");
        drop(temporary);
        drop((work, home));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A data directory with the old shared file in it and the `Default`
    /// profile's directory made, as the first start after upgrading finds it.
    fn upgraded(what: &str, old: &str) -> (PathBuf, PathBuf) {
        let data = scratch(what);
        std::fs::write(data.join(FILE), old).expect("the old file");
        let profile = data.join(registry::LEGACY_DIR);
        std::fs::create_dir_all(&profile).expect("the default profile");
        (data, profile)
    }

    #[test]
    fn the_old_shared_file_is_copied_into_the_default_profile_once_and_kept() {
        let old = "# mine\nhttps://a.example/\tA\nhttps://b.example/\n";
        let (data, profile) = upgraded("migrate", old);
        let said = migrate_legacy(&data, &profile).expect("said on the row");
        assert_eq!(
            said,
            format!(
                "2 bookmarks copied into this profile from {}",
                data.join(FILE).display()
            )
        );
        assert_eq!(
            std::fs::read_to_string(profile.join(FILE)).expect("copied"),
            old,
            "byte for byte, comments and all"
        );
        assert_eq!(
            std::fs::read_to_string(data.join(FILE)).expect("kept"),
            old,
            "the old file is left for an older blinkterm"
        );
        assert_eq!(
            std::fs::read_to_string(data.join(MIGRATED)).expect("a marker"),
            format!("1\t{}\n", profile.display())
        );
        assert!(!profile.join("bookmarks.tmp").exists());
        for path in [profile.join(FILE), data.join(MIGRATED)] {
            let mode = std::fs::metadata(&path)
                .expect("there")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{}", path.display());
        }
        assert_eq!(
            urls(&Bookmarks::load(&profile)),
            ["https://a.example/", "https://b.example/"]
        );

        // Once: not again, and not after the person removes the copy.
        assert_eq!(migrate_legacy(&data, &profile), None);
        std::fs::remove_file(profile.join(FILE)).expect("removed by hand");
        assert_eq!(migrate_legacy(&data, &profile), None, "the marker says so");
        assert!(!profile.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn the_old_file_is_not_copied_into_any_other_profile_nor_over_bookmarks_already_there() {
        let old = "https://a.example/\tA\n";
        let (data, default) = upgraded("not-default", old);
        let managed = data.join(registry::MANAGED_DIR).join("3f9a1c0b7e2d");
        let elsewhere = scratch("not-default-elsewhere");
        for profile in [&managed, &elsewhere] {
            std::fs::create_dir_all(profile).expect("a profile");
            assert_eq!(
                migrate_legacy(&data, profile),
                None,
                "{}",
                profile.display()
            );
            assert!(!profile.join(FILE).exists());
        }
        assert!(!data.join(MIGRATED).exists(), "nothing marked");

        let mine = "https://mine.example/\n";
        std::fs::write(default.join(FILE), mine).expect("the profile's own");
        assert_eq!(migrate_legacy(&data, &default), None);
        assert_eq!(
            std::fs::read_to_string(default.join(FILE)).expect("kept"),
            mine,
            "never written over"
        );

        // A data directory with no old file has nothing to copy.
        let fresh = scratch("not-default-fresh");
        let profile = fresh.join(registry::LEGACY_DIR);
        std::fs::create_dir_all(&profile).expect("the default profile");
        assert_eq!(migrate_legacy(&fresh, &profile), None);
        assert!(!profile.join(FILE).exists());
        assert!(!fresh.join(MIGRATED).exists());

        // An old file of comments alone is copied, and nothing is said.
        let (quiet, profile) = upgraded("not-default-quiet", "# nothing yet\n");
        assert_eq!(migrate_legacy(&quiet, &profile), None);
        assert!(profile.join(FILE).exists());
        assert!(quiet.join(MIGRATED).exists());
        for dir in [data, elsewhere, fresh, quiet] {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn the_copy_waits_for_an_older_blinkterm_holding_the_old_files_lock() {
        let (data, profile) = upgraded("migrate-lock", "https://a.example/\n");
        let held = profile::hold(&data.join(LOCK)).expect("the older one holds it");
        let (tx, rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            tx.send(Instant::now()).expect("listening");
            drop(held);
        });
        assert!(migrate_legacy(&data, &profile).is_some());
        let done = Instant::now();
        let let_go = rx.recv().expect("the holder let go");
        holder.join().expect("the holder finishes");
        assert!(done >= let_go, "the copy did not wait for the lock");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn an_in_memory_bookmarks_writes_nothing_and_says_so() {
        let mut marks = Bookmarks::in_memory();
        assert_eq!(marks.path(), None, "the caller says so on the row");
        assert_eq!(marks.toggle("https://a.example/", "A"), Ok(Toggled::Added));
        assert_eq!(urls(&marks), ["https://a.example/"]);
        assert_eq!(
            marks.toggle("https://a.example/", "A"),
            Ok(Toggled::Removed)
        );
    }

    #[test]
    fn a_completion_is_the_rest_of_the_newest_bookmark_that_starts_the_same_way() {
        let mut marks = Bookmarks::in_memory();
        marks
            .toggle("https://www.Example.org/docs", "")
            .expect("added");
        marks.toggle("https://example.com/", "").expect("added");
        assert_eq!(marks.complete("exa"), Some("mple.com/".to_string()));
        assert_eq!(marks.complete("Example.o"), Some("rg/docs".to_string()));
        assert_eq!(
            marks.complete("https://www.exam"),
            Some("ple.com/".to_string())
        );
        assert_eq!(marks.complete("example.com/"), None);
        assert_eq!(marks.complete(""), None);
        assert_eq!(marks.complete("ex a"), None);

        let mut marks = Bookmarks::in_memory();
        marks.toggle("http://plain.example/", "").expect("added");
        assert_eq!(marks.complete("pla"), None);
        assert_eq!(
            marks.complete("http://pla"),
            Some("in.example/".to_string())
        );
    }

    #[test]
    fn matches_rank_prefix_then_substring_then_title_newest_first() {
        let mut marks = Bookmarks::in_memory();
        for (url, title) in [
            ("https://docs.example/", "Home"),
            ("https://example.com/rust", "Guide"),
            ("https://example.org/", "The Rust book"),
            ("https://rust.example/", ""),
            ("https://rust-lang.example/", ""),
        ] {
            marks.toggle(url, title).expect("added");
        }
        let found: Vec<&str> = marks
            .matches("rust")
            .into_iter()
            .map(|m| m.url.as_str())
            .collect();
        assert_eq!(
            found,
            [
                "https://rust-lang.example/",
                "https://rust.example/",
                "https://example.com/rust",
                "https://example.org/",
            ]
        );
        assert_eq!(marks.matches("")[0].url, "https://rust-lang.example/");
    }
}
