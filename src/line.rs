//! One line of text being typed: the url bar's, and a page's `prompt()`'s.
//!
//! The url bar had this to itself, and it was in [`crate::app`] because that
//! was the only place anything was typed into this program rather than into a
//! page. A `prompt()` is the second: a page asking a question whose answer is
//! a line of text, drawn on the same row and edited with the same keys. Two
//! editors that disagreed about what backspace does would be one reflex
//! betrayed in whichever of them was met second, so the editing moved here and
//! both of them call it.
//!
//! It used to be a line typed at the end of, as a shell's was before readline:
//! backspace, `ctrl+u`, and a first key that replaces what was offered. That
//! is enough for a line typed from nothing, and not for a url, which is edited
//! in the middle more often than a shell's line is — a path changed, a query
//! fixed, a typo in the host noticed after the path was typed — and which
//! the person was otherwise left to delete back to and type again
//! ([#14](https://github.com/m96-chan/blinkterm/issues/14)). So there is a
//! cursor, and the keys readline taught for moving it and deleting around it.
//! It is drawn by the terminal's own cursor, which the row already used to
//! say where the typing is: all that changed is that it can be put somewhere
//! other than the end.
//!
//! The cursor moves by cluster, not by `char`: an `é` typed as `e` and a
//! combining accent is one step and one backspace, a flag is one, and the
//! cursor is never between a letter and its mark, where the next character
//! typed would take the mark away from the letter it was on.
//! [`crate::screen::clusters`] is the rule, and says what it does not know.
//!
//! Everything in a line is plain text in [`crate::text`]'s sense, always. A
//! character that is not is refused as it is typed or pasted, rather than
//! kept and filtered out of the row later, because a line whose invisible
//! characters were invisible only on the row would have a cursor that stood
//! still for a keystroke while a backspace took a character nobody could
//! see — and the row and the cursor would disagree about where the typing is.

use std::ops::Range;

use crate::input::{Key, KeyAction, KeyInput};
use crate::screen;
use crate::text;

/// What a keystroke did to the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// Nothing that ends the line: a move, a deletion, a key that means
    /// nothing here, or a key let go.
    Typing,
    /// Text went in at the cursor — a character typed, or a suggestion taken.
    /// A caller that offers completions asks for one now and at no other
    /// time: a suggestion made again after a backspace is one that cannot be
    /// deleted.
    Inserted,
    /// Up or `ctrl+p`: the caller may put an older line in with
    /// [`Line::set_text`]. A prompt has none and treats it as typing.
    Previous,
    /// Down or `ctrl+n`: the newer one.
    Next,
    /// Escape: leave what was there alone.
    Cancel,
    /// Enter: what is in the line is the answer.
    Go,
    /// `ctrl+q`, which quits even from the url bar.
    Quit,
}

/// One line being edited, with a cursor, a selection that lasts one key, and
/// a suggestion that is shown and not yet typed.
///
/// The fields are private so that the rules joining them are kept in one
/// place: the cursor is always on a cluster boundary, the text is always
/// plain, the selection lasts exactly one key, and a suggestion exists only
/// with the cursor at the end of an unselected line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// What has been typed, or what was offered and not yet typed over.
    text: String,
    /// Byte offset of the cursor: always a cluster boundary of `text`, in
    /// `0..=text.len()`.
    cursor: usize,
    /// Whether `text` is still the whole of what was offered, untouched, so
    /// that the next key replaces it rather than adding to it.
    whole: bool,
    /// A continuation offered but not typed, drawn dim after the cursor; Tab,
    /// or Right at the end, takes it. Cleared by every key.
    hint: String,
}

/// The part of a line the row shows, as [`Line::view`] chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// The typed text that fits, in whole clusters.
    pub text: String,
    /// The part of the suggestion that fits after it, to be drawn dim.
    pub hint: String,
    /// The cursor's column within the room: always less than the room.
    pub cursor: usize,
}

/// What one key asks of a line, before the selection is taken into account.
///
/// The key table, apart from the line it acts on, so that which keys mean
/// what is one `match` and what each one does is another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Insert(char),
    DeleteBack,
    DeleteForward,
    Left,
    Right,
    Home,
    End,
    WordLeft,
    WordRight,
    DeleteWordBack,
    DeleteWordForward,
    KillToStart,
    KillToEnd,
    TakeHint,
    Previous,
    Next,
    Nothing,
}

