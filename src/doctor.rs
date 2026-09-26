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
//! means nothing on the other end is a terminal that answers — a
//! `script(1)` log, a pipe.
//!
//! # Asked before every run, and asked again through tmux
//!
//! A run asks the same questions before it starts the engine
//! ([`probe`]): a terminal that cannot draw should cost a sentence in the
//! shell, not a Chromium start and a blank pane. In tmux the first asking
//! proves nothing either way — tmux answers DA1 itself, within a
//! millisecond, and eats the graphics query without forwarding it,
//! `allow-passthrough` or not — so when the environment says tmux (or
//! screen) and the first stage heard no `OK`, the question is asked a second
//! time wrapped for tmux's passthrough ([`ask_wrapped`]), under its own id
//! ([`WRAPPED_QUERY_ID`]) so that the two answers are told apart in the same
//! bytes. tmux does not answer a wrapped DA1; the terminal behind it does,
//! and tmux forwards the answer, so the second stage has a sentinel of its
//! own. With `allow-passthrough off` the wrapped questions go nowhere and
//! the second stage waits out its two seconds. [`Verdict`] is what the two
//! stages add up to, and [`crate::route::choose`] what a run makes of it.
//!
//! The graphics query is asked with `t=d` only. Measured against tOS's
//! terminal, a `t=s` query answers `OK` without looking at the file, so a
//! query cannot tell whether the terminal will *read* `/dev/shm`; what the
//! doctor reports about `/dev/shm` is whether this side can write there,
//! which is what [`crate::graphics::Painter::new`] decides on.
//!
//! [`crate::input`] has no arm for an APC and drops the two `CSI ?` answers,
//! rightly — the running program never wants them — so [`read_answer`]
//! reads its own three shapes out of the bytes, and hands the rest to the
//! program's parser for the three answers it does know.

use std::io::{self, IsTerminal, Write};
use std::os::unix::io::RawFd;
use std::path::Path;
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

/// The image id the *wrapped* query is asked under, so that the answer to
/// the raw one and to the one that went through a multiplexer are told apart
/// in the same bytes.
pub const WRAPPED_QUERY_ID: u32 = 32;

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

/// The second stage, for tmux: the graphics query under
/// [`WRAPPED_QUERY_ID`], the cell size, `CSI ? u` and DA1, wrapped for its
/// passthrough.
///
/// The cell size is asked here and not only by the pane: tmux swallows it
/// bare, and a passthrough written in the same breath as the pane's
/// `?1049h` never reaches the terminal — tmux drops a raw string while a
/// redraw of the window is pending, which the alternate screen has just
/// asked for (seen with tmux 3.4: the pane's wrapped `CSI 16 t` went
/// nowhere, the probe's came back). Over ssh through tmux it is the only
/// way the cell is known. Mouse and background are not asked: tmux drops
/// both whatever is asked.
pub fn ask_wrapped() -> Vec<u8> {
    let query =
        format!("\x1b_Gi={WRAPPED_QUERY_ID},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[?u\x1b[c");
    graphics::wrap_for_tmux(query.as_bytes())
}

/// What the bytes the terminal sent back say. Pure.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalAnswer {
    /// `Some(Ok(()))` on `;OK`, `Some(Err(text))` on anything else — Kitty's
    /// `EBADF:…`, `EINVAL:…` — and `None` when no answer for [`QUERY_ID`]
    /// came.
    pub graphics: Option<Result<(), String>>,
    /// The same for [`WRAPPED_QUERY_ID`]: the answer that came through a
    /// multiplexer's passthrough, when one did.
    pub wrapped_graphics: Option<Result<(), String>>,
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
            match graphics_answer(command) {
                Some((QUERY_ID, result)) => {
                    answer.graphics.get_or_insert(result);
                }
                Some((_, result)) => {
                    answer.wrapped_graphics.get_or_insert(result);
                }
                None => {}
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

/// The id and payload of one graphics answer, when it is the answer to
/// [`QUERY_ID`] or [`WRAPPED_QUERY_ID`].
fn graphics_answer(command: &[u8]) -> Option<(u32, Result<(), String>)> {
    let text = String::from_utf8_lossy(command);
    let (control, payload) = text.split_once(';')?;
    let id = [QUERY_ID, WRAPPED_QUERY_ID]
        .into_iter()
        .find(|id| control.split(',').any(|pair| pair == format!("i={id}")))?;
    Some((
        id,
        if payload == "OK" {
            Ok(())
        } else {
            Err(crate::text::sanitize(payload).into_owned())
        },
    ))
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
    Ok(read_answer(&listen(input, &ask(), timeout)?))
}

/// Write `questions` to stdout and gather what comes back on `input` until
/// a DA1 is among it or `timeout`. The caller holds raw mode.
fn listen(input: RawFd, questions: &[u8], timeout: Duration) -> io::Result<Vec<u8>> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(questions)?;
    stdout.flush()?;
    drop(stdout);
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
    Ok(heard)
}

