//! The pane: what is turned on at the start, off at the end, and drawn in
//! between.
//!
//! A program that takes the alternate screen, hides the cursor, turns on mouse
//! reporting and bracketed paste and pushes keyboard flags has made five
//! changes to a terminal that belongs to somebody else. Every one of them has
//! to be undone on every way out — a clean quit, a `SIGTERM`, a panic, the
//! engine dying — because the thing left behind otherwise is a shell with no
//! cursor that reports every mouse move as garbage on the command line, and
//! the person's next move is to close the pane.
//!
//! Which is why the terminal state lives in a static here as well as in a
//! guard. A release build aborts on panic (`panic = "abort"`, kept in this
//! crate's release profile for that reason), so `Drop` is not a guarantee;
//! [`emergency`] is the same
//! restoration written so it can run from a panic hook or a signal path, with
//! nothing borrowed and one `write(2)`.

use std::borrow::Cow;
use std::io::{self, Write};
use std::os::unix::io::RawFd;
use std::sync::Mutex;

use tos_preview::fit::Metrics;

/// The keyboard flags this program asks for.
///
/// Disambiguate so that `ctrl+i` is not `tab`; event types so that a page sees
/// key releases; alternate keys so that a shifted key reports what it made;
/// associated text so that a key that typed something says what. Not
/// `REPORT_ALL_KEYS_AS_ESCAPE`: it would make every printable key an escape
/// sequence, which is more parsing for no more information, since the text is
/// already reported.
pub const KEYBOARD_FLAGS: u8 = 1 | 2 | 4 | 16;

/// Everything turned on at the start.
///
/// Bracketed paste (`?2004h`) is the one here that is not about drawing or
/// pointing. Without it a terminal delivers a paste as though it had been
/// typed, and a browser cannot afford that: every newline in the clipboard is
/// an Enter, which in a form's `<input>` is a submission per line, and every
/// escape byte is the start of a key. With it the terminal wraps the paste in
/// `CSI 200 ~` and `CSI 201 ~`, [`crate::input`] hands over what is between
/// them as one piece of text, and the page is given it as text — one
/// `Input.insertText`, which fires no key at all.
///
/// Every motion is asked for (`?1003h`), not only motion with a button held
/// (`?1002h`, which it includes): a link under a pointer at rest is what the
/// row names ([`crate::hover`]), and a page that sees the pointer move is a
/// page whose hover styling and tooltips work. What it costs is on the pty.
/// tOS's compositor forwards every pointer event evdev gives it to the pane
/// under the pointer, with no "same cell" check, so a 1000 Hz mouse moving is
/// a thousand sixteen-byte reports a second — 16 kB/s into an 8 KiB read, a
/// few hundred reports a pass — and none while it rests. Parsing them is
/// microseconds; what matters is what is sent on for each, which is why the
/// page is told and asked at most once a pass and not once a report. Kitty,
/// WezTerm and Ghostty in cell mode report a motion only when the cell
/// changes.
pub fn enter_sequence() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b[?1049h"); // the alternate screen
    out.extend_from_slice(b"\x1b[?25l"); // no cursor
    out.extend_from_slice(b"\x1b[?1000h"); // report buttons
    out.extend_from_slice(b"\x1b[?1003h"); // and every motion, held or not
    out.extend_from_slice(b"\x1b[?1006h"); // in SGR, which has no 223 limit
    out.extend_from_slice(b"\x1b[?1016h"); // in pixels, if the terminal can
    out.extend_from_slice(b"\x1b[?2004h"); // a paste as a paste, not as keys
    out.extend_from_slice(format!("\x1b[>{KEYBOARD_FLAGS}u").as_bytes());
    out.extend_from_slice(b"\x1b[2J"); // an empty screen to draw on
    out
}

/// The question that decides what mouse coordinates mean.
///
/// Asked after the mode is set, because DECRQM answers with the state as it is
/// now: a terminal that took `?1016h` answers 1, one that ignored it answers 2
/// if it knows the mode and 0 if it does not, and only the first is a licence
/// to treat coordinates as pixels.
pub const ASK_PIXEL_MOUSE: &[u8] = b"\x1b[?1016$p";

/// Everything turned off at the end, in the reverse order.
///
/// With the pointer's shape put back to the arrow as well: it is not set at
/// the start, but a hand left over from a link the pointer was on when the
/// program ended — or panicked, since [`emergency`] writes this too — would
/// be the shell's pointer from then on, in a terminal that understands it.
pub fn leave_sequence() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b[<u"); // pop the keyboard flags
    out.extend_from_slice(b"\x1b[?2004l");
    out.extend_from_slice(b"\x1b[?1016l");
    out.extend_from_slice(b"\x1b[?1006l");
    out.extend_from_slice(b"\x1b[?1003l");
    out.extend_from_slice(b"\x1b[?1000l");
    out.extend_from_slice(&pointer_shape("default"));
    out.extend_from_slice(b"\x1b[?25h"); // the cursor comes back
    out.extend_from_slice(b"\x1b[?1049l"); // and so does the screen
    out
}

/// Tell the terminal what the pointer should look like: `OSC 22 ; name ST`.
///
/// Kitty's pointer-shape protocol, which Ghostty speaks too; every terminal
/// that does not know OSC 22 drops it, tOS's included, so it is sent without
/// asking. The name is a `&'static str` on purpose: it is always one of
/// [`crate::hover::Shape::name`]'s, a table this program owns, and never the
/// page's `cursor` string, which only chooses among them.
pub fn pointer_shape(name: &'static str) -> Vec<u8> {
    let mut out = b"\x1b]22;".to_vec();
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

/// The terminal settings as they were before this program touched them.
///
/// A static rather than only a field, so that the panic hook can put them back
/// without a reference to anything.
static SAVED: Mutex<Option<libc::termios>> = Mutex::new(None);

/// Put the terminal back, from anywhere.
///
/// Deliberately not a method and deliberately not fallible: it runs where
/// there is nothing left to report an error to.
pub fn emergency() {
    let bytes = leave_sequence();
    // SAFETY: `bytes` is alive for the call and `bytes.len()` is exactly how
    // much of it there is. `write(2)` rather than `println!` because this runs
    // from a panic hook, where the usual machinery may be the thing that
    // broke; the result is dropped because there is nothing left to report to.
    unsafe {
        libc::write(1, bytes.as_ptr() as *const libc::c_void, bytes.len());
    }
    if let Ok(mut saved) = SAVED.lock() {
        if let Some(termios) = saved.take() {
            // SAFETY: `tcsetattr(3)` only reads through the pointer, and
            // `termios` is a live local. 0 is stdin, which is where the
            // settings came from.
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &termios);
            }
        }
    }
}

/// The pane, in the state this program needs it, for as long as it is held.
pub struct Pane {
    input: RawFd,
    output: RawFd,
    restored: bool,
}

