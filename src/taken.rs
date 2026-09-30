//! The chords a terminal or the operating system keeps: what a `key.` line
//! may name and still never receive.
//!
//! A key that hits one of Kitty's mappings never reaches the pane: Kitty runs
//! the action and the program under it hears nothing. So a `key.cmd+t =
//! new-tab` in a Kitty on macOS is a line that does nothing, silently, unless
//! somebody says so — and this module is what says so: at start, on the row,
//! when the terminal is Kitty (`app::run`), and in `--doctor`'s `keys:` lines
//! whatever the terminal is. It is also what the Mac keymap's own chords are
//! held to, by a test in [`crate::bindings`].
//!
//! # Where the tables come from
//!
//! Kitty's are its defaults, dumped from Kitty 0.45.0 with `kitty +runpy`
//! over `kitty.options.definition` — every `map` with `add_to_default`, with
//! `kitty_mod` at its default of `ctrl+shift` and a stock `kitty.conf`.
//! [`KITTY_EVERYWHERE`] is the ones on every platform, [`KITTY_MACOS`] the
//! ones added on macOS. A chord prefix (`ctrl+shift+a`, `ctrl+shift+p`) is
//! listed as its first chord, which is what Kitty takes. `kitty_mod+kp_add`
//! and `kitty_mod+kp_subtract` are keypad keys, which a [`Chord`] cannot
//! spell, and are left out.
//!
//! What gives a chord back is `map <chord> no_op` in `kitty.conf` — Kitty
//! then has a mapping that does nothing and passes the key on — which is the
//! line [`unmap_line`] writes. `discard_event` is the opposite and swallows
//! it.
//!
//! [`MACOS_SYSTEM`] is macOS's own: the System Settings defaults that are
//! taken before any terminal sees a key. A person may have turned any of
//! them off, and `ctrl+space` only exists with two input sources, so every
//! sentence about them says "by default" and names the settings pane.
//!
//! Nothing here reads the person's `kitty.conf`, which may have unmapped the
//! key already: the sentences say "by default" for that reason too. Tables
//! for Ghostty, WezTerm and iTerm2 on macOS would be another [`Keeper`].

use crate::bindings::{Bindings, Chord};
use crate::input::{Key, Mods};

/// Who keeps a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keeper {
    Kitty,
    MacOs,
}

/// Kitty's default mappings on every platform: the chord as
/// [`Chord::parse`] reads it, and what Kitty does with it.
pub const KITTY_EVERYWHERE: &[(&str, &str)] = &[
    ("ctrl+shift+c", "copy_to_clipboard"),
    ("ctrl+shift+v", "paste_from_clipboard"),
    ("ctrl+shift+s", "paste_from_selection"),
    ("shift+insert", "paste_from_selection"),
    ("ctrl+shift+o", "pass_selection_to_program"),
    ("ctrl+shift+up", "scroll_line_up"),
    ("ctrl+shift+k", "scroll_line_up"),
    ("ctrl+shift+down", "scroll_line_down"),
    ("ctrl+shift+j", "scroll_line_down"),
    ("ctrl+shift+pageup", "scroll_page_up"),
    ("ctrl+shift+pagedown", "scroll_page_down"),
    ("ctrl+shift+home", "scroll_home"),
    ("ctrl+shift+end", "scroll_end"),
    ("ctrl+shift+z", "scroll_to_prompt -1"),
    ("ctrl+shift+x", "scroll_to_prompt 1"),
    ("ctrl+shift+h", "show_scrollback"),
    ("ctrl+shift+g", "show_last_command_output"),
    ("ctrl+shift+/", "search_scrollback"),
    ("ctrl+shift+enter", "new_window"),
    ("ctrl+shift+n", "new_os_window"),
    ("ctrl+shift+w", "close_window"),
    ("ctrl+shift+]", "next_window"),
    ("ctrl+shift+[", "previous_window"),
    ("ctrl+shift+f", "move_window_forward"),
    ("ctrl+shift+b", "move_window_backward"),
    ("ctrl+shift+`", "move_window_to_top"),
    ("ctrl+shift+r", "start_resizing_window"),
    ("ctrl+shift+1", "first_window"),
    ("ctrl+shift+2", "second_window"),
    ("ctrl+shift+3", "third_window"),
    ("ctrl+shift+4", "fourth_window"),
    ("ctrl+shift+5", "fifth_window"),
    ("ctrl+shift+6", "sixth_window"),
    ("ctrl+shift+7", "seventh_window"),
    ("ctrl+shift+8", "eighth_window"),
    ("ctrl+shift+9", "ninth_window"),
    ("ctrl+shift+0", "tenth_window"),
    ("ctrl+shift+f7", "focus_visible_window"),
    ("ctrl+shift+f8", "swap_with_window"),
    ("ctrl+shift+right", "next_tab"),
    ("ctrl+tab", "next_tab"),
    ("ctrl+shift+left", "previous_tab"),
    ("ctrl+shift+tab", "previous_tab"),
    ("ctrl+shift+t", "new_tab"),
    ("ctrl+shift+q", "close_tab"),
    ("ctrl+shift+.", "move_tab_forward"),
    ("ctrl+shift+,", "move_tab_backward"),
    ("ctrl+shift+alt+t", "set_tab_title"),
    ("ctrl+shift+l", "next_layout"),
    ("ctrl+shift+=", "change_font_size +2"),
    ("ctrl+shift+plus", "change_font_size +2"),
    ("ctrl+shift+-", "change_font_size -2"),
    ("ctrl+shift+backspace", "change_font_size 0"),
    ("ctrl+shift+e", "open_url_with_hints"),
    ("ctrl+shift+p", "the hints kittens, a chord prefix"),
    ("ctrl+shift+f1", "show_kitty_doc"),
    ("ctrl+shift+f11", "toggle_fullscreen"),
    ("ctrl+shift+f10", "toggle_maximized"),
    ("ctrl+shift+u", "unicode_input"),
    ("ctrl+shift+f2", "edit_config_file"),
    ("ctrl+shift+esc", "kitty_shell"),
    ("ctrl+shift+a", "set_background_opacity, a chord prefix"),
    ("ctrl+shift+delete", "clear_terminal reset"),
    ("ctrl+shift+f5", "load_config_file"),
    ("ctrl+shift+f6", "debug_config"),
];

