//! The history list: every page visited, newest first, filtered by typing,
//! one picked to open here or in a new tab, or to forget.
//!
//! What the url bar's completion cannot do: it helps somebody who remembers
//! how a url starts, and this is for somebody who remembers a word of a
//! title from last week. It is the tab list's shape — the same rows under
//! the row, the same pick, the same keys, all of it [`crate::list`] — over a
//! different set of rows: `history: ` as the prompt, each row `  when  title
//! —  url`, and the count at the right.
//!
//! The rows are a copy of [`History::entries`](crate::history::History::entries)
//! taken when the list opens, not the file read again: the history in memory
//! is already folded one entry per url and kept current by every visit, and
//! a copy means a page that finishes loading behind the list does not move
//! the rows under the pick. Each row keeps its title and url lowercased
//! together, once, so that a key is a pass of substring tests and nothing
//! else — at [`crate::history::CAP`] entries that is nothing a person can
//! see, and a test here times ten thousand. If the cap ever grows by orders
//! of magnitude, the next step is to narrow the last matches when a filter
//! only grew, rather than to test every row again.
//!
//! A filter is words, in any order: a row matches when every word is in its
//! title or its url, whatever the case. Newest first and nothing else — a
//! ranking by how well a row matches would reorder the rows under the pick
//! with every key, and the newest match is usually the one wanted anyway.
//!
//! Three keys are the list's own, taken before the filter's line sees them,
//! because [`Line`] would read them as something else: `alt+enter` and
//! `ctrl+enter` open the pick in a new tab (Enter with any modifier is `Go`
//! to the line), and `shift+delete` forgets it (a delete forward to the
//! line). Two for a new tab because each is taken by a terminal somewhere —
//! WezTerm keeps `alt+enter` for full screen, Ghostty on Linux keeps
//! `ctrl+enter` — and a middle click on a row does the same, for a terminal
//! that sends neither. `ctrl+shift+delete` forgets too, since some keyboards
//! and terminals only give shift+delete with it.

use crate::history;
use crate::input::{Key, KeyAction, KeyInput};
use crate::line::Line;
use crate::list::{self, List, Window};
use crate::text;

/// One page, as the list copied it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    url: String,
    title: String,
    last: u64,
    /// The title and the url, lowercased, one line each: what a filter's
    /// words are looked for in. Sanitized as the row is drawn, so that a word
    /// never matches letters the person cannot see.
    haystack: String,
}

/// One page as the list shows it: what [`HistoryList::entry`] hands back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub url: &'a str,
    /// The last title the page had that said anything; empty when none did.
    pub title: &'a str,
    /// When it was last visited, in seconds since the epoch.
    pub last: u64,
}

/// What a key did to the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Still open; the row and the rows are out of date.
    Typing,
    /// Escape, or Enter with nothing matching: closed, nothing opened.
    Close,
    /// Open `url`: here, or in a new tab in front.
    Open { url: String, new_tab: bool },
    /// Forget `url`: the caller takes it out of the history, and then out of
    /// the list with [`HistoryList::forgotten`]. The list stays open, so that
    /// forgetting several is several keys.
    Forget(String),
    /// `ctrl+q`.
    Quit,
}

/// The list while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryList {
    list: List,
    /// Newest first, as the history had them when the list opened, less any
    /// forgotten since.
    rows: Vec<Row>,
    /// Said instead of the count until the next key: what went wrong
    /// forgetting a page, which is otherwise nowhere to be seen while the
    /// list has the screen.
    pub notice: Option<String>,
}

impl HistoryList {
    /// Open over `entries`, newest first, with the newest picked and nothing
    /// typed. Nothing is read from the file: see the module's docs.
    pub fn open(entries: &[history::Entry]) -> HistoryList {
        let rows = entries
            .iter()
            .map(|entry| Row {
                haystack: format!(
                    "{}\n{}",
                    text::sanitize(&entry.title).to_lowercase(),
                    text::sanitize(&entry.url).to_lowercase()
                ),
                url: entry.url.clone(),
                title: entry.title.clone(),
                last: entry.last,
            })
            .collect();
        HistoryList {
            list: List::open(0),
            rows,
            notice: None,
        }
    }