impl Pane {
    /// Take the terminal: raw mode, alternate screen, mouse, keyboard flags.
    ///
    /// Raw mode is done here rather than with [`tos_platform::tty::RawMode`]
    /// because the settings have to be saved somewhere a panic hook can reach
    /// them, and a guard that owns its copy cannot be that place.
    pub fn enter(input: RawFd, output: RawFd) -> io::Result<Pane> {
        // SAFETY: `termios` is a C struct of integers and byte arrays, and
        // all-zero is a valid value of every one of its fields. It is handed
        // straight to `tcgetattr` below, which overwrites it.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: writes one `termios` through a pointer to a live local.
        if unsafe { libc::tcgetattr(input, &mut saved) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = saved;
        // SAFETY: `cfmakeraw(3)` edits the struct in place; `raw` is a live
        // local and is the copy, so `saved` still holds what to put back.
        unsafe { libc::cfmakeraw(&mut raw) };
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: read-only through the pointer, and `raw` is a live local.
        if unsafe { libc::tcsetattr(input, libc::TCSANOW, &raw) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if let Ok(mut slot) = SAVED.lock() {
            *slot = Some(saved);
        }

        let mut pane = Pane {
            input,
            output,
            restored: false,
        };
        pane.write(&enter_sequence())?;
        pane.write(ASK_PIXEL_MOUSE)?;
        Ok(pane)
    }

    /// Write bytes to the terminal.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(bytes)?;
        stdout.flush()
    }

    pub fn input_fd(&self) -> RawFd {
        self.input
    }

    pub fn output_fd(&self) -> RawFd {
        self.output
    }

    /// Measure the pane again, which is what a `SIGWINCH` means.
    pub fn metrics(&self) -> io::Result<Metrics> {
        Metrics::probe(self.output)
    }

    /// Put everything back, now.
    pub fn leave(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        let _ = self.write(&leave_sequence());
        if let Ok(mut saved) = SAVED.lock() {
            if let Some(termios) = saved.take() {
                // SAFETY: as in `emergency`: read-only through a pointer to a
                // live local, and the descriptor is the one `enter` took.
                unsafe {
                    libc::tcsetattr(self.input, libc::TCSANOW, &termios);
                }
            }
        }
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.leave();
    }
}

/// The top row: what page this is.
///
/// Drawn in reverse video across the whole width so that the page below it
/// cannot be mistaken for part of it, and so that a picture that is one cell
/// too tall covers something that is already there rather than the first line
/// of the page.
///
/// The row is the only text this program writes to the terminal, and most of
/// what is on it is the page's. Everything on it goes through [`clip_to`],
/// [`tail_to`] or `head_to`, which is where it is made plain text for the
/// last time.
///
/// What is being typed into the url bar used to be drawn from here as well,
/// with the url as an extra argument. It is [`prompt_line`] now, called with
/// what [`crate::line::Line::view`] chose to show, because a line with a
/// cursor in the middle is more than a string and this module does not
/// depend on the one that knows what else it is.
pub fn status_line(cols: u32, text: &str) -> Vec<u8> {
    let cols = cols.max(1) as usize;
    let mut out = b"\x1b[1;1H\x1b[K\x1b[7m".to_vec();
    let shown = clip_to(text, cols);
    out.extend_from_slice(shown.as_bytes());
    out.extend(std::iter::repeat_n(
        b' ',
        cols.saturating_sub(width(&shown)),
    ));
    out.extend_from_slice(b"\x1b[0m\x1b[?25l");
    out
}

/// The cells left for typing after `prompt`, clipped the way [`prompt_line`]
/// will clip it: the room a [`crate::line::Line::view`] is asked to fill.
pub fn prompt_room(cols: u32, prompt: &str) -> usize {
    prompt_room_beside(cols, prompt, "")
}

/// [`prompt_room`] less what `right` will take at the end of the row: its
/// width and two cells of gap, or nothing when `right` is empty. The room
/// [`prompt_line_beside`] leaves for the typing.
pub fn prompt_room_beside(cols: u32, prompt: &str, right: &str) -> usize {
    let left = left_of(cols.max(1) as usize, right);
    left.saturating_sub(width(&clip_to(prompt, left)))
}

/// The cells a row leaves before `right` at its end: all of them for nothing
/// there, or what is left of the pane after `right` — clipped by the pane
/// alone — and two cells of gap, so that the typing and the words beside it
/// do not read as one.
fn left_of(cols: usize, right: &str) -> usize {
    let right = clip_to(right, cols);
    if right.is_empty() {
        return cols;
    }
    cols.saturating_sub(width(&right) + 2)
}

/// The top row as a line being typed into: a prompt, what has been typed
/// after it, the rest of a suggestion dim after that, and the cursor where
/// the typing is.
///
/// `text`, `hint` and `cursor` are a [`crate::line::View`]'s fields, passed
/// apart so that this module depends on nothing above it. The view has
/// already chosen what fits in [`prompt_room`]; this draws it, and is still
/// the floor under a caller that did not ask — what does not fit is cut, the
/// typing from its front and the hint from its end, and a cursor past what
/// is shown is put at the end of it.
///
/// The row with no hint is the url bar's row from before it had a cursor,
/// byte for byte, for as long as the url fits: the dim pair is written only
/// when there is something to dim. Dim is the one attribute the row can add
/// without a colour, and colours are what it has always done without — in
/// reverse video it reads as a fainter cell in every terminal this runs in,
/// which is what a suggestion not yet taken should look like.
///
/// A prompt wider than the pane is clipped rather than left to push the
/// cursor off the edge — which a `url: ` never is, and a page's question can
/// be. [`dialog_prompt`] clips it long before that; this is the floor under
/// it.
pub fn prompt_line(cols: u32, prompt: &str, text: &str, hint: &str, cursor: usize) -> Vec<u8> {
    prompt_line_beside(cols, prompt, text, hint, cursor, "")
}

/// [`prompt_line`] with `right` at the right-hand end of the row: what the
/// find prompt's count (`3/17`) is drawn as.
///
/// `right` is clipped by the pane alone and the typing by what it leaves, the
/// way [`split_line`] keeps a dialog's keys: the count is the answer to what
/// is being typed, and a needle whose count had been pushed off the row would
/// be a question with nowhere to read the answer. The view the caller passes
/// is the one [`prompt_room_beside`] asked for, so the cursor is in sight of
/// the count rather than under it. With `right` empty this is
/// [`prompt_line`], byte for byte — which is what [`prompt_line`] is — so the
/// url bar and a `prompt()` did not change by a byte when this came.
pub fn prompt_line_beside(
    cols: u32,
    prompt: &str,
    text: &str,
    hint: &str,
    cursor: usize,
    right: &str,
) -> Vec<u8> {
    let cols = cols.max(1) as usize;
    let mut out = b"\x1b[1;1H\x1b[K\x1b[7m".to_vec();
    let right = clip_to(right, cols);
    let left = left_of(cols, &right);
    let prompt = clip_to(prompt, left);
    let room = left.saturating_sub(width(&prompt));
    let shown = tail_to(text, room);
    let hint = head_to(hint, room.saturating_sub(width(&shown)));
    out.extend_from_slice(prompt.as_bytes());
    out.extend_from_slice(shown.as_bytes());
    if !hint.is_empty() {
        out.extend_from_slice(b"\x1b[2m");
        out.extend_from_slice(hint.as_bytes());
        out.extend_from_slice(b"\x1b[22m");
    }
    let used = width(&prompt) + width(&shown) + width(&hint);
    out.extend(std::iter::repeat_n(
        b' ',
        cols.saturating_sub(used + width(&right)),
    ));
    out.extend_from_slice(right.as_bytes());
    out.extend_from_slice(b"\x1b[0m");
    // The cursor is put back where the typing is, and shown, because this is
    // the one moment the person is editing rather than watching.
    let column = width(&prompt) + cursor.min(width(&shown)) + 1;
    out.extend_from_slice(format!("\x1b[1;{column}H\x1b[?25h").as_bytes());
    out
}

/// A dialog's question as the prompt of a line being typed: cut to half the
/// pane, so that there is room to see the answer, and a space after it; or
/// nothing, for a `prompt()` that asked nothing.
pub fn dialog_prompt(cols: u32, caption: &str) -> String {
    if caption.is_empty() {
        return String::new();
    }
    let half = (cols.max(1) / 2) as usize;
    format!("{} ", clip_to(caption, half))
}

/// The top row while the page in front is waiting on a dialog: the question,
/// and the keys that answer it at the right-hand end.
///
/// The hint is what survives a narrow pane, not the question. A question cut
/// to `Delete all…` still says there is a question, and the row being taken
/// over says the rest; a row with no hint is a page that has stopped for no
/// reason anybody can see and no key anybody knows. So the caption is clipped
/// to what the hint leaves, less two cells of gap so that the two do not read
/// as one sentence.
///
/// A `prompt()` is not drawn here but by [`prompt_line`], with
/// [`dialog_prompt`] as its prompt. The hint goes then — Enter and Escape are
/// what a line has always been finished with, and the half of the row it
/// would take is the half the answer needs.
pub fn dialog_line(cols: u32, caption: &str, hint: &str) -> Vec<u8> {
    split_line(cols, caption, hint)
}

/// The top row in two halves: `left` from the start, `right` at the end.
///
/// What a dialog's question and its keys are drawn as, and what a download's
/// progress beside the tab's own line is. The right-hand half is the one that
/// survives a narrow pane, because in both cases it is the news: the keys
/// that answer a question nobody could otherwise see how to answer, the file
/// that is arriving while the page it came from stays where it was. So `left`
/// is clipped to what `right` leaves, less two cells of gap so that the two
/// do not read as one sentence, and `right` is clipped only by the pane.
pub fn split_line(cols: u32, left: &str, right: &str) -> Vec<u8> {
    let cols = cols.max(1) as usize;
    let mut out = b"\x1b[1;1H\x1b[K\x1b[7m".to_vec();
    let right = clip_to(right, cols);
    let room = cols.saturating_sub(width(&right) + 2);
    let shown = clip_to(left, room);
    out.extend_from_slice(shown.as_bytes());
    out.extend(std::iter::repeat_n(
        b' ',
        cols.saturating_sub(width(&shown) + width(&right)),
    ));
    out.extend_from_slice(right.as_bytes());
    out.extend_from_slice(b"\x1b[0m\x1b[?25l");
    out
}

/// One tab, as the strip needs it: a name, whether it is the one in front,
/// and whether it is waiting on an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabLabel<'a> {
    pub title: &'a str,
    pub active: bool,
    /// The page has a dialog open, and has stopped until it is answered.
    ///
    /// Drawn as a `!` after the tab's number — `2! Title` — because a tab
    /// that is not in front cannot show its question, and a page stopped
    /// behind one looks, from the strip, exactly like a page that is loading
    /// slowly. ASCII on purpose: the warning signs a font would draw better
    /// are ambiguous-width in East Asian terminals, and a strip that is one
    /// cell out is a strip that wraps.
    pub dialog: bool,
}

