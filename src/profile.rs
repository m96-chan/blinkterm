//! Where the engine keeps cookies, logins and site data, and who may use it.
//!
//! A browser that forgets every login when it is closed is a browser people
//! stop using, and until this module that is what `blinkterm` was: the engine
//! was started with no `--user-data-dir`, so each run began signed out of
//! everything. The fix looks like one flag and is not, because of three things
//! that were measured against Chromium 141.0.7390.37, both the headless shell
//! and the full `chrome --headless`.
//!
//! # Without a directory, the answer depends on the engine
//!
//! Given no `--user-data-dir`, the headless shell keeps its browser context in
//! memory and writes nothing to disk. Full Chromium in headless mode makes
//! `/tmp/.org.chromium.Chromium.XXXXXX` — a whole profile — and does *not*
//! remove it: not after `Browser.close`, not after `SIGTERM`, not after
//! `SIGKILL`. Five runs left five directories. So this program always passes a
//! directory, and when the person wants nothing kept it makes a temporary one
//! itself with `mkdtemp(3)` and removes it itself, whichever engine it is.
//!
//! # Only `Browser.close` writes the cookie jar
//!
//! The headless shell honours `--user-data-dir`, creates it with its parents
//! at 0700, and writes `Default/Cookies`, `Local Storage` and
//! `DevToolsActivePort` into it. A cookie was set with `Network.setCookie`,
//! the engine was stopped one way or another, a second engine was started on
//! the same directory, and `Network.getCookies` asked whether it was there:
//!
//! | how the first engine stopped                          | the cookie |
//! | ----------------------------------------------------- | ---------- |
//! | `SIGKILL` to the group at once                        | lost       |
//! | `SIGKILL` three seconds after the cookie was set      | lost       |
//! | `SIGTERM` to the group (exit 143, gone in 1.9 s)      | lost       |
//! | `Browser.close`, then `SIGKILL` at once               | lost       |
//! | `Browser.close`, then waited for (exit 0, gone 1.9 s) | **kept**   |
//! | `SIGKILL` 35 seconds after the cookie was set         | kept       |
//!
//! The last row is Chromium's periodic flush, about every thirty seconds, and
//! is why a run that is killed outright loses the last half a minute rather
//! than everything. Full Chromium is the same: `Browser.close` keeps the
//! cookie and is gone in 1.8 s, and `SIGTERM` loses it — while exiting 0, and
//! leaving a `SingletonLock` behind that names a process that no longer
//! exists. So "asked to stop" means one thing here, `Browser.close` over the
//! debugging connection and then waiting for the process to finish, and that
//! is what [`crate::app::run`] does on the way out. [`crate::engine::Engine`]'s
//! own kill is the fallback for when that did not work, and is not a way to
//! keep anything.
//!
//! # The lock is this program's, because the engine's is not a lock
//!
//! Two engines on one profile share one `Cookies` database, and SQLite shared
//! by two processes that each think they own it is how a cookie jar gets
//! corrupted. Chromium's defence is `SingletonLock`, and it does not help here:
//! the headless shell has no `ProcessSingleton` at all — `strings` finds
//! `SingletonLock` only in the full browser — and a second headless shell on
//! the same directory starts and runs alongside the first, overwriting its
//! `DevToolsActivePort` and logging leveldb `LOCK` warnings. Full Chromium does
//! refuse, exiting 21 within 80 ms, but it silently takes over a stale lock
//! from the same host and exits 0 *without starting* when the lock names
//! another host. Neither is something to build on.
//!
//! So the lock is `flock(2)` on `blinkterm.lock` inside the profile, taken
//! before the engine is started and held by this process until it exits. A
//! pid file would be stale after every abort and would need the same
//! is-that-pid-still-us guessing `SingletonLock` does. An `flock` is taken
//! atomically, needs no cleaning up, and is released by the kernel however the
//! process ends — a quit, a `SIGTERM`, a `panic = "abort"`, a `SIGKILL`. It
//! belongs to the open file description rather than to the process, so a
//! second `open` of the same file in the same process is refused too, which is
//! what lets it be tested without a second process. `fcntl` locks were the
//! other choice and were rejected for the opposite property: they vanish when
//! *any* descriptor this process has on the file is closed. The standard
//! library opens files `O_CLOEXEC`, so the engine does not inherit the lock
//! and cannot keep it past this program.
//!
//! A second `blinkterm` on a profile that is in use fails, and says which pid
//! holds it and what to do instead. Falling back to a temporary profile
//! quietly would be friendlier for a second and then cost somebody a login
//! they thought was being kept: a note on the status row is exactly the kind
//! of thing nobody reads.