    /// The filter being typed.
    pub fn line(&self) -> &Line {
        &self.list.line
    }

    /// The filter being typed, to paste into.
    pub fn line_mut(&mut self) -> &mut Line {
        &mut self.list.line
    }

    /// How many pages the list has, matching or not.
    pub fn total(&self) -> usize {
        self.rows.len()
    }

    /// The pages that match the filter, newest first, as indices for
    /// [`HistoryList::entry`]. An empty filter matches every page.
    pub fn matches(&self) -> Vec<usize> {
        let words = words(self.list.line.text());
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| has_words(&row.haystack, &words))
            .map(|(index, _)| index)
            .collect()
    }

    /// The page at `index`, one of [`HistoryList::matches`].
    pub fn entry(&self, index: usize) -> Entry<'_> {
        let row = &self.rows[index];
        Entry {
            url: &row.url,
            title: &row.title,
            last: row.last,
        }
    }

    /// Which match is picked, clamped to the `matches` there are; `None`
    /// for none.
    pub fn picked(&self, matches: usize) -> Option<usize> {
        self.list.picked(matches)
    }

    /// Which matches fit in `rows`, keeping the pick in view. See
    /// [`List::window`].
    pub fn window(&self, count: usize, rows: usize, first: usize) -> Window {
        self.list.window(count, rows, first)
    }

    /// What the right-hand end of the row says: the notice while there is
    /// one, else `12/2000`.
    pub fn count_text(&self, matched: usize) -> String {
        match &self.notice {
            Some(notice) => notice.clone(),
            None => List::count_text(matched, self.total()),
        }
    }

    /// One key, with `page` how many rows the list has on the screen.
    ///
    /// The list's own three chords are looked at before the filter's line is
    /// (see the module's docs for why they have to be); everything else is
    /// [`List::step`]'s, with a pick turned into the page to open here.
    pub fn step(&mut self, key: &KeyInput, page: usize) -> Step {
        if key.action == KeyAction::Release {
            return Step::Typing;
        }
        self.notice = None;
        let matched = self.matches();
        let pick = self.picked(matched.len()).map(|at| matched[at]);
        match key.key {
            Key::Enter if key.mods.alt() || key.mods.ctrl() => {
                return match pick {
                    Some(index) => self.opening(index, true),
                    None => Step::Close,
                };
            }
            Key::Delete if key.mods.shift() => {
                return match pick {
                    Some(index) => Step::Forget(self.rows[index].url.clone()),
                    None => Step::Typing,
                };
            }
            _ => {}
        }
        match self.list.step(key, matched.len(), page) {
            list::Step::Typing => Step::Typing,
            list::Step::Close | list::Step::Pick(None) => Step::Close,
            list::Step::Pick(Some(at)) => self.opening(matched[at], false),
            list::Step::Quit => Step::Quit,
        }
    }

    /// A click on the `row`th visible row (from zero), with `shown` the
    /// window that was drawn: open the page on it — in a new tab when it was
    /// the `middle` button — or nothing for a row past the end.
    pub fn click(&mut self, row: usize, shown: &Window, middle: bool) -> Step {
        let matched = self.matches();
        match self.list.click(row, shown, matched.len()) {
            list::Step::Pick(Some(at)) => self.opening(matched[at], middle),
            _ => Step::Typing,
        }
    }

    /// `url` was forgotten: its row goes. The pick stays where it was, which
    /// puts it on the row after, so that forgetting a run of pages is the
    /// same key again and again; clamped, as always, when that was the last.
    pub fn forgotten(&mut self, url: &str) {
        self.rows.retain(|row| row.url != url);
    }

    /// Open the page at `index`, here or in a new tab.
    fn opening(&self, index: usize, new_tab: bool) -> Step {
        Step::Open {
            url: self.rows[index].url.clone(),
            new_tab,
        }
    }
}

/// A filter as the words it is made of, lowercased: split on any run of
/// whitespace, so that two spaces are not an empty word that matches all.
pub fn words(filter: &str) -> Vec<String> {
    filter.split_whitespace().map(str::to_lowercase).collect()
}

