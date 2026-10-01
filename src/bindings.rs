//! `key.<chord> = <action>`: what a person may rebind, and how a chord is
//! spelled.
//!
//! The keys this program keeps for itself are a `match` in `app.rs`
//! (`command`), and it stays one: it reads shift only where shift matters and
//! answers more spellings than anybody writes down, which a table of exact
//! rows would have to list one by one. So what a config file says about keys
//! is kept beside that function rather than instead of it — a list of rows,
//! looked up first, whose answer is a command, "not a command even if the
//! built-in table says so", or nothing said, in which case the built-in table
//! answers as it always did. That is [`Lookup`], and `app::keyed` is where it
//! is asked: every place a key becomes a command asks it there.
//!
//! [`ACTIONS`] is the other half: every command with a name, the chords the
//! built-in table answers it on, and a half-line of what it does. `--help`'s
//! list of actions is made from it ([`help`]), and tests hold `docs/configuration.md`'s
//! rebinding table and `command` itself to it, so that a command added
//! without a name, or a default key changed in one place only, fails `cargo
//! test` rather than a person reading the help.
//!
//! # A chord needs ctrl, alt or super
//!
//! A bare letter, or a shifted one, is the page's by this program's first
//! rule: every unmodified key goes to the page, so that a text field is a
//! text field. A `key.` line must not be able to take `j` from every field by
//! a typo, so [`Binding::parse`] refuses a chord without ctrl, alt or super —
//! shift is not enough, `shift+a` is a capital A — unless its key is one of
//! `f1`…`f24`, which no field types with and which is where `f5` for reload
//! lives. [`Chord::parse`] itself stays the spelling parser, permissive, so
//! that the rule is one sentence in one place.
//!
//! # A chord is exact
//!
//! [`Chord::matches`] wants the same key and the same four modifier bits,
//! no more and no fewer. The built-in table reads shift only where it
//! changes what a key does — `tab`, `t`, `a`, `h`, `j`, `r`, `s`, `pageup`
//! and `pagedown` — and a table of rows has to be either exact or a
//! language; a row for `ctrl+tab` that also caught `ctrl+shift+tab` would
//! take the previous-tab key away with the next-tab one. So `key.ctrl+= =
//! none` frees `ctrl+=` and leaves `ctrl+shift+=` zooming in, as the built-in
//! table has it. Caps lock and num lock, which the Kitty protocol reports as
//! modifier bits too, are not part of any chord and are ignored.
//!
//! `+` is the separator, so a chord that means the `+` key spells it `plus`;
//! and a space cannot be written at the end of a line that is trimmed, so it
//! is `space`. Everything else that is one character is itself: `alt+=`,
//! `ctrl+-`, `ctrl+/`.
//!
//! # Two keymaps, one table
//!
//! The keys above were chosen against Linux terminals, and on a Mac most of
//! them do not arrive. Kitty on macOS keeps `cmd+t`, `cmd+w`, `cmd+l`,
//! `cmd+r`, `cmd+f`, `cmd+1`…`cmd+9` and a dozen more for itself; and Option
//! is not alt there — with Kitty's default `macos_option_as_alt no` it
//! composes, so Option+= arrives as the text `≠`, and `input::key_event`
//! names a key by the codepoint the terminal reports, never by the
//! base-layout alternate. So a Mac gets a keymap of its own, [`Keymap::Mac`]:
//! the `mac` column of [`ACTIONS`], made into rows by [`Keymap::rows`] and
//! asked after the file's `key.` lines and before `app::command`, which still
//! answers underneath with the Linux chords for a person who set
//! `macos_option_as_alt` or unmapped Kitty's keys. [`Keymap::Linux`] has no
//! rows: it is the `match`, unchanged.
//!
//! The Mac column has no `alt+<printable>`, for the reason above, and every
//! `cmd` chord in it was chosen against [`crate::taken::KITTY_MACOS`] and
//! [`crate::taken::MACOS_SYSTEM`], which a test holds it to. Where the Linux
//! chord is ctrl and reaches a Kitty pane on a Mac anyway (`ctrl+l`,
//! `ctrl+r`, `ctrl+t`, `ctrl+w`, `ctrl+f`, `ctrl+q`, the zoom keys,
//! `ctrl+.`), the Mac column keeps it.

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