use std::ffi::OsStr;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The file inside a profile that the lock is taken on.
pub const LOCK_FILE: &str = "blinkterm.lock";

/// How long a temporary profile's removal keeps trying, and how often.
///
/// A group that has just been sent `SIGKILL` is not necessarily finished
/// writing: the signal is delivered, the processes are reaped by init, and for
/// a few milliseconds in between a renderer can still be creating a file in a
/// directory that `remove_dir_all` has already emptied, which then fails with
/// "directory not empty". A second try finds the stragglers. A second is far
/// more than that race needs and short enough that a quit is never held up by
/// it noticeably.
const REMOVE_FOR: Duration = Duration::from_secs(1);
const REMOVE_EVERY: Duration = Duration::from_millis(20);

/// The temporary profile this process made, for the paths that cannot run a
/// destructor.
///
/// The release profile sets `panic = "abort"`, so a panic does not drop the
/// [`Profile`] and the directory would outlive the program. The panic hook
/// calls [`remove_temp_profile`], which reads this, for the same reason it
/// calls [`crate::engine::kill_engine`] rather than relying on `Drop`.
static TEMP_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Which profile the person asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// `$XDG_DATA_HOME/blinkterm/profile`, or `~/.local/share/...` without it.
    Default,
    /// `--profile <dir>`.
    At(PathBuf),
    /// `--temp-profile`: nothing kept, nothing left behind.
    Temporary,
}

/// A profile directory this program has the use of until the value is dropped.
///
/// For a kept profile that means the lock; for a temporary one it means the
/// directory itself, which is removed when this goes.
#[derive(Debug)]
pub struct Profile {
    dir: PathBuf,
    /// The open lock file. It is never read after it is taken: holding it open
    /// is what holds the lock, and the kernel drops the lock with the last
    /// descriptor, which is this one.
    #[allow(dead_code)]
    lock: Option<File>,
    temporary: bool,
}

impl Profile {
    /// Resolve `choice` to a directory, make it if it is not there, and take
    /// it for this process.
    pub fn take(choice: Choice) -> Result<Profile, String> {
        let dir = match choice {
            Choice::Temporary => return Profile::temporary(),
            Choice::Default => Profile::default_dir()?,
            Choice::At(dir) if dir.is_absolute() => dir,
            Choice::At(dir) => std::env::current_dir()
                .map_err(|e| format!("cannot tell where {} is: {e}", dir.display()))?
                .join(dir),
        };
        make_private_dir(&dir)?;
        let lock = lock(&dir)?;
        Ok(Profile {
            dir,
            lock: Some(lock),
            temporary: false,
        })
    }