/// Kitty's default mappings added on macOS, in the same shape.
///
/// `cmd+c` is not here, though Kitty maps it: its action is
/// `copy_or_noop`, which copies a terminal selection and, with none, returns
/// "pass the key on" to Kitty's dispatcher — and with the mouse reported to
/// this program, Kitty has made no selection. So `cmd+c` arrives, and it is
/// the Mac keymap's copy.
pub const KITTY_MACOS: &[(&str, &str)] = &[
    ("cmd+v", "paste_from_clipboard"),
    ("alt+cmd+pageup", "scroll_line_up"),
    ("cmd+up", "scroll_line_up"),
    ("alt+cmd+pagedown", "scroll_line_down"),
    ("cmd+down", "scroll_line_down"),
    ("cmd+pageup", "scroll_page_up"),
    ("cmd+pagedown", "scroll_page_down"),
    ("cmd+home", "scroll_home"),
    ("cmd+end", "scroll_end"),
    ("cmd+f", "search_scrollback"),
    ("cmd+enter", "new_window"),
    ("cmd+n", "new_os_window"),
    ("shift+cmd+d", "close_window"),
    ("cmd+r", "start_resizing_window"),
    ("cmd+1", "first_window"),
    ("cmd+2", "second_window"),
    ("cmd+3", "third_window"),
    ("cmd+4", "fourth_window"),
    ("cmd+5", "fifth_window"),
    ("cmd+6", "sixth_window"),
    ("cmd+7", "seventh_window"),
    ("cmd+8", "eighth_window"),
    ("cmd+9", "ninth_window"),
    ("shift+cmd+]", "next_tab"),
    ("shift+cmd+[", "previous_tab"),
    ("cmd+t", "new_tab"),
    ("cmd+w", "close_tab"),
    ("shift+cmd+w", "close_os_window"),
    ("shift+cmd+i", "set_tab_title"),
    ("cmd+plus", "change_font_size +2"),
    ("cmd+=", "change_font_size +2"),
    ("shift+cmd+=", "change_font_size +2"),
    ("cmd+-", "change_font_size -2"),
    ("shift+cmd+-", "change_font_size -2"),
    ("cmd+0", "change_font_size 0"),
    ("ctrl+cmd+f", "toggle_fullscreen"),
    ("alt+cmd+s", "toggle_macos_secure_keyboard_entry"),
    ("cmd+`", "macos_cycle_through_os_windows"),
    ("shift+cmd+`", "macos_cycle_through_os_windows_backwards"),
    ("ctrl+cmd+space", "unicode_input"),
    ("cmd+,", "edit_config_file"),
    ("alt+cmd+r", "clear_terminal reset"),
    ("cmd+k", "clear_terminal to_cursor"),
    ("alt+cmd+k", "clear_terminal scrollback"),
    ("cmd+l", "clear_terminal last_command"),
    ("ctrl+cmd+l", "clear_terminal to_cursor_scroll"),
    ("ctrl+cmd+,", "load_config_file"),
    ("alt+cmd+,", "debug_config"),
    ("shift+cmd+/", "open the kitty website"),
    ("cmd+h", "hide_macos_app"),
    ("alt+cmd+h", "hide_macos_other_apps"),
    ("cmd+m", "minimize_macos_window"),
    ("cmd+q", "quit"),
];

