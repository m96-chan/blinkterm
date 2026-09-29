//! The terminal this program runs in: its size, raw mode, and reads that do
//! not block.
//!
//! Copied from tOS's `tos-platform` (`compositor/tos-platform/src/tty.rs` at
//! `c7677bde`), where it served the nested development backend. The code is
//! unchanged; the `SAFETY` comments are this crate's, which asks for one on
//! every `unsafe` block.

use std::io;
use std::os::unix::io::RawFd;

/// A terminal returned to its previous state when dropped.
pub struct RawMode {
    fd: RawFd,
    saved: libc::termios,
    restored: bool,
}

impl RawMode {
    /// Put a terminal into raw mode: no echo, no line buffering, no signal
    /// generation, so that every key reaches the program unchanged.
    pub fn acquire(fd: RawFd) -> io::Result<RawMode> {
        // SAFETY: `termios` is plain integers and arrays; all zeroes is a
        // valid value, and it is overwritten by `tcgetattr` before use.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: writes one `termios` through a pointer to a live local.
        if unsafe { libc::tcgetattr(fd, &mut saved) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = saved;
        // SAFETY: edits one `termios` through a pointer to a live local.
        unsafe { libc::cfmakeraw(&mut raw) };
        // Return from read as soon as a single byte is available.
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: read-only through the pointer, and `raw` is a live local.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(RawMode {
            fd,
            saved,
            restored: false,
        })
    }

    /// Restore the saved settings early.
    pub fn restore(&mut self) {
        if !self.restored {
            // SAFETY: read-only through a pointer to a field of `self`.
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved);
            }
            self.restored = true;
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Size of a terminal, in cells and pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
    pub width_px: u16,
    pub height_px: u16,
}

/// Ask the kernel how big a terminal is.
pub fn terminal_size(fd: RawFd) -> io::Result<TerminalSize> {
    // SAFETY: `winsize` is four `u16`s; all zeroes is a valid value.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `TIOCGWINSZ` writes one `winsize`, and `ws` is a live local of
    // exactly that type.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(TerminalSize {
        cols: ws.ws_col,
        rows: ws.ws_row,
        width_px: ws.ws_xpixel,
        height_px: ws.ws_ypixel,
    })
}

/// Put a descriptor into non blocking mode.
pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: `F_GETFL` takes no argument and touches no memory.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: `F_SETFL` takes an integer and touches no memory.
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// What a read produced.
///
/// End of file and "nothing right now" have to be told apart: `poll` reports a
/// hung up terminal as readable forever, so treating the resulting zero byte
/// read as "try again" spins at full speed with no way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    Data(usize),
    WouldBlock,
    Eof,
}

/// Read whatever is available.
pub fn read_available(fd: RawFd, buf: &mut [u8]) -> io::Result<ReadOutcome> {
    // SAFETY: writes at most `buf.len()` bytes into `buf`, which is borrowed
    // mutably for the call.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        let err = io::Error::last_os_error();
        return match err.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Ok(ReadOutcome::WouldBlock),
            // A terminal whose other end has gone reports EIO, not end of file.
            _ if err.raw_os_error() == Some(libc::EIO) => Ok(ReadOutcome::Eof),
            _ => Err(err),
        };
    }
    if n == 0 {
        return Ok(ReadOutcome::Eof);
    }
    Ok(ReadOutcome::Data(n as usize))
}

/// Wait until any of `fds` is readable, or until the timeout expires.
pub fn poll_readable(fds: &[RawFd], timeout_ms: i32) -> io::Result<Vec<RawFd>> {
    let mut poll_fds: Vec<libc::pollfd> = fds
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    if poll_fds.is_empty() {
        return Ok(Vec::new());
    }
    // SAFETY: the pointer and the count are the vector's own, and `poll`
    // writes only the `revents` of those entries.
    let n = unsafe {
        libc::poll(
            poll_fds.as_mut_ptr(),
            poll_fds.len() as libc::nfds_t,
            timeout_ms,
        )
    };
    if n < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            return Ok(Vec::new());
        }
        return Err(err);
    }
    Ok(poll_fds
        .into_iter()
        .filter(|p| p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
        .map(|p| p.fd)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pipe, as its two ends.
    fn pipe() -> (RawFd, RawFd) {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: `pipe` writes two descriptors into a two-element array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        (fds[0], fds[1])
    }

    fn close(fd: RawFd) {
        // SAFETY: closes a descriptor this test opened and uses no more.
        unsafe {
            libc::close(fd);
        }
    }

    #[test]
    fn polling_nothing_returns_nothing() {
        assert!(poll_readable(&[], 0).unwrap().is_empty());
    }

    #[test]
    fn a_pipe_becomes_readable() {
        let (read_end, write_end) = pipe();
        assert!(poll_readable(&[read_end], 0).unwrap().is_empty());
        // SAFETY: reads one byte from a static one-byte string.
        unsafe {
            libc::write(write_end, b"x".as_ptr() as *const libc::c_void, 1);
        }
        assert_eq!(poll_readable(&[read_end], 100).unwrap(), vec![read_end]);

        let mut buf = [0u8; 4];
        assert_eq!(
            read_available(read_end, &mut buf).unwrap(),
            ReadOutcome::Data(1)
        );
        close(read_end);
        close(write_end);
    }

    #[test]
    fn a_closed_writer_reports_end_of_file() {
        // Distinct from "nothing to read": a hung up terminal must end the
        // loop rather than being polled forever.
        let (read_end, write_end) = pipe();
        close(write_end);
        let mut buf = [0u8; 4];
        assert_eq!(
            read_available(read_end, &mut buf).unwrap(),
            ReadOutcome::Eof
        );
        close(read_end);
    }

    #[test]
    fn non_blocking_reads_return_immediately() {
        let (read_end, write_end) = pipe();
        set_nonblocking(read_end).unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(
            read_available(read_end, &mut buf).unwrap(),
            ReadOutcome::WouldBlock
        );
        close(read_end);
        close(write_end);
    }
}
