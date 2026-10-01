//! `blinkterm --install-engine`: fetch the `chrome-headless-shell` the tests
//! pass against, check it against a checksum compiled into this program,
//! unpack it where the search will find it, start it once, and say where it
//! is and how to remove it.
//!
//! # Explicit, never implicit
//!
//! This is the only place the program itself reaches the network. It runs
//! when somebody types `--install-engine` on a command line and at no other
//! time: there is no settings key for it, no key binding, nothing a page can
//! ask for, and a run that finds no engine says that this switch exists
//! rather than using it. A settings file that downloaded 100 MB on every run
//! would be the opposite of what SECURITY.md promises — nothing leaves the
//! machine unless the person asked.
//!
//! # Pinned, not latest
//!
//! [`SHELL_VERSION`] is the version CI installs and the engine suite
//! (`tests/engine.rs`) passes against, and nothing else is fetched. "Latest"
//! would be whatever Chrome for Testing published this morning, which no test
//! has seen; and it would need a JSON index fetched and trusted first. What
//! is installed is what was tested, so the search looks only at the pinned
//! version's directory: a blinkterm upgraded past it stops finding the old
//! one and its error names this switch, which fetches the new one beside it.
//!
//! # The checksum is compiled in
//!
//! [`SHELLS`] carries a SHA-256 per platform, and the zip is checked against
//! it before anything is unpacked. A checksum fetched from the same place as
//! the zip would only prove the download finished; one in the binary proves
//! it is the bytes the maintainer hashed. `linux64` and `mac-arm64` are the
//! two `SHELL_SHA256` values in `.github/workflows/ci.yml`, which CI checks
//! on every run; `mac-x64`'s was computed on 2026-10-01 from
//! `curl -fsSL <url> | shasum -a 256` against the same version, since no CI
//! job runs an Intel Mac. A unit test reads CI's workflow, `README.md`,
//! `docs/install.md` and `--help` and fails when any of them disagrees with
//! the pin here, so bumping the engine is one edit in five places that the
//! build will not let drift.
//!
//! # `curl` and `unzip`, and a hash of our own
//!
//! The download is `curl`, https only and redirects to https only, the same
//! command both CI jobs use; it honours `https_proxy` itself. The unpacking
//! is `unzip`, also what CI uses. Both are there on a Mac, and one `apt
//! install` away on Linux; both are looked for before anything is fetched,
//! so nobody waits for 100 MB to hear that `unzip` is missing. The zip is
//! only ever handed to `unzip` once its SHA-256 equals the compiled-in one,
//! so a hostile archive (a `../` path, a link out of the directory) would
//! first have to be the archive the maintainer hashed. The hash itself is
//! [`crate::sha256`], in the crate, because `shasum` and `sha256sum` are
//! different programs on the two platforms and the one that decides whether
//! downloaded code runs is not one to borrow.
//!
//! # Staged, then renamed
//!
//! Everything happens in `<data>/engine/.staging-<pid>/`: the zip, the
//! unpacked tree, the `chmod`. Only a whole, checked tree is renamed to
//! `<data>/engine/<version>/`, and `rename(2)` within one directory is atomic,
//! so the search never sees half an engine. A second `--install-engine` at
//! the same time loses the rename (`ENOTEMPTY`), removes its own staging and
//! reports the winner's engine. A download interrupted by `ctrl+c` leaves its
//! staging directory behind; the next run removes every one whose pid is no
//! longer a process, before it fetches anything. `<data>` is
//! [`crate::profile::data_dir`], beside the profile and the bookmarks, and
//! `engine/` is made 0700 like them.
//!
//! # Where it sits in the search
//!
//! Any engine somebody named — `--engine`, `$BLINKTERM_ENGINE`, `engine =` —
//! still wins. After those the installed engine comes first, before `PATH`
//! (see [`crate::engine::CANDIDATES`]): installing it was an explicit act, and
//! a `chromium-shell` some package pulled in must not beat it.
//!
//! # Started, not `ldd`-ed
//!
//! As `--doctor` says of itself, the engine is started, not found: after the
//! rename it is launched once on a temporary profile, exactly as a run would
//! launch it, by the same code as `--doctor`. A Linux without `libnss3.so`
//! shows up as the engine's own sentence about it, with a pointer to the
//! `deb.deps` file in the unpacked directory that lists the packages; a Mac
//! has no `ldd` to run anyway. The files are kept when it does not answer:
//! the download was good, and the machine is what is missing.
//!
//! # Not done here
//!
//! Removing an engine (the report prints the `rm -r` line, and never deletes
//! an older version itself); a version other than the pin; Linux on arm64,
//! for which Chrome for Testing publishes no build; fonts, which a page of
//! Japanese needs and a package manager provides.

