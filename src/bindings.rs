//! Platform defaults and scoped `key.<scope>.<chord> = <action>` overrides.
//! Chords compare all four modifiers exactly; lock bits are ignored. Global
//! bindings require ctrl, alt or super (or a function key), so ordinary typing
//! still belongs to the page. Scoped bindings may use bare letters.

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

/// A key with its modifiers, as written: `ctrl+[`, `alt+=`, `f5`.
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
    /// `alt+p`: the line that allows this page's origin the camera, the
    /// microphone and the rest. See [`crate::permissions`].
    Permissions,
    /// `alt+l`: the login form filled from the password command. See
    /// [`crate::login`].
    FillLogin,
    Copy,
    CopyUrl,
    /// `alt+s`: the page as a PDF in the downloads directory. See
    /// [`crate::save`].
    SavePdf,
    /// `alt+shift+s`: the whole page as a PNG there.
    SaveScreenshot,
    ToggleNormal,
    /// `ctrl+shift+h`: the history list. See [`crate::historylist`].
    History,
    /// `alt+b`: blocking off for the site in front, or on again. See
    /// [`crate::block`].
    Block,
    /// An action belonging to an editor or an interaction scope.
    Local(LocalAction),
}

/// One row of [`ACTIONS`]: the name a `key.` line gives, the action, the
/// chords the built-in table answers it on (comma-separated, as
/// [`Chord::parse`] reads them), and the half-line the README's table says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub name: &'static str,
    pub action: Action,
    pub keys: &'static str,
    pub what: &'static str,
}

/// The name of the one row that stands for eight actions.
const TAB_NAMES: &str = "tab-1 .. tab-8";

/// Every action, in the order `--help` lists them. `tab-1`…`tab-8` are one
/// row, `Action::Tab(1)`, whose name and keys are written as ranges;
/// [`Action::every`] and [`defaults`] expand it.
///
/// The keys are the documented spellings, not every press the built-in table
/// answers: it reads shift only for `tab`, `t`, `a`, `h`, `s` and the page keys, so
/// `ctrl+shift+=` zooms in too, and a chord is exact.
pub const ACTIONS: [Row; 29] = [
    Row {
        name: "quit",
        action: Action::Quit,
        keys: "ctrl+q",
        what: "quit",
    },
    Row {
        name: "url",
        action: Action::EditUrl,
        keys: "ctrl+l",
        what: "type a url",
    },
    Row {
        name: "reload",
        action: Action::Reload,
        keys: "ctrl+r",
        what: "reload",
    },
    Row {
        name: "back",
        action: Action::Back,
        keys: "alt+left",
        what: "back",
    },
    Row {
        name: "forward",
        action: Action::Forward,
        keys: "alt+right",
        what: "forward",
    },
    Row {
        name: "new-tab",
        action: Action::NewTab,
        keys: "ctrl+t",
        what: "a new tab",
    },
    Row {
        name: "close-tab",
        action: Action::CloseTab,
        keys: "ctrl+w",
        what: "close this tab",
    },
    Row {
        name: "reopen-tab",
        action: Action::ReopenTab,
        keys: "alt+t",
        what: "reopen the tab closed last",
    },
    Row {
        name: "bookmark",
        action: Action::Bookmark,
        keys: "ctrl+d",
        what: "bookmark this page, or remove the bookmark",
    },
    Row {
        name: "next-tab",
        action: Action::NextTab,
        keys: "ctrl+]",
        what: "the next tab",
    },
    Row {
        name: "previous-tab",
        action: Action::PreviousTab,
        keys: "ctrl+[",
        what: "the tab before",
    },
    Row {
        name: TAB_NAMES,
        action: Action::Tab(1),
        keys: "alt+1 .. alt+8",
        what: "the nth tab",
    },
    Row {
        name: "last-tab",
        action: Action::LastTab,
        keys: "alt+9",
        what: "the last tab",
    },
    Row {
        name: "list-tabs",
        action: Action::ListTabs,
        keys: "alt+a",
        what: "the tab list",
    },
    Row {
        name: "history",
        action: Action::History,
        keys: "alt+h",
        what: "the history list",
    },
    Row {
        name: "move-tab-left",
        action: Action::MoveTabLeft,
        keys: "alt+shift+pageup",
        what: "move this tab left",
    },
    Row {
        name: "move-tab-right",
        action: Action::MoveTabRight,
        keys: "alt+shift+pagedown",
        what: "move this tab right",
    },
    Row {
        name: "zoom-in",
        action: Action::ZoomIn,
        keys: "alt+=, ctrl+=",
        what: "zoom in",
    },
    Row {
        name: "zoom-out",
        action: Action::ZoomOut,
        keys: "alt+-, ctrl+-",
        what: "zoom out",
    },
    Row {
        name: "zoom-reset",
        action: Action::ZoomReset,
        keys: "alt+0, ctrl+0",
        what: "back to 100%",
    },
    Row {
        name: "find",
        action: Action::Find,
        keys: "ctrl+f",
        what: "find in the page",
    },
    Row {
        name: "permissions",
        action: Action::Permissions,
        keys: "alt+p",
        what: "allow this site the camera, microphone, location, notifications or clipboard",
    },
    Row {
        name: "block",
        action: Action::Block,
        keys: "alt+b",
        what: "stop blocking ads and trackers on this site, or start again",
    },
    Row {
        name: "fill-login",
        action: Action::FillLogin,
        keys: "alt+l",
        what: "fill the login form from your password manager",
    },
    Row {
        name: "copy",
        action: Action::Copy,
        keys: "alt+c",
        what: "copy the selection, or the line being typed",
    },
    Row {
        name: "copy-url",
        action: Action::CopyUrl,
        keys: "alt+u",
        what: "copy the url",
    },
    Row {
        name: "save-pdf",
        action: Action::SavePdf,
        keys: "alt+s",
        what: "save this page as a PDF",
    },
    Row {
        name: "save-screenshot",
        action: Action::SaveScreenshot,
        keys: "alt+shift+s",
        what: "save the whole page as a picture",
    },
    Row {
        name: "normal-mode",
        action: Action::ToggleNormal,
        keys: "ctrl+.",
        what: "normal mode on or off",
    },
];