/// macOS's own shortcuts, System Settings' defaults, in the same shape.
pub const MACOS_SYSTEM: &[(&str, &str)] = &[
    ("cmd+tab", "the app switcher"),
    ("cmd+shift+tab", "the app switcher"),
    ("cmd+space", "Spotlight"),
    ("ctrl+space", "the previous input source"),
    ("cmd+shift+3", "a screenshot"),
    ("cmd+shift+4", "a screenshot"),
    ("cmd+shift+5", "a screenshot"),
    ("ctrl+up", "Mission Control"),
    ("ctrl+down", "application windows"),
    ("ctrl+left", "Mission Control: a space left"),
    ("ctrl+right", "Mission Control: a space right"),
    ("ctrl+cmd+q", "lock screen"),
    ("alt+cmd+esc", "force quit"),
];

/// Who keeps `chord` by default, and what it does there; `macos` adds
/// Kitty's macOS mappings and macOS's own to Kitty's everywhere ones.
pub fn keeper(chord: &Chord, macos: bool) -> Option<(Keeper, &'static str)> {
    let find = |table: &'static [(&'static str, &'static str)]| {
        table
            .iter()
            .find(|(spelled, _)| Chord::parse(spelled).as_ref() == Ok(chord))
            .map(|(_, what)| *what)
    };
    if let Some(what) = find(KITTY_EVERYWHERE) {
        return Some((Keeper::Kitty, what));
    }
    if !macos {
        return None;
    }
    if let Some(what) = find(KITTY_MACOS) {
        return Some((Keeper::Kitty, what));
    }
    find(MACOS_SYSTEM).map(|what| (Keeper::MacOs, what))
}

/// `chord` as a person on a Mac writes it: [`Chord::spell`], with `cmd` for
/// super.
fn written(chord: &Chord) -> String {
    chord.spell().replace("super", "cmd")
}

/// `chord` as `kitty.conf` spells it: `cmd` for super, and the key names
/// Kitty uses — `page_up`, `page_down`, `equal`, `minus`, `plus`, `space`,
/// `escape`.
pub fn kitty_spelling(chord: &Chord) -> String {
    let mut words: Vec<String> = [
        (Mods::CTRL, "ctrl"),
        (Mods::ALT, "alt"),
        (Mods::SHIFT, "shift"),
        (Mods::SUPER, "cmd"),
    ]
    .iter()
    .filter(|(bit, _)| chord.mods.0 & bit != 0)
    .map(|(_, word)| word.to_string())
    .collect();
    let key = match chord.key {
        Key::PageUp => "page_up".to_string(),
        Key::PageDown => "page_down".to_string(),
        Key::Escape => "escape".to_string(),
        Key::Char('=') => "equal".to_string(),
        Key::Char('-') => "minus".to_string(),
        Key::Char('+') => "plus".to_string(),
        Key::Char(' ') => "space".to_string(),
        _ => {
            let bare = Chord {
                mods: Mods::default(),
                key: chord.key,
            };
            bare.spell()
        }
    };
    words.push(key);
    words.join("+")
}

/// The `kitty.conf` line that gives `chord` back to the program.
pub fn unmap_line(chord: &Chord) -> String {
    format!("map {} no_op", kitty_spelling(chord))
}

