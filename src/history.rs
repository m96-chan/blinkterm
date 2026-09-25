//! The pages visited, for the url bar to offer back.
//!
//! What is recorded is a url and the title it last had, how many times it
//! was visited and when last: once per page that landed and finished loading
//! without a problem — the moment the engine's `Page.loadEventFired` arrives,
//! which is the one moment the final url after redirects and the title are
//! both known. A page that did not come, an `about:blank` and a `data:` url
//! are not recorded; only `http`, `https` and `file` are
//! ([`History::records`]). What it is for is the url bar: Up and Down walk
//! it, and a url it knows is offered, dim, after what is being typed.
//!
//! It is kept in the profile, as `<profile>/history`, made readable by its
//! owner alone (0600), because a list of the pages somebody has visited is as
//! private as the cookies beside it. A temporary profile keeps none on disk:
//! [`History::in_memory`] is walkable for the run and gone with it, which is
//! what `--temp-profile` promises about everything else. Deleting the file is
//! how it is forgotten; nothing else reads it.
//!
//! It is an append-only log, one line per visit, rather than a database or a
//! file rewritten per visit. A crash in the middle of a write loses that one
//! line and never the file, a visit costs one `write(2)`, and nothing here
//! has to be careful about two writers, because a profile has one
//! `blinkterm` at a time ([`crate::profile`]'s lock). The log is folded on
//! load — the last line for a url is the one that counts — and when it has
//! grown to twice [`CAP`] lines, or ends in a line cut short, it is written
//! again with the newest [`CAP`] entries, to a new file renamed over the old,
//! so that even that cannot leave half a history.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::text;

/// Entries kept: a year of somebody's browsing is more than the url bar will
/// ever usefully offer, and a fold of this many lines is instant.
pub const CAP: usize = 2000;

/// Lines in the log at which it is compacted on load.
pub const COMPACT_AT: usize = 2 * CAP;

/// The file in the profile the log is kept in.
pub const FILE: &str = "history";

/// One page, as it was last seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub url: String,
    /// The last title it had that was not empty.
    pub title: String,
    /// How many times it has been visited.
    pub visits: u32,
    /// When it was last visited, in seconds since the epoch.
    pub last: u64,
}

impl Entry {
    /// The entry as one line of the log, newline included: four fields
    /// separated by tabs, the url and the title last because they are text.
    ///
    /// Neither can hold a tab or a newline. A url that could is not recorded
    /// ([`History::records`]), and the title is plain text by the time it
    /// gets here ([`text::sanitize`] makes every break a space).
    fn line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\n",
            self.last, self.visits, self.url, self.title
        )
    }
}

/// The pages visited, newest first, one entry per url.
#[derive(Debug)]
pub struct History {
    /// Newest first, one per url, at most [`CAP`].
    entries: Vec<Entry>,
    /// The log, or `None` for a temporary profile.
    path: Option<PathBuf>,
}

impl History {
    /// A history that is never written anywhere: a temporary profile's.
    pub fn in_memory() -> History {
        History {
            entries: Vec::new(),
            path: None,
        }
    }

    /// The history kept in the profile at `dir`, and where to add to it.
    ///
    /// A missing file is an empty history, and a line that does not parse is
    /// skipped, as is a last line with no newline after it — a write cut
    /// short. Nothing here fails: a history that cannot be read is not a
    /// reason not to browse. Compacted when it has grown to [`COMPACT_AT`]
    /// lines, or its last line was cut short, so that the next line appended
    /// starts a line of its own.
    pub fn load(dir: &Path) -> History {
        let path = dir.join(FILE);
        let log = std::fs::read(&path).unwrap_or_default();
        let log = String::from_utf8_lossy(&log);
        let mut lines = 0;
        // The newest line for each url, and when in the log it was, which is
        // the order: a clock that went backwards does not reorder the list.
        let mut newest: HashMap<String, (usize, Entry)> = HashMap::new();
        for line in log.split_inclusive('\n') {
            lines += 1;
            let Some(line) = line.strip_suffix('\n') else {
                continue;
            };
            let Some(entry) = History::parse_line(line) else {
                continue;
            };
            let visits = newest
                .get(&entry.url)
                .map_or(0, |(_, known)| known.visits)
                .max(entry.visits);
            let entry = Entry { visits, ..entry };
            newest.insert(entry.url.clone(), (lines, entry));
        }
        let mut entries: Vec<(usize, Entry)> = newest.into_values().collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        entries.truncate(CAP);
        let history = History {
            entries: entries.into_iter().map(|(_, entry)| entry).collect(),
            path: Some(path),
        };
        if lines >= COMPACT_AT || !(log.is_empty() || log.ends_with('\n')) {
            let _ = history.compact();
        }
        history
    }

