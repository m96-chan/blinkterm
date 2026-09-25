//! The tab list: every tab by title and url, filtered by typing, one picked.
//!
//! Pure state over a [`Tabs`]; what it draws is [`crate::screen::list_rows`]
//! under the row, and the row is [`crate::screen::prompt_line_beside`] with
//! `tabs: ` as the prompt, the find prompt's shape. Nothing here talks to the
//! engine: picking a tab is [`Tabs::switch_to`] by the caller, and everything
//! else is the person reading.
//!
//! It is what the strip cannot be once there are more tabs than the row can
//! name: a place where every tab's title is whole, and where the twelfth tab,
//! which no `alt+` digit reaches, is two keystrokes away — `1` `2` and Enter.
//! In strip order rather than most recent first, because nothing records when
//! a tab was last in front, and an order the list invented would be one that
//! `ctrl+tab` does not walk.

use std::borrow::Cow;

use crate::input::{Key, KeyAction, KeyInput};
use crate::line::{Edit, Line};
use crate::tabs::Tabs;
use crate::text;

/// The list while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabList {
    /// The filter being typed. Starts empty: there is no last filter to
    /// offer, since what the person wants each time is a different tab.
    pub line: Line,
    /// Which of the matches is picked, from zero. Clamped into the matches
    /// whenever it is read, so that a filter which shrinks the list never
    /// leaves the pick past its end.
    picked: usize,
}

/// One tab as the list shows it: what [`TabList::matches`] hands back per
/// tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<'a> {
    /// The tab's index in the strip, from zero; its number is `index + 1`.
    pub index: usize,
    /// [`crate::tabs::Tab::label`], which is already the best name the tab
    /// has — and is sometimes a sentence made on the spot, which is why it is
    /// a `Cow` rather than a borrow.
    pub label: Cow<'a, str>,
    pub url: &'a str,
    /// Waiting on a dialog or a path: marked `!` as the strip marks it.
    pub asks: bool,
}

/// What a key did to the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Still open; the row and the rows are out of date.
    Typing,
    /// Escape: closed, nothing picked.
    Close,
    /// Enter, or a click on a row: switch to this tab (its strip index).
    /// `None` when the filter matched nothing — Enter on an empty list
    /// closes it, as Escape does, since there is nothing else it can mean.
    Pick(Option<usize>),
    /// `ctrl+q`.
    Quit,
}

/// The matches on the screen: `first..first + shown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub first: usize,
    pub shown: usize,
}

impl TabList {
    /// Open with the tab in front picked, so that Enter with nothing typed
    /// goes nowhere.
    pub fn open(active: usize) -> TabList {
        TabList {
            line: Line::empty(),
            picked: active,
        }
    }

