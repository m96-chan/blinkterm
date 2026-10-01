//! What a terminal and the blinkterm that drives its window say to each
//! other, and the private socket they say it on.
//!
//! Issue #83 splits one blinkterm into two kinds of process. A *backend* per
//! profile holds the profile lock, the engine and every window's logic — the
//! url bar, the lists, the motion policy, the session; a *frontend* per
//! terminal holds the terminal: raw mode, the parser, the painter, the
//! keybinding configuration and the helper programs a person sees (file
//! pickers, password commands, the external browser). This module is the
//! wire between the two, and nothing else: the messages, their framing, the
//! socket a backend listens on, and a writer that never blocks the loop that
//! feeds it. [`crate::backend`] and [`crate::frontend`] are the two ends.
//!
//! # Framing
//!
//! ```text
//! u32 BE total | u32 BE header_len | header: JSON | body: raw bytes
//! ```
//!
//! `total` counts everything after itself: the four bytes of `header_len`,
//! the header and the body. The header is one JSON object, built and read
//! with [`crate::json`] like every CDP message, whose `"t"` names the
//! message; the body is whatever bytes the message carries that would be a
//! waste to spell as JSON. Only three do: [`ToFrontend::Text`] (bytes for
//! the terminal), [`ToFrontend::Frame`] (an encoded picture) and a
//! [`ToBackend::HelperDone`] whose password command found a login (what it
//! printed). A body on any other message is an error, as is a `"t"` nobody
//! knows: the receiver hangs up rather than guess, because the two ends are
//! always the same build ([`ToBackend::Hello`] makes sure of that) and a
//! message it does not know is a peer it should not be talking to.
//!
//! The limits are read off the first eight bytes, before anything is held
//! for the message: a header over [`MAX_HEADER`] or a body over [`MAX_BODY`]
//! is an error at once, not an allocation. [`MAX_BODY`] is
//! [`crate::cdp::MAX_MESSAGE`], since the largest body is a frame that came
//! off the engine's pipe under that limit. [`MAX_HEADER`] is 1 MiB because
//! the largest header is a paste: [`crate::input::PASTE_LIMIT`] bytes of text
//! that JSON can make up to six times longer (`\u001b` for one byte), and
//! nothing else comes near it.
//!
//! # The endpoint
//!
//! A backend listens on [`SOCKET_FILE`] inside its profile, beside the lock
//! and the `--remote` socket, bound exactly the way [`crate::remote`] binds
//! that one (`remote::bind_private`): only under the profile lock,
//! over whatever a crash left; 0600 inside the 0700 profile; and when the
//! path is too long for `sun_path` or the filesystem cannot hold a socket, in
//! a fresh 0700 directory of its own behind a symlink. [`connect`] follows
//! the link, and reads a file nobody is listening on — what a `SIGKILL`
//! leaves — as [`Connect::NobodyThere`], the same as no file at all.
//!
//! The directory is the access control, as it is for the `--remote` socket,
//! and [`accept`] checks once more: a peer whose user is not this process's
//! effective user is hung up on before a byte is read from it. On Linux that
//! is `SO_PEERCRED`, on a Mac `getpeereid(3)`; on anything else the peer
//! cannot be asked, and is refused. What a connected peer can do is drive a
//! window: everything the keyboard can, through typed messages — no CDP, no
//! script.
//!
//! # The outbox
//!
//! A backend serves several terminals from one loop, and a terminal that
//! stops reading — suspended, its ssh link gone quiet — must not stop the
//! others. So each connection gets an [`Outbox`]: a queue drained into a
//! non-blocking socket as far as the socket takes it, with a byte cap
//! ([`MAX_OUTBOX`]) past which the connection is given up on, and the rule
//! [`crate::screen::Outbox`] already has for the terminal: a frame not yet
//! started is replaced by a newer one rather than queued behind it.
//!
//! # A secret on the wire
//!
//! A password command's output crosses once, from the frontend that ran it
//! to the backend that fills the form, as the body of a
//! [`ToBackend::HelperDone`]. It is held in a [`Secret`] at both ends, which
//! is overwritten with zeros when it is dropped; the [`Decoder`] clears every
//! byte it has consumed, and the [`Outbox`] every message it has written or
//! thrown away, so no copy of it lingers in either buffer.

use std::collections::VecDeque;
use std::io::{ErrorKind, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use crate::appearance::{self, Alpha};
use crate::bindings::{Binding, Bindings, Keymap};
use crate::block;
use crate::download;
use crate::engine::Launch;
use crate::fit::{Cells, Metrics};
use crate::input::{Input, Key, KeyAction, KeyInput, Mods, MouseInput, MouseKind};
use crate::json::Json;
use crate::login;
use crate::options::Options;
use crate::picker::{self, Command};
use crate::save::Paper;
use crate::sites;
use crate::zoom::Scale;

/// The protocol's version. [`ToBackend::Hello`] carries it, and the
/// program's own version beside it; a backend refuses a frontend whose
/// either differs.
pub const PROTOCOL: u32 = 1;

/// The backend socket's name inside the profile, next to the lock.
pub const SOCKET_FILE: &str = "backend.sock";

/// The most a header may be, in bytes. See the module's section on framing
/// for why a megabyte.
pub const MAX_HEADER: usize = 1 << 20;

/// The most a body may be, in bytes: the most a CDP message may be.
pub const MAX_BODY: usize = crate::cdp::MAX_MESSAGE;

/// The most one connection's [`Outbox`] may hold before the peer is taken
/// to have stopped reading. Room for a hundred frames and then some; a
/// frontend that is that far behind is not catching up.
pub const MAX_OUTBOX: usize = 32 << 20;

/// This program's version, which both ends must share.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The name the fallback directory starts with: a different one from the
/// `--remote` socket's, so that clearing one never removes the other.
const FALLBACK_PREFIX: &str = "blinkterm-backend-";

/// The largest integer a JSON number carries exactly.
const MAX_EXACT: f64 = 9_007_199_254_740_991.0;

/// Where the backend socket (or the link to it) is, in the profile at
/// `profile`.
pub fn socket_path(profile: &Path) -> PathBuf {
    profile.join(SOCKET_FILE)
}

// ---------------------------------------------------------------------------
// The messages.
// ---------------------------------------------------------------------------

/// What a frontend says to its backend.
#[derive(Debug, PartialEq)]
pub enum ToBackend {
    /// The first thing said on a connection: who is asking, for which
    /// profile. `dir` is the canonical profile directory, `None` for a
    /// temporary profile, which only its own frontend can reach.
    Hello {
        protocol: u32,
        version: String,
        dir: Option<PathBuf>,
    },
    /// Open a window, or resume the one `nonce` named before.
    Open(Box<Open>),
    /// What the terminal said, other than the reports the frontend answers
    /// itself ([`Input::Mode`], [`Input::CellSize`]).
    Input { input: Input, pixel_mouse: bool },
    /// The terminal is a different size now.
    Resize { metrics: Metrics, viewport_gen: u32 },
    /// Frame `seq` was painted or dropped. `waited_ms` is how long the
    /// terminal took to take it, when the frontend measured one
    /// ([`crate::screen::Outbox::take_frame_wait`]).
    Painted {
        seq: u64,
        waited_ms: Option<u64>,
        viewport_gen: u32,
    },
    /// A helper the backend asked for has finished.
    HelperDone { id: u64, outcome: Outcome },
    /// This window is done.
    Close { why: CloseWhy },
    /// Are you there.
    Ping,
}

/// What [`ToBackend::Open`] carries: everything a window is made from.
#[derive(Debug, Clone, PartialEq)]
pub struct Open {
    /// Sixteen lowercase hex digits the frontend chose, so that a retry
    /// resumes the window rather than making a second one.
    pub nonce: String,
    pub window: WindowSettings,
    pub browser: BrowserSettings,
    pub metrics: Metrics,
    pub route: RouteFlags,
    pub pixel_mouse: bool,
    /// The urls this start was given, not yet normalised.
    pub urls: Vec<String>,
    /// `--restore`.
    pub restore: bool,
    /// What the frontend found wrong before it attached, for the row.
    pub problems: Vec<String>,
    /// The frontend's working directory, which relative paths are read from.
    pub cwd: PathBuf,
    /// The frontend's `$HOME`, when it has one: `~` in a path and a
    /// command. (Not [`WindowSettings::home`], which is a page.)
    pub home_dir: Option<PathBuf>,
    /// Whether the terminal this window is drawn on can open a window of its
    /// own — `$DISPLAY` or `$WAYLAND_DISPLAY`, or a Mac not reached over ssh:
    /// [`crate::picker::has_display`] in the frontend's environment, not the
    /// backend's. A backend started from a desktop serves a terminal reached
    /// over ssh too, and the other way round; this is what decides, per
    /// window, a picker's kind, a password command's and `alt+o` (#104).
    pub display: bool,
}

/// What a backend says to a frontend.
#[derive(Debug, PartialEq)]
pub enum ToFrontend {
    /// The answer to a [`ToBackend::Hello`] it takes.
    Welcome {
        protocol: u32,
        version: String,
        pid: u32,
        generation: u64,
        dir: PathBuf,
        label: String,
    },
    /// The answer to one it does not; `retry` when trying again shortly may
    /// get a different one.
    Refused { why: String, retry: bool },
    /// The window is open: a new one, or `resumed`.
    Opened { window: u64, resumed: bool },
    /// Bytes to write to the terminal, in order.
    Text(Vec<u8>),
    /// Take the picture down.
    ClearPicture,
    /// Clear the screen, and forget what was placed on it.
    ClearScreen,
    /// A picture to paint.
    Frame(Frame),
    /// Run a helper program on the terminal's side.
    Helper { id: u64, job: Job },
    /// The window is gone; the frontend exits with `exit`.
    Closed { why: String, exit: u8 },
    /// Yes.
    Pong,
    /// On the pair to the frontend that started this backend, before
    /// anything else: it holds the profile and its engine is up.
    Ready { dir: PathBuf, pid: u32 },
    /// On the pair: another process (`pid`) holds the profile.
    Busy { pid: u32 },
    /// On the pair: it could not start, and why.
    Failed { why: String },
}

/// What [`ToFrontend::Frame`] carries.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Counted per window; [`ToBackend::Painted`] gives it back.
    pub seq: u64,
    /// The size the frame was laid out for; one from before the last
    /// [`ToBackend::Resize`] is dropped (and still acknowledged).
    pub viewport_gen: u32,
    pub cells: Cells,
    /// The row the picture starts on, zero-based.
    pub row: u32,
    pub kind: FrameKind,
    /// The encoded picture.
    pub image: Vec<u8>,
}

/// How a frame's picture is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Jpeg,
    Png,
}

/// What the frontend's route decided about its frames: whether the engine's
/// acks wait for the terminal, whether frames go as PNG, whether they are
/// keyed for `--alpha`, and at what alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RouteFlags {
    pub paced: bool,
    pub png: bool,
    pub keyed: bool,
    pub alpha: Option<u8>,
    /// The motion cast's `everyNthFrame`, from the frame-rate cap
    /// ([`crate::route::Route::every_nth`]); 1 is every frame.
    pub every_nth: u32,
}

/// Why a frontend is closing its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseWhy {
    /// The person quit it.
    Quit,
    /// The terminal went away (`SIGHUP`).
    Hangup,
    /// The terminal stopped answering: its input ended.
    Terminal,
}

impl CloseWhy {
    fn name(self) -> &'static str {
        match self {
            CloseWhy::Quit => "quit",
            CloseWhy::Hangup => "hangup",
            CloseWhy::Terminal => "terminal",
        }
    }
}

/// A helper program the backend wants run beside the terminal.
#[derive(Debug, Clone, PartialEq)]
pub enum Job {
    /// A file picker for the page's file input `node` in tab `tab`.
    Picker {
        tab: String,
        node: i64,
        session: Option<String>,
        command: Command,
        terminal: bool,
        dir: PathBuf,
        multiple: bool,
    },
    /// A password command for the page at `url` in tab `tab`.
    Login {
        tab: String,
        url: String,
        command: Command,
        terminal: bool,
        site: login::Site,
        dir: PathBuf,
    },
    /// Open `url` in another browser: `command`, or the platform's own.
    External {
        command: Option<Command>,
        url: String,
    },
}