/// The default set is selected independently of the terminal's host OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keymap {
    Mac,
    Linux,
}
impl Default for Keymap {
    fn default() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Linux
        }
    }
}
impl Keymap {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "mac" => Ok(Self::Mac),
            "linux" => Ok(Self::Linux),
            _ => Err(format!("keymap: {value:?}; expected mac or linux")),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Mac => "mac",
            Self::Linux => "linux",
        }
    }
}

/// A local interaction. URL, find, prompt, upload and lists inherit line edits;
/// history also inherits list navigation. An explicit override always wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    Global,
    Line,
    Url,
    Find,
    List,
    History,
    Dialog,
    Prompt,
    Upload,
    Normal,
    Hints,
    Page,
}
impl Scope {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "line" => Self::Line,
            "url" => Self::Url,
            "find" => Self::Find,
            "list" => Self::List,
            "history" => Self::History,
            "dialog" => Self::Dialog,
            "prompt" => Self::Prompt,
            "upload" => Self::Upload,
            "normal" => Self::Normal,
            "hints" => Self::Hints,
            "page" => Self::Page,
            _ => return None,
        })
    }
    fn parents(self) -> &'static [Self] {
        match self {
            Self::History => &[Self::List, Self::Line],
            Self::Prompt => &[Self::Dialog, Self::Line],
            Self::Url | Self::Find | Self::List | Self::Upload => &[Self::Line],
            _ => &[],
        }
    }
    fn accepts(self, action: LocalAction) -> bool {
        let owner = action.scope();
        self == owner || self.parents().contains(&owner)
    }
}