/// The commands that have names: every one of the program's own, since the
/// keys `command` answers are exactly the ones a person may move.
/// `tab-1`…`tab-8` are one command with a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    EditUrl,
    Reload,
    Back,
    Forward,
    NewTab,
    CloseTab,
    ReopenTab,
    Bookmark,
    NextTab,
    PreviousTab,
    /// The nth tab, counted from one, one to eight; `alt+9` is
    /// [`Action::LastTab`], whatever the count.
    Tab(usize),
    LastTab,
    ListTabs,
    MoveTabLeft,
    MoveTabRight,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Find,
    /// `alt+r`: the article without the page around it. See
    /// [`crate::reader`].
    Reader,
    /// `alt+p`: the line that allows this page's origin the camera, the
    /// microphone and the rest. See [`crate::permissions`].
    Permissions,
    /// `alt+l`: the login form filled from the password command. See
    /// [`crate::login`].
    FillLogin,
    Copy,
    CopyUrl,
    /// `alt+o`: the page in front in the desktop browser. See
    /// [`crate::external`].
    OpenExternal,
    /// `alt+s`: the page as a PDF in the downloads directory. See
    /// [`crate::save`].
    SavePdf,
    /// `alt+shift+s`: the whole page as a PNG there.
    SaveScreenshot,
    ToggleNormal,
    /// `ctrl+shift+h`: the history list. See [`crate::historylist`].
    History,
    /// `ctrl+shift+j`: the page's console. See [`crate::consolelist`].
    Console,
    /// `alt+b`: blocking off for the site in front, or on again. See
    /// [`crate::block`].
    Block,
    /// `alt+shift+r`: read the site styles and scripts again. See
    /// [`crate::sites`].
    ReloadSites,
}

/// One row of [`ACTIONS`]: the name a `key.` line gives, the action, the
/// chords the built-in table answers it on (comma-separated, as
/// [`Chord::parse`] reads them), the chords the Mac keymap puts it on, and
/// the half-line the rebinding table says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub name: &'static str,
    pub action: Action,
    /// The Linux keymap's chords: what `app::command` answers.
    pub keys: &'static str,
    /// The Mac keymap's chords, `cmd` for super: see [`Keymap::Mac`].
    pub mac: &'static str,
    pub what: &'static str,
}

/// `keymap = mac | linux`: which set of built-in keys a run answers.
///
/// Linux is `app::command` alone, as it always was. Mac is the `mac` column
/// of [`ACTIONS`] made into rows ([`Keymap::rows`]) and asked after the
/// file's `key.` lines and before that `match`, which still answers
/// underneath: see the module's section on the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Keymap {
    #[default]
    Linux,
    Mac,
}

impl Keymap {
    /// `mac` or `linux`, in any case. The error names both, in the shape
    /// the other settings' errors have.
    pub fn parse(text: &str) -> Result<Keymap, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "mac" => Ok(Keymap::Mac),
            "linux" => Ok(Keymap::Linux),
            _ => Err(format!("keymap is mac or linux, not {:?}", text.trim())),
        }
    }

    /// The word `parse` reads.
    pub fn name(self) -> &'static str {
        match self {
            Keymap::Linux => "linux",
            Keymap::Mac => "mac",
        }
    }

    /// The keymap a run has when nothing says which: mac on macOS, where the
    /// terminal keeps most of what the Linux one needs, and linux elsewhere.
    pub fn platform() -> Keymap {
        if cfg!(target_os = "macos") {
            Keymap::Mac
        } else {
            Keymap::Linux
        }
    }

    /// This keymap's chords in `row`, as written.
    pub fn column(self, row: &Row) -> &'static str {
        match self {
            Keymap::Linux => row.keys,
            Keymap::Mac => row.mac,
        }
    }

    /// The rows this keymap puts in front of `app::command`: none for Linux,
    /// whose keys are that `match`; for Mac every chord of the `mac` column,
    /// `ctrl+1`…`ctrl+8` expanded.
    pub fn rows(self) -> Vec<Binding> {
        if self == Keymap::Linux {
            return Vec::new();
        }
        Action::every()
            .into_iter()
            .flat_map(|action| {
                defaults_on(self, action)
                    .into_iter()
                    .map(move |chord| Binding {
                        chord,
                        action: Some(action),
                    })
            })
            .collect()
    }
}

/// The name of the one row that stands for eight actions.
const TAB_NAMES: &str = "tab-1 .. tab-8";

