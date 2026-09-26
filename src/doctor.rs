//! `--doctor` and `--print-engine`: the facts a first run needs, one per
//! line, and the one question this program cannot ask itself while it runs —
//! does the terminal speak the two protocols the pane assumes.
//!
//! # The engine is started, not found
//!
//! "Found at /opt/…" is not the question; "answers on its pipe" is. An engine
//! missing `libnss3.so` is found and does not answer, and an `--engine-arg`
//! the engine chokes on is only visible by starting it. So the doctor starts
//! it exactly as a run would — the same flags, the same extras, the same
//! twenty seconds — on a temporary profile that goes when the engine does,
//! and never on the kept one: a `blinkterm` already running on that has its
//! lock, and must not be disturbed by a doctor beside it. `--print-engine`
//! is the other end: the path the search finds and nothing started, for a
//! script.
//!
//! # Three questions and a sentinel
//!
//! Nothing a running `blinkterm` does tells it whether the terminal speaks
//! the Kitty graphics and keyboard protocols: the program assumes both, and
//! a person in tmux without passthrough finds out by seeing garbage. The
//! doctor asks, in one write:
//!
//! - the three the pane already asks and never waits for — pixel mouse,
//!   background, cell size ([`crate::screen`]) — because they are free and
//!   they are what a person asks next;
//! - a graphics query, `a=q` with `t=d` and a one-pixel image, under id 31 so
//!   that it is never mistaken for the running program's image;
//! - `CSI ? u`, which a terminal speaking the keyboard protocol answers with
//!   the flags currently pushed — `0` is a yes with nothing pushed;
//! - and DA1, `CSI c`, last.
//!
//! DA1 is the sentinel. Every terminal answers it, in order, after whatever
//! came before it; so the answers to the questions before it, if any, have
//! arrived by the time it does, and a DA1 that does not come in two seconds
//! means nothing on the other end is a terminal that answers — tmux without
//! `allow-passthrough`, a `script(1)` log, a pipe.
//!
//! The graphics query is asked with `t=d` only. Measured against tOS's
//! terminal, a `t=s` query answers `OK` without looking at the file, so a
//! query cannot tell whether the terminal will *read* shared memory; what
//! the doctor reports about shared memory is whether this side can make a
//! frame there — a file in `/dev/shm` on Linux, a `shm_open(3)` object on a
//! Mac — which is what [`crate::graphics::Painter::new`] decides on.
//!
//! [`crate::input`] has no arm for an APC and drops the two `CSI ?` answers,
//! rightly — the running program never wants them — so [`read_answer`]
//! reads its own three shapes out of the bytes, and hands the rest to the
//! program's parser for the three answers it does know.

use std::io::{self, IsTerminal, Write};
use std::os::unix::io::RawFd;
use std::time::{Duration, Instant};

use tos_platform::tty::{self, RawMode, ReadOutcome};

use crate::appearance;
use crate::download;
use crate::engine::{self, Engine};
use crate::graphics;
use crate::input::{Input, Parser};
use crate::json::Json;
use crate::options::{Options, Provenance};
use crate::profile::{self, Profile};
use crate::screen;

/// How long the terminal is given to answer everything.
pub const TERMINAL_TIMEOUT: Duration = Duration::from_secs(2);

/// The image id the graphics query is asked under, which is not the running
/// program's ([`crate::graphics::IMAGE_ID`]).
pub const QUERY_ID: u32 = 31;

/// The graphics query, the keyboard query and DA1, in that order; see the
/// module's section on them. [`ask`] puts the pane's own three in front.
pub const ASK: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[?u\x1b[c";

/// Every question, in the one write: the pane's three, then [`ASK`].
pub fn ask() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(screen::ASK_PIXEL_MOUSE);
    out.extend_from_slice(screen::ASK_BACKGROUND);
    out.extend_from_slice(screen::ASK_CELL_SIZE);
    out.extend_from_slice(ASK);
    out
}

/// What the bytes the terminal sent back say. Pure.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalAnswer {
    /// `Some(Ok(()))` on `;OK`, `Some(Err(text))` on anything else — Kitty's
    /// `EBADF:…`, `EINVAL:…` — and `None` when no answer for [`QUERY_ID`]
    /// came.
    pub graphics: Option<Result<(), String>>,
    /// The flags in `CSI ? <n> u`, `None` when it did not come.
    pub keyboard: Option<u32>,
    /// Whether `CSI ? … c` came: the sentinel.
    pub da1: bool,
    /// The DECRPM answer for mode 1016, pixel mouse.
    pub pixel_mouse: Option<u8>,
    /// The background colour, from `OSC 11`.
    pub background: Option<(u8, u8, u8)>,
    /// The cell, width by height, from `CSI 6 ; h ; w t`.
    pub cell: Option<(u32, u32)>,
}