use std::fs::DirBuilder;
use std::io::IsTerminal;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::doctor::{more, say};
use crate::engine;
use crate::options::Options;
use crate::profile::{self, Profile};
use crate::sha256;
use crate::text::sanitize;

/// The `chrome-headless-shell` version `--install-engine` fetches: the one
/// CI installs and the engine suite passes against.
pub const SHELL_VERSION: &str = "153.0.8010.52";

/// One platform's build of [`SHELL_VERSION`]: its name in Chrome for
/// Testing's terms, and the SHA-256 of its zip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shell {
    pub platform: &'static str,
    pub sha256: &'static str,
}

/// Every platform `--install-engine` knows, with the checksum its zip must
/// have. See the module doc for where each came from.
pub const SHELLS: [Shell; 3] = [
    Shell {
        platform: "linux64",
        sha256: "944dc1eae654637fed4d57650198774f9c43b45f34e48febb84f43c541b5de76",
    },
    Shell {
        platform: "mac-arm64",
        sha256: "47fe02eae3a1b6e9ba298c7a8aea4ca1be87741ef8fa2da55b8a6f76d636e894",
    },
    Shell {
        platform: "mac-x64",
        sha256: "0cb6d585712b6e2b584c3c31fbecba2fc6a734038c3ebe12d9f754abe7b04aba",
    },
];

/// This build's platform in Chrome for Testing's terms, or why there is
/// none.
pub fn platform() -> Result<&'static str, String> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok("linux64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("mac-arm64")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok("mac-x64")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Err(
            "Chrome for Testing publishes no chrome-headless-shell for Linux on arm64; \
             install your distribution's chromium and blinkterm finds it on PATH"
                .to_string(),
        )
    } else {
        Err(format!(
            "Chrome for Testing publishes no chrome-headless-shell for {} on {}; \
             install a chromium and set {} to it",
            std::env::consts::OS,
            std::env::consts::ARCH,
            engine::ENGINE_ENV
        ))
    }
}

/// The [`Shell`] for `platform`, if there is one.
pub fn shell_for(platform: &str) -> Option<Shell> {
    SHELLS.into_iter().find(|shell| shell.platform == platform)
}

/// Where Chrome for Testing keeps `version`'s zip for `platform`.
pub fn url(version: &str, platform: &str) -> String {
    format!(
        "https://storage.googleapis.com/chrome-for-testing-public/{version}/{platform}/chrome-headless-shell-{platform}.zip"
    )
}

/// `<data>/engine`, where every installed version is.
pub fn engines_dir(data: &Path) -> PathBuf {
    data.join("engine")
}

/// `<data>/engine/<version>`, one version's unpacked zip.
pub fn version_dir(data: &Path, version: &str) -> PathBuf {
    engines_dir(data).join(version)
}

/// The executable inside [`version_dir`]: the zip's one top directory is
/// kept, because the binary must stay beside its `.pak` files and ICU data.
pub fn executable_in(data: &Path, version: &str, platform: &str) -> PathBuf {
    version_dir(data, version)
        .join(format!("chrome-headless-shell-{platform}"))
        .join("chrome-headless-shell")
}

/// The pinned engine, if `--install-engine` has put it in this person's
/// data directory and it is executable. Read by the engine search.
pub fn installed_engine() -> Option<PathBuf> {
    installed_engine_in(&Profile::data_dir().ok()?)
}