/// Actions applied directly, without synthesizing keyboard events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalAction {
    LineHome,
    LineEnd,
    LineLeft,
    LineRight,
    LineWordLeft,
    LineWordRight,
    LineDeleteBack,
    LineDeleteForward,
    LineDeleteWordBack,
    LineDeleteWordForward,
    LineKillStart,
    LineKillEnd,
    LineHint,
    LinePrevious,
    LineNext,
    Accept,
    Cancel,
    FindNext,
    FindPrevious,
    ListPrevious,
    ListNext,
    ListPageUp,
    ListPageDown,
    HistoryNewTab,
    HistoryForget,
    DialogAccept,
    DialogDismiss,
    UploadComplete,
    Hints,
    HintsNewTab,
    ScrollDown,
    ScrollUp,
    HalfDown,
    HalfUp,
    TopPrefix,
    ScrollTop,
    ScrollBottom,
    Back,
    Forward,
    Reload,
    Url,
    NewTab,
    Find,
    Insert,
    HintsCancel,
    HintsBackspace,
    PageEscape,
}
const LOCAL_ACTIONS: &[(&str, LocalAction)] = &[
    ("line-home", LocalAction::LineHome),
    ("line-end", LocalAction::LineEnd),
    ("line-left", LocalAction::LineLeft),
    ("line-right", LocalAction::LineRight),
    ("line-word-left", LocalAction::LineWordLeft),
    ("line-word-right", LocalAction::LineWordRight),
    ("line-delete-back", LocalAction::LineDeleteBack),
    ("line-delete-forward", LocalAction::LineDeleteForward),
    ("line-delete-word-back", LocalAction::LineDeleteWordBack),
    (
        "line-delete-word-forward",
        LocalAction::LineDeleteWordForward,
    ),
    ("line-kill-start", LocalAction::LineKillStart),
    ("line-kill-end", LocalAction::LineKillEnd),
    ("line-hint", LocalAction::LineHint),
    ("line-previous", LocalAction::LinePrevious),
    ("line-next", LocalAction::LineNext),
    ("accept", LocalAction::Accept),
    ("cancel", LocalAction::Cancel),
    ("find-next", LocalAction::FindNext),
    ("find-previous", LocalAction::FindPrevious),
    ("list-previous", LocalAction::ListPrevious),
    ("list-next", LocalAction::ListNext),
    ("list-page-up", LocalAction::ListPageUp),
    ("list-page-down", LocalAction::ListPageDown),
    ("history-new-tab", LocalAction::HistoryNewTab),
    ("history-forget", LocalAction::HistoryForget),
    ("dialog-accept", LocalAction::DialogAccept),
    ("dialog-dismiss", LocalAction::DialogDismiss),
    ("upload-complete", LocalAction::UploadComplete),
    ("hints", LocalAction::Hints),
    ("hints-new-tab", LocalAction::HintsNewTab),
    ("scroll-down", LocalAction::ScrollDown),
    ("scroll-up", LocalAction::ScrollUp),
    ("half-down", LocalAction::HalfDown),
    ("half-up", LocalAction::HalfUp),
    ("top-prefix", LocalAction::TopPrefix),
    ("scroll-top", LocalAction::ScrollTop),
    ("scroll-bottom", LocalAction::ScrollBottom),
    ("back", LocalAction::Back),
    ("forward", LocalAction::Forward),
    ("reload", LocalAction::Reload),
    ("url", LocalAction::Url),
    ("new-tab", LocalAction::NewTab),
    ("find", LocalAction::Find),
    ("insert", LocalAction::Insert),
    ("hints-cancel", LocalAction::HintsCancel),
    ("hints-backspace", LocalAction::HintsBackspace),
    ("page-escape", LocalAction::PageEscape),
];
impl LocalAction {
    fn scope(self) -> Scope {
        use LocalAction::*;
        match self {
            FindNext | FindPrevious => Scope::Find,
            ListPrevious | ListNext | ListPageUp | ListPageDown => Scope::List,
            HistoryNewTab | HistoryForget => Scope::History,
            DialogAccept | DialogDismiss => Scope::Dialog,
            UploadComplete => Scope::Upload,
            HintsCancel | HintsBackspace => Scope::Hints,
            PageEscape => Scope::Page,
            Hints | HintsNewTab | ScrollDown | ScrollUp | HalfDown | HalfUp | TopPrefix
            | ScrollTop | ScrollBottom | Back | Forward | Reload | Url | NewTab | Find | Insert => {
                Scope::Normal
            }
            _ => Scope::Line,
        }
    }
    pub fn name(self) -> &'static str {
        LOCAL_ACTIONS
            .iter()
            .find(|(_, a)| *a == self)
            .map_or("", |(name, _)| *name)
    }
}