/// How much room a url has to be worth putting after the strip.
///
/// Less than this and what is shown is a scheme and an ellipsis, which says
/// nothing and takes the space the titles wanted.
const URL_MINIMUM: usize = 12;

/// The top row when there is more than one tab: `1 title  2 title  3 title`,
/// and the active tab's url after them if anything is left over.
///
/// The whole row is reverse video, as [`status_line`] draws it, so the active
/// tab cannot be marked by reversing it again — it is *un*-reversed instead
/// (`\x1b[27m`), which against a reversed row is the same emphasis the other
/// way round and needs no colour. Colour would have to be chosen against a
/// theme this program cannot see.
pub fn tab_line(cols: u32, tabs: &[TabLabel], url: &str) -> Vec<u8> {
    let cols = cols.max(1) as usize;
    let mut out = b"\x1b[1;1H\x1b[K\x1b[7m".to_vec();
    let mut used = 0;
    for (text, active) in strip(cols, tabs, url) {
        used += width(&text);
        if active {
            out.extend_from_slice(b"\x1b[27m");
            out.extend_from_slice(text.as_bytes());
            out.extend_from_slice(b"\x1b[7m");
        } else {
            out.extend_from_slice(text.as_bytes());
        }
    }
    out.extend(std::iter::repeat_n(b' ', cols.saturating_sub(used)));
    out.extend_from_slice(b"\x1b[0m\x1b[?25l");
    out
}

/// The strip as the runs it is drawn in: the text, and whether it is the
/// active tab and so emphasised.
///
/// Separate from the escapes so that what fits can be tested as what fits.
fn strip(cols: usize, tabs: &[TabLabel], url: &str) -> Vec<(String, bool)> {
    // What every tab costs before its title: its number, the mark of a
    // dialog if it has one, a space, and the two spaces that separate it from
    // the one before.
    let fixed: usize = tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| {
            number_width(index + 1) + usize::from(tab.dialog) + 1 + if index == 0 { 0 } else { 2 }
        })
        .sum();
    let budget = cols.saturating_sub(fixed);
    // Measured as they will be drawn. `clip_to` would clean them anyway, but
    // after the room was shared out, and a title of forty zero-width spaces
    // would be given forty cells and show nothing in them.
    let names: Vec<Cow<str>> = tabs
        .iter()
        .map(|tab| crate::text::sanitize(tab.title))
        .collect();
    let url = crate::text::sanitize(url);
    let wanted: Vec<usize> = names.iter().map(|name| width(name)).collect();
    let given = shares(budget, &wanted);

    let mut runs: Vec<(String, bool)> = Vec::new();
    let mut used = 0;
    for (index, tab) in tabs.iter().enumerate() {
        if index > 0 {
            runs.push(("  ".to_string(), false));
            used += 2;
        }
        let mark = if tab.dialog { "!" } else { "" };
        let text = format!(
            "{}{mark} {}",
            index + 1,
            clip_to(&names[index], given[index])
        );
        used += width(&text);
        runs.push((text, tab.active));
    }

    // The url gets whatever the titles did not want, and only if that is
    // enough to read: the strip is what this row is for now.
    let left = cols.saturating_sub(used);
    if !url.is_empty() && left >= URL_MINIMUM + 2 {
        runs.push((format!("  {}", clip_to(&url, left - 2)), false));
    }

    // Titles can be clipped to nothing but numbers cannot, so a pane narrower
    // than `1  2  3 …` still has more strip than row. The row wins: what is
    // past the edge is cut, and the numbers that are left still say which tab
    // is which, because the order never changes.
    let mut fitted = Vec::with_capacity(runs.len());
    let mut used = 0;
    for (text, active) in runs {
        let room = cols - used;
        if width(&text) <= room {
            used += width(&text);
            fitted.push((text, active));
            continue;
        }
        if room > 0 {
            fitted.push((clip_to(&text, room), active));
        }
        break;
    }
    fitted
}

/// Share `budget` cells between titles that want `wanted`.
///
/// Equally, except that a title which wants less than its share gives the rest
/// back to the ones that want more — so two short titles and one long one show
/// the long one rather than three equal stumps. What nobody gets is a share of
/// zero while somebody else has room to spare.
fn shares(mut budget: usize, wanted: &[usize]) -> Vec<usize> {
    let mut given = vec![0usize; wanted.len()];
    let mut open: Vec<usize> = (0..wanted.len()).collect();
    while !open.is_empty() {
        let each = budget / open.len();
        if each == 0 {
            break;
        }
        let modest: Vec<usize> = open
            .iter()
            .copied()
            .filter(|&i| wanted[i] <= each)
            .collect();
        if modest.is_empty() {
            let spare = budget - each * open.len();
            for (rank, &i) in open.iter().enumerate() {
                given[i] = each + usize::from(rank < spare);
            }
            break;
        }
        for i in modest {
            given[i] = wanted[i];
            budget -= wanted[i];
            open.retain(|&open| open != i);
        }
    }
    given
}

/// How many cells a tab's number takes.
fn number_width(n: usize) -> usize {
    if n < 10 {
        1
    } else {
        n.to_string().len()
    }
}

/// How wide a string is in cells, counting each cluster once.
///
/// The ranges are the East Asian wide and fullwidth blocks, which is what a
/// tOS pane draws at two cells, and the emoji the same terminals draw at two.
/// It is not the whole of UAX #11 — and [`clusters`] is not the whole of
/// UAX #29 — because the cost of being wrong here is a row one cell short,
/// and the cost of a table is a table. What it does know is that a combining
/// mark sits on the letter before it: an `é` typed as `e` and U+0301 is one
/// cell, not two, which is the difference between a cursor that lands on the
/// letter it is after and one that lands a cell past it.
pub fn width(text: &str) -> usize {
    clusters(text)
        .into_iter()
        .map(|range| cluster_width(&text[range]))
        .sum()
}