/// [`installed_engine`] under `data` rather than the environment's.
pub fn installed_engine_in(data: &Path) -> Option<PathBuf> {
    let executable = executable_in(data, SHELL_VERSION, platform().ok()?);
    engine::is_executable(&executable).then_some(executable)
}

/// What [`install_with`] left in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The engine's executable.
    pub executable: PathBuf,
    /// The version directory, which removing removes the engine.
    pub dir: PathBuf,
    /// The zip's size, 0 when nothing was fetched.
    pub bytes: u64,
    /// False when it was there already and nothing was fetched.
    pub fresh: bool,
}

/// Everything but the network: stage, fetch with `fetch` into the path it
/// is given, verify against `shell.sha256`, unpack, make executable, rename
/// into place. A version already in place is returned without calling
/// `fetch`. On any error nothing is left behind but what was there before.
pub fn install_with(
    data: &Path,
    shell: &Shell,
    version: &str,
    fetch: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<Installed, String> {
    let executable = executable_in(data, version, shell.platform);
    let dir = version_dir(data, version);
    let already = |executable: PathBuf, dir: PathBuf| Installed {
        executable,
        dir,
        bytes: 0,
        fresh: false,
    };
    if engine::is_executable(&executable) {
        return Ok(already(executable, dir));
    }
    let engines = engines_dir(data);
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&engines)
        .map_err(|e| format!("cannot make {}: {e}", engines.display()))?;
    let staging = engines.join(format!(".staging-{}", std::process::id()));
    profile::remove_tree(&staging);
    DirBuilder::new()
        .mode(0o700)
        .create(&staging)
        .map_err(|e| format!("cannot make {}: {e}", staging.display()))?;

    let staged = stage(&staging, shell, version, fetch);
    let (unpacked, bytes) = match staged {
        Ok(staged) => staged,
        Err(why) => {
            profile::remove_tree(&staging);
            return Err(why);
        }
    };
    let renamed = std::fs::rename(&unpacked, &dir);
    profile::remove_tree(&staging);
    match renamed {
        Ok(()) => Ok(Installed {
            executable,
            dir,
            bytes,
            fresh: true,
        }),
        // Another --install-engine renamed its tree into place first.
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) => {
            if engine::is_executable(&executable) {
                Ok(already(executable, dir))
            } else {
                Err(format!(
                    "{} is there already and holds no engine; remove it and run again",
                    dir.display()
                ))
            }
        }
        Err(e) => Err(format!(
            "cannot move the engine to {}: {e}; nothing installed",
            dir.display()
        )),
    }
}

/// The part of [`install_with`] that happens inside `staging`: the unpacked
/// tree, ready to rename, and the zip's size.
fn stage(
    staging: &Path,
    shell: &Shell,
    version: &str,
    fetch: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(PathBuf, u64), String> {
    let link = url(version, shell.platform);
    let zip = staging.join("shell.zip");
    fetch(&zip)?;
    let bytes = std::fs::metadata(&zip)
        .map_err(|e| format!("{link} left no zip at {}: {e}", zip.display()))?
        .len();
    let got = sha256::of_file(&zip).map_err(|e| format!("cannot read {}: {e}", zip.display()))?;
    if !got.eq_ignore_ascii_case(shell.sha256) {
        return Err(format!(
            "checksum mismatch for {link}: expected {}, got {got}; nothing installed",
            shell.sha256
        ));
    }
    let unpacked = staging.join("unpacked");
    let out = Command::new("unzip")
        .arg("-q")
        .arg(&zip)
        .arg("-d")
        .arg(&unpacked)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot run unzip: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "unzip failed on {} ({}): {}; nothing installed",
            zip.display(),
            out.status,
            sanitize(String::from_utf8_lossy(&out.stderr).trim())
        ));
    }
    let top = unpacked.join(format!("chrome-headless-shell-{}", shell.platform));
    let binary = top.join("chrome-headless-shell");
    if !binary.is_file() {
        return Err(format!(
            "the zip from {link} did not hold chrome-headless-shell-{}/chrome-headless-shell; \
             nothing installed",
            shell.platform
        ));
    }
    // `unzip` restores the mode the zip recorded, and Chrome for Testing's
    // records 0755; this makes that not matter.
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("cannot make {} executable: {e}", binary.display()))?;
    // `curl` sets no quarantine attribute, so there is normally nothing to
    // remove; a zip that came some other way might have one, and Gatekeeper
    // would then refuse the engine without saying so on its pipe.
    if cfg!(target_os = "macos") {
        let _ = Command::new("xattr")
            .args(["-dr", "com.apple.quarantine"])
            .arg(&top)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    Ok((unpacked, bytes))
}

