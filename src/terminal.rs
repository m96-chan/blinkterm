//! The terminal a window is drawn on, as the window sees it.
//!
//! A window — the tabs, the row, the prompts, the frames the motion policy
//! wants painted — used to write to the [`Pane`] directly, decode its own
//! frames with the [`Painter`](crate::graphics::Painter) beside it and run the helper programs (a file
//! picker, a password command, a desktop browser) in the middle of its own
//! loop. That is one terminal and one window in one process, which is all
//! there was; the design for issue #83 is several terminals on one profile,
//! with the windows in one process that owns the engine and each terminal in
//! a process of its own. So this module is the line between them, drawn
//! where the backend draws it.
//!
//! [`Terminal`] is everything a window asks of a terminal, and nothing else:
//! bytes to write in order, the picture taken off, the screen cleared, a
//! frame to paint ([`FrameOut`], still encoded), whether the frames handed
//! over have been painted ([`Painted`]), the pointer's shape, how big the
//! terminal is now, and a helper program to run ([`Helper`]) and its answer
//! ([`HelperOutcome`]). [`LocalTerminal`] is the one that exists: the pane
//! this process holds, a [`Canvas`] that decodes beside it, the parser that
//! reads it, and the helpers it runs — the frontend's
//! ([`crate::frontend`]). The other is the backend's
//! (`crate::backend::RemoteTerminal`), which encodes each of these as a
//! message to the frontend that holds the terminal: nothing here returns
//! anything a socket could not carry back later, except the two answers a local terminal has
//! at once and a remote one never will (a frame that would not decode, a
//! helper that could not be started), which are answers a window can do
//! without.
//!
//! Helpers are started and answered, never waited for. A file picker or a
//! password command with a window of its own runs beside the loop, polled by
//! its descriptor; one that needs the terminal is run to the end inside
//! [`Terminal::start_helper`], with the terminal given to it and taken back,
//! and its answer is there for the next [`Terminal::poll_helper`]. The window
//! asks straight away for the answer of one it started in the terminal; from
//! a terminal in another process it comes on a later pass.

use std::collections::VecDeque;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::external;
use crate::fit::{Cells, Metrics};
use crate::graphics::Canvas;
use crate::hover::Shape;
use crate::input::{Input, Parser};
use crate::login;
use crate::picker;
use crate::route::Route;
use crate::screen::{self, Pane};
use crate::upload;

/// A frame as the engine sent it, base64 taken off: a screencast frame, or
/// the lossless still.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoded<'a> {
    Jpeg(&'a [u8]),
    Png(&'a [u8]),
}

/// A frame the window wants painted, and where: the cells it fills and the
/// screen row they start on, which are the window's layout, and the pane's
/// size in pixels, which a still is fitted to. `viewport_gen` is the window's
/// count of the sizes it has been laid out at ([`crate::app`] steps it on
/// every relayout), so that a terminal that paints later than it is told can
/// tell a frame of the old size from one of the new; the local terminal
/// paints at once and has no use for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameOut<'a> {
    pub payload: Encoded<'a>,
    pub cells: Cells,
    pub row: u32,
    pub pixels: (u32, u32),
    pub viewport_gen: u32,
}

/// Whether the frames handed over have gone to the terminal, asked once a
/// pass on a route paced by the link: `all` when every one has been written
/// or replaced, and `waited` for how long the last one written waited to be,
/// once per frame. See [`crate::screen::Outbox`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Painted {
    pub all: bool,
    pub waited: Option<Duration>,
}

/// A helper program a window wants run: what was decided before anything
/// started, by the window, from its settings and its page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Helper {
    /// A file picker for the input `chooser` on the tab `tab`, started in
    /// `dir`: with a window of its own, or given the terminal.
    Picker {
        tab: String,
        chooser: upload::Chooser,
        command: picker::Command,
        kind: picker::Kind,
        dir: PathBuf,
    },
    /// A password command for `site`, for the tab `tab` at `url`, started in
    /// `dir`.
    Login {
        tab: String,
        url: String,
        site: login::Site,
        command: picker::Command,
        kind: picker::Kind,
        dir: PathBuf,
    },
    /// A desktop browser: `argv`, with `BROWSER` as `browser` says and
    /// started in `home`. Left to run, never answered. `url` and
    /// `configured` (`external-browser`) are what `argv` was made from, for
    /// a terminal in another process, which makes its own from them with its
    /// own environment ([`crate::ipc::Job::External`]).
    External {
        argv: Vec<String>,
        browser: Option<String>,
        home: Option<PathBuf>,
        url: String,
        configured: Option<picker::Command>,
    },
}

