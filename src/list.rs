//! A list over the screen, filtered by typing, one row picked: what the tab
//! list ([`crate::tablist`]) is made of, apart from what a tab is.
//!
//! Pure state, and it knows nothing about what the rows are: only how many
//! match the filter now, which the owner counts and hands in with each key.
//! That split is the whole reason this is a module of its own — the pick, the
//! page keys, the window that scrolls only when the pick leaves it and the
//! click that picks the row under it were the tab list's, and they never
//! looked at a tab. What differs between the two lists is what a row is, how
//! a filter matches one, and what picking it does, and each owner keeps those.

use crate::input::{Key, KeyAction, KeyInput};
use crate::line::{Edit, Line};

/// The filter and the pick while a list is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    /// The filter being typed. Starts empty: there is no last filter to
    /// offer, since what the person wants each time is a different row.
    pub line: Line,
    /// Which of the matches is picked, from zero. Clamped into the matches
    /// whenever it is read, so that a filter which shrinks the list never
    /// leaves the pick past its end.
    picked: usize,
}

/// What a key did to the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Still open; the row and the rows are out of date.
    Typing,
    /// Escape: closed, nothing picked.
    Close,
    /// Enter, or a click on a row: this one. What it carries is the owner's
    /// business — here it is the position among the matches, and the tab
    /// list turns that into a strip index. `None` when the filter matched
    /// nothing: Enter on an empty list closes it, as Escape does, since there
    /// is nothing else it can mean.
    Pick(Option<usize>),
    /// `ctrl+q`.
    Quit,
}

/// The matches on the screen: `first..first + shown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// The first match on the screen, from zero.
    pub first: usize,
    /// How many matches from `first` are on the screen.
    pub shown: usize,
}

impl List {
    /// Open with the `picked`th match picked and nothing typed.
    pub fn open(picked: usize) -> List {
        List {
            line: Line::empty(),
            picked,
        }
    }

    /// Which match is picked, clamped to the `count` there are; `None` for
    /// none.
    pub fn picked(&self, count: usize) -> Option<usize> {
        count.checked_sub(1).map(|last| self.picked.min(last))
    }

    /// One key. `count` is how many rows match the filter now — which the
    /// owner knows and the list does not — and `page` how many rows the list
    /// has on the screen.
    ///
    /// [`Line`] is asked first, so the url bar's editing keys are the
    /// filter's; what it reports as `Previous`/`Next` (Up, Down, `ctrl+p`,
    /// `ctrl+n`) moves the pick instead of walking a history the list does
    /// not have. PageUp and PageDown move it by `page` rows. A change to the
    /// text puts the pick back on the first match, because the matches are a
    /// new list and the first of them is the best answer to what was just
    /// typed; a move of the cursor leaves it alone.
    pub fn step(&mut self, key: &KeyInput, count: usize, page: usize) -> Step {
        if key.action == KeyAction::Release {
            return Step::Typing;
        }
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
            Edit::Go => Step::Pick(self.picked(count)),
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
    /// window that was drawn and `count` how many rows match: pick the match
    /// on it, or nothing for a row past the end.
    pub fn click(&mut self, row: usize, shown: &Window, count: usize) -> Step {
        if row >= shown.shown {
            return Step::Typing;
        }
        let at = shown.first + row;
        if at >= count {
            return Step::Typing;
        }
        self.picked = at;
        Step::Pick(Some(at))
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

    #[test]
    fn the_pick_moves_with_up_down_ctrl_p_ctrl_n_and_the_page_keys_and_stays_inside_the_count() {
        let mut list = List::open(10);
        assert_eq!(list.step(&key(Key::Down, 0), 12, 5), Step::Typing);
        assert_eq!(list.picked(12), Some(11));
        list.step(&key(Key::Down, 0), 12, 5);
        assert_eq!(list.picked(12), Some(11), "Down past the end stays");
        list.step(&key(Key::Char('p'), Mods::CTRL), 12, 5);
        assert_eq!(list.picked(12), Some(10), "ctrl+p is Up");
        list.step(&key(Key::Char('n'), Mods::CTRL), 12, 5);
        assert_eq!(list.picked(12), Some(11), "ctrl+n is Down");
        list.step(&key(Key::PageUp, 0), 12, 5);
        assert_eq!(list.picked(12), Some(6), "PageUp by a page");
        list.step(&key(Key::PageUp, 0), 12, 5);
        list.step(&key(Key::PageUp, 0), 12, 5);
        assert_eq!(list.picked(12), Some(0));
        list.step(&key(Key::Up, 0), 12, 5);
        assert_eq!(list.picked(12), Some(0), "Up at the top stays");
        list.step(&key(Key::PageDown, 0), 12, 5);
        assert_eq!(list.picked(12), Some(5), "PageDown by a page");
        assert_eq!(list.picked(3), Some(2), "clamped into a smaller count");
        assert_eq!(list.picked(0), None, "and on nothing, nothing");
    }

    #[test]
    fn typing_puts_the_pick_back_on_the_first_match_and_a_cursor_move_does_not() {
        let mut list = List::open(5);
        list.step(&typed('x'), 12, 5);
        assert_eq!(list.picked(12), Some(0));
        list.step(&key(Key::Down, 0), 12, 5);
        list.step(&key(Key::Down, 0), 12, 5);
        list.step(&key(Key::Left, 0), 12, 5);
        assert_eq!(list.picked(12), Some(2));
        assert_eq!(list.line.text(), "x");
    }

    #[test]
    fn enter_picks_the_position_escape_closes_ctrl_q_quits_and_enter_on_nothing_picks_none() {
        let mut list = List::open(0);
        list.step(&key(Key::Down, 0), 3, 5);
        assert_eq!(list.step(&key(Key::Enter, 0), 3, 5), Step::Pick(Some(1)));
        assert_eq!(list.step(&key(Key::Escape, 0), 3, 5), Step::Close);
        assert_eq!(
            list.step(&key(Key::Char('q'), Mods::CTRL), 3, 5),
            Step::Quit
        );
        assert_eq!(list.step(&key(Key::Enter, 0), 0, 5), Step::Pick(None));
        // A release is nothing at all.
        let mut release = key(Key::Enter, 0);
        release.action = KeyAction::Release;
        assert_eq!(list.step(&release, 3, 5), Step::Typing);
    }

    #[test]
    fn a_click_picks_the_row_under_it_and_past_the_end_picks_nothing() {
        let mut list = List::open(0);
        let shown = list.window(2, 5, 0);
        assert_eq!(shown, Window { first: 0, shown: 2 });
        assert_eq!(list.click(1, &shown, 2), Step::Pick(Some(1)));
        assert_eq!(list.picked(2), Some(1));
        assert_eq!(list.click(2, &shown, 2), Step::Typing);
        assert_eq!(list.click(40, &shown, 2), Step::Typing);
        // A window further down: the row is counted from its first.
        let shown = Window { first: 7, shown: 5 };
        assert_eq!(list.click(2, &shown, 12), Step::Pick(Some(9)));
    }

    #[test]
    fn the_window_keeps_the_pick_in_view_and_moves_only_when_it_leaves() {
        let at = |pick: usize, first: usize| {
            let list = List::open(pick);
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
        let list = List::open(1);
        assert_eq!(list.window(3, 5, 12), Window { first: 0, shown: 3 });
        assert_eq!(list.window(0, 5, 0), Window { first: 0, shown: 0 });
    }

    #[test]
    fn the_count_reads_matched_over_total() {
        assert_eq!(List::count_text(2, 11), "2/11");
    }
}