/// Remove every `.staging-<pid>` under `engines` whose process is gone: what
/// an interrupted `--install-engine` left.
fn sweep_staging(engines: &Path) {
    let Ok(entries) = std::fs::read_dir(engines) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix(".staging-"))
            .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|pid| pid.parse::<i32>().ok())
            .filter(|&pid| pid > 0)
        else {
            continue;
        };
        if !profile::process_exists(pid) {
            profile::remove_tree(&entry.path());
        }
    }
}

/// The version directories under `<data>/engine` other than `version`'s,
/// sorted: what an earlier blinkterm installed, for the report to name.
pub fn other_versions(data: &Path, version: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(engines_dir(data)) else {
        return Vec::new();
    };
    let mut others: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && name != version
        })
        .map(|entry| entry.path())
        .collect();
    others.sort();
    others
}

/// `curl` fetching `link` into `to`: https only, redirects to https only,
/// its progress bar on stderr when that is a terminal. The only network
/// access this program makes of its own.
fn fetch(link: &str, to: &Path) -> Result<(), String> {
    let mut curl = Command::new("curl");
    curl.args([
        "--fail",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--tlsv1.2",
        "--connect-timeout",
        "30",
        "--retry",
        "2",
    ]);
    if std::io::stderr().is_terminal() {
        curl.args(["--progress-bar", "--show-error"]);
    } else {
        curl.args(["--silent", "--show-error"]);
    }
    let status = curl
        .arg("--output")
        .arg(to)
        .arg(link)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "curl exited {} fetching {link}; nothing installed",
            status
                .code()
                .map_or_else(|| "on a signal".to_string(), |code| code.to_string())
        ))
    }
}

/// Size in MB, one decimal, for the report.
fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// `--install-engine`, for `main`: the report on stdout as it goes, and
/// true when the engine is in place and answered.
pub fn run(options: &Options) -> bool {
    match run_or_say(options) {
        Ok(fine) => fine,
        Err(why) => {
            eprintln!("blinkterm: {}", sanitize(&why));
            false
        }
    }
}

fn run_or_say(options: &Options) -> Result<bool, String> {
    let platform = platform()?;
    let shell =
        shell_for(platform).ok_or_else(|| format!("no checksum for {platform} is compiled in"))?;
    let data = Profile::data_dir()?;
    let engines = engines_dir(&data);
    // First, before any network: what an interrupted run left.
    sweep_staging(&engines);
    let link = url(SHELL_VERSION, platform);
    say(
        "engine",
        &format!("chrome-headless-shell {SHELL_VERSION} for {platform}"),
    );
    if !engine::is_executable(&executable_in(&data, SHELL_VERSION, platform)) {
        for (tool, how) in [
            (
                "curl",
                "macOS ships it; on Debian or Ubuntu, apt install curl",
            ),
            (
                "unzip",
                "macOS ships it; on Debian or Ubuntu, apt install unzip",
            ),
        ] {
            if engine::search_path(tool).is_none() {
                return Err(format!(
                    "--install-engine needs {tool} on PATH and there is none ({how}); \
                     nothing fetched"
                ));
            }
        }
        say("from", &link);
    }
    let installed = install_with(&data, &shell, SHELL_VERSION, |zip| fetch(&link, zip))?;
    let path = installed.executable.display().to_string();
    if installed.fresh {
        say(
            "sha-256",
            &format!(
                "{} of {}, as compiled in",
                shell.sha256,
                megabytes(installed.bytes)
            ),
        );
        say("installed", &path);
    } else {
        say("installed", &format!("already installed at {path}"));
    }
    more(&match &options.engine.path {
        Some(named) => format!(
            "but {} is named, and wins over it until it is not",
            named.display()
        ),
        None => "found before PATH from now on; --engine, $BLINKTERM_ENGINE and engine = \
                 still win"
            .to_string(),
    });

    let launch = engine::Launch {
        path: Some(installed.executable.clone()),
        ..options.engine.clone()
    };
    let answered = match crate::doctor::start_once(&launch) {
        Ok((took, product)) => {
            say(
                "answered",
                &format!("on its pipe in {:.2} s: {product}", took.as_secs_f64()),
            );
            true
        }
        Err(why) => {
            say("answered", &format!("no: {why}"));
            let top = installed
                .executable
                .parent()
                .unwrap_or(&installed.dir)
                .display()
                .to_string();
            if cfg!(target_os = "linux") {
                more(&format!(
                    "the libraries it wants are listed in {top}/deb.deps"
                ));
            } else if cfg!(target_os = "macos") {
                more(&format!(
                    "if macOS refused it: xattr -dr com.apple.quarantine {top}"
                ));
            }
            false
        }
    };
    say("remove", &format!("rm -r {}", installed.dir.display()));
    for (i, other) in other_versions(&data, SHELL_VERSION).iter().enumerate() {
        let line = format!("{} (rm -r it if nothing names it)", other.display());
        if i == 0 {
            say("older", &line);
        } else {
            more(&line);
        }
    }
    Ok(answered)
}