impl Action {
    /// Which of the line's actions a key is.
    ///
    /// Readline's keys, because they are what a terminal taught for a line
    /// being edited: `ctrl+a` and `ctrl+e` to the ends, `ctrl+b`/`ctrl+f` and
    /// `alt+b`/`alt+f` by a character and a word, `ctrl+w` and `alt+d` a word
    /// either side, `ctrl+u` and `ctrl+k` to either end. The arrows, Home, End
    /// and Delete for the reflex that came from everywhere else, with `ctrl`
    /// or `alt` on an arrow for a word — `alt+left` is Back when the url bar
    /// is closed, but the bar is asked first while it is open, and in a line
    /// being typed a word is what it means.
    ///
    /// `ctrl+t` (transpose) and `ctrl+y` (yank) are deliberately missing:
    /// there is no kill ring to yank from, and `ctrl+t` is the new-tab reflex
    /// one key away. A key that is not here does nothing in the line, and
    /// does not reach the page or the tab commands either.
    fn of(key: &KeyInput) -> Action {
        let ctrl = key.mods.ctrl();
        let alt = key.mods.alt();
        match key.key {
            Key::Char(c) if ctrl && !alt => match c {
                'a' => Action::Home,
                'e' => Action::End,
                'b' => Action::Left,
                'f' => Action::Right,
                'h' => Action::DeleteBack,
                'd' => Action::DeleteForward,
                'w' => Action::DeleteWordBack,
                'u' => Action::KillToStart,
                'k' => Action::KillToEnd,
                'p' => Action::Previous,
                'n' => Action::Next,
                _ => Action::Nothing,
            },
            Key::Char(c) if alt && !ctrl => match c {
                'b' => Action::WordLeft,
                'f' => Action::WordRight,
                'd' => Action::DeleteWordForward,
                _ => Action::Nothing,
            },
            Key::Backspace if ctrl || alt => Action::DeleteWordBack,
            Key::Backspace => Action::DeleteBack,
            Key::Delete if ctrl || alt => Action::DeleteWordForward,
            Key::Delete => Action::DeleteForward,
            Key::Left if ctrl || alt => Action::WordLeft,
            Key::Left => Action::Left,
            Key::Right if ctrl || alt => Action::WordRight,
            Key::Right => Action::Right,
            Key::Home => Action::Home,
            Key::End => Action::End,
            Key::Tab if !ctrl && !alt && !key.mods.shift() => Action::TakeHint,
            Key::Up if !ctrl && !alt => Action::Previous,
            Key::Down if !ctrl && !alt => Action::Next,
            // A key that typed something, with no ctrl or alt on it: shift
            // and super are how a capital or a symbol is reached. A character
            // that is not plain text is not typed at all — see the module.
            _ if !ctrl && !alt => match key.text {
                Some(c) if text::is_plain(c) => Action::Insert(c),
                _ => Action::Nothing,
            },
            _ => Action::Nothing,
        }
    }
}

impl Line {
    /// A line with nothing in it and nothing selected: a new tab's url bar.
    pub fn empty() -> Line {
        Line {
            text: String::new(),
            cursor: 0,
            whole: false,
            hint: String::new(),
        }
    }

    /// A line offered with all of it selected: the first key replaces it, and
    /// a backspace deletes it, the way a browser treats the address it shows
    /// on `ctrl+l` and the default a `prompt()` offers. The cursor is at the
    /// end, where it was before there was anywhere else for it to be.
    ///
    /// What is offered is made plain text first ([`text::sanitize`]), as
    /// everything in a line is.
    pub fn selected(text: impl Into<String>) -> Line {
        let mut line = Line::empty();
        line.set_text(text);
        line.whole = true;
        line
    }

    /// What is in the line.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Where the cursor is, as a byte offset into [`Line::text`]: always the
    /// start or the end of a cluster.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether the line is still what was offered, selected, so that the next
    /// key replaces it.
    pub fn whole(&self) -> bool {
        self.whole
    }

    /// The suggestion being shown after the cursor, or nothing.
    pub fn hint(&self) -> &str {
        &self.hint
    }

    /// Replace the text — a line from history put in with Up — with the
    /// cursor at the end, the selection spent and no suggestion.
    ///
    /// Made plain first: a break becomes a space and the rest of what
    /// [`text::sanitize`] removes goes, so what is set is what the row shows.
    pub fn set_text(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.text = text::sanitize(&text).into_owned();
        self.cursor = self.text.len();
        self.whole = false;
        self.hint.clear();
    }

    /// Offer a continuation of what is typed, to be drawn dim after it and
    /// taken with Tab or Right; `None` takes one back.
    ///
    /// Only offered with the cursor at the end of a line that is not
    /// selected: a suggestion is the rest of what is being typed, and with the
    /// cursor in the middle, or a whole line about to be replaced, nothing is
    /// being typed at the end. Otherwise it is cleared.
    pub fn suggest(&mut self, hint: Option<String>) {
        self.hint.clear();
        if self.cursor != self.text.len() || self.whole {
            return;
        }
        if let Some(hint) = hint {
            self.hint = text::sanitize(&hint).into_owned();
        }
    }

    /// Put `text` in at the cursor — replacing the whole line, if it is still
    /// selected — and leave the cursor after it. Returns whether anything
    /// went in.
    ///
    /// This is what a paste calls, and it is the same rule as a key typed: a
    /// character that is not plain text ([`text::is_plain`]) is dropped. A
    /// line is one line, so a newline or a tab in what was pasted goes too,
    /// rather than becoming a space the way it does on the row: a url copied
    /// with the line break after it is the url, and a break in the middle of
    /// one is not a space either. A caller that would rather have the spaces
    /// runs [`text::sanitize`] first. The suggestion goes, as it does for
    /// every edit; the caller asks for a new one.
    pub fn insert_str(&mut self, text: &str) -> bool {
        let plain: String = text.chars().filter(|&c| text::is_plain(c)).collect();
        if plain.is_empty() {
            return false;
        }
        self.hint.clear();
        if std::mem::take(&mut self.whole) {
            self.text.clear();
            self.cursor = 0;
        }
        self.text.insert_str(self.cursor, &plain);
        self.cursor += plain.len();
        self.snap();
        true
    }

