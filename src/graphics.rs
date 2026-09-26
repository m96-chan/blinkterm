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
//! That check is what makes the same binary work in Kitty.
//!
//! # On macOS
//!
//! A Mac has no `/dev/shm`: a POSIX shared memory object made by
//! `shm_open(3)` has a name and no path, so the file store above cannot make
//! one. The object store in this module does, with three rules the readers
//! (kitty's `graphics.c`, Ghostty's `graphics_image.zig`) already work
//! around on their side. The object is written through a mapping, because
//! macOS refuses `read(2)` and `write(2)` on a shared memory descriptor. It
//! is sized once, because macOS refuses a second `ftruncate(2)`, so there is
//! no `.part` to rename: the object is whole before its name is sent. And
//! its size as `fstat` reports it is rounded up to a page, so the command
//! carries `S=`, the protocol's key for exactly that. Consumption is noticed
//! the same way: a reader unlinks what it read, so a name this program can
//! still unlink is one nobody read, and the fallback to inline works for
//! objects as it does for files.
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

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use tos_preview::fit::Cells;

use crate::base64;

/// Base64 goes out in chunks, as the protocol asks.
pub const CHUNK: usize = 4096;

/// The image and placement this program owns in the terminal's store.
pub const IMAGE_ID: u32 = 1;
pub const PLACEMENT_ID: u32 = 1;

/// Where POSIX shared memory objects live, as files — on Linux. Elsewhere
/// the directory does not exist and [`Painter::new`] does not look at it;
/// the constant stays unconditional so that nothing naming it needs a `cfg`.
pub const SHM_DIR: &str = "/dev/shm";

/// What the doctor says about shared memory when [`probe_shared_memory`]
/// succeeds, and when it does not: the thing that was tried, in this
/// platform's words.
#[cfg(target_os = "linux")]
pub const SHM_HOW: &str = "/dev/shm writable";
#[cfg(target_os = "linux")]
pub const SHM_HOW_NOT: &str = "/dev/shm not writable";
#[cfg(not(target_os = "linux"))]
pub const SHM_HOW: &str = "shm_open works";
#[cfg(not(target_os = "linux"))]
pub const SHM_HOW_NOT: &str = "shm_open fails";

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
    /// `t=s`: a POSIX shared memory name, which the terminal reads and
    /// unlinks.
    SharedMemory,
    /// `t=d`: base64 in the escape sequence itself.
    Inline,
}

/// Where a `t=s` frame is kept: as a file in a directory, or as a POSIX
/// shared memory object with no path at all.
enum Store {
    /// `<dir>/<name>`: `/dev/shm` on Linux, or a test's directory anywhere.
    Files(PathBuf),
    /// `shm_open(3)` objects, which is the only kind of `t=s` a Mac has.
    #[cfg(not(target_os = "linux"))]
    Objects,
}

impl Store {
    /// Whether a frame's command says how many bytes it is, with `S=`.
    ///
    /// Only for objects, whose size as a reader's `fstat` sees it is rounded
    /// up to a page. A file is exactly as long as the frame, and the command
    /// for one stays byte for byte what every measured terminal has read.
    fn announces_size(&self) -> bool {
        match self {
            Store::Files(_) => false,
            #[cfg(not(target_os = "linux"))]
            Store::Objects => true,
        }
    }
}

/// The state a sequence of frames needs: which names are outstanding, and
/// whether the terminal is reading them.
pub struct Painter {
    transport: Transport,
    store: Store,
    prefix: String,
    counter: u64,
    /// Names without their leading `/`, oldest first.
    outstanding: VecDeque<String>,
    unconsumed: u32,
}

impl Painter {
    /// Choose a transport by trying the better one.
    ///
    /// `/dev/shm` may be absent, read-only or full — in a container, in a
    /// rescue shell, on a machine whose tmpfs is exhausted — and each of those
    /// is a reason to send frames inline rather than to fail. Off Linux there
    /// is no `/dev/shm` to try, and the store is `shm_open(3)` objects,
    /// tried the same way.
    pub fn new() -> Painter {
        #[cfg(target_os = "linux")]
        {
            Painter::at(Path::new(SHM_DIR))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Painter::objects()
        }
    }