/// The bytes of a file, or `None` with a line saying why the check that
/// wanted it is skipped: `.github/` and `docs/` are not in the published
/// crate, and its tests must still pass.
#[cfg(test)]
fn read_beside_manifest(path: &str) -> Option<String> {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    match std::fs::read_to_string(&full) {
        Ok(text) => Some(text),
        Err(e) => {
            eprintln!("skipped {}: {e}", full.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A scratch directory of this test's own, gone again before and after.
    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blinkterm-unit-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn file_with_mode(path: &Path, bytes: &[u8], mode: u32) {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(path, bytes).expect("written");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    /// Every version the text names after `marker`, up to the next
    /// character that cannot be in one.
    fn versions_after<'a>(text: &'a str, marker: &str) -> Vec<&'a str> {
        text.match_indices(marker)
            .map(|(at, _)| {
                let rest = &text[at + marker.len()..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                    .unwrap_or(rest.len());
                rest[..end].trim_end_matches('.')
            })
            .filter(|version| !version.is_empty())
            .collect()
    }

    #[test]
    fn the_pin_is_the_one_ci_installs_and_the_docs_tell_people_to_fetch() {
        if let Some(ci) = read_beside_manifest(".github/workflows/ci.yml") {
            let mut versions = 0;
            let mut last_sha = None;
            let mut seen = Vec::new();
            for line in ci.lines() {
                let line = line.trim();
                if let Some(version) = line.strip_prefix("SHELL_VERSION:") {
                    assert_eq!(version.trim(), SHELL_VERSION, "ci.yml: {line}");
                    versions += 1;
                }
                if let Some(sha) = line.strip_prefix("SHELL_SHA256:") {
                    last_sha = Some(sha.trim().to_string());
                }
                for shell in SHELLS {
                    if line.contains(&format!("chrome-headless-shell-{}.zip", shell.platform)) {
                        assert_eq!(
                            last_sha.as_deref(),
                            Some(shell.sha256),
                            "ci.yml's checksum for {}",
                            shell.platform
                        );
                        seen.push(shell.platform);
                    }
                }
            }
            assert_eq!(versions, 2, "ci.yml pins the engine in two jobs");
            assert!(seen.contains(&"linux64"), "ci.yml installs linux64");
            assert!(seen.contains(&"mac-arm64"), "ci.yml installs mac-arm64");
        }
        for path in ["README.md", "docs/install.md", "src/main.rs"] {
            let Some(text) = read_beside_manifest(path) else {
                continue;
            };
            for marker in [
                "chrome-for-testing-public/",
                "chrome-headless-shell ",
                "blinkterm/engine/",
            ] {
                for version in versions_after(&text, marker) {
                    assert_eq!(version, SHELL_VERSION, "{path}: {marker}{version}");
                }
            }
        }
    }

    #[test]
    fn the_three_platforms_have_sixty_four_lowercase_hex_digits_each_and_this_platform_is_one_of_them(
    ) {
        for shell in SHELLS {
            assert_eq!(shell.sha256.len(), 64, "{}", shell.platform);
            assert!(
                shell
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{}",
                shell.platform
            );
        }
        if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            let why = platform().expect_err("no build for Linux on arm64");
            eprintln!("skipped: {why}");
            return;
        }
        let here = platform().expect("a platform Chrome for Testing builds for");
        assert_eq!(shell_for(here).map(|shell| shell.platform), Some(here));
        assert_eq!(shell_for("win64"), None);
    }

    #[test]
    fn the_url_is_chrome_for_testings_public_bucket_by_version_and_platform() {
        assert_eq!(
            url("153.0.8010.52", "mac-arm64"),
            "https://storage.googleapis.com/chrome-for-testing-public/153.0.8010.52/\
             mac-arm64/chrome-headless-shell-mac-arm64.zip"
        );
    }

    #[test]
    fn the_installed_engine_is_the_pinned_version_s_executable_and_nothing_else() {
        let Ok(platform) = platform() else {
            return;
        };
        let data = scratch("installed");
        assert_eq!(installed_engine_in(&data), None, "no engine directory");

        file_with_mode(
            &executable_in(&data, "0.0.1", platform),
            b"#!/bin/sh\n",
            0o755,
        );
        assert_eq!(installed_engine_in(&data), None, "another version alone");

        let pinned = executable_in(&data, SHELL_VERSION, platform);
        file_with_mode(&pinned, b"#!/bin/sh\n", 0o644);
        assert_eq!(installed_engine_in(&data), None, "not executable");

        std::fs::set_permissions(&pinned, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(installed_engine_in(&data), Some(pinned));
        std::fs::remove_dir_all(&data).ok();
    }

    #[test]
    fn a_download_whose_checksum_does_not_match_installs_nothing_and_says_so() {
        let data = scratch("mismatch");
        let shell = SHELLS[0];
        let failed = install_with(&data, &shell, "0.0.1", |zip| {
            std::fs::write(zip, b"garbage").map_err(|e| e.to_string())
        })
        .expect_err("a mismatch");
        assert!(failed.contains("checksum mismatch"), "{failed}");
        assert!(failed.contains("nothing installed"), "{failed}");
        assert!(failed.contains(shell.sha256), "{failed}");
        let left: Vec<_> = std::fs::read_dir(engines_dir(&data))
            .expect("the engine directory")
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert!(left.is_empty(), "left behind: {left:?}");
        std::fs::remove_dir_all(&data).ok();
    }

    /// CRC-32 (IEEE), the checksum a zip entry carries.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// A zip of one stored (uncompressed) file, recorded as 0644 so that
    /// the `chmod` after unpacking is what makes it executable.
    fn stored_zip(name: &str, body: &[u8]) -> Vec<u8> {
        let crc = crc32(body);
        let size = body.len() as u32;
        let name_len = name.len() as u16;
        let mut zip = Vec::new();
        let u16le = |zip: &mut Vec<u8>, v: u16| zip.extend_from_slice(&v.to_le_bytes());
        let u32le = |zip: &mut Vec<u8>, v: u32| zip.extend_from_slice(&v.to_le_bytes());
        // Local file header.
        u32le(&mut zip, 0x0403_4b50);
        for v in [10, 0, 0, 0, 0x21] {
            u16le(&mut zip, v); // version, flags, stored, time, date
        }
        for v in [crc, size, size] {
            u32le(&mut zip, v);
        }
        u16le(&mut zip, name_len);
        u16le(&mut zip, 0);
        zip.extend_from_slice(name.as_bytes());
        zip.extend_from_slice(body);
        // Central directory.
        let central = zip.len() as u32;
        u32le(&mut zip, 0x0201_4b50);
        for v in [(3 << 8) | 20, 10, 0, 0, 0, 0x21] {
            u16le(&mut zip, v); // made by unix, needed, flags, stored, time, date
        }
        for v in [crc, size, size] {
            u32le(&mut zip, v);
        }
        for v in [name_len, 0, 0, 0, 0] {
            u16le(&mut zip, v); // name, extra, comment, disk, internal
        }
        u32le(&mut zip, 0o100_644 << 16);
        u32le(&mut zip, 0);
        zip.extend_from_slice(name.as_bytes());
        let central_size = zip.len() as u32 - central;
        // End of central directory.
        u32le(&mut zip, 0x0605_4b50);
        for v in [0, 0, 1, 1] {
            u16le(&mut zip, v);
        }
        u32le(&mut zip, central_size);
        u32le(&mut zip, central);
        u16le(&mut zip, 0);
        zip
    }

    #[test]
    fn a_zip_that_matches_is_unpacked_made_executable_and_renamed_into_place() {
        if engine::search_path("unzip").is_none() {
            eprintln!("skipped: no unzip on PATH");
            return;
        }
        let Ok(platform) = platform() else {
            return;
        };
        let data = scratch("unpack");
        let zip = stored_zip(
            &format!("chrome-headless-shell-{platform}/chrome-headless-shell"),
            b"#!/bin/sh\nexit 0\n",
        );
        let mut hash = sha256::Sha256::new();
        hash.update(&zip);
        let digest: &'static str = Box::leak(sha256::hex(&hash.finish()).into_boxed_str());
        let shell = Shell {
            platform,
            sha256: digest,
        };

        let installed = install_with(&data, &shell, "0.0.1", |to| {
            std::fs::write(to, &zip).map_err(|e| e.to_string())
        })
        .expect("installed");
        assert!(installed.fresh);
        assert_eq!(installed.bytes, zip.len() as u64);
        assert_eq!(
            installed.executable,
            executable_in(&data, "0.0.1", platform)
        );
        assert_eq!(installed.dir, version_dir(&data, "0.0.1"));
        let mode = std::fs::metadata(&installed.executable)
            .expect("the engine is there")
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "executable: {mode:o}");
        let left: Vec<_> = std::fs::read_dir(engines_dir(&data))
            .expect("the engine directory")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["0.0.1".to_string()], "no staging, no zip");

        let fetched = AtomicBool::new(false);
        let again = install_with(&data, &shell, "0.0.1", |_| {
            fetched.store(true, Ordering::SeqCst);
            Err("fetched again".to_string())
        })
        .expect("already there");
        assert!(!again.fresh);
        assert_eq!(again.executable, installed.executable);
        assert!(!fetched.load(Ordering::SeqCst), "a second run fetched");
        std::fs::remove_dir_all(&data).ok();
    }

    #[test]
    fn other_versions_lists_sibling_directories_and_not_staging_or_files() {
        let data = scratch("others");
        let engines = engines_dir(&data);
        for dir in ["141.0.7390.37", SHELL_VERSION, ".staging-12", "100.0.1.2"] {
            std::fs::create_dir_all(engines.join(dir)).expect("a directory");
        }
        std::fs::write(engines.join("notes"), b"").expect("a file");
        assert_eq!(
            other_versions(&data, SHELL_VERSION),
            vec![engines.join("100.0.1.2"), engines.join("141.0.7390.37")]
        );
        assert!(other_versions(&data.join("absent"), SHELL_VERSION).is_empty());
        std::fs::remove_dir_all(&data).ok();
    }

    #[test]
    fn a_staging_directory_whose_process_is_gone_is_swept_and_a_live_one_is_not() {
        let engines = scratch("staging");
        let ours = engines.join(format!(".staging-{}", std::process::id()));
        let gone = engines.join(".staging-999999999");
        let version = engines.join("0.0.1");
        for dir in [&ours, &gone, &version] {
            std::fs::create_dir_all(dir.join("unpacked")).expect("a directory");
        }
        sweep_staging(&engines);
        assert!(ours.exists(), "a live process's staging went");
        assert!(!gone.exists(), "a dead process's staging stayed");
        assert!(version.exists(), "an installed version went");
        std::fs::remove_dir_all(&engines).ok();
    }
}