    /// Apply one keystroke.
    ///
    /// A key let go does nothing, and neither does a modifier pressed on its
    /// own, which the Kitty protocol reports as a key of its own; neither
    /// spends the selection, because somebody reaching for shift to type a
    /// capital has not typed anything yet. Every other key spends it, whether
    /// or not it changed anything — an arrow pressed on an offered default is
    /// somebody who has looked at it, and a move on a selected line deselects
    /// rather than replaces: Left is "put me at the start", Right "at the
    /// end". A deletion of any size on a selected line deletes the lot, and a
    /// character replaces it.
    ///
    /// `ctrl+u` deletes from the start to the cursor, as readline's does. It
    /// used to empty the line; with the cursor at the end, which is where it
    /// always was then, that is the same thing.
    pub fn step(&mut self, key: &KeyInput) -> Edit {
        if key.action == KeyAction::Release || matches!(key.key, Key::Other(_)) {
            return Edit::Typing;
        }
        match key.key {
            Key::Escape => return Edit::Cancel,
            Key::Enter => return Edit::Go,
            Key::Char('q') if key.mods.ctrl() => return Edit::Quit,
            _ => {}
        }
        let whole = std::mem::take(&mut self.whole);
        let hint = std::mem::take(&mut self.hint);
        let action = Action::of(key);
        if whole {
            return self.step_selected(action);
        }
        let end = self.text.len();
        match action {
            Action::Insert(c) => {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                self.snap();
                return Edit::Inserted;
            }
            Action::DeleteBack => self.delete(self.cluster_before()..self.cursor),
            Action::DeleteForward => self.delete(self.cursor..self.cluster_after()),
            Action::Left => self.cursor = self.cluster_before(),
            Action::Right | Action::TakeHint if self.cursor == end && !hint.is_empty() => {
                self.text.push_str(&hint);
                self.cursor = self.text.len();
                return Edit::Inserted;
            }
            Action::Right => self.cursor = self.cluster_after(),
            Action::Home => self.cursor = 0,
            Action::End => self.cursor = end,
            Action::WordLeft => self.cursor = self.word_before(),
            Action::WordRight => self.cursor = self.word_after(),
            Action::DeleteWordBack => self.delete(self.word_before()..self.cursor),
            Action::DeleteWordForward => self.delete(self.cursor..self.word_after()),
            Action::KillToStart => self.delete(0..self.cursor),
            Action::KillToEnd => self.delete(self.cursor..end),
            Action::Previous => return Edit::Previous,
            Action::Next => return Edit::Next,
            Action::TakeHint | Action::Nothing => {}
        }
        Edit::Typing
    }

    /// The first key on an offered line, which is all of it selected.
    fn step_selected(&mut self, action: Action) -> Edit {
        match action {
            Action::Insert(c) => {
                self.text.clear();
                self.text.push(c);
                self.cursor = self.text.len();
                return Edit::Inserted;
            }
            Action::DeleteBack
            | Action::DeleteForward
            | Action::DeleteWordBack
            | Action::DeleteWordForward
            | Action::KillToStart
            | Action::KillToEnd => {
                self.text.clear();
                self.cursor = 0;
            }
            Action::Left | Action::Home | Action::WordLeft => self.cursor = 0,
            Action::Right | Action::End | Action::WordRight => self.cursor = self.text.len(),
            Action::Previous => return Edit::Previous,
            Action::Next => return Edit::Next,
            Action::TakeHint | Action::Nothing => {}
        }
        Edit::Typing
    }

