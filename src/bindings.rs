//! `key.<chord> = <action>`: what a person may rebind, and how a chord is
//! spelled.
//!
//! The keys this program keeps for itself are a `match` in `app.rs`
//! (`command`), and it stays one: two other changes in flight add commands
//! and arms to it, and a table that replaced the function would collide with
//! both. So what a config file says about keys is kept beside that function
//! rather than instead of it — a list of rows, looked up first, whose answer
//! is a command, "not a command even if the built-in table says so", or
//! nothing said, in which case the built-in table answers as it always did.
//! That is [`Lookup`], and wiring it in is one line at the one place a key
//! becomes a command.
//!
//! That line is not in yet. Until it is, a `key.` line in the config file is
//! read, checked with everything here — so a misspelt chord is still named
//! with its line number — and then refused as not remappable yet, because a
//! file that is read and quietly ignored is worse than one that says so.
//!
//! # A chord is exact
//!
//! [`Chord::matches`] wants the same key and the same four modifier bits,
//! no more and no fewer. The built-in table reads shift only for `tab`, and
//! a table of rows has to be either exact or a language; a row for `ctrl+tab`
//! that also caught `ctrl+shift+tab` would take the previous-tab key away
//! with the next-tab one. Caps lock and num lock, which the Kitty protocol
//! reports as modifier bits too, are not part of any chord and are ignored.
//!
//! `+` is the separator, so a chord that means the `+` key spells it `plus`;
//! and a space cannot be written at the end of a line that is trimmed, so it
//! is `space`. Everything else that is one character is itself: `alt+=`,
//! `ctrl+-`, `ctrl+/`.

use crate::input::{Key, KeyAction, KeyInput, Mods};

/// The modifier bits a chord is made of: shift, alt, ctrl and super. The
/// protocol's other bits — hyper, meta, caps lock, num lock — are not.
const CHORD_MODS: u32 = Mods::SHIFT | Mods::ALT | Mods::CTRL | Mods::SUPER;

/// The keys with names, as a chord spells them and as [`Key`] has them.
/// `esc` is also read as `escape`; see [`Chord::parse`].
const NAMED: [(&str, Key); 16] = [
    ("tab", Key::Tab),
    ("enter", Key::Enter),
    ("esc", Key::Escape),
    ("backspace", Key::Backspace),
    ("insert", Key::Insert),
    ("delete", Key::Delete),
    ("up", Key::Up),
    ("down", Key::Down),
    ("left", Key::Left),
    ("right", Key::Right),
    ("home", Key::Home),
    ("end", Key::End),
    ("pageup", Key::PageUp),
    ("pagedown", Key::PageDown),
    ("plus", Key::Char('+')),
    ("space", Key::Char(' ')),
];

/// A key with its modifiers, as written: `ctrl+shift+tab`, `alt+=`, `f5`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

impl Chord {
    /// `mod+…+key`, case-insensitive for the words. Modifiers: `ctrl`,
    /// `alt`, `shift`, `super` (also `meta`, `cmd`). Keys: one character
    /// (`a`, `=`, `-`, `/`; `plus` for `+`, `space` for ` `), or `tab`,
    /// `enter`, `esc`/`escape`, `backspace`, `insert`, `delete`, `up`,
    /// `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`,
    /// `f1`…`f24`. An error names what was not understood.
    ///
    /// A letter is kept lower-case whatever case it was written in: the
    /// keyboard protocol reports the key, not the character shift made of it,
    /// and `ctrl+Q` meaning something `ctrl+q` does not would be a trap.
    pub fn parse(text: &str) -> Result<Chord, String> {
        let text = text.trim();
        let mut parts: Vec<&str> = text.split('+').collect();
        let key = parts.pop().unwrap_or_default();
        if key.is_empty() {
            return Err(if text.is_empty() {
                "a chord needs a key".to_string()
            } else {
                format!("{text}: no key after the last +")
            });
        }
        let mut mods = Mods::default();
        for word in parts {
            let bit = match word.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => Mods::CTRL,
                "alt" | "opt" | "option" => Mods::ALT,
                "shift" => Mods::SHIFT,
                "super" | "meta" | "cmd" => Mods::SUPER,
                "" => return Err(format!("{text}: an empty modifier between two +")),
                _ => {
                    return Err(format!(
                        "{text}: {word:?} is not a modifier; ctrl, alt, shift or super"
                    ))
                }
            };
            if mods.0 & bit != 0 {
                return Err(format!("{text}: {word} twice"));
            }
            mods = mods.with(bit);
        }
        let key = parse_key(key).ok_or_else(|| format!("{text}: {key:?} is not a key"))?;
        Ok(Chord { mods, key })
    }

    /// The spelling `parse` reads, modifiers in one fixed order.
    pub fn spell(&self) -> String {
        let mut words = Vec::new();
        for (bit, word) in [
            (Mods::CTRL, "ctrl"),
            (Mods::ALT, "alt"),
            (Mods::SHIFT, "shift"),
            (Mods::SUPER, "super"),
        ] {
            if self.mods.0 & bit != 0 {
                words.push(word.to_string());
            }
        }
        words.push(spell_key(self.key));
        words.join("+")
    }

    /// Whether `input` is this chord: same key, same ctrl, alt, shift and
    /// super bits. Exact on purpose; see the module's section on it.
    pub fn matches(&self, input: &KeyInput) -> bool {
        let key = match input.key {
            Key::Char(c) => Key::Char(c.to_ascii_lowercase()),
            other => other,
        };
        key == self.key && input.mods.0 & CHORD_MODS == self.mods.0 & CHORD_MODS
    }
}

