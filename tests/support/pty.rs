//! A pseudoterminal for a test to run the real program in: `openpty(3)`,
//! the child in a session of its own with the pty as its controlling
//! terminal — what a terminal emulator gives a shell — and the master end
//! read and written here.
//!
//! The pty is sized with `TIOCSWINSZ`, pixels included, so that a program
//! that measures its cells from the window size (`--no-probe` does) gets a
//! cell size without asking.
//!
//! The master end is read as soon as there is something on it, with
//! `poll(2)`, not on a timer. A pty's buffer is small, and on macOS
//! smaller: XNU stops a writer on the slave end at a high-water mark of
//! about 1.2 KB and wakes it only once the master end has been drained
//! below it. A reader that slept 5 ms whenever the master was empty so let
//! through about 1.2 KB per sleep — one inline raw frame, some 470 KB of
//! base64 at 15 frames a second, took one to two seconds to come down, and
//! everything the program wrote after it waited behind it (#114). Linux's
//! buffer is large enough that the sleep never showed.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// A child on a pseudoterminal, and the master end of it.
pub struct Pty {
    /// `None` once the terminal has hung up ([`Pty::hang_up`]).
    master: Option<File>,
    pub child: Child,
    /// The child's end has closed: everything it wrote has been read.
    pub ended: bool,
}

/// Mark `fd` close-on-exec, so that no other child — the program's own
/// backend above all — inherits this pty.
fn cloexec(fd: libc::c_int) {
    // SAFETY: `fcntl(2)` with `F_SETFD` reads no memory; `fd` is open, made
    // by `openpty` just before.
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
}

impl Pty {
    /// Run `command` on a fresh pty of `cols` by `rows` cells, each `cell`
    /// pixels. Its standard input and output are the pty; its standard error
    /// is whatever `command` already says, so that a test can keep it apart
    /// from the screen.
    pub fn spawn(mut command: Command, cols: u16, rows: u16, cell: (u16, u16)) -> Pty {
        let mut size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: cols * cell.0,
            ws_ypixel: rows * cell.1,
        };
        let (mut master, mut slave): (libc::c_int, libc::c_int) = (-1, -1);
        // SAFETY: `openpty(3)` writes one descriptor through each of the
        // first two pointers, both live locals; the name is not asked for
        // (null), the termios is the default (null), and the window size is
        // read from a live local for the length of the call.
        let r = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::addr_of_mut!(size),
            )
        };
        assert_eq!(r, 0, "openpty: {}", std::io::Error::last_os_error());
        cloexec(master);
        cloexec(slave);
        // SAFETY: both descriptors were just made by `openpty` and belong to
        // nothing else; each is taken over once.
        let (master, slave) = unsafe { (File::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        let stdin = slave.try_clone().expect("the pty again");
        let stdout = slave.try_clone().expect("the pty again");
        command
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout));
        // SAFETY: the closure runs in the child between `fork` and `exec`,
        // where only async-signal-safe calls are allowed; `setsid(2)`,
        // `ioctl(2)` and reading `errno` are. Descriptor 0 is the pty's slave
        // end by then, which `TIOCSCTTY` makes the new session's terminal.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("the program starts");
        drop(slave);
        // SAFETY: `F_SETFL` with `O_NONBLOCK` reads no memory; the
        // descriptor is `master`'s, open for the whole call.
        unsafe {
            let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Pty {
            master: Some(master),
            child,
            ended: false,
        }
    }

    /// Whatever the child wrote within `within`: as soon as something has
    /// arrived and the line has gone quiet for a moment, or at the deadline.
    /// Empty once the child's end has closed.
    pub fn read(&mut self, within: Duration) -> Vec<u8> {
        let deadline = Instant::now() + within;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 1 << 16];
        while !self.ended {
            let Some(master) = self.master.as_mut() else {
                self.ended = true;
                break;
            };
            match master.read(&mut buf) {
                Ok(0) => self.ended = true,
                Ok(n) => {
                    out.extend_from_slice(&buf[..n]);
                    continue;
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                // Linux says EIO once every slave descriptor has closed.
                Err(_) => self.ended = true,
            }
            let now = Instant::now();
            if self.ended || !out.is_empty() || now >= deadline {
                break;
            }
            let left = deadline - now;
            self.wait_readable(left.min(Duration::from_millis(100)));
        }
        out
    }

    /// Wait up to `within` for the master end to have something to read,
    /// or to have hung up; either way the next read says which.
    fn wait_readable(&mut self, within: Duration) {
        let Some(master) = self.master.as_ref() else {
            return;
        };
        let mut fds = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = within.as_millis().clamp(1, 100) as libc::c_int;
        // SAFETY: `poll(2)` reads and writes the one `pollfd` it is given, a
        // live local, for the length of the call; the descriptor is
        // `master`'s, open for the whole call.
        let r = unsafe { libc::poll(&mut fds, 1, ms) };
        if r < 0 {
            if std::io::Error::last_os_error().kind() != ErrorKind::Interrupted {
                self.ended = true;
            }
        } else if fds.revents & libc::POLLNVAL != 0 {
            // Not a descriptor poll will watch: fall back to a short sleep
            // rather than spin.
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Type `bytes`.
    pub fn write(&mut self, bytes: &[u8]) {
        let mut left = bytes;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !left.is_empty() && Instant::now() < deadline {
            let Some(master) = self.master.as_mut() else {
                return;
            };
            match master.write(left) {
                Ok(n) => left = &left[n..],
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }

    /// Close the master end, as a terminal emulator does when its window is
    /// closed: the kernel hangs up on the child's session.
    pub fn hang_up(&mut self) {
        self.master = None;
        self.ended = true;
    }

    /// The child's exit, waiting up to `within`.
    pub fn exit_within(&mut self, within: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + within;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