    /// What of the line fits in `room` cells, with the cursor in sight.
    ///
    /// Stateless, so that drawing the row is not a mutation of the line, and
    /// still steady: the window does not slide a cell with every key, which
    /// is a row that shimmers while it is typed on, but jumps by half the room
    /// when the cursor would leave it, so there is always half a room of
    /// context on the side the cursor is moving towards. Readline gets the
    /// same by keeping an offset; the half-room paging gets it from the cursor
    /// alone.
    ///
    /// The arithmetic, with `avail` the room (at least one cell), `c` the
    /// cells before the cursor and `total` the cells of the text and the hint,
    /// plus one for the cursor when there is no hint to sit on:
    ///
    /// - When everything fits, the window starts at 0.
    /// - Otherwise it starts at the smallest multiple `s` of half the room
    ///   that puts the cursor inside `[s, s + avail)` — 0 if the cursor is in
    ///   the first room — and never later than `total - avail`, so that a
    ///   cursor at the end shows the whole tail, which is what the row showed
    ///   before there was a cursor to move. `c - s < avail` holds either way:
    ///   the first by choice of `s`, and the clamp because the cursor is at
    ///   most `total - 1`.
    /// - The window is filled with whole clusters from the first one that
    ///   starts at `s` or after. A wide one that straddles `s` is skipped,
    ///   which costs a blank cell at the left and moves the start to at most
    ///   `s + 1`; the cursor is on a cluster start at or after `s`, so it is
    ///   still at or after the new start, and still less than `avail` from it.
    pub fn view(&self, room: usize) -> View {
        let avail = room.max(1);
        let before = screen::width(&self.text[..self.cursor]);
        let total = screen::width(&self.text)
            + screen::width(&self.hint)
            + usize::from(self.hint.is_empty());
        let mut start = 0;
        if total > avail {
            let step = (avail / 2).max(1);
            if before >= avail {
                start = (before - avail + 1).div_ceil(step) * step;
            }
            start = start.min(total - avail);
        }

        let mut view = View {
            text: String::new(),
            hint: String::new(),
            cursor: 0,
        };
        let mut column = 0;
        let mut first = None;
        let mut used = 0;
        let parts = [(&self.text, false), (&self.hint, true)];
        'fill: for (part, is_hint) in parts {
            for range in screen::clusters(part) {
                let cluster = &part[range];
                let cells = screen::width(cluster);
                let at = column;
                column += cells;
                if at < start {
                    continue;
                }
                first.get_or_insert(at);
                if used + cells > avail {
                    break 'fill;
                }
                used += cells;
                if is_hint {
                    view.hint.push_str(cluster);
                } else {
                    view.text.push_str(cluster);
                }
            }
        }
        view.cursor = before - first.unwrap_or(before).min(before);
        view
    }

    /// Take `range` out, leaving the cursor where it began.
    fn delete(&mut self, range: Range<usize>) {
        self.text.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.snap();
    }

    /// Move the cursor forward to the end of the cluster it is inside, if an
    /// edit has made it inside one: a flag's second half typed before a lone
    /// indicator, or a letter put in front of a mark that had nothing to sit
    /// on, joins two clusters around the cursor.
    fn snap(&mut self) {
        if let Some(range) = screen::clusters(&self.text)
            .into_iter()
            .find(|range| range.start < self.cursor && self.cursor < range.end)
        {
            self.cursor = range.end;
        }
    }

    /// Where the cluster before the cursor starts.
    fn cluster_before(&self) -> usize {
        screen::clusters(&self.text[..self.cursor])
            .last()
            .map_or(0, |range| range.start)
    }

    /// Where the cluster after the cursor ends.
    fn cluster_after(&self) -> usize {
        screen::clusters(&self.text[self.cursor..])
            .first()
            .map_or(self.cursor, |range| self.cursor + range.end)
    }

    /// Where the word before the cursor starts: past whatever separates it
    /// from the cursor, then past the word.
    ///
    /// A word is a run of letters and digits, which is readline's
    /// `backward-kill-word` and not the shell's word between spaces: `ctrl+w`
    /// after `https://example.com/docs` takes `docs`, and not the url. A
    /// cluster is a letter when the character it starts with is, so a word
    /// ends after its last accent rather than before it.
    fn word_before(&self) -> usize {
        let clusters = screen::clusters(&self.text[..self.cursor]);
        let mut at = self.cursor;
        let mut in_word = false;
        for range in clusters.into_iter().rev() {
            let wordy = is_word(&self.text[range.clone()]);
            if in_word && !wordy {
                break;
            }
            in_word |= wordy;
            at = range.start;
        }
        at
    }

    /// Where the word after the cursor ends, by the same rule as
    /// [`Line::word_before`].
    fn word_after(&self) -> usize {
        let rest = &self.text[self.cursor..];
        let mut at = self.cursor;
        let mut in_word = false;
        for range in screen::clusters(rest) {
            let wordy = is_word(&rest[range.clone()]);
            if in_word && !wordy {
                break;
            }
            in_word |= wordy;
            at = self.cursor + range.end;
        }
        at
    }
}