/// What the terminal, or the thing in front of it, turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The raw query was answered `OK`: a Kitty, WezTerm, Ghostty or tOS
    /// pane, whatever the environment says.
    Direct,
    /// The raw query went unanswered and the wrapped one was answered `OK`:
    /// the terminal behind tmux draws, and `allow-passthrough` is on.
    ThroughTmux,
    /// A DA1 came and no `OK` either way: a terminal that does not speak the
    /// protocol, or tmux with passthrough off in front of one that does —
    /// [`refusal`] says which from the environment.
    NoGraphics,
    /// Nothing came in the time: a log, a pipe, a serial line.
    NoAnswer,
    /// `--no-probe`: assumed to draw; nothing was asked.
    Skipped,
}

impl Verdict {
    /// Whether a run goes ahead on this verdict.
    pub fn draws(self) -> bool {
        matches!(
            self,
            Verdict::Direct | Verdict::ThroughTmux | Verdict::Skipped
        )
    }
}

/// What the answers of both stages add up to. Pure.
pub fn judge(answer: &TerminalAnswer) -> Verdict {
    if answer.graphics == Some(Ok(())) {
        Verdict::Direct
    } else if answer.wrapped_graphics == Some(Ok(())) {
        Verdict::ThroughTmux
    } else if answer.da1 {
        Verdict::NoGraphics
    } else {
        Verdict::NoAnswer
    }
}

/// The two stages, on stdout and `input`, in raw mode for their duration:
/// [`ask`] always, and [`ask_wrapped`] when `try_wrapped` — the environment
/// says tmux or screen — and the first stage heard no `OK`. Each waits for
/// its own DA1 or `timeout`. Keys typed meanwhile are read and dropped, as
/// `--doctor` drops them.
///
/// Fails only when `input` is not a terminal (raw mode cannot be had) or
/// stdout cannot be written, which a run would have failed on next anyway.
pub fn probe(
    input: RawFd,
    try_wrapped: bool,
    timeout: Duration,
) -> io::Result<(Verdict, TerminalAnswer)> {
    let _raw = RawMode::acquire(input)?;
    let mut answer = read_answer(&listen(input, &ask(), timeout)?);
    if try_wrapped && answer.graphics != Some(Ok(())) {
        let wrapped = read_answer(&listen(input, &ask_wrapped(), timeout)?);
        answer.wrapped_graphics = wrapped.wrapped_graphics;
        answer.keyboard = answer.keyboard.or(wrapped.keyboard);
        answer.cell = answer.cell.or(wrapped.cell);
        answer.da1 |= wrapped.da1;
    }
    Ok((judge(&answer), answer))
}