    /// Where the profile goes when nobody says, from this process's
    /// environment.
    pub fn default_dir() -> Result<PathBuf, String> {
        Profile::resolve(
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
    }

    /// Where the profile goes, given the two variables that decide it.
    ///
    /// The XDG base directory specification says a relative `$XDG_DATA_HOME`
    /// is invalid and is to be ignored, and an empty one is the same as none;
    /// both then fall back to `$HOME/.local/share`, which is the spec's
    /// default. With neither there is no answer that is not a guess, and a
    /// profile in a guessed place is a login kept somewhere nobody will look,
    /// so it is an error that names the two ways round it.
    pub fn resolve(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Result<PathBuf, String> {
        let usable = |value: Option<&OsStr>| {
            value
                .map(Path::new)
                .filter(|path| path.is_absolute())
                .map(Path::to_path_buf)
        };
        if let Some(data) = usable(xdg) {
            return Ok(data.join("blinkterm").join("profile"));
        }
        if let Some(home) = usable(home) {
            return Ok(home
                .join(".local")
                .join("share")
                .join("blinkterm")
                .join("profile"));
        }
        Err(
            "no $XDG_DATA_HOME and no $HOME, so nowhere to keep a profile; \
             use --profile <dir> or --temp-profile"
                .to_string(),
        )
    }

    /// A new, empty profile under the system's temporary directory, removed
    /// when this is dropped.
    ///
    /// `mkdtemp(3)` rather than a name picked here: it creates the directory
    /// and picks the name in one step, so no other process can have made it
    /// first, and it creates it 0700 by contract. The name carries this
    /// process's pid so that the next run can tell a directory left by an
    /// abort from one that belongs to a `blinkterm` still running — which is
    /// what [`sweep`] does first, before making a new one.
    ///
    /// There is no lock: nobody else knows the name.
    pub fn temporary() -> Result<Profile, String> {
        let parent = std::env::temp_dir();
        sweep(&parent);
        let template = parent.join(format!("blinkterm-{}-XXXXXX", std::process::id()));
        let mut bytes = template.as_os_str().as_bytes().to_vec();
        bytes.push(0);
        // SAFETY: `bytes` is a NUL-terminated buffer this function owns, and
        // `mkdtemp(3)` only rewrites the six `X`s before the NUL in place,
        // never past it. The buffer outlives the call.
        let made = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
        if made.is_null() {
            return Err(format!(
                "cannot make a temporary profile in {}: {}",
                parent.display(),
                std::io::Error::last_os_error()
            ));
        }
        bytes.pop();
        let dir = PathBuf::from(OsStr::from_bytes(&bytes));
        if let Ok(mut recorded) = TEMP_DIR.lock() {
            *recorded = Some(dir.clone());
        }
        Ok(Profile {
            dir,
            lock: None,
            temporary: true,
        })
    }

    /// The directory, for `--user-data-dir`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether this is thrown away at the end: there is nothing in it worth
    /// asking the engine to flush.
    pub fn is_temporary(&self) -> bool {
        self.temporary
    }

    /// Remove a temporary profile. A kept one is left alone; this does
    /// nothing to it, and doing it twice does nothing the second time.
    ///
    /// Meant to be called once the engine is gone — [`crate::engine::Engine`]
    /// does, at the end of its kill — and retried for up to a second because
    /// "gone" can still mean a few milliseconds of writes landing.
    pub fn remove(&mut self) {
        if !self.temporary {
            return;
        }
        remove_tree(&self.dir);
        if let Ok(mut recorded) = TEMP_DIR.lock() {
            if recorded.as_deref() == Some(self.dir.as_path()) {
                *recorded = None;
            }
        }
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        if self.temporary {
            self.remove();
        }
    }
}

/// Remove this process's temporary profile, from a place that owns nothing.
///
/// For the panic hook, after [`crate::engine::kill_engine`]: under
/// `panic = "abort"` no destructor runs, so without this every panic with
/// `--temp-profile` would leave a profile in `/tmp` — until the next run's
/// [`sweep`] found it. `try_lock` rather than `lock`, because the panic could
/// have happened with the lock held, and a hook that deadlocks is worse than a
/// directory left for the sweep.
pub fn remove_temp_profile() {
    let dir = match TEMP_DIR.try_lock() {
        Ok(mut recorded) => recorded.take(),
        Err(_) => None,
    };
    if let Some(dir) = dir {
        remove_tree(&dir);
    }
}

/// Remove the temporary profiles in `parent` whose process is gone.
///
/// A temporary profile is removed on every exit this program gets to see,
/// including a panic; the one it does not see is a `SIGKILL`, and that leaves
/// a directory named for a pid that no longer exists. Those are recognised by
/// name — `blinkterm-<pid>-` and the six characters `mkdtemp` chose — and by
/// `kill(pid, 0)` answering `ESRCH`. Anything else is left: a pid that is
/// alive may be another `blinkterm` using its profile right now, and `EPERM`
/// is a process that exists and belongs to somebody else.
pub fn sweep(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = temporary_pid(&name.to_string_lossy()) else {
            continue;
        };
        if pid == std::process::id() as i32 || process_exists(pid) {
            continue;
        }
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            remove_tree(&entry.path());
        }
    }
}