/// Every action, in the order `--help` lists them. `tab-1`…`tab-8` are one
/// row, `Action::Tab(1)`, whose name and keys are written as ranges;
/// [`Action::every`] and [`defaults`] expand it.
///
/// The keys are the documented spellings, not every press the built-in table
/// answers: it reads shift only for `tab`, `t`, `a`, `h`, `j`, `r`, `s` and the page keys,
/// so `ctrl+shift+=` zooms in too, and a chord is exact.
///
/// The `mac` column, one reason each:
///
/// - `cmd` where Kitty on macOS leaves it free and Chrome or Safari use it:
///   `cmd+[`/`cmd+]` back and forward, `cmd+d` bookmark, `cmd+shift+t`
///   reopen, `cmd+shift+a` Chrome's tab search, `cmd+y` Chrome's history,
///   `cmd+alt+left`/`right` Chrome's tab switch, `cmd+shift+r` Safari's
///   Reader, `cmd+alt+j` Chrome's console.
/// - `ctrl` where the Linux ctrl chord already reaches a Kitty pane on a
///   Mac: `ctrl+l`, `ctrl+r`, `ctrl+t`, `ctrl+w`, `ctrl+f`, `ctrl+q`, `ctrl+=`,
///   `ctrl+-`, `ctrl+0`, `ctrl+.`.
/// - `ctrl+1`…`ctrl+9` for the tabs, because `cmd+1`…`cmd+9` are Kitty's
///   windows and Option composes.
/// - `cmd+c` for copy: Kitty's `cmd+c` is `copy_or_noop`, which with no
///   terminal selection passes the key on — and with the mouse reported to
///   this program, Kitty has none. `cmd+shift+c` as well, for the case it
///   does.
/// - `ctrl+pageup`/`ctrl+pagedown` as the second spelling of the tab keys,
///   which needs no Option and works from an external keyboard.
/// - The rest are `cmd` or `cmd+shift` on the letter the Linux alt chord
///   has, where that is free: `cmd+p`, `cmd+b`, `cmd+u`, `cmd+s`,
///   `cmd+shift+s`, `cmd+shift+o`, and `cmd+shift+l` because `cmd+l` is
///   Kitty's.
/// - `cmd+alt+shift+r` for reload-sites: `cmd+r` and `cmd+alt+r` are
///   Kitty's (resizing the window, resetting the terminal), and
///   `cmd+shift+r` is the reader's.
pub const ACTIONS: [Row; 33] = [
    Row {
        name: "quit",
        action: Action::Quit,
        keys: "ctrl+q",
        mac: "ctrl+q",
        what: "close this window",
    },
    Row {
        name: "url",
        action: Action::EditUrl,
        keys: "ctrl+l",
        mac: "ctrl+l",
        what: "type a url",
    },
    Row {
        name: "reload",
        action: Action::Reload,
        keys: "ctrl+r",
        mac: "ctrl+r",
        what: "reload",
    },
    Row {
        name: "back",
        action: Action::Back,
        keys: "alt+left",
        mac: "cmd+[",
        what: "back",
    },
    Row {
        name: "forward",
        action: Action::Forward,
        keys: "alt+right",
        mac: "cmd+]",
        what: "forward",
    },
    Row {
        name: "new-tab",
        action: Action::NewTab,
        keys: "ctrl+t",
        mac: "ctrl+t",
        what: "a new tab",
    },
    Row {
        name: "close-tab",
        action: Action::CloseTab,
        keys: "ctrl+w",
        mac: "ctrl+w",
        what: "close this tab",
    },
    Row {
        name: "reopen-tab",
        action: Action::ReopenTab,
        keys: "ctrl+shift+t, alt+t",
        mac: "cmd+shift+t",
        what: "reopen the tab closed last",
    },
    Row {
        name: "bookmark",
        action: Action::Bookmark,
        keys: "ctrl+d",
        mac: "cmd+d",
        what: "bookmark this page, or remove the bookmark",
    },
    Row {
        name: "next-tab",
        action: Action::NextTab,
        keys: "ctrl+tab",
        mac: "cmd+alt+right, ctrl+pagedown",
        what: "the next tab",
    },
    Row {
        name: "previous-tab",
        action: Action::PreviousTab,
        keys: "ctrl+shift+tab",
        mac: "cmd+alt+left, ctrl+pageup",
        what: "the tab before",
    },
    Row {
        name: TAB_NAMES,
        action: Action::Tab(1),
        keys: "alt+1 .. alt+8",
        mac: "ctrl+1 .. ctrl+8",
        what: "the nth tab",
    },
    Row {
        name: "last-tab",
        action: Action::LastTab,
        keys: "alt+9",
        mac: "ctrl+9",
        what: "the last tab",
    },
    Row {
        name: "list-tabs",
        action: Action::ListTabs,
        keys: "ctrl+shift+a, alt+a",
        mac: "cmd+shift+a",
        what: "the tab list",
    },
    Row {
        name: "history",
        action: Action::History,
        keys: "ctrl+shift+h, alt+h",
        mac: "cmd+y",
        what: "the history list",
    },
    Row {
        name: "console",
        action: Action::Console,
        keys: "ctrl+shift+j, alt+j",
        mac: "cmd+alt+j",
        what: "the page's console: logs, errors and failed requests",
    },
    Row {
        name: "move-tab-left",
        action: Action::MoveTabLeft,
        keys: "ctrl+shift+pageup, alt+shift+pageup",
        mac: "cmd+shift+pageup",
        what: "move this tab left",
    },
    Row {
        name: "move-tab-right",
        action: Action::MoveTabRight,
        keys: "ctrl+shift+pagedown, alt+shift+pagedown",
        mac: "cmd+shift+pagedown",
        what: "move this tab right",
    },
    Row {
        name: "zoom-in",
        action: Action::ZoomIn,
        keys: "alt+=, ctrl+=",
        mac: "ctrl+=",
        what: "zoom in",
    },
    Row {
        name: "zoom-out",
        action: Action::ZoomOut,
        keys: "alt+-, ctrl+-",
        mac: "ctrl+-",
        what: "zoom out",
    },
    Row {
        name: "zoom-reset",
        action: Action::ZoomReset,
        keys: "alt+0, ctrl+0",
        mac: "ctrl+0",
        what: "back to 100%",
    },
    Row {
        name: "find",
        action: Action::Find,
        keys: "ctrl+f",
        mac: "ctrl+f",
        what: "find in the page",
    },
    Row {
        name: "reader",
        action: Action::Reader,
        keys: "alt+r",
        mac: "cmd+shift+r",
        what: "the article without the page around it",
    },
    Row {
        name: "permissions",
        action: Action::Permissions,
        keys: "alt+p",
        mac: "cmd+p",
        what: "allow this site the camera, microphone, location, notifications or clipboard",
    },
    Row {
        name: "block",
        action: Action::Block,
        keys: "alt+b",
        mac: "cmd+b",
        what: "stop blocking ads and trackers on this site, or start again",
    },
    Row {
        name: "reload-sites",
        action: Action::ReloadSites,
        keys: "alt+shift+r",
        mac: "cmd+alt+shift+r",
        what: "read the site styles and scripts again",
    },
    Row {
        name: "fill-login",
        action: Action::FillLogin,
        keys: "alt+l",
        mac: "cmd+shift+l",
        what: "fill the login form from your password manager",
    },
    Row {
        name: "copy",
        action: Action::Copy,
        keys: "alt+c",
        mac: "cmd+c, cmd+shift+c",
        what: "copy the selection, or the line being typed",
    },
    Row {
        name: "copy-url",
        action: Action::CopyUrl,
        keys: "alt+u",
        mac: "cmd+u",
        what: "copy the url",
    },
    Row {
        name: "open-external",
        action: Action::OpenExternal,
        keys: "alt+o",
        mac: "cmd+shift+o",
        what: "open this page in the desktop browser",
    },
    Row {
        name: "save-pdf",
        action: Action::SavePdf,
        keys: "alt+s",
        mac: "cmd+s",
        what: "save this page as a PDF",
    },
    Row {
        name: "save-screenshot",
        action: Action::SaveScreenshot,
        keys: "alt+shift+s",
        mac: "cmd+shift+s",
        what: "save the whole page as a picture",
    },
    Row {
        name: "normal-mode",
        action: Action::ToggleNormal,
        keys: "ctrl+.",
        mac: "ctrl+.",
        what: "normal mode on or off",
    },
];