    /// One line of the log, or `None` for one that is not: the pure half of
    /// [`History::load`].
    pub fn parse_line(line: &str) -> Option<Entry> {
        let mut fields = line.splitn(4, '\t');
        let last = fields.next()?.parse().ok()?;
        let visits = fields.next()?.parse().ok()?;
        let url = fields.next()?;
        let title = fields.next()?;
        if !History::records(url) {
            return None;
        }
        Some(Entry {
            url: url.to_string(),
            title: text::sanitize(title).into_owned(),
            visits,
            last,
        })
    }

    /// Record a visit to `url`, titled `title`, at `now`: the entry goes to
    /// the front with one more visit, and its title is replaced when the new
    /// one says anything. One line is appended to the log, if there is one.
    ///
    /// The error is for a test to see: the caller drops it, because a history
    /// that cannot be written is not a reason to stop browsing, and there is
    /// nowhere on the row to say so that would not cover something that
    /// matters more.
    pub fn visited(&mut self, url: &str, title: &str, now: u64) -> Result<(), String> {
        if !History::records(url) {
            return Ok(());
        }
        let title = text::sanitize(title);
        let title = title.trim();
        let mut entry = match self.entries.iter().position(|entry| entry.url == url) {
            Some(at) => self.entries.remove(at),
            None => Entry {
                url: url.to_string(),
                title: String::new(),
                visits: 0,
                last: now,
            },
        };
        entry.visits = entry.visits.saturating_add(1);
        entry.last = now;
        if !title.is_empty() {
            entry.title = title.to_string();
        }
        let line = entry.line();
        self.entries.insert(0, entry);
        self.entries.truncate(CAP);
        let Some(path) = &self.path else {
            return Ok(());
        };
        OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut file| file.write_all(line.as_bytes()))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// Write the entries as a fresh log, oldest first so that appending goes
    /// on in order, to a file beside the log that is then renamed over it.
    fn compact(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let fresh = path.with_extension("tmp");
        let log: String = self.entries.iter().rev().map(Entry::line).collect();
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&fresh)
            .and_then(|mut file| file.write_all(log.as_bytes()))
            .and_then(|()| std::fs::rename(&fresh, path))
            .map_err(|e| format!("cannot compact {}: {e}", path.display()))
    }

    /// Every page, newest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The rest of a url that starts with what has been typed, to be offered
    /// after it; or `None`.
    ///
    /// Matched on [`key`], so that `exa` finds `https://www.example.com/`;
    /// and what is offered is the url's own characters after the match, so
    /// that its case is what the page had. Of the urls that match and are
    /// longer than what is typed, the most visited, and of those the newest:
    /// a suggestion is a guess at where somebody is going, and where they go
    /// most is the best guess there is. Prefix only, because a suggestion is
    /// drawn as the rest of what is typed, and the rest of something that
    /// does not start the same way is not a rest.
    ///
    /// Nothing is offered for text with a space in it, since a url has none
    /// and the suggestion would be drawn after the space.
    pub fn complete(&self, typed: &str) -> Option<String> {
        if typed.is_empty() || typed.chars().any(char::is_whitespace) {
            return None;
        }
        let wanted = key(typed);
        if wanted.is_empty() {
            return None;
        }
        let best = self
            .entries
            .iter()
            .filter(|entry| {
                let known = key(&entry.url);
                known.len() > wanted.len() && known.starts_with(&wanted)
            })
            .fold(None::<&Entry>, |best, entry| match best {
                Some(best) if (best.visits, best.last) >= (entry.visits, entry.last) => Some(best),
                _ => Some(entry),
            })?;
        let from = prefix_len(&best.url) + wanted.len();
        best.url.get(from..).map(str::to_string)
    }

    /// The entries worth walking with Up for what has been typed, best first.
    ///
    /// Three tiers, newest first within each: urls that start with it, then
    /// urls that have it anywhere, then pages whose title has it — so that
    /// `docs` finds the docs page by name as well as by path. Nothing typed is
    /// every page, newest first: Up on an empty bar walks back through where
    /// you have been.
    pub fn matches(&self, typed: &str) -> Vec<&Entry> {
        let typed = typed.trim();
        if typed.is_empty() {
            return self.entries.iter().collect();
        }
        let wanted = key(typed);
        let words = typed.to_lowercase();
        let tiers: [&dyn Fn(&Entry) -> bool; 3] = [
            &|entry| key(&entry.url).starts_with(&wanted),
            &|entry| key(&entry.url).contains(&wanted),
            &|entry| entry.title.to_lowercase().contains(&words),
        ];
        let mut taken = vec![false; self.entries.len()];
        let mut out = Vec::new();
        for tier in tiers {
            for (index, entry) in self.entries.iter().enumerate() {
                if !taken[index] && tier(entry) {
                    taken[index] = true;
                    out.push(entry);
                }
            }
        }
        out
    }

    /// Whether a url is one to remember at all: a web page or a file, and
    /// nothing that could not be written as one field of one line.
    ///
    /// `about:`, `data:` and `chrome-error:` are not places anybody goes back
    /// to by typing, and a `data:` url can be megabytes.
    pub fn records(url: &str) -> bool {
        let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
        let Some(scheme) = ["http://", "https://", "file://"]
            .into_iter()
            .find(|scheme| lower.starts_with(scheme))
        else {
            return false;
        };
        url.len() > scheme.len() && !url.chars().any(|c| c.is_whitespace() || !text::is_plain(c))
    }
}