/// How a [`Job`] ended.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Picker {
        tab: String,
        node: i64,
        result: Picked,
    },
    Login {
        tab: String,
        url: String,
        result: Fetched,
    },
    External(Result<(), String>),
}

/// What a file picker answered.
#[derive(Debug, Clone, PartialEq)]
pub enum Picked {
    Files(Vec<PathBuf>),
    Cancel,
    Failed(String),
}

/// What a password command answered.
#[derive(Debug, PartialEq)]
pub enum Fetched {
    /// What it printed, for [`crate::login::parse_output`]: the one body
    /// that is a secret.
    Found(Secret),
    None,
    Failed(String),
}

/// Bytes that are overwritten with zeros when they are dropped, and that
/// `{:?}` does not print: what a password command printed, on its way from
/// the frontend that ran it to the backend that fills the form.
#[derive(PartialEq, Eq)]
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(bytes: Vec<u8>) -> Secret {
        Secret(bytes)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        picker::scrub(&mut self.0);
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

// ---------------------------------------------------------------------------
// Settings.
// ---------------------------------------------------------------------------

/// The settings that are a window's own: each frontend sends its own in
/// [`ToBackend::Open`], and two windows on one backend can differ in every
/// one of them.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowSettings {
    pub scale: Scale,
    pub scheme: appearance::Choice,
    pub force_dark: bool,
    pub alpha: Alpha,
    pub normal_mode: bool,
    pub search_url: Option<String>,
    /// `--pdf-paper`, else the frontend's locale's ([`Paper::locale`]): the
    /// paper of the person at the terminal, not of whoever started the
    /// backend. A frontend always says; `None` is for a test's settings.
    pub pdf_paper: Option<Paper>,
    /// The page a window with no url opens.
    pub home: String,
    pub keymap: Keymap,
    /// The keymap's rows under the settings' `key.` lines; sent as the
    /// keymap's name and the lines, and built again from them.
    pub bindings: Bindings,
    pub pickers: picker::Pickers,
    pub logins: login::Programs,
    pub external_browser: Option<Command>,
}

impl From<&Options> for WindowSettings {
    fn from(options: &Options) -> WindowSettings {
        WindowSettings {
            scale: options.scale,
            scheme: options.scheme,
            force_dark: options.force_dark,
            alpha: options.alpha,
            normal_mode: options.normal_mode,
            search_url: options.search_url.clone(),
            // Resolved here, on the frontend, so that the locale read is the
            // terminal's (#104).
            pdf_paper: Some(
                options
                    .pdf_paper
                    .unwrap_or_else(|| Paper::from_locale(&Paper::locale())),
            ),
            home: options.home.clone(),
            keymap: options.keymap,
            bindings: options.bindings.clone(),
            pickers: options.pickers.clone(),
            logins: options.logins.clone(),
            external_browser: options.external_browser.clone(),
        }
    }
}

impl WindowSettings {
    pub fn to_json(&self) -> Json {
        let scale = match self.scale {
            Scale::Auto => Json::string("auto"),
            Scale::Fixed(n) => Json::Number(n),
        };
        let scheme = match self.scheme {
            appearance::Choice::Auto => "auto",
            appearance::Choice::Light => "light",
            appearance::Choice::Dark => "dark",
        };
        let alpha = match self.alpha {
            Alpha::Off => Json::Null,
            Alpha::On(n) => Json::number(n),
        };
        let paper = self.pdf_paper.map_or(Json::Null, |paper| {
            Json::string(match paper {
                Paper::A4 => "a4",
                Paper::Letter => "letter",
            })
        });
        let keys = self
            .bindings
            .iter()
            .map(|row| {
                let action = row.action.map_or("none".to_string(), |a| a.name());
                Json::Array(vec![Json::string(row.chord.spell()), Json::string(action)])
            })
            .collect();
        let pickers = &self.pickers;
        Json::object(vec![
            ("scale", scale),
            ("scheme", Json::string(scheme)),
            ("force_dark", Json::Bool(self.force_dark)),
            ("alpha", alpha),
            ("normal_mode", Json::Bool(self.normal_mode)),
            ("search_url", opt_string(self.search_url.as_deref())),
            ("pdf_paper", paper),
            ("home", Json::string(&self.home)),
            ("keymap", Json::string(self.keymap.name())),
            ("keys", Json::Array(keys)),
            (
                "pickers",
                Json::object(vec![
                    ("gui", opt_command(pickers.gui.as_ref())),
                    ("gui_multiple", opt_command(pickers.gui_multiple.as_ref())),
                    ("terminal", opt_command(pickers.terminal.as_ref())),
                    (
                        "terminal_multiple",
                        opt_command(pickers.terminal_multiple.as_ref()),
                    ),
                ]),
            ),
            (
                "logins",
                Json::object(vec![
                    ("gui", opt_command(self.logins.gui.as_ref())),
                    ("terminal", opt_command(self.logins.terminal.as_ref())),
                ]),
            ),
            (
                "external_browser",
                opt_command(self.external_browser.as_ref()),
            ),
        ])
    }

    pub fn from_json(json: &Json) -> Result<WindowSettings, String> {
        let scale = match field(json, "scale")? {
            Json::String(s) if s == "auto" => Scale::Auto,
            Json::Number(n) if (0.5..=4.0).contains(n) => Scale::Fixed(*n),
            _ => return Err(bad("scale")),
        };
        let scheme = appearance::Choice::parse(&string(json, "scheme")?)?;
        let alpha = match field(json, "alpha")? {
            Json::Null => Alpha::Off,
            n => match u8::try_from(whole(n, "alpha")?) {
                Ok(n @ 1..=100) => Alpha::On(n),
                _ => return Err(bad("alpha")),
            },
        };
        let pdf_paper = opt_str(json, "pdf_paper")?
            .map(|paper| Paper::parse(&paper))
            .transpose()?;
        let keymap = Keymap::parse(&string(json, "keymap")?)?;
        let mut rows = Vec::new();
        for row in array(json, "keys")? {
            match row.as_array() {
                Some([Json::String(chord), Json::String(action)]) => {
                    rows.push(Binding::parse(chord, action)?);
                }
                _ => return Err(bad("keys")),
            }
        }
        let pickers = field(json, "pickers")?;
        let logins = field(json, "logins")?;
        Ok(WindowSettings {
            scale,
            scheme,
            force_dark: boolean(json, "force_dark")?,
            alpha,
            normal_mode: boolean(json, "normal_mode")?,
            search_url: opt_str(json, "search_url")?,
            pdf_paper,
            home: string(json, "home")?,
            keymap,
            bindings: Bindings::on(keymap, rows),
            pickers: picker::Pickers {
                gui: opt_command_from(pickers, "gui")?,
                gui_multiple: opt_command_from(pickers, "gui_multiple")?,
                terminal: opt_command_from(pickers, "terminal")?,
                terminal_multiple: opt_command_from(pickers, "terminal_multiple")?,
            },
            logins: login::Programs {
                gui: opt_command_from(logins, "gui")?,
                terminal: opt_command_from(logins, "terminal")?,
            },
            external_browser: opt_command_from(json, "external_browser")?,
        })
    }
}

/// The settings that are the browser's, and so the profile's: one engine,
/// one download directory, one block list and one set of site styles serve
/// every window on a backend. The first frontend's are the ones it starts
/// with, and a later one that asks for different ones is refused, naming
/// the first that differs ([`BrowserSettings::differences`]); nothing is
/// silently ignored and nothing restarts under a window that is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSettings {
    pub engine: Launch,
    pub download: download::Choice,
    pub block: block::Lists,
    pub sites: sites::Location,
    pub console: bool,
}

impl From<&Options> for BrowserSettings {
    fn from(options: &Options) -> BrowserSettings {
        BrowserSettings {
            engine: options.engine.clone(),
            download: options.download.clone(),
            block: options.block.clone(),
            sites: options.sites.clone(),
            console: options.console,
        }
    }
}

impl BrowserSettings {
    pub fn to_json(&self) -> Json {
        let engine = &self.engine;
        let download = match &self.download {
            download::Choice::Default => Json::Null,
            download::Choice::At(dir) => path_json(dir),
        };
        Json::object(vec![
            (
                "engine",
                Json::object(vec![
                    ("path", engine.path.as_deref().map_or(Json::Null, path_json)),
                    ("args", strings_json(&engine.args)),
                    ("user_agent", opt_string(engine.user_agent.as_deref())),
                    ("proxy", opt_string(engine.proxy.as_deref())),
                    ("mute", Json::Bool(engine.mute)),
                ]),
            ),
            ("download", download),
            (
                "block",
                Json::object(vec![
                    (
                        "paths",
                        Json::Array(self.block.paths.iter().map(|p| path_json(p)).collect()),
                    ),
                    ("enabled", Json::Bool(self.block.enabled)),
                ]),
            ),
            (
                "sites",
                Json::object(vec![
                    (
                        "dir",
                        self.sites.dir.as_deref().map_or(Json::Null, path_json),
                    ),
                    ("enabled", Json::Bool(self.sites.enabled)),
                ]),
            ),
            ("console", Json::Bool(self.console)),
        ])
    }

    pub fn from_json(json: &Json) -> Result<BrowserSettings, String> {
        let engine = field(json, "engine")?;
        let block = field(json, "block")?;
        let sites = field(json, "sites")?;
        let download = match field(json, "download")? {
            Json::Null => download::Choice::Default,
            dir => download::Choice::At(path_value(dir, "download")?),
        };
        Ok(BrowserSettings {
            engine: Launch {
                path: opt_path(engine, "path")?,
                args: strings(engine, "args")?,
                user_agent: opt_str(engine, "user_agent")?,
                proxy: opt_str(engine, "proxy")?,
                mute: boolean(engine, "mute")?,
            },
            download,
            block: block::Lists {
                paths: array(block, "paths")?
                    .iter()
                    .map(|p| path_value(p, "paths"))
                    .collect::<Result<_, _>>()?,
                enabled: boolean(block, "enabled")?,
            },
            sites: sites::Location {
                dir: opt_path(sites, "dir")?,
                enabled: boolean(sites, "enabled")?,
            },
            console: boolean(json, "console")?,
        })
    }

