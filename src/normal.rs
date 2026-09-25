//! Normal mode: the keys that browse without a mouse, and the word on the
//! row that says the keyboard is in it
//! ([#13](https://github.com/m96-chan/blinkterm/issues/13)).
//!
//! A mode because a letter cannot be two things at once. Every key this
//! program does not keep for itself goes to the page, which is what makes a
//! page's own shortcuts and every text field work; a key that scrolls or
//! labels the links has to be a key the page does not get, and the only way
//! to have both is to say which of the two the keyboard is doing. Opt-in,
//! because a person who never asks for it should find nothing changed:
//! `ctrl+.` turns it on and off, and the program starts in insert mode —
//! every key the page's — unless `Options::normal_mode` says otherwise. The
//! mode is the program's, not a tab's, as the find prompt is: one person, one
//! keyboard, one word on the row; and a navigation keeps it.
//!
//! `ctrl+.` because it is free: in the Kitty keyboard protocol it arrives as
//! `CSI 46;5u`, the period with ctrl, and none of the four terminals this
//! program is run in binds it by default — tOS binds `shift+.` only under
//! its `ctrl+a` leader, Kitty's `move_tab_forward` is `ctrl+shift+.`,
//! Ghostty's keys are on the comma, and WezTerm has nothing on the period. In
//! a terminal without the protocol it is a bare `.`, which is the same story
//! `ctrl+=` has and the same answer: the program asks for the protocol.
//!
//! The scroll keys are wheel notches on the animator thread, never keys to
//! the page. Measured against `chrome-headless-shell` 153: `End`, `PageDown`
//! and Space dispatched as keys do scroll, but with an `<input>` focused `End`
//! moves the caret and scrolls nothing, and a dispatched `j` types a `j`. A
//! wheel event scrolls whatever has focus; `deltaY: 1e7` lands at
//! `scrollHeight - innerHeight` in one frame, clamped by the engine, which
//! is what [`FAR`] is for; and half a viewport is one notch of that distance.
//!
//! In normal mode a letter with no binding does nothing, rather than typing
//! into whatever the page has focused: a letter there is a command or a
//! mistake. Named keys — arrows, Tab, Enter, Space, Backspace, Home and End,
//! the page keys, the F-keys — and every key with a modifier still go to the
//! page, because Tab and Enter are how a page is walked without a mouse
//! today and a page's `ctrl` shortcuts are the page's.

use crate::input::{Key, KeyAction, KeyInput};

/// How far `gg`/`G` ask the page to go: past any page's end, clamped by the
/// engine in one frame (measured 13 479 of a 13 839 pixel page with 1e7).
pub const FAR: f64 = 1.0e7;

/// Insert, or normal with perhaps a `g` waiting for its second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mode {
    pub normal: bool,
    /// A `g` has been pressed and the next key decides.
    pending_g: bool,
}

/// A scroll asked for from the keyboard, in units the loop turns into CSS
/// pixels with the tab's [`crate::zoom::Viewport`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scroll {
    /// One wheel notch, `-1` up or `1` down.
    Notch(i32),
    /// Half a viewport, `-1` up or `1` down.
    HalfPage(i32),
    /// The top or the bottom: a notch of `∓FAR`.
    End(i32),
}

/// What a key means in normal mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// Not a normal-mode key: the page's, as in insert mode.
    Page,
    /// A letter with no binding, or the first `g`: swallowed.
    Nothing,
    Scroll(Scroll),
    /// `f`/`F`: show hints; `true` opens links in a new tab.
    Hints {
        new_tab: bool,
    },
    Back,
    Forward,
    Reload,
    /// `o`: the url bar with the url selected.
    OpenUrl,
    /// `O`: a new tab, cursor in the bar.
    NewTab,
    /// `/`: the find prompt.
    Find,
    /// `i`.
    Insert,
}

impl Mode {
    /// Normal from the start when `normal` says so, insert otherwise.
    pub fn starting(normal: bool) -> Mode {
        Mode {
            normal,
            pending_g: false,
        }
    }

    /// `ctrl+.`: the other mode; a pending `g` is forgotten.
    pub fn toggle(&mut self) {
        self.normal = !self.normal;
        self.pending_g = false;
    }