/// The cells one cluster takes: its first character's, or two for an emoji
/// that asked for its picture (U+FE0F after a narrow character, `☺️`) and for
/// a flag, which is two regional indicators drawn as one.
fn cluster_width(cluster: &str) -> usize {
    let mut chars = cluster.chars();
    let Some(base) = chars.next() else {
        return 0;
    };
    let flag = regional(base) && chars.next().is_some_and(regional);
    if flag || cluster.contains('\u{fe0f}') {
        2
    } else {
        char_width(base)
    }
}

fn char_width(c: char) -> usize {
    let c = c as u32;
    let wide = (0x1100..=0x115f).contains(&c)
        || (0x2e80..=0xa4cf).contains(&c)
        || (0xac00..=0xd7a3).contains(&c)
        || (0xf900..=0xfaff).contains(&c)
        || (0xfe30..=0xfe6f).contains(&c)
        || (0xff00..=0xff60).contains(&c)
        || (0xffe0..=0xffe6).contains(&c)
        || (0x1f1e6..=0x1f1ff).contains(&c)
        || (0x1f300..=0x1f9ff).contains(&c);
    if wide {
        2
    } else {
        1
    }
}

/// Whether `c` is one of the twenty-six regional indicators, two of which
/// spell a flag.
fn regional(c: char) -> bool {
    ('\u{1f1e6}'..='\u{1f1ff}').contains(&c)
}

/// Whether `c` attaches to the character before it rather than taking a cell
/// of its own.
///
/// Not UAX #29's Grapheme_Extend, which is a table of some two thousand code
/// points: the nonspacing marks of the scripts a title or a url is written
/// in, and the characters that join, select or modify. Each range is here
/// because a person can type or paste it, and a cursor that stops between a
/// letter and its accent is a backspace that takes the accent and leaves the
/// letter looking untouched.
///
/// Some of these never reach a line or the row, because
/// [`crate::text::sanitize`] takes them out first — the joiners and the tag
/// characters are invisible, and invisible is what it removes. They are
/// listed all the same, so that [`width`] is right about a string that has
/// not been through it.
pub fn extends(c: char) -> bool {
    matches!(c,
        // Combining diacritical marks: the general blocks.
        '\u{300}'..='\u{36f}'
        | '\u{1ab0}'..='\u{1aff}'
        | '\u{1dc0}'..='\u{1dff}'
        | '\u{20d0}'..='\u{20ff}'
        | '\u{fe20}'..='\u{fe2f}'
        // Cyrillic.
        | '\u{483}'..='\u{489}'
        // Hebrew points.
        | '\u{591}'..='\u{5bd}'
        | '\u{5bf}'
        | '\u{5c1}'..='\u{5c2}'
        | '\u{5c4}'..='\u{5c5}'
        | '\u{5c7}'
        // Arabic marks.
        | '\u{610}'..='\u{61a}'
        | '\u{64b}'..='\u{65f}'
        | '\u{670}'
        | '\u{6d6}'..='\u{6dc}'
        | '\u{6df}'..='\u{6e4}'
        | '\u{6e7}'..='\u{6e8}'
        | '\u{6ea}'..='\u{6ed}'
        // Devanagari, the nonspacing ones only: a spacing sign takes a cell.
        | '\u{900}'..='\u{902}'
        | '\u{93a}'
        | '\u{93c}'
        | '\u{941}'..='\u{948}'
        | '\u{94d}'
        | '\u{951}'..='\u{957}'
        | '\u{962}'..='\u{963}'
        // Thai.
        | '\u{e31}'
        | '\u{e34}'..='\u{e3a}'
        | '\u{e47}'..='\u{e4e}'
        // Kana voicing marks as combining characters, which an IME can
        // produce instead of the precomposed kana.
        | '\u{3099}'..='\u{309a}'
        // The joiners, the variation selectors, the skin tones and the tags
        // that spell a subdivision's flag.
        | '\u{200c}'..='\u{200d}'
        | '\u{fe00}'..='\u{fe0f}'
        | '\u{e0100}'..='\u{e010f}'
        | '\u{1f3fb}'..='\u{1f3ff}'
        | '\u{e0020}'..='\u{e007f}'
    )
}

/// The byte ranges of the clusters of `text`, in order: what a person sees
/// as one character, and so what a cursor steps over and a backspace takes.
///
/// A cluster is a character and every mark that [`extends`] it; a zero-width
/// joiner also takes the character after it, so that a family made of three
/// people joined is one cluster; and a regional indicator takes the one
/// after it, once, so that two flags side by side are two clusters and not
/// one. A text that starts with a mark starts with a cluster that is only
/// marks, since there is nothing for them to sit on. Every boundary is a
/// `char` boundary, and the ranges cover the text with no gaps.
///
/// What it does not know, and says so: conjoining Hangul jamo (an IME
/// produces precomposed syllables, and a url never has jamo), Indic conjuncts
/// beyond the marks listed, and keycap sequences. Being wrong about one of
/// those is a cursor that stops inside a glyph and a row a cell off; being
/// right about all of them is a table.
pub fn clusters(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, base)) = chars.next() {
        let mut end = start + base.len_utf8();
        // Whether the last character taken was a joiner, which takes the
        // next one whatever it is.
        let mut joined = false;
        while let Some(&(at, c)) = chars.peek() {
            let pair = regional(base) && end == start + base.len_utf8() && regional(c);
            if !(joined || extends(c) || pair) {
                break;
            }
            joined = c == '\u{200d}';
            end = at + c.len_utf8();
            chars.next();
        }
        out.push(start..end);
    }
    out
}

/// As much of the front of a string as fits, with an ellipsis when it does not.
///
/// Sanitized first ([`crate::text::sanitize`]), whatever the caller did —
/// this is the last function a string goes through before the terminal, and
/// the invariant `nothing_a_page_says_can_speak_to_the_terminal` tests is
/// kept here. Cut between clusters, so a letter never loses its accent.
pub fn clip_to(text: &str, cols: usize) -> String {
    let text = crate::text::sanitize(text);
    if width(&text) <= cols {
        return text.into_owned();
    }
    if cols <= 1 {
        return "…".chars().take(cols).collect();
    }
    let mut out = head_to(&text, cols - 1);
    out.push('…');
    out
}

/// As much of the front of a string as fits, and no ellipsis: what is left
/// of a suggestion after the typing, which is not cut short but only runs off
/// the edge. Sanitized first, as [`clip_to`] is.
fn head_to(text: &str, cols: usize) -> String {
    let text = crate::text::sanitize(text);
    let mut used = 0;
    let mut end = 0;
    for range in clusters(&text) {
        let cells = cluster_width(&text[range.clone()]);
        if used + cells > cols {
            break;
        }
        used += cells;
        end = range.end;
    }
    text[..end].to_string()
}