/// One key word, or `None`.
fn parse_key(word: &str) -> Option<Key> {
    let mut chars = word.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(Key::Char(c.to_ascii_lowercase()));
    }
    let lower = word.to_ascii_lowercase();
    if lower == "escape" {
        return Some(Key::Escape);
    }
    if let Some((_, key)) = NAMED.iter().find(|(name, _)| *name == lower) {
        return Some(*key);
    }
    let number: u8 = lower.strip_prefix('f')?.parse().ok()?;
    (1..=24).contains(&number).then_some(Key::Function(number))
}

/// How [`parse_key`] reads `key` back.
fn spell_key(key: Key) -> String {
    if let Some((name, _)) = NAMED.iter().find(|(_, named)| *named == key) {
        return name.to_string();
    }
    match key {
        Key::Char(c) => c.to_string(),
        Key::Function(n) => format!("f{n}"),
        // Never made by `parse_key`; spelt so that it at least says what it is.
        other => format!("{other:?}").to_ascii_lowercase(),
    }
}

/// The commands that have names. One per command in `app.rs` that a person
/// could want on a different key; `tab-1`…`tab-9` are one command with a
/// number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    EditUrl,
    Reload,
    Back,
    Forward,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    /// The nth tab, counted from one, one to nine.
    Tab(usize),
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Find,
    Copy,
    CopyUrl,
}

/// Every action with a fixed name, in the order `--help` lists them.
const ACTIONS: [(&str, Action); 15] = [
    ("quit", Action::Quit),
    ("url", Action::EditUrl),
    ("reload", Action::Reload),
    ("back", Action::Back),
    ("forward", Action::Forward),
    ("new-tab", Action::NewTab),
    ("close-tab", Action::CloseTab),
    ("next-tab", Action::NextTab),
    ("previous-tab", Action::PreviousTab),
    ("zoom-in", Action::ZoomIn),
    ("zoom-out", Action::ZoomOut),
    ("zoom-reset", Action::ZoomReset),
    ("find", Action::Find),
    ("copy", Action::Copy),
    ("copy-url", Action::CopyUrl),
];

impl Action {
    /// `quit`, `url`, `reload`, `back`, `forward`, `new-tab`, `close-tab`,
    /// `next-tab`, `previous-tab`, `tab-1`…`tab-9`, `zoom-in`, `zoom-out`,
    /// `zoom-reset`, `find`, `copy`, `copy-url`. Unknown: an error listing
    /// them.
    pub fn parse(text: &str) -> Result<Action, String> {
        let lower = text.trim().to_ascii_lowercase();
        if let Some((_, action)) = ACTIONS.iter().find(|(name, _)| *name == lower) {
            return Ok(*action);
        }
        if let Some(n) = lower.strip_prefix("tab-").and_then(|n| n.parse().ok()) {
            if (1..=9).contains(&n) {
                return Ok(Action::Tab(n));
            }
        }
        Err(format!(
            "{text:?} is not an action; they are {}, tab-1..tab-9, or none",
            ACTIONS.map(|(name, _)| name).join(", ")
        ))
    }

    /// The name `parse` reads.
    pub fn name(self) -> String {
        if let Action::Tab(n) = self {
            return format!("tab-{n}");
        }
        ACTIONS
            .iter()
            .find(|(_, action)| *action == self)
            .map(|(name, _)| name.to_string())
            .unwrap_or_default()
    }
}

/// One line of the file: `None` is `= none`, the chord unbound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub chord: Chord,
    pub action: Option<Action>,
}

impl Binding {
    /// `key.<chord> = <value>`'s two halves, `chord` without the `key.`.
    pub fn parse(chord: &str, value: &str) -> Result<Binding, String> {
        let chord = Chord::parse(chord).map_err(|why| format!("key.{why}"))?;
        let action = if value.trim().eq_ignore_ascii_case("none") {
            None
        } else {
            Some(Action::parse(value)?)
        };
        Ok(Binding { chord, action })
    }
}