impl Helper {
    /// Whether it needs the terminal while it runs.
    pub fn in_terminal(&self) -> bool {
        match self {
            Helper::Picker { kind, .. } | Helper::Login { kind, .. } => {
                *kind == picker::Kind::Terminal
            }
            Helper::External { .. } => false,
        }
    }
}

/// How a helper ended.
#[derive(Debug)]
pub enum HelperOutcome {
    Picker(picker::Outcome),
    Login(login::Outcome),
}

/// What [`Terminal::start_helper`] did: started it, or could not, with the
/// sentence for the row. An `Err` beside these is the terminal itself
/// failing — given to a program and not taken back — which ends the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    Running,
    Failed(String),
}

/// Everything a window asks of the terminal it is drawn on. See the module.
pub trait Terminal {
    /// Bytes for the terminal, in order, never dropped: the row, the list's
    /// rows, a clipboard's OSC 52.
    fn write(&mut self, bytes: &[u8]) -> Result<(), String>;
    /// The picture off the screen, the way the route put it there.
    fn clear_picture(&mut self) -> Result<(), String>;
    /// The whole screen cleared, the placeholder cells with it, so that the
    /// next paint writes them again.
    fn clear_screen(&mut self) -> Result<(), String>;
    /// Paint `frame`. `false` for one that would not decode, which only a
    /// terminal that decodes as it is told can say; see [`Canvas::paint`].
    fn frame(&mut self, frame: FrameOut<'_>) -> Result<bool, String>;
    /// Whether what was painted has gone out; once a pass. See [`Painted`].
    fn painted(&mut self) -> Painted;
    /// Whether frames are acknowledged to the engine once they have gone
    /// out, rather than at once: a route slower than the engine.
    fn paced(&self) -> bool;
    /// Whether a PNG goes to the terminal as it came.
    fn png_route(&self) -> bool;
    /// The pointer's shape, by [`Shape::name`].
    fn shape(&mut self, shape: Shape) -> Result<(), String>;
    /// How big the terminal is, when that has changed since the last time
    /// this was asked — a `SIGWINCH`, a cell size the terminal answered, or
    /// the terminal taken back from a helper — and `None` when it has not.
    fn resized(&mut self) -> Result<Option<Metrics>, String>;
    /// Start `job`, to be answered as `id`. See [`Started`].
    fn start_helper(&mut self, id: u64, job: Helper) -> Result<Started, String>;
    /// The descriptors the helpers running beside the loop are heard on.
    fn helper_fds(&self) -> Vec<RawFd>;
    /// The answer of helper `id`, once it has one, and then only once.
    /// `ready` is what the last poll found readable.
    fn poll_helper(&mut self, id: u64, ready: &[RawFd]) -> Option<HelperOutcome>;
    /// Helper `id` is not wanted any more: ended if it is running, its
    /// answer dropped if it has one.
    fn end_helper(&mut self, id: u64);
}

/// The terminal this process holds.
pub struct LocalTerminal {
    pane: Pane,
    canvas: Canvas,
    parser: Parser,
    /// The terminal's answer to `CSI 16 t`, for a pane whose kernel window
    /// size has no pixels in it. See [`crate::screen::ASK_CELL_SIZE`].
    cell_hint: Option<(u32, u32)>,
    /// Set by the `SIGWINCH` handler; this program's own, handed in.
    winched: &'static AtomicBool,
    /// Whether the pane wants measuring again for a reason of this side's:
    /// a cell size that changed, or the terminal back from a helper.
    remeasure: bool,
    /// The file picker with a window that is open, if one is: one at a time.
    picker: Option<(u64, picker::Gui)>,
    /// The password command with a window that is running, if one is.
    login: Option<(u64, login::Gui)>,
    /// What the helpers that ran in the terminal answered, for the next
    /// poll.
    done: VecDeque<(u64, HelperOutcome)>,
    /// The desktop browsers started and not yet gone, asked once a pass
    /// whether they have, so that none is left a zombie; never signalled — a
    /// browser outlives this program on purpose.
    launched: Vec<Child>,
}

impl LocalTerminal {
    /// The terminal over `pane`, painting with `canvas`. `cell_hint` is
    /// the cell size the probe heard, if it heard one; `winched` is the flag
    /// the `SIGWINCH` handler sets.
    pub fn new(
        pane: Pane,
        canvas: Canvas,
        cell_hint: Option<(u32, u32)>,
        winched: &'static AtomicBool,
    ) -> LocalTerminal {
        LocalTerminal {
            pane,
            canvas,
            parser: Parser::new(),
            cell_hint,
            winched,
            remeasure: false,
            picker: None,
            login: None,
            done: VecDeque::new(),
            launched: Vec::new(),
        }
    }