    /// Every setting in which `self` and `other` differ, in a fixed order:
    /// the name a settings file gives it, then `self`'s value and `other`'s,
    /// each as a person would read it. Empty when they are the same.
    pub fn differences(&self, other: &BrowserSettings) -> Vec<(&'static str, String, String)> {
        fn or(value: Option<String>, otherwise: &str) -> String {
            value.unwrap_or_else(|| otherwise.to_string())
        }
        fn list(items: &[String]) -> String {
            if items.is_empty() {
                "none".to_string()
            } else {
                items.join(" ")
            }
        }
        let describe = |s: &BrowserSettings| -> [(&'static str, String); 11] {
            let path = |p: &Option<PathBuf>, otherwise: &str| {
                or(p.as_ref().map(|p| p.display().to_string()), otherwise)
            };
            [
                ("engine", path(&s.engine.path, "the PATH search")),
                ("engine-arg", list(&s.engine.args)),
                (
                    "user-agent",
                    or(s.engine.user_agent.clone(), "the engine's own"),
                ),
                ("proxy", or(s.engine.proxy.clone(), "none")),
                ("mute", s.engine.mute.to_string()),
                (
                    "download-dir",
                    match &s.download {
                        download::Choice::Default => "the default".to_string(),
                        download::Choice::At(dir) => dir.display().to_string(),
                    },
                ),
                (
                    "block-list",
                    list(
                        &s.block
                            .paths
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>(),
                    ),
                ),
                ("block", s.block.enabled.to_string()),
                ("sites-dir", path(&s.sites.dir, "the default")),
                ("sites", s.sites.enabled.to_string()),
                ("console", s.console.to_string()),
            ]
        };
        describe(self)
            .into_iter()
            .zip(describe(other))
            .filter(|((_, ours), (_, theirs))| ours != theirs)
            .map(|((key, ours), (_, theirs))| (key, ours, theirs))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// What the terminal said, as JSON.
// ---------------------------------------------------------------------------

/// `metrics` as `{cols, rows, cell: [w, h]}`.
pub fn metrics_to_json(metrics: &Metrics) -> Json {
    Json::object(vec![
        ("cols", Json::number(metrics.cols)),
        ("rows", Json::number(metrics.rows)),
        (
            "cell",
            Json::Array(vec![
                Json::number(metrics.cell.0),
                Json::number(metrics.cell.1),
            ]),
        ),
    ])
}

pub fn metrics_from_json(json: &Json) -> Result<Metrics, String> {
    let cell = match array(json, "cell")? {
        [w, h] => (u32_value(w, "cell")?, u32_value(h, "cell")?),
        _ => return Err(bad("cell")),
    };
    Ok(Metrics {
        cols: u32_field(json, "cols")?,
        rows: u32_field(json, "rows")?,
        cell,
    })
}

/// `cells` as `{cols, rows}`.
pub fn cells_to_json(cells: &Cells) -> Json {
    Json::object(vec![
        ("cols", Json::number(cells.cols)),
        ("rows", Json::number(cells.rows)),
    ])
}

pub fn cells_from_json(json: &Json) -> Result<Cells, String> {
    Ok(Cells {
        cols: u32_field(json, "cols")?,
        rows: u32_field(json, "rows")?,
    })
}

/// A key as a name (`"enter"`, `"pageup"`), or `{"char": "a"}`, `{"f": 5}`,
/// `{"other": 57441}`.
fn key_to_json(key: Key) -> Json {
    let name = match key {
        Key::Char(c) => return Json::object(vec![("char", Json::string(c))]),
        Key::Function(n) => return Json::object(vec![("f", Json::number(n))]),
        Key::Other(n) => return Json::object(vec![("other", Json::number(n))]),
        Key::Enter => "enter",
        Key::Tab => "tab",
        Key::Backspace => "backspace",
        Key::Escape => "escape",
        Key::Insert => "insert",
        Key::Delete => "delete",
        Key::Up => "up",
        Key::Down => "down",
        Key::Left => "left",
        Key::Right => "right",
        Key::Home => "home",
        Key::End => "end",
        Key::PageUp => "pageup",
        Key::PageDown => "pagedown",
    };
    Json::string(name)
}

fn key_from_json(json: &Json) -> Result<Key, String> {
    if let Some(name) = json.as_str() {
        return Ok(match name {
            "enter" => Key::Enter,
            "tab" => Key::Tab,
            "backspace" => Key::Backspace,
            "escape" => Key::Escape,
            "insert" => Key::Insert,
            "delete" => Key::Delete,
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "home" => Key::Home,
            "end" => Key::End,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            _ => return Err(bad("key")),
        });
    }
    if let Some(c) = json.get("char") {
        return one_char(c, "key").map(Key::Char);
    }
    if let Some(n) = json.get("f") {
        return u8::try_from(whole(n, "key")?)
            .map(Key::Function)
            .map_err(|_| bad("key"));
    }
    if let Some(n) = json.get("other") {
        return u32_value(n, "key").map(Key::Other);
    }
    Err(bad("key"))
}

pub fn key_input_to_json(key: &KeyInput) -> Json {
    let action = match key.action {
        KeyAction::Press => "press",
        KeyAction::Repeat => "repeat",
        KeyAction::Release => "release",
    };
    Json::object(vec![
        ("key", key_to_json(key.key)),
        ("mods", Json::number(key.mods.0)),
        ("action", Json::string(action)),
        ("text", key.text.map_or(Json::Null, Json::string)),
    ])
}

pub fn key_input_from_json(json: &Json) -> Result<KeyInput, String> {
    let action = match string(json, "action")?.as_str() {
        "press" => KeyAction::Press,
        "repeat" => KeyAction::Repeat,
        "release" => KeyAction::Release,
        _ => return Err(bad("action")),
    };
    let text = match field(json, "text")? {
        Json::Null => None,
        c => Some(one_char(c, "text")?),
    };
    Ok(KeyInput {
        key: key_from_json(field(json, "key")?)?,
        mods: Mods(u32_field(json, "mods")?),
        action,
        text,
    })
}

pub fn mouse_input_to_json(mouse: &MouseInput) -> Json {
    let kind = match mouse.kind {
        MouseKind::Press => "press",
        MouseKind::Release => "release",
        MouseKind::Move => "move",
        MouseKind::Wheel => "wheel",
    };
    Json::object(vec![
        ("event", Json::string(kind)),
        ("button", mouse.button.map_or(Json::Null, Json::number)),
        ("mods", Json::number(mouse.mods.0)),
        ("x", Json::number(mouse.x)),
        ("y", Json::number(mouse.y)),
        (
            "wheel",
            Json::Array(vec![
                Json::number(mouse.wheel.0),
                Json::number(mouse.wheel.1),
            ]),
        ),
    ])
}

pub fn mouse_input_from_json(json: &Json) -> Result<MouseInput, String> {
    let kind = match string(json, "event")?.as_str() {
        "press" => MouseKind::Press,
        "release" => MouseKind::Release,
        "move" => MouseKind::Move,
        "wheel" => MouseKind::Wheel,
        _ => return Err(bad("event")),
    };
    let button = match field(json, "button")? {
        Json::Null => None,
        n => Some(u32_value(n, "button")?),
    };
    let wheel = match array(json, "wheel")? {
        [x, y] => (i32_value(x, "wheel")?, i32_value(y, "wheel")?),
        _ => return Err(bad("wheel")),
    };
    Ok(MouseInput {
        kind,
        button,
        mods: Mods(u32_field(json, "mods")?),
        x: u32_field(json, "x")?,
        y: u32_field(json, "y")?,
        wheel,
    })
}

/// Any [`Input`], with a `"kind"` saying which. [`Input::Mode`] and
/// [`Input::CellSize`] have spellings too, though a frontend answers those
/// itself, so that every value has one.
pub fn input_to_json(input: &Input) -> Json {
    let (kind, mut fields): (&str, Vec<(&str, Json)>) = match input {
        Input::Key(key) => ("key", vec![("key", key_input_to_json(key))]),
        Input::Mouse(mouse) => ("mouse", vec![("mouse", mouse_input_to_json(mouse))]),
        Input::Mode { mode, state } => (
            "mode",
            vec![
                ("mode", Json::number(*mode)),
                ("state", Json::number(*state)),
            ],
        ),
        Input::Paste(text) => ("paste", vec![("text", Json::string(text))]),
        Input::PasteRefused { bytes } => (
            "paste_refused",
            vec![("bytes", Json::Number(*bytes as f64))],
        ),
        Input::PasteCut => ("paste_cut", Vec::new()),
        Input::Colour { slot, rgb } => (
            "colour",
            vec![
                ("slot", Json::number(*slot)),
                (
                    "rgb",
                    Json::Array(vec![
                        Json::number(rgb.0),
                        Json::number(rgb.1),
                        Json::number(rgb.2),
                    ]),
                ),
            ],
        ),
        Input::CellSize { width, height } => (
            "cell_size",
            vec![
                ("width", Json::number(*width)),
                ("height", Json::number(*height)),
            ],
        ),
    };
    fields.insert(0, ("kind", Json::string(kind)));
    Json::object(fields)
}

pub fn input_from_json(json: &Json) -> Result<Input, String> {
    let byte = |j: &Json| u8::try_from(whole(j, "rgb")?).map_err(|_| bad("rgb"));
    Ok(match string(json, "kind")?.as_str() {
        "key" => Input::Key(key_input_from_json(field(json, "key")?)?),
        "mouse" => Input::Mouse(mouse_input_from_json(field(json, "mouse")?)?),
        "mode" => Input::Mode {
            mode: u16::try_from(whole(field(json, "mode")?, "mode")?).map_err(|_| bad("mode"))?,
            state: u8::try_from(whole(field(json, "state")?, "state")?)
                .map_err(|_| bad("state"))?,
        },
        "paste" => Input::Paste(string(json, "text")?),
        "paste_refused" => Input::PasteRefused {
            bytes: usize::try_from(whole(field(json, "bytes")?, "bytes")?)
                .map_err(|_| bad("bytes"))?,
        },
        "paste_cut" => Input::PasteCut,
        "colour" => Input::Colour {
            slot: u32_field(json, "slot")?,
            rgb: match array(json, "rgb")? {
                [r, g, b] => (byte(r)?, byte(g)?, byte(b)?),
                _ => return Err(bad("rgb")),
            },
        },
        "cell_size" => Input::CellSize {
            width: u32_field(json, "width")?,
            height: u32_field(json, "height")?,
        },
        _ => return Err(bad("kind")),
    })
}

// ---------------------------------------------------------------------------
// Framing.
// ---------------------------------------------------------------------------

/// A message that can cross the socket: a header, and a body when it has
/// one.
pub trait Wire: Sized {
    /// The header — an object whose first field is `"t"` — and the body.
    fn to_header(&self) -> (Json, Option<&[u8]>);
    /// The message a header and a body make, or why they make none.
    fn from_wire(header: Json, body: Vec<u8>) -> Result<Self, String>;
}

/// `message`, framed. The frame is allocated once, at its size, so that no
/// copy of a body is left behind by a buffer that grew.
pub fn encode<M: Wire>(message: &M) -> Vec<u8> {
    let (header, body) = message.to_header();
    let header = header.to_string();
    let body = body.unwrap_or_default();
    let total = 4 + header.len() + body.len();
    let mut out = Vec::with_capacity(4 + total);
    out.extend_from_slice(&frame_length(total).to_be_bytes());
    out.extend_from_slice(&frame_length(header.len()).to_be_bytes());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(body);
    out
}

/// A length as the frame spells it. Nothing that is sent comes near four
/// gigabytes — a body is a frame the engine sent under [`MAX_BODY`] — so one
/// that does is a bug, and stops here rather than going out as a lie.
fn frame_length(n: usize) -> u32 {
    u32::try_from(n).expect("a message under four gigabytes")
}

/// Bytes off a connection, turned back into messages as they complete.
///
/// Reads end wherever the kernel likes; a message can arrive in a hundred
/// pieces or two in one, and [`Decoder::feed`] hands back whatever
/// messages are complete and keeps the rest.
#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// Take `bytes`, and give back every message they complete, in order.
    ///
    /// An error is a peer to hang up on: a length over a limit (seen in the
    /// first eight bytes, before anything is held for the message), a header
    /// that is not a JSON object with a `"t"`, a message nobody knows. What
    /// follows an error is not read.
    pub fn feed<M: Wire>(&mut self, bytes: &[u8]) -> Result<Vec<M>, String> {
        self.buffer.extend_from_slice(bytes);
        let mut messages = Vec::new();
        let mut at = 0;
        let result = loop {
            let rest = &self.buffer[at..];
            if rest.len() < 8 {
                break Ok(());
            }
            let total = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            let header_len = u32::from_be_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
            if header_len > MAX_HEADER {
                break Err(format!(
                    "a message header of {header_len} bytes; the most is {MAX_HEADER}"
                ));
            }
            let Some(body_len) = total.checked_sub(4 + header_len) else {
                break Err(format!(
                    "a message of {total} bytes with a header of {header_len}"
                ));
            };
            if body_len > MAX_BODY {
                break Err(format!(
                    "a message body of {body_len} bytes; the most is {MAX_BODY}"
                ));
            }
            if rest.len() < 4 + total {
                // Room for the rest of this one, once, so that the buffer
                // does not grow (and leave copies behind) as it arrives.
                let wanted = at + 4 + total;
                self.buffer
                    .reserve(wanted.saturating_sub(self.buffer.len()));
                break Ok(());
            }
            let header = &rest[8..8 + header_len];
            let body = rest[8 + header_len..4 + total].to_vec();
            at += 4 + total;
            match parse_header(header).and_then(|header| M::from_wire(header, body)) {
                Ok(message) => messages.push(message),
                Err(why) => break Err(why),
            }
        };
        picker::scrub(&mut self.buffer[..at]);
        self.buffer.drain(..at);
        if self.buffer.is_empty() && self.buffer.capacity() > 4 * MAX_HEADER {
            self.buffer = Vec::new();
        }
        result.map(|()| messages)
    }

    /// How many bytes are held towards a message not yet complete.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        picker::scrub(&mut self.buffer);
    }
}

/// A header's bytes as a JSON object with a string `"t"`.
fn parse_header(bytes: &[u8]) -> Result<Json, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "a message header that is not UTF-8")?;
    let header =
        Json::parse(text).map_err(|e| format!("a message header that is not JSON: {e}"))?;
    match header.get("t") {
        Some(Json::String(_)) => Ok(header),
        _ => Err("a message with no \"t\"".to_string()),
    }
}