    /// Back to typing into the page.
    pub fn insert(&mut self) {
        self.normal = false;
        self.pending_g = false;
    }

    /// What `key` means now. Always [`Action::Page`] in insert mode; a repeat
    /// is a press, so a held `j` keeps scrolling.
    ///
    /// A release goes where its press went: the page's keys are released on
    /// the page, which saw them go down, and a letter's release is
    /// [`Action::Nothing`], since the press already acted and the page never
    /// saw it. A release forgets nothing, so the `g` of a `gg` typed quickly
    /// is still waiting when the first one comes up.
    ///
    /// A shifted letter is told by the text the terminal reported, then by
    /// shift on the lower-case key — the rule [`crate::keys`] follows too.
    /// A pending `g` is forgotten by every press that is not the second `g`,
    /// and that press is handled as itself.
    pub fn step(&mut self, key: &KeyInput) -> Action {
        if !self.normal {
            return Action::Page;
        }
        let release = key.action == KeyAction::Release;
        let pending_g = if release {
            false
        } else {
            std::mem::take(&mut self.pending_g)
        };
        if key.mods.ctrl() || key.mods.alt() || key.mods.meta() {
            return Action::Page;
        }
        let Key::Char(c) = key.key else {
            return Action::Page;
        };
        let c = match key.text {
            Some(text) => text,
            None if key.mods.shift() => c.to_ascii_uppercase(),
            None => c,
        };
        if release {
            return if c == ' ' {
                Action::Page
            } else {
                Action::Nothing
            };
        }
        match c {
            // Space is a named key as far as a page is concerned: it pages
            // down, or presses the button that has focus.
            ' ' => Action::Page,
            'f' => Action::Hints { new_tab: false },
            'F' => Action::Hints { new_tab: true },
            'j' => Action::Scroll(Scroll::Notch(1)),
            'k' => Action::Scroll(Scroll::Notch(-1)),
            'd' => Action::Scroll(Scroll::HalfPage(1)),
            'u' => Action::Scroll(Scroll::HalfPage(-1)),
            'g' if pending_g => Action::Scroll(Scroll::End(-1)),
            'g' => {
                self.pending_g = true;
                Action::Nothing
            }
            'G' => Action::Scroll(Scroll::End(1)),
            'H' => Action::Back,
            'L' => Action::Forward,
            'r' => Action::Reload,
            'o' => Action::OpenUrl,
            'O' => Action::NewTab,
            '/' => Action::Find,
            'i' => Action::Insert,
            _ => Action::Nothing,
        }
    }

    /// The word on the row: `None` in insert, `normal`, `normal g`.
    pub fn word(&self) -> Option<&'static str> {
        match (self.normal, self.pending_g) {
            (false, _) => None,
            (true, false) => Some("normal"),
            (true, true) => Some("normal g"),
        }
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

    /// A shifted letter as the Kitty protocol with alternate keys reports
    /// it: the unshifted key, shift held, and the text it made.
    fn shifted(c: char) -> KeyInput {
        KeyInput {
            key: Key::Char(c.to_ascii_lowercase()),
            mods: Mods(Mods::SHIFT),
            action: KeyAction::Press,
            text: Some(c),
        }
    }

    #[test]
    fn insert_mode_hands_every_key_to_the_page() {
        let mut mode = Mode::starting(false);
        for input in [typed('f'), typed('j'), shifted('G'), key(Key::Escape, 0)] {
            assert_eq!(mode.step(&input), Action::Page, "{input:?}");
        }
        assert_eq!(mode.word(), None);
    }

    #[test]
    fn the_normal_keys_and_what_they_do() {
        let table = [
            (typed('f'), Action::Hints { new_tab: false }),
            (shifted('F'), Action::Hints { new_tab: true }),
            (typed('F'), Action::Hints { new_tab: true }),
            (
                key(Key::Char('f'), Mods::SHIFT),
                Action::Hints { new_tab: true },
            ),
            (typed('j'), Action::Scroll(Scroll::Notch(1))),
            (typed('k'), Action::Scroll(Scroll::Notch(-1))),
            (typed('d'), Action::Scroll(Scroll::HalfPage(1))),
            (typed('u'), Action::Scroll(Scroll::HalfPage(-1))),
            (shifted('G'), Action::Scroll(Scroll::End(1))),
            (shifted('H'), Action::Back),
            (shifted('L'), Action::Forward),
            (typed('r'), Action::Reload),
            (typed('o'), Action::OpenUrl),
            (shifted('O'), Action::NewTab),
            (typed('/'), Action::Find),
            (typed('i'), Action::Insert),
            (key(Key::Escape, 0), Action::Page),
        ];
        for (input, wanted) in table {
            let mut mode = Mode::starting(true);
            assert_eq!(mode.step(&input), wanted, "{input:?}");
        }
    }

