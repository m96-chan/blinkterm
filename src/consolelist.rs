//! The console panel: what the page in front logged, threw and failed to
//! fetch, newest last, filtered by typing.
//!
//! It is the tab list's shape — the same rows under the row, the same pick,
//! the same keys, all of it [`crate::list`] — over a different set of rows:
//! `console: ` as the prompt, each row `level  text — url:line`, and the
//! count at the right, or `nothing logged` when there is nothing to count.
//! The level is the row's first word, padded to one column, and not a
//! colour: the list's rows are drawn without colour on purpose
//! ([`crate::screen::list_rows`]), and a word can be filtered on — `error`
//! narrows the list to the errors.
//!
//! The rows are a copy of [`Recorder::entries`](crate::console::Recorder::entries)
//! taken when the panel opens, as the history list copies the history: a
//! page that logs every ten milliseconds would otherwise move the rows under
//! the pick with every entry, and would make every key a copy of up to a
//! thousand entries. Close it and open it again to see what came since.
//!
//! Newest last, with the pick on the newest when it opens, because a console
//! reads downward and the newest is where the eye goes first; it is the one
//! list here that is not newest first. A filter is words in any order, as in
//! the history list, looked for in the level, the text and the place.
//!
//! `enter` closes, as `esc` does: a row is nothing to open. A click moves
//! the pick and nothing else.

use crate::console::Entry;
use crate::historylist::{has_words, words};
use crate::input::{KeyAction, KeyInput};
use crate::line::Line;
use crate::list::{self, List, Window};
use crate::text;

/// What [`ConsoleList::count_text`] says for a console with nothing in it.
pub const NOTHING: &str = "nothing logged";

/// What `ctrl+shift+j` says with `console = false` or `--no-console`.
pub const OFF: &str = "the console is off (console = false)";

/// One entry, as the panel copied it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    entry: Entry,
    /// `lead text place`, sanitized and lowercased once: what a filter's
    /// words are looked for in, which is what the row shows.
    haystack: String,
}

/// What a key did to the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Still open; the row and the rows are out of date.
    Typing,
    /// Escape or Enter: closed.
    Close,
    /// `ctrl+q`.
    Quit,
}

/// The panel while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleList {
    list: List,
    /// Oldest first, as the recorder had them when the panel opened.
    rows: Vec<Row>,
}