/// One sentence per `key.` line of the file that binds a chord Kitty or
/// macOS keeps by default, in file order. A line that unbinds (`= none`)
/// asks for nothing to arrive and is not one; the keymap's own rows are not
/// the file's and are held clean by a test instead.
pub fn conflicts(bindings: &Bindings, macos: bool) -> Vec<String> {
    bindings
        .iter()
        .filter_map(|binding| {
            let action = binding.action?;
            let (who, what) = keeper(&binding.chord, macos)?;
            let chord = written(&binding.chord);
            let line = format!("key.{chord} = {}", action.name());
            Some(match who {
                Keeper::Kitty => format!(
                    "{line}: Kitty keeps {chord} by default ({what}); put \"{}\" in kitty.conf to pass it through",
                    unmap_line(&binding.chord)
                ),
                Keeper::MacOs => format!(
                    "{line}: macOS keeps {chord} by default ({what}); System Settings > Keyboard > Keyboard Shortcuts turns it off"
                ),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::Binding;

    fn chord(text: &str) -> Chord {
        Chord::parse(text).expect(text)
    }

    #[test]
    fn every_chord_in_the_tables_parses_and_no_platform_lists_one_twice() {
        let mut seen: Vec<Chord> = Vec::new();
        for table in [KITTY_EVERYWHERE, KITTY_MACOS] {
            for (spelled, _) in table {
                let parsed = chord(spelled);
                assert!(!seen.contains(&parsed), "{spelled} twice for Kitty");
                seen.push(parsed);
            }
        }
        let mut system: Vec<Chord> = Vec::new();
        for (spelled, _) in MACOS_SYSTEM {
            let parsed = chord(spelled);
            assert!(!system.contains(&parsed), "{spelled} twice for macOS");
            system.push(parsed);
        }
    }

    #[test]
    fn kitty_keeps_ctrl_tab_everywhere_and_cmd_t_only_on_macos() {
        for macos in [false, true] {
            assert_eq!(
                keeper(&chord("ctrl+tab"), macos),
                Some((Keeper::Kitty, "next_tab")),
                "macos {macos}"
            );
        }
        assert_eq!(keeper(&chord("cmd+t"), false), None);
        assert_eq!(
            keeper(&chord("cmd+t"), true),
            Some((Keeper::Kitty, "new_tab"))
        );
        assert_eq!(
            keeper(&chord("ctrl+left"), true).map(|(who, _)| who),
            Some(Keeper::MacOs)
        );
        assert_eq!(keeper(&chord("ctrl+left"), false), None);
        assert_eq!(
            keeper(&chord("cmd+c"), true),
            None,
            "copy_or_noop passes on"
        );
        assert_eq!(keeper(&chord("cmd+d"), true), None);
    }

    #[test]
    fn a_kitty_conf_line_spells_the_chord_as_kitty_does() {
        for (ours, theirs) in [
            ("cmd+t", "map cmd+t no_op"),
            ("ctrl+shift+pageup", "map ctrl+shift+page_up no_op"),
            ("ctrl+=", "map ctrl+equal no_op"),
            ("cmd+-", "map cmd+minus no_op"),
            ("ctrl+shift+plus", "map ctrl+shift+plus no_op"),
            ("ctrl+shift+esc", "map ctrl+shift+escape no_op"),
            ("ctrl+tab", "map ctrl+tab no_op"),
            ("cmd+alt+left", "map alt+cmd+left no_op"),
        ] {
            assert_eq!(unmap_line(&chord(ours)), theirs, "{ours}");
        }
    }

    #[test]
    fn conflicts_name_each_bound_file_row_kitty_or_macos_keeps_and_nothing_else() {
        let bindings = Bindings::from_rows(vec![
            Binding::parse("cmd+t", "new-tab").expect("a row"),
            Binding::parse("cmd+d", "bookmark").expect("a row"),
            Binding::parse("cmd+w", "none").expect("a row"),
        ]);
        let said = conflicts(&bindings, true);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].starts_with("key.cmd+t = new-tab: Kitty keeps cmd+t"),
            "{}",
            said[0]
        );
        assert!(said[0].contains("\"map cmd+t no_op\""), "{}", said[0]);
        assert!(
            conflicts(&bindings, false).is_empty(),
            "cmd+t is Kitty's on macOS only"
        );
        let system = Bindings::from_rows(vec![Binding::parse("ctrl+left", "back").expect("a row")]);
        let said = conflicts(&system, true);
        assert_eq!(said.len(), 1);
        assert!(
            said[0].contains("macOS keeps ctrl+left by default"),
            "{}",
            said[0]
        );
        assert!(said[0].contains("System Settings"), "{}", said[0]);
    }
}
