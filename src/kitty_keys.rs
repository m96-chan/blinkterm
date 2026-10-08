//! Kitty 0.45 stock shortcuts, from its generated default kitty.conf.
//! Prefix maps reserve their first chord too. These are deliberately static:
//! doctor does not read a user's terminal configuration or infer a terminal.
use crate::bindings::{Bindings, Chord};

/// A reserved terminal chord; mac-only reservations depend on the host OS,
/// not on which blinkterm keymap was selected.
#[derive(Debug)]
pub struct Reservation {
    pub chord: &'static str,
    pub action: &'static str,
    pub kitty: &'static str,
    pub mac_only: bool,
}
pub const DEFAULTS: &[Reservation] = &[
    Reservation {
        chord: "ctrl+shift+c",
        action: "copy_to_clipboard",
        kitty: "kitty_mod+c",
        mac_only: false,
    },
    Reservation {
        chord: "super+c",
        action: "copy_or_noop",
        kitty: "cmd+c",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+v",
        action: "paste_from_clipboard",
        kitty: "kitty_mod+v",
        mac_only: false,
    },
    Reservation {
        chord: "super+v",
        action: "paste_from_clipboard",
        kitty: "cmd+v",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+s",
        action: "paste_from_selection",
        kitty: "kitty_mod+s",
        mac_only: false,
    },
    Reservation {
        chord: "shift+insert",
        action: "paste_from_selection",
        kitty: "shift+insert",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+o",
        action: "pass_selection_to_program",
        kitty: "kitty_mod+o",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+up",
        action: "scroll_line_up",
        kitty: "kitty_mod+up",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+k",
        action: "scroll_line_up",
        kitty: "kitty_mod+k",
        mac_only: false,
    },
    Reservation {
        chord: "alt+super+pageup",
        action: "scroll_line_up",
        kitty: "opt+cmd+page_up",
        mac_only: true,
    },
    Reservation {
        chord: "super+up",
        action: "scroll_line_up",
        kitty: "cmd+up",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+down",
        action: "scroll_line_down",
        kitty: "kitty_mod+down",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+j",
        action: "scroll_line_down",
        kitty: "kitty_mod+j",
        mac_only: false,
    },
    Reservation {
        chord: "alt+super+pagedown",
        action: "scroll_line_down",
        kitty: "opt+cmd+page_down",
        mac_only: true,
    },
    Reservation {
        chord: "super+down",
        action: "scroll_line_down",
        kitty: "cmd+down",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+pageup",
        action: "scroll_page_up",
        kitty: "kitty_mod+page_up",
        mac_only: false,
    },
    Reservation {
        chord: "super+pageup",
        action: "scroll_page_up",
        kitty: "cmd+page_up",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+pagedown",
        action: "scroll_page_down",
        kitty: "kitty_mod+page_down",
        mac_only: false,
    },
    Reservation {
        chord: "super+pagedown",
        action: "scroll_page_down",
        kitty: "cmd+page_down",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+home",
        action: "scroll_home",
        kitty: "kitty_mod+home",
        mac_only: false,
    },
    Reservation {
        chord: "super+home",
        action: "scroll_home",
        kitty: "cmd+home",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+end",
        action: "scroll_end",
        kitty: "kitty_mod+end",
        mac_only: false,
    },
    Reservation {
        chord: "super+end",
        action: "scroll_end",
        kitty: "cmd+end",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+z",
        action: "scroll_to_prompt -1",
        kitty: "kitty_mod+z",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+x",
        action: "scroll_to_prompt 1",
        kitty: "kitty_mod+x",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+h",
        action: "show_scrollback",
        kitty: "kitty_mod+h",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+g",
        action: "show_last_command_output",
        kitty: "kitty_mod+g",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+/",
        action: "search_scrollback",
        kitty: "kitty_mod+/",
        mac_only: false,
    },
    Reservation {
        chord: "super+f",
        action: "search_scrollback",
        kitty: "cmd+f",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+enter",
        action: "new_window",
        kitty: "kitty_mod+enter",
        mac_only: false,
    },
    Reservation {
        chord: "super+enter",
        action: "new_window",
        kitty: "cmd+enter",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+n",
        action: "new_os_window",
        kitty: "kitty_mod+n",
        mac_only: false,
    },
    Reservation {
        chord: "super+n",
        action: "new_os_window",
        kitty: "cmd+n",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+w",
        action: "close_window",
        kitty: "kitty_mod+w",
        mac_only: false,
    },
    Reservation {
        chord: "shift+super+d",
        action: "close_window",
        kitty: "shift+cmd+d",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+]",
        action: "next_window",
        kitty: "kitty_mod+]",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+[",
        action: "previous_window",
        kitty: "kitty_mod+[",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+f",
        action: "move_window_forward",
        kitty: "kitty_mod+f",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+b",
        action: "move_window_backward",
        kitty: "kitty_mod+b",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+`",
        action: "move_window_to_top",
        kitty: "kitty_mod+`",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+r",
        action: "start_resizing_window",
        kitty: "kitty_mod+r",
        mac_only: false,
    },
    Reservation {
        chord: "super+r",
        action: "start_resizing_window",
        kitty: "cmd+r",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+1",
        action: "first_window",
        kitty: "kitty_mod+1",
        mac_only: false,
    },
    Reservation {
        chord: "super+1",
        action: "first_window",
        kitty: "cmd+1",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+2",
        action: "second_window",
        kitty: "kitty_mod+2",
        mac_only: false,
    },
    Reservation {
        chord: "super+2",
        action: "second_window",
        kitty: "cmd+2",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+3",
        action: "third_window",
        kitty: "kitty_mod+3",
        mac_only: false,
    },
    Reservation {
        chord: "super+3",
        action: "third_window",
        kitty: "cmd+3",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+4",
        action: "fourth_window",
        kitty: "kitty_mod+4",
        mac_only: false,
    },
    Reservation {
        chord: "super+4",
        action: "fourth_window",
        kitty: "cmd+4",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+5",
        action: "fifth_window",
        kitty: "kitty_mod+5",
        mac_only: false,
    },
    Reservation {
        chord: "super+5",
        action: "fifth_window",
        kitty: "cmd+5",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+6",
        action: "sixth_window",
        kitty: "kitty_mod+6",
        mac_only: false,
    },
    Reservation {
        chord: "super+6",
        action: "sixth_window",
        kitty: "cmd+6",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+7",
        action: "seventh_window",
        kitty: "kitty_mod+7",
        mac_only: false,
    },
    Reservation {
        chord: "super+7",
        action: "seventh_window",
        kitty: "cmd+7",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+8",
        action: "eighth_window",
        kitty: "kitty_mod+8",
        mac_only: false,
    },
    Reservation {
        chord: "super+8",
        action: "eighth_window",
        kitty: "cmd+8",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+9",
        action: "ninth_window",
        kitty: "kitty_mod+9",
        mac_only: false,
    },
    Reservation {
        chord: "super+9",
        action: "ninth_window",
        kitty: "cmd+9",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+0",
        action: "tenth_window",
        kitty: "kitty_mod+0",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+f7",
        action: "focus_visible_window",
        kitty: "kitty_mod+f7",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+f8",
        action: "swap_with_window",
        kitty: "kitty_mod+f8",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+right",
        action: "next_tab",
        kitty: "kitty_mod+right",
        mac_only: false,
    },
    Reservation {
        chord: "shift+super+]",
        action: "next_tab",
        kitty: "shift+cmd+]",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+tab",
        action: "next_tab",
        kitty: "ctrl+tab",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+left",
        action: "previous_tab",
        kitty: "kitty_mod+left",
        mac_only: false,
    },
    Reservation {
        chord: "shift+super+[",
        action: "previous_tab",
        kitty: "shift+cmd+[",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+tab",
        action: "previous_tab",
        kitty: "ctrl+shift+tab",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+t",
        action: "new_tab",
        kitty: "kitty_mod+t",
        mac_only: false,
    },
    Reservation {
        chord: "super+t",
        action: "new_tab",
        kitty: "cmd+t",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+q",
        action: "close_tab",
        kitty: "kitty_mod+q",
        mac_only: false,
    },
    Reservation {
        chord: "super+w",
        action: "close_tab",
        kitty: "cmd+w",
        mac_only: true,
    },
    Reservation {
        chord: "shift+super+w",
        action: "close_os_window",
        kitty: "shift+cmd+w",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+.",
        action: "move_tab_forward",
        kitty: "kitty_mod+.",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+,",
        action: "move_tab_backward",
        kitty: "kitty_mod+,",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+alt+t",
        action: "set_tab_title",
        kitty: "kitty_mod+alt+t",
        mac_only: false,
    },
    Reservation {
        chord: "shift+super+i",
        action: "set_tab_title",
        kitty: "shift+cmd+i",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+l",
        action: "next_layout",
        kitty: "kitty_mod+l",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+=",
        action: "change_font_size all +2.0",
        kitty: "kitty_mod+equal",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+plus",
        action: "change_font_size all +2.0",
        kitty: "kitty_mod+plus",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+kp_add",
        action: "change_font_size all +2.0",
        kitty: "kitty_mod+kp_add",
        mac_only: false,
    },
    Reservation {
        chord: "super+plus",
        action: "change_font_size all +2.0",
        kitty: "cmd+plus",
        mac_only: true,
    },
    Reservation {
        chord: "super+=",
        action: "change_font_size all +2.0",
        kitty: "cmd+equal",
        mac_only: true,
    },
    Reservation {
        chord: "shift+super+=",
        action: "change_font_size all +2.0",
        kitty: "shift+cmd+equal",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+-",
        action: "change_font_size all -2.0",
        kitty: "kitty_mod+minus",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+kp_subtract",
        action: "change_font_size all -2.0",
        kitty: "kitty_mod+kp_subtract",
        mac_only: false,
    },
    Reservation {
        chord: "super+-",
        action: "change_font_size all -2.0",
        kitty: "cmd+minus",
        mac_only: true,
    },
    Reservation {
        chord: "shift+super+-",
        action: "change_font_size all -2.0",
        kitty: "shift+cmd+minus",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+backspace",
        action: "change_font_size all 0",
        kitty: "kitty_mod+backspace",
        mac_only: false,
    },
    Reservation {
        chord: "super+0",
        action: "change_font_size all 0",
        kitty: "cmd+0",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+e",
        action: "open_url_with_hints",
        kitty: "kitty_mod+e",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+p",
        action: "kitten hints --type path --program -",
        kitty: "kitty_mod+p",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+f1",
        action: "show_kitty_doc overview",
        kitty: "kitty_mod+f1",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+f11",
        action: "toggle_fullscreen",
        kitty: "kitty_mod+f11",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+super+f",
        action: "toggle_fullscreen",
        kitty: "ctrl+cmd+f",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+f10",
        action: "toggle_maximized",
        kitty: "kitty_mod+f10",
        mac_only: false,
    },
    Reservation {
        chord: "alt+super+s",
        action: "toggle_macos_secure_keyboard_entry",
        kitty: "opt+cmd+s",
        mac_only: true,
    },
    Reservation {
        chord: "super+`",
        action: "macos_cycle_through_os_windows",
        kitty: "cmd+`",
        mac_only: true,
    },
    Reservation {
        chord: "super+shift+`",
        action: "macos_cycle_through_os_windows_backwards",
        kitty: "cmd+shift+`",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+u",
        action: "kitten unicode_input",
        kitty: "kitty_mod+u",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+super+space",
        action: "kitten unicode_input",
        kitty: "ctrl+cmd+space",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+f2",
        action: "edit_config_file",
        kitty: "kitty_mod+f2",
        mac_only: false,
    },
    Reservation {
        chord: "super+,",
        action: "edit_config_file",
        kitty: "cmd+,",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+esc",
        action: "kitty_shell window",
        kitty: "kitty_mod+escape",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+a",
        action: "set_background_opacity +0.1",
        kitty: "kitty_mod+a",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+shift+delete",
        action: "clear_terminal reset active",
        kitty: "kitty_mod+delete",
        mac_only: false,
    },
    Reservation {
        chord: "alt+super+r",
        action: "clear_terminal reset active",
        kitty: "opt+cmd+r",
        mac_only: true,
    },
    Reservation {
        chord: "super+k",
        action: "clear_terminal to_cursor active",
        kitty: "cmd+k",
        mac_only: true,
    },
    Reservation {
        chord: "alt+super+k",
        action: "clear_terminal scrollback active",
        kitty: "option+cmd+k",
        mac_only: true,
    },
    Reservation {
        chord: "super+l",
        action: "clear_terminal last_command active",
        kitty: "cmd+l",
        mac_only: true,
    },
    Reservation {
        chord: "super+ctrl+l",
        action: "clear_terminal to_cursor_scroll active",
        kitty: "cmd+ctrl+l",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+f5",
        action: "load_config_file",
        kitty: "kitty_mod+f5",
        mac_only: false,
    },
    Reservation {
        chord: "ctrl+super+,",
        action: "load_config_file",
        kitty: "ctrl+cmd+,",
        mac_only: true,
    },
    Reservation {
        chord: "ctrl+shift+f6",
        action: "debug_config",
        kitty: "kitty_mod+f6",
        mac_only: false,
    },
    Reservation {
        chord: "alt+super+,",
        action: "debug_config",
        kitty: "opt+cmd+,",
        mac_only: true,
    },
    Reservation {
        chord: "shift+super+/",
        action: "open_url https://sw.kovidgoyal.net/kitty/",
        kitty: "shift+cmd+/",
        mac_only: true,
    },
    Reservation {
        chord: "super+h",
        action: "hide_macos_app",
        kitty: "cmd+h",
        mac_only: true,
    },
    Reservation {
        chord: "alt+super+h",
        action: "hide_macos_other_apps",
        kitty: "opt+cmd+h",
        mac_only: true,
    },
    Reservation {
        chord: "super+m",
        action: "minimize_macos_window",
        kitty: "cmd+m",
        mac_only: true,
    },
    Reservation {
        chord: "super+q",
        action: "quit",
        kitty: "cmd+q",
        mac_only: true,
    },
];
pub fn collision(chord: &Chord, mac: bool) -> Option<&'static Reservation> {
    DEFAULTS
        .iter()
        .find(|r| (!r.mac_only || mac) && Chord::parse(r.chord).ok().as_ref() == Some(chord))
}
/// Only final, enabled custom bindings are diagnosed. A later override or
/// `none` for the same scoped chord makes an earlier line irrelevant.
pub fn warnings(bindings: &Bindings, mac: bool) -> Vec<String> {
    let rows: Vec<_> = bindings.iter().collect();
    rows.iter()
        .enumerate()
        .filter_map(|(at, row)| {
            if row.action.is_none()
                || rows[at + 1..]
                    .iter()
                    .any(|later| later.scope == row.scope && later.chord == row.chord)
            {
                return None;
            }
            let r = collision(&row.chord, mac)?;
            Some(format!(
                "key {:?}.{}: Kitty 0.45 reserves this for {}; to free it in kitty.conf: map {}",
                row.scope,
                row.chord.spell(),
                r.action,
                r.kitty
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::Binding;
    #[test]
    fn warnings_use_host_os_and_only_effective_enabled_custom_rows() {
        let bindings = Bindings::from_rows(vec![
            Binding::parse("cmd+l", "url").unwrap(),
            Binding::parse("ctrl+shift+p", "reload").unwrap(),
            Binding::parse("ctrl+shift+h", "history").unwrap(),
            Binding::parse("ctrl+shift+h", "none").unwrap(),
        ]);
        let mac = warnings(&bindings, true);
        assert_eq!(mac.len(), 2);
        assert!(mac[0].contains("clear_terminal last_command active"));
        assert!(mac[0].contains("map cmd+l"));
        assert_eq!(warnings(&bindings, false).len(), 1);
    }
}