    /// The same, in a directory a test can watch.
    pub fn at(dir: &Path) -> Painter {
        let prefix = default_prefix();
        let usable = probe_file(dir, &format!("{prefix}-probe")).is_ok();
        Painter::with(Store::Files(dir.to_path_buf()), prefix, usable)
    }

    /// Frames as `shm_open(3)` objects, if one can be made here.
    #[cfg(not(target_os = "linux"))]
    pub fn objects() -> Painter {
        Painter::objects_named(default_prefix())
    }

    /// [`Painter::objects`] under a prefix of the caller's: objects share
    /// one namespace across the machine, so two painters in one process —
    /// two tests running at once — need prefixes of their own where two
    /// directories would have kept them apart.
    #[cfg(not(target_os = "linux"))]
    fn objects_named(prefix: String) -> Painter {
        let usable = object::probe(&format!("/{prefix}-probe")).is_ok();
        Painter::with(Store::Objects, prefix, usable)
    }

    fn with(store: Store, prefix: String, usable: bool) -> Painter {
        Painter {
            transport: if usable {
                Transport::SharedMemory
            } else {
                Transport::Inline
            },
            store,
            prefix,
            counter: 0,
            outstanding: VecDeque::new(),
            unconsumed: 0,
        }
    }

    pub fn transport(&self) -> Transport {
        self.transport
    }

