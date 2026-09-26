//! Putting a frame of video where a picture goes.
//!
//! Every screencast frame is a whole picture, so every frame is a
//! transmission of a new image. Sixty times a second, that raises three
//! questions the protocol answers badly if you do not think about them: which
//! image id to use, which format the bytes are in, and how they get there.
//!
//! # Raw pixels, decoded here
//!
//! **`f=24` and `f=32`, not `f=100`.** The frames arrive from the engine as
//! JPEG while the page is moving and as PNG when it stops (see
//! [`crate::motion`]), and neither should be handed to the terminal to
//! decode: it cannot read JPEG on the graphics path at all, and the PNG
//! decode it does do is on the compositor's parse loop, which
//! `docs/design/browser.md` already names as the first cost to delete. So
//! this program decodes — 8 ms for a 1280x770 JPEG frame, on its own thread,
//! in the pane — and hands over pixels, which is the protocol's own raw
//! format and needs no compositor change at all.
//!
//! It costs bytes: 2.9 MB of RGB where the JPEG was 185 kB. Through `t=s`
//! that is a `write` into tmpfs and a `read` out of it, which is a memcpy at
//! memory speed and cheaper than the decode it replaces. Through the inline
//! fallback it is not, and the section below says what that means.
//!
//! # One image id, retransmitted
//!
//! A fixed image id (`i=1`) with a fixed placement id (`p=1`), sent as `a=T`
//! every frame. Not two ids alternating, and no `a=d` delete of the old one.
//!
//! The reason is in the store rather than in the protocol.
//! `GraphicsStore::store` removes any image already under the id and subtracts
//! its bytes before inserting the new one, and `GraphicsStore::place` drops
//! any placement with the same image and placement id before making the new
//! one (`compositor/tos-term/src/graphics.rs`). So a retransmission under the
//! same pair leaves exactly one image and one placement behind, whatever the
//! frame rate — there is nothing to leak and nothing to delete. Alternating
//! two ids would hold two frames' worth of RGBA instead of one and would need
//! a delete after every frame, and a delete is a command whose effect on the
//! screen lands between the new image and its placement: a window, however
//! small, in which the pane has no picture in it. Retransmitting has no such
//! window, because the replacement happens inside one parse.
//!
//! # A name, not the bytes
//!
//! `t=s` hands the compositor a POSIX shared memory name and it reads the file
//! itself, which keeps the frame out of the PTY sixty times a second — base64
//! would add a third to it and the pane's reader would spend its day on it.
//! The compositor *unlinks the object after reading it*
//! (`docs/design/graphics-file-transmission.md`), so every frame needs a name
//! of its own; the counter in [`Painter`] is that.
//!
//! Two consequences are handled here. A name is written under a `.part`
//! suffix and renamed into place, because the reader takes the file's size
//! from the descriptor and a frame caught mid-write would be read short. And
//! names that were never consumed are collected: if the terminal on the other
//! end does not implement `t=s` — every terminal that is not tOS — the files
//! pile up in `/dev/shm` and nothing appears on screen, so after a few
//! unconsumed names [`Painter`] gives up and sends the bytes inline instead.
//! That check is what makes the same binary work in a local Kitty whose page
//! moves: a page at rest sends four frames and the check needs twenty-four,
//! which is why the terminal is asked first ([`crate::doctor::probe`]) and a
//! run that is not local never writes a name at all ([`crate::route`]).
//!
//! **The fallback sends the same raw pixels, base64, and it is slow.** That
//! is a decision rather than an oversight. Sending the encoded frame instead
//! is not available: the motion frames are JPEG and no terminal's graphics
//! path reads JPEG. Re-encoding the decoded pixels as PNG would mean a PNG
//! *encoder* in this crate — a third codec written from a specification — to
//! make faster a path that exists only for terminals which are not tOS. So:
//! 2.9 MB becomes 3.9 MB of base64 a frame and a pane in somebody else's
//! terminal gets a slideshow. It is correct, it is obviously correct, and the
//! terminal this program is for never takes it.
//!
//! # Through tmux: wrapped, and placed with text
//!
//! tmux eats a raw graphics command whole. It passes one on only inside a
//! DCS — `ESC P tmux;`, the command with every `ESC` doubled, `ESC \\` — and
//! only with `allow-passthrough on`; the terminal's answer comes back to the
//! pane either way. [`wrap_for_tmux`] makes that form, cutting between
//! commands so that no wrapper is over [`DCS_LIMIT`]: tmux 3.4 passed a
//! 1,020,428-byte DCS through in a tenth of a second and dropped a
//! 1,124,128-byte one without a word, and a wrapper per 4 kB chunk costs a
//! third of the throughput in the 53 bytes of reset tmux writes after each.
//!
//! A placement at the cursor is no good there: tmux owns the cursor, and
//! the outer one is wherever tmux last left it. So the image is a *virtual*
//! placement (`U=1`) and the picture is text — a pane of `U+10EEEE` cells,
//! each carrying its row and column as two combining marks
//! ([`DIACRITICS`]) and the image id as its foreground colour, which tmux
//! stores, copies about and redraws like any other text
//! ([`placeholder_pane`]). They are written once per size and again after
//! something wrote over them, never per frame: a frame is the wrapped
//! command and nothing else. [`PLACEHOLDER_IMAGE_ID`] says why the id is 16.
//!
//! # Over ssh: the engine's PNG, as it came
//!
//! `f=100`, the base64 the engine sent re-chunked at [`CHUNK`], and nothing
//! decoded here ([`Painter::png_frame`]). Raw pixels are 3.9 MB of base64 a
//! frame at 1280x770; the engine's PNG of the same frame is 120 to 200 kB
//! on a page of text and within 15% of what a deflate of the raw pixels
//! would make, so there is no encoder here and no `o=z`. The terminal
//! decodes it, which is the cost the first section refuses for a local pane
//! and the lesser one over a network. See [`crate::route`] for when each is
//! chosen.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use tos_preview::fit::Cells;

use crate::base64;
use crate::route::{Payload, Placement, Route, Wrap};

/// Base64 goes out in chunks, as the protocol asks.
pub const CHUNK: usize = 4096;

/// The image and placement this program owns in the terminal's store.
pub const IMAGE_ID: u32 = 1;
pub const PLACEMENT_ID: u32 = 1;

/// Where POSIX shared memory objects live, as files.
pub const SHM_DIR: &str = "/dev/shm";