impl ConsoleList {
    /// Open over `entries`, oldest first, with the newest picked and
    /// nothing typed.
    pub fn open(entries: Vec<Entry>) -> ConsoleList {
        let rows: Vec<Row> = entries
            .into_iter()
            .map(|entry| Row {
                haystack: text::sanitize(&format!(
                    "{} {} {}",
                    entry.lead(),
                    entry.text,
                    entry.place
                ))
                .to_lowercase(),
                entry,
            })
            .collect();
        ConsoleList {
            list: List::open(rows.len().saturating_sub(1)),
            rows,
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

    /// How many entries the panel has, matching or not.
    pub fn total(&self) -> usize {
        self.rows.len()
    }

    /// The entries that match the filter, oldest first, as indices for
    /// [`ConsoleList::entry`]. An empty filter matches every entry.
    pub fn matches(&self) -> Vec<usize> {
        let words = words(self.list.line.text());
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| has_words(&row.haystack, &words))
            .map(|(index, _)| index)
            .collect()
    }

    /// The entry at `index`, one of [`ConsoleList::matches`].
    pub fn entry(&self, index: usize) -> &Entry {
        &self.rows[index].entry
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

    /// What the right-hand end of the row says: [`NOTHING`] for an empty
    /// console, else `12/340`.
    pub fn count_text(&self, matched: usize) -> String {
        if self.total() == 0 {
            NOTHING.to_string()
        } else {
            List::count_text(matched, self.total())
        }
    }

    /// One key, with `page` how many rows the panel has on the screen:
    /// [`List::step`]'s, with a pick turned into closing.
    pub fn step(&mut self, key: &KeyInput, page: usize) -> Step {
        if key.action == KeyAction::Release {
            return Step::Typing;
        }
        match self.list.step(key, self.matches().len(), page) {
            list::Step::Typing => Step::Typing,
            list::Step::Close | list::Step::Pick(_) => Step::Close,
            list::Step::Quit => Step::Quit,
        }
    }

    /// A click on the `row`th visible row (from zero), with `shown` the
    /// window that was drawn: the pick moves to it, and the panel stays.
    pub fn click(&mut self, row: usize, shown: &Window) -> Step {
        self.list.click(row, shown, self.matches().len());
        Step::Typing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::{Level, Source};
    use crate::input::{Key, Mods};

    fn entry(level: Level, text: &str, place: &str) -> Entry {
        Entry {
            level,
            source: Source::Console,
            text: text.to_string(),
            place: place.to_string(),
        }
    }

    /// Oldest first, as the recorder has them.
    fn four() -> Vec<Entry> {
        vec![
            entry(Level::Log, "booting app", "https://a.example/app.js:3"),
            entry(
                Level::Error,
                "Failed to load resource: the server responded with a status of 404 (Not Found)",
                "https://a.example/logo.png",
            ),
            entry(
                Level::Warn,
                "deprecated API used",
                "https://a.example/app.js:40",
            ),
            entry(
                Level::Error,
                "Uncaught Error: boom",
                "https://a.example/app.js:77",
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

    fn type_in(list: &mut ConsoleList, text: &str) {
        for c in text.chars() {
            assert_eq!(list.step(&typed(c), 5), Step::Typing);
        }
    }

    fn texts(list: &ConsoleList) -> Vec<&str> {
        list.matches()
            .into_iter()
            .map(|index| list.entry(index).text.as_str())
            .collect()
    }

    #[test]
    fn the_panel_opens_with_the_newest_row_picked_and_scrolled_into_view() {
        let list = ConsoleList::open(four());
        assert_eq!(list.line().text(), "");
        assert_eq!(list.total(), 4);
        assert_eq!(list.matches(), [0, 1, 2, 3]);
        assert_eq!(list.picked(4), Some(3));
        assert_eq!(list.window(4, 2, 0), Window { first: 2, shown: 2 });
        assert_eq!(list.count_text(4), "4/4");
    }

    #[test]
    fn typing_narrows_by_level_text_and_place_with_the_words_in_any_order() {
        for (filter, wanted) in [
            (
                "error",
                vec![
                    "Failed to load resource: the server responded with a status of 404 (Not Found)",
                    "Uncaught Error: boom",
                ],
            ),
            ("boom ERROR", vec!["Uncaught Error: boom"]),
            ("app.js:40", vec!["deprecated API used"]),
            ("warn", vec!["deprecated API used"]),
            ("logo 404", vec![
                "Failed to load resource: the server responded with a status of 404 (Not Found)",
            ]),
            ("zebra", vec![]),
        ] {
            let mut list = ConsoleList::open(four());
            type_in(&mut list, filter);
            assert_eq!(texts(&list), wanted, "{filter:?}");
        }
    }

    #[test]
    fn enter_and_escape_close_and_ctrl_q_quits() {
        let mut list = ConsoleList::open(four());
        assert_eq!(list.step(&key(Key::Up, 0), 5), Step::Typing);
        assert_eq!(list.picked(4), Some(2));
        assert_eq!(list.step(&key(Key::Enter, 0), 5), Step::Close);
        assert_eq!(list.step(&key(Key::Escape, 0), 5), Step::Close);
        assert_eq!(list.step(&key(Key::Char('q'), Mods::CTRL), 5), Step::Quit);
        let mut release = key(Key::Escape, 0);
        release.action = KeyAction::Release;
        assert_eq!(list.step(&release, 5), Step::Typing);
        // Enter on nothing matching closes too.
        type_in(&mut list, "zebra");
        assert_eq!(list.step(&key(Key::Enter, 0), 5), Step::Close);
    }

    #[test]
    fn a_click_moves_the_pick_and_past_the_end_moves_nothing() {
        let mut list = ConsoleList::open(four());
        let shown = list.window(4, 3, 0);
        assert_eq!(shown, Window { first: 1, shown: 3 });
        assert_eq!(list.click(0, &shown), Step::Typing);
        assert_eq!(list.picked(4), Some(1));
        assert_eq!(list.click(3, &shown), Step::Typing);
        assert_eq!(list.picked(4), Some(1), "past the end");
    }

    #[test]
    fn an_empty_log_says_so_where_the_count_goes() {
        let mut list = ConsoleList::open(Vec::new());
        assert_eq!(list.total(), 0);
        assert!(list.matches().is_empty());
        assert_eq!(list.picked(0), None);
        assert_eq!(list.count_text(0), NOTHING);
        assert_eq!(list.step(&key(Key::Down, 0), 5), Step::Typing);
        assert_eq!(list.step(&key(Key::Enter, 0), 5), Step::Close);
    }

    #[test]
    fn a_hostile_message_is_matched_by_what_the_row_shows() {
        let entries = vec![entry(Level::Log, "\x1b]0;x\x07log", "")];
        for filter in ["xlog", "]0;", "log ]0;x"] {
            let mut list = ConsoleList::open(entries.clone());
            type_in(&mut list, filter);
            assert_eq!(list.matches(), [0], "{filter:?}");
        }
    }
}