/// The pid in a temporary profile's name, if `name` is one.
fn temporary_pid(name: &str) -> Option<i32> {
    let rest = name.strip_prefix("blinkterm-")?;
    let (pid, suffix) = rest.split_once('-')?;
    if suffix.len() != 6 || pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok().filter(|&pid: &i32| pid > 0)
}

/// Whether there is a process with this pid, as far as `kill(2)` can tell.
fn process_exists(pid: i32) -> bool {
    // SAFETY: signal 0 sends nothing and only asks whether it could, and
    // `kill(2)` reads no memory. `pid` is positive — `temporary_pid` filters
    // out everything else — so this is one process and never a group.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// `remove_dir_all`, again and again for [`REMOVE_FOR`] while it fails and
/// the directory is still there.
fn remove_tree(dir: &Path) {
    let deadline = Instant::now() + REMOVE_FOR;
    loop {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if Instant::now() >= deadline => return,
            Err(_) => std::thread::sleep(REMOVE_EVERY),
        }
    }
}

/// Make `dir` and its parents 0700, or say why not.
///
/// A profile is a cookie jar, and a cookie is a login, so nobody else on the
/// machine gets to read it: the directories this makes are 0700, the same as
/// the engine makes its own. One that already exists keeps the mode it has —
/// the person may have chosen it — and one that exists and is not a directory
/// is an error that names it, rather than whatever the engine would make of a
/// `--user-data-dir` that is a file.
fn make_private_dir(dir: &Path) -> Result<(), String> {
    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => return Ok(()),
        Ok(_) => {
            return Err(format!(
                "{} is not a directory, so it cannot be a profile",
                dir.display()
            ))
        }
        Err(_) => {}
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("cannot make the profile {}: {e}", dir.display()))
}