/// The header with `t` first and `fields` after it.
fn header(t: &str, mut fields: Vec<(&str, Json)>) -> Json {
    fields.insert(0, ("t", Json::string(t)));
    Json::object(fields)
}

/// The sentence for a `"t"` nobody knows.
fn unknown(t: &str) -> String {
    format!("unknown message {t:?}")
}

/// An error unless `body` is empty: `t` carries none.
fn bodiless(t: &str, body: &[u8]) -> Result<(), String> {
    if body.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "a {t} message carries no body, and this one has {} bytes",
            body.len()
        ))
    }
}

impl Wire for ToBackend {
    fn to_header(&self) -> (Json, Option<&[u8]>) {
        let json = match self {
            ToBackend::Hello {
                protocol,
                version,
                dir,
            } => header(
                "hello",
                vec![
                    ("protocol", Json::number(*protocol)),
                    ("version", Json::string(version)),
                    ("dir", dir.as_deref().map_or(Json::Null, path_json)),
                ],
            ),
            ToBackend::Open(open) => header(
                "open",
                vec![
                    ("nonce", Json::string(&open.nonce)),
                    ("window", open.window.to_json()),
                    ("browser", open.browser.to_json()),
                    ("metrics", metrics_to_json(&open.metrics)),
                    (
                        "route",
                        Json::object(vec![
                            ("paced", Json::Bool(open.route.paced)),
                            ("png", Json::Bool(open.route.png)),
                            ("keyed", Json::Bool(open.route.keyed)),
                            ("alpha", open.route.alpha.map_or(Json::Null, Json::number)),
                            ("every_nth", Json::number(open.route.every_nth)),
                        ]),
                    ),
                    ("pixel_mouse", Json::Bool(open.pixel_mouse)),
                    ("urls", strings_json(&open.urls)),
                    ("restore", Json::Bool(open.restore)),
                    ("problems", strings_json(&open.problems)),
                    ("cwd", path_json(&open.cwd)),
                    (
                        "home_dir",
                        open.home_dir.as_deref().map_or(Json::Null, path_json),
                    ),
                    ("display", Json::Bool(open.display)),
                ],
            ),
            ToBackend::Input { input, pixel_mouse } => header(
                "input",
                vec![
                    ("input", input_to_json(input)),
                    ("pixel_mouse", Json::Bool(*pixel_mouse)),
                ],
            ),
            ToBackend::Resize {
                metrics,
                viewport_gen,
            } => header(
                "resize",
                vec![
                    ("metrics", metrics_to_json(metrics)),
                    ("viewport_gen", Json::number(*viewport_gen)),
                ],
            ),
            ToBackend::Painted {
                seq,
                waited_ms,
                viewport_gen,
            } => header(
                "painted",
                vec![
                    ("seq", Json::Number(*seq as f64)),
                    (
                        "waited_ms",
                        waited_ms.map_or(Json::Null, |ms| Json::Number(ms as f64)),
                    ),
                    ("viewport_gen", Json::number(*viewport_gen)),
                ],
            ),
            ToBackend::HelperDone { id, outcome } => {
                let body = match outcome {
                    Outcome::Login {
                        result: Fetched::Found(secret),
                        ..
                    } => Some(secret.bytes()),
                    _ => None,
                };
                return (
                    header(
                        "helper_done",
                        vec![
                            ("id", Json::Number(*id as f64)),
                            ("outcome", outcome_to_json(outcome)),
                        ],
                    ),
                    body,
                );
            }
            ToBackend::Close { why } => header("close", vec![("why", Json::string(why.name()))]),
            ToBackend::Ping => header("ping", vec![]),
        };
        (json, None)
    }

    fn from_wire(header: Json, body: Vec<u8>) -> Result<ToBackend, String> {
        let t = string(&header, "t")?;
        if t != "helper_done" {
            bodiless(&t, &body)?;
        }
        let h = &header;
        Ok(match t.as_str() {
            "hello" => ToBackend::Hello {
                protocol: u32_field(h, "protocol")?,
                version: string(h, "version")?,
                dir: opt_path(h, "dir")?,
            },
            "open" => {
                let nonce = string(h, "nonce")?;
                if !is_nonce(&nonce) {
                    return Err(bad("nonce"));
                }
                let route = field(h, "route")?;
                let alpha = match field(route, "alpha")? {
                    Json::Null => None,
                    n => Some(u8::try_from(whole(n, "alpha")?).map_err(|_| bad("alpha"))?),
                };
                ToBackend::Open(Box::new(Open {
                    nonce,
                    window: WindowSettings::from_json(field(h, "window")?)?,
                    browser: BrowserSettings::from_json(field(h, "browser")?)?,
                    metrics: metrics_from_json(field(h, "metrics")?)?,
                    route: RouteFlags {
                        paced: boolean(route, "paced")?,
                        png: boolean(route, "png")?,
                        keyed: boolean(route, "keyed")?,
                        alpha,
                        every_nth: u32::try_from(whole(field(route, "every_nth")?, "every_nth")?)
                            .map_err(|_| bad("every_nth"))?
                            .max(1),
                    },
                    pixel_mouse: boolean(h, "pixel_mouse")?,
                    urls: strings(h, "urls")?,
                    restore: boolean(h, "restore")?,
                    problems: strings(h, "problems")?,
                    cwd: path_value(field(h, "cwd")?, "cwd")?,
                    home_dir: opt_path(h, "home_dir")?,
                    display: boolean(h, "display")?,
                }))
            }
            "input" => ToBackend::Input {
                input: input_from_json(field(h, "input")?)?,
                pixel_mouse: boolean(h, "pixel_mouse")?,
            },
            "resize" => ToBackend::Resize {
                metrics: metrics_from_json(field(h, "metrics")?)?,
                viewport_gen: u32_field(h, "viewport_gen")?,
            },
            "painted" => ToBackend::Painted {
                seq: whole(field(h, "seq")?, "seq")?,
                waited_ms: match field(h, "waited_ms")? {
                    Json::Null => None,
                    n => Some(whole(n, "waited_ms")?),
                },
                viewport_gen: u32_field(h, "viewport_gen")?,
            },
            "helper_done" => ToBackend::HelperDone {
                id: whole(field(h, "id")?, "id")?,
                outcome: outcome_from_json(field(h, "outcome")?, body)?,
            },
            "close" => ToBackend::Close {
                why: match string(h, "why")?.as_str() {
                    "quit" => CloseWhy::Quit,
                    "hangup" => CloseWhy::Hangup,
                    "terminal" => CloseWhy::Terminal,
                    _ => return Err(bad("why")),
                },
            },
            "ping" => ToBackend::Ping,
            other => return Err(unknown(other)),
        })
    }
}

impl Wire for ToFrontend {
    fn to_header(&self) -> (Json, Option<&[u8]>) {
        let json = match self {
            ToFrontend::Welcome {
                protocol,
                version,
                pid,
                generation,
                dir,
                label,
            } => header(
                "welcome",
                vec![
                    ("protocol", Json::number(*protocol)),
                    ("version", Json::string(version)),
                    ("pid", Json::number(*pid)),
                    ("generation", Json::Number(*generation as f64)),
                    ("dir", path_json(dir)),
                    ("label", Json::string(label)),
                ],
            ),
            ToFrontend::Refused { why, retry } => header(
                "refused",
                vec![("why", Json::string(why)), ("retry", Json::Bool(*retry))],
            ),
            ToFrontend::Opened { window, resumed } => header(
                "opened",
                vec![
                    ("window", Json::Number(*window as f64)),
                    ("resumed", Json::Bool(*resumed)),
                ],
            ),
            ToFrontend::Text(bytes) => return (header("text", vec![]), Some(bytes)),
            ToFrontend::ClearPicture => header("clear_picture", vec![]),
            ToFrontend::ClearScreen => header("clear_screen", vec![]),
            ToFrontend::Frame(frame) => {
                let kind = match frame.kind {
                    FrameKind::Jpeg => "jpeg",
                    FrameKind::Png => "png",
                };
                return (
                    header(
                        "frame",
                        vec![
                            ("seq", Json::Number(frame.seq as f64)),
                            ("viewport_gen", Json::number(frame.viewport_gen)),
                            ("cells", cells_to_json(&frame.cells)),
                            ("row", Json::number(frame.row)),
                            ("kind", Json::string(kind)),
                        ],
                    ),
                    Some(&frame.image),
                );
            }
            ToFrontend::Helper { id, job } => header(
                "helper",
                vec![("id", Json::Number(*id as f64)), ("job", job_to_json(job))],
            ),
            ToFrontend::Closed { why, exit } => header(
                "closed",
                vec![("why", Json::string(why)), ("exit", Json::number(*exit))],
            ),
            ToFrontend::Pong => header("pong", vec![]),
            ToFrontend::Ready { dir, pid } => header(
                "ready",
                vec![("dir", path_json(dir)), ("pid", Json::number(*pid))],
            ),
            ToFrontend::Busy { pid } => header("busy", vec![("pid", Json::number(*pid))]),
            ToFrontend::Failed { why } => header("failed", vec![("why", Json::string(why))]),
        };
        (json, None)
    }

    fn from_wire(header: Json, body: Vec<u8>) -> Result<ToFrontend, String> {
        let t = string(&header, "t")?;
        if !matches!(t.as_str(), "text" | "frame") {
            bodiless(&t, &body)?;
        }
        let h = &header;
        Ok(match t.as_str() {
            "welcome" => ToFrontend::Welcome {
                protocol: u32_field(h, "protocol")?,
                version: string(h, "version")?,
                pid: u32_field(h, "pid")?,
                generation: whole(field(h, "generation")?, "generation")?,
                dir: path_value(field(h, "dir")?, "dir")?,
                label: string(h, "label")?,
            },
            "refused" => ToFrontend::Refused {
                why: string(h, "why")?,
                retry: boolean(h, "retry")?,
            },
            "opened" => ToFrontend::Opened {
                window: whole(field(h, "window")?, "window")?,
                resumed: boolean(h, "resumed")?,
            },
            "text" => ToFrontend::Text(body),
            "clear_picture" => ToFrontend::ClearPicture,
            "clear_screen" => ToFrontend::ClearScreen,
            "frame" => ToFrontend::Frame(Frame {
                seq: whole(field(h, "seq")?, "seq")?,
                viewport_gen: u32_field(h, "viewport_gen")?,
                cells: cells_from_json(field(h, "cells")?)?,
                row: u32_field(h, "row")?,
                kind: match string(h, "kind")?.as_str() {
                    "jpeg" => FrameKind::Jpeg,
                    "png" => FrameKind::Png,
                    _ => return Err(bad("kind")),
                },
                image: body,
            }),
            "helper" => ToFrontend::Helper {
                id: whole(field(h, "id")?, "id")?,
                job: job_from_json(field(h, "job")?)?,
            },
            "closed" => ToFrontend::Closed {
                why: string(h, "why")?,
                exit: u8::try_from(whole(field(h, "exit")?, "exit")?).map_err(|_| bad("exit"))?,
            },
            "pong" => ToFrontend::Pong,
            "ready" => ToFrontend::Ready {
                dir: path_value(field(h, "dir")?, "dir")?,
                pid: u32_field(h, "pid")?,
            },
            "busy" => ToFrontend::Busy {
                pid: u32_field(h, "pid")?,
            },
            "failed" => ToFrontend::Failed {
                why: string(h, "why")?,
            },
            other => return Err(unknown(other)),
        })
    }
}