/// The largest DCS [`wrap_for_tmux`] makes.
///
/// tmux 3.4 passed a 1,020,428-byte DCS through whole and dropped a
/// 1,124,128-byte one with no error and nothing emitted: its string buffer
/// is 1 MiB. A frame's chunks are 4 kB commands, so a wrapper holds about
/// 240 of them and a 260 kB PNG frame goes in one.
pub const DCS_LIMIT: usize = 1_000_000;

/// The image id under placeholders.
///
/// Not 1: a placeholder cell carries the id as its foreground colour,
/// `38;5;<id>`, and tmux rewrites an index below 8 as the basic colour
/// (`31` for 1 — which kitty happens to read back as index 1, and nothing
/// obliges another terminal to) and one from 16 up verbatim, whatever the
/// outer `TERM` says about colours. A 24-bit colour is worse: without the
/// `RGB` feature tmux snaps `38;2;0;0;1` to `38;5;16` and the id is gone.
/// 16 is the first number that reaches the terminal exactly as written.
pub const PLACEHOLDER_IMAGE_ID: u32 = 16;

/// The placeholder character: a cell that shows the part of a virtual
/// placement its marks name.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// kitty's `rowcolumn-diacritics.txt`, all 297, in order: index n is the
/// combining mark for row or column n, zero-based.
#[rustfmt::skip]
pub const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A,
    0x034B, 0x034C, 0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365,
    0x0366, 0x0367, 0x0368, 0x0369, 0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F,
    0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592, 0x0593, 0x0594, 0x0595, 0x0597,
    0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1, 0x05A8, 0x05A9,
    0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6,
    0x06D7, 0x06D8, 0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2,
    0x06E4, 0x06E7, 0x06E8, 0x06EB, 0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736,
    0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743, 0x0745, 0x0747, 0x0749, 0x074A,
    0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3, 0x0816, 0x0817,
    0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951,
    0x0953, 0x0954, 0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD,
    0x193A, 0x1A17, 0x1A75, 0x1A76, 0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C,
    0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1,
    0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4, 0x1DC5, 0x1DC6,
    0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF,
    0x1DE0, 0x1DE1, 0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1,
    0x20D4, 0x20D5, 0x20D6, 0x20D7, 0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0,
    0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2, 0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6,
    0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE, 0x2DEF, 0x2DF0,
    0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1,
    0xA8E0, 0xA8E1, 0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9,
    0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED, 0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2,
    0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1, 0xFE20, 0xFE21, 0xFE22, 0xFE23,
    0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187, 0x1D188, 0x1D189,
    0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// The most rows or columns a placeholder pane can address: one mark each.
/// A pane wider or taller gets the picture in the first 297 and blank cells
/// beyond; at 8 px a cell that is 2,376 pixels.
pub const PLACEHOLDER_MAX: u32 = DIACRITICS.len() as u32;

/// How many frames may be in flight before an unconsumed name means the
/// terminal is not reading them.
///
/// The compositor reads a name when it parses the escape sequence, which is
/// after the bytes have crossed the PTY and got to the front of its queue —
/// so a name written for this frame may well still be there when the next is
/// written. Sixteen frames is a quarter of a second at sixty; a terminal that
/// has not read a name by then is not going to.
const IN_FLIGHT: usize = 16;

/// One decoded frame, in the layout its decoder produced.
///
/// Three channels or four, and the protocol has a format for each, so the
/// pixels go across as they are rather than being widened or narrowed:
/// `tos_term::jpeg` produces RGB and a JPEG has no alpha to lose,
/// `tos_term::png` produces RGBA and a still is one frame in a hundred and
/// fifty milliseconds, so neither conversion would buy anything.
#[derive(Debug, Clone, Copy)]
pub struct Raw<'a> {
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
    /// Bytes a pixel: three for `f=24`, four for `f=32`.
    pub channels: u32,
}

impl<'a> Raw<'a> {
    /// Three bytes a pixel, which is what a decoded JPEG is.
    pub fn rgb(pixels: &'a [u8], width: u32, height: u32) -> Raw<'a> {
        Raw {
            pixels,
            width,
            height,
            channels: 3,
        }
    }

    /// Four, which is what a decoded PNG is.
    pub fn rgba(pixels: &'a [u8], width: u32, height: u32) -> Raw<'a> {
        Raw {
            pixels,
            width,
            height,
            channels: 4,
        }
    }

    /// The protocol's `f=` for this layout.
    fn format(&self) -> u32 {
        if self.channels == 3 {
            24
        } else {
            32
        }
    }
}

/// How the payload reaches the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// `t=s`: a name in `/dev/shm`, which the terminal reads and unlinks.
    SharedMemory,
    /// `t=d`: base64 in the escape sequence itself.
    Inline,
}

/// The state a sequence of frames needs: which names are outstanding, and
/// whether the terminal is reading them.
pub struct Painter {
    transport: Transport,
    route: Route,
    /// The cells the placeholders on screen were written for, under
    /// [`Placement::Unicode`]; `None` when they have to be written again.
    placed: Option<Cells>,
    dir: PathBuf,
    prefix: String,
    counter: u64,
    outstanding: VecDeque<PathBuf>,
    unconsumed: u32,
}

impl Painter {
    /// Choose a transport by trying the better one.
    ///
    /// `/dev/shm` may be absent, read-only or full — in a container, in a
    /// rescue shell, on a machine whose tmpfs is exhausted — and each of those
    /// is a reason to send frames inline rather than to fail.
    pub fn new() -> Painter {
        Painter::at(Path::new(SHM_DIR))
    }

    /// The same, in a directory a test can watch.
    pub fn at(dir: &Path) -> Painter {
        let prefix = format!("blinkterm-{}", std::process::id());
        let probe = dir.join(format!("{prefix}-probe"));
        let usable = std::fs::write(&probe, b"probe").is_ok();
        let _ = std::fs::remove_file(&probe);
        Painter {
            transport: if usable {
                Transport::SharedMemory
            } else {
                Transport::Inline
            },
            route: Route::local(usable),
            placed: None,
            dir: dir.to_path_buf(),
            prefix,
            counter: 0,
            outstanding: VecDeque::new(),
            unconsumed: 0,
        }
    }

    /// The painter for a route [`crate::route::choose`] made, in `/dev/shm`.
    pub fn with_route(route: Route) -> Painter {
        Painter::at_with(Path::new(SHM_DIR), route)
    }