/// Whether every one of `words` is somewhere in `haystack`, in any order.
/// No words is a match.
pub fn has_words(haystack: &str, words: &[String]) -> bool {
    words.iter().all(|word| haystack.contains(word.as_str()))
}

/// When `then` was, from `now`, in words: `just now`, `5 minutes ago`,
/// `1 day ago`, down to `2 years ago`.
///
/// Arithmetic on seconds and nothing else: no calendar and no time zone,
/// since "3 days ago" is the same whatever the zone and the list only has
/// to say roughly when. A month is thirty days. A `then` after `now` — a
/// clock that went backwards since — is `just now`, which is the least
/// wrong thing it can be.
pub fn ago(then: u64, now: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;
    const MONTH: u64 = 30 * DAY;
    const YEAR: u64 = 365 * DAY;
    let since = now.saturating_sub(then);
    let (count, unit) = if since < MINUTE {
        return "just now".to_string();
    } else if since < HOUR {
        (since / MINUTE, "minute")
    } else if since < DAY {
        (since / HOUR, "hour")
    } else if since < WEEK {
        (since / DAY, "day")
    } else if since < MONTH {
        (since / WEEK, "week")
    } else if since < YEAR {
        (since / MONTH, "month")
    } else {
        (since / YEAR, "year")
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural} ago")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Mods;

    fn entry(url: &str, title: &str, last: u64) -> history::Entry {
        history::Entry {
            url: url.to_string(),
            title: title.to_string(),
            visits: 1,
            last,
        }
    }

    /// Newest first, as [`history::History::entries`] has them.
    fn four() -> Vec<history::Entry> {
        vec![
            entry("https://ci.example/build/4471", "Build log", 400),
            entry("https://mail.example/inbox", "Inbox — Mail", 300),
            entry("https://docs.example/changelog", "Changelog", 200),
            entry(
                "https://news.example/rust-release",
                "Rust 1.87 released",
                100,
            ),
        ]
    }

    fn key(k: Key, mods: u32) -> KeyInput {
        KeyInput {
            key: k,
            mods: Mods(mods),
            action: KeyAction::Press,
            text: None,
        }
    }

    fn typed(c: char) -> KeyInput {
        KeyInput {
            key: Key::Char(c),
            mods: Mods::default(),
            action: KeyAction::Press,
            text: Some(c),
        }
    }

    fn type_in(list: &mut HistoryList, text: &str) {
        for c in text.chars() {
            assert_eq!(list.step(&typed(c), 5), Step::Typing);
        }
    }

    fn urls(list: &HistoryList) -> Vec<&str> {
        list.matches()
            .into_iter()
            .map(|index| list.entry(index).url)
            .collect()
    }

    fn open(url: &str, new_tab: bool) -> Step {
        Step::Open {
            url: url.to_string(),
            new_tab,
        }
    }

    #[test]
    fn the_list_opens_newest_first_with_the_newest_picked_and_shows_every_page_untyped() {
        let list = HistoryList::open(&four());
        assert_eq!(list.line().text(), "");
        assert_eq!(list.total(), 4);
        assert_eq!(list.matches(), [0, 1, 2, 3]);
        assert_eq!(list.picked(4), Some(0));
        assert_eq!(
            list.entry(1),
            Entry {
                url: "https://mail.example/inbox",
                title: "Inbox — Mail",
                last: 300
            }
        );
        assert_eq!(list.count_text(4), "4/4");
    }

    #[test]
    fn typing_narrows_by_title_and_url_with_the_words_in_any_order_whatever_their_case() {
        for (filter, wanted) in [
            (
                "log",
                vec![
                    "https://ci.example/build/4471",
                    "https://docs.example/changelog",
                ],
            ),
            (
                "LOG",
                vec![
                    "https://ci.example/build/4471",
                    "https://docs.example/changelog",
                ],
            ),
            ("build log", vec!["https://ci.example/build/4471"]),
            ("log  build", vec!["https://ci.example/build/4471"]),
            ("docs.ex", vec!["https://docs.example/changelog"]),
            ("released news", vec!["https://news.example/rust-release"]),
            // A word from the title and a word from the url is a match too.
            ("inbox mail.example", vec!["https://mail.example/inbox"]),
        ] {
            let mut list = HistoryList::open(&four());
            type_in(&mut list, filter);
            assert_eq!(urls(&list), wanted, "{filter:?}");
        }
    }

    #[test]
    fn a_word_nobody_visited_matches_nothing_and_enter_then_closes() {
        let mut list = HistoryList::open(&four());
        type_in(&mut list, "log zebra");
        assert!(list.matches().is_empty());
        assert_eq!(list.count_text(0), "0/4");
        assert_eq!(list.step(&key(Key::Enter, 0), 5), Step::Close);
        assert_eq!(list.step(&key(Key::Enter, Mods::ALT), 5), Step::Close);
        assert_eq!(
            list.step(&key(Key::Delete, Mods::SHIFT), 5),
            Step::Typing,
            "nothing to forget"
        );
        assert_eq!(list.step(&key(Key::Escape, 0), 5), Step::Close);
        assert_eq!(list.step(&key(Key::Char('q'), Mods::CTRL), 5), Step::Quit);
    }

    #[test]
    fn enter_opens_the_pick_here_and_alt_enter_or_ctrl_enter_in_a_new_tab() {
        let mut list = HistoryList::open(&four());
        type_in(&mut list, "log");
        list.step(&key(Key::Down, 0), 5);
        let changelog = "https://docs.example/changelog";
        assert_eq!(list.step(&key(Key::Enter, 0), 5), open(changelog, false));
        assert_eq!(
            list.step(&key(Key::Enter, Mods::ALT), 5),
            open(changelog, true)
        );
        assert_eq!(
            list.step(&key(Key::Enter, Mods::CTRL), 5),
            open(changelog, true)
        );
        // Shift alone is still Enter: here.
        assert_eq!(
            list.step(&key(Key::Enter, Mods::SHIFT), 5),
            open(changelog, false)
        );
        // A release is nothing at all.
        let mut release = key(Key::Enter, Mods::ALT);
        release.action = KeyAction::Release;
        assert_eq!(list.step(&release, 5), Step::Typing);
    }

    #[test]
    fn shift_delete_and_ctrl_shift_delete_forget_the_pick_and_the_pick_stays_on_the_next_row() {
        let mut list = HistoryList::open(&four());
        list.step(&key(Key::Down, 0), 5);
        let mail = "https://mail.example/inbox";
        assert_eq!(
            list.step(&key(Key::Delete, Mods::SHIFT), 5),
            Step::Forget(mail.to_string())
        );
        // Until the caller says it went, the row is there.
        assert_eq!(list.total(), 4);
        list.forgotten(mail);
        assert_eq!(list.total(), 3);
        assert_eq!(
            urls(&list),
            [
                "https://ci.example/build/4471",
                "https://docs.example/changelog",
                "https://news.example/rust-release"
            ]
        );
        assert_eq!(list.picked(3), Some(1), "on the row that was after it");
        assert_eq!(
            list.step(&key(Key::Delete, Mods::SHIFT | Mods::CTRL), 5),
            Step::Forget("https://docs.example/changelog".to_string())
        );
        list.forgotten("https://docs.example/changelog");
        list.forgotten("https://news.example/rust-release");
        assert_eq!(list.picked(1), Some(0), "the last row gone: clamped");

        // A plain Delete is still the filter's.
        let mut list = HistoryList::open(&four());
        type_in(&mut list, "logx");
        list.step(&key(Key::Left, 0), 5);
        assert_eq!(list.step(&key(Key::Delete, 0), 5), Step::Typing);
        assert_eq!(list.line().text(), "log");
    }

    #[test]
    fn a_middle_click_opens_the_row_in_a_new_tab_and_a_left_one_here() {
        let mut list = HistoryList::open(&four());
        let shown = list.window(4, 3, 0);
        assert_eq!(shown, Window { first: 0, shown: 3 });
        assert_eq!(
            list.click(2, &shown, false),
            open("https://docs.example/changelog", false)
        );
        assert_eq!(list.picked(4), Some(2));
        assert_eq!(
            list.click(1, &shown, true),
            open("https://mail.example/inbox", true)
        );
        assert_eq!(list.click(3, &shown, false), Step::Typing, "past the end");
    }

    #[test]
    fn a_notice_replaces_the_count_until_the_next_key() {
        let mut list = HistoryList::open(&four());
        list.notice = Some("forgotten, but not from the file".to_string());
        assert_eq!(list.count_text(4), "forgotten, but not from the file");
        // A release is not a key yet.
        let mut release = typed('x');
        release.action = KeyAction::Release;
        list.step(&release, 5);
        assert!(list.notice.is_some());
        list.step(&key(Key::Down, 0), 5);
        assert_eq!(list.count_text(4), "4/4");
    }

    #[test]
    fn a_relative_date_reads_as_words() {
        const DAY: u64 = 86_400;
        let now = 1_000_000_000;
        for (since, words) in [
            (0, "just now"),
            (59, "just now"),
            (60, "1 minute ago"),
            (61, "1 minute ago"),
            (300, "5 minutes ago"),
            (3599, "59 minutes ago"),
            (3600, "1 hour ago"),
            (7200, "2 hours ago"),
            (86_399, "23 hours ago"),
            (DAY, "1 day ago"),
            (6 * DAY, "6 days ago"),
            (7 * DAY, "1 week ago"),
            (29 * DAY, "4 weeks ago"),
            (30 * DAY, "1 month ago"),
            (364 * DAY, "12 months ago"),
            (365 * DAY, "1 year ago"),
            (2 * 365 * DAY, "2 years ago"),
        ] {
            assert_eq!(ago(now - since, now), words, "{since} seconds");
        }
        assert_eq!(ago(now + 500, now), "just now", "a clock gone backwards");
    }

    #[test]
    fn ten_thousand_rows_filter_in_one_pass_and_the_pick_is_the_newest_match() {
        let entries: Vec<history::Entry> = (0..10_000u64)
            .rev()
            .map(|n| {
                entry(
                    &format!("https://site{}.example/page/{n}", n % 97),
                    &format!("Article number {n} about topic {}", n % 13),
                    n,
                )
            })
            .collect();
        let mut list = HistoryList::open(&entries);
        assert_eq!(list.matches().len(), 10_000);
        type_in(&mut list, "Topic 7 site5.");
        let fits = |url: &str, title: &str| {
            url.contains("site5.")
                && title.contains("topic")
                && (title.contains('7') || url.contains('7'))
        };
        let matched = list.matches();
        let wanted = entries.iter().filter(|e| fits(&e.url, &e.title)).count();
        assert!(wanted > 0);
        assert_eq!(matched.len(), wanted);
        for &index in &matched {
            let entry = list.entry(index);
            assert!(fits(entry.url, entry.title), "{entry:?}");
        }
        // Newest first, so the first match is the newest that fits.
        let newest = entries
            .iter()
            .find(|e| fits(&e.url, &e.title))
            .expect("one fits");
        assert_eq!(list.picked(matched.len()), Some(0));
        assert_eq!(list.entry(matched[0]).url, newest.url);
    }

    #[test]
    fn words_split_on_any_whitespace_and_lowercase() {
        assert_eq!(words("  Build\tLOG  x "), ["build", "log", "x"]);
        assert!(words("   ").is_empty());
        assert!(has_words("anything", &[]));
        assert!(has_words("build log\nhttps://ci", &words("CI log")));
        assert!(!has_words("build log\nhttps://ci", &words("ci mail")));
    }

    #[test]
    fn a_hostile_title_is_matched_by_what_the_row_shows() {
        let entries = [entry("https://evil.example/", "\x1b]0;x\x07log", 1)];
        for filter in ["xlog", "]0;"] {
            let mut list = HistoryList::open(&entries);
            type_in(&mut list, filter);
            assert_eq!(list.matches(), [0], "{filter:?}");
        }
    }
}