/// A fresh nonce for [`Open::nonce`]: eight bytes of `/dev/urandom`, or,
/// where that cannot be read, the clock, the pid and a counter, which are
/// unique enough for the windows of one machine's terminals.
pub fn new_nonce() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    crate::registry::random_hex(8).unwrap_or_else(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or_default();
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mixed = nanos ^ (u64::from(std::process::id()) << 32) ^ count.rotate_left(17);
        format!("{mixed:016x}")
    })
}

/// Whether `text` is a nonce as [`Open::nonce`] has it: sixteen lowercase
/// hex digits.
pub fn is_nonce(text: &str) -> bool {
    text.len() == 16 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn job_to_json(job: &Job) -> Json {
    match job {
        Job::Picker {
            tab,
            node,
            session,
            command,
            terminal,
            dir,
            multiple,
        } => Json::object(vec![
            ("kind", Json::string("picker")),
            ("tab", Json::string(tab)),
            ("node", Json::Number(*node as f64)),
            ("session", opt_string(session.as_deref())),
            ("command", strings_json(&command.words)),
            ("terminal", Json::Bool(*terminal)),
            ("dir", path_json(dir)),
            ("multiple", Json::Bool(*multiple)),
        ]),
        Job::Login {
            tab,
            url,
            command,
            terminal,
            site,
            dir,
        } => Json::object(vec![
            ("kind", Json::string("login")),
            ("tab", Json::string(tab)),
            ("url", Json::string(url)),
            ("command", strings_json(&command.words)),
            ("terminal", Json::Bool(*terminal)),
            (
                "site",
                Json::object(vec![
                    ("host", Json::string(&site.host)),
                    ("domain", Json::string(&site.domain)),
                    ("url", Json::string(&site.url)),
                ]),
            ),
            ("dir", path_json(dir)),
        ]),
        Job::External { command, url } => Json::object(vec![
            ("kind", Json::string("external")),
            ("command", opt_command(command.as_ref())),
            ("url", Json::string(url)),
        ]),
    }
}

fn job_from_json(json: &Json) -> Result<Job, String> {
    Ok(match string(json, "kind")?.as_str() {
        "picker" => Job::Picker {
            tab: string(json, "tab")?,
            node: signed(field(json, "node")?, "node")?,
            session: opt_str(json, "session")?,
            command: command_from(json, "command")?,
            terminal: boolean(json, "terminal")?,
            dir: path_value(field(json, "dir")?, "dir")?,
            multiple: boolean(json, "multiple")?,
        },
        "login" => {
            let site = field(json, "site")?;
            Job::Login {
                tab: string(json, "tab")?,
                url: string(json, "url")?,
                command: command_from(json, "command")?,
                terminal: boolean(json, "terminal")?,
                site: login::Site {
                    host: string(site, "host")?,
                    domain: string(site, "domain")?,
                    url: string(site, "url")?,
                },
                dir: path_value(field(json, "dir")?, "dir")?,
            }
        }
        "external" => Job::External {
            command: opt_command_from(json, "command")?,
            url: string(json, "url")?,
        },
        _ => return Err(bad("kind")),
    })
}

fn outcome_to_json(outcome: &Outcome) -> Json {
    let failed = |why: &str| ("why", Json::string(why));
    match outcome {
        Outcome::Picker { tab, node, result } => {
            let mut fields = vec![
                ("kind", Json::string("picker")),
                ("tab", Json::string(tab)),
                ("node", Json::Number(*node as f64)),
            ];
            match result {
                Picked::Files(files) => {
                    fields.push(("result", Json::string("files")));
                    fields.push((
                        "files",
                        Json::Array(files.iter().map(|f| path_json(f)).collect()),
                    ));
                }
                Picked::Cancel => fields.push(("result", Json::string("cancel"))),
                Picked::Failed(why) => {
                    fields.push(("result", Json::string("failed")));
                    fields.push(failed(why));
                }
            }
            Json::object(fields)
        }
        Outcome::Login { tab, url, result } => {
            let mut fields = vec![
                ("kind", Json::string("login")),
                ("tab", Json::string(tab)),
                ("url", Json::string(url)),
            ];
            match result {
                Fetched::Found(_) => fields.push(("result", Json::string("found"))),
                Fetched::None => fields.push(("result", Json::string("none"))),
                Fetched::Failed(why) => {
                    fields.push(("result", Json::string("failed")));
                    fields.push(failed(why));
                }
            }
            Json::object(fields)
        }
        Outcome::External(result) => {
            let mut fields = vec![("kind", Json::string("external"))];
            match result {
                Ok(()) => fields.push(("result", Json::string("ok"))),
                Err(why) => {
                    fields.push(("result", Json::string("failed")));
                    fields.push(failed(why));
                }
            }
            Json::object(fields)
        }
    }
}

/// An outcome, and the message's body, which only a login that was found
/// may have.
fn outcome_from_json(json: &Json, body: Vec<u8>) -> Result<Outcome, String> {
    let kind = string(json, "kind")?;
    let result = string(json, "result")?;
    if !(kind == "login" && result == "found") {
        bodiless("helper_done", &body)?;
    }
    Ok(match (kind.as_str(), result.as_str()) {
        ("picker", result) => Outcome::Picker {
            tab: string(json, "tab")?,
            node: signed(field(json, "node")?, "node")?,
            result: match result {
                "files" => Picked::Files(
                    array(json, "files")?
                        .iter()
                        .map(|f| path_value(f, "files"))
                        .collect::<Result<_, _>>()?,
                ),
                "cancel" => Picked::Cancel,
                "failed" => Picked::Failed(string(json, "why")?),
                _ => return Err(bad("result")),
            },
        },
        ("login", result) => Outcome::Login {
            tab: string(json, "tab")?,
            url: string(json, "url")?,
            result: match result {
                "found" => Fetched::Found(Secret(body)),
                "none" => Fetched::None,
                "failed" => Fetched::Failed(string(json, "why")?),
                _ => return Err(bad("result")),
            },
        },
        ("external", "ok") => Outcome::External(Ok(())),
        ("external", "failed") => Outcome::External(Err(string(json, "why")?)),
        _ => return Err(bad("outcome")),
    })
}

// ---------------------------------------------------------------------------
// Reading fields.
// ---------------------------------------------------------------------------

/// The sentence for a field that is there and wrong.
fn bad(key: &str) -> String {
    format!("a message whose {key} makes no sense")
}

fn field<'a>(json: &'a Json, key: &str) -> Result<&'a Json, String> {
    json.get(key)
        .ok_or_else(|| format!("a message with no {key}"))
}

fn string(json: &Json, key: &str) -> Result<String, String> {
    field(json, key)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| bad(key))
}

fn boolean(json: &Json, key: &str) -> Result<bool, String> {
    field(json, key)?.as_bool().ok_or_else(|| bad(key))
}

fn array<'a>(json: &'a Json, key: &str) -> Result<&'a [Json], String> {
    field(json, key)?.as_array().ok_or_else(|| bad(key))
}

/// A string, or `null` or no field at all for none.
fn opt_str(json: &Json, key: &str) -> Result<Option<String>, String> {
    match json.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(bad(key)),
    }
}

fn strings(json: &Json, key: &str) -> Result<Vec<String>, String> {
    array(json, key)?
        .iter()
        .map(|item| item.as_str().map(str::to_string).ok_or_else(|| bad(key)))
        .collect()
}

/// A whole number from nought to the largest a JSON number holds exactly.
fn whole(json: &Json, key: &str) -> Result<u64, String> {
    match json.as_f64() {
        Some(n) if n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= MAX_EXACT => Ok(n as u64),
        _ => Err(bad(key)),
    }
}

/// A whole number that may be negative.
fn signed(json: &Json, key: &str) -> Result<i64, String> {
    match json.as_f64() {
        Some(n) if n.is_finite() && n.fract() == 0.0 && n.abs() <= MAX_EXACT => Ok(n as i64),
        _ => Err(bad(key)),
    }
}

fn u32_value(json: &Json, key: &str) -> Result<u32, String> {
    u32::try_from(whole(json, key)?).map_err(|_| bad(key))
}

fn u32_field(json: &Json, key: &str) -> Result<u32, String> {
    u32_value(field(json, key)?, key)
}

fn i32_value(json: &Json, key: &str) -> Result<i32, String> {
    i32::try_from(signed(json, key)?).map_err(|_| bad(key))
}

/// A string of exactly one character.
fn one_char(json: &Json, key: &str) -> Result<char, String> {
    let mut chars = json.as_str().ok_or_else(|| bad(key))?.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(bad(key)),
    }
}

fn opt_string(value: Option<&str>) -> Json {
    value.map_or(Json::Null, Json::string)
}

fn strings_json(items: &[String]) -> Json {
    Json::Array(items.iter().map(Json::string).collect())
}

fn opt_command(command: Option<&Command>) -> Json {
    command.map_or(Json::Null, |c| strings_json(&c.words))
}

fn command_from(json: &Json, key: &str) -> Result<Command, String> {
    let words = strings(json, key)?;
    if words.is_empty() {
        return Err(bad(key));
    }
    Ok(Command { words })
}

fn opt_command_from(json: &Json, key: &str) -> Result<Option<Command>, String> {
    match json.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(_) => command_from(json, key).map(Some),
    }
}

/// A path as a string, or — the rare one that is not UTF-8, which a JSON
/// string cannot hold — as an array of its bytes, so that it still names the
/// same file at the other end.
fn path_json(path: &Path) -> Json {
    match path.to_str() {
        Some(text) => Json::string(text),
        None => Json::Array(
            path.as_os_str()
                .as_bytes()
                .iter()
                .map(|&b| Json::number(b))
                .collect(),
        ),
    }
}

fn path_value(json: &Json, key: &str) -> Result<PathBuf, String> {
    match json {
        Json::String(text) => Ok(PathBuf::from(text)),
        Json::Array(bytes) => {
            let bytes = bytes
                .iter()
                .map(|b| u8::try_from(whole(b, key)?).map_err(|_| bad(key)))
                .collect::<Result<Vec<u8>, String>>()?;
            Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
        }
        _ => Err(bad(key)),
    }
}

fn opt_path(json: &Json, key: &str) -> Result<Option<PathBuf>, String> {
    match json.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(value) => path_value(value, key).map(Some),
    }
}

// ---------------------------------------------------------------------------
// The endpoint.
// ---------------------------------------------------------------------------

/// A backend's socket, bound under the profile lock and gone again when this
/// is dropped.
#[derive(Debug)]
pub struct Endpoint {
    bound: crate::remote::Bound,
}

impl Endpoint {
    /// The descriptor to wait on: readable when a frontend has connected.
    pub fn fd(&self) -> RawFd {
        self.bound.socket.as_raw_fd()
    }

    /// Where a frontend looks for this socket.
    pub fn path(&self) -> &Path {
        self.bound.path()
    }
}

/// Listen on the profile at `profile`, which the caller holds the lock on:
/// [`SOCKET_FILE`], 0600, or the private fallback behind a link when the
/// path cannot hold a socket. Whatever is there is removed first.
pub fn bind(profile: &Path) -> Result<Endpoint, String> {
    Ok(Endpoint {
        bound: crate::remote::bind_private(profile, SOCKET_FILE, FALLBACK_PREFIX)?,
    })
}