    /// The bytes that put `raw` on screen at `row`, `col`, sized to `cells`.
    ///
    /// The cursor is moved to the placement's corner first, because `a=T`
    /// places at the cursor, and `C=1` keeps it there so that the status line
    /// can be written afterwards without the picture having moved anything.
    pub fn frame(&mut self, raw: Raw<'_>, cells: Cells, row: u32, col: u32) -> Vec<u8> {
        let mut out = format!("\x1b[{row};{col}H").into_bytes();
        match self.transport {
            Transport::SharedMemory => match self.write_object(raw.pixels) {
                Some(name) => {
                    let size = self.store.announces_size().then_some(raw.pixels.len());
                    out.extend_from_slice(&shared_memory_command_sized(&name, &raw, cells, size));
                }
                None => {
                    // Writing failed, so the store that worked at startup
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
        match &self.store {
            Store::Files(dir) => {
                let path = dir.join(&name);
                let partial = dir.join(format!("{name}.part"));

                // Written under another name and renamed, so that the
                // compositor never sees a file that is still growing: it
                // takes the size from the descriptor and would read a short
                // image.
                if write_private(&partial, pixels).is_err() {
                    let _ = std::fs::remove_file(&partial);
                    return None;
                }
                if std::fs::rename(&partial, &path).is_err() {
                    let _ = std::fs::remove_file(&partial);
                    return None;
                }
            }
            // Whole before anyone is told its name: `create` maps, copies and
            // unmaps before it returns.
            #[cfg(not(target_os = "linux"))]
            Store::Objects => object::create(&format!("/{name}"), pixels)?,
        }

        self.outstanding.push_back(name.clone());
        if self.outstanding.len() > IN_FLIGHT {
            if let Some(old) = self.outstanding.pop_front() {
                // Still there means the terminal never read it. A few of those
                // in a row and this is not a terminal that speaks `t=s`.
                if self.retire(&old) {
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

    /// Remove the frame called `name`, and say whether it was still there —
    /// which, once the terminal has had its chance, means it never read it.
    /// A reader unlinks what it reads, a file and an object alike, so this is
    /// one question with two ways of asking it.
    fn retire(&self, name: &str) -> bool {
        match &self.store {
            Store::Files(dir) => std::fs::remove_file(dir.join(name)).is_ok(),
            #[cfg(not(target_os = "linux"))]
            Store::Objects => object::unlink(&format!("/{name}")),
        }
    }

    /// Remove anything this program left in shared memory.
    ///
    /// Called on the way out, including from the panic path: a frame written
    /// and not read is a file in a tmpfs, or on a Mac an object, that would
    /// otherwise stay until the machine is rebooted.
    pub fn clean_up(&mut self) {
        while let Some(name) = self.outstanding.pop_front() {
            self.retire(&name);
        }
    }
}

/// `blinkterm-<pid>`: unique across the machine while this process lives,
/// which is what an object's namespace needs, and short enough that a name
/// under it fits macOS's 31 bytes (a test pins the arithmetic).
fn default_prefix() -> String {
    format!("blinkterm-{}", std::process::id())
}

/// Write `bytes` to a new file at `path` that only this user can read.
///
/// 0600 rather than the umask's 0644: a frame is a picture of whatever is on
/// the screen, and between the write and the terminal's unlink it would
/// otherwise be readable by every local user. The reader is this user's own
/// terminal, so nothing that should read a frame is refused.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

/// Whether a file can be written into `dir` as `name` and removed again.
fn probe_file(dir: &Path, name: &str) -> std::io::Result<()> {
    let probe = dir.join(name);
    let written = std::fs::write(&probe, b"probe");
    let _ = std::fs::remove_file(&probe);
    written
}

/// Whether this side can make a `t=s` frame at all, tried under `name` (no
/// `/`): a file in [`SHM_DIR`] on Linux, a `shm_open(3)` object elsewhere.
/// The same test [`Painter::new`] makes, for the doctor, which reports the
/// error as well as the answer.
pub fn probe_shared_memory(name: &str) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        probe_file(Path::new(SHM_DIR), name)
    }
    #[cfg(not(target_os = "linux"))]
    {
        object::probe(&format!("/{name}"))
    }
}

/// `PSHMNAMLEN` in XNU's `sys/posix_shm.h`: macOS refuses a shared memory
/// name longer than this, slash included, with `ENAMETOOLONG`. Compiled for
/// the tests everywhere, so that the arithmetic in the names is checked on
/// Linux too.
#[cfg(any(test, not(target_os = "linux")))]
const OBJECT_NAME_MAX: usize = 31;

/// A `t=s` frame as a POSIX shared memory object, for the systems where
/// `shm_open(3)` makes something that is not a file.
///
/// Written through a mapping and never through `write(2)`, because on macOS
/// a shared memory descriptor cannot be read or written, only mapped (kitty's
/// reader says the same of its side). Sized exactly once, because macOS
/// refuses a second `ftruncate`. Created `O_EXCL` under a name nobody else
/// has, and 0600: unlike a file in `/dev/shm` made with the umask, a frame
/// here is readable by this user alone.
#[cfg(not(target_os = "linux"))]
mod object {
    use std::ffi::CString;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    use super::OBJECT_NAME_MAX as NAME_MAX;

    /// Make `name` hold exactly `bytes`. `None` is any failure, and the
    /// object is unlinked again on the way out of one so nothing is left.
    pub fn create(name: &str, bytes: &[u8]) -> Option<()> {
        make(name, bytes).ok()
    }

    /// Unlink `name`; `true` if it was still there, which after the terminal
    /// has had its chance means the terminal never read it.
    pub fn unlink(name: &str) -> bool {
        let Ok(name) = CString::new(name) else {
            return false;
        };
        // SAFETY: `name` is a NUL-terminated string that outlives the call,
        // and `shm_unlink(2)` only reads it.
        unsafe { libc::shm_unlink(name.as_ptr()) == 0 }
    }

    /// Whether an object can be made, mapped and removed here at all, with
    /// the error that says why not: `ENAMETOOLONG`, `EACCES`, `ENOSPC`.
    pub fn probe(name: &str) -> io::Result<()> {
        make(name, b"probe")?;
        unlink(name);
        Ok(())
    }

    /// [`create`], with the reason. The name is checked against
    /// [`NAME_MAX`] here rather than left to `shm_open`, so that a longer
    /// prefix fails a test on any platform instead of a frame on a Mac.
    fn make(name: &str, bytes: &[u8]) -> io::Result<()> {
        if name.len() > NAME_MAX {
            return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
        }
        let c_name =
            CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: `c_name` is NUL-terminated and outlives the call, which
        // only reads it. The mode goes through the variadic tail as the
        // `unsigned int` a promoted `mode_t` is.
        let fd = unsafe {
            libc::shm_open(
                c_name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` was returned open by `shm_open` just now and nothing
        // else holds it, so the `OwnedFd` is its only owner and closes it on
        // every way out of this function.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let filled = fill(&fd, bytes);
        if filled.is_err() {
            unlink(name);
        }
        filled
    }

    /// Size the object once and copy `bytes` in through a mapping.
    fn fill(fd: &OwnedFd, bytes: &[u8]) -> io::Result<()> {
        let len = libc::off_t::try_from(bytes.len())
            .map_err(|_| io::Error::from_raw_os_error(libc::EFBIG))?;
        // SAFETY: `ftruncate(2)` takes a descriptor and a length and reads no
        // memory; `fd` is open for writing for the whole call.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), len) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if bytes.is_empty() {
            // Complete once truncated, and `mmap` of nothing would fail.
            return Ok(());
        }
        // SAFETY: a fresh shared mapping of the object, placed by the kernel
        // (null hint), `bytes.len()` long — exactly the size just set — from
        // a descriptor open read-write. No Rust reference points into it.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes.len(),
                libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `map` is `bytes.len()` writable bytes that this function
        // alone can see, so the copy stays inside it, and it cannot overlap
        // `bytes`, which lives in this process's own heap or stack.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), map.cast::<u8>(), bytes.len()) };
        // SAFETY: `map` and the length are exactly what `mmap` was given and
        // returned, and nothing refers into the mapping after this.
        unsafe { libc::munmap(map, bytes.len()) };
        Ok(())
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
    shared_memory_command_sized(name, raw, cells, None)
}

/// [`shared_memory_command`], with `S=` when `size` is given: the
/// protocol's key for how many of the object's bytes are the image, which
/// matters where a reader's `fstat` sees the size rounded up to a page.
fn shared_memory_command_sized(
    name: &str,
    raw: &Raw<'_>,
    cells: Cells,
    size: Option<usize>,
) -> Vec<u8> {
    let control = control(raw, cells);
    let size = size.map(|bytes| format!(",S={bytes}")).unwrap_or_default();
    format!(
        "\x1b_G{control},t=s{size};{}\x1b\\",
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
    let control = control(raw, cells);
    let encoded = base64::encode(raw.pixels);
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

#[cfg(test)]
mod tests {
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
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600, "a frame is this user's alone");
            }
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

    #[test]
    fn an_object_name_never_exceeds_the_macos_limit() {
        // The largest pid macOS hands out and ten digits of counter, which at
        // sixty frames a second is five and a half years of frames.
        let longest = format!("/blinkterm-{}-{}", 99_999, u32::MAX);
        assert!(longest.len() <= OBJECT_NAME_MAX, "{longest}");
        let ours = format!("/{}-{}", default_prefix(), 9_999_999_999u64);
        assert!(ours.len() <= OBJECT_NAME_MAX, "{ours}");
        let probe = format!("/{}-probe", default_prefix());
        assert!(probe.len() <= OBJECT_NAME_MAX, "{probe}");
    }

    #[test]
    fn a_file_frame_says_nothing_about_its_size_and_is_the_command_it_always_was() {
        let dir = temp_dir("unsized");
        let mut painter = Painter::at(&dir);
        let pixels = rgb();
        let bytes = painter.frame(Raw::rgb(&pixels, 2, 2), cells(4, 2), 2, 1);
        let body = apc_bodies(&bytes).remove(0);
        let cmd = GraphicsCommand::parse(&body).expect("parses");
        let name = String::from_utf8(cmd.payload).expect("a name");
        let mut wanted = b"\x1b[2;1H".to_vec();
        wanted.extend(shared_memory_command(
            &name,
            &Raw::rgb(&pixels, 2, 2),
            cells(4, 2),
        ));
        assert_eq!(bytes, wanted);
        assert!(!body.windows(3).any(|w| w == b",S="), "no S= on a file");
        painter.clean_up();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The object store, which exists only where there is no `/dev/shm`,
    /// read back the way a terminal on a Mac reads it.
    #[cfg(not(target_os = "linux"))]
    mod objects {
        use super::*;
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        /// What Kitty and Ghostty do with a name on macOS: open it read-only,
        /// ask its size, map that much and copy it out. `read(2)` is not an
        /// option there, which is the point of mapping.
        fn read_object(name: &str) -> std::io::Result<Vec<u8>> {
            let c_name = CString::new(name).expect("a name without NUL");
            // SAFETY: `c_name` is NUL-terminated and outlives the call; with
            // no `O_CREAT` the variadic mode is not read, so none is passed.
            let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: `fd` was just returned open and nothing else owns it.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: `fstat(2)` fills in the whole `stat` it is pointed at,
            // and `stat` is exactly that much space, alive for the call.
            if unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: `fstat` succeeded, so every field has been written.
            let size = unsafe { stat.assume_init() }.st_size;
            let size = usize::try_from(size).expect("a size");
            if size == 0 {
                return Ok(Vec::new());
            }
            // SAFETY: a fresh read-only shared mapping the kernel places, of
            // the size `fstat` reported, from a descriptor open for reading.
            let map = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    size,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    fd.as_raw_fd(),
                    0,
                )
            };
            if map == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: `map` is `size` readable bytes until the `munmap`
            // below, and the slice is copied out before that.
            let bytes = unsafe { std::slice::from_raw_parts(map.cast::<u8>(), size) }.to_vec();
            // SAFETY: exactly the pointer and length `mmap` gave, and the
            // slice above is gone.
            unsafe { libc::munmap(map, size) };
            Ok(bytes)
        }

        /// A prefix of the test's own, so that tests running at once do not
        /// make the same name in the machine-wide namespace.
        fn painter(which: &str) -> Painter {
            Painter::objects_named(format!("blinkterm-{}-{which}", std::process::id()))
        }

        #[test]
        fn an_object_frame_is_mapped_whole_and_unlinks_once() {
            let mut painter = painter("m");
            assert_eq!(painter.transport(), Transport::SharedMemory);
            let pixels: Vec<u8> = (0..300u32).map(|i| (i * 7) as u8).collect();
            let bytes = painter.frame(Raw::rgb(&pixels, 10, 10), cells(4, 2), 2, 1);
            let cmd = GraphicsCommand::parse(&apc_bodies(&bytes)[0]).expect("parses");
            assert_eq!(cmd.medium, Medium::SharedMemory);
            assert_eq!(
                cmd.data_size,
                pixels.len() as u64,
                "S= says how much of a page-rounded object is the frame"
            );
            let name = String::from_utf8(cmd.payload).expect("a name");
            assert!(name.starts_with('/') && !name[1..].contains('/'), "{name}");

            let read = read_object(&name).expect("a terminal can open and map it");
            assert!(read.len() >= pixels.len(), "{} bytes", read.len());
            assert_eq!(&read[..pixels.len()], &pixels[..]);

            // The terminal's unlink, and then ours finding nothing: which is
            // how a consumed frame is told from an unread one.
            assert!(object::unlink(&name), "the reader's unlink finds it");
            assert!(!object::unlink(&name), "and a second finds nothing");
            let gone = read_object(&name).expect_err("unlinked");
            assert_eq!(gone.raw_os_error(), Some(libc::ENOENT));
            painter.clean_up();
        }

        #[test]
        fn a_terminal_that_never_unlinks_an_object_gets_the_bytes_instead() {
            let mut painter = painter("u");
            assert_eq!(painter.transport(), Transport::SharedMemory);
            let mut names = Vec::new();
            for _ in 0..IN_FLIGHT * 2 {
                let bytes = painter.frame(Raw::rgb(b"rgb", 1, 1), cells(4, 2), 2, 1);
                let cmd = GraphicsCommand::parse(&apc_bodies(&bytes)[0]).expect("parses");
                if cmd.medium == Medium::SharedMemory {
                    names.push(String::from_utf8(cmd.payload).expect("a name"));
                }
            }
            assert_eq!(
                painter.transport(),
                Transport::Inline,
                "unread objects mean the terminal does not speak t=s"
            );
            painter.clean_up();
            for name in &names {
                let gone = read_object(name).expect_err("nothing is left behind");
                assert_eq!(gone.raw_os_error(), Some(libc::ENOENT), "{name}");
            }
        }

        #[test]
        fn an_object_name_that_is_too_long_is_refused_before_shm_open() {
            let long = format!("/{}", "x".repeat(OBJECT_NAME_MAX));
            assert_eq!(object::create(&long, b"x"), None);
            let too_long = object::probe(&long).expect_err("refused");
            assert_eq!(too_long.raw_os_error(), Some(libc::ENAMETOOLONG));
        }
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