/// Take the lock on the profile at `dir`, and write this process's pid into it
/// for the next one to read; or say who has it.
///
/// The file is opened without truncating: until the lock is taken, what is in
/// it is the holder's pid, and that is what the refusal quotes.
fn lock(dir: &Path) -> Result<File, String> {
    let path = dir.join(LOCK_FILE);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    // SAFETY: `flock(2)` takes a descriptor and a flag word and reads no
    // memory. The descriptor is `file`'s, which is open for the whole call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(format!("cannot lock {}: {error}", path.display()));
        }
        let mut holder = String::new();
        let _ = file.read_to_string(&mut holder);
        let holder = match holder.trim().parse::<u32>() {
            Ok(pid) => format!(" (pid {pid})"),
            Err(_) => String::new(),
        };
        return Err(format!(
            "the profile at {} is in use by another blinkterm{holder}; quit it, \
             or run this one with --temp-profile or --profile <dir>",
            dir.display()
        ));
    }
    // The pid is a courtesy for the message above, not part of the lock, so a
    // failure to write it is not a failure to take the profile.
    let _ = file
        .set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| writeln!(file, "{}", std::process::id()))
        .and_then(|()| file.flush());
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A scratch directory of this test's own, gone again before and after.
    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blinkterm-unit-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path)
            .expect("the path exists")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn an_absolute_xdg_data_home_is_where_the_profile_goes() {
        assert_eq!(
            Profile::resolve(Some(OsStr::new("/data")), Some(OsStr::new("/home/a"))),
            Ok(PathBuf::from("/data/blinkterm/profile"))
        );
    }

    #[test]
    fn a_relative_or_empty_xdg_data_home_is_ignored_as_the_spec_says() {
        for xdg in ["data", "", "./x"] {
            assert_eq!(
                Profile::resolve(Some(OsStr::new(xdg)), Some(OsStr::new("/home/a"))),
                Ok(PathBuf::from("/home/a/.local/share/blinkterm/profile")),
                "XDG_DATA_HOME={xdg:?}"
            );
        }
    }

    #[test]
    fn without_xdg_data_home_the_profile_is_under_home() {
        assert_eq!(
            Profile::resolve(None, Some(OsStr::new("/home/a"))),
            Ok(PathBuf::from("/home/a/.local/share/blinkterm/profile"))
        );
    }

    #[test]
    fn with_neither_variable_the_error_names_both_ways_round_it() {
        let why = Profile::resolve(None, None).unwrap_err();
        assert!(why.contains("--profile"), "{why}");
        assert!(why.contains("--temp-profile"), "{why}");
        let why = Profile::resolve(Some(OsStr::new("rel")), Some(OsStr::new(""))).unwrap_err();
        assert!(why.contains("$HOME"), "{why}");
    }

    #[test]
    fn a_profile_that_is_taken_is_made_private_and_says_who_took_it() {
        let root = scratch("take");
        let dir = root.join("deep").join("profile");
        let profile = Profile::take(Choice::At(dir.clone())).expect("the profile is taken");
        assert_eq!(profile.dir(), dir);
        assert!(!profile.is_temporary());
        assert_eq!(mode(&dir), 0o700, "a cookie jar nobody else may read");
        let written = std::fs::read_to_string(dir.join(LOCK_FILE)).expect("the lock file");
        assert_eq!(written.trim(), std::process::id().to_string());

        drop(profile);
        assert!(dir.exists(), "a kept profile is kept");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_profile_that_is_in_use_is_refused_until_it_is_let_go() {
        let dir = scratch("twice");
        let first = Profile::take(Choice::At(dir.clone())).expect("the first takes it");
        // The same process, a second open: `flock` belongs to the open file
        // description, so this is refused exactly as a second program would be.
        let why = Profile::take(Choice::At(dir.clone())).unwrap_err();
        assert!(why.contains(&dir.display().to_string()), "{why}");
        assert!(
            why.contains(&format!("pid {}", std::process::id())),
            "{why}"
        );
        assert!(why.contains("--temp-profile"), "{why}");

        drop(first);
        let again = Profile::take(Choice::At(dir.clone())).expect("free once let go");
        drop(again);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_where_the_profile_should_be_is_an_error_that_names_it() {
        let root = scratch("file");
        let file = root.join("profile");
        std::fs::write(&file, b"not a directory").expect("a file");
        let why = Profile::take(Choice::At(file.clone())).unwrap_err();
        assert!(why.contains(&file.display().to_string()), "{why}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_temporary_profile_is_private_named_for_us_and_gone_when_dropped() {
        let profile = Profile::temporary().expect("a temporary profile");
        let dir = profile.dir().to_path_buf();
        assert!(profile.is_temporary());
        assert!(dir.starts_with(std::env::temp_dir()), "{}", dir.display());
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            temporary_pid(&name),
            Some(std::process::id() as i32),
            "{name}"
        );
        assert_eq!(mode(&dir), 0o700);
        std::fs::create_dir_all(dir.join("Default")).expect("something in it");
        std::fs::write(dir.join("Default").join("Cookies"), b"x").expect("a file in it");

        drop(profile);
        assert!(!dir.exists(), "{} is still there", dir.display());
    }

    #[test]
    fn the_sweep_takes_what_a_dead_process_left_and_leaves_ours() {
        let parent = scratch("sweep");
        // A pid that was certainly ours to see die: a child that has exited
        // and been waited for.
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("true runs");
        let dead = child.id();
        child.wait().expect("true is reaped");
        let left = parent.join(format!("blinkterm-{dead}-abcdef"));
        let ours = parent.join(format!("blinkterm-{}-ghijkl", std::process::id()));
        let unrelated = parent.join(format!("blinkterm-it-x-{dead}"));
        for dir in [&left, &ours, &unrelated] {
            std::fs::create_dir_all(dir.join("Default")).expect("a directory");
        }

        sweep(&parent);
        assert!(!left.exists(), "a dead process's profile stayed");
        assert!(ours.exists(), "a live process's profile went");
        assert!(unrelated.exists(), "a name that is not ours went");
        std::fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn only_the_names_mkdtemp_makes_are_read_as_temporary_profiles() {
        assert_eq!(temporary_pid("blinkterm-123-a1B2c3"), Some(123));
        assert_eq!(temporary_pid("blinkterm-123-short"), None);
        assert_eq!(temporary_pid("blinkterm-it-frames-123"), None);
        assert_eq!(temporary_pid("blinkterm--abcdef"), None);
        assert_eq!(temporary_pid("blinkterm-0-abcdef"), None);
        assert_eq!(temporary_pid("other-123-abcdef"), None);
    }
}