/// Up and Down through [`History::matches`], remembering what was typed so
/// that Down past the newest puts it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Walk {
    typed: String,
    urls: Vec<String>,
    /// Which of `urls` is in the bar, or `None` for what was typed.
    index: Option<usize>,
}

impl Walk {
    /// A walk through what matches `typed`, standing on `typed` itself.
    ///
    /// The matches are taken now and kept, so that the list does not change
    /// under the walk as the line it is walking changes the text.
    pub fn new(history: &History, typed: &str) -> Walk {
        Walk {
            typed: typed.to_string(),
            urls: history
                .matches(typed)
                .into_iter()
                .map(|entry| entry.url.clone())
                .collect(),
            index: None,
        }
    }

    /// One older, or `None` at the oldest, where the walk stays.
    pub fn up(&mut self) -> Option<&str> {
        let next = self.index.map_or(0, |index| index + 1);
        let url = self.urls.get(next)?;
        self.index = Some(next);
        Some(url)
    }

    /// One newer; past the newest, what was typed; `None` when that is
    /// already where the walk is.
    pub fn down(&mut self) -> Option<&str> {
        match self.index? {
            0 => {
                self.index = None;
                Some(&self.typed)
            }
            index => {
                self.index = Some(index - 1);
                self.urls.get(index - 1).map(String::as_str)
            }
        }
    }
}

/// The part of a url that is matched against: lower-cased, with `https://`
/// and a leading `www.` taken off.
///
/// `http://` is kept. What is typed without a scheme is sent as `https://`
/// ([`crate::app::normalise`]), so a completion that matched an `http://`
/// site on its host would send the person to it over the wrong scheme — to
/// a port that may not answer. An `http://` site is offered once `http` is
/// typed, and walked to with Up either way.
///
/// Only ASCII is lowered, so that every byte offset in the key is one in the
/// url after `prefix_len` bytes, which is how a completion is cut from the
/// url's own characters.
pub fn key(url: &str) -> String {
    url[prefix_len(url)..].to_ascii_lowercase()
}