/// As much of the end of a string as fits, which is where a url is typed.
///
/// Sanitized first ([`crate::text::sanitize`]), whatever the caller did —
/// this is the last function a string goes through before the terminal, and
/// the invariant `nothing_a_page_says_can_speak_to_the_terminal` tests is
/// kept here. It matters most here: what is being typed is whatever was
/// pasted, and nothing between the paste and the row has filtered it. Cut
/// between clusters, as [`clip_to`] is.
pub fn tail_to(text: &str, cols: usize) -> String {
    let text = crate::text::sanitize(text);
    if width(&text) <= cols {
        return text.into_owned();
    }
    let mut used = 0;
    let mut start = text.len();
    for range in clusters(&text).into_iter().rev() {
        let cells = cluster_width(&text[range.clone()]);
        if used + cells > cols {
            break;
        }
        used += cells;
        start = range.start;
    }
    text[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).to_string()
    }

    /// The row a line being typed after `prompt` is drawn as, with the cursor
    /// at the end of `typed`: what the url bar and a `prompt()` both do.
    fn typing(cols: u32, prompt: &str, typed: &str) -> Vec<u8> {
        let mut line = crate::line::Line::empty();
        line.set_text(typed);
        let view = line.view(prompt_room(cols, prompt));
        prompt_line(cols, prompt, &view.text, &view.hint, view.cursor)
    }

    /// The url bar's row with `typed` in it.
    fn url_bar(cols: u32, typed: &str) -> Vec<u8> {
        typing(cols, "url: ", typed)
    }

    /// A `prompt()`'s row: its question, and `typed` as the answer.
    fn answering(cols: u32, caption: &str, typed: &str) -> Vec<u8> {
        typing(cols, &dialog_prompt(cols, caption), typed)
    }

    #[test]
    fn everything_turned_on_is_turned_off_again() {
        let on = text(&enter_sequence());
        let off = text(&leave_sequence());
        for (set, reset) in [
            ("?1049h", "?1049l"),
            ("?25l", "?25h"),
            ("?1000h", "?1000l"),
            ("?1003h", "?1003l"),
            ("?1006h", "?1006l"),
            ("?1016h", "?1016l"),
            ("?2004h", "?2004l"),
        ] {
            assert!(on.contains(set), "{set} is never set");
            assert!(off.contains(reset), "{set} is never unset");
        }
        assert!(on.contains("\x1b[>23u"), "the flags are pushed: {on:?}");
        assert!(off.contains("\x1b[<u"), "and popped: {off:?}");
    }

    #[test]
    fn the_flags_are_the_four_that_were_asked_for() {
        // DISAMBIGUATE | REPORT_EVENT_TYPES | REPORT_ALTERNATE_KEYS |
        // REPORT_ASSOCIATED_TEXT, and not REPORT_ALL_KEYS_AS_ESCAPE.
        assert_eq!(KEYBOARD_FLAGS, 23);
        assert_eq!(KEYBOARD_FLAGS & 8, 0);
    }

    /// The terminal has to make of these bytes what they were meant as.
    #[test]
    fn a_terminal_reads_the_setup_the_way_it_was_written() {
        let mut terminal = tos_term::Terminal::new(80, 24, tos_term::TerminalConfig::default());
        terminal.advance(&enter_sequence());
        assert!(terminal.modes.alt_screen);
        assert!(!terminal.modes.cursor_visible);
        assert_eq!(terminal.keyboard_flags().0, KEYBOARD_FLAGS);
        assert!(terminal.modes.bracketed_paste, "a paste comes bracketed");
        assert_eq!(
            terminal.mouse().tracking,
            tos_term::MouseTracking::AnyEvent,
            "every motion is reported, held or not"
        );
        terminal.advance(b"\x1b[?1003$p");
        let answer = String::from_utf8(terminal.take_output()).expect("ascii");
        assert_eq!(answer, "\x1b[?1003;1$y");

        terminal.advance(ASK_PIXEL_MOUSE);
        let answer = String::from_utf8(terminal.take_output()).expect("ascii");
        // Today's tOS does not know the mode, which is the answer this
        // program has to cope with: cells, not pixels.
        assert!(
            answer == "\x1b[?1016;0$y" || answer == "\x1b[?1016;1$y",
            "unexpected answer {answer:?}"
        );

        terminal.advance(&leave_sequence());
        assert!(!terminal.modes.alt_screen);
        assert!(terminal.modes.cursor_visible);
        assert!(!terminal.modes.bracketed_paste);
        assert_eq!(terminal.keyboard_flags().0, 0);
        assert_eq!(terminal.mouse().tracking, tos_term::MouseTracking::None);
    }

    #[test]
    fn a_pointer_shape_is_an_osc_22_of_one_of_the_tables_names() {
        assert_eq!(pointer_shape("pointer"), b"\x1b]22;pointer\x1b\\");
        // A terminal that does not know it says nothing back and draws
        // nothing: tOS's parser drops an OSC it has no arm for.
        let mut terminal = tos_term::Terminal::new(80, 24, tos_term::TerminalConfig::default());
        terminal.advance(&pointer_shape(crate::hover::Shape::Pointer.name()));
        assert!(terminal.take_output().is_empty());
        assert_eq!(terminal.title(), "");
        assert!(terminal.grid().row(0).to_text().trim().is_empty());
    }

    #[test]
    fn the_status_line_fills_the_width_and_no_more() {
        let line = text(&status_line(20, "tOS — a title"));
        assert!(line.starts_with("\x1b[1;1H\x1b[K\x1b[7m"));
        let body = line
            .trim_start_matches("\x1b[1;1H\x1b[K\x1b[7m")
            .trim_end_matches("\x1b[0m\x1b[?25l");
        assert_eq!(width(body), 20, "{body:?}");
    }

    #[test]
    fn a_long_title_is_cut_and_says_it_was() {
        assert_eq!(clip_to("abcdef", 10), "abcdef");
        assert_eq!(clip_to("abcdef", 6), "abcdef");
        assert_eq!(clip_to("abcdef", 4), "abc…");
        assert_eq!(clip_to("abcdef", 1), "…");
        // A wide character is two cells and is never cut in half.
        assert_eq!(width("\u{65e5}\u{672c}\u{8a9e}"), 6);
        assert_eq!(clip_to("\u{65e5}\u{672c}\u{8a9e}", 4), "\u{65e5}…");
    }

    #[test]
    fn a_url_being_typed_shows_its_end_and_the_cursor() {
        let line = text(&url_bar(20, "https://example.com/a/very/long/path"));
        assert!(line.contains("url: "));
        assert!(line.contains("very/long/path"), "{line:?}");
        assert!(line.ends_with("\x1b[?25h"), "the cursor is shown: {line:?}");
        // At the end of what was typed, on the last cell of the row rather
        // than past it: a line keeps a cell for its cursor.
        assert!(line.contains("\x1b[1;20H"), "{line:?}");
        assert_eq!(
            cells(&url_bar(20, "https://example.com/a/very/long/path")),
            20
        );
    }

    #[test]
    fn a_combining_mark_takes_no_cell_and_stays_with_its_letter() {
        assert_eq!(width("e\u{301}"), 1);
        assert_eq!(width("cafe\u{301}"), 4);
        assert_eq!(clusters("e\u{301}x"), vec![0..3, 3..4]);
        assert_eq!(clusters("a\u{5b8}\u{5bc}b"), vec![0..5, 5..6], "two points");
        // Cut on either side, the letter keeps its accent.
        assert_eq!(clip_to("e\u{301}xyz", 2), "e\u{301}…");
        assert_eq!(tail_to("xe\u{301}", 1), "e\u{301}");
        assert_eq!(head_to("e\u{301}x", 1), "e\u{301}");
        // A mark with nothing before it is a cluster of its own, one cell,
        // which is what it was before there were clusters.
        assert_eq!(clusters("\u{301}a"), vec![0..2, 2..3]);
        assert_eq!(width("\u{301}a"), 2);
        // Every boundary is a character's, and the clusters cover the text.
        let mixed = "a\u{301}日\u{1f468}\u{200d}\u{1f469}\u{1f1ef}\u{1f1f5}x";
        let ranges = clusters(mixed);
        assert_eq!(ranges.first().map(|r| r.start), Some(0));
        assert_eq!(ranges.last().map(|r| r.end), Some(mixed.len()));
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
            assert!(mixed.is_char_boundary(pair[0].end));
        }
    }

    #[test]
    fn an_emoji_sequence_and_a_flag_are_one_cluster_two_cells_wide() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        assert_eq!(clusters(family), vec![0..family.len()]);
        assert_eq!(width(family), 2);
        // A skin tone sits on the hand it colours.
        assert_eq!(clusters("\u{1f44b}\u{1f3fd}").len(), 1);
        assert_eq!(width("\u{1f44b}\u{1f3fd}"), 2);
        // Two flags side by side are two flags, not one and a half.
        let flags = "\u{1f1ef}\u{1f1f5}\u{1f1fa}\u{1f1f8}";
        assert_eq!(clusters(flags), vec![0..8, 8..16]);
        assert_eq!(width(flags), 4);
        assert_eq!(width("\u{1f1ef}"), 2, "a lone indicator is drawn wide too");
        // A narrow character that asked for its picture is two cells.
        assert_eq!(clusters("\u{263a}\u{fe0f}").len(), 1);
        assert_eq!(width("\u{263a}\u{fe0f}"), 2);
        assert_eq!(width("\u{263a}"), 1);
    }

    #[test]
    fn the_prompt_line_draws_the_hint_dim_and_the_cursor_before_it() {
        // Nothing to dim is no dim at all: the url bar as it always was.
        let plain = text(&prompt_line(20, "url: ", "exa", "", 3));
        assert!(!plain.contains("\x1b[2m"), "{plain:?}");
        assert_eq!(
            plain,
            "\x1b[1;1H\x1b[K\x1b[7murl: exa            \x1b[0m\x1b[1;9H\x1b[?25h"
        );
        // A hint goes after the typing, dim, and the cursor stays on its
        // first cell: that is where the next letter goes.
        let hinted = text(&prompt_line(20, "url: ", "exa", "mple.com/", 3));
        assert_eq!(
            hinted,
            "\x1b[1;1H\x1b[K\x1b[7murl: exa\x1b[2mmple.com/\x1b[22m   \x1b[0m\x1b[1;9H\x1b[?25h"
        );
        assert_eq!(cells(hinted.as_bytes()), 20);
        // A hint longer than the room runs off the edge rather than being
        // cut with an ellipsis, and still fills the row exactly.
        let long = prompt_line(12, "url: ", "exa", "mple.com/a/b/c", 3);
        assert_eq!(cells(&long), 12);
        assert!(
            text(&long).contains("\x1b[2mmple\x1b[22m"),
            "{:?}",
            text(&long)
        );
        // A cursor in the middle of the typing is drawn there.
        let middle = text(&prompt_line(20, "url: ", "example.com", "", 2));
        assert!(middle.ends_with("\x1b[1;8H\x1b[?25h"), "{middle:?}");
        // And one past what is shown — a caller that did not ask for a view —
        // is put at the end of it rather than out in the padding.
        let past = text(&prompt_line(10, "url: ", "abc", "", 40));
        assert!(past.ends_with("\x1b[1;9H\x1b[?25h"), "{past:?}");
        assert_eq!(prompt_room(10, "url: "), 5);
        assert_eq!(prompt_room(3, "url: "), 0);
    }

    #[test]
    fn a_prompt_with_nothing_beside_it_is_the_prompt_line_byte_for_byte() {
        for cols in [1u32, 3, 8, 20, 80] {
            for (prompt, typed, hint, cursor) in [
                ("url: ", "example.com", "", 11),
                ("url: ", "exa", "mple.com/", 3),
                ("find: ", "\u{6771}\u{4eac}", "", 1),
                ("Delete these files? ", "", "", 0),
            ] {
                assert_eq!(
                    prompt_line_beside(cols, prompt, typed, hint, cursor, ""),
                    prompt_line(cols, prompt, typed, hint, cursor),
                    "{cols} cols, {prompt:?}"
                );
                assert_eq!(
                    prompt_room_beside(cols, prompt, ""),
                    prompt_room(cols, prompt)
                );
            }
        }
    }

    #[test]
    fn the_count_sits_at_the_right_and_the_typing_gives_way_to_it() {
        let needle = "a needle forty cells wide, or near it...";
        assert_eq!(width(needle), 40);
        let mut line = crate::line::Line::empty();
        line.set_text(needle);
        let room = prompt_room_beside(30, "find: ", "3/17");
        assert_eq!(room, prompt_room(30, "find: ") - 6, "the count and a gap");
        let view = line.view(room);
        let row = text(&prompt_line_beside(
            30,
            "find: ",
            &view.text,
            &view.hint,
            view.cursor,
            "3/17",
        ));
        let body = row
            .trim_start_matches("\x1b[1;1H\x1b[K\x1b[7m")
            .split("\x1b[0m")
            .next()
            .unwrap_or_default();
        assert_eq!(width(body), 30, "{body:?}");
        assert!(body.starts_with("find: "), "{body:?}");
        assert!(body.ends_with("  3/17"), "two cells of gap: {body:?}");
        // The typing is the tail that fits, with the cursor in sight of it
        // and never on the count.
        let typing = &body["find: ".len()..body.len() - "3/17".len()];
        assert!(needle.ends_with(typing.trim_end()), "{typing:?}");
        let column: usize = row
            .rsplit("\x1b[1;")
            .next()
            .and_then(|tail| tail.strip_suffix("H\x1b[?25h"))
            .and_then(|digits| digits.parse().ok())
            .expect("the cursor is put back");
        assert!(column <= 30 - 6, "column {column}: {row:?}");
        assert!(only_the_rows_own_escapes(row.as_bytes()), "{row:?}");

        // No matches is words, and they keep their room too.
        let none = text(&prompt_line_beside(
            24,
            "find: ",
            "zzz",
            "",
            3,
            "no matches",
        ));
        assert!(none.contains("find: zzz"), "{none:?}");
        assert!(none.contains("  no matches\x1b[0m"), "{none:?}");
        assert_eq!(cells(none.as_bytes()), 24);

        // At eight columns the count is still whole, and the prompt gives
        // way to it; narrower than the count, the count as far as it goes.
        let narrow = text(&prompt_line_beside(8, "find: ", "fox", "", 3, "3/17"));
        assert_eq!(cells(narrow.as_bytes()), 8, "{narrow:?}");
        assert!(narrow.contains("3/17\x1b[0m"), "{narrow:?}");
        assert_eq!(prompt_room_beside(8, "find: ", "3/17"), 0);
        let tiny = text(&prompt_line_beside(3, "find: ", "fox", "", 3, "3/17"));
        assert_eq!(cells(tiny.as_bytes()), 3, "{tiny:?}");

        // What is beside the typing is made plain like everything else on
        // the row, whoever wrote it.
        let hostile = prompt_line_beside(40, "find: ", "x", "", 1, "1/2\x1b]0;t\x07");
        assert!(only_the_rows_own_escapes(&hostile), "{:?}", text(&hostile));
    }

    fn labels<'a>(titles: &[&'a str], active: usize) -> Vec<TabLabel<'a>> {
        titles
            .iter()
            .enumerate()
            .map(|(index, title)| TabLabel {
                title,
                active: index == active,
                dialog: false,
            })
            .collect()
    }

    /// What the strip reads as, with the emphasis written as brackets.
    fn strip_text(cols: usize, tabs: &[TabLabel], url: &str) -> String {
        strip(cols, tabs, url)
            .into_iter()
            .map(
                |(text, active)| {
                    if active {
                        format!("[{text}]")
                    } else {
                        text
                    }
                },
            )
            .collect()
    }

    #[test]
    fn the_strip_numbers_the_tabs_and_marks_the_one_in_front() {
        let tabs = labels(&["One", "Two", "Three"], 1);
        assert_eq!(strip_text(80, &tabs, ""), "1 One  [2 Two]  3 Three");
        // The number is part of what is emphasised: it is how the tab is
        // selected, and a number outside the mark would read as a separator.
        let line = String::from_utf8(tab_line(80, &tabs, "")).expect("ascii");
        assert!(line.contains("\x1b[27m2 Two\x1b[7m"), "{line:?}");
        assert!(line.contains("1 One"), "{line:?}");
    }

    #[test]
    fn the_strip_fills_the_width_and_no_more() {
        for cols in [8u32, 13, 20, 40, 80] {
            for count in 2..=9usize {
                let titles: Vec<String> =
                    (1..=count).map(|n| format!("Title number {n}")).collect();
                let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
                let tabs = labels(&refs, count - 1);
                let line = String::from_utf8(tab_line(cols, &tabs, "https://example.com/page"))
                    .expect("ascii");
                let body = line
                    .trim_start_matches("\x1b[1;1H\x1b[K\x1b[7m")
                    .trim_end_matches("\x1b[0m\x1b[?25l")
                    .replace("\x1b[27m", "")
                    .replace("\x1b[7m", "");
                assert_eq!(
                    width(&body),
                    cols as usize,
                    "{cols} cols, {count} tabs: {body:?}"
                );
            }
        }
    }

    #[test]
    fn a_long_title_is_clipped_and_a_short_one_gives_its_room_away() {
        // Three tabs, forty cells: the numbers and separators cost 10, so 30
        // is shared. "ok" wants two and gives back the rest.
        let tabs = labels(
            &["ok", "a title that will not fit in ten cells", "also ok"],
            0,
        );
        let text = strip_text(40, &tabs, "");
        assert_eq!(
            width(&text) - 2,
            40,
            "the brackets are the test's, not the row's"
        );
        assert!(text.starts_with("[1 ok]  2 a title that"), "{text:?}");
        assert!(text.contains('…'), "the long one says it was cut: {text:?}");
        assert!(text.ends_with("3 also ok"), "{text:?}");
    }

    #[test]
    fn the_url_comes_after_the_strip_when_there_is_room_for_it() {
        let tabs = labels(&["A", "B"], 0);
        let wide = strip_text(60, &tabs, "https://example.com/a");
        assert!(wide.ends_with("  https://example.com/a"), "{wide:?}");
        // And not when there is not: half a url is not worth a title.
        let narrow = strip_text(12, &tabs, "https://example.com/a");
        assert_eq!(narrow, "[1 A]  2 B");
    }

    #[test]
    fn more_tabs_than_the_pane_is_wide_is_still_one_row() {
        let titles: Vec<String> = (1..=9).map(|n| format!("Tab {n}")).collect();
        let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
        let tabs = labels(&refs, 0);
        let text = strip_text(10, &tabs, "https://example.com");
        assert!(width(&text) <= 10 + 2, "{text:?}");
        assert!(text.starts_with("[1"), "{text:?}");
    }

    #[test]
    fn the_shares_go_to_the_titles_that_want_them() {
        // Nobody wants more than a third: everybody gets what they asked for.
        assert_eq!(shares(30, &[5, 5, 5]), vec![5, 5, 5]);
        // One wants everything: it gets what the other two left.
        assert_eq!(shares(30, &[2, 100, 3]), vec![2, 25, 3]);
        // Everybody wants more than there is: it is split, and the odd cell
        // goes to the left.
        assert_eq!(shares(10, &[100, 100, 100]), vec![4, 3, 3]);
        // Nothing to share.
        assert_eq!(shares(1, &[10, 10, 10]), vec![0, 0, 0]);
    }

    /// What a row reads as with its own escapes taken off.
    fn row_body(line: &str) -> &str {
        line.trim_start_matches("\x1b[1;1H\x1b[K\x1b[7m")
            .trim_end_matches("\x1b[0m\x1b[?25l")
    }

    #[test]
    fn a_dialog_fills_the_width_and_keeps_its_hint() {
        let captions = [
            "alert: saved",
            "Delete these three files? This cannot be undone.",
            // Wide characters, which are two cells and never cut in half.
            "\u{3053}\u{306e}\u{30da}\u{30fc}\u{30b8}\u{3092}\u{96e2}\u{308c}\u{307e}\u{3059}\u{304b}\u{ff1f}",
            "",
        ];
        for cols in 10u32..=80 {
            for caption in captions {
                for hint in ["any key", "y/n"] {
                    let line = text(&dialog_line(cols, caption, hint));
                    assert!(line.starts_with("\x1b[1;1H\x1b[K\x1b[7m"), "{line:?}");
                    assert!(
                        line.ends_with("\x1b[0m\x1b[?25l"),
                        "no cursor: there is nothing to type: {line:?}"
                    );
                    let body = row_body(&line);
                    assert_eq!(width(body), cols as usize, "{cols} cols: {body:?}");
                    assert!(body.ends_with(hint), "{cols} cols: {body:?}");
                    // And never run into the question.
                    let before = &body[..body.len() - hint.len()];
                    assert!(
                        before.ends_with("  ") || before.trim().is_empty(),
                        "{cols} cols: {body:?}"
                    );
                }
            }
        }
        // With room, all of it; without, as much of the front as fits.
        let wide = text(&dialog_line(40, "Delete 3 files?", "y/n"));
        assert!(row_body(&wide).starts_with("Delete 3 files?  "), "{wide:?}");
        let narrow = text(&dialog_line(16, "Delete 3 files?", "y/n"));
        assert_eq!(row_body(&narrow), "Delete 3 f…  y/n");
        let cjk = text(&dialog_line(
            12,
            "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{8cea}\u{554f}",
            "y/n",
        ));
        assert_eq!(row_body(&cjk), "\u{65e5}\u{672c}\u{8a9e}…  y/n");
        // A pane narrower than the hint keeps as much of the hint as fits,
        // and still no more than the pane.
        let tiny = text(&dialog_line(4, "anything", "any key"));
        assert_eq!(width(row_body(&tiny)), 4, "{tiny:?}");
    }

    #[test]
    fn a_download_beside_the_tab_line_keeps_the_download_when_the_row_is_narrow() {
        let tab = "Quarterly figures  —  https://example.com/reports";
        let download = "downloading report.pdf 42%";
        let wide = text(&split_line(100, tab, download));
        let body = row_body(&wide);
        assert_eq!(width(body), 100);
        assert!(body.starts_with(tab), "{body:?}");
        assert!(body.ends_with(download), "{body:?}");

        let narrow = text(&split_line(40, tab, download));
        let body = row_body(&narrow);
        assert_eq!(width(body), 40, "{body:?}");
        assert_eq!(body, "Quarterly f…  downloading report.pdf 42%");
        assert!(narrow.ends_with("\x1b[0m\x1b[?25l"), "{narrow:?}");

        // Narrower than the download's words: those, as far as they go, and
        // the tab's line gone rather than run into them.
        let tiny = text(&split_line(10, tab, download));
        assert_eq!(row_body(&tiny), "downloadi…");

        // Byte for byte what a dialog without typing was before it was
        // factored out.
        assert_eq!(
            dialog_line(40, "Delete 3 files?", "y/n"),
            split_line(40, "Delete 3 files?", "y/n")
        );
    }

    #[test]
    fn a_prompt_on_the_row_shows_the_cursor_after_what_was_typed() {
        let line = text(&answering(40, "Your name?", "Ada"));
        assert!(
            line.starts_with("\x1b[1;1H\x1b[K\x1b[7mYour name? Ada"),
            "{line:?}"
        );
        // "Your name? " is eleven cells and "Ada" three, so the cursor is on
        // the fifteenth.
        assert!(line.ends_with("\x1b[0m\x1b[1;15H\x1b[?25h"), "{line:?}");
        assert!(
            !line.contains("enter/esc"),
            "the answer has the room: {line:?}"
        );

        // A long question gets half the row, so the answer is not pushed off
        // it; a long answer shows its end, where the typing is, and keeps the
        // last cell for the cursor.
        let long = "Please tell us, in your own words, what happened";
        let line = text(&answering(30, long, "it all went wrong"));
        let body = line
            .trim_start_matches("\x1b[1;1H\x1b[K\x1b[7m")
            .split("\x1b[0m")
            .next()
            .unwrap_or_default();
        assert_eq!(width(body), 30, "{body:?}");
        assert!(body.starts_with("Please tell us… "), "{body:?}");
        assert!(body.trim_end().ends_with("went wrong"), "{body:?}");
        assert!(line.ends_with("\x1b[1;30H\x1b[?25h"), "{line:?}");

        // And the url bar is the same row with `url: ` for its question.
        assert!(text(&url_bar(20, "example.com"))
            .starts_with("\x1b[1;1H\x1b[K\x1b[7murl: example.com "));
        // A prompt with no question is just the line.
        assert_eq!(dialog_prompt(20, ""), "");
        let bare = text(&answering(20, "", "x"));
        assert!(bare.starts_with("\x1b[1;1H\x1b[K\x1b[7mx "), "{bare:?}");
        assert!(bare.ends_with("\x1b[1;2H\x1b[?25h"), "{bare:?}");
    }

    #[test]
    fn the_strip_marks_a_tab_that_is_waiting_on_an_answer() {
        let mut tabs = labels(&["One", "Two", "Three"], 0);
        tabs[1].dialog = true;
        assert_eq!(strip_text(80, &tabs, ""), "[1 One]  2! Two  3 Three");
        // The mark is part of what the row counts: it still fills the width
        // exactly, however narrow, and the numbers still come first.
        for cols in [8u32, 13, 20, 40, 80] {
            let line = String::from_utf8(tab_line(cols, &tabs, "https://example.com/page"))
                .expect("ascii");
            let body = row_body(&line)
                .replace("\x1b[27m", "")
                .replace("\x1b[7m", "");
            assert_eq!(width(&body), cols as usize, "{cols} cols: {body:?}");
        }
        // The one in front is marked too, with its emphasis over the mark.
        tabs[0].dialog = true;
        let line = String::from_utf8(tab_line(80, &tabs, "")).expect("ascii");
        assert!(line.contains("\x1b[27m1! One\x1b[7m"), "{line:?}");
    }

    /// What a page might title itself, or put in a url, or ask in a dialog,
    /// to get a word with the terminal: set its title, clear it, a C1 CSI, a
    /// bidi override, and three characters that are there and cannot be seen.
    const HOSTILE: [&str; 5] = [
        "\x1b]0;x\x07",
        "a\rb",
        "\u{9b}2J",
        "\u{202e}moc.knab",
        "\u{200b}\u{200b}\u{200b}",
    ];

    /// Whether a row is text between its own escapes and nothing else.
    ///
    /// The row's framing is taken off — the escapes this module writes, which
    /// are the only ones a row may hold — and what is left must have no C0,
    /// no DEL, and no C1 in its UTF-8 spelling, `C2 80` to `C2 9F`.
    fn only_the_rows_own_escapes(line: &[u8]) -> bool {
        let mut text = String::from_utf8(line.to_vec()).expect("a row is UTF-8");
        for framing in [
            "\x1b[1;1H\x1b[K\x1b[7m",
            "\x1b[0m",
            "\x1b[?25l",
            "\x1b[?25h",
            "\x1b[27m",
            "\x1b[7m",
            "\x1b[22m",
            "\x1b[2m",
        ] {
            text = text.replace(framing, "");
        }
        // The cursor put back after a line being typed: `ESC [ 1 ; n H`.
        while let Some(at) = text.find("\x1b[1;") {
            let rest = &text[at + 4..];
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 || !rest[digits..].starts_with('H') {
                return false;
            }
            text.replace_range(at..at + 4 + digits + 1, "");
        }
        let bytes = text.as_bytes();
        let c1 = bytes
            .windows(2)
            .any(|pair| pair[0] == 0xc2 && (0x80..=0x9f).contains(&pair[1]));
        !c1 && bytes.iter().all(|&b| b >= 0x20 && b != 0x7f)
    }

    /// The part of a row that is cells, however it ends.
    fn cells(line: &[u8]) -> usize {
        let line = text(line);
        let body = line
            .trim_start_matches("\x1b[1;1H\x1b[K\x1b[7m")
            .split("\x1b[0m")
            .next()
            .unwrap_or_default()
            .replace("\x1b[27m", "")
            .replace("\x1b[7m", "")
            .replace("\x1b[22m", "")
            .replace("\x1b[2m", "");
        width(&body)
    }

    #[test]
    fn nothing_a_page_says_can_speak_to_the_terminal() {
        for cols in [4u32, 12, 40, 80] {
            for hostile in HOSTILE {
                let mut rows = vec![
                    status_line(cols, hostile),
                    url_bar(cols, hostile),
                    prompt_line(cols, hostile, hostile, hostile, 0),
                    prompt_line(cols, "url: ", "a", hostile, 1),
                    dialog_line(cols, hostile, "y/n"),
                    answering(cols, hostile, hostile),
                ];
                for count in [2, 9] {
                    let titles = vec![hostile; count];
                    rows.push(tab_line(cols, &labels(&titles, 0), hostile));
                    rows.push(tab_line(cols, &labels(&titles, count - 1), hostile));
                }
                for row in rows {
                    assert!(
                        only_the_rows_own_escapes(&row),
                        "{cols} cols, {hostile:?}: {:?}",
                        text(&row)
                    );
                    assert_eq!(
                        cells(&row),
                        cols as usize,
                        "{cols} cols, {hostile:?}: {:?}",
                        text(&row)
                    );
                }
            }
        }
        // A page's `cursor` value chooses a pointer shape and is never the
        // name written: every hostile string is the arrow.
        for hostile in HOSTILE {
            let shape = crate::hover::shape(hostile);
            assert_eq!(shape, crate::hover::Shape::Default, "{hostile:?}");
            assert_eq!(pointer_shape(shape.name()), b"\x1b]22;default\x1b\\");
        }
        // And what is left is the letters, which is what the person reads.
        assert!(row_body(&text(&status_line(80, "\x1b]0;x\x07"))).starts_with("]0;x "));
        assert!(row_body(&text(&status_line(80, "a\rb"))).starts_with("a b "));
        assert!(row_body(&text(&status_line(
            80,
            "https://evil.example/\u{202e}moc.knab",
        )))
        .starts_with("https://evil.example/moc.knab "));
    }

    /// The same rows, read by a terminal: the compositor's own, which is the
    /// one this was written for. Nothing is set, nothing is answered, and the
    /// row says the letters.
    #[test]
    fn a_terminal_that_reads_the_row_is_told_nothing_but_text() {
        for hostile in HOSTILE {
            let rows = [
                status_line(80, hostile),
                prompt_line(80, "url: ", hostile, hostile, 0),
                dialog_line(80, hostile, "y/n"),
                tab_line(80, &labels(&[hostile, "\x1b]2;x\x07"], 0), hostile),
            ];
            for row in rows {
                let mut terminal =
                    tos_term::Terminal::new(80, 24, tos_term::TerminalConfig::default());
                terminal.advance(&enter_sequence());
                let _ = terminal.take_output();
                terminal.advance(&row);
                assert_eq!(terminal.title(), "", "the row set the title: {row:?}");
                assert!(
                    terminal.take_output().is_empty(),
                    "the row asked the terminal something: {row:?}"
                );
            }
        }
        let mut terminal = tos_term::Terminal::new(80, 24, tos_term::TerminalConfig::default());
        terminal.advance(&enter_sequence());
        terminal.advance(&status_line(80, "\x1b]0;x\x07"));
        assert!(
            terminal.grid().row(0).to_text().starts_with("]0;x"),
            "{:?}",
            terminal.grid().row(0).to_text()
        );
        terminal.advance(&status_line(80, "a\rb"));
        assert!(terminal.grid().row(0).to_text().starts_with("a b"));
    }

    #[test]
    fn a_tab_strip_measures_titles_after_they_are_cleaned() {
        let invisible = format!("{}x", "\u{200b}".repeat(40));
        let tabs = labels(&["One", &invisible, "A long title"], 0);
        // Twenty cells, ten of them numbers and gaps. The forty zero-width
        // spaces are given none of the other ten: what the second tab wants is
        // the one `x` it can show, and the room it does not want goes to the
        // title that can use it — six cells, rather than the three it would
        // get if the invisible title were counted as forty-one.
        assert_eq!(strip_text(20, &tabs, ""), "[1 One]  2 x  3 A lon…");
    }

    #[test]
    fn a_short_url_keeps_its_whole_self() {
        assert_eq!(tail_to("abc", 10), "abc");
        assert_eq!(tail_to("abcdef", 3), "def");
        assert_eq!(tail_to("\u{65e5}\u{672c}\u{8a9e}", 5), "\u{672c}\u{8a9e}");
    }
}
