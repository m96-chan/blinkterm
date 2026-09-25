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
//! What is deliberately not here is a cursor that moves. The row is one line
//! the person is typing at the end of, as a shell's line is before readline:
//! backspace, `ctrl+u`, and a first key that replaces what was offered. That is
//! the whole of what a url or a prompt's answer usually needs, and a cursor in
//! the middle of a line would need drawing, which a status row has no way to
//! do but by moving the terminal's own — the one thing the row already uses to
//! say where the typing is.

use crate::input::{Key, KeyInput};

/// What a keystroke in the url bar did to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// Still typing.
    Typing,
    /// Escape: leave the url alone.
    Cancel,
    /// Enter: go to what is in the buffer.
    Go,
    /// `ctrl+q`, which quits even from the url bar.
    Quit,
}

/// Apply one keystroke to the buffer. The whole of the url bar's behaviour,
/// with nothing to talk to, so that it can be tested a key at a time.
pub fn edit_step(buffer: &mut String, whole: bool, key: &KeyInput) -> Edit {
    match key.key {
        Key::Escape => Edit::Cancel,
        Key::Enter => Edit::Go,
        Key::Char('q') if key.mods.ctrl() => Edit::Quit,
        Key::Char('u') if key.mods.ctrl() => {
            buffer.clear();
            Edit::Typing
        }
        Key::Backspace => {
            if whole {
                buffer.clear();
            } else {
                buffer.pop();
            }
            Edit::Typing
        }
        _ => {
            if let Some(c) = key.text {
                if whole {
                    buffer.clear();
                }
                buffer.push(c);
            }
            Edit::Typing
        }
    }
}

/// A line and whether all of it is still selected.
///
/// The url bar keeps these two as fields of its own, because it was written
/// before there was a second thing to type into and it has the rest of the
/// loop's state beside them. A prompt has nothing else, so it carries them
/// together, and the one rule that joins them — the selection lasts exactly
/// one key, whatever that key was — is kept in one place rather than in every
/// caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// What has been typed, or what was offered and not yet typed over.
    pub text: String,
    /// Whether `text` is still the whole of what was offered, untouched, so
    /// that the next key replaces it rather than adding to it.
    pub whole: bool,
}

impl Line {
    /// A line offered with all of it selected: the first key replaces it, and
    /// a backspace deletes it, the way a browser treats the address it shows
    /// on `ctrl+l` and the default a `prompt()` offers.
    pub fn selected(text: impl Into<String>) -> Line {
        Line {
            text: text.into(),
            whole: true,
        }
    }

    /// Apply one keystroke. The selection is spent by the first key, whether
    /// or not that key changed anything — an arrow pressed on an offered
    /// default is somebody who has looked at it, and the next thing they type
    /// is an addition.
    pub fn step(&mut self, key: &KeyInput) -> Edit {
        let whole = std::mem::take(&mut self.whole);
        edit_step(&mut self.text, whole, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{KeyAction, Mods};

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

    #[test]
    fn the_url_bar_starts_with_the_whole_address_selected() {
        // ctrl+l, then typing: what is there goes, the way it would in a
        // browser where the address was selected.
        let mut buffer = "https://example.com/a".to_string();
        assert_eq!(edit_step(&mut buffer, true, &typed('x')), Edit::Typing);
        assert_eq!(buffer, "x");
        // And from then on it is ordinary typing.
        edit_step(&mut buffer, false, &typed('y'));
        assert_eq!(buffer, "xy");
        edit_step(&mut buffer, false, &key(Key::Backspace, 0));
        assert_eq!(buffer, "x");

        // A backspace as the first thing deletes the lot, not one character.
        let mut buffer = "https://example.com/a".to_string();
        edit_step(&mut buffer, true, &key(Key::Backspace, 0));
        assert!(buffer.is_empty());

        // ctrl+u empties it whenever.
        let mut buffer = "half typed".to_string();
        edit_step(&mut buffer, false, &key(Key::Char('u'), Mods::CTRL));
        assert!(buffer.is_empty());
    }

    #[test]
    fn the_url_bar_knows_when_it_is_finished() {
        let mut buffer = "example.com".to_string();
        assert_eq!(edit_step(&mut buffer, false, &key(Key::Enter, 0)), Edit::Go);
        assert_eq!(
            buffer, "example.com",
            "enter does not change what was typed"
        );
        assert_eq!(
            edit_step(&mut buffer, false, &key(Key::Escape, 0)),
            Edit::Cancel
        );
        assert_eq!(
            edit_step(&mut buffer, false, &key(Key::Char('q'), Mods::CTRL)),
            Edit::Quit
        );
    }

    #[test]
    fn a_line_forgets_its_selection_after_the_first_key() {
        let mut line = Line::selected("default");
        assert!(line.whole);
        assert_eq!(line.step(&typed('x')), Edit::Typing);
        assert_eq!(line.text, "x", "the first key replaced what was offered");
        assert!(!line.whole);
        line.step(&typed('y'));
        assert_eq!(line.text, "xy", "and the second one added to it");

        // A key that types nothing still spends the selection: whoever
        // pressed it has seen the default, and what comes next is an edit.
        let mut line = Line::selected("default");
        line.step(&key(Key::Left, 0));
        assert!(!line.whole);
        line.step(&key(Key::Backspace, 0));
        assert_eq!(line.text, "defaul", "one character, not the lot");

        // And a backspace first is the lot, as in the url bar.
        let mut line = Line::selected("default");
        line.step(&key(Key::Backspace, 0));
        assert!(line.text.is_empty());
    }
}