/// How many bytes of `url` [`key`] leaves off the front.
fn prefix_len(url: &str) -> usize {
    let starts = |at: usize, prefix: &str| {
        url.get(at..at + prefix.len())
            .is_some_and(|there| there.eq_ignore_ascii_case(prefix))
    };
    let mut at = 0;
    if starts(0, "https://") {
        at = "https://".len();
    }
    if starts(at, "www.") {
        at += "www.".len();
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A scratch directory of this test's own, gone again before and after.
    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blinkterm-history-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn urls(history: &History) -> Vec<&str> {
        history
            .entries()
            .iter()
            .map(|entry| entry.url.as_str())
            .collect()
    }

    fn visit(history: &mut History, url: &str, title: &str, now: u64) {
        history.visited(url, title, now).expect("recorded");
    }

    #[test]
    fn a_visit_moves_the_page_to_the_front_and_counts_it() {
        let mut history = History::in_memory();
        visit(&mut history, "https://a.example/", "A", 1);
        visit(&mut history, "https://b.example/", "B", 2);
        assert_eq!(urls(&history), ["https://b.example/", "https://a.example/"]);
        visit(&mut history, "https://a.example/", "A", 3);
        assert_eq!(urls(&history), ["https://a.example/", "https://b.example/"]);
        let a = &history.entries()[0];
        assert_eq!((a.visits, a.last), (2, 3));
    }

    #[test]
    fn a_title_that_is_empty_does_not_replace_the_one_known() {
        let mut history = History::in_memory();
        visit(&mut history, "https://a.example/", "The page", 1);
        visit(&mut history, "https://a.example/", "", 2);
        assert_eq!(history.entries()[0].title, "The page");
        visit(
            &mut history,
            "https://a.example/",
            "Renamed\t\x1b]0;x\x07",
            3,
        );
        assert_eq!(history.entries()[0].title, "Renamed ]0;x");
    }

    #[test]
    fn the_list_is_cut_at_the_cap() {
        let mut history = History::in_memory();
        for n in 0..CAP + 10 {
            visit(
                &mut history,
                &format!("https://example.com/{n}"),
                "",
                n as u64,
            );
        }
        assert_eq!(history.entries().len(), CAP);
        assert_eq!(
            history.entries()[0].url,
            format!("https://example.com/{}", CAP + 9)
        );
        assert_eq!(
            history.entries()[CAP - 1].url,
            "https://example.com/10",
            "the oldest went"
        );
    }

    #[test]
    fn a_line_round_trips_and_a_broken_one_is_skipped() {
        let entry = Entry {
            url: "https://example.com/a?b=c".to_string(),
            title: "A page — with a title".to_string(),
            visits: 3,
            last: 1_700_000_000,
        };
        let line = entry.line();
        assert!(line.ends_with('\n'));
        assert_eq!(
            History::parse_line(line.trim_end_matches('\n')),
            Some(entry)
        );
        // An empty title is a title.
        assert_eq!(
            History::parse_line("1\t1\thttps://x.example/\t").map(|e| e.title),
            Some(String::new())
        );
        for broken in [
            "",
            "garbage",
            "1\t1\thttps://x.example/",
            "x\t1\thttps://x.example/\tT",
            "1\t-1\thttps://x.example/\tT",
            "1\t1\tabout:blank\tT",
            "1\t1\thttps://x.example/ y\tT",
        ] {
            assert_eq!(History::parse_line(broken), None, "{broken:?}");
        }
    }

    #[test]
    fn the_log_is_appended_to_and_folded_on_load() {
        let dir = scratch("fold");
        let mut history = History::load(&dir);
        assert!(history.entries().is_empty(), "no file is no history");
        visit(&mut history, "https://a.example/", "A", 1);
        visit(&mut history, "https://b.example/", "B", 2);
        visit(&mut history, "https://a.example/", "", 3);
        let log = std::fs::read_to_string(dir.join(FILE)).expect("a log");
        assert_eq!(log.lines().count(), 3, "one line a visit: {log:?}");

        let again = History::load(&dir);
        assert_eq!(again.entries(), history.entries());
        assert_eq!(again.entries()[0].title, "A", "the title kept from before");
        assert_eq!(again.entries()[0].visits, 2);

        // A line cut short by a crash is not read, and the log is written
        // again so that the next visit starts a line of its own.
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join(FILE))
            .expect("the log");
        file.write_all(b"4\t1\thttps://c.exa").expect("written");
        drop(file);
        let mut again = History::load(&dir);
        assert_eq!(urls(&again), ["https://a.example/", "https://b.example/"]);
        visit(&mut again, "https://c.example/", "C", 5);
        let reread = History::load(&dir);
        assert_eq!(
            urls(&reread),
            [
                "https://c.example/",
                "https://a.example/",
                "https://b.example/"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_log_is_compacted_on_load() {
        let dir = scratch("compact");
        let mut log = String::new();
        for n in 0..COMPACT_AT {
            // Two lines for every url: half as many entries as lines.
            let entry = Entry {
                url: format!("https://example.com/{}", n / 2),
                title: String::new(),
                visits: (n % 2 + 1) as u32,
                last: n as u64,
            };
            log.push_str(&entry.line());
        }
        std::fs::write(dir.join(FILE), &log).expect("a log");
        let history = History::load(&dir);
        assert_eq!(history.entries().len(), CAP);
        assert_eq!(
            history.entries()[0].url,
            format!("https://example.com/{}", COMPACT_AT / 2 - 1)
        );
        assert_eq!(history.entries()[0].visits, 2);
        let written = std::fs::read_to_string(dir.join(FILE)).expect("a log");
        assert_eq!(written.lines().count(), CAP, "one line an entry now");
        assert!(!dir.join("history.tmp").exists());
        assert_eq!(History::load(&dir).entries(), history.entries());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_is_readable_by_its_owner_alone() {
        let dir = scratch("mode");
        let mut history = History::load(&dir);
        visit(&mut history, "https://example.com/", "", 1);
        let mode = std::fs::metadata(dir.join(FILE))
            .expect("the log")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "a list of pages visited nobody else may read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_in_memory_history_writes_nothing() {
        let mut history = History::in_memory();
        visit(&mut history, "https://example.com/", "", 1);
        assert!(history.path.is_none());
        assert_eq!(history.entries().len(), 1);
    }

    #[test]
    fn a_completion_is_the_rest_of_the_most_visited_matching_url() {
        let mut history = History::in_memory();
        visit(&mut history, "https://example.com/", "", 1);
        for now in 2..7 {
            visit(&mut history, "https://www.Example.org/docs", "", now);
        }
        visit(&mut history, "https://exact.example/", "", 8);
        // Five visits beat one, even an older five.
        assert_eq!(history.complete("exa"), Some("mple.org/docs".to_string()));
        // `www.` and `https://` are not what anybody types, and the case is
        // the url's own.
        assert_eq!(history.complete("Exam"), Some("ple.org/docs".to_string()));
        assert_eq!(
            history.complete("https://www.exam"),
            Some("ple.org/docs".to_string())
        );
        assert_eq!(history.complete("example.c"), Some("om/".to_string()));
        // Ties go to the newest: one visit each, and the later one wins.
        visit(&mut history, "https://example.co.uk/", "", 9);
        assert_eq!(history.complete("example.c"), Some("o.uk/".to_string()));
        // Nothing for a url typed out in full, nothing typed, a space, or a
        // host nobody has visited.
        assert_eq!(history.complete("example.com/"), None);
        assert_eq!(history.complete(""), None);
        assert_eq!(history.complete("exa mple"), None);
        assert_eq!(history.complete("nowhere"), None);

        // An http site is not offered for a bare host, which would go to it
        // over https, but is once http is typed.
        let mut history = History::in_memory();
        visit(&mut history, "http://plain.example/", "", 1);
        assert_eq!(history.complete("pla"), None);
        assert_eq!(
            history.complete("http://pla"),
            Some("in.example/".to_string())
        );
    }

    #[test]
    fn matches_rank_prefix_then_substring_then_title() {
        let mut history = History::in_memory();
        visit(&mut history, "https://docs.example/", "Home", 1);
        visit(&mut history, "https://example.com/rust", "Guide", 2);
        visit(&mut history, "https://example.org/", "The Rust book", 3);
        visit(&mut history, "https://rust.example/", "", 4);
        let found: Vec<&str> = history
            .matches("rust")
            .iter()
            .map(|entry| entry.url.as_str())
            .collect();
        assert_eq!(
            found,
            [
                "https://rust.example/",
                "https://example.com/rust",
                "https://example.org/",
            ]
        );
        assert_eq!(history.matches("").len(), 4, "nothing typed is everything");
        assert_eq!(history.matches("  ")[0].url, "https://rust.example/");
    }

    #[test]
    fn a_walk_goes_up_through_matches_and_down_back_to_what_was_typed() {
        let mut history = History::in_memory();
        visit(&mut history, "https://a.example/", "", 1);
        visit(&mut history, "https://b.example/", "", 2);
        let mut walk = Walk::new(&history, "");
        assert_eq!(walk.down(), None, "nothing newer than what was typed");
        assert_eq!(walk.up(), Some("https://b.example/"));
        assert_eq!(walk.up(), Some("https://a.example/"));
        assert_eq!(walk.up(), None, "the oldest is where it stays");
        assert_eq!(walk.down(), Some("https://b.example/"));
        assert_eq!(walk.down(), Some(""), "what was typed, which was nothing");
        assert_eq!(walk.down(), None);

        let mut walk = Walk::new(&history, "a.ex");
        assert_eq!(walk.up(), Some("https://a.example/"));
        assert_eq!(walk.up(), None);
        assert_eq!(walk.down(), Some("a.ex"));
    }

    #[test]
    fn only_web_and_file_pages_are_recorded() {
        for url in [
            "https://example.com/",
            "http://localhost:3000/",
            "file:///etc/hostname",
            "HTTPS://EXAMPLE.COM/",
        ] {
            assert!(History::records(url), "{url}");
        }
        for url in [
            "about:blank",
            "data:text/html,hi",
            "chrome-error://chromewebdata/",
            "https://",
            "https://a b/",
            "https://a\tb/",
            "https://a\u{202e}b/",
            "",
        ] {
            assert!(!History::records(url), "{url:?}");
        }
        let mut history = History::in_memory();
        visit(&mut history, "about:blank", "", 1);
        assert!(history.entries().is_empty());
    }
}