/// Read what came back. Keys typed while it was being waited for are in the
/// same bytes and change nothing: only the shapes that were asked for are
/// looked at.
pub fn read_answer(bytes: &[u8]) -> TerminalAnswer {
    let mut answer = TerminalAnswer::default();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"\x1b_G") {
            let body = &bytes[i + 3..];
            let Some(end) = find(body, b"\x1b\\") else {
                break;
            };
            let command = &body[..end];
            if let Some(result) = graphics_answer(command) {
                answer.graphics.get_or_insert(result);
            }
            i += 3 + end + 2;
            continue;
        }
        if bytes[i..].starts_with(b"\x1b[?") {
            let body = &bytes[i + 3..];
            let params = body
                .iter()
                .take_while(|b| b.is_ascii_digit() || **b == b';')
                .count();
            match body.get(params) {
                Some(b'u') => {
                    let text = std::str::from_utf8(&body[..params]).unwrap_or_default();
                    answer.keyboard = answer.keyboard.or(text.parse().ok());
                }
                Some(b'c') => answer.da1 = true,
                _ => {}
            }
        }
        i += 1;
    }
    let mut parser = Parser::new();
    let mut inputs = parser.feed(bytes);
    inputs.extend(parser.flush());
    for input in inputs {
        match input {
            Input::Mode { mode: 1016, state } => answer.pixel_mouse = Some(state),
            Input::Colour { slot: 11, rgb } => answer.background = Some(rgb),
            Input::CellSize { width, height } => answer.cell = Some((width, height)),
            _ => {}
        }
    }
    answer
}