    /// The tabs that match the filter, in strip order.
    ///
    /// The filter is matched case-insensitively against the sanitized label
    /// and url — what the rows show, so that a filter never matches letters
    /// the person cannot see — and a filter that is only digits also matches
    /// a tab whose number starts with them, so `1` `2` reaches the twelfth
    /// tab that `alt+` cannot. An empty filter matches every tab.
    pub fn matches<'a, C>(&self, tabs: &'a Tabs<C>) -> Vec<Entry<'a>> {
        let filter = self.line.text().to_lowercase();
        let digits = !filter.is_empty() && filter.bytes().all(|b| b.is_ascii_digit());
        tabs.iter()
            .enumerate()
            .filter_map(|(index, tab)| {
                let label = tab.label();
                let url = tab.url.as_str();
                let wanted = filter.is_empty()
                    || (digits && (index + 1).to_string().starts_with(&filter))
                    || text::sanitize(&label).to_lowercase().contains(&filter)
                    || text::sanitize(url).to_lowercase().contains(&filter);
                wanted.then(|| Entry {
                    index,
                    label,
                    url,
                    asks: tab.asks(),
                })
            })
            .collect()
    }

    /// Which match is picked, clamped to the `matches` there are; `None`
    /// for none.
    pub fn picked(&self, matches: usize) -> Option<usize> {
        matches.checked_sub(1).map(|last| self.picked.min(last))
    }

    /// One key. `matched` is the strip index of every tab that matches now,
    /// in order — which the caller knows and the list does not — and `page`
    /// how many rows the list has on the screen.
    ///
    /// [`Line`] is asked first, so the url bar's editing keys are the
    /// filter's; what it reports as `Previous`/`Next` (Up, Down, `ctrl+p`,
    /// `ctrl+n`) moves the pick instead of walking a history the list does
    /// not have. PageUp and PageDown move it by `page` rows. A change to the
    /// text puts the pick back on the first match, because the matches are a
    /// new list and the first of them is the best answer to what was just
    /// typed; a move of the cursor leaves it alone.
    pub fn step(&mut self, key: &KeyInput, matched: &[usize], page: usize) -> Step {
        if key.action == KeyAction::Release {
            return Step::Typing;
        }
        let count = matched.len();
        // Where the pick is now, which is where a move starts from.
        let at = self.picked(count).unwrap_or(0);
        let last = count.saturating_sub(1);
        let plain = !key.mods.ctrl() && !key.mods.alt();
        match key.key {
            Key::PageUp if plain => {
                self.picked = at.saturating_sub(page.max(1));
                return Step::Typing;
            }
            Key::PageDown if plain => {
                self.picked = (at + page.max(1)).min(last);
                return Step::Typing;
            }
            _ => {}
        }
        let before = self.line.text().to_string();
        match self.line.step(key) {
            Edit::Go => Step::Pick(self.picked(count).map(|at| matched[at])),
            Edit::Cancel => Step::Close,
            Edit::Quit => Step::Quit,
            Edit::Previous => {
                self.picked = at.saturating_sub(1);
                Step::Typing
            }
            Edit::Next => {
                self.picked = (at + 1).min(last);
                Step::Typing
            }
            Edit::Typing | Edit::Inserted => {
                if self.line.text() != before {
                    self.picked = 0;
                }
                Step::Typing
            }
        }
    }

    /// A click on the `row`th visible row (from zero), with `shown` the
    /// window that was drawn and `matched` the strip index of every match:
    /// pick the tab on it, or nothing for a row past the end.
    pub fn click(&mut self, row: usize, shown: &Window, matched: &[usize]) -> Step {
        if row >= shown.shown {
            return Step::Typing;
        }
        let at = shown.first + row;
        match matched.get(at) {
            Some(&index) => {
                self.picked = at;
                Step::Pick(Some(index))
            }
            None => Step::Typing,
        }
    }

    /// Which matches fit in `rows`, keeping the pick in view.
    ///
    /// The strip's rule: a window that moves only when the pick leaves it,
    /// kept in `first` between draws by the caller, and never past the end,
    /// so that a list that shrank under a filter is not a screen of blank
    /// rows with the matches scrolled off the top.
    pub fn window(&self, count: usize, rows: usize, first: usize) -> Window {
        let rows = rows.max(1);
        let mut first = first.min(count.saturating_sub(rows));
        if let Some(at) = self.picked(count) {
            if at < first {
                first = at;
            } else if at >= first + rows {
                first = at + 1 - rows;
            }
        }
        Window {
            first,
            shown: count.saturating_sub(first).min(rows),
        }
    }

    /// `2/11`, for the right-hand end of the row.
    pub fn count_text(matched: usize, total: usize) -> String {
        format!("{matched}/{total}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Mods;
    use crate::tabs::Tab;

    fn tab(title: &str, url: &str) -> Tab<u32> {
        let mut tab = Tab::new(title, 0, url);
        tab.title = title.to_string();
        tab
    }

    fn tabs(list: &[(&str, &str)]) -> Tabs<u32> {
        let mut tabs = Tabs::new(tab(list[0].0, list[0].1));
        for (title, url) in &list[1..] {
            tabs.open_behind(tab(title, url));
        }
        tabs
    }

    fn three() -> Tabs<u32> {
        tabs(&[
            ("Build log", "https://ci.example/build/4471"),
            ("Mail", "https://mail.example/inbox"),
            ("Changelog", "https://docs.example/changelog"),
        ])
    }

    fn twelve() -> Tabs<u32> {
        let titles: Vec<(String, String)> = (1..=12)
            .map(|n| {
                (
                    format!("Page {}", "x".repeat(n)),
                    format!("https://p{n}.example/"),
                )
            })
            .collect();
        let refs: Vec<(&str, &str)> = titles
            .iter()
            .map(|(title, url)| (title.as_str(), url.as_str()))
            .collect();
        tabs(&refs)
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

    fn indices<C>(list: &TabList, tabs: &Tabs<C>) -> Vec<usize> {
        list.matches(tabs).iter().map(|entry| entry.index).collect()
    }

    fn type_in<C>(list: &mut TabList, tabs: &Tabs<C>, text: &str) {
        for c in text.chars() {
            let matched = indices(list, tabs);
            assert_eq!(list.step(&typed(c), &matched, 5), Step::Typing);
        }
    }

    #[test]
    fn the_list_opens_on_the_tab_in_front_and_shows_every_tab_untyped() {
        let tabs = three();
        let list = TabList::open(1);
        assert_eq!(list.line.text(), "");
        assert_eq!(indices(&list, &tabs), [0, 1, 2]);
        assert_eq!(list.picked(3), Some(1), "the tab in front is picked");
        let entries = list.matches(&tabs);
        assert_eq!(entries[2].label, "Changelog");
        assert_eq!(entries[2].url, "https://docs.example/changelog");
        assert!(!entries[2].asks);
    }

    #[test]
    fn typing_narrows_by_title_and_url_without_regard_to_case_and_digits_reach_a_number() {
        let tabs = three();
        let mut list = TabList::open(0);
        type_in(&mut list, &tabs, "log");
        assert_eq!(indices(&list, &tabs), [0, 2]);

        let mut list = TabList::open(0);
        type_in(&mut list, &tabs, "LOG");
        assert_eq!(indices(&list, &tabs), [0, 2], "without regard to case");

        let mut list = TabList::open(0);
        type_in(&mut list, &tabs, "docs.ex");
        assert_eq!(indices(&list, &tabs), [2], "the url counts too");

        // The twelfth tab, which no `alt+` digit reaches: its number starts
        // with `12`, and nothing else's does or has it in its words.
        let tabs = twelve();
        let mut list = TabList::open(0);
        type_in(&mut list, &tabs, "12");
        assert_eq!(indices(&list, &tabs), [11]);
        // `1` alone is the first and the tenth to the twelfth by number, and
        // every url with a `1` in it: p1, p10, p11, p12.
        let mut list = TabList::open(0);
        type_in(&mut list, &tabs, "1");
        assert_eq!(indices(&list, &tabs), [0, 9, 10, 11]);
        // And a title with the digits in it is a match whatever its number.
        let mut tabs = three();
        tabs.open_behind(tab("Issue 12", "https://issues.example/"));
        let mut list = TabList::open(0);
        type_in(&mut list, &tabs, "12");
        assert_eq!(indices(&list, &tabs), [3]);
    }

    #[test]
    fn the_pick_starts_on_the_tab_in_front_moves_with_up_and_down_and_stays_inside_the_matches() {
        let tabs = twelve();
        let all: Vec<usize> = (0..12).collect();
        let mut list = TabList::open(10);
        assert_eq!(list.step(&key(Key::Down, 0), &all, 5), Step::Typing);
        assert_eq!(list.picked(12), Some(11));
        list.step(&key(Key::Down, 0), &all, 5);
        assert_eq!(list.picked(12), Some(11), "Down past the end stays");
        list.step(&key(Key::Char('p'), Mods::CTRL), &all, 5);
        assert_eq!(list.picked(12), Some(10), "ctrl+p is Up");
        list.step(&key(Key::Char('n'), Mods::CTRL), &all, 5);
        assert_eq!(list.picked(12), Some(11), "ctrl+n is Down");
        list.step(&key(Key::PageUp, 0), &all, 5);
        assert_eq!(list.picked(12), Some(6), "PageUp by a page");
        list.step(&key(Key::PageUp, 0), &all, 5);
        list.step(&key(Key::PageUp, 0), &all, 5);
        assert_eq!(list.picked(12), Some(0));
        list.step(&key(Key::Up, 0), &all, 5);
        assert_eq!(list.picked(12), Some(0), "Up at the top stays");
        list.step(&key(Key::PageDown, 0), &all, 5);
        assert_eq!(list.picked(12), Some(5), "PageDown by a page");

        // Typing puts the pick back on the first match; a cursor move does not.
        list.step(&typed('x'), &all, 5);
        assert_eq!(list.picked(12), Some(0));
        let matched = indices(&list, &tabs);
        list.step(&key(Key::Down, 0), &matched, 5);
        list.step(&key(Key::Down, 0), &matched, 5);
        list.step(&key(Key::Left, 0), &matched, 5);
        assert_eq!(list.picked(matched.len()), Some(2));
        // A filter that leaves one match clamps the pick onto it.
        let mut list = TabList::open(9);
        type_in(&mut list, &tabs, "p3.");
        assert_eq!(indices(&list, &tabs), [2]);
        assert_eq!(list.picked(1), Some(0));
        assert_eq!(list.picked(0), None, "and on nothing, nothing");
    }

    #[test]
    fn enter_picks_escape_closes_ctrl_q_quits_and_enter_on_nothing_closes() {
        let tabs = three();
        let mut list = TabList::open(0);
        type_in(&mut list, &tabs, "mail");
        let matched = indices(&list, &tabs);
        assert_eq!(
            list.step(&key(Key::Enter, 0), &matched, 5),
            Step::Pick(Some(1)),
            "the strip index, not the match's"
        );
        assert_eq!(list.step(&key(Key::Escape, 0), &matched, 5), Step::Close);
        assert_eq!(
            list.step(&key(Key::Char('q'), Mods::CTRL), &matched, 5),
            Step::Quit
        );
        let mut list = TabList::open(0);
        type_in(&mut list, &tabs, "nothing like it");
        let matched = indices(&list, &tabs);
        assert!(matched.is_empty());
        assert_eq!(
            list.step(&key(Key::Enter, 0), &matched, 5),
            Step::Pick(None)
        );
        // A release is nothing at all.
        let mut release = key(Key::Enter, 0);
        release.action = KeyAction::Release;
        assert_eq!(list.step(&release, &matched, 5), Step::Typing);
    }

    #[test]
    fn a_click_on_a_row_picks_the_tab_on_it_and_a_row_past_the_end_picks_nothing() {
        let tabs = three();
        let mut list = TabList::open(0);
        type_in(&mut list, &tabs, "log");
        let matched = indices(&list, &tabs);
        let shown = list.window(matched.len(), 5, 0);
        assert_eq!(shown, Window { first: 0, shown: 2 });
        assert_eq!(list.click(1, &shown, &matched), Step::Pick(Some(2)));
        assert_eq!(list.picked(matched.len()), Some(1));
        assert_eq!(list.click(2, &shown, &matched), Step::Typing);
        assert_eq!(list.click(40, &shown, &matched), Step::Typing);
    }

    #[test]
    fn the_window_keeps_the_pick_in_view_and_moves_only_when_it_leaves() {
        let at = |pick: usize, first: usize| {
            let list = TabList::open(pick);
            let window = list.window(20, 5, first);
            (window.first, window.first + window.shown)
        };
        assert_eq!(at(0, 0), (0, 5));
        assert_eq!(at(4, 0), (0, 5));
        assert_eq!(at(5, 0), (1, 6));
        assert_eq!(at(3, 1), (1, 6), "still in view: the window stays");
        assert_eq!(at(0, 1), (0, 5));
        assert_eq!(at(19, 0), (15, 20));
        // A list that shrank under a filter is not scrolled past its end.
        let list = TabList::open(1);
        assert_eq!(list.window(3, 5, 12), Window { first: 0, shown: 3 });
        assert_eq!(list.window(0, 5, 0), Window { first: 0, shown: 0 });
    }

    #[test]
    fn a_hostile_title_is_matched_by_what_the_row_shows() {
        // The row shows `]0;xlog`: the escape and the bell are not there to
        // be typed or seen, so the filter is matched against the letters.
        let tabs = tabs(&[
            ("\x1b]0;x\x07log", "https://evil.example/\u{202e}moc.knab"),
            ("Mail", "https://mail.example/"),
        ]);
        for (filter, wanted) in [
            ("log", vec![0]),
            ("xlog", vec![0]),
            ("]0;", vec![0]),
            ("/moc.knab", vec![0]),
        ] {
            let mut list = TabList::open(0);
            type_in(&mut list, &tabs, filter);
            assert_eq!(indices(&list, &tabs), wanted, "{filter:?}");
        }
    }

    #[test]
    fn the_count_reads_matched_over_total() {
        assert_eq!(TabList::count_text(2, 11), "2/11");
    }
}