/// Exact local builtin chords. Text entry and hint label typing are fallback
/// behaviors, not commands; an explicit `none` runs before those fallbacks.
pub const LOCAL_DEFAULTS: &[(Scope, &str, LocalAction)] = &[
    (Scope::Line, "home, ctrl+a", LocalAction::LineHome),
    (Scope::Line, "end, ctrl+e", LocalAction::LineEnd),
    (Scope::Line, "left, ctrl+b", LocalAction::LineLeft),
    (Scope::Line, "right, ctrl+f", LocalAction::LineRight),
    (
        Scope::Line,
        "alt+b, alt+left, ctrl+left",
        LocalAction::LineWordLeft,
    ),
    (
        Scope::Line,
        "alt+f, alt+right, ctrl+right",
        LocalAction::LineWordRight,
    ),
    (
        Scope::Line,
        "backspace, ctrl+h",
        LocalAction::LineDeleteBack,
    ),
    (
        Scope::Line,
        "delete, ctrl+d",
        LocalAction::LineDeleteForward,
    ),
    (
        Scope::Line,
        "ctrl+w, alt+backspace, ctrl+backspace",
        LocalAction::LineDeleteWordBack,
    ),
    (
        Scope::Line,
        "alt+d, alt+delete, ctrl+delete",
        LocalAction::LineDeleteWordForward,
    ),
    (Scope::Line, "ctrl+u", LocalAction::LineKillStart),
    (Scope::Line, "ctrl+k", LocalAction::LineKillEnd),
    (Scope::Line, "tab", LocalAction::LineHint),
    (Scope::Line, "up, ctrl+p", LocalAction::LinePrevious),
    (Scope::Line, "down, ctrl+n", LocalAction::LineNext),
    (Scope::Line, "enter", LocalAction::Accept),
    (Scope::Line, "esc", LocalAction::Cancel),
    (Scope::Find, "enter, ctrl+g", LocalAction::FindNext),
    (Scope::Find, "shift+enter", LocalAction::FindPrevious),
    (Scope::List, "pageup", LocalAction::ListPageUp),
    (Scope::List, "pagedown", LocalAction::ListPageDown),
    (
        Scope::History,
        "ctrl+enter, alt+enter",
        LocalAction::HistoryNewTab,
    ),
    (Scope::History, "shift+delete", LocalAction::HistoryForget),
    (
        Scope::Dialog,
        "enter, y, shift+y",
        LocalAction::DialogAccept,
    ),
    (Scope::Dialog, "esc, n, shift+n", LocalAction::DialogDismiss),
    (Scope::Upload, "tab", LocalAction::UploadComplete),
    (Scope::Normal, "f", LocalAction::Hints),
    (Scope::Normal, "shift+f", LocalAction::HintsNewTab),
    (Scope::Normal, "j", LocalAction::ScrollDown),
    (Scope::Normal, "k", LocalAction::ScrollUp),
    (Scope::Normal, "d", LocalAction::HalfDown),
    (Scope::Normal, "u", LocalAction::HalfUp),
    (Scope::Normal, "g", LocalAction::TopPrefix),
    (Scope::Normal, "shift+g", LocalAction::ScrollBottom),
    (Scope::Normal, "shift+h", LocalAction::Back),
    (Scope::Normal, "shift+l", LocalAction::Forward),
    (Scope::Normal, "r", LocalAction::Reload),
    (Scope::Normal, "o", LocalAction::Url),
    (Scope::Normal, "shift+o", LocalAction::NewTab),
    (Scope::Normal, "/", LocalAction::Find),
    (Scope::Normal, "i", LocalAction::Insert),
    (Scope::Hints, "esc", LocalAction::HintsCancel),
    (Scope::Hints, "backspace", LocalAction::HintsBackspace),
    (Scope::Page, "esc", LocalAction::PageEscape),
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
        if let Action::Local(action) = self {
            return action.name().to_string();
        }
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

/// Linux default chords, for documentation and compatibility checks.
/// Runtime lookup uses the selected keymap through [`default_action`].
pub fn defaults(action: Action) -> Vec<Chord> {
    default_chords(Keymap::Linux, action)
}

pub fn default_keys(keymap: Keymap, action: Action) -> String {
    if keymap == Keymap::Linux {
        if let Action::Tab(n) = action {
            return format!("alt+{n}");
        }
        return ACTIONS
            .iter()
            .find(|r| r.action == action)
            .map_or(String::new(), |r| r.keys.to_string());
    }
    match action {
        Action::Quit => "ctrl+q",
        Action::EditUrl => "ctrl+l",
        Action::Reload => "ctrl+r",
        Action::Back => "super+[",
        Action::Forward => "super+]",
        Action::NewTab => "ctrl+t",
        Action::CloseTab => "ctrl+w",
        Action::ReopenTab => "ctrl+y",
        Action::Bookmark => "super+d",
        Action::PreviousTab => "ctrl+[",
        Action::NextTab => "ctrl+]",
        Action::Tab(n) => return format!("ctrl+{n}"),
        Action::LastTab => "ctrl+9",
        Action::ListTabs => "ctrl+e",
        Action::History => "ctrl+h",
        Action::MoveTabLeft => "ctrl+pageup",
        Action::MoveTabRight => "ctrl+pagedown",
        Action::ZoomIn => "ctrl+=",
        Action::ZoomOut => "ctrl+-",
        Action::ZoomReset => "ctrl+0",
        Action::Find => "ctrl+f",
        Action::Permissions => "ctrl+p",
        Action::Block => "ctrl+b",
        Action::FillLogin => "ctrl+g",
        Action::Copy => "ctrl+c",
        Action::CopyUrl => "ctrl+;",
        Action::SavePdf => "ctrl+s",
        Action::SaveScreenshot => "ctrl+shift+d",
        Action::ToggleNormal => "ctrl+.",
        Action::Local(_) => "",
    }
    .to_string()
}
pub fn default_chords(keymap: Keymap, action: Action) -> Vec<Chord> {
    default_keys(keymap, action)
        .split(',')
        .filter_map(|s| Chord::parse(s).ok())
        .collect()
}
pub fn default_action(keymap: Keymap, input: &KeyInput) -> Option<Action> {
    if input.action == KeyAction::Release {
        return None;
    }
    Action::every()
        .into_iter()
        .find(|a| default_chords(keymap, *a).iter().any(|c| c.matches(input)))
}
pub fn help() -> String {
    help_for(Keymap::default())
}
pub fn help_for(keymap: Keymap) -> String {
    let mut out = format!(
        "\nactions (key.<chord> = <action>; none unbinds; keymap={}):\n",
        keymap.name()
    );
    for row in &ACTIONS {
        let keys = if matches!(row.action, Action::Tab(_)) {
            format!(
                "{}+1 .. {}+8",
                if keymap == Keymap::Mac { "ctrl" } else { "alt" },
                if keymap == Keymap::Mac { "ctrl" } else { "alt" }
            )
        } else {
            default_keys(keymap, row.action)
        };
        out.push_str(&format!("  {:<18} {}  {}\n", row.name, keys, row.what));
    }
    out.push_str("Platform default: mac on macOS, linux elsewhere. --keymap mac|linux selects it.\nA chord joins ctrl, alt, shift or super (cmd) with + to a character, plus,\nspace, tab, enter, esc, backspace, insert, delete, arrows, home, end,\npageup, pagedown or f1..f24. Global keys need ctrl, alt or super, or an f-key.\nScoped keys may be bare: key.<scope>.<chord> = <action> or none.\nScopes: line, url, find, list, history, dialog, prompt, upload, normal, hints, page.\n");
    for scope in [
        Scope::Line,
        Scope::Find,
        Scope::List,
        Scope::History,
        Scope::Dialog,
        Scope::Upload,
        Scope::Normal,
        Scope::Hints,
        Scope::Page,
    ] {
        out.push_str(&format!(
            "  {scope:?}: {}\n",
            LOCAL_ACTIONS
                .iter()
                .filter(|(_, a)| a.scope() == scope)
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out
}

/// One line of the file: `None` is `= none`, the chord unbound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub scope: Scope,
    pub chord: Chord,
    pub action: Option<Action>,
}

impl Binding {
    /// `key.<chord> = <value>`'s two halves, `chord` without the `key.`.
    /// Checked in the order they are written: the chord's spelling, then
    /// that it has ctrl, alt or super or is an f-key (see the module's
    /// section on it), then the action.
    pub fn parse(chord: &str, value: &str) -> Result<Binding, String> {
        let (scope, chord) = match chord.split_once('.') {
            Some((name, chord)) if Scope::parse(name).is_some() => {
                (Scope::parse(name).unwrap_or_default(), chord)
            }
            _ => (Scope::Global, chord),
        };
        let written = chord.trim();
        let chord = Chord::parse(written).map_err(|why| format!("key.{why}"))?;
        let held = chord.mods.0 & (Mods::CTRL | Mods::ALT | Mods::SUPER) != 0;
        if scope == Scope::Global && !held && !matches!(chord.key, Key::Function(_)) {
            return Err(format!(
                "key.{written}: a key with no ctrl, alt or super is the page's; add one, or use f1..f24"
            ));
        }
        let action = if value.trim().eq_ignore_ascii_case("none") {
            None
        } else {
            Some(if scope == Scope::Global {
                Action::parse(value)?
            } else if value.trim() == "quit" {
                Action::Quit
            } else {
                let action = LOCAL_ACTIONS
                    .iter()
                    .find(|(name, action)| *name == value.trim() && scope.accepts(*action))
                    .map(|(_, a)| *a)
                    .ok_or_else(|| format!("{value:?} is not an action for {scope:?}"))?;
                Action::Local(action)
            })
        };
        Ok(Binding {
            scope,
            chord,
            action,
        })
    }
}

/// Every `key.` line, in file order.
///
/// A later line for the same chord replaces an earlier one, so that a file
/// can be appended to, and that is not a duplicate the parser refuses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bindings {
    rows: Vec<Binding>,
    pub keymap: Keymap,
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
        Bindings {
            rows,
            keymap: Keymap::default(),
        }
    }

    /// What the rows say about `input`. A release is never a command, as the
    /// built-in table never makes one of it, so it is `Default` before any
    /// row is looked at.
    pub fn lookup(&self, input: &KeyInput) -> Lookup {
        if input.action == KeyAction::Release {
            return Lookup::Default;
        }
        match self
            .rows
            .iter()
            .rev()
            .find(|row| row.scope == Scope::Global && row.chord.matches(input))
        {
            Some(Binding {
                action: Some(action),
                ..
            }) => Lookup::Bound(*action),
            Some(Binding { action: None, .. }) => Lookup::Unbound,
            None => Lookup::Default,
        }
    }

    /// Explicit local override, then inherited common edits. `none` consumes
    /// the key before either the builtin or ordinary text insertion.
    pub fn scoped(&self, scope: Scope, input: &KeyInput) -> Lookup {
        if input.action == KeyAction::Release {
            return Lookup::Default;
        }
        for scope in std::iter::once(scope).chain(scope.parents().iter().copied()) {
            if let Some(row) = self
                .rows
                .iter()
                .rev()
                .find(|row| row.scope == scope && row.chord.matches(input))
            {
                return row.action.map_or(Lookup::Unbound, Lookup::Bound);
            }
        }
        Lookup::Default
    }
    /// Local override first, inherited overrides next, then exact builtin.
    /// Global `none` suppresses a local fallback too (notably the quit key).
    pub fn local(&self, scope: Scope, input: &KeyInput) -> Lookup {
        let explicit = self.scoped(scope, input);
        if explicit != Lookup::Default {
            return explicit;
        }
        if self.lookup(input) == Lookup::Unbound {
            return Lookup::Unbound;
        }
        if input.action == KeyAction::Release {
            return Lookup::Default;
        }
        for current in std::iter::once(scope).chain(scope.parents().iter().copied()) {
            // A prompt's dialog confirmation letters are ordinary text.
            if scope == Scope::Prompt && current == Scope::Dialog {
                continue;
            }
            for (owner, keys, action) in LOCAL_DEFAULTS {
                if *owner == current
                    && keys
                        .split(',')
                        .filter_map(|k| Chord::parse(k).ok())
                        .any(|c| c.matches(input))
                {
                    return Lookup::Bound(Action::Local(*action));
                }
            }
        }
        Lookup::Default
    }
    pub fn with_keymap(mut self, keymap: Keymap) -> Self {
        self.keymap = keymap;
        self
    }
    pub fn action(&self, input: &KeyInput) -> Option<Action> {
        match self.lookup(input) {
            Lookup::Bound(a) => Some(a),
            Lookup::Unbound => None,
            Lookup::Default => default_action(self.keymap, input),
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
    fn every_default_chord_in_the_table_parses_and_spells_back_the_same() {
        for row in &ACTIONS {
            if matches!(row.action, Action::Tab(_)) {
                continue;
            }
            let spelled: Vec<&str> = row.keys.split(',').map(str::trim).collect();
            let chords = defaults(row.action);
            assert_eq!(chords.len(), spelled.len(), "{}: {}", row.name, row.keys);
            for (chord, spelled) in chords.iter().zip(spelled) {
                assert_eq!(chord.spell(), spelled, "{}", row.name);
            }
        }
        for n in 1..=8 {
            let spelled: Vec<String> = defaults(Action::Tab(n)).iter().map(Chord::spell).collect();
            assert_eq!(spelled, [format!("alt+{n}")]);
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
    fn the_help_lists_every_action_once_with_its_default_chords() {
        for keymap in [Keymap::Mac, Keymap::Linux] {
            let help = help_for(keymap);
            for row in &ACTIONS {
                assert!(
                    help.lines().any(|l| l.trim().starts_with(row.name)),
                    "{}",
                    row.name
                );
            }
            assert!(help.contains("Scopes: line"));
            assert!(help.contains(&format!("keymap={}", keymap.name())));
        }
    }

    /// The rows of `docs/configuration.md`'s `### Rebinding keys` table, each cell with
    /// its backticks gone and `…` written `..`, as [`ACTIONS`] writes a range.
    fn readme_rows() -> Vec<Vec<String>> {
        include_str!("../docs/configuration.md")
            .lines()
            .skip_while(|line| *line != "### Rebinding keys")
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

    #[test]
    fn the_readme_s_rebinding_table_is_the_action_table() {
        let rows = readme_rows();
        let names: Vec<&str> = rows.iter().map(|cells| cells[0].as_str()).collect();
        let wanted: Vec<&str> = ACTIONS.iter().map(|row| row.name).collect();
        assert_eq!(names, wanted);
        for (cells, row) in rows.iter().zip(&ACTIONS) {
            if matches!(row.action, Action::Tab(_)) {
                assert_eq!(cells[1], row.keys);
                continue;
            }
            let written: Vec<String> = cells[1]
                .split(',')
                .map(|chord| Chord::parse(chord).expect(chord).spell())
                .collect();
            let wanted: Vec<String> = defaults(row.action).iter().map(Chord::spell).collect();
            assert_eq!(written, wanted, "{}", row.name);
            assert_eq!(cells[3], row.what, "{}", row.name);
        }
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
                    "copy-url, save-pdf, save-screenshot, normal-mode, tab-1..tab-8, or none"
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
            scope: Scope::Global,
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
    #[test]
    fn both_platform_tables_cover_every_action_with_exact_non_conflicting_chords() {
        for keymap in [Keymap::Mac, Keymap::Linux] {
            let mut used = Vec::new();
            for action in Action::every() {
                let defaults = default_chords(keymap, action);
                assert!(!defaults.is_empty(), "{keymap:?} {action:?}");
                for chord in defaults {
                    assert!(!used.contains(&chord), "duplicate {}", chord.spell());
                    assert!(
                        crate::kitty_keys::collision(&chord, true).is_none(),
                        "{keymap:?}: {}",
                        chord.spell()
                    );
                    assert_eq!(
                        default_action(keymap, &key(chord.key, chord.mods.0)),
                        Some(action)
                    );
                    used.push(chord);
                }
            }
        }
        for (_, keys, _) in LOCAL_DEFAULTS {
            for chord in keys.split(',').map(|k| Chord::parse(k).expect(k)) {
                assert!(
                    crate::kitty_keys::collision(&chord, true).is_none(),
                    "local {}",
                    chord.spell()
                );
            }
        }
    }
    #[test]
    fn scopes_validate_actions_and_specific_overrides_beat_common_edits() {
        let bindings = Bindings::from_rows(vec![
            Binding::parse("line.ctrl+a", "line-end").unwrap(),
            Binding::parse("url.ctrl+a", "line-home").unwrap(),
        ]);
        assert_eq!(
            bindings.scoped(Scope::Url, &key(Key::Char('a'), Mods::CTRL)),
            Lookup::Bound(Action::Local(LocalAction::LineHome))
        );
        assert_eq!(
            bindings.scoped(Scope::Find, &key(Key::Char('a'), Mods::CTRL)),
            Lookup::Bound(Action::Local(LocalAction::LineEnd))
        );
        assert!(Binding::parse("normal.j", "scroll-top").is_ok());
        assert!(Binding::parse("line.j", "scroll-top").is_err());
        assert!(Binding::parse("j", "reload").is_err());
    }
}