/// The next frontend that has connected, if one has, as a non-blocking
/// stream; `None` when nobody is waiting.
///
/// A peer that is not this process's effective user — or one whose user
/// cannot be asked — is hung up on and the next one looked at. An error is
/// one that trying again will not mend, as for [`crate::remote::Listener`].
pub fn accept(endpoint: &Endpoint) -> Result<Option<UnixStream>, String> {
    // SAFETY: `geteuid(2)` takes nothing, reads no memory and cannot fail.
    let ours = unsafe { libc::geteuid() };
    loop {
        match endpoint.bound.socket.accept() {
            Ok((stream, _)) => {
                if peer_uid(&stream).ok() != Some(ours) {
                    continue;
                }
                // On a Mac an accepted socket inherits the listener's
                // O_NONBLOCK and on Linux it does not; this makes it the same
                // on both.
                stream
                    .set_nonblocking(true)
                    .map_err(|e| format!("{}: {e}", endpoint.path().display()))?;
                return Ok(Some(stream));
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(None),
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::Interrupted | ErrorKind::ConnectionAborted
                ) => {}
            Err(e) => return Err(format!("{}: {e}", endpoint.path().display())),
        }
    }
}

/// The user on the other end of `stream`, as the kernel recorded it at
/// `connect(2)`.
#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> std::io::Result<libc::uid_t> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let size = size_of::<libc::ucred>();
    let mut len = size as libc::socklen_t;
    // SAFETY: `SO_PEERCRED` writes at most `len` bytes, which is the size of
    // `cred`, a live local, through the pointer, and its new length through
    // `len`, another; the descriptor is `stream`'s, open for the whole call.
    let r = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if len as usize != size {
        return Err(std::io::Error::other("a short SO_PEERCRED"));
    }
    Ok(cred.uid)
}

/// The user on the other end of `stream`, as the kernel recorded it at
/// `connect(2)`.
#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> std::io::Result<libc::uid_t> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: `getpeereid(3)` writes one `uid_t` and one `gid_t` through the
    // two pointers, both to live locals of those types; the descriptor is
    // `stream`'s, open for the whole call.
    let r = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(uid)
}

/// Elsewhere there is no asking, and a peer nobody can vouch for is refused.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_uid(_stream: &UnixStream) -> std::io::Result<libc::uid_t> {
    Err(std::io::Error::new(
        ErrorKind::Unsupported,
        "no way to ask a socket's peer who it is here",
    ))
}

/// What [`connect`] found.
#[derive(Debug)]
pub enum Connect {
    /// A backend took the connection. The stream is blocking.
    Stream(UnixStream),
    /// No backend is listening on the profile: no file, a file nobody is
    /// on (what a crash leaves), or a path too long with no link to a
    /// shorter one.
    NobodyThere,
}

/// Connect to the backend on the profile at `profile`, through the link to
/// the fallback when there is one, as [`crate::remote::deliver`] does.
pub fn connect(profile: &Path) -> Result<Connect, String> {
    let link = socket_path(profile);
    let target = match std::fs::read_link(&link) {
        Ok(target) => profile.join(target),
        Err(_) => link,
    };
    match UnixStream::connect(&target) {
        Ok(stream) => Ok(Connect::Stream(stream)),
        Err(e)
            if matches!(
                e.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::InvalidInput
            ) =>
        {
            Ok(Connect::NobodyThere)
        }
        Err(e) => Err(format!(
            "cannot reach the blinkterm serving {}: {e}",
            profile.display()
        )),
    }
}

// ---------------------------------------------------------------------------
// The outbox.
// ---------------------------------------------------------------------------

/// One connection's messages on their way out, written as far as a
/// non-blocking socket takes them and never further. See the module's
/// section on it.
#[derive(Debug)]
pub struct Outbox {
    /// In the order handed over. At most one frame that has not started.
    queue: VecDeque<Item>,
    /// How much of the front item has been written.
    written: usize,
    /// Every byte in `queue`, the front item's written ones included.
    bytes: usize,
    cap: usize,
}

/// One message, cleared when it is dropped: written, replaced, or left in an
/// outbox that went.
#[derive(Debug)]
struct Item {
    bytes: Vec<u8>,
    frame: bool,
}

impl Drop for Item {
    fn drop(&mut self) {
        picker::scrub(&mut self.bytes);
    }
}

impl Default for Outbox {
    fn default() -> Outbox {
        Outbox::with_cap(MAX_OUTBOX)
    }
}

impl Outbox {
    /// An outbox that holds up to [`MAX_OUTBOX`].
    pub fn new() -> Outbox {
        Outbox::default()
    }

    /// An outbox that holds up to `cap` bytes.
    pub fn with_cap(cap: usize) -> Outbox {
        Outbox {
            queue: VecDeque::new(),
            written: 0,
            bytes: 0,
            cap,
        }
    }

    /// A framed message, after everything handed over before it; never
    /// dropped. An error is a peer that has stopped reading: the cap is
    /// reached, and the message was not queued.
    pub fn push(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        self.enqueue(Item {
            bytes,
            frame: false,
        })
    }

    /// A framed [`ToFrontend::Frame`]: after everything handed over before
    /// it, and in place of the frame in the queue if there is one that has
    /// not started going out — which is dropped unwritten. True when one
    /// was. An error is as for [`Outbox::push`].
    pub fn push_frame(&mut self, bytes: Vec<u8>) -> Result<bool, String> {
        let started = usize::from(self.written > 0);
        let waiting = self
            .queue
            .iter()
            .skip(started)
            .position(|item| item.frame)
            .map(|at| at + started);
        let replaced = match waiting {
            Some(at) => {
                if let Some(old) = self.queue.remove(at) {
                    self.bytes -= old.bytes.len();
                }
                true
            }
            None => false,
        };
        self.enqueue(Item { bytes, frame: true })?;
        Ok(replaced)
    }

    /// [`encode`] `message` and [`Outbox::push`] it.
    pub fn send<M: Wire>(&mut self, message: &M) -> Result<(), String> {
        self.push(encode(message))
    }

    fn enqueue(&mut self, item: Item) -> Result<(), String> {
        if self.bytes + item.bytes.len() > self.cap {
            return Err(format!(
                "not reading what it is sent: more than {} MiB waiting",
                self.cap >> 20
            ));
        }
        self.bytes += item.bytes.len();
        self.queue.push_back(item);
        Ok(())
    }