/// The sentence a run ends with when the terminal cannot draw; `None` when
/// it can. Plain text, one line, naming what to do.
pub fn refusal(verdict: Verdict, env: &crate::route::Env) -> Option<String> {
    match verdict {
        Verdict::Direct | Verdict::ThroughTmux | Verdict::Skipped => None,
        Verdict::NoGraphics if env.tmux => Some(
            "the terminal behind tmux does not draw pictures, or tmux's allow-passthrough \
             is off: set 'allow-passthrough on' in tmux.conf, and run inside a Kitty, \
             WezTerm, Ghostty or tOS pane"
                .to_string(),
        ),
        Verdict::NoGraphics if env.screen => Some(
            "GNU screen is between this program and the terminal and does not pass \
             pictures through; run outside screen"
                .to_string(),
        ),
        Verdict::NoGraphics => Some(format!(
            "this terminal (TERM={}) does not speak the Kitty graphics protocol; blinkterm \
             needs a Kitty, WezTerm, Ghostty or tOS pane. --doctor says what was asked \
             and answered",
            if env.term.is_empty() {
                "unset"
            } else {
                env.term.as_str()
            }
        )),
        Verdict::NoAnswer => Some(format!(
            "nothing answered the terminal in {} s: not a terminal that answers queries \
             (a log? a pipe?); --no-probe skips this check",
            TERMINAL_TIMEOUT.as_secs()
        )),
    }
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
    let terminal_ok = terminal_lines(options);
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
    let probe =
        Path::new(graphics::SHM_DIR).join(format!("blinkterm-{}-doctor", std::process::id()));
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            say("/dev/shm", "writable, so frames go as t=s");
        }
        Err(e) => say(
            "/dev/shm",
            &format!("not writable ({e}), so frames go inline as base64"),
        ),
    }
}

fn terminal_lines(options: &Options) -> bool {
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        say("terminal", "stdin/stdout is not a terminal; nothing asked");
        return true;
    }
    let env = crate::route::Env::current();
    let (verdict, answer) = match probe(0, env.tmux || env.screen, TERMINAL_TIMEOUT) {
        Ok(heard) => heard,
        Err(e) => {
            say("terminal", &format!("cannot ask: {e}"));
            return false;
        }
    };
    let route = crate::route::choose(
        &env,
        options.route,
        verdict,
        graphics::Painter::shm_usable(),
    );
    let cells = tty::terminal_size(1)
        .ok()
        .map(|size| (u32::from(size.cols), u32::from(size.rows)));
    let facts = terminal_facts(verdict, &answer, &env, &route, cells);
    let mut facts = facts.iter();
    if let Some(first) = facts.next() {
        say("terminal", first);
    }
    for fact in facts {
        more(fact);
    }
    matches!(verdict, Verdict::Direct | Verdict::ThroughTmux)
}