/// Whether a cluster is part of a word: its first character is a letter or a
/// digit.
fn is_word(cluster: &str) -> bool {
    cluster.chars().next().is_some_and(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Mods;

    fn typed(c: char) -> KeyInput {
        KeyInput {
            key: Key::Char(c),
            mods: Mods::default(),
            action: KeyAction::Press,
            text: Some(c),
        }
    }

    fn key(k: Key, mods: u32) -> KeyInput {
        KeyInput {
            key: k,
            mods: Mods(mods),
            action: KeyAction::Press,
            text: None,
        }
    }

    fn ctrl(c: char) -> KeyInput {
        key(Key::Char(c), Mods::CTRL)
    }

    fn alt(c: char) -> KeyInput {
        key(Key::Char(c), Mods::ALT)
    }

    /// A line holding `text`, not selected, with the cursor at the end.
    fn holding(text: &str) -> Line {
        let mut line = Line::empty();
        line.set_text(text);
        line
    }

    /// The line with a `|` where the cursor is, which is how a test reads.
    fn shown(line: &Line) -> String {
        let mut out = line.text().to_string();
        out.insert(line.cursor(), '|');
        out
    }

    fn press(line: &mut Line, keys: &[KeyInput]) {
        for key in keys {
            line.step(key);
        }
    }

    #[test]
    fn the_url_bar_starts_with_the_whole_address_selected() {
        // ctrl+l, then typing: what is there goes, the way it would in a
        // browser where the address was selected.
        let mut line = Line::selected("https://example.com/a");
        assert_eq!(line.step(&typed('x')), Edit::Inserted);
        assert_eq!(line.text(), "x");
        // And from then on it is ordinary typing.
        line.step(&typed('y'));
        assert_eq!(line.text(), "xy");
        line.step(&key(Key::Backspace, 0));
        assert_eq!(line.text(), "x");

        // A backspace as the first thing deletes the lot, not one character.
        let mut line = Line::selected("https://example.com/a");
        line.step(&key(Key::Backspace, 0));
        assert!(line.text().is_empty());
    }

    #[test]
    fn the_line_knows_when_it_is_finished() {
        let mut line = holding("example.com");
        assert_eq!(line.step(&key(Key::Enter, 0)), Edit::Go);
        assert_eq!(
            line.text(),
            "example.com",
            "enter does not change what was typed"
        );
        assert_eq!(line.step(&key(Key::Escape, 0)), Edit::Cancel);
        assert_eq!(line.step(&ctrl('q')), Edit::Quit);
        assert_eq!(line.text(), "example.com");
    }

    #[test]
    fn a_line_forgets_its_selection_after_the_first_key() {
        let mut line = Line::selected("default");
        assert!(line.whole());
        assert_eq!(line.step(&typed('x')), Edit::Inserted);
        assert_eq!(line.text(), "x", "the first key replaced what was offered");
        assert!(!line.whole());
        line.step(&typed('y'));
        assert_eq!(line.text(), "xy", "and the second one added to it");

        // A key that types nothing still spends the selection: whoever
        // pressed it has seen the default, and what comes next is an edit.
        let mut line = Line::selected("default");
        line.step(&key(Key::Function(1), 0));
        assert!(!line.whole());
        line.step(&key(Key::Backspace, 0));
        assert_eq!(line.text(), "defaul", "one character, not the lot");
    }

    #[test]
    fn a_character_goes_in_at_the_cursor_not_at_the_end() {
        let mut line = holding("exmple.com");
        press(
            &mut line,
            &[key(Key::Home, 0), key(Key::Right, 0), key(Key::Right, 0)],
        );
        assert_eq!(line.step(&typed('a')), Edit::Inserted);
        assert_eq!(shown(&line), "exa|mple.com");
        // A capital is a shifted key and is typed like any other.
        let mut capital = typed('X');
        capital.mods = Mods(Mods::SHIFT);
        line.step(&capital);
        assert_eq!(shown(&line), "exaX|mple.com");
    }

    #[test]
    fn left_and_right_move_by_one_cluster_and_stop_at_the_ends() {
        let mut line = holding("ab");
        press(&mut line, &[key(Key::Left, 0)]);
        assert_eq!(shown(&line), "a|b");
        press(&mut line, &[key(Key::Left, 0), key(Key::Left, 0)]);
        assert_eq!(shown(&line), "|ab", "and no further");
        press(&mut line, &[ctrl('f'), ctrl('f'), ctrl('f')]);
        assert_eq!(shown(&line), "ab|", "ctrl+f is right, and it stops too");
        press(&mut line, &[ctrl('b')]);
        assert_eq!(shown(&line), "a|b");

        // A letter and its accent, a wide character, and a flag are one step
        // each.
        for (text, steps) in [
            ("e\u{301}x", vec![3, 4]),
            ("\u{65e5}\u{672c}", vec![3, 6]),
            ("\u{1f1ef}\u{1f1f5}a", vec![8, 9]),
            ("\u{263a}\u{fe0f}!", vec![6, 7]),
        ] {
            let mut line = holding(text);
            press(&mut line, &[key(Key::Home, 0)]);
            for &at in &steps {
                line.step(&key(Key::Right, 0));
                assert_eq!(line.cursor(), at, "{text:?}");
            }
            for &at in steps.iter().rev().skip(1) {
                line.step(&key(Key::Left, 0));
                assert_eq!(line.cursor(), at, "{text:?}");
            }
        }
    }

    #[test]
    fn a_character_that_is_not_plain_text_is_not_typed() {
        // A zero-width joiner, a bidi override and an escape would each be a
        // backspace that seems to do nothing, or worse; they are refused, and
        // the line is as it was.
        let mut line = holding("ab");
        for c in ['\u{200d}', '\u{202e}', '\x1b', '\u{9b}'] {
            assert_eq!(line.step(&typed(c)), Edit::Typing, "U+{:04X}", c as u32);
        }
        assert_eq!(shown(&line), "ab|");
        // So an emoji family typed a person at a time is its people, which
        // is how the row draws it after sanitizing anyway.
        let mut family = Line::empty();
        for c in ['\u{1f468}', '\u{200d}', '\u{1f469}'] {
            family.step(&typed(c));
        }
        assert_eq!(family.text(), "\u{1f468}\u{1f469}");
        family.step(&key(Key::Backspace, 0));
        assert_eq!(family.text(), "\u{1f468}");
        // And what is offered or set is made plain, a break becoming a space.
        assert_eq!(Line::selected("a\u{200b}b\nc").text(), "ab c");
    }

    #[test]
    fn home_end_ctrl_a_and_ctrl_e_go_to_the_ends() {
        let mut line = holding("example.com");
        press(&mut line, &[key(Key::Home, 0)]);
        assert_eq!(line.cursor(), 0);
        press(&mut line, &[key(Key::End, 0)]);
        assert_eq!(line.cursor(), 11);
        press(&mut line, &[ctrl('a')]);
        assert_eq!(line.cursor(), 0);
        press(&mut line, &[ctrl('e')]);
        assert_eq!(line.cursor(), 11);
    }

    #[test]
    fn backspace_takes_the_cluster_before_and_delete_the_one_after() {
        let mut line = holding("cafe\u{301}s");
        press(&mut line, &[key(Key::Left, 0)]);
        line.step(&key(Key::Backspace, 0));
        assert_eq!(shown(&line), "caf|s", "the e and its accent together");
        line.step(&key(Key::Delete, 0));
        assert_eq!(shown(&line), "caf|");
        line.step(&key(Key::Delete, 0));
        assert_eq!(shown(&line), "caf|", "nothing after the end to delete");
        line.step(&ctrl('h'));
        assert_eq!(shown(&line), "ca|", "ctrl+h is backspace");
        press(&mut line, &[key(Key::Home, 0), ctrl('d')]);
        assert_eq!(shown(&line), "|a", "ctrl+d is delete");
        line.step(&key(Key::Backspace, 0));
        assert_eq!(shown(&line), "|a", "nothing before the start");

        let mut flags = holding("\u{1f1ef}\u{1f1f5}\u{1f1fa}\u{1f1f8}");
        flags.step(&key(Key::Backspace, 0));
        assert_eq!(
            flags.text(),
            "\u{1f1ef}\u{1f1f5}",
            "one flag, not half of one"
        );
    }

    #[test]
    fn ctrl_w_and_alt_backspace_delete_the_word_before_the_cursor() {
        let mut line = holding("https://example.com/docs");
        line.step(&ctrl('w'));
        assert_eq!(shown(&line), "https://example.com/|");
        line.step(&key(Key::Backspace, Mods::ALT));
        assert_eq!(shown(&line), "https://example.|");
        line.step(&ctrl('w'));
        assert_eq!(shown(&line), "https://|");
        line.step(&ctrl('w'));
        assert_eq!(shown(&line), "|");

        // From the middle, only what is before the cursor, and a word ends
        // after its accents.
        let mut line = holding("a cafe\u{301} b");
        press(&mut line, &[key(Key::Left, 0), key(Key::Left, 0)]);
        line.step(&ctrl('w'));
        assert_eq!(shown(&line), "a | b");
    }

    #[test]
    fn alt_d_and_ctrl_delete_delete_the_word_after() {
        let mut line = holding("https://example.com/docs");
        line.step(&key(Key::Home, 0));
        line.step(&alt('d'));
        assert_eq!(shown(&line), "|://example.com/docs");
        line.step(&key(Key::Delete, Mods::CTRL));
        assert_eq!(shown(&line), "|.com/docs");
        press(&mut line, &[key(Key::End, 0), alt('d')]);
        assert_eq!(shown(&line), ".com/docs|", "nothing after the end");
    }

    #[test]
    fn alt_b_alt_f_ctrl_and_alt_arrows_move_by_word() {
        let mut line = holding("https://example.com/docs");
        line.step(&alt('b'));
        assert_eq!(shown(&line), "https://example.com/|docs");
        line.step(&key(Key::Left, Mods::CTRL));
        assert_eq!(shown(&line), "https://example.|com/docs");
        line.step(&key(Key::Left, Mods::ALT));
        assert_eq!(shown(&line), "https://|example.com/docs");
        line.step(&alt('f'));
        assert_eq!(shown(&line), "https://example|.com/docs");
        line.step(&key(Key::Right, Mods::CTRL));
        assert_eq!(shown(&line), "https://example.com|/docs");
        line.step(&key(Key::Right, Mods::ALT));
        assert_eq!(shown(&line), "https://example.com/docs|");
        line.step(&alt('f'));
        assert_eq!(shown(&line), "https://example.com/docs|");
        press(&mut line, &[ctrl('a'), alt('b')]);
        assert_eq!(shown(&line), "|https://example.com/docs");
    }

    #[test]
    fn ctrl_u_kills_to_the_start_and_ctrl_k_to_the_end() {
        // With the cursor at the end, ctrl+u empties the line, as it always
        // did.
        let mut line = holding("half typed");
        line.step(&ctrl('u'));
        assert!(line.text().is_empty());

        let mut line = holding("example.com/path");
        press(&mut line, &[ctrl('a'), alt('f'), alt('f')]);
        assert_eq!(shown(&line), "example.com|/path");
        let mut before = line.clone();
        before.step(&ctrl('u'));
        assert_eq!(shown(&before), "|/path");
        line.step(&ctrl('k'));
        assert_eq!(shown(&line), "example.com|");
    }

    #[test]
    fn the_first_key_on_a_selected_line_replaces_it_and_a_move_only_deselects() {
        let offered = "https://example.com/a";
        let mut line = Line::selected(offered);
        line.step(&key(Key::Left, 0));
        assert_eq!(
            shown(&line),
            format!("|{offered}"),
            "Left puts me at the start"
        );
        assert!(!line.whole());

        let mut line = Line::selected(offered);
        line.step(&key(Key::Right, 0));
        assert_eq!(shown(&line), format!("{offered}|"), "Right at the end");
        line.step(&typed('b'));
        assert_eq!(
            line.text(),
            "https://example.com/ab",
            "and then it is typing"
        );

        let mut line = Line::selected(offered);
        line.step(&ctrl('w'));
        assert_eq!(line.text(), "", "a deletion of any size is all of it");
        let mut line = Line::selected(offered);
        line.step(&alt('b'));
        assert_eq!(shown(&line), format!("|{offered}"));
        assert_eq!(line.text(), offered);
    }

    #[test]
    fn a_release_or_a_bare_modifier_neither_edits_nor_spends_the_selection() {
        const LEFT_SHIFT: u32 = 57441;
        let mut line = Line::selected("default");
        let mut released = typed('x');
        released.action = KeyAction::Release;
        assert_eq!(line.step(&released), Edit::Typing);
        assert_eq!(
            line.step(&key(Key::Other(LEFT_SHIFT), Mods::SHIFT)),
            Edit::Typing
        );
        assert!(line.whole());
        assert_eq!(line.text(), "default");
        // Nor does it clear a suggestion: nothing has been typed.
        let mut line = line_with_hint("exa", "mple.com");
        line.step(&released);
        assert_eq!(line.hint(), "mple.com");
    }

    fn line_with_hint(text: &str, hint: &str) -> Line {
        let mut line = holding(text);
        line.suggest(Some(hint.to_string()));
        assert_eq!(line.hint(), hint);
        line
    }

    #[test]
    fn a_hint_is_taken_by_tab_or_by_right_at_the_end_and_cleared_by_any_edit() {
        let mut line = line_with_hint("exa", "mple.com/");
        assert_eq!(line.step(&key(Key::Tab, 0)), Edit::Inserted);
        assert_eq!(shown(&line), "example.com/|");
        assert_eq!(line.hint(), "");

        let mut line = line_with_hint("exa", "mple.com/");
        assert_eq!(line.step(&key(Key::Right, 0)), Edit::Inserted);
        assert_eq!(shown(&line), "example.com/|");

        // Tab with nothing to take is nothing.
        assert_eq!(line.step(&key(Key::Tab, 0)), Edit::Typing);
        assert_eq!(shown(&line), "example.com/|");

        for edit in [
            key(Key::Backspace, 0),
            key(Key::Left, 0),
            key(Key::Home, 0),
            ctrl('w'),
            typed('m'),
            key(Key::Up, 0),
        ] {
            let mut line = line_with_hint("exa", "mple.com/");
            line.step(&edit);
            assert_eq!(line.hint(), "", "{edit:?}");
            assert!(!line.text().contains("mple.com"), "{edit:?}");
        }
    }

    #[test]
    fn a_hint_is_only_offered_at_the_end_of_an_unselected_line() {
        let mut line = holding("exa");
        line.step(&key(Key::Left, 0));
        line.suggest(Some("mple.com".to_string()));
        assert_eq!(line.hint(), "", "the cursor is not at the end");

        let mut line = Line::selected("exa");
        line.suggest(Some("mple.com".to_string()));
        assert_eq!(line.hint(), "", "the line is about to be replaced");

        let mut line = line_with_hint("exa", "mple.com");
        line.suggest(None);
        assert_eq!(line.hint(), "", "and None takes one back");
    }

    #[test]
    fn up_and_down_are_reported_and_change_nothing() {
        let mut line = holding("exa");
        for (k, edit) in [
            (key(Key::Up, 0), Edit::Previous),
            (ctrl('p'), Edit::Previous),
            (key(Key::Down, 0), Edit::Next),
            (ctrl('n'), Edit::Next),
        ] {
            assert_eq!(line.step(&k), edit, "{k:?}");
            assert_eq!(shown(&line), "exa|");
        }
        let mut line = Line::selected("exa");
        assert_eq!(line.step(&key(Key::Up, 0)), Edit::Previous);
        assert!(!line.whole(), "it deselects, as any key does");
    }

    #[test]
    fn insert_str_goes_in_at_the_cursor_and_drops_control_characters() {
        let mut line = holding("ad");
        line.step(&key(Key::Left, 0));
        assert!(line.insert_str("b\nc"));
        assert_eq!(shown(&line), "abc|d");
        let mut line = Line::empty();
        assert!(line.insert_str("a\nb\tc"));
        assert_eq!(line.text(), "abc");
        // Invisible and bidi characters go too: what is pasted is what is
        // plain text of it.
        assert!(line.insert_str("\u{202e}\u{200b}d\x1b"));
        assert_eq!(shown(&line), "abcd|");
        // A selected line is replaced by a paste, as by a key.
        let mut line = Line::selected("https://old.example");
        assert!(line.insert_str("new.example"));
        assert_eq!(shown(&line), "new.example|");
        assert!(!line.whole());
        // Nothing to put in is nothing done, and the selection is kept.
        let mut line = Line::selected("kept");
        assert!(!line.insert_str("\r\n\u{200b}"));
        assert!(!line.insert_str(""));
        assert!(line.whole());
        assert_eq!(line.text(), "kept");
        // A paste clears a suggestion; the caller asks for another.
        let mut line = line_with_hint("exa", "mple.com");
        line.insert_str("m");
        assert_eq!(line.hint(), "");
    }

    #[test]
    fn a_cursor_is_never_left_inside_a_cluster_an_edit_made() {
        // An indicator typed in front of a flag pairs with the flag's first
        // half, so the cursor after it would be mid-flag: it goes to the end
        // of the new pair instead.
        let mut line = holding("\u{1f1ef}\u{1f1f5}");
        line.step(&key(Key::Home, 0));
        line.step(&typed('\u{1f1fa}'));
        assert_eq!(line.cursor(), 8);
        assert!(screen::clusters(line.text())
            .iter()
            .any(|range| range.end == line.cursor()));
    }

    #[test]
    fn the_view_shows_everything_when_it_fits() {
        let line = holding("example.com");
        assert_eq!(
            line.view(20),
            View {
                text: "example.com".to_string(),
                hint: String::new(),
                cursor: 11,
            }
        );
        let hinted = line_with_hint("exa", "mple.com");
        assert_eq!(
            hinted.view(11),
            View {
                text: "exa".to_string(),
                hint: "mple.com".to_string(),
                cursor: 3,
            },
            "a hint sits under the cursor, so it needs no cell of its own"
        );
    }

    #[test]
    fn the_view_keeps_the_cursor_in_sight_and_shows_the_tail_at_the_end() {
        let url = "https://example.com/a/very/long/path/x";
        let mut line = holding(url);
        let view = line.view(10);
        assert_eq!(view.text, "ng/path/x", "the end, and a cell for the cursor");
        assert_eq!(view.cursor, 9);
        line.step(&key(Key::Home, 0));
        let view = line.view(10);
        assert_eq!(view.text, "https://ex");
        assert_eq!(view.cursor, 0);
    }

    #[test]
    fn the_view_pages_by_half_a_room_rather_than_scrolling_per_key() {
        let mut line = holding("abcdefghijklmnopqrstuvwxyz");
        line.step(&key(Key::Home, 0));
        let mut starts = Vec::new();
        for _ in 0..16 {
            line.step(&key(Key::Right, 0));
            let view = line.view(10);
            starts.push(view.text.chars().next().unwrap_or(' '));
        }
        // Still while the cursor is in the first room, then a jump of five,
        // held for five keys, then another.
        assert_eq!(starts.iter().collect::<String>(), "aaaaaaaaafffffkk");
    }

    #[test]
    fn the_view_never_splits_a_wide_character() {
        let page = "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30da}\u{30fc}\u{30b8}";
        let mut line = holding(page);
        let view = line.view(8);
        assert_eq!(view.text, "\u{30da}\u{30fc}\u{30b8}");
        assert_eq!(view.cursor, 6);
        // The cursor after the second character, in a room of four: the
        // window starts at the second, and shows two whole characters.
        press(
            &mut line,
            &[key(Key::Home, 0), key(Key::Right, 0), key(Key::Right, 0)],
        );
        let view = line.view(4);
        assert_eq!(view.text, "\u{672c}\u{8a9e}");
        assert_eq!(view.cursor, 2);
        // An odd room with wide characters in it leaves a cell blank rather
        // than showing half of one.
        let view = line.view(5);
        assert!(screen::width(&view.text) <= 5);
    }

    #[test]
    fn the_view_puts_the_hint_after_the_text_and_clips_it_too() {
        let line = line_with_hint("exa", "mple.com/a/long/path");
        let view = line.view(8);
        assert_eq!(view.text, "exa");
        assert_eq!(view.hint, "mple.");
        assert_eq!(view.cursor, 3);
    }

    #[test]
    fn the_cursor_column_is_always_inside_the_room() {
        fn check(line: &Line) {
            for room in 1..=20 {
                let view = line.view(room);
                assert!(view.cursor < room, "{room}: {view:?} at {}", line.cursor());
                assert!(
                    screen::width(&view.text) + screen::width(&view.hint) <= room,
                    "{room}: {view:?}"
                );
            }
        }
        let mixed = "ab\u{65e5}c\u{e9}d\u{301}\u{1f1ef}\u{1f1f5}/path?q=\u{8a9e}\u{8a9e}xyz";
        let mut line = holding(mixed);
        line.step(&key(Key::Home, 0));
        loop {
            check(&line);
            let at = line.cursor();
            line.step(&key(Key::Right, 0));
            if line.cursor() == at {
                break;
            }
        }
        // With a suggestion after the end, which is the only place one is.
        let mut line = holding(mixed);
        line.suggest(Some("\u{65e5}more".to_string()));
        check(&line);
        // And a room of nothing is a room of one: the cursor is somewhere.
        assert_eq!(holding(mixed).view(0).cursor, 0);
    }
}