    /// The same, in a directory a test can watch.
    ///
    /// The route may only take the shared memory away, never give it: `at`
    /// has tried the directory, and a route that says inline — a PNG, a
    /// placeholder, a terminal on another machine — is inline whatever the
    /// directory said. Unicode placement is inline by construction, since
    /// nothing behind tmux is local enough to be handed a name.
    pub fn at_with(dir: &Path, route: Route) -> Painter {
        let mut painter = Painter::at(dir);
        let inline = route.transport == Transport::Inline
            || route.payload == Payload::Png
            || route.placement == Placement::Unicode;
        if inline {
            painter.transport = Transport::Inline;
        }
        painter.route = Route {
            transport: painter.transport,
            ..route
        };
        painter
    }

    /// Whether frames could go through `/dev/shm` on this machine: the
    /// write-and-remove `new` does, asked on its own so that
    /// [`crate::route::choose`] can be told before there is a painter.
    pub fn shm_usable() -> bool {
        Painter::new().transport() == Transport::SharedMemory
    }

    pub fn transport(&self) -> Transport {
        self.transport
    }

    pub fn route(&self) -> Route {
        self.route
    }

    /// The cells a frame is placed in: the pane's, cut to what placeholders
    /// can address on the route that uses them.
    fn fit(&self, cells: Cells) -> Cells {
        match self.route.placement {
            Placement::Direct => cells,
            Placement::Unicode => Cells {
                cols: cells.cols.clamp(1, PLACEHOLDER_MAX),
                rows: cells.rows.clamp(1, PLACEHOLDER_MAX),
            },
        }
    }

    /// The cursor move a placement at the cursor needs first; nothing for a
    /// virtual placement, which has no cursor.
    fn cursor(&self, row: u32, col: u32) -> Vec<u8> {
        match self.route.placement {
            Placement::Direct => format!("\x1b[{row};{col}H").into_bytes(),
            Placement::Unicode => Vec::new(),
        }
    }

    /// Graphics commands as the route sends them: bare, or wrapped for tmux.
    fn wrapped(&self, commands: Vec<u8>) -> Vec<u8> {
        match self.route.wrap {
            Wrap::None => commands,
            Wrap::Tmux => wrap_for_tmux(&commands),
        }
    }

    /// A PNG from the engine, put on screen as it came: `f=100`, inline,
    /// sized to `cells`. The terminal reads the size from the file, so a
    /// frame the engine cast at half the pane is scaled into the same cells.
    pub fn png_frame(&mut self, png: &[u8], cells: Cells, row: u32, col: u32) -> Vec<u8> {
        let cells = self.fit(cells);
        let mut out = self.cursor(row, col);
        out.extend_from_slice(&self.wrapped(png_command(png, cells, self.route.placement)));
        out
    }

    /// Take the picture off the screen and the frame out of the store, the
    /// way this route put it there: [`clear_command`] at the cursor, the
    /// virtual placement by id under placeholders — `d=I`, whose uppercase
    /// removes virtual placements too and frees the data. The placeholder
    /// cells are the caller's to clear, as the rows always were; the next
    /// paint writes them again.
    pub fn clear(&mut self) -> Vec<u8> {
        self.placed = None;
        let command = match self.route.placement {
            Placement::Direct => clear_command(),
            Placement::Unicode => {
                format!("\x1b_Ga=d,d=I,i={PLACEHOLDER_IMAGE_ID},q=2\x1b\\").into_bytes()
            }
        };
        self.wrapped(command)
    }

    /// The placeholder cells for a picture in `cells` at screen row `row`,
    /// when they are not on screen already; empty otherwise, and always on a
    /// route that places at the cursor. Text, to be written as text.
    pub fn placeholders(&mut self, cells: Cells, row: u32) -> Vec<u8> {
        if self.route.placement != Placement::Unicode {
            return Vec::new();
        }
        let cells = self.fit(cells);
        if self.placed == Some(cells) {
            return Vec::new();
        }
        self.placed = Some(cells);
        placeholder_pane(PLACEHOLDER_IMAGE_ID, cells, row)
    }

    /// Something wrote over the rows — a resize's clear, the tab list — so
    /// the next paint writes the placeholders again.
    pub fn invalidate_placeholders(&mut self) {
        self.placed = None;
    }

    /// The bytes that put `raw` on screen at `row`, `col`, sized to `cells`.
    ///
    /// The cursor is moved to the placement's corner first, because `a=T`
    /// places at the cursor, and `C=1` keeps it there so that the status line
    /// can be written afterwards without the picture having moved anything.
    ///
    /// Through tmux the command is wrapped, the placement is virtual and
    /// nothing moves the cursor: see the module's section on it.
    pub fn frame(&mut self, raw: Raw<'_>, cells: Cells, row: u32, col: u32) -> Vec<u8> {
        if self.route.placement == Placement::Unicode || self.route.wrap == Wrap::Tmux {
            let cells = self.fit(cells);
            let mut out = self.cursor(row, col);
            let command = placed_inline_command(&raw, cells, self.route.placement);
            out.extend_from_slice(&self.wrapped(command));
            return out;
        }
        let mut out = format!("\x1b[{row};{col}H").into_bytes();
        match self.transport {
            Transport::SharedMemory => match self.write_object(raw.pixels) {
                Some(name) => out.extend_from_slice(&shared_memory_command(&name, &raw, cells)),
                None => {
                    // Writing failed, so the directory that worked at startup
                    // does not any more; the picture is more important than
                    // the transport it arrives by.
                    self.transport = Transport::Inline;
                    out.extend_from_slice(&inline_command(&raw, cells));
                }
            },
            Transport::Inline => out.extend_from_slice(&inline_command(&raw, cells)),
        }
        out
    }

    /// Write one frame under a fresh name, and retire the names that are old
    /// enough to have been read by now.
    fn write_object(&mut self, pixels: &[u8]) -> Option<String> {
        self.counter += 1;
        let name = format!("{}-{}", self.prefix, self.counter);
        let path = self.dir.join(&name);
        let partial = self.dir.join(format!("{name}.part"));

        // Written under another name and renamed, so that the compositor never
        // sees a file that is still growing: it takes the size from the
        // descriptor and would read a short image.
        if std::fs::write(&partial, pixels).is_err() {
            let _ = std::fs::remove_file(&partial);
            return None;
        }
        if std::fs::rename(&partial, &path).is_err() {
            let _ = std::fs::remove_file(&partial);
            return None;
        }

        self.outstanding.push_back(path);
        if self.outstanding.len() > IN_FLIGHT {
            if let Some(old) = self.outstanding.pop_front() {
                // Still there means the terminal never read it. A few of those
                // in a row and this is not a terminal that speaks `t=s`.
                if std::fs::remove_file(&old).is_ok() {
                    self.unconsumed += 1;
                    if self.unconsumed >= IN_FLIGHT as u32 / 2 {
                        self.transport = Transport::Inline;
                    }
                } else {
                    self.unconsumed = 0;
                }
            }
        }
        Some(format!("/{name}"))
    }