    /// The pane as it is now, measured.
    pub fn metrics(&self) -> Result<Metrics, String> {
        self.pane
            .metrics(self.cell_hint)
            .map_err(|e| format!("cannot measure the pane: {e}"))
    }

    pub fn route(&self) -> Route {
        self.canvas.route()
    }

    /// The descriptor the terminal's bytes arrive on.
    pub fn input_fd(&self) -> RawFd {
        self.pane.input_fd()
    }

    /// What `bytes` from the terminal say.
    pub fn parse(&mut self, bytes: &[u8]) -> Vec<Input> {
        self.parser.feed(bytes)
    }

    /// Whether mouse reports are in pixels: see [`Parser::pixel_coordinates`].
    pub fn pixel_coordinates(&self) -> bool {
        self.parser.pixel_coordinates()
    }

    /// [`Parser::pasting`].
    pub fn pasting(&self) -> bool {
        self.parser.pasting()
    }

    /// [`Parser::abandon_paste`].
    pub fn abandon_paste(&mut self) -> bool {
        self.parser.abandon_paste()
    }

    /// [`Parser::flush`].
    pub fn flush(&mut self) -> Option<Input> {
        self.parser.flush()
    }

    /// The terminal said how big a cell is. The pane is measured again on
    /// the next pass, as if it had been resized, which it has as far as the
    /// page is concerned when the kernel had no pixels to go on: the page,
    /// the placement and the HiDPI guess were all made from 8x16. A hint
    /// that changes nothing measured — a kernel that knew the pixels all
    /// along — is not a resize; `cell` is the cell the window was laid out
    /// for.
    pub fn cell_size(&mut self, width: u32, height: u32, cell: (u32, u32)) {
        self.cell_hint = Some((width, height));
        let measured = self.pane.metrics(self.cell_hint);
        if measured.is_ok_and(|metrics| metrics.cell != cell) {
            self.remeasure = true;
        }
    }

    /// The desktop browsers that have exited, forgotten; once a pass.
    pub fn reap(&mut self) {
        external::reap(&mut self.launched);
    }

    /// Give the terminal back, now.
    pub fn leave(&mut self) {
        self.pane.leave();
    }

    /// Give the terminal to a helper that needs it, run `run` to the end,
    /// and take the terminal back.
    ///
    /// The picture is taken off first, the modes turned off and the
    /// settings put back ([`Pane::release`]); the helper runs on the
    /// terminal as it would from the shell, in this program's foreground
    /// group; then everything is turned on again ([`Pane::resume`]) and the
    /// pane is measured again on the next pass, as after a resize — because
    /// the terminal may well have been resized while the helper had it, and
    /// the screen is empty either way. The pointer's shape went back to the
    /// arrow with the modes; the window that asked knows, because it asked
    /// for a helper in the terminal.
    ///
    /// Nothing else happens meanwhile. The engine's events wait in the tabs'
    /// queues and are read on the next pass. A `SIGTERM` meanwhile is heard
    /// once the helper has exited.
    fn hand_over<T>(&mut self, what: &str, run: impl FnOnce() -> T) -> Result<T, String> {
        let clear = self.canvas.clear();
        self.pane.write(&clear).map_err(|e| e.to_string())?;
        self.pane
            .release()
            .map_err(|e| format!("cannot give the terminal to the {what}: {e}"))?;
        let answer = run();
        self.pane
            .resume()
            .map_err(|e| format!("cannot take the terminal back from the {what}: {e}"))?;
        self.remeasure = true;
        Ok(answer)
    }
}

impl Terminal for LocalTerminal {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.pane.write(bytes).map_err(|e| e.to_string())
    }

    fn clear_picture(&mut self) -> Result<(), String> {
        let clear = self.canvas.clear();
        self.write(&clear)
    }

    fn clear_screen(&mut self) -> Result<(), String> {
        self.write(b"\x1b[2J")?;
        // The clear took any placeholder cells with it, and the picture may
        // now be another number of them: the next paint writes them again.
        self.canvas.invalidate_placeholders();
        Ok(())
    }

    /// The placeholder cells first when the route draws with them and they
    /// are not on screen, as text; then the frame. A frame that names a
    /// shared memory object is written as text is, never dropped: a name
    /// handed over and replaced would be a file nobody reads, and the
    /// painter counts those as a terminal that does not read names. Any
    /// other frame goes in the pane's slot, where a newer one replaces it
    /// unwritten ([`screen::Outbox`]).
    fn frame(&mut self, frame: FrameOut<'_>) -> Result<bool, String> {
        let Some(strokes) = self.canvas.paint(frame) else {
            return Ok(false);
        };
        if !strokes.placeholders.is_empty() {
            self.write(&strokes.placeholders)?;
        }
        if strokes.named {
            self.write(&strokes.bytes)?;
        } else {
            self.pane
                .write_frame(strokes.bytes)
                .map_err(|e| e.to_string())?;
        }
        Ok(true)
    }