    /// Write as much as `out` takes without blocking. True when everything
    /// is written; false when `out` would block, and then the caller waits
    /// for it to be writable (`POLLOUT`) and calls again. An error is a
    /// peer that has gone.
    pub fn flush(&mut self, out: &mut impl Write) -> Result<bool, String> {
        while let Some(front) = self.queue.front() {
            match out.write(&front.bytes[self.written..]) {
                Ok(0) => return Err("the connection is closed".to_string()),
                Ok(n) => {
                    self.written += n;
                    if self.written == front.bytes.len() {
                        self.bytes -= front.bytes.len();
                        self.written = 0;
                        self.queue.pop_front();
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(false),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(true)
    }

    /// Whether there is anything left to write.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// How many bytes are waiting, the front message's written ones
    /// included.
    pub fn queued(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::Action;

    /// A scratch directory of this test's own, gone again before and after.
    fn scratch(what: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blinkterm-ipc-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    /// Wait until `fd` is readable, for up to two seconds.
    fn readable(fd: RawFd) -> bool {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `poll` is one valid `pollfd` on this stack frame, the count
        // says one, and `poll(2)` writes only its `revents`.
        let n = unsafe { libc::poll(&mut poll, 1, 2000) };
        n == 1 && poll.revents & libc::POLLIN != 0
    }

    fn options(args: &[&str]) -> Options {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let cli = crate::options::parse_args(&args).expect("arguments");
        crate::options::resolve(cli, Default::default(), Default::default()).expect("options")
    }

    fn window() -> WindowSettings {
        let mut window = WindowSettings::from(&options(&[
            "--scale",
            "1.5",
            "--color-scheme",
            "dark",
            "--force-dark",
            "--alpha",
            "40",
            "--normal-mode",
            "--search-url",
            "https://search.example/?q=%s",
            "--pdf-paper",
            "letter",
            "--keymap",
            "mac",
            "--file-picker",
            "zenity --file-selection",
            "--password-command-terminal",
            "pass show {host}",
            "--external-browser",
            "firefox --new-tab",
        ]));
        window.home = "https://home.example/".to_string();
        window.bindings = Bindings::on(
            window.keymap,
            vec![
                Binding::parse("ctrl+b", "back").expect("a row"),
                Binding::parse("alt+w", "none").expect("a row"),
                Binding::parse("ctrl+plus", "zoom-in").expect("a row"),
                Binding::parse("f5", "tab-3").expect("a row"),
            ],
        );
        window
    }

    fn browser() -> BrowserSettings {
        BrowserSettings::from(&options(&[
            "--engine",
            "/opt/engine/chrome",
            "--engine-arg",
            "--lang=de",
            "--engine-arg",
            "--disable-gpu",
            "--user-agent",
            "agent/1",
            "--proxy",
            "socks5://127.0.0.1:9050",
            "--mute",
            "--download-dir",
            "/tmp/down",
            "--block-list",
            "/etc/hosts.block",
            "--no-sites",
            "--no-console",
        ]))
    }

    fn open() -> Open {
        Open {
            nonce: "0123456789abcdef".to_string(),
            window: window(),
            browser: browser(),
            metrics: Metrics {
                cols: 120,
                rows: 40,
                cell: (9, 18),
            },
            route: RouteFlags {
                paced: true,
                png: false,
                keyed: true,
                alpha: Some(40),
                every_nth: 2,
            },
            pixel_mouse: true,
            urls: vec!["example.com".to_string(), "über.example/ü".to_string()],
            restore: true,
            problems: vec!["no clipboard".to_string()],
            cwd: PathBuf::from("/home/someone/work"),
            home_dir: Some(PathBuf::from(std::ffi::OsString::from_vec(
                b"/home/\xffodd".to_vec(),
            ))),
            display: true,
        }
    }

    fn inputs() -> Vec<Input> {
        let mut key = KeyInput::press(Key::Char('é'));
        key.mods = Mods(Mods::CTRL | Mods::SHIFT);
        key.text = Some('É');
        let mut release = KeyInput::press(Key::Function(12));
        release.action = KeyAction::Release;
        let mut repeat = KeyInput::press(Key::Other(57441));
        repeat.action = KeyAction::Repeat;
        vec![
            Input::Key(key),
            Input::Key(release),
            Input::Key(repeat),
            Input::Key(KeyInput::press(Key::PageDown)),
            Input::Key(KeyInput::press(Key::Escape)),
            Input::Mouse(MouseInput {
                kind: MouseKind::Wheel,
                button: None,
                mods: Mods(Mods::ALT),
                x: 640,
                y: 1,
                wheel: (-3, 2),
            }),
            Input::Mouse(MouseInput {
                kind: MouseKind::Press,
                button: Some(2),
                mods: Mods::default(),
                x: 1,
                y: 40,
                wheel: (0, 0),
            }),
            Input::Mode {
                mode: 2026,
                state: 2,
            },
            Input::Paste("line one\n\x1b[201~\"quoted\" \u{202e}".to_string()),
            Input::PasteRefused { bytes: 1 << 20 },
            Input::PasteCut,
            Input::Colour {
                slot: 11,
                rgb: (0x12, 0xab, 0xff),
            },
            Input::CellSize {
                width: 10,
                height: 21,
            },
        ]
    }

    fn to_backend() -> Vec<ToBackend> {
        let mut all = vec![
            ToBackend::Hello {
                protocol: PROTOCOL,
                version: VERSION.to_string(),
                dir: Some(PathBuf::from(
                    "/home/someone/.local/share/blinkterm/profiles/x",
                )),
            },
            ToBackend::Hello {
                protocol: PROTOCOL,
                version: VERSION.to_string(),
                dir: None,
            },
            ToBackend::Open(Box::new(open())),
            ToBackend::Resize {
                metrics: Metrics {
                    cols: 80,
                    rows: 24,
                    cell: (8, 16),
                },
                viewport_gen: 7,
            },
            ToBackend::Painted {
                seq: 1 << 40,
                waited_ms: Some(17),
                viewport_gen: 7,
            },
            ToBackend::Painted {
                seq: 0,
                waited_ms: None,
                viewport_gen: 0,
            },
            ToBackend::HelperDone {
                id: 3,
                outcome: Outcome::Picker {
                    tab: "T1".to_string(),
                    node: 42,
                    result: Picked::Files(vec![PathBuf::from("/a b"), PathBuf::from("/c")]),
                },
            },
            ToBackend::HelperDone {
                id: 4,
                outcome: Outcome::Picker {
                    tab: "T1".to_string(),
                    node: -1,
                    result: Picked::Cancel,
                },
            },
            ToBackend::HelperDone {
                id: 5,
                outcome: Outcome::Picker {
                    tab: "T1".to_string(),
                    node: 9,
                    result: Picked::Failed("no zenity".to_string()),
                },
            },
            ToBackend::HelperDone {
                id: 6,
                outcome: Outcome::Login {
                    tab: "T2".to_string(),
                    url: "https://a.example/login".to_string(),
                    result: Fetched::Found(Secret::new(b"hunter2\nuser: me\n\xff".to_vec())),
                },
            },
            ToBackend::HelperDone {
                id: 7,
                outcome: Outcome::Login {
                    tab: "T2".to_string(),
                    url: "https://a.example/login".to_string(),
                    result: Fetched::None,
                },
            },
            ToBackend::HelperDone {
                id: 8,
                outcome: Outcome::Login {
                    tab: "T2".to_string(),
                    url: "https://a.example/login".to_string(),
                    result: Fetched::Failed("exit 1".to_string()),
                },
            },
            ToBackend::HelperDone {
                id: 9,
                outcome: Outcome::External(Ok(())),
            },
            ToBackend::HelperDone {
                id: 10,
                outcome: Outcome::External(Err("no browser".to_string())),
            },
            ToBackend::Close {
                why: CloseWhy::Quit,
            },
            ToBackend::Close {
                why: CloseWhy::Hangup,
            },
            ToBackend::Close {
                why: CloseWhy::Terminal,
            },
            ToBackend::Ping,
        ];
        for (n, input) in inputs().into_iter().enumerate() {
            all.push(ToBackend::Input {
                input,
                pixel_mouse: n % 2 == 0,
            });
        }
        all
    }

    fn to_frontend() -> Vec<ToFrontend> {
        vec![
            ToFrontend::Welcome {
                protocol: PROTOCOL,
                version: VERSION.to_string(),
                pid: 4242,
                generation: 3,
                dir: PathBuf::from("/p"),
                label: "work".to_string(),
            },
            ToFrontend::Refused {
                why: "shutting down".to_string(),
                retry: true,
            },
            ToFrontend::Opened {
                window: 2,
                resumed: true,
            },
            ToFrontend::Text(b"\x1b[1;1Hrow\x1b]52;c;eA==\x07".to_vec()),
            ToFrontend::Text(Vec::new()),
            ToFrontend::ClearPicture,
            ToFrontend::ClearScreen,
            ToFrontend::Frame(Frame {
                seq: 99,
                viewport_gen: 4,
                cells: Cells { cols: 80, rows: 23 },
                row: 1,
                kind: FrameKind::Jpeg,
                image: (0..=255).cycle().take(5000).collect(),
            }),
            ToFrontend::Frame(Frame {
                seq: 100,
                viewport_gen: 4,
                cells: Cells { cols: 80, rows: 23 },
                row: 1,
                kind: FrameKind::Png,
                image: b"\x89PNG".to_vec(),
            }),
            ToFrontend::Helper {
                id: 1,
                job: Job::Picker {
                    tab: "T1".to_string(),
                    node: 12,
                    session: Some("S1".to_string()),
                    command: Command {
                        words: vec!["zenity".to_string(), "--file-selection".to_string()],
                    },
                    terminal: false,
                    dir: PathBuf::from("/home/someone"),
                    multiple: true,
                },
            },
            ToFrontend::Helper {
                id: 2,
                job: Job::Login {
                    tab: "T1".to_string(),
                    url: "https://a.example/".to_string(),
                    command: Command {
                        words: vec!["pass".to_string(), "{host}".to_string()],
                    },
                    terminal: true,
                    site: login::Site {
                        host: "a.example".to_string(),
                        domain: "a.example".to_string(),
                        url: "https://a.example/".to_string(),
                    },
                    dir: PathBuf::from("/"),
                },
            },
            ToFrontend::Helper {
                id: 3,
                job: Job::External {
                    command: None,
                    url: "https://a.example/".to_string(),
                },
            },
            ToFrontend::Closed {
                why: "quit".to_string(),
                exit: 0,
            },
            ToFrontend::Pong,
            ToFrontend::Ready {
                dir: PathBuf::from("/p"),
                pid: 1,
            },
            ToFrontend::Busy { pid: 77 },
            ToFrontend::Failed {
                why: "no engine".to_string(),
            },
        ]
    }

    /// `messages` through [`encode`] and back, fed in pieces of the sizes
    /// `chunks` cycles through.
    fn through<M: Wire + std::fmt::Debug>(messages: &[M], chunks: &[usize]) -> Vec<M> {
        let bytes: Vec<u8> = messages.iter().flat_map(encode).collect();
        let mut decoder = Decoder::new();
        let mut out = Vec::new();
        let mut at = 0;
        for &size in chunks.iter().cycle() {
            if at >= bytes.len() {
                break;
            }
            let end = (at + size).min(bytes.len());
            out.extend(decoder.feed::<M>(&bytes[at..end]).expect("decoded"));
            at = end;
        }
        assert_eq!(decoder.buffered(), 0);
        out
    }

    #[test]
    fn every_message_comes_back_as_it_was_sent() {
        let sent = to_backend();
        for message in &sent {
            let mut decoder = Decoder::new();
            let back: Vec<ToBackend> = decoder.feed(&encode(message)).expect("decoded");
            assert_eq!(back.len(), 1, "{message:?}");
            assert_eq!(&back[0], message);
        }
        let sent = to_frontend();
        for message in &sent {
            let mut decoder = Decoder::new();
            let back: Vec<ToFrontend> = decoder.feed(&encode(message)).expect("decoded");
            assert_eq!(back.len(), 1, "{message:?}");
            assert_eq!(&back[0], message);
        }
    }

    #[test]
    fn a_message_split_anywhere_decodes_to_one() {
        let message = ToBackend::Open(Box::new(open()));
        let bytes = encode(&message);
        for cut in 0..bytes.len() {
            let mut decoder = Decoder::new();
            let first: Vec<ToBackend> = decoder.feed(&bytes[..cut]).expect("the first part");
            assert!(first.is_empty(), "complete at {cut}");
            let second: Vec<ToBackend> = decoder.feed(&bytes[cut..]).expect("the rest");
            assert_eq!(
                second,
                vec![ToBackend::Open(Box::new(open()))],
                "cut at {cut}"
            );
        }
        assert_eq!(through(&to_backend(), &[1]), to_backend());
        assert_eq!(through(&to_frontend(), &[1]), to_frontend());
        assert_eq!(through(&to_frontend(), &[7, 3, 1000, 2]), to_frontend());
        assert_eq!(through(&to_backend(), &[13, 4096, 5]), to_backend());
    }

    #[test]
    fn two_messages_in_one_read_decode_to_two() {
        let mut bytes = encode(&ToFrontend::Pong);
        bytes.extend(encode(&ToFrontend::Text(b"hi".to_vec())));
        let mut decoder = Decoder::new();
        let got: Vec<ToFrontend> = decoder.feed(&bytes).expect("decoded");
        assert_eq!(
            got,
            vec![ToFrontend::Pong, ToFrontend::Text(b"hi".to_vec())]
        );
        // And a whole one with the start of the next.
        let mut bytes = encode(&ToFrontend::ClearScreen);
        let next = encode(&ToFrontend::ClearPicture);
        bytes.extend_from_slice(&next[..5]);
        let got: Vec<ToFrontend> = decoder.feed(&bytes).expect("decoded");
        assert_eq!(got, vec![ToFrontend::ClearScreen]);
        assert_eq!(decoder.buffered(), 5);
        let got: Vec<ToFrontend> = decoder.feed(&next[5..]).expect("decoded");
        assert_eq!(got, vec![ToFrontend::ClearPicture]);
    }

    /// The first eight bytes of a frame.
    fn prefix(total: usize, header: usize) -> Vec<u8> {
        let mut bytes = (total as u32).to_be_bytes().to_vec();
        bytes.extend((header as u32).to_be_bytes());
        bytes
    }

    #[test]
    fn a_length_over_a_limit_is_an_error_and_not_an_allocation() {
        let mut decoder = Decoder::new();
        let why = decoder
            .feed::<ToFrontend>(&prefix(4 + MAX_HEADER + 1, MAX_HEADER + 1))
            .unwrap_err();
        assert!(why.contains("header"), "{why}");
        assert!(decoder.buffer.capacity() < 1024, "it made room for it");

        let mut decoder = Decoder::new();
        let why = decoder
            .feed::<ToFrontend>(&prefix(4 + 10 + MAX_BODY + 1, 10))
            .unwrap_err();
        assert!(why.contains("body"), "{why}");
        assert!(decoder.buffer.capacity() < 1024, "it made room for it");

        let mut decoder = Decoder::new();
        let why = decoder.feed::<ToFrontend>(&prefix(5, 10)).unwrap_err();
        assert!(why.contains("header of 10"), "{why}");

        // At the limits it waits for the rest.
        let mut decoder = Decoder::new();
        let got = decoder
            .feed::<ToFrontend>(&prefix(4 + 10 + MAX_BODY, 10))
            .expect("in bounds");
        assert!(got.is_empty());
    }

    /// A frame of `header` and `body`, as a peer might send it.
    fn raw(header: &str, body: &[u8]) -> Vec<u8> {
        let mut bytes = prefix(4 + header.len() + body.len(), header.len());
        bytes.extend_from_slice(header.as_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn what_is_not_a_message_is_an_error() {
        let feed = |bytes: Vec<u8>| Decoder::new().feed::<ToBackend>(&bytes).unwrap_err();
        assert!(feed(raw(r#"{"t":"launch"}"#, b"")).contains("unknown message \"launch\""));
        assert!(feed(raw(r#"{"x":1}"#, b"")).contains("\"t\""));
        assert!(feed(raw("[1]", b"")).contains("\"t\""));
        assert!(feed(raw("{", b"")).contains("not JSON"));
        assert!(feed(raw("\u{0}\u{ff}", b"")).contains("JSON"));
        assert!(feed(raw(r#"{"t":"ping"}"#, b"x")).contains("no body"));
        assert!(feed(raw(r#"{"t":"resize"}"#, b"")).contains("metrics"));
        let outcome = r#"{"t":"helper_done","id":1,"outcome":{"kind":"external","result":"ok"}}"#;
        assert!(feed(raw(outcome, b"secret")).contains("no body"));
        assert!(Decoder::new()
            .feed::<ToFrontend>(&raw(r#"{"t":"pong"}"#, b"x"))
            .unwrap_err()
            .contains("no body"));
        // A frontend's message is not a backend's, and the other way round.
        assert!(Decoder::new()
            .feed::<ToFrontend>(&encode(&ToBackend::Ping))
            .unwrap_err()
            .contains("unknown message"));
        // Numbers that are not what they say.
        let painted = |seq: &str| {
            raw(
                &format!(r#"{{"t":"painted","seq":{seq},"waited_ms":null,"viewport_gen":0}}"#),
                b"",
            )
        };
        assert!(Decoder::new().feed::<ToBackend>(&painted("1")).is_ok());
        for seq in ["-1", "1.5", "1e300", "\"1\"", "null"] {
            assert!(feed(painted(seq)).contains("seq"), "{seq}");
        }
        let open = encode(&ToBackend::Open(Box::new(Open {
            nonce: "XYZ".to_string(),
            ..open()
        })));
        assert!(feed(open).contains("nonce"));
    }

    #[test]
    fn open_carries_the_terminals_display() {
        // The terminal's answer, not the backend's: it goes in `open` both
        // ways, and an `open` that does not say is refused rather than
        // guessed at (#104).
        for display in [true, false] {
            let sent = ToBackend::Open(Box::new(Open { display, ..open() }));
            let framed = encode(&sent);
            let header = String::from_utf8_lossy(&framed[8..]).to_string();
            assert!(
                header.contains(&format!(r#""display":{display}"#)),
                "{header}"
            );
            let back: Vec<ToBackend> = Decoder::new().feed(&framed).expect("decoded");
            assert_eq!(back, vec![sent]);

            let without = header.replace(&format!(r#","display":{display}"#), "");
            assert_ne!(without, header);
            let refused = Decoder::new()
                .feed::<ToBackend>(&raw(&without, b""))
                .unwrap_err();
            assert!(refused.contains("display"), "{refused}");
        }
    }

    #[test]
    fn a_secret_is_not_printed() {
        let outcome = Outcome::Login {
            tab: "T".to_string(),
            url: "u".to_string(),
            result: Fetched::Found(Secret::new(b"hunter2".to_vec())),
        };
        let shown = format!("{outcome:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        let framed = encode(&ToBackend::HelperDone { id: 1, outcome });
        assert!(framed.ends_with(b"hunter2"), "the body is the secret");
        let header = String::from_utf8_lossy(&framed[8..framed.len() - 7]).to_string();
        assert!(!header.contains("hunter2"), "{header}");
    }

    #[test]
    fn settings_come_from_the_options_and_back_from_json() {
        let options = options(&["--proxy", "http://p:1", "--scale", "2", "--no-block"]);
        let window = WindowSettings::from(&options);
        assert_eq!(window.scale, Scale::Fixed(2.0));
        assert_eq!(window.bindings, options.bindings);
        let browser = BrowserSettings::from(&options);
        assert_eq!(browser.engine.proxy.as_deref(), Some("http://p:1"));
        assert!(!browser.block.enabled);

        let window = self::window();
        assert_eq!(
            WindowSettings::from_json(&window.to_json()),
            Ok(window.clone())
        );
        let rows: Vec<_> = window.bindings.iter().cloned().collect();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[2].action, Some(Action::ZoomIn));
        let browser = self::browser();
        assert_eq!(
            BrowserSettings::from_json(&browser.to_json()),
            Ok(browser.clone())
        );
        let plain = WindowSettings::from(&self::options(&[]));
        assert_eq!(WindowSettings::from_json(&plain.to_json()), Ok(plain));
    }

    #[test]
    fn browser_settings_name_every_key_that_differs() {
        let ours = BrowserSettings::from(&options(&[]));
        assert!(ours.differences(&ours.clone()).is_empty());
        let theirs = browser();
        let keys: Vec<&str> = ours.differences(&theirs).iter().map(|d| d.0).collect();
        assert_eq!(
            keys,
            [
                "engine",
                "engine-arg",
                "user-agent",
                "proxy",
                "mute",
                "download-dir",
                "block-list",
                "sites",
                "console"
            ]
        );
        let proxy = BrowserSettings::from(&options(&["--proxy", "socks5://h:1"]));
        assert_eq!(
            ours.differences(&proxy),
            vec![("proxy", "none".to_string(), "socks5://h:1".to_string())]
        );
        let unblocked = BrowserSettings::from(&options(&["--no-block"]));
        assert_eq!(
            unblocked.differences(&ours),
            vec![("block", "false".to_string(), "true".to_string())]
        );
    }

    #[test]
    fn bind_makes_a_private_socket_in_the_profile() {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        let dir = scratch("private");
        let endpoint = bind(&dir).expect("bound");
        assert_eq!(endpoint.path(), socket_path(&dir));
        let meta = std::fs::symlink_metadata(endpoint.path()).expect("there");
        assert!(meta.file_type().is_socket());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        // SAFETY: `F_GETFD` reads a descriptor's flags and no memory; the
        // descriptor is the endpoint's, open for the whole call.
        let flags = unsafe { libc::fcntl(endpoint.fd(), libc::F_GETFD) };
        assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
        // Beside the --remote socket, each its own.
        let remote = crate::remote::Listener::bind(&dir).expect("the remote socket");
        drop(endpoint);
        assert!(std::fs::symlink_metadata(socket_path(&dir)).is_err());
        assert!(remote.path().exists(), "the remote socket went with it");
        drop(remote);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_profile_too_long_for_a_socket_gets_a_private_one_behind_a_link() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("long");
        let dir = root.join("p".repeat(120));
        std::fs::create_dir_all(&dir).expect("a deep profile");
        let endpoint = bind(&dir).expect("bound somewhere");
        let remote = crate::remote::Listener::bind(&dir).expect("the remote socket too");
        let target = std::fs::read_link(socket_path(&dir)).expect("a link");
        let fallback = target.parent().expect("a directory").to_path_buf();
        assert!(fallback
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(FALLBACK_PREFIX));
        let mode = std::fs::metadata(&fallback).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let Connect::Stream(mut stream) = connect(&dir).expect("connected") else {
            panic!("nobody there");
        };
        assert!(readable(endpoint.fd()));
        let mut accepted = accept(&endpoint).expect("accepted").expect("a peer");
        stream
            .write_all(&encode(&ToBackend::Ping))
            .expect("written");
        assert!(readable(accepted.as_raw_fd()));
        let mut chunk = [0u8; 64];
        let n = std::io::Read::read(&mut accepted, &mut chunk).expect("read");
        let got: Vec<ToBackend> = Decoder::new().feed(&chunk[..n]).expect("decoded");
        assert_eq!(got, vec![ToBackend::Ping]);

        // What a crash leaves — a link into a fallback of ours — is cleared
        // by the next bind, and the --remote socket's fallback is not.
        drop((stream, accepted, endpoint));
        assert!(!fallback.exists(), "the fallback outlived its endpoint");
        let left = crate::profile::make_temp_dir(FALLBACK_PREFIX).expect("a fallback dir");
        std::os::unix::fs::symlink(left.join("sock"), socket_path(&dir)).expect("a link");
        let remote_target = std::fs::read_link(remote.path()).expect("a link");
        let endpoint = bind(&dir).expect("bound again");
        assert!(!left.exists(), "the crash's fallback is still there");
        assert!(remote_target.exists(), "the remote socket's fallback went");
        drop(endpoint);
        drop(remote);
        assert!(std::fs::symlink_metadata(socket_path(&dir)).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn accept_admits_a_peer_of_the_same_user() {
        let dir = scratch("accept");
        let endpoint = bind(&dir).expect("bound");
        assert!(accept(&endpoint).expect("nobody").is_none());
        let Connect::Stream(stream) = connect(&dir).expect("connected") else {
            panic!("nobody there");
        };
        assert!(readable(endpoint.fd()));
        let accepted = accept(&endpoint).expect("accepted").expect("a peer");
        // SAFETY: `geteuid(2)` takes nothing, reads no memory and cannot fail.
        assert_eq!(peer_uid(&accepted).expect("asked"), unsafe {
            libc::geteuid()
        });
        // SAFETY: `F_GETFL` reads a descriptor's flags and no memory; the
        // descriptor is the accepted stream's, open for the whole call.
        let flags = unsafe { libc::fcntl(accepted.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0 && flags & libc::O_NONBLOCK != 0, "it blocks");
        assert!(accept(&endpoint).expect("nobody else").is_none());
        drop((stream, accepted, endpoint));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn connect_finds_nobody_at_a_stale_socket_or_none() {
        let dir = scratch("stale");
        assert!(matches!(connect(&dir), Ok(Connect::NobodyThere)));
        // The standard library's listener leaves its file behind, as a
        // crash does.
        drop(std::os::unix::net::UnixListener::bind(socket_path(&dir)).expect("bound"));
        assert!(socket_path(&dir).exists());
        assert!(matches!(connect(&dir), Ok(Connect::NobodyThere)));
        // And binding over it works.
        let endpoint = bind(&dir).expect("bound over a stale socket");
        assert!(matches!(connect(&dir), Ok(Connect::Stream(_))));
        drop(endpoint);
        assert!(matches!(connect(&dir), Ok(Connect::NobodyThere)));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A writer that takes `room` bytes at a time and then would block until
    /// given more.
    struct Slow {
        got: Vec<u8>,
        room: usize,
    }

    impl Write for Slow {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.room == 0 {
                return Err(ErrorKind::WouldBlock.into());
            }
            let n = bytes.len().min(self.room);
            self.room -= n;
            self.got.extend_from_slice(&bytes[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_outbox_writes_what_the_socket_takes_and_keeps_the_rest_in_order() {
        let mut outbox = Outbox::new();
        let mut out = Slow {
            got: Vec::new(),
            room: 3,
        };
        outbox.send(&ToFrontend::Pong).unwrap();
        outbox.send(&ToFrontend::Text(b"abc".to_vec())).unwrap();
        let total = outbox.queued();
        assert_eq!(outbox.flush(&mut out), Ok(false));
        assert_eq!(
            outbox.queued(),
            total,
            "the front's written bytes still count"
        );
        out.room = 1000;
        assert_eq!(outbox.flush(&mut out), Ok(true));
        assert!(outbox.is_empty() && outbox.queued() == 0);
        let got: Vec<ToFrontend> = Decoder::new().feed(&out.got).unwrap();
        assert_eq!(
            got,
            vec![ToFrontend::Pong, ToFrontend::Text(b"abc".to_vec())]
        );
    }

    fn frame(seq: u64) -> Vec<u8> {
        encode(&ToFrontend::Frame(Frame {
            seq,
            viewport_gen: 0,
            cells: Cells { cols: 1, rows: 1 },
            row: 1,
            kind: FrameKind::Png,
            image: vec![seq as u8; 100],
        }))
    }

    fn seqs(bytes: &[u8]) -> Vec<String> {
        Decoder::new()
            .feed::<ToFrontend>(bytes)
            .unwrap()
            .into_iter()
            .map(|m| match m {
                ToFrontend::Frame(f) => format!("frame {}", f.seq),
                ToFrontend::Text(t) => String::from_utf8(t).unwrap(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_newer_frame_replaces_one_not_yet_started_and_never_one_half_written() {
        let mut outbox = Outbox::new();
        assert_eq!(outbox.push_frame(frame(1)), Ok(false));
        outbox
            .send(&ToFrontend::Text(b"after one".to_vec()))
            .unwrap();
        assert_eq!(outbox.push_frame(frame(2)), Ok(true));
        let mut out = Slow {
            got: Vec::new(),
            room: 10_000,
        };
        assert_eq!(outbox.flush(&mut out), Ok(true));
        assert_eq!(seqs(&out.got), ["after one", "frame 2"]);

        // A frame that has started going out stays whole; the next waits
        // behind it, and the one after that replaces the waiting one.
        let mut out = Slow {
            got: Vec::new(),
            room: 10,
        };
        outbox.push_frame(frame(3)).unwrap();
        assert_eq!(outbox.flush(&mut out), Ok(false));
        assert_eq!(outbox.push_frame(frame(4)), Ok(false));
        assert_eq!(outbox.push_frame(frame(5)), Ok(true));
        out.room = 10_000;
        assert_eq!(outbox.flush(&mut out), Ok(true));
        assert_eq!(seqs(&out.got), ["frame 3", "frame 5"]);
    }

    #[test]
    fn an_outbox_past_its_cap_gives_up_on_the_peer() {
        let mut outbox = Outbox::with_cap(1 << 20);
        let text = ToFrontend::Text(vec![b'x'; 400 << 10]);
        outbox.send(&text).unwrap();
        outbox.send(&text).unwrap();
        let why = outbox.send(&text).unwrap_err();
        assert!(why.contains("not reading"), "{why}");
        // A frame that replaces another fits where the two would not.
        let mut outbox = Outbox::with_cap(250);
        outbox.push_frame(frame(1)).unwrap();
        outbox.push_frame(frame(2)).unwrap();
        assert!(outbox.push(frame(3)).is_err());
        // A peer that hung up is an error, not a spin.
        struct Gone;
        impl Write for Gone {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Ok(0)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(outbox.flush(&mut Gone).is_err());
    }
}