/// The control data and payload of one graphics answer, when it is the
/// answer to [`QUERY_ID`].
fn graphics_answer(command: &[u8]) -> Option<Result<(), String>> {
    let text = String::from_utf8_lossy(command);
    let (control, payload) = text.split_once(';')?;
    let ours = control
        .split(',')
        .any(|pair| pair == format!("i={QUERY_ID}"));
    if !ours {
        return None;
    }
    Some(if payload == "OK" {
        Ok(())
    } else {
        Err(crate::text::sanitize(payload).into_owned())
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Ask, on stdout, and listen on `input`, in raw mode for the duration,
/// until the sentinel or `timeout`. Whatever came is read whether or not DA1 arrived.
///
/// A [`RawMode`] guard is enough here, unlike in [`crate::screen::Pane`]:
/// there is no panic hook that has to reach the saved settings, and nothing
/// else is turned on — no alternate screen, no mouse, no keyboard flags —
/// because `CSI ? u` reports what is pushed and the doctor should report the
/// terminal at rest.
pub fn ask_terminal(input: RawFd, timeout: Duration) -> io::Result<TerminalAnswer> {
    let _raw = RawMode::acquire(input)?;
    let mut stdout = io::stdout().lock();
    stdout.write_all(&ask())?;
    stdout.flush()?;
    let deadline = Instant::now() + timeout;
    let mut heard = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        if !tty::poll_readable(&[input], ms)?.contains(&input) {
            continue;
        }
        match tty::read_available(input, &mut buf)? {
            ReadOutcome::Data(n) => heard.extend_from_slice(&buf[..n]),
            ReadOutcome::Eof => break,
            ReadOutcome::WouldBlock => {}
        }
        if read_answer(&heard).da1 {
            break;
        }
    }
    Ok(read_answer(&heard))
}

/// The column the facts start in, after their names.
const LABEL: usize = 11;

/// One fact, its name in the margin.
fn say(name: &str, fact: &str) {
    println!("{:<LABEL$}{fact}", format!("{name}:"));
}

/// A line under the last fact, in the same column.
fn more(fact: &str) {
    println!("{:<LABEL$}{fact}", "");
}

/// `--print-engine`: the path, or the sentence about why there is none.
pub fn print_engine(options: &Options) -> bool {
    match engine::locate_with(options.engine.path.as_deref()) {
        Ok(path) => {
            println!("{}", path.display());
            true
        }
        Err(why) => {
            eprintln!("blinkterm: {why}");
            false
        }
    }
}

/// The whole report, for `main`: printed as it goes, each line as its fact
/// is known, so that a hang shows where. True when every fact that decides a
/// run was a yes: the engine answered, and the terminal, if it was asked,
/// answered the graphics query. The keyboard protocol alone missing is a
/// warning, not a failure: the program runs without it, and loses key
/// releases and `ctrl+i` against `tab`.
pub fn report(options: &Options, provenance: &Provenance) -> bool {
    println!("blinkterm {}", env!("CARGO_PKG_VERSION"));
    config_line(provenance);
    let engine_ok = engine_lines(options, provenance);
    profile_line(&options.profile);
    match download::prepare(options.download.clone()) {
        Ok(dir) => say("downloads", &dir.display().to_string()),
        Err(why) => say("downloads", &why),
    }
    shm_line();
    let terminal_ok = terminal_lines();
    engine_ok && terminal_ok
}

fn config_line(provenance: &Provenance) {
    match (&provenance.config, provenance.found) {
        (Some(path), true) => say(
            "config",
            &format!(
                "{} ({} setting{})",
                path.display(),
                provenance.settings,
                if provenance.settings == 1 { "" } else { "s" }
            ),
        ),
        (Some(path), false) => say("config", &format!("none (looked at {})", path.display())),
        (None, _) => say("config", "none read"),
    }
}

fn engine_lines(options: &Options, provenance: &Provenance) -> bool {
    let path = match engine::locate_with(options.engine.path.as_deref()) {
        Ok(path) => path,
        Err(why) => {
            say("engine", &format!("none: {why}"));
            return false;
        }
    };
    say("engine", &path.display().to_string());
    more(&match provenance.engine_from {
        Some(source) => format!("from {source}"),
        None => "on PATH".to_string(),
    });
    let profile = match Profile::temporary() {
        Ok(profile) => profile,
        Err(why) => {
            more(&format!("not started: {why}"));
            return false;
        }
    };
    let started = Instant::now();
    let engine = match Engine::launch_with(profile, crate::app::ENGINE_TIMEOUT, &options.engine) {
        Ok(engine) => engine,
        Err(why) => {
            more(&format!("did not answer: {}", crate::text::sanitize(&why)));
            return false;
        }
    };
    let took = started.elapsed();
    let product = engine
        .browser()
        .and_then(|mut browser| {
            browser.call_within("Browser.getVersion", Json::empty(), Duration::from_secs(5))
        })
        .ok()
        .and_then(|reply| {
            reply
                .get("product")
                .and_then(Json::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "no product named".to_string());
    more(&format!(
        "answered on its pipe in {:.2} s: {}",
        took.as_secs_f64(),
        crate::text::sanitize(&product)
    ));
    // Dropped here, which stops it and removes its temporary profile, before
    // the terminal is put in raw mode.
    drop(engine);
    true
}

fn profile_line(choice: &profile::Choice) {
    let dir = match choice {
        profile::Choice::Temporary => {
            say("profile", "temporary, thrown away on exit");
            return;
        }
        profile::Choice::Default => Profile::default_dir(),
        profile::Choice::At(dir) if dir.is_absolute() => Ok(dir.clone()),
        profile::Choice::At(dir) => std::env::current_dir()
            .map(|cwd| cwd.join(dir))
            .map_err(|e| format!("cannot tell where {} is: {e}", dir.display())),
    };
    match dir {
        Ok(dir) => say(
            "profile",
            &format!(
                "{} ({})",
                dir.display(),
                if dir.is_dir() {
                    "exists"
                } else {
                    "not made yet"
                }
            ),
        ),
        Err(why) => say("profile", &why),
    }
}

fn shm_line() {
    let name = format!("blinkterm-{}-doctor", std::process::id());
    match graphics::probe_shared_memory(&name) {
        Ok(()) => say(
            "shared memory",
            &format!("{}, so frames go as t=s", graphics::SHM_HOW),
        ),
        Err(e) => say(
            "shared memory",
            &format!(
                "{} ({e}), so frames go inline as base64",
                graphics::SHM_HOW_NOT
            ),
        ),
    }
}

fn terminal_lines() -> bool {
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        say("terminal", "stdin/stdout is not a terminal; nothing asked");
        return true;
    }
    let answer = match ask_terminal(0, TERMINAL_TIMEOUT) {
        Ok(answer) => answer,
        Err(e) => {
            say("terminal", &format!("cannot ask: {e}"));
            return false;
        }
    };
    if !answer.da1 && answer.graphics.is_none() && answer.keyboard.is_none() {
        say(
            "terminal",
            &format!(
                "no answer in {} s: not a terminal that answers queries \
                 (tmux without allow-passthrough? a log?)",
                TERMINAL_TIMEOUT.as_secs()
            ),
        );
        return false;
    }
    let graphics = match &answer.graphics {
        Some(Ok(())) => "yes".to_string(),
        Some(Err(why)) => format!("no ({why})"),
        None => "no (no answer to the query)".to_string(),
    };
    say("terminal", &format!("graphics protocol: {graphics}"));
    more(&match answer.keyboard {
        Some(0) => "keyboard protocol: yes (no flags pushed)".to_string(),
        Some(flags) => format!("keyboard protocol: yes (flags {flags} pushed)"),
        None => "keyboard protocol: no (no answer to CSI ? u)".to_string(),
    });
    more(&match answer.pixel_mouse {
        Some(1..=3) => "mouse in pixels: yes".to_string(),
        _ => "mouse in pixels: no (cells)".to_string(),
    });
    if let Some(rgb) = answer.background {
        more(&format!(
            "background: #{:02x}{:02x}{:02x}, {}",
            rgb.0,
            rgb.1,
            rgb.2,
            if appearance::is_dark(rgb) {
                "dark"
            } else {
                "light"
            }
        ));
    }
    let cells = tty::terminal_size(1).ok();
    match (answer.cell, cells) {
        (Some((w, h)), Some(size)) => more(&format!(
            "cell: {w}x{h} px, {}x{} cells",
            size.cols, size.rows
        )),
        (Some((w, h)), None) => more(&format!("cell: {w}x{h} px")),
        (None, Some(size)) => more(&format!(
            "cell: not answered, {}x{} cells",
            size.cols, size.rows
        )),
        (None, None) => {}
    }
    matches!(answer.graphics, Some(Ok(())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal() -> tos_term::Terminal {
        tos_term::Terminal::new(80, 24, tos_term::TerminalConfig::default())
    }

    #[test]
    fn the_terminal_answers_the_three_questions_in_order_and_the_answer_reads_as_three_yeses() {
        let mut terminal = terminal();
        terminal.advance(ASK);
        let answer = read_answer(&terminal.take_output());
        assert_eq!(answer.graphics, Some(Ok(())));
        assert_eq!(answer.keyboard, Some(0));
        assert!(answer.da1);
    }

    #[test]
    fn a_terminal_that_answers_only_da1_reads_as_two_nos_and_a_sentinel() {
        let answer = read_answer(b"\x1b[?62;22c");
        assert_eq!(answer.graphics, None);
        assert_eq!(answer.keyboard, None);
        assert!(answer.da1);
    }

    #[test]
    fn silence_reads_as_no_sentinel() {
        assert_eq!(read_answer(b""), TerminalAnswer::default());
    }

    #[test]
    fn an_error_from_the_graphics_query_carries_its_text() {
        let answer = read_answer(b"\x1b_Gi=31;EBADF:Failed to open file\x1b\\\x1b[?1u\x1b[?62c");
        assert_eq!(
            answer.graphics,
            Some(Err("EBADF:Failed to open file".to_string()))
        );
        assert_eq!(answer.keyboard, Some(1));
    }

    #[test]
    fn keys_typed_during_the_wait_do_not_change_the_answer() {
        let quiet = read_answer(b"\x1b_Gi=31;OK\x1b\\\x1b[?0u\x1b[?62;22c");
        let typed = read_answer(b"ab\x1b[97;5u\x1b_Gi=31;OK\x1b\\q\x1b[?0u\x1b[Ax\x1b[?62;22c");
        assert_eq!(typed, quiet);
    }

    #[test]
    fn a_query_for_another_image_id_is_not_this_programs_answer() {
        let answer = read_answer(b"\x1b_Gi=1;OK\x1b\\\x1b_Gi=310;OK\x1b\\\x1b[?62c");
        assert_eq!(answer.graphics, None);
        assert!(answer.da1);
    }

    #[test]
    fn the_existing_three_questions_are_read_from_the_same_bytes() {
        let config = tos_term::TerminalConfig::default();
        let (width, height) = (config.cell_width, config.cell_height);
        let mut terminal = terminal();
        terminal.advance(&ask());
        let bytes = terminal.take_output();
        let answer = read_answer(&bytes);
        assert_eq!(answer.graphics, Some(Ok(())), "{bytes:?}");
        assert_eq!(answer.keyboard, Some(0));
        assert!(answer.da1);
        assert!(answer.pixel_mouse.is_some(), "{bytes:?}");
        assert!(answer.background.is_some(), "{bytes:?}");
        assert_eq!(answer.cell, Some((width, height)));
    }
}