/// Every `key.` line, in file order.
///
/// A later line for the same chord replaces an earlier one, so that a file
/// can be appended to, and that is not a duplicate the parser refuses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bindings {
    rows: Vec<Binding>,
}

/// What the rows say about one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// This command, whatever the built-in table says.
    Bound(Action),
    /// No command, even if the built-in table has one: the key is the
    /// page's.
    Unbound,
    /// Nothing said; the built-in table answers.
    Default,
}

impl Bindings {
    pub fn from_rows(rows: Vec<Binding>) -> Bindings {
        Bindings { rows }
    }

    /// What the rows say about `input`. A release is never a command, as the
    /// built-in table never makes one of it, so it is `Default` before any
    /// row is looked at.
    pub fn lookup(&self, input: &KeyInput) -> Lookup {
        if input.action == KeyAction::Release {
            return Lookup::Default;
        }
        match self.rows.iter().rev().find(|row| row.chord.matches(input)) {
            Some(Binding {
                action: Some(action),
                ..
            }) => Lookup::Bound(*action),
            Some(Binding { action: None, .. }) => Lookup::Unbound,
            None => Lookup::Default,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Binding> {
        self.rows.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key, mods: u32) -> KeyInput {
        KeyInput {
            key,
            mods: Mods(mods),
            action: KeyAction::Press,
            text: None,
        }
    }

    #[test]
    fn every_chord_the_defaults_use_parses_and_spells_back_the_same() {
        for spelled in [
            "ctrl+q",
            "ctrl+l",
            "ctrl+r",
            "ctrl+t",
            "ctrl+w",
            "ctrl+f",
            "ctrl+tab",
            "ctrl+shift+tab",
            "ctrl+=",
            "ctrl+plus",
            "ctrl+-",
            "ctrl+_",
            "ctrl+0",
            "alt+left",
            "alt+right",
            "alt+1",
            "alt+9",
            "alt+c",
            "alt+u",
            "alt+=",
            "alt+-",
            "alt+0",
        ] {
            let chord = Chord::parse(spelled).expect(spelled);
            assert_eq!(chord.spell(), spelled);
        }
        assert_eq!(
            Chord::parse("ctrl+shift+tab"),
            Ok(Chord {
                mods: Mods(Mods::CTRL | Mods::SHIFT),
                key: Key::Tab
            })
        );
    }

    #[test]
    fn modifiers_are_case_insensitive_and_in_any_order() {
        let wanted = Chord::parse("ctrl+shift+tab").expect("a chord");
        for spelled in ["shift+ctrl+tab", "CTRL+Shift+TAB", "Control+shift+Tab"] {
            assert_eq!(Chord::parse(spelled), Ok(wanted), "{spelled}");
        }
        assert_eq!(Chord::parse("ctrl+Q"), Chord::parse("ctrl+q"));
        for spelled in ["super+a", "meta+a", "cmd+a"] {
            assert_eq!(
                Chord::parse(spelled).map(|c| c.mods),
                Ok(Mods(Mods::SUPER)),
                "{spelled}"
            );
        }
    }

    #[test]
    fn plus_and_space_are_words_because_they_cannot_be_written_bare() {
        assert_eq!(Chord::parse("alt+plus").map(|c| c.key), Ok(Key::Char('+')));
        assert_eq!(
            Chord::parse("ctrl+space").map(|c| c.key),
            Ok(Key::Char(' '))
        );
        assert_eq!(Chord::parse("ctrl+ ").map(|c| c.spell()).ok(), None);
        let spelled = Chord {
            mods: Mods(Mods::ALT),
            key: Key::Char('+'),
        }
        .spell();
        assert_eq!(spelled, "alt+plus");
    }

    #[test]
    fn a_named_key_is_one_of_the_list_and_f13_is_a_function_key() {
        for (spelled, key) in [
            ("f13", Key::Function(13)),
            ("F5", Key::Function(5)),
            ("f24", Key::Function(24)),
            ("escape", Key::Escape),
            ("esc", Key::Escape),
            ("pagedown", Key::PageDown),
            ("enter", Key::Enter),
        ] {
            assert_eq!(Chord::parse(spelled).map(|c| c.key), Ok(key), "{spelled}");
        }
        for wrong in ["f0", "f25", "fx", "return", "pgdn"] {
            let why = Chord::parse(wrong).expect_err(wrong);
            assert!(why.contains("is not a key"), "{wrong}: {why}");
        }
    }

    #[test]
    fn a_chord_with_no_key_or_an_unknown_modifier_says_which() {
        assert_eq!(
            Chord::parse("ctrl+"),
            Err("ctrl+: no key after the last +".to_string())
        );
        assert_eq!(
            Chord::parse("hyper+a"),
            Err("hyper+a: \"hyper\" is not a modifier; ctrl, alt, shift or super".to_string())
        );
        assert!(Chord::parse("ctrl+ctrl+a")
            .expect_err("twice")
            .contains("twice"));
        assert!(Chord::parse("").is_err());
    }

    #[test]
    fn every_action_has_a_name_that_parses_back_to_it() {
        let mut every: Vec<Action> = ACTIONS.iter().map(|(_, action)| *action).collect();
        every.extend((1..=9).map(Action::Tab));
        for action in every {
            assert_eq!(Action::parse(&action.name()), Ok(action), "{action:?}");
        }
        assert_eq!(Action::parse("url"), Ok(Action::EditUrl));
        assert_eq!(Action::parse("Tab-3"), Ok(Action::Tab(3)));
        for wrong in ["tab-0", "tab-10", "bookmark"] {
            let why = Action::parse(wrong).expect_err(wrong);
            assert!(why.contains("is not an action"), "{why}");
            assert!(
                why.contains("copy-url"),
                "the list is in the sentence: {why}"
            );
        }
    }

    #[test]
    fn a_chord_matches_the_key_with_exactly_its_modifiers() {
        let chord = |text| Chord::parse(text).expect(text);
        assert!(chord("ctrl+tab").matches(&key(Key::Tab, Mods::CTRL)));
        assert!(!chord("ctrl+tab").matches(&key(Key::Tab, Mods::CTRL | Mods::SHIFT)));
        assert!(!chord("ctrl+=").matches(&key(Key::Char('='), Mods::CTRL | Mods::SHIFT)));
        assert!(!chord("alt+1").matches(&key(Key::Char('1'), Mods::ALT | Mods::SUPER)));
        assert!(!chord("alt+1").matches(&key(Key::Char('1'), 0)));
        // Caps lock (64) and num lock (128) are not part of a chord.
        assert!(chord("alt+1").matches(&key(Key::Char('1'), Mods::ALT | 64 | 128)));
        assert!(chord("ctrl+q").matches(&key(Key::Char('Q'), Mods::CTRL)));
    }

    fn row(chord: &str, action: Option<Action>) -> Binding {
        Binding {
            chord: Chord::parse(chord).expect(chord),
            action,
        }
    }

    #[test]
    fn a_later_row_for_the_same_chord_replaces_an_earlier_one() {
        let table = Bindings::from_rows(vec![
            row("alt+b", Some(Action::Back)),
            row("alt+b", Some(Action::Forward)),
        ]);
        assert_eq!(
            table.lookup(&key(Key::Char('b'), Mods::ALT)),
            Lookup::Bound(Action::Forward)
        );
        assert_eq!(table.iter().count(), 2, "both lines are kept, in order");
    }

    #[test]
    fn none_unbinds_and_an_empty_table_answers_default_for_everything() {
        let empty = Bindings::default();
        assert!(empty.is_empty());
        for input in [
            key(Key::Char('q'), Mods::CTRL),
            key(Key::Tab, Mods::CTRL),
            key(Key::Char('a'), 0),
        ] {
            assert_eq!(empty.lookup(&input), Lookup::Default, "{input:?}");
        }
        let table = Bindings::from_rows(vec![
            Binding::parse("ctrl+q", "none").expect("a row"),
            Binding::parse("alt+b", "back").expect("a row"),
        ]);
        assert_eq!(
            table.lookup(&key(Key::Char('q'), Mods::CTRL)),
            Lookup::Unbound
        );
        assert_eq!(
            table.lookup(&key(Key::Char('b'), Mods::ALT)),
            Lookup::Bound(Action::Back)
        );
        assert_eq!(
            table.lookup(&key(Key::Char('w'), Mods::CTRL)),
            Lookup::Default
        );
    }

    #[test]
    fn a_release_is_never_a_command_whatever_the_table_says() {
        let table = Bindings::from_rows(vec![row("alt+b", Some(Action::Back))]);
        let mut released = key(Key::Char('b'), Mods::ALT);
        released.action = KeyAction::Release;
        assert_eq!(table.lookup(&released), Lookup::Default);
    }

    #[test]
    fn a_binding_names_its_chord_when_the_chord_is_wrong() {
        assert_eq!(
            Binding::parse("ctrl+", "back"),
            Err("key.ctrl+: no key after the last +".to_string())
        );
        let why = Binding::parse("ctrl+a", "bookmark").expect_err("not an action");
        assert!(why.starts_with("\"bookmark\" is not an action"), "{why}");
    }
}