    fn painted(&mut self) -> Painted {
        // The wait first, as the loop always took it: it is consumed every
        // pass whether or not anything is owed.
        let waited = self.pane.take_frame_wait();
        Painted {
            all: self.pane.frames_flushed(),
            waited,
        }
    }

    fn paced(&self) -> bool {
        self.canvas.route().paced()
    }

    fn png_route(&self) -> bool {
        self.canvas.png_route()
    }

    fn shape(&mut self, shape: Shape) -> Result<(), String> {
        self.write(&screen::pointer_shape(shape.name()))
    }

    fn resized(&mut self) -> Result<Option<Metrics>, String> {
        let winched = self.winched.swap(false, Ordering::SeqCst);
        if !(winched || std::mem::take(&mut self.remeasure)) {
            return Ok(None);
        }
        self.metrics().map(Some)
    }

    fn start_helper(&mut self, id: u64, job: Helper) -> Result<Started, String> {
        match job {
            Helper::Picker {
                tab,
                chooser,
                command,
                kind: picker::Kind::Gui,
                dir,
            } => match picker::Gui::spawn(&command, &dir, &tab, &chooser) {
                Ok(gui) => {
                    self.picker = Some((id, gui));
                    Ok(Started::Running)
                }
                Err(why) => Ok(Started::Failed(why)),
            },
            Helper::Picker {
                command,
                kind: picker::Kind::Terminal,
                dir,
                ..
            } => {
                let outcome = self.hand_over("file picker", || {
                    picker::run_terminal(&command, &dir, upload::home().as_deref())
                })?;
                self.done.push_back((id, HelperOutcome::Picker(outcome)));
                Ok(Started::Running)
            }
            Helper::Login {
                tab,
                url,
                site,
                command,
                kind: picker::Kind::Gui,
                dir,
            } => match login::Gui::spawn(&command, &site, &tab, &url, &dir) {
                Ok(gui) => {
                    self.login = Some((id, gui));
                    Ok(Started::Running)
                }
                Err(why) => Ok(Started::Failed(why)),
            },
            Helper::Login {
                site,
                command,
                kind: picker::Kind::Terminal,
                dir,
                ..
            } => {
                let outcome = self.hand_over("password command", || {
                    login::run_terminal(&command, &site, &dir)
                })?;
                self.done.push_back((id, HelperOutcome::Login(outcome)));
                Ok(Started::Running)
            }
            Helper::External {
                argv,
                browser,
                home,
                ..
            } => {
                let mut process = external::process(&argv, browser.as_deref(), home.as_deref());
                match external::launch(&mut process, &argv[0]) {
                    Ok(child) => {
                        self.launched.push(child);
                        Ok(Started::Running)
                    }
                    Err(why) => Ok(Started::Failed(why)),
                }
            }
        }
    }

    fn helper_fds(&self) -> Vec<RawFd> {
        let picker = self.picker.as_ref().and_then(|(_, gui)| gui.fd());
        let login = self.login.as_ref().and_then(|(_, gui)| gui.fd());
        picker.into_iter().chain(login).collect()
    }

    fn poll_helper(&mut self, id: u64, ready: &[RawFd]) -> Option<HelperOutcome> {
        if let Some(at) = self.done.iter().position(|(done, _)| *done == id) {
            return self.done.remove(at).map(|(_, outcome)| outcome);
        }
        if let Some((_, gui)) = self.picker.as_mut().filter(|(running, _)| *running == id) {
            let readable = gui.fd().is_some_and(|fd| ready.contains(&fd));
            let outcome = gui.pump(readable)?;
            self.picker = None;
            return Some(HelperOutcome::Picker(outcome));
        }
        if let Some((_, gui)) = self.login.as_mut().filter(|(running, _)| *running == id) {
            let readable = gui.fd().is_some_and(|fd| ready.contains(&fd));
            let outcome = gui.pump(readable)?;
            self.login = None;
            return Some(HelperOutcome::Login(outcome));
        }
        None
    }

    fn end_helper(&mut self, id: u64) {
        // Dropping a helper with a window is what ends it.
        if self
            .picker
            .as_ref()
            .is_some_and(|(running, _)| *running == id)
        {
            self.picker = None;
        }
        if self
            .login
            .as_ref()
            .is_some_and(|(running, _)| *running == id)
        {
            self.login = None;
        }
        self.done.retain(|(done, _)| *done != id);
    }
}