    /// Remove anything this program left in `/dev/shm`.
    ///
    /// Called on the way out, including from the panic path: a frame written
    /// and not read is a file that would otherwise sit in a tmpfs until the
    /// machine is rebooted.
    pub fn clean_up(&mut self) {
        for path in self.outstanding.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Default for Painter {
    fn default() -> Self {
        Painter::new()
    }
}

impl Drop for Painter {
    fn drop(&mut self) {
        self.clean_up();
    }
}

/// The control keys every frame carries.
///
/// `s=` and `v=` are the picture's real pixel dimensions, and for a raw
/// payload they are not advisory the way they are for a PNG: pixels are a
/// rectangle and nothing else, so this is where the terminal learns its
/// shape. `c=` and `r=` are the cells it is drawn into, which is a separate
/// thing, and the two agree because the page is laid out at the pane's own
/// pixel size — a frame that is the size of the cells it fills is blitted
/// rather than resampled.
fn control(raw: &Raw<'_>, cells: Cells) -> String {
    format!(
        "a=T,f={},s={},v={},i={IMAGE_ID},p={PLACEMENT_ID},c={},r={},C=1,q=2",
        raw.format(),
        raw.width.max(1),
        raw.height.max(1),
        cells.cols.max(1),
        cells.rows.max(1)
    )
}

/// `t=s`: the payload is the name of the object, not the image.
pub fn shared_memory_command(name: &str, raw: &Raw<'_>, cells: Cells) -> Vec<u8> {
    let control = control(raw, cells);
    format!(
        "\x1b_G{control},t=s;{}\x1b\\",
        base64::encode(name.as_bytes())
    )
    .into_bytes()
}

/// Take the picture off the screen and the frame out of the store.
///
/// For switching tabs, which is the one moment the picture on screen belongs
/// to a page that is no longer being shown. The alternative — leaving it until
/// the new tab's first frame lands — would show the old page under the new
/// tab's title for as long as the new page takes to paint, which on a tab that
/// was opened a second ago and has not loaded is as long as the network takes.
/// An empty pane is honest about there being nothing to show yet.
///
/// `d=I` rather than `d=i`: the uppercase form frees the image data as well as
/// the placement, and the data is the pane in RGBA — nearly four megabytes at
/// 1280x770. The next frame transmits a new image under the same id anyway, so
/// there is nothing to keep.
pub fn clear_command() -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={IMAGE_ID},p={PLACEMENT_ID},q=2\x1b\\").into_bytes()
}

/// `t=d`: the pixels themselves, base64, in as many escape sequences as it
/// takes — which for a pane-sized frame is several hundred.
pub fn inline_command(raw: &Raw<'_>, cells: Cells) -> Vec<u8> {
    chunked(&control(raw, cells), raw.pixels)
}

/// The control keys for a placement of either kind: the frame's own at the
/// cursor, or a virtual one under [`PLACEHOLDER_IMAGE_ID`].
///
/// A virtual placement has no `p=` — a placement id is the underline colour
/// of a placeholder cell, which tmux drops unless the outer terminal has
/// `usstyle` — and no `C=`, since it has no cursor to move. `size` is `None`
/// for a PNG, which carries its own.
fn placed_control(
    format: u32,
    size: Option<(u32, u32)>,
    cells: Cells,
    placement: Placement,
) -> String {
    let mut out = format!("a=T,f={format}");
    if let Some((width, height)) = size {
        out.push_str(&format!(",s={},v={}", width.max(1), height.max(1)));
    }
    let (cols, rows) = (cells.cols.max(1), cells.rows.max(1));
    match placement {
        Placement::Direct => out.push_str(&format!(
            ",i={IMAGE_ID},p={PLACEMENT_ID},c={cols},r={rows},C=1,q=2"
        )),
        Placement::Unicode => out.push_str(&format!(
            ",i={PLACEHOLDER_IMAGE_ID},U=1,c={cols},r={rows},q=2"
        )),
    }
    out
}

/// Raw pixels, inline, placed either way.
fn placed_inline_command(raw: &Raw<'_>, cells: Cells, placement: Placement) -> Vec<u8> {
    let size = Some((raw.width, raw.height));
    chunked(
        &placed_control(raw.format(), size, cells, placement),
        raw.pixels,
    )
}

/// `f=100`: a PNG, inline, placed either way. No `s=`/`v=`: the file says.
pub fn png_command(png: &[u8], cells: Cells, placement: Placement) -> Vec<u8> {
    chunked(&placed_control(100, None, cells, placement), png)
}

/// A payload as base64 in [`CHUNK`]-sized commands, the first carrying the
/// control keys and every one saying whether more follow.
fn chunked(control: &str, payload: &[u8]) -> Vec<u8> {
    let encoded = base64::encode(payload);
    if encoded.is_empty() {
        return format!("\x1b_G{control};\x1b\\").into_bytes();
    }

    let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(CHUNK).collect();
    let mut out = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        let head = if index == 0 {
            format!("\x1b_G{control},m={more};")
        } else {
            format!("\x1b_Gm={more};")
        };
        out.extend_from_slice(head.as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// Graphics commands wrapped for tmux's passthrough: `ESC P tmux;`, the
/// bytes with every `ESC` doubled, `ESC \\` — as many wrappers as it takes to
/// keep each under [`DCS_LIMIT`].
///
/// A wrapper is cut only after a command's `ESC \\`, never inside one, so
/// each carries whole commands; the chunks of a frame already say `m=1` and
/// `m=0` and do not care which wrapper they ride in. As few wrappers as the
/// limit allows, because tmux follows each with its own resets and turns the
/// outer terminal's mouse modes off and on again around it.
pub fn wrap_for_tmux(bytes: &[u8]) -> Vec<u8> {
    const OPEN: &[u8] = b"\x1bPtmux;";
    const CLOSE: &[u8] = b"\x1b\\";
    let escapes = bytes.iter().filter(|&&b| b == 0x1b).count();
    let mut out = Vec::with_capacity(bytes.len() + escapes + OPEN.len() + CLOSE.len());
    // Bytes in the wrapper that is open; 0 when none is.
    let mut open = 0usize;
    for unit in commands(bytes) {
        let size = unit.len() + unit.iter().filter(|&&b| b == 0x1b).count();
        if open > 0 && open + size + CLOSE.len() > DCS_LIMIT {
            out.extend_from_slice(CLOSE);
            open = 0;
        }
        if open == 0 {
            out.extend_from_slice(OPEN);
            open = OPEN.len();
        }
        for &byte in unit {
            if byte == 0x1b {
                out.push(0x1b);
            }
            out.push(byte);
        }
        open += size;
    }
    if open > 0 {
        out.extend_from_slice(CLOSE);
    }
    out
}

/// `bytes` cut after every `ESC \\`: whole escape strings, and whatever is
/// between them with the one that follows.
fn commands(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest
            .windows(2)
            .position(|pair| pair == b"\x1b\\")
            .map_or(rest.len(), |at| at + 2);
        let (unit, after) = rest.split_at(end);
        rest = after;
        Some(unit)
    })
}

/// The combining mark for row or column `n`.
fn diacritic(n: u32) -> char {
    DIACRITICS
        .get(n as usize)
        .and_then(|&code| char::from_u32(code))
        .unwrap_or('\u{0305}')
}

/// One row of placeholder cells: row `r` of the image (zero-based), `cols`
/// wide, written at screen row `screen_row` from the first column.
///
/// The image id is the foreground colour, `38;5;<id>`, and both marks go on
/// every cell — never left for the terminal to infer from the cell to the
/// left: tmux copies cells about when a window is split or resized, and a
/// cell that carries its own coordinates survives that.
pub fn placeholder_row(id: u32, r: u32, cols: u32, screen_row: u32) -> Vec<u8> {
    let mut out = format!("\x1b[{screen_row};1H\x1b[38;5;{id}m").into_bytes();
    let row_mark = diacritic(r);
    let mut cell = [0u8; 4];
    for c in 0..cols.min(PLACEHOLDER_MAX) {
        for mark in [PLACEHOLDER, row_mark, diacritic(c)] {
            out.extend_from_slice(mark.encode_utf8(&mut cell).as_bytes());
        }
    }
    out.extend_from_slice(b"\x1b[39m");
    out
}

/// The whole pane of them: image rows `0..cells.rows` at screen rows `row`
/// onwards, cut at [`PLACEHOLDER_MAX`] either way. A 100x29 pane is 23.8 kB,
/// written once per size.
pub fn placeholder_pane(id: u32, cells: Cells, row: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for r in 0..cells.rows.min(PLACEHOLDER_MAX) {
        out.extend_from_slice(&placeholder_row(id, r, cells.cols, row + r));
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tos_term::graphics::{Action, Format, GraphicsCommand, Medium};

    fn cells(cols: u32, rows: u32) -> Cells {
        Cells { cols, rows }
    }

    /// A 2x2 picture, in each of the two layouts the protocol takes.
    fn rgb() -> Vec<u8> {
        vec![
            10, 20, 30, 40, 50, 60, //
            70, 80, 90, 100, 110, 120,
        ]
    }

    fn rgba() -> Vec<u8> {
        vec![
            10, 20, 30, 255, 40, 50, 60, 255, //
            70, 80, 90, 255, 100, 110, 120, 255,
        ]
    }

    /// Pull the APC bodies out the way the terminal's parser does.
    fn apc_bodies(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut bodies = Vec::new();
        let mut rest = bytes;
        while let Some(start) = find(rest, b"\x1b_G") {
            let body = &rest[start + 3..];
            let end = find(body, b"\x1b\\").expect("unterminated APC");
            bodies.push(body[..end].to_vec());
            rest = &body[end + 2..];
        }
        bodies
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    #[test]
    fn a_shared_memory_frame_names_an_object_and_says_nothing_back() {
        let pixels = rgb();
        let raw = Raw::rgb(&pixels, 640, 368);
        let bytes = shared_memory_command("/blinkterm-1-2", &raw, cells(80, 23));
        let bodies = apc_bodies(&bytes);
        assert_eq!(bodies.len(), 1);
        let cmd = GraphicsCommand::parse(&bodies[0]).expect("parses");
        assert_eq!(cmd.action, Action::TransmitAndDisplay);
        assert_eq!(cmd.format, Format::Rgb, "the pixels go over, not a file");
        assert_eq!((cmd.width, cmd.height), (640, 368));
        assert_eq!(cmd.medium, Medium::SharedMemory);
        assert_eq!(cmd.image_id, IMAGE_ID);
        assert_eq!(cmd.placement_id, PLACEMENT_ID);
        assert_eq!((cmd.cols, cmd.rows), (80, 23));
        assert!(cmd.cursor_stays, "the status line is written after this");
        assert_eq!(cmd.quiet, 2, "nothing is reading a reply");
        assert_eq!(cmd.payload, b"/blinkterm-1-2");
    }

    /// A still is RGBA because that is what the PNG decoder produces, and the
    /// protocol takes it under a format of its own rather than a conversion.
    #[test]
    fn a_still_goes_over_as_rgba_and_a_motion_frame_as_rgb() {
        let three = rgb();
        let four = rgba();
        for (raw, format) in [
            (Raw::rgb(&three, 2, 2), Format::Rgb),
            (Raw::rgba(&four, 2, 2), Format::Rgba),
        ] {
            let bodies = apc_bodies(&inline_command(&raw, cells(2, 1)));
            let cmd = GraphicsCommand::parse(&bodies[0]).expect("parses");
            assert_eq!(cmd.format, format);
            assert_eq!((cmd.width, cmd.height), (2, 2));
        }
    }

    #[test]
    fn an_inline_frame_is_the_picture_in_chunks_that_reassemble() {
        let pixels: Vec<u8> = (0..CHUNK * 5).map(|i| (i * 31) as u8).collect();
        let raw = Raw::rgb(&pixels, (CHUNK as u32 * 5) / 3, 1);
        let bodies = apc_bodies(&inline_command(&raw, cells(10, 4)));
        assert!(bodies.len() > 1);

        let first = GraphicsCommand::parse(&bodies[0]).expect("parses");
        assert_eq!(first.medium, Medium::Direct);
        assert_eq!(first.image_id, IMAGE_ID);
        assert!(first.more);
        assert!(
            !GraphicsCommand::parse(bodies.last().unwrap())
                .expect("parses")
                .more
        );

        let mut joined = Vec::new();
        for body in &bodies {
            let semicolon = body.iter().position(|&b| b == b';').unwrap();
            joined.extend_from_slice(&tos_term::graphics::decode_base64(&body[semicolon + 1..]));
        }
        assert_eq!(joined, pixels);
    }

    #[test]
    fn switching_tabs_takes_the_old_page_off_the_screen() {
        let mut terminal = tos_term::Terminal::new(40, 12, tos_term::TerminalConfig::default());
        let mut painter = Painter::at(Path::new("/nonexistent-for-a-test"));
        let pixels = rgb();
        terminal.advance(&painter.frame(Raw::rgb(&pixels, 2, 2), cells(4, 2), 2, 1));
        assert_eq!(terminal.graphics().placements().count(), 1);
        assert!(terminal.graphics().image(IMAGE_ID).is_some());

        terminal.advance(&clear_command());
        assert_eq!(
            terminal.graphics().placements().count(),
            0,
            "the picture is gone"
        );
        assert!(
            terminal.graphics().image(IMAGE_ID).is_none(),
            "and so are its pixels"
        );
    }

    #[test]
    fn a_frame_puts_the_cursor_where_the_picture_goes() {
        let mut painter = Painter::at(Path::new("/nonexistent-for-a-test"));
        assert_eq!(painter.transport(), Transport::Inline);
        let bytes = painter.frame(Raw::rgb(b"rgb", 1, 1), cells(4, 2), 2, 1);
        assert!(bytes.starts_with(b"\x1b[2;1H"), "{bytes:?}");
    }

    #[test]
    fn frames_get_a_fresh_name_each_time_because_the_last_one_was_eaten() {
        let dir = temp_dir("names");
        let mut painter = Painter::at(&dir);
        assert_eq!(painter.transport(), Transport::SharedMemory);

        let mut names = Vec::new();
        for _ in 0..4 {
            let bytes = painter.frame(Raw::rgb(b"rgb", 1, 1), cells(4, 2), 2, 1);
            let body = apc_bodies(&bytes).remove(0);
            let cmd = GraphicsCommand::parse(&body).expect("parses");
            let name = String::from_utf8(cmd.payload).expect("a name");
            assert!(name.starts_with('/'), "a POSIX name: {name}");
            assert!(!name.contains(".part"), "{name}");
            // The file is there, whole, and nothing is left half-written.
            let path = dir.join(name.trim_start_matches('/'));
            assert_eq!(std::fs::read(&path).unwrap(), b"rgb");
            names.push(name);
            // The terminal reads and unlinks; here the test does.
            std::fs::remove_file(&path).expect("consume");
        }
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 4, "every frame needs its own name");
        assert!(
            dir.read_dir().unwrap().next().is_none(),
            "nothing left over"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_terminal_that_never_reads_a_name_gets_the_bytes_instead() {
        let dir = temp_dir("unread");
        let mut painter = Painter::at(&dir);
        for _ in 0..IN_FLIGHT * 2 {
            painter.frame(Raw::rgb(b"rgb", 1, 1), cells(4, 2), 2, 1);
        }
        assert_eq!(
            painter.transport(),
            Transport::Inline,
            "unconsumed names mean the terminal does not speak t=s"
        );
        painter.clean_up();
        let left = dir.read_dir().unwrap().count();
        assert_eq!(left, 0, "and nothing is left behind in /dev/shm");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn what_the_terminal_makes_of_a_frame_is_one_image_and_one_placement() {
        // The real thing: drive a terminal with the bytes and look at its
        // store. Retransmitting under the same id must not accumulate.
        let mut terminal = tos_term::Terminal::new(40, 12, tos_term::TerminalConfig::default());
        let pixels = rgb();
        for _ in 0..8 {
            terminal.advance(&inline_command(&Raw::rgb(&pixels, 2, 2), cells(8, 4)));
        }
        let store = terminal.graphics();
        assert_eq!(store.placements().count(), 1);
        let image = store.image(IMAGE_ID).expect("the one image");
        assert_eq!((image.width, image.height), (2, 2));
        // RGB widens to the RGBA the store holds, with an opaque alpha.
        assert_eq!(&image.data[..8], &[10, 20, 30, 255, 40, 50, 60, 255]);
        let placement = store.placements().next().expect("the one placement");
        assert_eq!(placement.image_id, IMAGE_ID);
        assert_eq!(placement.placement_id, PLACEMENT_ID);
        assert_eq!((placement.cols, placement.rows), (8, 4));
    }

    /// tmux's passthrough, as a model: every `ESC P tmux;` … `ESC \` has its
    /// wrapper taken off and its doubled escapes halved, and what is outside
    /// a wrapper goes through as it is. Checked against what tmux 3.4 was
    /// recorded emitting for the wrapped query (the first test below uses the
    /// recorded bytes), and against the real tmux by hand.
    pub(crate) fn tmux_unwrap(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            if let Some(body) = rest.strip_prefix(b"\x1bPtmux;") {
                let mut i = 0;
                loop {
                    match (body.get(i), body.get(i + 1)) {
                        (Some(0x1b), Some(0x1b)) => {
                            out.push(0x1b);
                            i += 2;
                        }
                        (Some(0x1b), Some(b'\\')) => {
                            i += 2;
                            break;
                        }
                        (Some(&byte), _) => {
                            out.push(byte);
                            i += 1;
                        }
                        (None, _) => panic!("an unterminated wrapper"),
                    }
                }
                rest = &body[i..];
            } else {
                out.push(rest[0]);
                rest = &rest[1..];
            }
        }
        out
    }

    fn tmux_route() -> Route {
        Route {
            transport: Transport::Inline,
            payload: Payload::Png,
            placement: Placement::Unicode,
            wrap: Wrap::Tmux,
            every_nth: 2,
        }
    }

    /// A 1x1 PNG, the smallest real file there is.
    fn png_1x1() -> Vec<u8> {
        base64::decode(
            b"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
        )
        .expect("base64")
    }

    #[test]
    fn wrapping_for_tmux_doubles_every_escape_and_closes_with_st() {
        let query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
        let wrapped = wrap_for_tmux(query);
        // What the design's probe wrote into a tmux pane, byte for byte, and
        // what tmux then emitted to the terminal outside it.
        assert_eq!(
            wrapped,
            b"\x1bPtmux;\x1b\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\x1b\\\x1b\\".to_vec()
        );
        assert_eq!(tmux_unwrap(&wrapped), query.to_vec());
        assert!(
            wrap_for_tmux(b"").is_empty(),
            "nothing is no wrapper at all"
        );
    }

    #[test]
    fn a_frame_of_two_hundred_and_fifty_chunks_is_cut_into_wrappers_under_the_dcs_limit_and_never_inside_a_chunk(
    ) {
        let pixels: Vec<u8> = (0..CHUNK * 3 / 4 * 600).map(|i| (i * 7) as u8).collect();
        let frame = inline_command(&Raw::rgb(&pixels, 600, CHUNK as u32 / 4), cells(80, 24));
        assert!(apc_bodies(&frame).len() >= 250);
        let wrapped = wrap_for_tmux(&frame);
        let wrappers: Vec<&[u8]> = split_wrappers(&wrapped);
        assert!(wrappers.len() >= 2, "{} wrappers", wrappers.len());
        for wrapper in &wrappers {
            assert!(wrapper.len() <= DCS_LIMIT, "{}", wrapper.len());
            // Each one, unwrapped, is whole commands and nothing cut.
            let inner = tmux_unwrap(wrapper);
            assert!(inner.starts_with(b"\x1b_G"));
            assert!(inner.ends_with(b"\x1b\\"));
            let bodies = apc_bodies(&inner);
            let rebuilt: Vec<u8> = bodies
                .iter()
                .flat_map(|body| [&b"\x1b_G"[..], body, b"\x1b\\"].concat())
                .collect();
            assert_eq!(rebuilt, inner);
        }
        assert!(
            wrappers.len() <= wrapped.len() / (DCS_LIMIT / 2) + 1,
            "as few wrappers as the limit allows"
        );
    }

    /// The wrappers in `bytes`, each `ESC P tmux;` to its closing `ESC \`.
    fn split_wrappers(bytes: &[u8]) -> Vec<&[u8]> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == 0x1b && bytes[i + 1] == 0x1b {
                i += 2;
                continue;
            }
            if bytes[i] == 0x1b && bytes[i + 1] == b'\\' {
                out.push(&bytes[start..i + 2]);
                i += 2;
                start = i;
                continue;
            }
            i += 1;
        }
        out
    }

    #[test]
    fn unwrapping_what_was_wrapped_gives_the_frame_back() {
        let mut painter = Painter::at_with(Path::new("/nonexistent-for-a-test"), tmux_route());
        let png: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let bytes = painter.png_frame(&png, cells(100, 29), 2, 1);
        let unwrapped = tmux_unwrap(&bytes);
        assert_eq!(
            unwrapped,
            png_command(&png, cells(100, 29), Placement::Unicode)
        );
        let mut joined = Vec::new();
        for body in apc_bodies(&unwrapped) {
            let semicolon = body.iter().position(|&b| b == b';').unwrap();
            joined.extend_from_slice(&tos_term::graphics::decode_base64(&body[semicolon + 1..]));
        }
        assert_eq!(joined, png, "the engine's bytes, untouched");
    }

    #[test]
    fn a_png_frame_is_f_100_with_no_size_and_the_pixels_are_the_engines_bytes() {
        let route = Route {
            placement: Placement::Direct,
            wrap: Wrap::None,
            ..tmux_route()
        };
        let mut painter = Painter::at_with(Path::new("/nonexistent-for-a-test"), route);
        let png = png_1x1();
        let bytes = painter.png_frame(&png, cells(4, 2), 2, 1);
        assert!(bytes.starts_with(b"\x1b[2;1H"), "placed at the cursor");
        let bodies = apc_bodies(&bytes);
        let cmd = GraphicsCommand::parse(&bodies[0]).expect("parses");
        assert_eq!(cmd.format, Format::Png);
        assert_eq!((cmd.width, cmd.height), (0, 0), "the file says");
        assert_eq!(cmd.medium, Medium::Direct);
        assert_eq!(cmd.payload, png);
        assert_eq!((cmd.image_id, cmd.placement_id), (IMAGE_ID, PLACEMENT_ID));

        // And the terminal decodes it into an image of the file's size.
        let mut terminal = tos_term::Terminal::new(40, 12, tos_term::TerminalConfig::default());
        terminal.advance(&bytes);
        let image = terminal.graphics().image(IMAGE_ID).expect("stored");
        assert_eq!((image.width, image.height), (1, 1));
    }

    #[test]
    fn a_placeholder_frame_carries_u_1_and_no_placement_id() {
        let mut painter = Painter::at_with(Path::new("/nonexistent-for-a-test"), tmux_route());
        assert_eq!(painter.transport(), Transport::Inline);
        let bytes = painter.png_frame(&png_1x1(), cells(100, 29), 2, 1);
        assert!(
            bytes.starts_with(b"\x1bPtmux;"),
            "no cursor move under tmux"
        );
        let inner = tmux_unwrap(&bytes);
        let body = apc_bodies(&inner).remove(0);
        let control =
            String::from_utf8_lossy(&body[..body.iter().position(|&b| b == b';').unwrap()])
                .into_owned();
        let keys: Vec<&str> = control.split(',').collect();
        assert!(keys.contains(&"U=1"), "{control}");
        assert!(keys.contains(&"i=16"), "{control}");
        assert!(
            keys.contains(&"c=100") && keys.contains(&"r=29"),
            "{control}"
        );
        assert!(!keys.iter().any(|key| key.starts_with("p=")), "{control}");
        assert!(!keys.iter().any(|key| key.starts_with("C=")), "{control}");
        // Raw pixels on the same route say the same, with their size.
        let raw = painter.frame(Raw::rgb(&rgb(), 2, 2), cells(100, 29), 2, 1);
        let body = apc_bodies(&tmux_unwrap(&raw)).remove(0);
        let cmd = GraphicsCommand::parse(&body).expect("parses");
        assert_eq!(
            (cmd.width, cmd.height, cmd.image_id),
            (2, 2, PLACEHOLDER_IMAGE_ID)
        );
    }

    #[test]
    fn the_diacritic_table_has_two_hundred_and_ninety_seven_entries_and_starts_with_overline() {
        assert_eq!(DIACRITICS.len(), 297);
        assert_eq!(PLACEHOLDER_MAX, 297);
        assert_eq!(DIACRITICS[0], 0x0305, "combining overline");
        assert_eq!(
            (DIACRITICS[1], DIACRITICS[2], DIACRITICS[29], DIACRITICS[30]),
            (0x030D, 0x030E, 0x036F, 0x0483)
        );
        assert_eq!(DIACRITICS[296], 0x1D244);
        assert!(
            DIACRITICS.windows(2).all(|pair| pair[0] < pair[1]),
            "in order"
        );
        for code in DIACRITICS {
            assert!(char::from_u32(code).is_some(), "{code:#x} is a character");
        }
    }

    #[test]
    fn a_placeholder_row_puts_the_row_diacritic_then_the_column_diacritic_on_every_cell() {
        let row = placeholder_row(16, 0, 2, 2);
        let mut wanted = b"\x1b[2;1H\x1b[38;5;16m".to_vec();
        wanted.extend_from_slice(&[0xF4, 0x8E, 0xBB, 0xAE, 0xCC, 0x85, 0xCC, 0x85]);
        wanted.extend_from_slice(&[0xF4, 0x8E, 0xBB, 0xAE, 0xCC, 0x85, 0xCC, 0x8D]);
        wanted.extend_from_slice(b"\x1b[39m");
        assert_eq!(row, wanted);
        let third = placeholder_row(16, 2, 1, 4);
        assert!(third.starts_with(b"\x1b[4;1H"));
        assert!(String::from_utf8(third)
            .unwrap()
            .contains("\u{10EEEE}\u{030E}\u{0305}"));
    }

    #[test]
    fn a_placeholder_pane_stops_at_two_hundred_and_ninety_seven_columns() {
        let pane = placeholder_pane(16, cells(400, 3), 2);
        let text = String::from_utf8(pane).expect("utf-8");
        assert_eq!(text.matches(PLACEHOLDER).count(), 297 * 3);
        let tall = placeholder_pane(16, cells(1, 400), 2);
        assert_eq!(
            String::from_utf8(tall)
                .unwrap()
                .matches(PLACEHOLDER)
                .count(),
            297
        );
        let mut painter = Painter::at_with(Path::new("/nonexistent-for-a-test"), tmux_route());
        let bytes = painter.png_frame(&png_1x1(), cells(400, 30), 2, 1);
        let body = apc_bodies(&tmux_unwrap(&bytes)).remove(0);
        let cmd = GraphicsCommand::parse(&body).expect("parses");
        assert_eq!(
            (cmd.cols, cmd.rows),
            (297, 30),
            "the placement is the cells the placeholders can address"
        );
    }

    #[test]
    fn the_placeholder_image_id_is_one_tmux_forwards_unchanged() {
        // tmux rewrites `38;5;n` below 8 as a basic colour and passes 16 up
        // verbatim, and `38;5` stops at 255.
        const { assert!(PLACEHOLDER_IMAGE_ID >= 16 && PLACEHOLDER_IMAGE_ID < 256) };
        let mut painter = Painter::at_with(Path::new("/nonexistent-for-a-test"), tmux_route());
        let cells = painter.placeholders(cells(1, 1), 2);
        assert!(
            cells.starts_with(b"\x1b[2;1H\x1b[38;5;16m"),
            "the id as an indexed colour, not a 24-bit one tmux would snap: {cells:?}"
        );
    }

    #[test]
    fn a_placeholder_pane_is_written_once_and_again_after_a_clear_or_a_resize() {
        let mut painter = Painter::at_with(Path::new("/nonexistent-for-a-test"), tmux_route());
        assert!(!painter.placeholders(cells(10, 4), 2).is_empty());
        assert!(painter.placeholders(cells(10, 4), 2).is_empty(), "once");
        assert!(
            !painter.placeholders(cells(12, 4), 2).is_empty(),
            "a new size"
        );
        painter.clear();
        assert!(
            !painter.placeholders(cells(12, 4), 2).is_empty(),
            "after a clear"
        );
        painter.invalidate_placeholders();
        assert!(
            !painter.placeholders(cells(12, 4), 2).is_empty(),
            "after a resize"
        );
        let mut local = Painter::at(Path::new("/nonexistent-for-a-test"));
        assert!(
            local.placeholders(cells(10, 4), 2).is_empty(),
            "never at the cursor"
        );
    }

    #[test]
    fn clearing_under_placeholders_deletes_the_virtual_placement_with_capital_i() {
        let mut painter = Painter::at_with(Path::new("/nonexistent-for-a-test"), tmux_route());
        let clear = painter.clear();
        assert_eq!(
            tmux_unwrap(&clear),
            b"\x1b_Ga=d,d=I,i=16,q=2\x1b\\".to_vec()
        );
        assert!(clear.starts_with(b"\x1bPtmux;"), "wrapped, or tmux eats it");

        // The terminal agrees: what was stored under 16 is gone.
        let mut terminal = tos_term::Terminal::new(40, 12, tos_term::TerminalConfig::default());
        terminal.advance(&tmux_unwrap(&painter.png_frame(
            &png_1x1(),
            cells(4, 2),
            2,
            1,
        )));
        assert!(terminal.graphics().image(PLACEHOLDER_IMAGE_ID).is_some());
        terminal.advance(&tmux_unwrap(&clear));
        assert!(terminal.graphics().image(PLACEHOLDER_IMAGE_ID).is_none());

        let mut local = Painter::at(Path::new("/nonexistent-for-a-test"));
        assert_eq!(local.clear(), clear_command());
    }

    #[test]
    fn a_route_that_says_inline_is_inline_whatever_dev_shm_says() {
        let dir = temp_dir("route");
        assert_eq!(Painter::at(&dir).transport(), Transport::SharedMemory);
        let ssh = Route {
            placement: Placement::Direct,
            wrap: Wrap::None,
            ..tmux_route()
        };
        let painter = Painter::at_with(&dir, ssh);
        assert_eq!(painter.transport(), Transport::Inline);
        assert_eq!(painter.route().transport, Transport::Inline);
        assert_eq!(
            Painter::at_with(&dir, Route::local(true)).transport(),
            Transport::SharedMemory
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn temp_dir(what: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "blinkterm-{what}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("a directory to write in");
        dir
    }
}