/// The terminal's lines, the first under the `terminal:` label: what was
/// heard, and the route a run would take on it. Pure, so that what the doctor
/// says about tmux is a test rather than a hope.
pub fn terminal_facts(
    verdict: Verdict,
    answer: &TerminalAnswer,
    env: &crate::route::Env,
    route: &crate::route::Route,
    cells: Option<(u32, u32)>,
) -> Vec<String> {
    if verdict == Verdict::NoAnswer {
        return vec![format!(
            "no answer in {} s: not a terminal that answers queries (a log? a pipe?)",
            TERMINAL_TIMEOUT.as_secs()
        )];
    }
    let tmux = route.wrap == crate::route::Wrap::Tmux;
    let mut facts = Vec::new();
    facts.push(match (verdict, &answer.graphics) {
        (Verdict::Direct, _) => "graphics protocol: yes".to_string(),
        (Verdict::ThroughTmux, _) => "graphics protocol: yes, through tmux passthrough".to_string(),
        (_, Some(Err(why))) => format!("graphics protocol: no ({why})"),
        _ if env.tmux => "graphics protocol: no (no answer raw or through tmux; \
                          is allow-passthrough on?)"
            .to_string(),
        _ => "graphics protocol: no (no answer to the query)".to_string(),
    });
    facts.push(match answer.keyboard {
        _ if tmux => "keyboard protocol: not used inside tmux (tmux re-encodes keys)".to_string(),
        Some(0) => "keyboard protocol: yes (no flags pushed)".to_string(),
        Some(flags) => format!("keyboard protocol: yes (flags {flags} pushed)"),
        None => "keyboard protocol: no (no answer to CSI ? u)".to_string(),
    });
    facts.push(match answer.pixel_mouse {
        _ if tmux => "mouse in pixels: no (cells; tmux)".to_string(),
        Some(1..=3) => "mouse in pixels: yes".to_string(),
        _ => "mouse in pixels: no (cells)".to_string(),
    });
    match answer.background {
        Some(rgb) => facts.push(format!(
            "background: #{:02x}{:02x}{:02x}, {}",
            rgb.0,
            rgb.1,
            rgb.2,
            if appearance::is_dark(rgb) {
                "dark"
            } else {
                "light"
            }
        )),
        None if env.tmux => facts.push("background: not answered (tmux)".to_string()),
        None => {}
    }
    match (answer.cell, cells) {
        (Some((w, h)), Some((cols, rows))) => {
            facts.push(format!("cell: {w}x{h} px, {cols}x{rows} cells"))
        }
        (Some((w, h)), None) => facts.push(format!("cell: {w}x{h} px")),
        (None, Some((cols, rows))) => {
            facts.push(format!("cell: not answered, {cols}x{rows} cells"))
        }
        (None, None) => {}
    }
    facts.push(format!("route: {}", route.describe()));
    facts
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

    use crate::route::{self, Env};

    fn in_tmux() -> Env {
        Env {
            tmux: true,
            term: "tmux-256color".to_string(),
            ..Env::default()
        }
    }

    #[test]
    fn the_wrapped_query_is_the_raw_one_with_id_thirty_two_inside_a_dcs_with_every_escape_doubled()
    {
        let wrapped = ask_wrapped();
        assert!(wrapped.starts_with(b"\x1bPtmux;\x1b\x1b_Gi=32,s=1,v=1,a=q,t=d,f=24;AAAA"));
        assert!(wrapped.ends_with(b"\x1b\x1b[16t\x1b\x1b[?u\x1b\x1b[c\x1b\\"));
        let unwrapped = graphics::tests::tmux_unwrap(&wrapped);
        let asked = String::from_utf8(ASK.to_vec()).unwrap();
        let asked = asked
            .replace("i=31,", "i=32,")
            .replace("\x1b[?u", "\x1b[16t\x1b[?u");
        assert_eq!(unwrapped, asked.into_bytes());
        // And a terminal behind tmux answers it under its own id.
        let mut terminal = terminal();
        terminal.advance(&unwrapped);
        let answer = read_answer(&terminal.take_output());
        assert_eq!(answer.wrapped_graphics, Some(Ok(())));
        assert_eq!(answer.graphics, None);
        assert!(answer.cell.is_some(), "the cell, which tmux swallows bare");
        assert!(answer.da1);
    }

    #[test]
    fn an_answer_for_id_thirty_two_is_the_wrapped_answer_and_does_not_count_as_the_raw_one() {
        let answer = read_answer(b"\x1b_Gi=32;OK\x1b\\\x1b[?62;22c");
        assert_eq!(answer.wrapped_graphics, Some(Ok(())));
        assert_eq!(answer.graphics, None);
        let both = read_answer(b"\x1b_Gi=31;ENOENT:x\x1b\\\x1b_Gi=32;OK\x1b\\\x1b[?62c");
        assert_eq!(both.graphics, Some(Err("ENOENT:x".to_string())));
        assert_eq!(both.wrapped_graphics, Some(Ok(())));
    }

    #[test]
    fn a_raw_da1_with_no_graphics_answer_and_a_wrapped_ok_is_through_tmux() {
        // Stage one: tmux answers DA1 itself and nothing else. Stage two: the
        // terminal behind it, forwarded.
        let mut answer = read_answer(b"\x1b[?1;2;4c");
        let wrapped = read_answer(b"\x1b_Gi=32;OK\x1b\\\x1b[?0u\x1b[?62;22c");
        answer.wrapped_graphics = wrapped.wrapped_graphics;
        assert_eq!(judge(&answer), Verdict::ThroughTmux);
    }

    #[test]
    fn a_raw_ok_is_direct_even_when_tmux_is_in_the_environment() {
        let answer = read_answer(b"\x1b_Gi=31;OK\x1b\\\x1b[?0u\x1b[?62;22c");
        assert_eq!(judge(&answer), Verdict::Direct);
        assert_eq!(refusal(Verdict::Direct, &in_tmux()), None);
    }

    #[test]
    fn da1_and_no_ok_either_way_is_no_graphics_and_silence_is_no_answer() {
        assert_eq!(judge(&read_answer(b"\x1b[?62;22c")), Verdict::NoGraphics);
        assert_eq!(
            judge(&read_answer(b"\x1b_Gi=31;EINVAL:no\x1b\\\x1b[?62c")),
            Verdict::NoGraphics
        );
        assert_eq!(judge(&read_answer(b"")), Verdict::NoAnswer);
        assert!(!Verdict::NoGraphics.draws() && !Verdict::NoAnswer.draws());
        assert!(Verdict::Skipped.draws(), "--no-probe assumes it draws");
    }

    #[test]
    fn the_refusal_names_tmux_when_tmux_is_in_the_environment_and_screen_when_screen_is() {
        let tmux = refusal(Verdict::NoGraphics, &in_tmux()).expect("a refusal");
        assert!(tmux.contains("allow-passthrough on"), "{tmux}");
        let screen = Env {
            screen: true,
            ..Env::default()
        };
        let screen = refusal(Verdict::NoGraphics, &screen).expect("a refusal");
        assert!(screen.contains("GNU screen"), "{screen}");
        let silent = refusal(Verdict::NoAnswer, &Env::default()).expect("a refusal");
        assert!(silent.contains("--no-probe"), "{silent}");
        for sentence in [&tmux, &screen, &silent] {
            assert!(!sentence.contains('\n'), "one line: {sentence}");
        }
    }

    #[test]
    fn the_refusal_quotes_term_and_names_the_four_terminals_otherwise() {
        let env = Env {
            term: "xterm-256color".to_string(),
            ..Env::default()
        };
        let why = refusal(Verdict::NoGraphics, &env).expect("a refusal");
        assert!(why.contains("TERM=xterm-256color"), "{why}");
        for name in ["Kitty", "WezTerm", "Ghostty", "tOS"] {
            assert!(why.contains(name), "{why}");
        }
        assert!(why.contains("--doctor"), "{why}");
    }

    #[test]
    fn the_doctor_prints_the_route_the_run_would_take() {
        let env = in_tmux();
        let mut answer = read_answer(b"\x1b[?1;2;4c\x1b[6;16;8t");
        answer.wrapped_graphics = Some(Ok(()));
        let verdict = judge(&answer);
        let route = route::choose(&env, route::Choices::default(), verdict, true);
        let facts = terminal_facts(verdict, &answer, &env, &route, Some((100, 30)));
        assert_eq!(
            facts,
            [
                "graphics protocol: yes, through tmux passthrough",
                "keyboard protocol: not used inside tmux (tmux re-encodes keys)",
                "mouse in pixels: no (cells; tmux)",
                "background: not answered (tmux)",
                "cell: 8x16 px, 100x30 cells",
                "route: png frames, unicode placeholders, wrapped for tmux, inline, 30 fps cap",
            ]
        );

        let direct = read_answer(b"\x1b_Gi=31;OK\x1b\\\x1b[?0u\x1b[?62c");
        let route = route::choose(
            &Env::default(),
            route::Choices::default(),
            Verdict::Direct,
            true,
        );
        let facts = terminal_facts(Verdict::Direct, &direct, &Env::default(), &route, None);
        assert_eq!(facts[0], "graphics protocol: yes");
        assert_eq!(facts[1], "keyboard protocol: yes (no flags pushed)");
        assert!(
            facts.last().unwrap().starts_with("route: raw frames"),
            "{facts:?}"
        );

        let silent = terminal_facts(
            Verdict::NoAnswer,
            &TerminalAnswer::default(),
            &env,
            &route,
            None,
        );
        assert_eq!(silent.len(), 1);
        assert!(
            !silent[0].contains("tmux"),
            "tmux answers; it is not the silent one"
        );
    }
}