/// The rows with one name each: every row but `tab-1 .. tab-8`.
fn named_rows() -> impl Iterator<Item = &'static Row> {
    ACTIONS.iter().filter(|row| row.name != TAB_NAMES)
}

impl Action {
    /// A name from [`ACTIONS`], or `tab-1`…`tab-8`. `tab-9` is refused with
    /// the name of what `alt+9` does, since that is the one a person reaching
    /// for it means; anything else unknown is an error listing them.
    pub fn parse(text: &str) -> Result<Action, String> {
        let lower = text.trim().to_ascii_lowercase();
        if let Some(row) = named_rows().find(|row| row.name == lower) {
            return Ok(row.action);
        }
        if let Some(n) = lower.strip_prefix("tab-").and_then(|n| n.parse().ok()) {
            if (1..=8).contains(&n) {
                return Ok(Action::Tab(n));
            }
            if n == 9 {
                return Err(format!(
                    "{text:?}: alt+9 is the last tab, whatever the count; that action is last-tab"
                ));
            }
        }
        Err(format!(
            "{text:?} is not an action; they are {}, tab-1..tab-8, or none",
            named_rows()
                .map(|row| row.name)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// The name `parse` reads.
    pub fn name(self) -> String {
        if let Action::Tab(n) = self {
            return format!("tab-{n}");
        }
        named_rows()
            .find(|row| row.action == self)
            .map(|row| row.name.to_string())
            .unwrap_or_default()
    }

    /// Every action, `tab-1 .. tab-8` expanded, in [`ACTIONS`]' order.
    pub fn every() -> Vec<Action> {
        ACTIONS
            .iter()
            .flat_map(|row| match row.action {
                Action::Tab(_) => (1..=8).map(Action::Tab).collect(),
                action => vec![action],
            })
            .collect()
    }
}

/// The chords the built-in table answers `action` on, as [`ACTIONS`] lists
/// them: its row's `keys` parsed, and `alt+n` for `tab-n`. The Linux
/// keymap's; [`defaults_on`] is either.
///
/// Nothing in the program asks this — the built-in table is `app::command`,
/// which is a `match` — but the tests on both sides do, `docs/configuration.md`'s
/// table here and `command` itself in `app.rs`, and it is one function for both.
pub fn defaults(action: Action) -> Vec<Chord> {
    defaults_on(Keymap::Linux, action)
}

/// The chords `keymap` puts `action` on: its row's column parsed, and
/// `alt+n` (Linux) or `ctrl+n` (Mac) for `tab-n`.
pub fn defaults_on(keymap: Keymap, action: Action) -> Vec<Chord> {
    if let Action::Tab(n) = action {
        let held = match keymap {
            Keymap::Linux => "alt",
            Keymap::Mac => "ctrl",
        };
        return Chord::parse(&format!("{held}+{n}")).into_iter().collect();
    }
    ACTIONS
        .iter()
        .filter(|row| row.action == action)
        .flat_map(|row| keymap.column(row).split(','))
        .filter_map(|spelled| Chord::parse(spelled).ok())
        .collect()
}

/// The `actions:` block `--help` prints after its `keys:`: a name and
/// `keymap`'s chords per row, and what a chord may be.
///
/// What each does is not printed: the `keys:` block above it has already
/// said, in more words than half a line.
pub fn help(keymap: Keymap) -> String {
    let width = ACTIONS.iter().map(|row| row.name.len()).max().unwrap_or(0);
    let mut out = format!(
        "\nactions, keymap {} (key.<chord> = <action> in the settings file; none unbinds):\n",
        keymap.name()
    );
    for row in &ACTIONS {
        out.push_str(&format!("  {:<width$}  {}\n", row.name, keymap.column(row)));
    }
    out.push_str(
        "A chord is ctrl, alt, shift or super joined with + to a key: a character,\n\
         plus, space, tab, enter, esc, backspace, insert, delete, up, down, left,\n\
         right, home, end, pageup, pagedown or f1..f24; it needs ctrl, alt or super\n\
         unless it is an f-key.\n",
    );
    out
}

/// One line of the file: `None` is `= none`, the chord unbound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub chord: Chord,
    pub action: Option<Action>,
}

impl Binding {
    /// `key.<chord> = <value>`'s two halves, `chord` without the `key.`.
    /// Checked in the order they are written: the chord's spelling, then
    /// that it has ctrl, alt or super or is an f-key (see the module's
    /// section on it), then the action.
    pub fn parse(chord: &str, value: &str) -> Result<Binding, String> {
        let written = chord.trim();
        let chord = Chord::parse(written).map_err(|why| format!("key.{why}"))?;
        let held = chord.mods.0 & (Mods::CTRL | Mods::ALT | Mods::SUPER) != 0;
        if !held && !matches!(chord.key, Key::Function(_)) {
            return Err(format!(
                "key.{written}: a key with no ctrl, alt or super is the page's; add one, or use f1..f24"
            ));
        }
        let action = if value.trim().eq_ignore_ascii_case("none") {
            None
        } else {
            Some(Action::parse(value)?)
        };
        Ok(Binding { chord, action })
    }
}

/// Every `key.` line, in file order, over the keymap's own rows.
///
/// A later line for the same chord replaces an earlier one, so that a file
/// can be appended to, and that is not a duplicate the parser refuses. The
/// keymap's rows ([`Keymap::rows`]) are asked only when no line of the file
/// matched, and the built-in `match` only when neither did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bindings {
    keymap: Vec<Binding>,
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
    /// The file's `rows` over `keymap`'s.
    pub fn on(keymap: Keymap, rows: Vec<Binding>) -> Bindings {
        Bindings {
            keymap: keymap.rows(),
            rows,
        }
    }

    /// The file's `rows` over the Linux keymap, which has none of its own.
    pub fn from_rows(rows: Vec<Binding>) -> Bindings {
        Bindings::on(Keymap::Linux, rows)
    }

    /// What the rows say about `input`: the file's, last first, then the
    /// keymap's. A release is never a command, as the built-in table never
    /// makes one of it, so it is `Default` before any row is looked at.
    pub fn lookup(&self, input: &KeyInput) -> Lookup {
        if input.action == KeyAction::Release {
            return Lookup::Default;
        }
        let found = self
            .rows
            .iter()
            .rev()
            .chain(self.keymap.iter().rev())
            .find(|row| row.chord.matches(input));
        match found {
            Some(Binding {
                action: Some(action),
                ..
            }) => Lookup::Bound(*action),
            Some(Binding { action: None, .. }) => Lookup::Unbound,
            None => Lookup::Default,
        }
    }

    /// Whether the file had no `key.` lines; the keymap's rows do not count.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The file's `key.` lines, in file order; not the keymap's rows.
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

    /// `written`'s chords, parsed; `cmd` and `super` are one modifier.
    fn chords_of(written: &str) -> Vec<Chord> {
        written
            .split(',')
            .map(|chord| Chord::parse(chord).expect(chord))
            .collect()
    }

    #[test]
    fn every_default_chord_in_both_columns_parses_and_spells_back_the_same() {
        for keymap in [Keymap::Linux, Keymap::Mac] {
            for row in &ACTIONS {
                if matches!(row.action, Action::Tab(_)) {
                    continue;
                }
                let column = keymap.column(row);
                let spelled: Vec<&str> = column.split(',').map(str::trim).collect();
                let chords = defaults_on(keymap, row.action);
                assert_eq!(chords.len(), spelled.len(), "{}: {column}", row.name);
                for (chord, spelled) in chords.iter().zip(spelled) {
                    assert_eq!(Chord::parse(&chord.spell()), Ok(*chord), "{}", row.name);
                    if keymap == Keymap::Linux {
                        assert_eq!(chord.spell(), spelled, "{}", row.name);
                    } else {
                        // The Mac column writes `cmd` first, as a Mac does.
                        assert_eq!(Chord::parse(spelled), Ok(*chord), "{}", row.name);
                    }
                }
            }
        }
        for n in 1..=8 {
            let spelled: Vec<String> = defaults(Action::Tab(n)).iter().map(Chord::spell).collect();
            assert_eq!(spelled, [format!("alt+{n}")]);
            let spelled: Vec<String> = defaults_on(Keymap::Mac, Action::Tab(n))
                .iter()
                .map(Chord::spell)
                .collect();
            assert_eq!(spelled, [format!("ctrl+{n}")]);
        }
        for spelled in ["ctrl+plus", "ctrl+_"] {
            assert_eq!(Chord::parse(spelled).expect(spelled).spell(), spelled);
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
    fn every_mac_chord_is_a_binding_a_file_could_write() {
        for row in &ACTIONS {
            let name = if matches!(row.action, Action::Tab(_)) {
                "tab-1"
            } else {
                row.name
            };
            for chord in row.mac.split(',') {
                let chord = chord.split(" .. ").next().unwrap_or(chord);
                assert!(
                    Binding::parse(chord, name).is_ok(),
                    "{chord} for {}",
                    row.name
                );
            }
        }
        let rows = Keymap::Mac.rows();
        for (i, row) in rows.iter().enumerate() {
            assert!(
                rows[..i].iter().all(|before| before.chord != row.chord),
                "{} twice",
                row.chord.spell()
            );
        }
    }

    #[test]
    fn no_mac_chord_is_one_kitty_or_macos_keeps() {
        for row in Keymap::Mac.rows() {
            assert_eq!(
                crate::taken::keeper(&row.chord, true),
                None,
                "{} for {:?}",
                row.chord.spell(),
                row.action
            );
        }
    }

    /// The Mac chords of the actions added beside the Mac keymap, each
    /// held by name to what it is and to Kitty's and macOS's tables, so that
    /// none of them is a chord that never arrives; and the chords they were
    /// not given, because Kitty keeps them.
    #[test]
    fn the_newer_actions_mac_chords_are_free_in_kitty_and_macos() {
        for (name, wanted) in [
            ("open-external", "cmd+shift+o"),
            ("reload-sites", "cmd+alt+shift+r"),
            ("reader", "cmd+shift+r"),
            ("console", "cmd+alt+j"),
        ] {
            let row = ACTIONS.iter().find(|row| row.name == name).expect("a row");
            assert_eq!(row.mac, wanted, "{name}");
            let chord = Chord::parse(wanted).expect(wanted);
            assert_eq!(crate::taken::keeper(&chord, true), None, "{name}");
            assert!(
                Keymap::Mac
                    .rows()
                    .iter()
                    .any(|binding| binding.chord == chord && binding.action == Some(row.action)),
                "{name} on {wanted}"
            );
        }
        // Why reload-sites is not `cmd+r` or `cmd+alt+r`.
        for kept in ["cmd+r", "cmd+alt+r"] {
            let chord = Chord::parse(kept).expect(kept);
            assert!(
                crate::taken::keeper(&chord, true).is_some(),
                "{kept} is Kitty's"
            );
        }
    }

    #[test]
    fn the_linux_chords_kitty_keeps_are_exactly_the_eight_the_docs_give_a_twin_for() {
        let wanted = [
            ("reopen-tab", "ctrl+shift+t"),
            ("next-tab", "ctrl+tab"),
            ("previous-tab", "ctrl+shift+tab"),
            ("list-tabs", "ctrl+shift+a"),
            ("history", "ctrl+shift+h"),
            ("console", "ctrl+shift+j"),
            ("move-tab-left", "ctrl+shift+pageup"),
            ("move-tab-right", "ctrl+shift+pagedown"),
        ];
        for macos in [false, true] {
            let mut kept: Vec<(String, String)> = Vec::new();
            for action in Action::every() {
                for chord in defaults(action) {
                    if crate::taken::keeper(&chord, macos).is_some() {
                        kept.push((action.name(), chord.spell()));
                    }
                }
            }
            let wanted: Vec<(String, String)> = wanted
                .iter()
                .map(|(name, chord)| (name.to_string(), chord.to_string()))
                .collect();
            assert_eq!(kept, wanted, "macos {macos}");
        }
    }

    #[test]
    fn keymap_parses_mac_and_linux_case_insensitively_and_nothing_else() {
        for (text, wanted) in [
            ("mac", Keymap::Mac),
            ("MAC", Keymap::Mac),
            (" Linux ", Keymap::Linux),
        ] {
            assert_eq!(Keymap::parse(text), Ok(wanted), "{text}");
        }
        for keymap in [Keymap::Mac, Keymap::Linux] {
            assert_eq!(Keymap::parse(keymap.name()), Ok(keymap));
        }
        assert_eq!(
            Keymap::parse("windows"),
            Err("keymap is mac or linux, not \"windows\"".to_string())
        );
        assert!(Keymap::parse("macos").is_err());
        assert!(Keymap::parse("").is_err());
    }

    #[test]
    fn the_platform_s_keymap_is_mac_on_macos_and_linux_elsewhere() {
        let wanted = if cfg!(target_os = "macos") {
            Keymap::Mac
        } else {
            Keymap::Linux
        };
        assert_eq!(Keymap::platform(), wanted);
        assert_eq!(Keymap::default(), Keymap::Linux, "a table with no keymap");
    }

    #[test]
    fn help_prints_the_column_of_the_keymap_it_is_given_and_names_it() {
        for keymap in [Keymap::Linux, Keymap::Mac] {
            let help = help(keymap);
            let header = help
                .lines()
                .find(|line| line.starts_with("actions"))
                .expect("a header");
            assert!(
                header.contains(&format!("keymap {}", keymap.name())),
                "{header}"
            );
            let lines: Vec<&str> = help
                .lines()
                .skip_while(|line| !line.starts_with("actions"))
                .skip(1)
                .take(ACTIONS.len())
                .collect();
            assert_eq!(lines.len(), ACTIONS.len());
            for (line, row) in lines.iter().zip(&ACTIONS) {
                let line = line.trim();
                assert!(line.starts_with(row.name), "{line} for {}", row.name);
                assert!(
                    line.ends_with(keymap.column(row)),
                    "{line} for {}",
                    row.name
                );
            }
            for row in &ACTIONS {
                let named = help
                    .lines()
                    .filter(|line| line.trim().split("  ").next() == Some(row.name))
                    .count();
                assert_eq!(named, 1, "{}", row.name);
            }
            assert!(
                help.trim_end()
                    .ends_with("it needs ctrl, alt or super\nunless it is an f-key."),
                "{help}"
            );
        }
    }

    /// The rows of the table under `heading` in `doc`, each cell with its
    /// backticks gone and `…` written `..`, as [`ACTIONS`] writes a range.
    fn table_rows(doc: &str, heading: &str) -> Vec<Vec<String>> {
        doc.lines()
            .skip_while(|line| *line != heading)
            .skip(1)
            .take_while(|line| !line.starts_with('#'))
            .filter(|line| line.starts_with("| `"))
            .map(|line| {
                line.trim_matches('|')
                    .split('|')
                    .map(|cell| cell.replace('`', "").replace('…', "..").trim().to_string())
                    .collect()
            })
            .collect()
    }

    /// Whether a doc's cell says what `column` says: a range as written,
    /// else the same chords, `cmd` and `super` being one.
    fn same_chords(cell: &str, column: &str, action: Action) -> bool {
        if matches!(action, Action::Tab(_)) {
            return cell == column;
        }
        chords_of(cell) == chords_of(column)
    }

    #[test]
    fn the_readme_s_rebinding_table_is_the_action_table() {
        let rows = table_rows(
            include_str!("../docs/configuration.md"),
            "### Rebinding keys",
        );
        let names: Vec<&str> = rows.iter().map(|cells| cells[0].as_str()).collect();
        let wanted: Vec<&str> = ACTIONS.iter().map(|row| row.name).collect();
        assert_eq!(names, wanted);
        for (cells, row) in rows.iter().zip(&ACTIONS) {
            assert!(
                same_chords(&cells[1], row.keys, row.action),
                "{}: linux {}",
                row.name,
                cells[1]
            );
            assert!(
                same_chords(&cells[2], row.mac, row.action),
                "{}: mac {}",
                row.name,
                cells[2]
            );
            assert_eq!(cells[3], row.what, "{}", row.name);
        }
    }

    #[test]
    fn docs_usage_s_mac_table_lists_exactly_the_rows_whose_mac_chord_differs() {
        let rows = table_rows(include_str!("../docs/usage.md"), "### On a Mac");
        let differing: Vec<&Row> = ACTIONS
            .iter()
            .filter(|row| {
                if matches!(row.action, Action::Tab(_)) {
                    return row.keys != row.mac;
                }
                chords_of(row.keys) != chords_of(row.mac)
            })
            .collect();
        let names: Vec<&str> = rows.iter().map(|cells| cells[0].as_str()).collect();
        let wanted: Vec<&str> = differing.iter().map(|row| row.name).collect();
        assert_eq!(names, wanted);
        for (cells, row) in rows.iter().zip(differing) {
            assert!(same_chords(&cells[1], row.keys, row.action), "{}", row.name);
            assert!(same_chords(&cells[2], row.mac, row.action), "{}", row.name);
        }
    }

    #[test]
    fn a_keymap_row_is_under_the_file_s_rows_and_over_the_built_in_table() {
        let table = Bindings::on(Keymap::Mac, vec![row("cmd+d", None)]);
        assert_eq!(
            table.lookup(&key(Key::Char('d'), Mods::SUPER)),
            Lookup::Unbound
        );
        assert_eq!(
            table.lookup(&key(Key::Char('['), Mods::SUPER)),
            Lookup::Bound(Action::Back)
        );
        assert_eq!(
            table.lookup(&key(Key::Char('1'), Mods::CTRL)),
            Lookup::Bound(Action::Tab(1))
        );
        assert_eq!(
            table.lookup(&key(Key::Char('w'), Mods::CTRL)),
            Lookup::Bound(Action::CloseTab)
        );
        assert_eq!(
            table.lookup(&key(Key::Char('b'), Mods::ALT)),
            Lookup::Default,
            "the built-in table's, underneath"
        );
        let linux = Bindings::on(Keymap::Linux, vec![]);
        assert_eq!(
            linux.lookup(&key(Key::Char('['), Mods::SUPER)),
            Lookup::Default
        );
        let mut released = key(Key::Char('['), Mods::SUPER);
        released.action = KeyAction::Release;
        assert_eq!(table.lookup(&released), Lookup::Default);
    }

    #[test]
    fn is_empty_and_iter_are_about_the_file_s_rows_not_the_keymap_s() {
        let mac = Bindings::on(Keymap::Mac, vec![]);
        assert!(mac.is_empty());
        assert_eq!(mac.iter().count(), 0);
        let one = Bindings::on(Keymap::Mac, vec![row("f5", Some(Action::Reload))]);
        assert!(!one.is_empty());
        assert_eq!(one.iter().count(), 1);
        assert!(!Keymap::Mac.rows().is_empty());
        assert!(Keymap::Linux.rows().is_empty());
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
        let every = Action::every();
        assert_eq!(every.len(), ACTIONS.len() - 1 + 8);
        for action in every {
            assert_eq!(Action::parse(&action.name()), Ok(action), "{action:?}");
        }
        assert_eq!(Action::parse("url"), Ok(Action::EditUrl));
        assert_eq!(Action::parse("Tab-3"), Ok(Action::Tab(3)));
        assert_eq!(Action::parse("normal-mode"), Ok(Action::ToggleNormal));
        for wrong in ["tab-0", "tab-10", "bookmarks", "tab-1 .. tab-8"] {
            let why = Action::parse(wrong).expect_err(wrong);
            assert!(why.contains("is not an action"), "{why}");
            assert!(
                why.ends_with(
                    "copy-url, open-external, save-pdf, save-screenshot, normal-mode, tab-1..tab-8, or none"
                ),
                "the list is in the sentence: {why}"
            );
        }
        assert!(Action::parse("tab-9").is_err());
    }

    #[test]
    fn tab_9_is_refused_and_told_that_the_last_tab_is_last_tab() {
        assert_eq!(
            Action::parse("tab-9"),
            Err(
                "\"tab-9\": alt+9 is the last tab, whatever the count; that action is last-tab"
                    .to_string()
            )
        );
        let why = Action::parse("tab-10").expect_err("tab-10");
        assert!(why.starts_with("\"tab-10\" is not an action"), "{why}");
    }

    #[test]
    fn a_chord_needs_ctrl_alt_or_super_unless_it_is_a_function_key() {
        for bare in ["j", "shift+j", "space", "esc", "shift+pageup"] {
            assert_eq!(
                Binding::parse(bare, "back"),
                Err(format!(
                    "key.{bare}: a key with no ctrl, alt or super is the page's; add one, or use f1..f24"
                )),
                "{bare}"
            );
        }
        for held in ["f5", "shift+f5", "super+j", "ctrl+j", "alt+enter"] {
            assert!(Binding::parse(held, "back").is_ok(), "{held}");
        }
        // The spelling parser is not the rule.
        assert!(Chord::parse("j").is_ok());
        // Chord first, then modifiers, then the action.
        assert_eq!(
            Binding::parse("j", "nonsense")
                .expect_err("bare")
                .split(':')
                .next(),
            Some("key.j")
        );
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
        let why = Binding::parse("ctrl+a", "bookmarks").expect_err("not an action");
        assert!(why.starts_with("\"bookmarks\" is not an action"), "{why}");
    }
}