    #[test]
    fn gg_is_two_presses_and_any_key_between_forgets_the_first() {
        let mut mode = Mode::starting(true);
        assert_eq!(mode.step(&typed('g')), Action::Nothing);
        assert_eq!(mode.word(), Some("normal g"));
        assert_eq!(mode.step(&typed('g')), Action::Scroll(Scroll::End(-1)));
        assert_eq!(mode.word(), Some("normal"));

        assert_eq!(mode.step(&typed('g')), Action::Nothing);
        assert_eq!(mode.step(&typed('j')), Action::Scroll(Scroll::Notch(1)));
        assert_eq!(mode.word(), Some("normal"));
        assert_eq!(mode.step(&typed('g')), Action::Nothing, "a first g again");

        let mut mode = Mode::starting(true);
        assert_eq!(mode.step(&shifted('G')), Action::Scroll(Scroll::End(1)));
        // A release between the two is not a key between them.
        assert_eq!(mode.step(&typed('g')), Action::Nothing);
        let mut released = typed('g');
        released.action = KeyAction::Release;
        assert_eq!(mode.step(&released), Action::Nothing);
        assert_eq!(mode.step(&typed('g')), Action::Scroll(Scroll::End(-1)));
    }

    #[test]
    fn named_keys_and_shortcuts_reach_the_page_and_stray_letters_do_not() {
        let mut mode = Mode::starting(true);
        for input in [
            typed(' '),
            key(Key::Enter, 0),
            key(Key::Tab, 0),
            key(Key::Left, 0),
            key(Key::PageDown, 0),
            key(Key::Function(5), 0),
            key(Key::Backspace, 0),
            key(Key::Char('a'), Mods::CTRL),
            key(Key::Char('x'), Mods::ALT),
            key(Key::Char('j'), Mods::SUPER),
            key(Key::Other(57441), 0),
        ] {
            assert_eq!(mode.step(&input), Action::Page, "{input:?}");
            // And comes up where it went down.
            let mut released = input.clone();
            released.action = KeyAction::Release;
            assert_eq!(mode.step(&released), Action::Page, "{released:?}");
        }
        for c in ['x', '1', ';', '日'] {
            assert_eq!(mode.step(&typed(c)), Action::Nothing, "{c}");
        }
    }

    #[test]
    fn a_release_does_nothing_and_a_repeat_scrolls_again() {
        let mut mode = Mode::starting(true);
        let mut released = typed('j');
        released.action = KeyAction::Release;
        assert_eq!(mode.step(&released), Action::Nothing);
        let mut repeat = typed('j');
        repeat.action = KeyAction::Repeat;
        assert_eq!(mode.step(&repeat), Action::Scroll(Scroll::Notch(1)));
        assert_eq!(mode.step(&repeat), Action::Scroll(Scroll::Notch(1)));
    }

    #[test]
    fn the_toggle_forgets_a_pending_g_and_the_word_follows_the_mode() {
        let mut mode = Mode::default();
        assert_eq!(mode.word(), None);
        mode.toggle();
        assert_eq!(mode.word(), Some("normal"));
        mode.step(&typed('g'));
        assert_eq!(mode.word(), Some("normal g"));
        mode.toggle();
        assert_eq!(mode.word(), None);
        mode.toggle();
        assert_eq!(mode.word(), Some("normal"));
        assert_eq!(
            mode.step(&typed('g')),
            Action::Nothing,
            "a first g, not a second"
        );
        mode.insert();
        assert_eq!(mode.word(), None);
        assert_eq!(mode.step(&typed('j')), Action::Page);
    }
}
