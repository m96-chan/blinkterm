//! The loop that makes the other modules a browser.
//!
//! One thread, one `poll`, three descriptors: the terminal, the pipe the tab in
//! front knocks on, and the pipe the browser connection knocks on. Everything
//! else is a reaction to one of those being readable, which is what keeps this
//! file a sequence of decisions rather than a scheduler. A background tab's
//! pipe is not polled — it has no screencast and nothing urgent to say — but
//! its queue is drained every pass, so a page that renames itself or opens a
//! dialog while it is not being looked at is still heard.
//!
//! The decisions worth knowing about are here rather than scattered: which
//! keys this program keeps for itself and which the compositor took first,
//! what a wheel notch is worth, what a click on the status row means, what
//! happens to the frames that arrive faster than a pane can draw them, and
//! what is on the screen in the moment between two tabs.
//!
//! The one that is not here is which format a frame comes in and which of two
//! frames wins when they arrive out of order: that is [`crate::motion`],
//! because it is a policy with a measurement behind it and it can be tested
//! without an engine, a terminal or a pane.
//!
//! The other thing that is not here is the wheel's animation. It was, and a
//! loop that spends nine milliseconds decoding a frame is a loop that sends a
//! scroll's ticks in bursts, which the engine applies as jumps — so the ticks
//! moved to a thread of their own in [`crate::scroll`]. What is left on this
//! side is a notch handed over as it is read and a timestamp read back once a
//! pass.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tos_platform::tty::{self, ReadOutcome};
use tos_preview::fit::{Cells, Metrics};

use crate::appearance::Appearance;
use crate::cdp::{Client, Event, Notifier, Pending};
use crate::clipboard;
use crate::dialog::{Answer, Dialog, Kind};
use crate::download::{self, Downloads};
use crate::engine::Engine;
use crate::find;
use crate::graphics::{Painter, Raw};
use crate::history::{self, History};
use crate::hover::{self, Shape};
use crate::input::{Input, Key, KeyAction, KeyInput, MouseInput, MouseKind, Parser};
use crate::json::Json;
use crate::keys;
use crate::line::{Edit, Line};
use crate::load::{self, Loaded, Problem};
use crate::motion::{self, Motion};
use crate::options::Options;
use crate::profile::Profile;
use crate::screen::{self, Pane};
use crate::scroll::{self, Step};
use crate::tablist::{self, TabList};
use crate::tabs::{Outcome, Tab, Tabs};
use crate::upload::{self, Upload};
use crate::zoom::{self, Scale, Viewport, Zoom, Zooms};

/// How long the engine gets to answer on its pipe; `--doctor` gives it the
/// same.
pub const ENGINE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long it then gets to offer a page to drive.
const TARGET_TIMEOUT: Duration = Duration::from_secs(15);

/// How long `Browser.close` gets, to be answered and then to finish.
///
/// The reply comes at once and the process is gone about two seconds later —
/// 1.9 s measured on `about:blank` with the headless shell, 1.8 s with full
/// Chromium — and that is the time it spends writing the profile, which is
/// the point. So five: room for a heavier page than `about:blank`, and short
/// enough that a quit which is not going to be clean still ends.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// CSS pixels per wheel notch.
///
/// Chromium's own idea of a wheel tick is three lines of about forty pixels.
/// A terminal mouse reports notches and nothing else — no acceleration, no
/// fractional scroll — so the number is a constant, and this is the one that
/// makes a page move by about the same amount it would in a window.
///
/// What a notch *is* on the wire is [`crate::scroll`]'s: it starts a curve of
/// its own that delivers this much over [`crate::scroll::D`], and a tick every
/// 16 ms — on that module's thread, not on this loop — sends what every
/// running curve has not been given yet as one `Input.dispatchMouseEvent` of
/// type `mouseWheel`. It used to be one `Input.synthesizeScrollGesture` per
/// notch animated by the engine, and then one exponential approach to a
/// distance owed; the measurements that took both of those out are in that
/// module and in `docs/design/browser.md`.
pub const WHEEL_PIXELS: f64 = 120.0;

/// How close in time and space two presses have to be to be a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
const DOUBLE_CLICK_SLOP: i32 = 4;

/// How long the loop waits for something to happen.
///
/// Long enough that an idle browser costs twenty wake-ups a second, short
/// enough that a held Escape is told apart from an escape sequence and a
/// signal is noticed.
///
/// A scroll used to shorten it to the next tick of the animation, and that is
/// gone with the animation: the ticks are [`crate::scroll::Wheel`]'s thread's
/// and it keeps its own clock. All this loop does with a scroll is read
/// [`crate::scroll::Wheel::activity`] each pass, and the interval that reads
/// it is 400 ms — see [`motion::INPUT_QUIET`] — so fifty is eight times as
/// often as it needs to be.
const POLL_MS: i32 = 50;

/// How long a paste that has been opened and not closed may go without a byte
/// before it is given up on.
///
/// A terminal that sent `CSI 200 ~` and then nothing is a terminal that will
/// never send the end marker, and until it is given up on every key typed is
/// more paste. Forty times [`POLL_MS`], which is longer than any stall of an
/// ssh connection a person would sit through with a paste half-arrived; and it
/// is a silence, not a total, so a 64 KiB paste that trickles in over a slow
/// line for longer than this is not cut while it is still coming.
const PASTE_IDLE: Duration = Duration::from_secs(2);

/// How long the page gets to say what is selected.
///
/// The same two seconds as [`page_loaded`], and waited for, for the same
/// reason: the person has just pressed a key and is waiting on the answer,
/// which is a few hundred bytes from a page that is not stopped.
const SELECTION_TIMEOUT: Duration = Duration::from_secs(2);

/// The row the page starts on, one-based: the first is this program's.
const PAGE_ROW: u32 = 2;

/// How long a command on the path between two tabs may take.
///
/// Shorter than [`crate::cdp::CALL_TIMEOUT`], because the page being left may
/// well be the reason it is being left: a tab that has stopped answering must
/// not make the tab somebody asked for wait fifteen seconds to appear.
const SWITCH_TIMEOUT: Duration = Duration::from_secs(3);

/// How long an attach to a new tab's page has to be answered.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the engine gets to draw the lossless picture of a page that has
/// stopped.
///
/// It is a whole-page paint and an encode, so it is slower than a screencast
/// frame: 66 to 98 milliseconds at a pane's size on the VirtualBox machine
/// [`crate::motion`] was measured on. It does not block this loop for them —
/// the command goes out with [`Client::send`] and the reply is collected on
/// whichever pass it has arrived on — so this is a deadline rather than a
/// wait. A page that cannot produce a picture of itself in two seconds has
/// something else wrong with it, and the last motion frame stays up.
const STILL_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a search gets before the row gives up on it.
///
/// Sent and collected like the still, never waited for: see [`find`] for
/// the measurements, which are 31 to 61 ms of renderer on a long article and
/// up to a second on 2.4 million characters of text, more than anybody reads.
/// Five, so that the largest page measured has room to spare and a page that
/// takes longer has something else wrong with it. The clock does not run
/// while the page is stopped behind a dialog: the search is queued behind the
/// question like every other command, and lands when it has been answered.
const FIND_TIMEOUT: Duration = Duration::from_secs(5);

/// How long each of the two calls that make the find script's world gets.
///
/// Waited for, as [`SELECTION_TIMEOUT`] is and for the same reason: the
/// person has just pressed `ctrl+f` and the first letter they type needs the
/// world. Measured at 0.4 to 2.2 ms each.
const WORLD_TIMEOUT: Duration = Duration::from_secs(2);

/// The largest frame either decoder may produce, in bytes of pixels.
///
/// A frame is a pane, so this is never reached; it is the ceiling that stops
/// a malformed header from asking for a gigabyte. Sixty-four megabytes is a
/// 4096x4096 picture in RGBA, which is larger than any display tOS runs on.
const FRAME_BUDGET: usize = 64 * 1024 * 1024;

/// Set by the signal handlers. A handler may do nothing else.
static QUIT: AtomicBool = AtomicBool::new(false);
static RESIZED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_quit(_signal: libc::c_int) {
    QUIT.store(true, Ordering::SeqCst);
}

extern "C" fn on_winch(_signal: libc::c_int) {
    RESIZED.store(true, Ordering::SeqCst);
}

/// Ask to be told about the three signals that matter, without `SA_RESTART`:
/// a `poll` that is interrupted is a `poll` that comes back and looks at the
/// flags, which is the whole point of setting them.
fn install_signals() {
    // SAFETY: every pointer handed over below points at a live local that
    // outlives its call -- `sigaction(2)` and `sigemptyset(3)` copy what they
    // are given rather than keeping it. The handlers being installed do one
    // atomic store each and nothing else, which is what a handler is allowed
    // to do; `on_quit` and `on_winch` are `extern "C"`, so the kernel's idea
    // of how to call them matches theirs.
    unsafe {
        for (signal, handler) in [
            (libc::SIGTERM, on_quit as *const () as usize),
            (libc::SIGINT, on_quit as *const () as usize),
            (libc::SIGHUP, on_quit as *const () as usize),
            (libc::SIGWINCH, on_winch as *const () as usize),
        ] {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handler;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
        // An engine that closes its end of the pipe must not kill this
        // program before it has put the terminal back: with this, a write to
        // it is `EPIPE`, which `cdp` turns into a sentence.
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// Everything the loop owns that is not the terminal, the tabs or the engine.
struct Chrome {
    painter: Painter,
    parser: Parser,
    clicks: Clicks,
    buttons: u32,
    metrics: Metrics,
    /// Whether the page in front is moving, and what its screen holds.
    motion: Motion,
    /// The lossless still that has been asked for and not yet answered.
    still: Option<Still>,
    /// The thread that animates the wheel. This loop tells it what the hand
    /// did and asks it what it has sent; it does the rest on its own clock.
    wheel: scroll::Wheel,
    /// `Some` while the url is being typed.
    bar: Option<UrlBar>,
    /// `Some` while a needle is being typed into the find prompt. Asked about
    /// every key after the url bar, drawn on the row when the bar is closed.
    find: Option<Find>,
    /// The needle the find prompt last closed with, offered whole by the
    /// next `ctrl+f` as `ctrl+l` offers the url.
    last_needle: String,
    /// The pages visited, which the url bar offers back; kept in the profile,
    /// or only in memory for a temporary one. See [`crate::history`].
    history: History,
    /// `--search-url`, for [`destination`].
    search_url: Option<String>,
    /// The `Page.navigate` that has been sent and not yet answered.
    navigation: Option<Navigation>,
    /// Every file a page has handed over this run, and what the row says
    /// about them. The program's rather than a tab's: a download outlives
    /// the tab it started in, and its events come on the browser's
    /// connection. See [`crate::download`].
    downloads: Downloads,
    /// The directory the last file was uploaded from, this run: where the
    /// next file input's prompt starts. See [`upload::start_dir`].
    upload_dir: Option<PathBuf>,
    /// Where the pointer is and what is under it, for the tab in front only.
    /// See [`crate::hover`].
    hover: hover::Tracker,
    /// The hover ask that is out with the engine.
    asking: Option<Ask>,
    /// The pointer shape the terminal was last told, so that it is told only
    /// on a change.
    shape: Shape,
    /// The right-hand hint last drawn for a load in progress, so that the row
    /// is redrawn when its seconds change and not otherwise.
    hint: Option<String>,
    /// `--scale` as it was given, which `Auto` makes a question asked again
    /// of every new cell size.
    scale_choice: Scale,
    /// The answer to it for the pane as it is now: terminal pixels per CSS
    /// pixel before the tab's zoom. See [`viewport`].
    scale: f64,
    /// The zoom levels remembered per host; kept in the profile, or only in
    /// memory for a temporary one. See [`crate::zoom`].
    zooms: Zooms,
    /// What every page is told about light and dark, once the terminal has
    /// said. See [`crate::appearance`] and [`prepare_session`].
    appearance: Appearance,
    /// The terminal's answer to `CSI 16 t`, for a pane whose kernel window
    /// size has no pixels in it. See [`crate::screen::ASK_CELL_SIZE`].
    cell_hint: Option<(u32, u32)>,
    /// `Some` while the tab list is open. The person's, like the bar and the
    /// find prompt, and exclusive with them by the keyboard: whichever is
    /// open takes every key, so the other cannot be opened until it closes.
    /// See [`crate::tablist`].
    list: Option<TabList>,
    /// The first match on the screen while the list is open, so that the
    /// rows scroll only when the pick leaves them. See [`TabList::window`].
    ///
    /// A `Cell`, as [`Chrome::strip_first`] is, because what writes it is
    /// [`redraw_row`], which every path through the loop calls with the
    /// chrome borrowed for reading; the window is a fact about the last
    /// draw, not a decision anybody else makes.
    list_first: Cell<usize>,
    /// The first tab in the strip's window, kept between draws so that the
    /// strip scrolls only when the tab in front leaves it. See
    /// [`screen::tab_line_from`].
    strip_first: Cell<usize>,
}

impl Chrome {
    /// Where a file input's prompt starts, read now: see
    /// [`upload::start_dir`].
    fn upload_base(&self) -> PathBuf {
        upload::start_dir(
            self.upload_dir.as_deref(),
            std::env::current_dir().ok(),
            upload::home().as_deref(),
        )
    }
}

/// The url bar while it is open.
///
/// The line starts as the page's url with all of it selected, because a
/// browser's `ctrl+l` selects the whole address: the next thing typed replaces
/// it and a backspace deletes it, while an arrow keeps it to edit. There is no
/// selection to draw in a status line, but the behaviour is what the reflex
/// expects, and keeping the old url until then is what makes `ctrl+l` also a
/// way to read where you are.
struct UrlBar {
    line: Line,
    /// Up and Down in progress through the history. No suggestion is offered
    /// while walking: what is in the bar was put there whole, and the next
    /// letter typed ends the walk.
    walk: Option<history::Walk>,
}

impl UrlBar {
    fn new(line: Line) -> UrlBar {
        UrlBar { line, walk: None }
    }
}

/// The find prompt while it is open, and the page it is searching.
///
/// The program's rather than a tab's, like [`Navigation`] and [`Still`], with
/// the target beside it for the same reason: there is one row and one person
/// typing on it, and an answer collected against a tab that is no longer in
/// front would put one page's count under another's needle. A tab cannot be
/// switched from the keyboard while the prompt is open, because the prompt
/// takes every key; a page that opens a window, or closes itself, can put
/// another tab in front all the same, and [`switched`] closes the prompt then.
/// Per-tab prompts remembered behind every tab, which is Chrome's model, were
/// left out: state nobody can see in a strip, and one more thing to keep in
/// step with the engine.
struct Find {
    target: String,
    finder: find::Finder,
    /// The world the script runs in, once made for this document; `None` for
    /// a page that did not answer when it was asked for one.
    context: Option<i64>,
    /// The search out with the engine, what it asked, and when it went.
    pending: Option<(Pending, find::Ask, Instant)>,
    /// What to ask next, once the answer is in: keys typed while a search is
    /// out fold in here ([`find::Ask::merge`]), so at most one is ever out.
    wanted: Option<find::Ask>,
    /// Whether a world that went with its document has already been made
    /// again for this ask, so that a page which keeps navigating under the
    /// prompt is reported rather than chased.
    remade: bool,
}

impl Find {
    /// Fold `ask` into whatever is waiting to be sent.
    fn want(&mut self, ask: find::Ask) {
        self.wanted = Some(match self.wanted.take() {
            Some(older) => older.merge(ask),
            None => ask,
        });
    }
}

/// A `Page.navigate` that is out with the engine.
///
/// Kept beside its target, as [`Still`] is, and for the same reason: a reply
/// collected against a tab that is no longer in front would put one page's
/// failure under another page's name.
///
/// It used to be a `call`, and a navigation is on a person's critical path, so
/// that looked right. What made it wrong is a page that asks before it is
/// left. The engine runs the page's `beforeunload` before it answers
/// `Page.navigate` at all, and if that handler wants the person asked, the
/// reply is held until somebody has answered the dialog — which, from inside a
/// `call`, nobody can: the loop that would draw the question is the loop
/// sitting in the `call`. So a url typed over a half-filled form froze the
/// pane for the fifteen seconds of [`crate::cdp::CALL_TIMEOUT`] and then
/// reported a timeout, with the dialog only drawn afterwards. Sent and
/// collected later, the dialog is on the row on the next pass. There is no
/// deadline here either, for the same reason: the reply takes as long as the
/// person takes to read the question, and a timeout would be a guess at that.
///
/// `Page.reload` and `Page.navigateToHistoryEntry` are still `call`s. They were
/// measured against the same page and both answer at once, with the dialog
/// arriving as an event afterwards.
struct Navigation {
    target: String,
    /// What was asked for, which is what a failure in the reply is about:
    /// see [`navigated`].
    url: String,
    pending: Pending,
}

/// A `Page.captureScreenshot` that is out with the engine.
///
/// The target is kept beside the command because a tab can be switched away
/// from, resized or closed while the engine is drawing: a reply collected
/// against the wrong page would paint one tab's picture under another tab's
/// name.
struct Still {
    target: String,
    pending: Pending,
    /// When it went out, against [`STILL_TIMEOUT`].
    sent: Instant,
}

/// A `Runtime.evaluate` of [`hover::ASK`] that is out with the engine.
///
/// Beside its target, as [`Still`] is, so that an answer about one page is
/// never shown over another; and never a `call`, because a page whose
/// navigation is pending holds every such question until it commits — for a
/// host that never answers, forever. See [`crate::hover`].
struct Ask {
    target: String,
    /// The point it is about, as the [`hover::Tracker`] keeps it: in the
    /// terminal's pixels, which is what its answer is matched against. The
    /// page was asked about the same point in its own.
    at: (i32, i32),
    pending: Pending,
    /// When it went out, against [`hover::ASK_TIMEOUT`].
    sent: Instant,
}

/// Run until the person quits or something goes wrong.
pub fn run(options: Options) -> Result<(), String> {
    // SAFETY: `isatty(3)` takes a descriptor, reads no memory, and only
    // reports. 1 is stdout, which this program has by definition.
    if unsafe { libc::isatty(1) } != 1 {
        return Err("stdout is not a terminal, so there is nowhere to put a page".to_string());
    }
    install_signals();
    std::panic::set_hook(Box::new(|info| {
        // A release build aborts here, so this is the only chance to put the
        // terminal back and stop the engine.
        screen::emergency();
        crate::engine::kill_engine();
        crate::profile::remove_temp_profile();
        eprintln!("blinkterm: {info}");
    }));

    // Taken before the engine is started, so that a profile another blinkterm
    // is using is refused before anything has written to it.
    let profile = Profile::take(options.profile.clone())?;
    let mut engine = Engine::launch_with(profile, ENGINE_TIMEOUT, &options.engine)?;
    // The browser's own client, rather than a page's. It is the only one that
    // can hear about a target this program did not open — a `target=_blank`,
    // a `window.open` — and the only one that can open, close, raise or
    // attach to one, which is how every page's client is made.
    let mut browser = engine.browser()?;
    browser.call(
        "Target.setDiscoverTargets",
        Json::object(vec![("discover", Json::Bool(true))]),
    )?;
    // Before any page can be asked for anything, because a download that
    // begins before the engine is told where is one it refuses without a
    // word; and before the pane is taken, so that a directory which is a
    // file is a sentence in the shell.
    let downloads_dir = download::prepare(options.download.clone())?;
    download::enable(&mut browser, &downloads_dir)?;
    let first = crate::engine::first_page_target(&mut browser, TARGET_TIMEOUT).map_err(|why| {
        let tail = engine.tail();
        if tail.is_empty() {
            why
        } else {
            format!("{why}; the engine said: {}", tail.join(" / "))
        }
    })?;
    // What pages are told about light and dark, before there is a page to
    // tell: the flags now, the terminal's answer when it comes.
    let appearance = Appearance::new(options.scheme, options.force_dark);
    // Set up as every other tab is: so that the page most uploads happen in
    // is one whose file inputs are asked on the row, so that nothing a
    // session is told when it is made can be forgotten on this one, and so
    // that the first page is not a tab with less known about it than the
    // rest — its main frame's id above all, which is what its loading is
    // told apart from an iframe's by. See [`connect_tab`].
    let (client, frame) = connect_tab(&mut browser, &first, &appearance)?;
    let mut first = Tab::new(first, client, "about:blank");
    first.frame = frame;
    let mut tabs = Tabs::new(first);

    let mut pane = Pane::enter(0, 1).map_err(|e| format!("cannot take the terminal: {e}"))?;
    // Built here rather than in `drive`, so that what it knows about the
    // downloads is still here when `drive` is over and the engine is being
    // stopped. The rest of it goes at the end of this block, before the pane
    // is given back, as it always did.
    let (outcome, downloads) = match pane.metrics(None) {
        Ok(metrics) => {
            let mut chrome = Chrome {
                painter: Painter::new(),
                parser: Parser::new(),
                clicks: Clicks::default(),
                buttons: 0,
                metrics,
                motion: Motion::new(Instant::now()),
                still: None,
                wheel: scroll::Wheel::start(),
                bar: None,
                find: None,
                last_needle: String::new(),
                history: if engine.profile().is_temporary() {
                    History::in_memory()
                } else {
                    History::load(engine.profile().dir())
                },
                search_url: options.search_url.clone(),
                navigation: None,
                downloads: Downloads::new(downloads_dir),
                upload_dir: None,
                hover: hover::Tracker::default(),
                asking: None,
                shape: Shape::Default,
                hint: None,
                scale_choice: options.scale,
                scale: options.scale.resolve(metrics.cell),
                zooms: if engine.profile().is_temporary() {
                    Zooms::in_memory()
                } else {
                    Zooms::load(engine.profile().dir())
                },
                appearance,
                cell_hint: None,
                list: None,
                list_first: Cell::new(0),
                strip_first: Cell::new(0),
            };
            let outcome = drive(
                &mut pane,
                &mut tabs,
                &mut browser,
                &mut engine,
                &mut chrome,
                options,
            );
            (outcome, Some(chrome.downloads))
        }
        Err(e) => (Err(format!("cannot measure the pane: {e}")), None),
    };
    pane.leave();
    // Dropping the tabs closes every page's session, which is all a tab is
    // once the engine is about to be killed anyway.
    drop(tabs);
    let mut downloads = downloads;
    if let Some(downloads) = downloads.as_mut() {
        // Whatever is still coming is cancelled, whichever way the engine is
        // about to stop: `Browser.close` would cancel it too, but the
        // temporary profile's way out is a kill, and a kill leaves the
        // engine's partial file behind. Cancelled, the engine removes it.
        downloads.cancel_all(&mut browser);
    }
    if !engine.profile().is_temporary() {
        // `Browser.close` is the only stop that writes the cookie jar — a
        // `SIGTERM` loses it; `crate::profile` has the measurements — and the
        // writing happens after the reply, in the two seconds before the
        // process ends, so the engine is waited for rather than just asked.
        // Every way out comes through here: ctrl+q, the last tab closed, the
        // terminal gone, a SIGTERM, SIGINT or SIGHUP by way of QUIT, and an
        // error out of `drive`, where a connection that has already ended
        // makes this fail at once. A temporary profile has nothing worth
        // writing and skips it, which keeps its quit as fast as it was.
        let _ = browser.call_within("Browser.close", Json::empty(), CLOSE_TIMEOUT);
        engine.wait_for_exit(CLOSE_TIMEOUT);
    }
    browser.close();
    engine.kill();
    // And the partial files of every download this run saw begin and not
    // save, now that nothing can be writing them: the engine removes the
    // ones it cancelled, but after it has said so, and a kill can come in
    // between. Those files and nothing else — the directory is the person's.
    for partial in downloads.iter().flat_map(Downloads::partials) {
        let _ = std::fs::remove_file(partial);
    }
    outcome
}

/// Everything between taking the terminal and giving it back.
fn drive(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    engine: &mut Engine,
    chrome: &mut Chrome,
    options: Options,
) -> Result<(), String> {
    activate(tabs, browser, chrome)?;

    let url = normalise(options.urls.first().unwrap_or(&options.home));
    if let Some(tab) = tabs.active_mut() {
        tab.url = url.clone();
        tab.note = Some(format!("loading {url}"));
        tab.loading = true;
    }
    redraw_row(pane, tabs, chrome)?;
    if let Some(why) = navigate(tabs, chrome, &url) {
        if let Some(tab) = tabs.active_mut() {
            tab.note = Some(why);
        }
    }
    // Whether or not it was an error on the wire: a reply that says the page
    // did not come has replaced the loading note with why.
    redraw_row(pane, tabs, chrome)?;
    open_the_rest(tabs, browser, chrome, &options.urls);
    redraw_row(pane, tabs, chrome)?;

    let mut last_check = Instant::now();
    let mut buf = [0u8; 8192];
    // When the terminal last sent a byte of a paste that is still open; see
    // [`PASTE_IDLE`].
    let mut paste_heard: Option<Instant> = None;

    while !QUIT.load(Ordering::SeqCst) {
        if tabs.is_empty() {
            // The last tab closed itself, which is the page saying the browser
            // is over — the same thing `ctrl+w` on the last tab means.
            return Ok(());
        }
        // A download's last word has been on the row long enough.
        if chrome.downloads.expire(Instant::now()) {
            redraw_row(pane, tabs, chrome)?;
        }
        // A load's seconds, counted up: a redraw when the number changes,
        // which is once a second while something is loading and never
        // otherwise, at most a pass late.
        let hint = tabs
            .active()
            .filter(|tab| tab.loading)
            .map(|tab| load::loading_hint(tab.loading_for(Instant::now())));
        if hint != chrome.hint {
            chrome.hint = hint;
            redraw_row(pane, tabs, chrome)?;
        }
        if RESIZED.swap(false, Ordering::SeqCst) {
            chrome.metrics = pane
                .metrics(chrome.cell_hint)
                .map_err(|e| format!("cannot measure the pane: {e}"))?;
            // A font made bigger in the terminal is a resize too, and a cell
            // that has grown past 28 pixels is a HiDPI answer that follows it.
            chrome.scale = chrome.scale_choice.resolve(chrome.metrics.cell);
            pane.write(b"\x1b[2J").map_err(|e| e.to_string())?;
            let metrics = chrome.metrics;
            let mut css = page_pixels(metrics);
            if let Some(tab) = tabs.active_mut() {
                let stopped = tab.dialog.is_some();
                let viewport = viewport(chrome, tab);
                css = viewport.css;
                emulate(&mut tab.connection, viewport, stopped)?;
                restart_screencast(&mut tab.connection, metrics, stopped)?;
            }
            // The screen was cleared and the page is a different size, so
            // nothing that was captured before this is worth painting and the
            // still that reflows the page is worth asking for.
            chrome.motion.reset(Instant::now());
            // A scroll that was running is not dropped, though. A hand is
            // still on the wheel and a pane changing size is no reason for the
            // page to stop dead halfway through a flick; what a resize really
            // invalidates is the *point* the events are sent at, which may now
            // be off the page, so that is brought back inside it and the
            // notches carry on. See `docs/design/browser.md`. Inside it in
            // the page's own pixels, which is what the point is sent in.
            chrome.wheel.resized((css.0 as i32, css.1 as i32));
            // The page is a different size under the same pointer, so what
            // was under it is not known any more.
            forget_hover(pane, chrome)?;
            redraw_row(pane, tabs, chrome)?;
        }

        // The engine is a child process and can die at any point; without this
        // the first sign would be a command that timed out fifteen seconds
        // later.
        if last_check.elapsed() > Duration::from_millis(500) {
            last_check = Instant::now();
            engine.check()?;
        }
        if let Some(ended) = browser.ended() {
            return Err(format!("the engine stopped talking: {ended}"));
        }
        reap_dead_tabs(pane, tabs, browser, chrome)?;

        let wake = tabs.active().map(|tab| tab.connection.wake_fd());
        let mut watching = vec![pane.input_fd(), browser.wake_fd()];
        watching.extend(wake);
        let ready = tty::poll_readable(&watching, POLL_MS)
            .map_err(|e| format!("cannot wait for input: {e}"))?;

        if ready.contains(&pane.input_fd()) {
            match tty::read_available(pane.input_fd(), &mut buf) {
                Ok(ReadOutcome::Data(n)) => {
                    let inputs = chrome.parser.feed(&buf[..n]);
                    paste_heard = chrome.parser.pasting().then(Instant::now);
                    for input in inputs {
                        if !handle_input(pane, tabs, browser, chrome, input)? {
                            return Ok(());
                        }
                    }
                }
                Ok(ReadOutcome::Eof) => return Ok(()),
                Ok(ReadOutcome::WouldBlock) => {}
                Err(err) => return Err(format!("cannot read the terminal: {err}")),
            }
        } else if paste_heard.is_some_and(|at| at.elapsed() > PASTE_IDLE) {
            // A paste that was opened and has gone quiet: the end marker is
            // not coming, and what arrived is half of something.
            paste_heard = None;
            if chrome.parser.abandon_paste() {
                note(tabs, "paste cut short; try again");
                redraw_row(pane, tabs, chrome)?;
            }
        } else if let Some(input) = chrome.parser.flush() {
            // Nothing arrived, so a held escape was the Escape key after all.
            if !handle_input(pane, tabs, browser, chrome, input)? {
                return Ok(());
            }
        }
        // Whatever the pointer did in all the reports just read, told to the
        // page and asked about once: see [`crate::hover`].
        tick_hover(pane, tabs, chrome)?;

        // What the animator thread has been doing while this loop was busy.
        // Nothing here drives it — it has its own clock and its own way onto
        // the pipe — but every tick it sent is a page that moved, and a page
        // that moved is not a page to photograph. See [`motion::INPUT_QUIET`].
        if let Some(at) = chrome.wheel.activity() {
            chrome.motion.input(at);
        }

        if ready.contains(&browser.wake_fd()) {
            browser.drain_wake();
        }
        if let Some(wake) = wake {
            // By the tab that owns the descriptor rather than by whichever tab
            // is active now: handling a key may have switched tabs since the
            // poll, and the pipe that was readable is the one to empty.
            if ready.contains(&wake) {
                if let Some(tab) = tabs.iter().find(|tab| tab.connection.wake_fd() == wake) {
                    tab.connection.drain_wake();
                }
            }
        }
        // Which pages exist first, then what the page in front is doing: a
        // frame is read from whichever tab is active once the list has settled,
        // and never from one that has just been left behind.
        handle_target_events(pane, tabs, browser, chrome)?;
        handle_page_events(pane, tabs, chrome)?;
        // The answer to a navigation, which may have been held for as long as
        // a page's "leave this page?" was on the row. After the page's events,
        // so that a dialog which arrived on the same pass is already drawn.
        if chrome.navigation.is_some() {
            let before = tabs.active().map(Tab::line);
            collect_navigation(tabs, chrome);
            if tabs.active().map(Tab::line) != before {
                redraw_row(pane, tabs, chrome)?;
            }
        }
        // The answer to a search, and the next one if keys were typed while it
        // was out. After the page's events, so that a navigation which took
        // the document away has already closed the prompt.
        if pump_find(tabs, chrome) {
            redraw_row(pane, tabs, chrome)?;
        }
        // And last, because it is the thing to do when nothing else happened:
        // a page that has stopped moving gets its lossless picture.
        rest_shot(pane, tabs, chrome)?;
    }
    Ok(())
}

/// Put a command to a page: waited for, unless the page is `stopped`.
///
/// A page with a dialog open answers nothing that has to reach its renderer,
/// and that is more than `Runtime.evaluate` and the still. Measured against
/// `headless_shell` 141 with an `alert()` up, `Page.enable`,
/// `Emulation.setDeviceMetricsOverride`, `Page.startScreencast` and
/// `Page.stopScreencast` all went three seconds without a word — which is
/// every command a switch to or from that tab sends, and every one a resize
/// sends. What the same measurement found is that nothing is lost: each of
/// them sent as a notification while the dialog was up was done the moment it
/// was answered, the viewport at the size it was told and the frames coming.
///
/// So a stopped page is told rather than asked. The command goes on the wire
/// and the loop carries on, and the page that wakes up is already the size
/// the pane is and already casting — rather than a loop that sat out a
/// deadline per command before it could draw the question that is stopping
/// the page.
fn tell(
    client: &mut Client,
    stopped: bool,
    method: &str,
    params: Json,
    within: Duration,
) -> Result<(), String> {
    if stopped {
        client.notify(method, params)
    } else {
        client.call_within(method, params, within).map(|_| ())
    }
}

/// The page as the tab in front is told it: the pane, less the row, divided
/// by the terminal's scale and the tab's zoom.
///
/// Everything that sends the engine a size or a point asks this: the
/// override itself, a click, the wheel, and anything that hit-tests the page.
/// See [`crate::zoom::Viewport::css_point`] for the one mapping from the
/// terminal's pixels to the page's.
fn viewport(chrome: &Chrome, tab: &Tab<Client>) -> Viewport {
    Viewport::fit(
        page_pixels(chrome.metrics),
        chrome.scale * tab.zoom.factor(),
    )
}

/// Tell the engine how big the page is, and how many of the pane's pixels
/// are one of the page's.
///
/// One `Emulation.setDeviceMetricsOverride`, with the CSS size and the factor
/// as the device's pixel ratio, which is what a browser's zoom and a HiDPI
/// display both are to a page: it lays itself out again for the size, live,
/// and draws its stills at the pane's resolution. It outlives navigations and
/// reloads on its session, and a session made afterwards starts without it,
/// so it is sent where the size always was — when a tab comes to the front
/// and when the pane is resized — and when the level changes. The measurements
/// are in [`crate::zoom`].
fn emulate(client: &mut Client, viewport: Viewport, stopped: bool) -> Result<(), String> {
    tell(
        client,
        stopped,
        "Emulation.setDeviceMetricsOverride",
        viewport.metrics_params(),
        crate::cdp::CALL_TIMEOUT,
    )
}

/// Start the frames coming, in the format [`crate::motion`] argues for.
///
/// At the pane's size whatever the zoom: the engine casts no more pixels than
/// the page's CSS viewport has, so a page zoomed in sends frames smaller than
/// the pane — at 200%, half of it each way — and the terminal scales them
/// into the same cells, until the still at the pane's own resolution replaces
/// them. See [`crate::zoom`] for why nothing sharper was taken.
fn start_screencast(client: &mut Client, metrics: Metrics, stopped: bool) -> Result<(), String> {
    let (width, height) = page_pixels(metrics);
    tell(
        client,
        stopped,
        "Page.startScreencast",
        Json::object(vec![
            ("format", Json::string("jpeg")),
            ("quality", Json::number(motion::QUALITY)),
            ("maxWidth", Json::number(width)),
            ("maxHeight", Json::number(height)),
            ("everyNthFrame", Json::number(1)),
        ]),
        crate::cdp::CALL_TIMEOUT,
    )
}

fn restart_screencast(client: &mut Client, metrics: Metrics, stopped: bool) -> Result<(), String> {
    let _ = tell(
        client,
        stopped,
        "Page.stopScreencast",
        Json::empty(),
        crate::cdp::CALL_TIMEOUT,
    );
    start_screencast(client, metrics, stopped)
}

/// What a page calls itself, and the status its document came with, asked of
/// the page.
///
/// Deliberately not `Target.targetInfoChanged`'s `title`, which would cost
/// nothing and be wrong: against `chromium-shell` that field is derived from
/// the url and a `document.title` set by a script never changes it. See
/// [`crate::tabs`] for the measurement. The status rides in the same
/// evaluation, which is how a 404 is known without the `Network` domain — see
/// [`crate::load`] for why that domain is not paid for. `None` means the page
/// did not answer in time, and a tab keeps the name it had rather than losing
/// it to a page that is busy.
pub fn page_loaded(client: &mut Client) -> Option<Loaded> {
    let answer = client
        .call_within(
            "Runtime.evaluate",
            Json::object(vec![
                ("expression", Json::string(load::LOADED)),
                // Without it the array comes back as a handle to an object in
                // the page rather than as the array.
                ("returnByValue", Json::Bool(true)),
            ]),
            Duration::from_secs(2),
        )
        .ok()?;
    load::loaded(&answer)
}

/// Just the title: [`page_loaded`] with the status left out.
pub fn page_title(client: &mut Client) -> Option<String> {
    page_loaded(client).map(|loaded| loaded.title)
}

/// Send the active tab somewhere. The sentence, if it would not go.
///
/// A navigation fails in one of two ways, and only one of them comes back as
/// an error on the wire. A url the engine cannot parse is refused outright. A
/// url it can parse but not reach — a host that does not resolve, a port
/// nobody listens on — is a command that *succeeded*: the engine navigated,
/// to its own error page, and the reply carries the reason as `errorText`,
/// which [`navigated`] reads. It is quick, too: a `.invalid` host answers in
/// 10 ms, because the resolver refuses the name without asking anyone. The
/// error page's own events follow the reply, and [`Tab::landed`] takes the
/// url from them.
///
/// Neither is waited for here. Only a command that could not be put on the
/// wire is a sentence from here; the engine's answer — its refusal or its
/// `errorText` — arrives later and is collected by [`collect_navigation`].
/// See [`Navigation`] for why this does not wait for it. A navigation still
/// out when another is asked for is dropped, which is what the person asked
/// for by typing a second url.
///
/// The problem the tab had is cleared before anything is sent, so that a
/// reason from an earlier navigation — one whose reply was dropped and whose
/// failure landed afterwards — cannot be pinned on this one.
///
/// The load's clock starts here, at the Enter, and the page is taken as no
/// longer committed from here too: its renderer will hold anything asked of
/// it from the moment the engine has the navigation (see [`crate::hover`]),
/// and the engine's own word that it is leaving is three milliseconds behind.
fn navigate(tabs: &mut Tabs<Client>, chrome: &mut Chrome, url: &str) -> Option<String> {
    let tab = tabs.active_mut()?;
    tab.problem = None;
    tab.since = Some(Instant::now());
    tab.committed = false;
    let sent = tab.connection.send(
        "Page.navigate",
        Json::object(vec![("url", Json::string(url))]),
    );
    match sent {
        Ok(pending) => {
            chrome.navigation = Some(Navigation {
                target: tab.target.clone(),
                url: url.to_string(),
                pending,
            });
            None
        }
        Err(why) => Some(why),
    }
}

/// What a `Page.navigate` reply says about the tab it was sent to.
///
/// Only anything when the navigation failed: then the reason is recorded
/// against the url that was asked for, and the error page's landing, which is
/// ten to sixty milliseconds behind, keeps it. A reply without an
/// `errorText` says nothing the page's own events will not say better.
///
/// Or when the url turned out to be a file: `net::ERR_ABORTED` with
/// `isDownload`, and after it no landing, no rename and no history entry —
/// the page stays exactly where it was, measured, and nothing else would ever
/// take the "loading" note off. So it comes off here, the way it does after a
/// "leave this page?" answered no. The file itself is
/// [`crate::download`]'s, on the browser's connection.
pub fn navigated(tab: &mut Tab<Client>, url: &str, reply: &Json) {
    if download::became_download(reply) {
        stayed(tab);
        return;
    }
    if let Some(code) = load::failed(reply) {
        tab.failed_to_reach(url, &code);
    }
}

/// Take the answer to the navigation, if it has come back.
///
/// Mirrors [`collect_still`]: a tab that is no longer in front is a tab this
/// reply is not for any more, and dropping the [`Navigation`] drops its
/// [`Pending`], which tells the mailbox not to keep the answer. What an answer
/// that did come says goes where the `call` used to put it — on the tab's
/// note, which is what the row shows in place of the title.
fn collect_navigation(tabs: &mut Tabs<Client>, chrome: &mut Chrome) {
    let Some(navigation) = chrome.navigation.as_ref() else {
        return;
    };
    if tabs.active_target() != Some(navigation.target.as_str()) {
        chrome.navigation = None;
        return;
    }
    let Some(tab) = tabs.active_mut() else {
        return;
    };
    let Some(answer) = tab.connection.take_reply(&navigation.pending) else {
        return;
    };
    let url = chrome
        .navigation
        .take()
        .map(|navigation| navigation.url)
        .unwrap_or_default();
    match answer {
        Err(why) => tab.note = Some(why),
        // A reply can carry a failure and still be a reply: an unreachable
        // host, a refused connection. [`navigated`] reads it. One that is not
        // a failure to report is `net::ERR_ABORTED` after a "leave this
        // page?" was answered no — the page stayed, the note was already
        // cleared by `stayed`, and measured against `headless_shell` 141 that is
        // exactly what this reply carries then; `load::failed` passes over it.
        // The same code with `isDownload` is a url that was a file, and
        // `navigated` puts the tab back for that one.
        Ok(reply) => navigated(tab, &url, &reply),
    }
}

/// Start the active tab painting: sized, told it is in front, and casting.
///
/// Every command here is one a background tab did not have run on it, because
/// a background tab is a page with no screencast and no viewport of ours.
/// `Page.enable` is idempotent, so a tab that has been active before is not a
/// special case.
fn activate(
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    chrome: &mut Chrome,
) -> Result<(), String> {
    let Some(target) = tabs.active_target().map(str::to_string) else {
        return Ok(());
    };
    // Chromium treats a target that is not in front as a hidden page: its
    // animations stop and its `requestAnimationFrame` never fires, so the
    // screencast of a tab that was never activated is one frame and then
    // nothing.
    let _ = browser.call_within(
        "Target.activateTarget",
        Json::object(vec![("targetId", Json::string(&target))]),
        SWITCH_TIMEOUT,
    );
    let metrics = chrome.metrics;
    let base = chrome.upload_base();
    let Some(tab) = tabs.active_mut() else {
        return Ok(());
    };
    // First, whether the page is stopped behind a dialog, because that
    // decides whether anything below can be waited for. See [`tell`].
    bin_events(tab, &base);
    let mut stopped = tab.dialog.is_some();
    if let Err(why) = tell(
        &mut tab.connection,
        stopped,
        "Page.enable",
        Json::empty(),
        SWITCH_TIMEOUT,
    ) {
        // A page that opened its dialog between that look and this command
        // is the one way to get here with a page that is fine. The command is
        // queued behind the dialog like everything else, so the rest is told
        // rather than asked; anything else is a page that is not answering.
        bin_events(tab, &base);
        if tab.dialog.is_none() {
            return Err(why);
        }
        stopped = true;
    }
    // The level its host was left at, which another tab on the same site may
    // have changed while this one was behind. A page with no host keeps the
    // tab's own.
    if let Some(host) = zoom::host_key(&tab.url) {
        tab.zoom = chrome.zooms.get(Some(&host));
    }
    let viewport = viewport(chrome, tab);
    emulate(&mut tab.connection, viewport, stopped)?;
    // Whatever this tab queued and nobody has read goes in the bin, frames
    // above all: the newest of them is older than the tab was, and painting it
    // would put the page as it looked before it was left behind on screen
    // under the row of the tab that has just been chosen. Asking for the title
    // afterwards is what makes that safe: the only thing worth having in that
    // queue was the news that the page had finished loading, and this is that
    // news asked for directly.
    //
    // What is not safe is a `Page.frameNavigated` in there: a tab that landed
    // somewhere between the last pass and this one loses the landing, and
    // with it the url and whether the page came at all. It is a gap of one
    // pass, known and left, because the frames in the same queue are the
    // greater harm; the load event after it still asks the page, below and
    // then again when it fires.
    //
    // Except while a dialog is up, when the page cannot say what it is called
    // and a question would cost the two seconds of [`page_loaded`] for
    // nothing. The page is asked when the dialog closes instead.
    bin_events(tab, &base);
    if tab.dialog.is_none() {
        if let Some(loaded) = page_loaded(&mut tab.connection) {
            tab.loaded(loaded);
        }
    }
    start_screencast(&mut tab.connection, metrics, stopped)?;
    // A different page, so a different clock: nothing this tab sends can be
    // compared against what the last one had on screen, and a page that is
    // already loaded and still gets its lossless picture a rest interval
    // from now rather than never.
    chrome.motion.reset(Instant::now());
    Ok(())
}

/// Throw away what a tab has queued, except what it says about a dialog or a
/// file input.
///
/// A queue that is binned is binned for its frames, which are older than the
/// moment they would be painted in. A dialog is not like that: the page opened
/// it and is stopped until it is answered, however long ago that was, and a
/// `Page.javascriptDialogOpening` that went in the bin would be a tab that
/// had stopped with nothing on the row to say why and nothing to answer. A
/// `Page.fileChooserOpened` is the same question without the stopping: the
/// person clicked an input, and a prompt that went in the bin would be a
/// click that did nothing — which is what issue #11 was. `base` is where a
/// prompt opened here starts.
fn bin_events(tab: &mut Tab<Client>, base: &Path) {
    let home = upload::home();
    for event in tab.connection.events() {
        tab.dialog_event(&event);
        tab.chooser_event(&event, base, home.as_deref());
    }
}

/// Stop a tab painting, if it is still in the list.
///
/// Told rather than asked when the tab has a dialog open, because it would
/// not answer: leaving a tab that is waiting on a question is one of the ways
/// the person is expected to deal with it. See [`tell`].
fn deactivate(tabs: &mut Tabs<Client>, target: &str) {
    let Some(index) = tabs.index_of(target) else {
        return;
    };
    if let Some(tab) = tabs.get_mut(index) {
        let stopped = tab.dialog.is_some();
        let _ = tell(
            &mut tab.connection,
            stopped,
            "Page.stopScreencast",
            Json::empty(),
            SWITCH_TIMEOUT,
        );
    }
}

/// Follow a change of which tab is in front all the way through.
///
/// `was` is the target that was in front before whatever just happened. When
/// it is still the one in front this does nothing at all, so every path that
/// might have switched can call it and none of them has to know whether it
/// did.
fn switched(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    chrome: &mut Chrome,
    was: Option<String>,
) -> Result<(), String> {
    let now = tabs.active_target().map(str::to_string);
    if now == was {
        return Ok(());
    }
    // What the last tab was owed is not owed to this one, and there is
    // nothing in flight to disown: the animation is this program's, so
    // forgetting it is the whole of stopping it. It also lets go of that
    // tab's session, which a tab that is closing needs.
    chrome.wheel.forget();
    // The find prompt was searching the page being left. Closed before that
    // page stops painting, so that the clear goes to it while it is still
    // the one on the screen.
    close_find(tabs, chrome);
    // And the tab list, which a page that opens a window in front, or a tab
    // that closes itself from under the pick, has made a list of something
    // else. The screen is cleared below either way.
    chrome.list = None;
    // The same for the pointer: it is over a different page now, and an ask
    // in flight is the old page's, dropped the way the still is.
    forget_hover(pane, chrome)?;
    if let Some(was) = &was {
        deactivate(tabs, was);
    }
    if now.is_none() {
        return Ok(());
    }
    // The picture on the screen is the page that is no longer in front. It
    // goes now rather than when the new tab paints, because the new tab may
    // take a network's worth of time to paint anything and the old page under
    // the new tab's title would be a lie for all of it.
    pane.write(&crate::graphics::clear_command())
        .map_err(|e| e.to_string())?;
    pane.write(b"\x1b[2;1H\x1b[J").map_err(|e| e.to_string())?;
    if let Err(why) = activate(tabs, browser, chrome) {
        if let Some(tab) = tabs.active_mut() {
            tab.note = Some(why);
        }
    }
    Ok(())
}

/// Every url on the command line after the first, one tab each, behind the
/// first.
///
/// After the first has been told to load, so that the page the person will
/// look at first is the one that started first. Each is [`open_behind`] —
/// `Target.createTarget` with `background: true`, the road a link opened
/// with a modifier takes — so the first tab is never switched away from: it
/// was activated once, before this, and the others are tabs behind it. Each
/// is in the list before the engine's announcement of it is read, so that
/// announcement, which has no opener, is ignored as already ours rather
/// than made a second tab. Measured against `chrome-headless-shell` 153, the
/// first stays a visible page while the others are made, so it needs no
/// raising again. One that cannot be opened is a note on the first tab, and
/// the rest still open: one bad url is no reason to open none.
fn open_the_rest(tabs: &mut Tabs<Client>, browser: &mut Client, chrome: &Chrome, urls: &[String]) {
    for url in urls.iter().skip(1) {
        let url = normalise(url);
        if let Err(why) = open_behind(tabs, browser, &chrome.appearance, &url) {
            if let Some(first) = tabs.get_mut(0) {
                first.note = Some(format!("couldn't open {url}: {why}"));
            }
        }
    }
}

/// Open a page in a new tab and switch to it.
fn open_tab(
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    appearance: &Appearance,
    url: &str,
) -> Result<(), String> {
    create_tab(tabs, browser, appearance, url, false).map(|_| ())
}

/// Open `url` in a tab behind the one in front, and say where it went.
///
/// `Target.createTarget` with `background: true`, attached and set up as
/// every tab is (`connect_tab`), and put at the end of the list without
/// being switched to. The page in front keeps casting and keeps its input —
/// measured against `chrome-headless-shell` 153, a page animating at sixty
/// frames a second went on at sixty with a target opened behind it — and the
/// new page loads and gets its title on its own session without ever being
/// looked at. The flag is documented as Chrome's only; the headless shell
/// accepts it and changes nothing observable, and it is passed because it is
/// the documented word for what is meant.
///
/// What a middle click on a link ends up as, for callers that have a url and
/// no link — a link hint, or anything else. The engine's own announcement of
/// the target arrives with no opener, which would make it a tab behind too,
/// and is ignored because the reply to this `call` came first and the tab is
/// already in the list. The caller redraws the row.
pub fn open_behind(
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    appearance: &Appearance,
    url: &str,
) -> Result<usize, String> {
    create_tab(tabs, browser, appearance, url, true)
}

/// The body of [`open_tab`] and [`open_behind`], which differ only in the
/// `background` flag and in where the tab goes: the index is where it went.
///
/// If the call fails after the engine has made the target — a timeout — its
/// announcement makes it a tab behind on the next pass, which is better than
/// a page nobody is attached to.
fn create_tab(
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    appearance: &Appearance,
    url: &str,
    behind: bool,
) -> Result<usize, String> {
    let mut params = vec![("url", Json::string(url))];
    if behind {
        params.push(("background", Json::Bool(true)));
    }
    let created = browser.call("Target.createTarget", Json::object(params))?;
    let target = created
        .get("targetId")
        .and_then(Json::as_str)
        .ok_or_else(|| "the engine opened a page and did not say which".to_string())?
        .to_string();
    let (connection, frame) = connect_tab(browser, &target, appearance)?;
    let mut tab = Tab::new(target, connection, url);
    tab.frame = frame;
    Ok(if behind {
        tabs.open_behind(tab)
    } else {
        tabs.open(tab)
    })
}

/// Attach to a target and start listening to its page, whether or not it is
/// the tab in front; and its main frame's id, if the engine says.
///
/// `Page.enable` from the moment the tab exists rather than from the moment it
/// is looked at: a tab that is loading in the background still has to tell the
/// strip when it has a name, and the events it sends before anybody asks are
/// the only notice there is.
///
/// And a page's file inputs are asked about on the row from then on:
/// `Page.setInterceptFileChooserDialog`, which turns a click on one into a
/// `Page.fileChooserOpened` rather than the engine's own at-once `cancel`.
/// Here and not in [`activate`], because it is per session and a tab behind
/// gets the event too; and after `Page.enable`, because before it the command
/// is accepted and does nothing — both measured against
/// `chrome-headless-shell` 153. It survives the page navigating. See
/// [`crate::upload`].
///
/// Then [`prepare_session`], which is everything else a session is told once
/// for its whole life: sent and not waited for, so it costs the new tab
/// nothing, and ahead of the frame tree only because it is the one of these
/// the page will be looked at through.
///
/// `Page.getFrameTree` last, because a page's loading and an iframe's
/// arrive as the same events under different frame ids, and the main frame's
/// is what tells them apart ([`crate::load::is_main`]). It is browser-side
/// and answers at once; a tab whose tree does not come still works, taking
/// every frame's news as its own until its first landing names the frame.
/// The interception is not optional in the same way: a tab without it would
/// be one whose file inputs cancel themselves, so a refusal there is the
/// tab's failure, as `Page.enable`'s is.
///
/// Every tab is made here, the first one included: a session made any other
/// way would be a page that was never told what the rest were.
fn connect_tab(
    browser: &mut Client,
    target: &str,
    appearance: &Appearance,
) -> Result<(Client, Option<String>), String> {
    let mut connection = browser.attach(target, CONNECT_TIMEOUT)?;
    connection.call_within("Page.enable", Json::empty(), SWITCH_TIMEOUT)?;
    connection.call_within(
        "Page.setInterceptFileChooserDialog",
        Json::object(vec![("enabled", Json::Bool(true))]),
        SWITCH_TIMEOUT,
    )?;
    prepare_session(&mut connection, appearance);
    let frame = connection
        .call_within("Page.getFrameTree", Json::empty(), SWITCH_TIMEOUT)
        .ok()
        .and_then(|tree| load::main_frame(&tree));
    Ok((connection, frame))
}

/// Tell a new session what every session is told and nothing resets: the
/// colour scheme a page is to see, and whether it is to be painted dark.
///
/// This is where every per-session `Emulation` command that is not about the
/// size belongs. The size is not here because it changes with the pane and
/// the zoom, and is sent by `activate` to the tab in front; what is here is
/// per session and survives every navigation on it, and a target attached
/// later starts without it — measured, in [`crate::appearance`] — so a
/// session that missed this would be the one page in the browser that was
/// light in a dark terminal.
///
/// Told rather than asked: a page that has just been made may already be
/// stopped behind a dialog, and a scheme that did not take is a page that is
/// light, which is no reason not to open it.
pub fn prepare_session(client: &mut Client, appearance: &Appearance) {
    for (method, params) in appearance.commands() {
        let _ = client.notify(method, params);
    }
}

/// Close one tab: the page in the engine, and the session on it.
///
/// In that order, and both. A target closed while its session is still
/// attached is a page the engine keeps alive for the debugger that is still
/// there; a session closed without the target is a page that goes on rendering
/// for nobody.
///
/// Dropping the connection is also what makes a frame that was in flight
/// harmless: it is in that client's mailbox, and the mailbox goes with the
/// client. Nothing can paint it over the tab that comes next, because nothing
/// will ever read it.
///
/// A page is not asked first. `Target.closeTarget` does not run `beforeunload`,
/// so a tab with a half-filled form closes without a "leave this page?", and a
/// tab stopped behind a dialog closes with its dialog — the engine answers the
/// command at once either way, measured with an `alert()` up. That is what
/// makes `ctrl+w` the way out of a page that asks the same question forever,
/// and it is why the README says so rather than leaving somebody to find out
/// with their form.
fn close_tab(tabs: &mut Tabs<Client>, browser: &mut Client, index: usize) {
    let Some(mut tab) = tabs.close(index) else {
        return;
    };
    let _ = browser.call_within(
        "Target.closeTarget",
        Json::object(vec![("targetId", Json::string(&tab.target))]),
        SWITCH_TIMEOUT,
    );
    tab.connection.close();
}

/// Drop any tab whose session has gone.
///
/// A target that is closed takes its session with it — the engine says
/// `Target.detachedFromTarget`, and [`crate::cdp`] ends that tab's client when
/// it reads it — so this and `Target.targetDestroyed` are two ways of hearing
/// the same news and either may be looked at first. Which is why neither a
/// sentence nor an error comes out of here: a tab that went because the page
/// called `window.close` must not be reported as a failure just because its
/// client noticed before the browser's did, and whether a person sees a
/// message for that would otherwise depend on which of two mailboxes was
/// read first. A tab that died
/// for a reason worth a sentence gets one from `Target.targetCrashed`, which
/// arrives on the browser connection either way; an engine that died is caught
/// by [`Engine::check`] and by the browser connection ending, neither of which
/// is a page.
fn reap_dead_tabs(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    chrome: &mut Chrome,
) -> Result<(), String> {
    let dead: Vec<usize> = tabs
        .iter()
        .enumerate()
        .filter(|(_, tab)| tab.connection.ended().is_some())
        .map(|(index, _)| index)
        .collect();
    if dead.is_empty() {
        return Ok(());
    }
    let was = tabs.active_target().map(str::to_string);
    for index in dead.into_iter().rev() {
        tabs.close(index);
    }
    if tabs.is_empty() {
        // The loop's own check ends the program, cleanly.
        return Ok(());
    }
    switched(pane, tabs, browser, chrome, was)?;
    redraw_row(pane, tabs, chrome)
}

/// The page's size in pixels: the pane, less the status row.
fn page_pixels(metrics: Metrics) -> (u32, u32) {
    let (w, h) = metrics.usable_pixels();
    (w.max(1), h.max(1))
}

/// The page's size in cells, which is what the placement asks for.
fn page_cells(metrics: Metrics) -> Cells {
    Cells {
        cols: metrics.cols.max(1),
        rows: metrics.usable_rows(),
    }
}

/// Draw the top row.
///
/// Five things share it, and which one is showing is a decision rather than a
/// layout. A url being typed takes the whole row however many tabs are open:
/// it is the one moment the person is writing rather than reading, and half a
/// url beside a strip would be neither. A dialog on the page in front comes
/// next and takes the whole row too — the page has stopped until it is
/// answered, so the question is the most important thing on the screen — but
/// after the url bar, because a person who pressed `ctrl+l` before the page
/// asked is in the middle of typing, and the question is there when they
/// finish. One tab is the row this program had before it had tabs, byte for
/// byte — a browser showing one page should not look like a browser with a
/// tab bar in it. More than one is the strip, where a tab behind with a dialog
/// of its own is marked.
///
/// And a download, which is the fifth and never takes the row: its words sit
/// beside the tab's line — at the right-hand end with one tab, in the url's
/// place after the strip with more — and never instead of it, because the
/// tab's line is what a person navigates by and a download is news, not a
/// place. Behind the url bar and a dialog, like the tab's line itself.
///
/// The find prompt is a line being typed like the url bar, with its count at
/// the right-hand end where a dialog's keys go; a file input's path, being
/// typed, is another. Which of them has the row is [`row_owner`]'s to say.
///
/// Once none of them has it the row is shared, and two more things share
/// it. On the left, a link under the pointer — `link: https://…` — in place
/// of the tab's line, because the pointer resting on a link is the person
/// asking where it goes. On the right, after a download's words, a load in
/// progress: `esc stops`, and after a second the seconds. [`screen::split_line`]
/// keeps the right when the pane is narrow, which is the dialog's rule for
/// the same reason — the key that does something is what has to survive.
/// With the strip, the right-hand choice goes in the url's place, the hover
/// words and the hint before a url that is already the first thing to go; a
/// plain-http page's url is marked `not secure` there, the strip's labels
/// being too narrow to carry it. Markers are ASCII words, not glyphs: a
/// padlock or a spinner is ambiguous-width in East Asian terminals, and a row
/// one cell out wraps.
///
/// The zoom level is one more word of the same kind: `150%` while the tab in
/// front is not at 100%. With one tab it goes after whatever else is at the
/// right-hand end — the download's words or the loading hint — because the
/// right is clipped from its end when the pane is narrow, and `esc stops` is
/// a key that does something while the level is only news. With the strip it
/// goes after the download, the link or the hint that stands in the url's
/// place, for the same reason, and before the url when the url is there, so
/// that it is the url that runs out of room first. Beside the title, never
/// instead of it, and nothing of it under a line that owns the row.
fn redraw_row(pane: &mut Pane, tabs: &Tabs<Client>, chrome: &Chrome) -> Result<(), String> {
    let cols = chrome.metrics.cols;
    let Some(active) = tabs.active() else {
        return Ok(());
    };
    let now = Instant::now();
    let downloading = chrome.downloads.line(now);
    let hovered = &chrome.hover.shown().href;
    let pointing = (!hovered.is_empty()).then(|| hover::words(hovered));
    let loading = active
        .loading
        .then(|| load::loading_hint(active.loading_for(now)));
    let marker = active.zoom.marker();
    let owner = row_owner(
        tabs,
        chrome.bar.as_ref(),
        chrome.find.as_ref(),
        chrome.list.as_ref(),
    );
    if let Some(RowOwner::List(list)) = &owner {
        // The rows under the row are the list's while it is open, and are
        // written first: the row after them is what puts the cursor back
        // where the typing is.
        pane.write(&list_screen(tabs, chrome, list))
            .map_err(|e| e.to_string())?;
    }
    let bytes = if let Some(owner) = owner {
        owned_row(cols, tabs, owner)
    } else if tabs.len() < 2 {
        let left = pointing.unwrap_or_else(|| active.line());
        // The download's words or the loading hint, then the level.
        match words(&[downloading.or(loading).as_deref(), marker.as_deref()]) {
            Some(right) => screen::split_line(cols, &left, &right),
            None => screen::status_line(cols, &left),
        }
    } else {
        // Held here first, because a label can be a sentence made on the spot
        // — a tab whose page did not come — and the strip only borrows.
        let names: Vec<_> = tabs.iter().map(|tab| tab.label()).collect();
        let labels: Vec<screen::TabLabel> = names
            .iter()
            .zip(tabs.iter())
            .enumerate()
            .map(|(index, (name, tab))| screen::TabLabel {
                title: name,
                active: index == tabs.active_index(),
                // A dialog or a file input's path: the page is waiting on
                // the person either way.
                dialog: tab.asks(),
            })
            .collect();
        // After the strip, the download, the link or the hint in the url's
        // place and the level after it; or the level, the transport's warning
        // and the url, which is the one to run out of room.
        let right = match downloading.or(pointing).or(loading) {
            Some(news) => words(&[Some(&news), marker.as_deref()]),
            None => words(&[marker.as_deref(), active.trust.words(), Some(&active.url)]),
        };
        // From where the strip's window started last time, so that it moves
        // only when the tab in front leaves it.
        let (bytes, first) = screen::tab_line_from(
            cols,
            &labels,
            right.as_deref().unwrap_or_default(),
            chrome.strip_first.get(),
        );
        chrome.strip_first.set(first);
        bytes
    };
    pane.write(&bytes).map_err(|e| e.to_string())
}

/// The tab list's rows under the status row, for the list as it is now,
/// keeping the window it chose for the next draw.
///
/// As many rows as the page had, which is every row but the status row:
/// the picture is off the screen while the list is up (see
/// [`open_list_screen`]), and text under a placement would not be seen.
fn list_screen(tabs: &Tabs<Client>, chrome: &Chrome, list: &TabList) -> Vec<u8> {
    let rows = chrome.metrics.usable_rows();
    let matches = list.matches(tabs);
    let window = list.window(matches.len(), rows as usize, chrome.list_first.get());
    chrome.list_first.set(window.first);
    let picked = list.picked(matches.len());
    let items: Vec<screen::ListItem> = matches
        .iter()
        .enumerate()
        .skip(window.first)
        .take(window.shown)
        .map(|(at, entry)| screen::ListItem {
            number: entry.index + 1,
            title: &entry.label,
            url: entry.url,
            picked: picked == Some(at),
            asks: entry.asks,
        })
        .collect();
    screen::list_rows(chrome.metrics.cols, PAGE_ROW, rows, &items)
}

/// The row as whichever of them owns it draws it.
fn owned_row<C>(cols: u32, tabs: &Tabs<C>, owner: RowOwner<'_>) -> Vec<u8> {
    match owner {
        RowOwner::Bar(bar) => typing_row(cols, "url: ", &bar.line),
        RowOwner::Find(find) => {
            typing_row_beside(cols, "find: ", &find.finder.line, &find.finder.count_text())
        }
        RowOwner::List(list) => typing_row_beside(
            cols,
            "tabs: ",
            &list.line,
            &TabList::count_text(list.matches(tabs).len(), tabs.len()),
        ),
        RowOwner::Dialog(dialog) if dialog.typing() => typing_row(
            cols,
            &screen::dialog_prompt(cols, &dialog.caption()),
            &dialog.line,
        ),
        RowOwner::Dialog(dialog) => screen::dialog_line(cols, &dialog.caption(), dialog.hint()),
        RowOwner::Upload(upload) => typing_row(
            cols,
            &screen::dialog_prompt(cols, &upload.prompt()),
            &upload.line,
        ),
    }
}

/// What has the whole row, when something does: something being typed, or
/// a question the page is stopped on.
///
/// Each of these is kept where it belongs — the url bar on [`Chrome`],
/// because it is the person's and `ctrl+l` opens the same bar on any tab; the
/// rest on the tab in front, because each is that page's — and what is
/// shared is the one rule for which of them has the row and the cursor.
enum RowOwner<'a> {
    Bar(&'a UrlBar),
    /// `ctrl+f`'s prompt: the person's, like the bar, and not any page's.
    Find(&'a Find),
    /// `ctrl+shift+a`'s tab list: the person's, like the two before it, and
    /// the one of them that has the rows under the row as well.
    List(&'a TabList),
    /// Any dialog: a `prompt()` is a line with the cursor, the others are a
    /// question answered by a key.
    Dialog(&'a Dialog),
    Upload(&'a Upload),
}

/// Which line owns the row: the url bar, then the find prompt, then the tab
/// list, then a dialog, then a file input's path — or nothing, and the row
/// is the page's own.
///
/// The url bar first, because it was opened by the person, and a question
/// that arrived while they were typing is there when they finish. The find
/// prompt next, for the same reason, and the tab list after it; the keyboard
/// cannot have two of the three open, so the order among them is only ever a
/// statement. The dialog is after the list because a page that asks while the
/// list is up is a page the person is not looking at, and the list is what
/// they are doing: the question is there when they pick, on its tab. A dialog
/// next, because the page is stopped behind it — even behind an upload
/// prompt, since a chooser does not stop the page and a script can
/// `alert()` while a path is half typed; the path waits underneath and comes
/// back when the alert is answered. The upload prompt last, because it is
/// the page's question and the page is not waiting on it.
///
/// A line that joins them goes in here, in its place in that order, as one
/// more arm of [`RowOwner`]; [`redraw_row`] and
/// [`row_owns_cursor`] follow from it. What does not follow from it is which
/// keys reach each one, which is [`handle_input`]'s, in the same order but
/// with a rule of its own for each about which of the program's keys survive
/// it; and [`paste`], which goes to the same place by the same order.
fn row_owner<'a, C>(
    tabs: &'a Tabs<C>,
    bar: Option<&'a UrlBar>,
    find: Option<&'a Find>,
    list: Option<&'a TabList>,
) -> Option<RowOwner<'a>> {
    if let Some(bar) = bar {
        return Some(RowOwner::Bar(bar));
    }
    if let Some(find) = find {
        return Some(RowOwner::Find(find));
    }
    if let Some(list) = list {
        return Some(RowOwner::List(list));
    }
    let tab = tabs.active()?;
    if let Some(dialog) = &tab.dialog {
        return Some(RowOwner::Dialog(dialog));
    }
    tab.upload.as_ref().map(RowOwner::Upload)
}

/// The words that are there, two spaces apart, or `None` for none.
fn words(parts: &[Option<&str>]) -> Option<String> {
    let there: Vec<&str> = parts.iter().flatten().copied().collect();
    (!there.is_empty()).then(|| there.join("  "))
}

/// The row as `line` being typed after `prompt`: as much of it as fits, with
/// the cursor in sight.
fn typing_row(cols: u32, prompt: &str, line: &Line) -> Vec<u8> {
    typing_row_beside(cols, prompt, line, "")
}

/// The same, with `right` at the right-hand end of the row: the find
/// prompt's count. With `right` empty it is [`typing_row`].
fn typing_row_beside(cols: u32, prompt: &str, line: &Line, right: &str) -> Vec<u8> {
    let view = line.view(screen::prompt_room_beside(cols, prompt, right));
    screen::prompt_line_beside(cols, prompt, &view.text, &view.hint, view.cursor, right)
}

/// Everything the browser connection said: which pages there are.
fn handle_target_events(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    chrome: &mut Chrome,
) -> Result<(), String> {
    let events = browser.events();
    if events.is_empty() {
        return Ok(());
    }
    let was = tabs.active_target().map(str::to_string);
    let mut redraw = false;
    let mut note: Option<String> = None;

    for event in &events {
        // A download's news comes on this connection and is nobody's tab's.
        // `tabs.take` ignores these, so they are read first and only here.
        if chrome.downloads.take(event, Instant::now()) {
            redraw = true;
        }
        let appearance = chrome.appearance;
        let mut frame = None;
        let outcome = tabs.take(event, |target| {
            connect_tab(browser, target, &appearance).map(|(connection, main)| {
                frame = main;
                connection
            })
        });
        match outcome {
            Outcome::Ignored => {}
            // In front or behind, the tab is told its main frame by index,
            // since a tab opened behind is not the active one. Nothing else
            // differs: `switched` below is a no-op for a tab behind, because
            // the target in front did not change, which is the whole of
            // "behind".
            Outcome::Opened { index } | Outcome::OpenedBehind { index } => {
                if let (Some(frame), Some(tab)) = (frame, tabs.get_mut(index)) {
                    tab.frame = Some(frame);
                }
                redraw = true;
            }
            Outcome::Renamed => redraw = true,
            Outcome::Gone { mut tab, why } => {
                tab.connection.close();
                redraw = true;
                note = note.or(why);
            }
            Outcome::Failed(why) => {
                note = Some(why);
                redraw = true;
            }
        }
    }

    if tabs.is_empty() {
        // Nothing left to show, so the program is over. A page that closed
        // itself is a clean exit; one that died says why on the way out, in the
        // shell, where the terminal has been given back and it can be read.
        return match note {
            Some(why) => Err(why),
            None => Ok(()),
        };
    }
    if let (Some(note), Some(tab)) = (note, tabs.active_mut()) {
        tab.note = Some(note);
    }
    switched(pane, tabs, browser, chrome, was)?;
    if redraw {
        redraw_row(pane, tabs, chrome)?;
    }
    Ok(())
}

/// Everything every tab said since the last look.
///
/// Every tab and not only the one in front, because a background tab is a page
/// that is still running: it loads, it navigates, it renames itself and it can
/// open a dialog that stops it rendering, and the strip has to say so. What a
/// background tab does not produce is frames, because its screencast was
/// stopped when it stopped being in front, so this costs one drained queue per
/// tab and nothing else.
///
/// Frames are coalesced: every one is acknowledged, because the engine sends
/// no more until it has been, but only the last is drawn. A pane that cannot
/// keep up with sixty frames a second should fall behind by dropping frames,
/// not by drawing a queue of stale ones.
fn handle_page_events(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    chrome: &mut Chrome,
) -> Result<(), String> {
    let active = tabs.active_index();
    // The newest frame worth painting, still encoded.
    let mut newest_frame: Option<Vec<u8>> = None;
    let mut redraw = false;
    // Whether the page the find prompt is searching has gone to another.
    let mut find_left = false;
    // Whether the page in front left or arrived, which takes away whatever
    // link was under the pointer.
    let mut left_page = false;

    for index in 0..tabs.len() {
        let Some(tab) = tabs.get_mut(index) else {
            continue;
        };
        let events = tab.connection.events();
        if events.is_empty() {
            continue;
        }
        let mut ask_title = false;

        for Event { method, params } in events {
            match method.as_str() {
                "Page.screencastFrame" => {
                    if let Some(session) = params.get("sessionId").and_then(Json::as_i64) {
                        let _ = tab.connection.notify(
                            "Page.screencastFrameAck",
                            Json::object(vec![("sessionId", Json::number(session as f64))]),
                        );
                    }
                    // A frame from a tab that is not in front is a frame from
                    // before it was left: it is acknowledged, so that the tab
                    // is not left waiting, and thrown away.
                    if index != active {
                        continue;
                    }
                    if let Some(data) = params.get("data").and_then(Json::as_str) {
                        // CDP's `TimeSinceEpoch`: seconds, on the clock this
                        // program reads too, which is what makes a frame and
                        // a still comparable at all.
                        let stamp = params
                            .path(&["metadata", "timestamp"])
                            .and_then(Json::as_f64);
                        // Every frame is told to the policy, even though only
                        // the last will be painted. Frames are coalesced here
                        // because a pane cannot draw sixty a second; the count
                        // in a still's window is not, because one frame there
                        // is the still photographing itself and two are the
                        // page moving — see [`motion::SHUTTER_FRAMES`] — and a
                        // pass that happened to collect two must not look like
                        // a pass that collected one.
                        if !chrome.motion.motion_frame(stamp, Instant::now()) {
                            continue;
                        }
                        match crate::base64::decode(data.as_bytes()) {
                            Ok(jpeg) => newest_frame = Some(jpeg),
                            // A frame that will not decode is a frame, not a
                            // session: the next one is along in sixteen
                            // milliseconds.
                            Err(_) => continue,
                        }
                    }
                }
                "Page.frameNavigated" => {
                    // Only the main frame, which is the one whose url is the
                    // page's: an advert in an iframe navigating is not. And
                    // for the engine's error page, the url that did not come
                    // rather than `chrome-error://chromewebdata/` — see
                    // [`crate::load`].
                    if let Some(landing) = load::landing(&params) {
                        // Read beside the landing, from the same frame: how
                        // the document came, and the main frame's id, which a
                        // landing is the surest word on.
                        tab.trust = load::trust(&params);
                        if let Some(frame) = load::landed_frame(&params) {
                            tab.frame = Some(frame);
                        }
                        tab.landed(landing);
                        redraw = true;
                        // The matches, their highlights and the world they
                        // were found from went with the document. The prompt
                        // closes rather than searching a page the person has
                        // not seen yet; the needle is kept for the next
                        // `ctrl+f`.
                        find_left |= chrome
                            .find
                            .as_ref()
                            .is_some_and(|find| find.target == tab.target);
                        if index == active {
                            left_page = true;
                        }
                        // A page lands at its host's level, as it would in a
                        // browser: a link from a zoomed site to another lands
                        // at that one's. The tab in front is told now — told,
                        // not asked, because a page that has just landed may
                        // already be asking something — and a tab behind is
                        // told when it comes to the front.
                        let wanted =
                            zoom::host_key(&tab.url).map(|host| chrome.zooms.get(Some(&host)));
                        if let Some(wanted) = wanted.filter(|&wanted| wanted != tab.zoom) {
                            tab.zoom = wanted;
                            if index == active {
                                let viewport = viewport(chrome, tab);
                                let _ = emulate(&mut tab.connection, viewport, true);
                                chrome.motion.reset(Instant::now());
                                chrome.still = None;
                                chrome
                                    .wheel
                                    .resized((viewport.css.0 as i32, viewport.css.1 as i32));
                            }
                        }
                    }
                }
                "Page.frameStartedNavigating" => {
                    // The page is leaving, typed or clicked, before any byte
                    // has come back: the note goes up now rather than at the
                    // commit, which for a host that never answers is never.
                    // See [`crate::load`].
                    if let Some(url) = load::started(&params, tab.frame.as_deref()) {
                        tab.started(url, Instant::now());
                        redraw = true;
                        if index == active {
                            left_page = true;
                        }
                    }
                }
                "Page.frameStartedLoading" => {
                    // The older engines' word for the same, with no url in
                    // it; and a `pushState`'s, which the stop undoes a
                    // millisecond later. No note, so the row shows the hint.
                    if load::is_main(&params, tab.frame.as_deref()) && !tab.loading {
                        tab.loading = true;
                        tab.since = Some(Instant::now());
                        redraw = true;
                    }
                }
                "Page.frameStoppedLoading" => {
                    // The end of every load, however it ended: finished,
                    // stopped, abandoned. After a stop no load event is ever
                    // coming, so this and not that is what says it is over.
                    if load::is_main(&params, tab.frame.as_deref()) {
                        tab.stopped_loading();
                        redraw = true;
                    }
                }
                "Page.loadEventFired" => {
                    // Still the moment to ask the title. What says the load
                    // is over is `Page.frameStoppedLoading`, in the same
                    // millisecond.
                    ask_title = true;
                    redraw = true;
                }
                "Page.javascriptDialogOpening" | "Page.javascriptDialogClosed" => {
                    // The question goes on the tab it was asked on, in front
                    // or not, and waits there for the person; what answers it
                    // is a key, in `answer_dialog`. A page with a question
                    // open is stopped, so it is not asked its title until the
                    // question has gone.
                    let closed = method == "Page.javascriptDialogClosed";
                    // Whether this was a page asking to be left and being
                    // told no, read before the event clears what it was. The
                    // engine can close a dialog itself, and a "stay" that was
                    // not typed here is a stay all the same.
                    let stay = closed
                        && params.get("result").and_then(Json::as_bool) == Some(false)
                        && tab
                            .dialog
                            .as_ref()
                            .is_some_and(|dialog| dialog.kind == Kind::BeforeUnload);
                    if tab.dialog_event(&Event { method, params }) {
                        redraw = true;
                    }
                    if stay {
                        stayed(tab);
                    }
                    // A script that carries on after its dialog often
                    // renames the page, and the row shows the name.
                    if closed {
                        ask_title = true;
                        redraw = true;
                    }
                }
                "Page.fileChooserOpened" => {
                    // A click on a file input, on this tab, in front or not:
                    // a path to type on the row, answered by a key in
                    // `answer_upload`. The page is not stopped.
                    let base = chrome.upload_base();
                    let event = Event { method, params };
                    if tab.chooser_event(&event, &base, upload::home().as_deref()) {
                        redraw = true;
                    }
                }
                _ => {}
            }
        }

        if ask_title && tab.dialog.is_none() {
            if let Some(loaded) = page_loaded(&mut tab.connection) {
                tab.loaded(loaded);
                // The one moment the page's final url, after its redirects,
                // and its title are both known: this is a visit. A page that
                // did not come has a problem and is not one; `about:` and
                // `data:` are refused by the history itself. A tab behind is
                // in this loop too, so a page opened in a new window counts.
                // What it costs is one line appended to a file, and a history
                // that cannot be written is not a reason to stop.
                if tab.problem.is_none() && History::records(&tab.url) {
                    let _ = chrome.history.visited(&tab.url, &tab.title, unix_now());
                }
            }
        }
    }

    if find_left {
        close_find(tabs, chrome);
    }
    if left_page {
        forget_hover(pane, chrome)?;
    }
    if redraw {
        redraw_row(pane, tabs, chrome)?;
    }
    if let Some(jpeg) = newest_frame {
        // Ordered above, decoded here: a frame that lost to the still on
        // screen is eight milliseconds of work not done.
        //
        // A frame that will not decode is dropped on the same rule as one that
        // would not base64: one of them is nothing. A run of them is a page
        // that looks frozen, which is what the engine test comparing the two
        // formats through both decoders exists to catch before a person meets
        // it.
        if let Ok(image) = tos_term::jpeg::decode(&jpeg, FRAME_BUDGET) {
            let raw = Raw::rgb(&image.rgb, image.width, image.height);
            paint(pane, tabs, chrome, raw)?;
        }
    }
    Ok(())
}

/// Put a decoded frame on the screen.
fn paint(
    pane: &mut Pane,
    tabs: &Tabs<Client>,
    chrome: &mut Chrome,
    raw: Raw<'_>,
) -> Result<(), String> {
    // The tab list has the rows the picture would go on. The frame was
    // acknowledged and counted already, so the motion policy's clock is
    // right when the list closes; only the drawing stands down.
    if chrome.list.is_some() {
        return Ok(());
    }
    let bytes = chrome
        .painter
        .frame(raw, page_cells(chrome.metrics), PAGE_ROW, 1);
    pane.write(&bytes).map_err(|e| e.to_string())?;
    // The picture does not move the cursor (`C=1`), but the status line owns
    // the cursor's position when something is being typed on it, so it is
    // written again rather than left where the last frame found it.
    if row_owns_cursor(tabs, chrome) {
        redraw_row(pane, tabs, chrome)?;
    }
    Ok(())
}

/// Whether the row is a line being typed into: the url bar, the find
/// prompt, the answer to a `prompt()` on the page in front, or a path for its
/// file input — whichever [`row_owner`] says has the row.
fn row_owns_cursor(tabs: &Tabs<Client>, chrome: &Chrome) -> bool {
    typing_line(tabs, chrome).is_some()
}

/// Which line being typed has the row, when one has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Typing {
    /// `ctrl+l`, or a new tab.
    Url,
    /// `ctrl+f`.
    Find,
    /// `ctrl+shift+a`'s filter.
    List,
    /// A page's `prompt()`.
    Prompt,
    /// A path for the page's file input.
    Upload,
}

/// The line a key, a paste and the cursor go to: [`row_owner`]'s order, less
/// a dialog that is answered by a key rather than typed into — an alert has
/// the row and no line, and a path half typed under it waits. Derived rather
/// than decided again, so that what the row shows and where the typing goes
/// cannot disagree.
fn typing_line(tabs: &Tabs<Client>, chrome: &Chrome) -> Option<Typing> {
    match row_owner(
        tabs,
        chrome.bar.as_ref(),
        chrome.find.as_ref(),
        chrome.list.as_ref(),
    )? {
        RowOwner::Bar(_) => Some(Typing::Url),
        RowOwner::Find(_) => Some(Typing::Find),
        RowOwner::List(_) => Some(Typing::List),
        RowOwner::Dialog(dialog) => dialog.typing().then_some(Typing::Prompt),
        RowOwner::Upload(_) => Some(Typing::Upload),
    }
}

/// Whether the page in front is stopped behind a dialog.
fn asking(tabs: &Tabs<Client>) -> bool {
    tabs.active().is_some_and(|tab| tab.dialog.is_some())
}

/// A page that has stopped moving gets one lossless picture of itself.
///
/// This is the other half of [`crate::motion`]'s policy: the screencast is
/// JPEG so that a scroll keeps up, and once it has stopped the text somebody
/// is about to read is replaced with the PNG of the same page. It costs one
/// `Page.captureScreenshot` per stop and nothing at all while the page stays
/// still, so a static page is one still and then silence.
///
/// Nothing here waits. The screenshot is 66 to 98 milliseconds of engine at a
/// pane's size, and a loop that sat in a `call` for them would be a loop that
/// was not reading the terminal — which is what made a key pressed during one
/// arrive a tenth of a second late, and sometimes need pressing twice. So the
/// reply is collected first, and a new request only goes out when there is
/// nothing outstanding and the page has been quiet in both the ways
/// [`crate::motion`] asks about.
fn rest_shot(pane: &mut Pane, tabs: &mut Tabs<Client>, chrome: &mut Chrome) -> Result<(), String> {
    collect_still(pane, tabs, chrome)?;
    request_still(tabs, chrome);
    Ok(())
}

/// Take the reply to the still, if it has come back.
///
/// Called after the page's events have been drained, so that a frame which
/// arrived on the same pass as the reply has already been counted against it —
/// which matters, because the count is the rule: one frame in the window is
/// the still photographing itself and more than one is the page moving. See
/// [`motion::SHUTTER_FRAMES`].
fn collect_still(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    chrome: &mut Chrome,
) -> Result<(), String> {
    let Some(still) = chrome.still.as_ref() else {
        return Ok(());
    };
    if tabs.active_target() != Some(still.target.as_str()) {
        // The tab was switched away from or closed while the engine drew. The
        // motion state was reset with the switch, so there is nothing to tell
        // it; dropping the still drops its `Pending`, which is what tells the
        // mailbox not to keep a megabyte of picture nobody will collect.
        chrome.still = None;
        return Ok(());
    }
    let timed_out = still.sent.elapsed() >= STILL_TIMEOUT;
    let Some(tab) = tabs.active_mut() else {
        return Ok(());
    };
    let Some(answer) = tab.connection.take_reply(&still.pending) else {
        if timed_out {
            chrome.still = None;
            chrome.motion.still_failed();
        }
        return Ok(());
    };
    chrome.still = None;
    // Whether it is worth having is asked before it is decoded: a still that
    // lost to a frame is a megabyte of PNG this loop does not have to look at,
    // and the moment it loses is the moment the page is moving and the time is
    // wanted elsewhere.
    if !chrome.motion.still_arrived(motion::now_seconds()) {
        return Ok(());
    }
    let decoded = answer
        .ok()
        .and_then(|reply| reply.get("data").and_then(Json::as_str).map(str::to_string))
        .and_then(|data| crate::base64::decode(data.as_bytes()).ok())
        .and_then(|png| tos_term::png::decode(&png, FRAME_BUDGET).ok());
    let Some(image) = decoded else {
        // The tab stays marked at rest, which is what stops a page whose
        // screenshots will not decode being asked again every pass.
        chrome.motion.still_failed();
        return Ok(());
    };
    // At a fractional level the engine's rounding leaves the still a pixel or
    // two off the pane, and a picture that is not the pane's size is one the
    // terminal resamples until the text goes soft. See [`zoom::fit`].
    let pane_pixels = page_pixels(chrome.metrics);
    let fitted = zoom::fit(&image.rgba, image.width, image.height, 4, pane_pixels);
    let raw = match &fitted {
        Some(pixels) => Raw::rgba(pixels, pane_pixels.0, pane_pixels.1),
        None => Raw::rgba(&image.rgba, image.width, image.height),
    };
    paint(pane, tabs, chrome, raw)
}

/// Ask for a still, if the page has earned one.
///
/// A failure to send is a failure of the still and not of the program: the
/// session going is heard on the next pass by everything that cares.
///
/// Not while the page has a dialog open. The engine does not draw a page that
/// is stopped, and a still asked for then would sit out [`STILL_TIMEOUT`] and
/// be marked a failure — which is a page that then never gets its lossless
/// picture once it has been answered and has gone quiet again.
///
/// Nor while the tab list is open: the picture is off the screen, and one
/// taken now would be painted over nothing. Closing the list resets the
/// motion clock, which is what asks for the still then.
fn request_still(tabs: &mut Tabs<Client>, chrome: &mut Chrome) {
    if asking(tabs) || chrome.list.is_some() || !chrome.motion.wants_still(Instant::now()) {
        return;
    }
    let Some(target) = tabs.active_target().map(str::to_string) else {
        return;
    };
    let Some(tab) = tabs.active_mut() else {
        return;
    };
    let sent = tab.connection.send(
        "Page.captureScreenshot",
        Json::object(vec![("format", Json::string("png"))]),
    );
    match sent {
        Ok(pending) => {
            chrome.motion.still_requested();
            chrome.still = Some(Still {
                target,
                pending,
                sent: Instant::now(),
            });
        }
        Err(_) => chrome.motion.still_failed(),
    }
}

/// Where a step of the animation goes: one `mouseWheel` on the tab that was
/// being scrolled, put on the wire by the animator's own thread.
///
/// The event goes out with [`crate::cdp::Notifier::notify`], which is what an
/// acknowledged screencast frame uses: a `mouseWheel` has nothing to say back,
/// and fourteen round trips per notch would be fourteen replies to collect and
/// a `Pending` to carry for each of them. Chromium answers a notification all
/// the same, and those answers cost nothing: a notification's id is never
/// registered with the mailbox, so the reader thread drops its reply where it
/// reads it.
///
/// This is the only part of the animation that knows what CDP is, which is why
/// it is here rather than in [`crate::scroll`] — that module is arithmetic on
/// a distance and a clock, and it stays testable without an engine.
pub struct Wire(Notifier);

impl Wire {
    pub fn new(notifier: Notifier) -> Wire {
        Wire(notifier)
    }
}

impl scroll::Dispatch for Wire {
    fn send(&self, step: Step) -> Result<(), String> {
        self.0.notify(
            "Input.dispatchMouseEvent",
            Json::object(vec![
                ("type", Json::string("mouseWheel")),
                ("x", Json::number(step.at.0)),
                ("y", Json::number(step.at.1)),
                ("deltaX", Json::number(step.delta.0)),
                ("deltaY", Json::number(step.delta.1)),
                ("modifiers", Json::number(0)),
                ("button", Json::string("none")),
                ("buttons", Json::number(0)),
            ]),
        )
    }
}

/// One wheel notch: a curve handed to the animator thread.
///
/// Nothing is sent from here and nothing is timed from here. The notch starts
/// a curve of its own beside whatever is already running, and the thread pays
/// all of them out a tick at a time — which is what makes a second notch in
/// the middle of the first add to the movement rather than restart it, and
/// what keeps the ticks off this loop's schedule.
///
/// The sign is the page's: `deltaY` of +120 on a `mouseWheel` leaves
/// `window.scrollY` at 120, and `deltaX` of +120 leaves `scrollX` at 120 —
/// checked against the engine rather than read off the documentation, and the
/// opposite of what `Input.synthesizeScrollGesture`'s `yDistance` wanted.
fn scroll(tabs: &Tabs<Client>, chrome: &Chrome, report: &MouseInput) {
    let pixels = chrome.parser.pixel_coordinates();
    let (x, y) = crate::input::page_point(report, pixels, chrome.metrics.cell, 1);
    if y < 0 {
        // The status row is this program's, and turning the wheel over it is
        // not the page's business.
        return;
    }
    let Some(tab) = tabs.active() else {
        return;
    };
    // In the page's pixels, both of them: the point, and a notch that moves
    // the page the same distance on the screen whatever it is zoomed to.
    let viewport = viewport(chrome, tab);
    let at = css_point(viewport, (x, y));
    chrome.wheel.notch(
        &tab.target,
        Arc::new(Wire::new(tab.connection.notifier())),
        (at.0.round() as i32, at.1.round() as i32),
        viewport.notch(report.wheel, WHEEL_PIXELS),
    );
}

/// Once a pass: what the pointer did, told to the page and asked about, for
/// the tab in front.
///
/// Three steps, each cheap and each at most once. The answer to the last ask
/// is collected first, so that the row and the pointer's shape change the
/// pass it arrives. Then the page is told where the pointer is — one
/// `mouseMoved` at the latest position however many reports there were,
/// which is what makes its hover styling and tooltips work — and not counted
/// as input to [`crate::motion`], for the reason the mouse arm of
/// [`handle_input`] gives. Then, if nothing is out, the page is asked what is
/// under it: not while anything [`row_owner`] names has the row — the url
/// bar, the find prompt, a dialog that has stopped the page, a file input's
/// path — and not while a navigation has not committed, when the engine
/// would hold the question until it did ([`crate::hover`]).
///
/// Nothing at all while one of those has the row, the page's `mouseMoved`
/// included, rather than only no ask. The words the ask is for would have
/// nowhere to go, since whatever has the row takes all of it; a dialog's page
/// is stopped and would queue the moves behind its question; and the person
/// is answering or typing on the row, and should have an arrow to do it with
/// rather than the hand of a link they are not about to follow. The pointer
/// is asked about again when the row is the page's once more, the first time
/// it moves.
fn tick_hover(pane: &mut Pane, tabs: &mut Tabs<Client>, chrome: &mut Chrome) -> Result<(), String> {
    collect_hover(pane, tabs, chrome)?;
    let owned = row_owner(
        tabs,
        chrome.bar.as_ref(),
        chrome.find.as_ref(),
        chrome.list.as_ref(),
    )
    .is_some();
    if owned || tabs.active().is_none() {
        return forget_hover(pane, chrome);
    }
    let now = Instant::now();
    let buttons = chrome.buttons;
    // The tracker keeps the pointer in the terminal's pixels; the page is
    // told and asked in its own, at the level it is at now.
    let Some(viewport) = tabs.active().map(|tab| viewport(chrome, tab)) else {
        return Ok(());
    };
    let Some(tab) = tabs.active_mut() else {
        return Ok(());
    };
    if let Some((at, modifiers)) = chrome.hover.tell(now) {
        let (x, y) = css_point(viewport, at);
        let _ = tab.connection.notify(
            "Input.dispatchMouseEvent",
            Json::object(vec![
                ("type", Json::string("mouseMoved")),
                ("x", Json::number(x)),
                ("y", Json::number(y)),
                ("modifiers", Json::number(modifiers)),
                ("button", Json::string("none")),
                ("buttons", Json::number(buttons)),
            ]),
        );
    }
    if tab.loading && !tab.committed {
        return Ok(());
    }
    let Some(at) = chrome.hover.wants_ask(now, chrome.asking.is_some()) else {
        return Ok(());
    };
    // Recorded as asked whether or not it could be sent: a session that
    // cannot take a command is heard about elsewhere, and asking it again
    // every pass would not help.
    chrome.hover.asked(at, now);
    let (x, y) = css_point(viewport, at);
    if let Ok(pending) = tab.connection.send(
        "Runtime.evaluate",
        hover::ask(x.round() as i32, y.round() as i32),
    ) {
        chrome.asking = Some(Ask {
            target: tab.target.clone(),
            at,
            pending,
            sent: now,
        });
    }
    Ok(())
}

/// Take the answer to the hover ask, if it has come back; drop it if the
/// tab it was for is no longer in front or it has been out for
/// [`hover::ASK_TIMEOUT`]. A dropped ask leaves its position marked asked, so
/// the page is asked again when the pointer moves and not before.
fn collect_hover(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    chrome: &mut Chrome,
) -> Result<(), String> {
    let Some(ask) = chrome.asking.as_ref() else {
        return Ok(());
    };
    if tabs.active_target() != Some(ask.target.as_str()) {
        chrome.asking = None;
        return Ok(());
    }
    let Some(tab) = tabs.active() else {
        return Ok(());
    };
    let Some(answer) = tab.connection.take_reply(&ask.pending) else {
        if ask.sent.elapsed() >= hover::ASK_TIMEOUT {
            chrome.asking = None;
        }
        return Ok(());
    };
    let at = ask.at;
    chrome.asking = None;
    let Some(hovered) = answer.ok().as_ref().and_then(hover::answer) else {
        return Ok(());
    };
    if chrome.hover.answered(at, hovered) {
        redraw_row(pane, tabs, chrome)?;
    }
    sync_shape(pane, chrome)
}

/// The pointer is not over the page in front any more, or the page is not
/// the one it was over: forget what was under it, drop the ask about it, and
/// give the terminal its arrow back. The caller redraws the row; nothing
/// here does, so that a caller about to redraw anyway is not made to twice.
fn forget_hover(pane: &mut Pane, chrome: &mut Chrome) -> Result<(), String> {
    chrome.asking = None;
    chrome.hover.left();
    sync_shape(pane, chrome)
}

/// Tell the terminal the pointer's shape, if it is not the one it was last
/// told. The name is [`Shape::name`]'s, from this program's own table; the
/// page's `cursor` string only chose which. See [`screen::pointer_shape`].
fn sync_shape(pane: &mut Pane, chrome: &mut Chrome) -> Result<(), String> {
    let shape = chrome.hover.shown().shape;
    if shape == chrome.shape {
        return Ok(());
    }
    chrome.shape = shape;
    pane.write(&screen::pointer_shape(shape.name()))
        .map_err(|e| e.to_string())
}

/// Whether a mouse report is the pointer merely moving — no button held —
/// which is the hover's and not sent on as it comes. A drag is a move with a
/// button, a selection being made, and every report of it goes to the page.
fn routes_to_hover(report: &MouseInput) -> bool {
    report.kind == MouseKind::Move && report.button.is_none()
}

/// Where a point in the page's part of the pane is on the page, in CSS
/// pixels: the one place a terminal's coordinates become the page's, for a
/// click, a wheel notch and the hover alike.
///
/// The arithmetic is [`Viewport::css_point`]'s and nowhere else's; this is
/// the loop's one door to it, so that a caller holding a terminal point asks
/// here with the [`viewport`] of the tab it is about, and cannot do the
/// division some other way. What comes in is [`crate::input::page_point`]'s
/// — the report in terminal pixels, less the status row — and everything
/// that is about the screen rather than the page is decided on that before
/// it gets here: whether the point is on the row, whether two presses are a
/// double click, and whether the pointer has moved far enough to ask the page
/// again ([`hover::ASK_STEP`]), which at 300% would otherwise be twelve of
/// the screen's pixels and at 25% one.
fn css_point(viewport: Viewport, at: (i32, i32)) -> (f64, f64) {
    viewport.css_point(at.0, at.1)
}

/// Handle one thing the terminal said. `false` means quit.
fn handle_input(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    chrome: &mut Chrome,
    input: Input,
) -> Result<bool, String> {
    match input {
        Input::Mode { .. } => {}
        // The terminal's background, which this program asked for: if it
        // changes what pages are told, every tab is told again, in front or
        // behind, and a page already loaded changes where it stands. Told,
        // not asked, so that a tab stopped behind a dialog costs nothing.
        Input::Colour { slot: 11, rgb } => {
            if chrome.appearance.learned(rgb) {
                for index in 0..tabs.len() {
                    if let Some(tab) = tabs.get_mut(index) {
                        prepare_session(&mut tab.connection, &chrome.appearance);
                    }
                }
            }
        }
        // Another colour is nothing this program asked about.
        Input::Colour { .. } => {}
        // How big the terminal says a cell is. The pane is measured again
        // on the next pass, as if it had been resized, which it has as far
        // as the page is concerned when the kernel had no pixels to go on:
        // the page, the placement and the HiDPI guess were all made from
        // 8x16. A hint that changes nothing measured — a kernel that knew
        // the pixels all along — is not a resize.
        Input::CellSize { width, height } => {
            chrome.cell_hint = Some((width, height));
            let measured = pane.metrics(chrome.cell_hint);
            if measured.is_ok_and(|metrics| metrics.cell != chrome.metrics.cell) {
                RESIZED.store(true, Ordering::SeqCst);
            }
        }
        Input::PasteRefused { bytes } => {
            note(
                tabs,
                format!(
                    "paste refused: {} KiB is over {} KiB",
                    bytes.div_ceil(1024),
                    crate::input::PASTE_LIMIT / 1024
                ),
            );
            redraw_row(pane, tabs, chrome)?;
        }
        Input::Paste(text) => {
            // A paste is a person working on this page, as a key is.
            chrome.motion.input(Instant::now());
            paste(pane, tabs, chrome, &text)?;
        }
        Input::Key(key) => {
            // A key is a person working on this page, which is a reason not to
            // interrupt them with a screenshot — see [`motion::INPUT_QUIET`].
            // A release is not: it follows a press that has already been
            // counted, and a modifier let go on its own moves nothing.
            if key.action != KeyAction::Release {
                chrome.motion.input(Instant::now());
            }
            // A line being typed on the row is asked about every key first;
            // a `prompt()` is answered below, with the dialog it belongs to.
            match typing_line(tabs, chrome) {
                Some(Typing::Url) => return edit_url(pane, tabs, chrome, key),
                Some(Typing::Find) => return edit_find(pane, tabs, chrome, key),
                Some(Typing::List) => return edit_list(pane, tabs, browser, chrome, key),
                // A `prompt()` and a file input's path are answered below,
                // after the program's own keys have had their say.
                Some(Typing::Prompt | Typing::Upload) | None => {}
            }
            let was = tabs.active_target().map(str::to_string);
            let command = command(&key);
            if asking(tabs) {
                // The page is waiting on an answer. The tab keys still work,
                // because leaving the question where it is — or closing the
                // tab it is on — is an answer too; the rest wait for it, and
                // every key that is not one of the program's is the answer.
                match command {
                    // On a `prompt()` the line being typed is the only text
                    // on the screen the person can mean; on the others there
                    // is nothing, and the page cannot be asked.
                    Some(Command::CopySelection) => {
                        let typed = tabs
                            .active()
                            .and_then(|tab| tab.dialog.as_ref())
                            .filter(|dialog| dialog.typing())
                            .map(|dialog| dialog.line.text().to_string());
                        if let Some(typed) = typed {
                            copy_out(pane, tabs, &typed, Copied::Text)?;
                            redraw_row(pane, tabs, chrome)?;
                        }
                        return Ok(true);
                    }
                    Some(command) if survives_dialog(command) => {}
                    Some(_) => return Ok(true),
                    None => return answer_dialog(pane, tabs, chrome, key),
                }
            }
            if tabs.active().is_some_and(|tab| tab.upload.is_some()) {
                // A file input's path is being typed. The page is not
                // stopped, but the keys are the path's, by the same rule as a
                // dialog's: the tab keys and quit work, and `ctrl+l`, reload,
                // back and forward wait — a half-typed path is not something
                // to navigate away from by reflex, and Escape is one key.
                match command {
                    Some(Command::CopySelection) => {
                        let typed = tabs
                            .active()
                            .and_then(|tab| tab.upload.as_ref())
                            .map(|upload| upload.line.text().to_string())
                            .unwrap_or_default();
                        copy_out(pane, tabs, &typed, Copied::Text)?;
                        redraw_row(pane, tabs, chrome)?;
                        return Ok(true);
                    }
                    Some(command) if survives_dialog(command) => {}
                    Some(_) => return Ok(true),
                    None => return answer_upload(pane, tabs, chrome, key),
                }
            }
            match command {
                Some(Command::Quit) => return Ok(false),
                Some(Command::EditUrl) => {
                    chrome.bar = tabs
                        .active()
                        .map(|tab| UrlBar::new(Line::selected(tab.url.clone())));
                    redraw_row(pane, tabs, chrome)?;
                }
                Some(Command::Find) => {
                    open_find(tabs, chrome);
                    redraw_row(pane, tabs, chrome)?;
                }
                Some(Command::ListTabs) => {
                    chrome.list = Some(TabList::open(tabs.active_index()));
                    chrome.list_first.set(0);
                    open_list_screen(pane, chrome)?;
                    redraw_row(pane, tabs, chrome)?;
                }
                Some(Command::Reload) => {
                    // A page that did not come has no document to reload, and
                    // `Page.reload` of the error page says nothing about why it
                    // failed again. Going to the same url once more does: it is
                    // a `Page.navigate`, whose reply carries the reason. The
                    // engine takes a navigation to the url it is already on as
                    // a reload, so no second history entry is made — checked
                    // against the engine, in the engine tests.
                    let unreachable = match tabs.active().and_then(|tab| tab.problem.as_ref()) {
                        Some(Problem::Unreachable { url, .. }) => Some(url.clone()),
                        _ => None,
                    };
                    match unreachable {
                        Some(url) => {
                            if let Some(tab) = tabs.active_mut() {
                                tab.note = Some(format!("loading {url}"));
                                tab.loading = true;
                            }
                            redraw_row(pane, tabs, chrome)?;
                            if let Some(why) = navigate(tabs, chrome, &url) {
                                if let Some(tab) = tabs.active_mut() {
                                    tab.note = Some(why);
                                }
                            }
                            redraw_row(pane, tabs, chrome)?;
                        }
                        None => {
                            if let Some(tab) = tabs.active_mut() {
                                let _ = tab.connection.call("Page.reload", Json::empty());
                            }
                        }
                    }
                }
                Some(Command::Back) => go(tabs, -1),
                Some(Command::Forward) => go(tabs, 1),
                Some(Command::NewTab) => {
                    let appearance = chrome.appearance;
                    match open_tab(tabs, browser, &appearance, "about:blank") {
                        Ok(()) => {
                            switched(pane, tabs, browser, chrome, was)?;
                            // A new tab is a tab somebody is about to type an
                            // address into, so it opens with the cursor in the
                            // url bar — and with nothing in it, because there
                            // is no address here to replace.
                            chrome.bar = Some(UrlBar::new(Line::empty()));
                        }
                        Err(why) => {
                            if let Some(tab) = tabs.active_mut() {
                                tab.note = Some(why);
                            }
                        }
                    }
                    redraw_row(pane, tabs, chrome)?;
                }
                Some(Command::CloseTab) => {
                    // Closing the only tab is closing the browser, which is
                    // what every browser does and what `ctrl+q` does here.
                    if tabs.len() < 2 {
                        return Ok(false);
                    }
                    close_tab(tabs, browser, tabs.active_index());
                    switched(pane, tabs, browser, chrome, was)?;
                    redraw_row(pane, tabs, chrome)?;
                }
                Some(
                    what @ (Command::NextTab
                    | Command::PreviousTab
                    | Command::SelectTab(_)
                    | Command::LastTab),
                ) => {
                    let moved = match what {
                        Command::NextTab => tabs.select_next(),
                        Command::PreviousTab => tabs.select_previous(),
                        Command::SelectTab(number) => tabs.select(number),
                        Command::LastTab => tabs.select_last(),
                        _ => false,
                    };
                    if moved {
                        switched(pane, tabs, browser, chrome, was)?;
                        redraw_row(pane, tabs, chrome)?;
                    }
                }
                Some(Command::MoveTab(direction)) => {
                    // Nothing switched: the same page is in front, under a
                    // different number.
                    if tabs.move_active(direction) {
                        redraw_row(pane, tabs, chrome)?;
                    }
                }
                Some(Command::CopyUrl) => {
                    let url = tabs.active().map(|tab| tab.url.clone()).unwrap_or_default();
                    copy_out(pane, tabs, &url, Copied::Url)?;
                    redraw_row(pane, tabs, chrome)?;
                }
                Some(what @ (Command::ZoomIn | Command::ZoomOut | Command::ZoomReset)) => {
                    let now = tabs.active().map(|tab| tab.zoom).unwrap_or_default();
                    let wanted = match what {
                        Command::ZoomIn => now.step_in(),
                        Command::ZoomOut => now.step_out(),
                        _ => Zoom::DEFAULT,
                    };
                    rezoom(pane, tabs, chrome, wanted)?;
                }
                Some(Command::CopySelection) => {
                    let answer = tabs.active_mut().map(|tab| {
                        tab.connection.call_within(
                            "Runtime.evaluate",
                            clipboard::selection_params(),
                            SELECTION_TIMEOUT,
                        )
                    });
                    match answer.map(|reply| reply.map(|reply| clipboard::selection(&reply))) {
                        Some(Ok(Some(text))) if !text.is_empty() => {
                            copy_out(pane, tabs, &text, Copied::Text)?;
                        }
                        Some(Err(_)) => note(tabs, "the page did not answer"),
                        _ => note(tabs, "nothing selected"),
                    }
                    redraw_row(pane, tabs, chrome)?;
                }
                None => {
                    let loading = tabs.active().is_some_and(|tab| tab.loading);
                    let stops = key.key == Key::Escape
                        && key.action != KeyAction::Release
                        && escapes(
                            row_owner(
                                tabs,
                                chrome.bar.as_ref(),
                                chrome.find.as_ref(),
                                chrome.list.as_ref(),
                            )
                            .as_ref(),
                            loading,
                        ) == Escapes::StopLoading;
                    if stops {
                        stop_loading(pane, tabs, chrome)?;
                    } else if let Some(tab) = tabs.active_mut() {
                        send_key(&mut tab.connection, &key);
                    }
                }
            }
        }
        Input::Mouse(report) => {
            // The tab list has the screen: the page is not on it, so nothing
            // on the page can be meant. A press on a row picks that row; the
            // rest is dropped. Before the dialog's rule, because the list
            // has the row over a dialog too.
            if chrome.list.is_some() {
                if routes_to_list(&report, true) {
                    return click_list(pane, tabs, browser, chrome, &report);
                }
                return Ok(true);
            }
            // A page with a question open is not a page to click on or
            // scroll. What was sent would not be lost — the engine queues it
            // behind the dialog — which is worse: a click meant for the
            // question would land on the page the moment it was answered.
            if asking(tabs) {
                return Ok(true);
            }
            // Only the wheel. A hand on a wheel is what the still has to keep
            // out of the way of; a pointer drifting across a page is
            // not, and counting moves would mean a page nobody had scrolled
            // never got its lossless picture at all.
            if report.kind == MouseKind::Wheel {
                chrome.motion.input(Instant::now());
                // Not an event but a curve: what puts it on the wire is
                // [`crate::scroll::Wheel`]'s thread, on its own clock.
                scroll(tabs, chrome, &report);
                // And the page moves under a pointer that did not, so what
                // is under it is asked again.
                chrome.hover.scrolled();
                return Ok(true);
            }
            let cell = chrome.metrics.cell;
            let pixels = chrome.parser.pixel_coordinates();
            // In the terminal's pixels, inside the page: whether it is on the
            // row, and how far the pointer has moved, are about the screen.
            // The page's pixels come after, at the one place they are made.
            let point = crate::input::page_point(&report, pixels, cell, 1);
            // A bare motion is the hover's: remembered here and told to the
            // page once a pass by `tick_hover`, since a terminal in any-event
            // mode can send a thousand of them a second. Over the row it is
            // the pointer leaving the page.
            if routes_to_hover(&report) {
                if point.1 >= 0 {
                    chrome.hover.moved(point, report.mods.cdp());
                    return Ok(true);
                }
                let shown = !chrome.hover.shown().href.is_empty();
                forget_hover(pane, chrome)?;
                if shown {
                    redraw_row(pane, tabs, chrome)?;
                }
                return Ok(true);
            }
            // A press is where the pointer is, too, so a click's position
            // is the hover's.
            if report.kind == MouseKind::Press && point.1 >= 0 {
                chrome.hover.moved(point, report.mods.cdp());
            }
            let Some(viewport) = tabs.active().map(|tab| viewport(chrome, tab)) else {
                return Ok(true);
            };
            let clicks = &mut chrome.clicks;
            let buttons = &mut chrome.buttons;
            if let Some(tab) = tabs.active_mut() {
                send_mouse(
                    &mut tab.connection,
                    pixels,
                    clicks,
                    buttons,
                    cell,
                    viewport,
                    report,
                );
            }
        }
    }
    Ok(true)
}

/// The keys this program keeps for itself. Everything else is the page's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Quit,
    EditUrl,
    Reload,
    Back,
    Forward,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    /// The nth tab, counted from one.
    SelectTab(usize),
    /// `alt+9`: the last tab, however many there are.
    LastTab,
    /// `ctrl+shift+a` or `alt+a`: the tab list. See [`crate::tablist`].
    ListTabs,
    /// `ctrl+shift+pageup`/`pagedown` or `alt+shift+pageup`/`pagedown`: the
    /// tab in front one place left (`-1`) or right (`+1`).
    MoveTab(isize),
    /// `alt+c`: the page's selection to the host's clipboard — or, with the
    /// url bar, the find prompt, a `prompt()`'s line or a file input's path
    /// open, that line.
    CopySelection,
    /// `alt+u`: the current url to the host's clipboard.
    CopyUrl,
    /// `ctrl+f`: find in the page. See [`crate::find`].
    Find,
    /// `alt+=` or `ctrl+=`: the next of [`crate::zoom::LEVELS`] up.
    ZoomIn,
    /// `alt+-` or `ctrl+-`: the next one down.
    ZoomOut,
    /// `alt+0` or `ctrl+0`: back to 100%.
    ZoomReset,
}

/// Whether a key the program keeps for itself still works while the page in
/// front has a dialog open.
///
/// Quitting, and everything about tabs: opening one, closing this one — which
/// closes its dialog with it, see [`close_tab`] — and going to another, which
/// leaves the question on its tab, marked in the strip, for later; the list
/// of them, which is a way of going to another; and moving this one, which
/// reorders the strip around the question and does nothing to the page. What waits
/// is everything that would do something to the page that is asking. The url
/// bar would type over the question the person is meant to be reading; a
/// reload, a back or a forward to a page that is stopped would be queued
/// behind its dialog and done the moment it was answered, which is a
/// navigation nobody would remember asking for by then.
///
/// Copying the url survives too: it reads what this program already knows and
/// touches the page not at all. Copying the selection does not, because it
/// asks the page, and a page stopped behind a dialog answers nothing until the
/// deadline — except on a `prompt()`, where it copies the line being typed,
/// which [`handle_input`] does before it gets here. Find does not survive
/// either, for the same reason: it asks the page.
///
/// A zoom waits too. The page is stopped, so it would not lay itself out
/// again or be photographed at the new level until it was answered, and the
/// level then arrives with whatever else was queued — which is the reload
/// case again.
///
/// The same keys survive a file input's prompt, and the same wait, although
/// that page is not stopped: what waits is still what would do something to
/// the page the path is being typed for, and the copy is still of the line.
/// A zoom there would lay the page out again under an input the person is
/// answering for, and it is one key to finish the path first.
fn survives_dialog(command: Command) -> bool {
    match command {
        Command::Quit
        | Command::NewTab
        | Command::CloseTab
        | Command::NextTab
        | Command::PreviousTab
        | Command::SelectTab(_)
        | Command::LastTab
        | Command::ListTabs
        | Command::MoveTab(_)
        | Command::CopyUrl => true,
        Command::EditUrl
        | Command::Reload
        | Command::Back
        | Command::Forward
        | Command::CopySelection
        | Command::Find
        | Command::ZoomIn
        | Command::ZoomOut
        | Command::ZoomReset => false,
    }
}

/// A key, as the answer to the dialog on the page in front.
///
/// The dialog is taken off the tab as soon as it is answered rather than when
/// the engine says it has closed. The row is the person's, and a question they
/// have just answered should not still be on it for the round trip; the
/// `Page.javascriptDialogClosed` that follows finds nothing to clear, and is
/// what asks the page its title again. Nothing here waits for the engine,
/// either: `Page.handleJavaScriptDialog` is a notification, and one that
/// crossed a dialog the engine closed itself — "No dialog is showing" — has
/// nothing to say that anyone needs to hear.
fn answer_dialog(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    chrome: &mut Chrome,
    key: KeyInput,
) -> Result<bool, String> {
    let Some(tab) = tabs.active_mut() else {
        return Ok(true);
    };
    let Some(dialog) = tab.dialog.as_mut() else {
        return Ok(true);
    };
    match dialog.step(&key) {
        Answer::Waiting => {}
        Answer::Quit => return Ok(false),
        answer => {
            let params = dialog.reply(answer);
            let stay = dialog.kind == Kind::BeforeUnload && answer == Answer::Dismiss;
            tab.dialog = None;
            let _ = tab.connection.notify("Page.handleJavaScriptDialog", params);
            if stay {
                stayed(tab);
            }
        }
    }
    redraw_row(pane, tabs, chrome)?;
    Ok(true)
}

/// A key, as typing into the path for the file input on the page in front.
///
/// Sent, the prompt comes off the tab at once and the files go as
/// `DOM.setFileInputFiles` — a notification, because the reply is `{}` and the
/// engine reading the file is not this loop's business. The row says what
/// went, on the tab's note, until the page says something else: there is no
/// event for the page having read it, so there is no progress to show. The
/// directory it came from is where the next prompt starts.
///
/// Escaped, nothing is sent, and the page is told `cancel` the way a real
/// chooser would tell it: [`cancel_chooser`].
fn answer_upload(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    chrome: &mut Chrome,
    key: KeyInput,
) -> Result<bool, String> {
    let Some(tab) = tabs.active_mut() else {
        return Ok(true);
    };
    let Some(prompt) = tab.upload.as_mut() else {
        return Ok(true);
    };
    match prompt.step(&key, &upload::Disk) {
        upload::Outcome::Waiting => {}
        upload::Outcome::Quit => return Ok(false),
        upload::Outcome::Send => {
            let params = prompt.reply();
            tab.note = Some(prompt.sentence());
            chrome.upload_dir = prompt.last_dir();
            tab.upload = None;
            let _ = tab.connection.notify("DOM.setFileInputFiles", params);
        }
        upload::Outcome::Cancel => {
            let node = prompt.chooser.backend_node_id;
            tab.upload = None;
            cancel_chooser(&mut tab.connection, node);
        }
    }
    redraw_row(pane, tabs, chrome)?;
    Ok(true)
}

/// Tell a page its file input was dismissed: `cancel`, dispatched on the
/// input itself, which is what Chromium's own chooser fires on a dismiss
/// since 113.
///
/// There is no cancel in the protocol — `DOM.setFileInputFiles` with no
/// files is answered and does nothing at all, measured — so the input is
/// found by the node the event named (`DOM.resolveNode`, which answers at once
/// on a live page, and needs no `DOM.enable`), the event is fired on it
/// ([`Upload::CANCEL_FUNCTION`]), and the handle is let go. The first is a
/// call, briefly, because the next two need what it answers; they are
/// notifications. A page that does not answer in time does not hear its
/// `cancel`, which is the one thing here this program can afford to lose.
///
/// Public so that the engine tests send what this program sends.
pub fn cancel_chooser(client: &mut Client, backend_node_id: i64) {
    let Ok(node) = client.call_within(
        "DOM.resolveNode",
        Json::object(vec![(
            "backendNodeId",
            Json::number(backend_node_id as f64),
        )]),
        SWITCH_TIMEOUT,
    ) else {
        return;
    };
    let Some(object) = node
        .path(&["object", "objectId"])
        .and_then(Json::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let _ = client.notify(
        "Runtime.callFunctionOn",
        Json::object(vec![
            ("objectId", Json::string(&object)),
            ("functionDeclaration", Json::string(Upload::CANCEL_FUNCTION)),
        ]),
    );
    let _ = client.notify(
        "Runtime.releaseObject",
        Json::object(vec![("objectId", Json::string(object))]),
    );
}

/// A page that asked before it was left and was told no.
///
/// It is where it was, and nothing about the tab should say otherwise: not
/// the "loading" note that went up when the url was typed, not that url in
/// place of the one the page still has. So the note goes, the tab is not
/// loading, and the url is asked of the engine's own history — which answers
/// at once, dialog or none, because it is the browser's rather than the
/// page's.
///
/// A url that turned out to be a download is the other way a page ends up not
/// having gone anywhere, and [`navigated`] sends it here too. And a load
/// stopped with `esc` is the third: whatever is on the page — the last page,
/// or as much of the new one as came — is where the tab is, and the history
/// says which, because `Page.getNavigationHistory` is answered during a hang
/// that holds everything asked of the page itself (measured).
fn stayed(tab: &mut Tab<Client>) {
    tab.note = None;
    tab.loading = false;
    tab.since = None;
    // Whatever is on the page has committed, or it would not be there.
    tab.committed = true;
    let Ok(history) =
        tab.connection
            .call_within("Page.getNavigationHistory", Json::empty(), SWITCH_TIMEOUT)
    else {
        return;
    };
    if let Some(url) = load::current_url(&history) {
        tab.url = url;
    }
}

/// Who an Escape press is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escapes {
    /// The url bar is open: it closes (`Edit::Cancel`).
    UrlBar,
    /// The find prompt is open: it closes, and the highlights go with it.
    Find,
    /// The tab list is open: it closes, and the page comes back.
    List,
    /// The page in front has a dialog up: it is dismissed.
    Dialog,
    /// The page in front is asking for a file input's path: nothing is sent,
    /// and the input is told `cancel` ([`cancel_chooser`]).
    Upload,
    /// The page in front is loading: the load stops ([`stop_loading`]).
    StopLoading,
    /// Nothing of this program's wants it, so it is the page's key.
    Page,
}

/// Who an Escape press is for: whatever has the row, by [`row_owner`]'s
/// order; then a load; then the page.
///
/// Whatever has the row first, because Escape is how a line is abandoned and
/// how a question is said no to, and the row is where the person can see
/// which of them it will be. Its order is [`row_owner`]'s and not one of its
/// own — the url bar, the find prompt, the tab list, a dialog, a file
/// input's path — so
/// that what Escape closes is always the thing on the screen, never one
/// underneath it. That is why a dialog comes before a path here, which the
/// design for this key had the other way round: an `alert()` opened while a
/// path is half typed has the row, the page is stopped behind it, and an
/// Escape that cancelled the path instead would close something the person
/// cannot see and leave the question they can.
///
/// Then a load, because a page that is loading and hung is the one thing on
/// the screen the person cannot otherwise get out of, and Escape dispatched to
/// the page does not stop a load (measured: the navigation stayed held). Only
/// then the page, which is what Escape was before any of this. A page that is
/// loading *and* has a modal of its own open gets the stop first; that is a
/// desktop browser's order too. And a load under something that has the row
/// goes on: the first Escape closes the find prompt or answers the question,
/// the second stops the load, which is one press per thing the person can see.
///
/// The keys reach the first four by their own routes before this is asked —
/// [`handle_input`] gives a line being typed every key, and a dialog's or a
/// path's key to [`answer_dialog`] or [`answer_upload`] — and each of those
/// treats Escape as this says. What is asked here, with the key nobody took,
/// is whether it stops a load; the whole order is kept in one place all the
/// same, so that the truth table in the tests is the order, not a guess at it.
fn escapes(owner: Option<&RowOwner<'_>>, loading: bool) -> Escapes {
    match owner {
        Some(RowOwner::Bar(_)) => Escapes::UrlBar,
        Some(RowOwner::Find(_)) => Escapes::Find,
        Some(RowOwner::List(_)) => Escapes::List,
        Some(RowOwner::Dialog(_)) => Escapes::Dialog,
        Some(RowOwner::Upload(_)) => Escapes::Upload,
        None if loading => Escapes::StopLoading,
        None => Escapes::Page,
    }
}

/// Stop the load on the page in front, and put the row back where the page
/// is.
fn stop_loading(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    chrome: &mut Chrome,
) -> Result<(), String> {
    let Some(tab) = tabs.active_mut() else {
        return Ok(());
    };
    // Its reply, `net::ERR_ABORTED`, is passed over by `load::failed` anyway;
    // dropped, it is not even waited for.
    chrome.navigation = None;
    stop(tab);
    forget_hover(pane, chrome)?;
    redraw_row(pane, tabs, chrome)
}

/// What `esc` does to a loading tab: the engine told to stop, and the tab put
/// back to say what the page now is.
///
/// `Page.stopLoading` is sent as a notification. It is answered by the
/// browser in under a millisecond whatever the renderer is doing, and the
/// reply says nothing; what says the load is over is the
/// `Page.frameStoppedLoading` that follows within a millisecond or five, which
/// `handle_page_events` reads like any other. What that event cannot do is
/// put the url back: after a stop before anything committed the tab still
/// carries the url that was typed and the note that says it is loading, and
/// that is the state `stayed` exists for — a page asked to leave and told
/// no. So it is called here too. And the title is asked as a load event would
/// ask it, because after a stop past the commit — headers and half a body,
/// or a page waiting on an image — the document is there with its title, and
/// no load event is ever coming for it (measured against
/// `chrome-headless-shell` 153, all three cases).
///
/// Public so that the engine tests can do exactly this to a tab and look.
pub fn stop(tab: &mut Tab<Client>) {
    let _ = tab.connection.notify("Page.stopLoading", Json::empty());
    stayed(tab);
    tab.stopped_loading();
    if let Some(loaded) = page_loaded(&mut tab.connection) {
        tab.loaded(loaded);
    }
}

/// Which keys this program answers, and which it hands to the page.
///
/// The tab keys are the ones a browser has taught everybody — `ctrl+t`,
/// `ctrl+w`, `ctrl+tab`, `alt+1`..`alt+9` — and every one of them was checked
/// against `compositor/tos-session/src/keys.rs` before being taken, because a
/// key the compositor binds is a key that never reaches a pane at all. What
/// the compositor has near these: `ctrl+shift+t` opens a workspace,
/// `ctrl+shift+w` closes a pane, `ctrl+alt+shift+t` renames a workspace,
/// `ctrl+shift+1`..`9` and `super+1`..`9` select workspaces, `ctrl+a` is the
/// leader and `ctrl+space` opens the input method. None of those is one of
/// these: the compositor's tab-ish keys all carry shift or super, and this
/// program's all carry neither. `alt` it uses for nothing at all, which is why
/// the digits are there rather than on ctrl, where `ctrl+1` would be a key a
/// page can legitimately be sent.
///
/// `ctrl+tab` reaches the pane as `CSI 9;5u` and `ctrl+shift+tab` as
/// `CSI 9;6u`, because this program asks for the Kitty keyboard protocol's
/// disambiguate flag (`screen::KEYBOARD_FLAGS`) and `tos_input::encode` sends
/// a modified tab in the protocol's form rather than as a bare `\t`. In a
/// terminal that does not speak it, ctrl+tab arrives as a plain tab and goes
/// to the page — which is the right failure, since the page is where tab
/// usually belongs.
///
/// Copying is `alt+c` and `alt+u`, not `ctrl+shift+c`, because a key the
/// terminal never sends is not a key this program can bind. Every terminal it
/// runs in takes `ctrl+shift+c` and `ctrl+shift+v` for its own copy and paste
/// before a pane sees a byte: Kitty, WezTerm and Ghostty on Linux, and tOS
/// since #153 (`compositor/tos-session/src/keys.rs`, where "a program in a
/// pane can no longer be sent ctrl+shift+c or ctrl+shift+v by any means").
/// That is also how a paste gets in: the terminal's own paste key sends it,
/// bracketed, and it goes wherever the typing is — see [`paste`]. `ctrl+c`
/// and `ctrl+v` stay the page's, as they were: the engine copies and pastes
/// within itself with them, which is how a page expects them to work, and a
/// page's editor would lose them otherwise. `ctrl+y` is an editor's redo and
/// `ctrl+insert` is WezTerm's copy. `alt` is the modifier this program
/// already uses for its own movement and the compositor uses for nothing, and
/// `alt+letter` is left to the program by Kitty, WezTerm and Ghostty alike.
/// What it shadows on a page is an `accesskey` on `c` or `u`, on the same
/// terms `alt+1`..`alt+9` already shadow the digits.
///
/// Find is `ctrl+f` because that is the reflex, and the compositor binds
/// nothing on it. What it shadows is a page's own `ctrl+f` handler, which in
/// a headless engine opened nothing anyway. Inside a line being typed it is
/// readline's forward-a-character, because the line is asked first.
///
/// Zoom is on both. `ctrl+=`, `ctrl+-` and `ctrl+0` are the browser's reflex
/// and are bound, but only some terminals let them through: Kitty passes them
/// (its own font size is `ctrl+shift+=`) and so does tOS, whose compositor
/// binds `=` only under its `ctrl+a` leader; WezTerm and Ghostty keep them
/// for their own font size by default, and a legacy terminal sends `ctrl+=`
/// as a bare `=`. `alt+=`, `alt+-` and `alt+0` are the program's own, on the
/// modifier it already uses for back, forward, tabs and copying, and pass
/// through every one of them and over any ssh. `+` and `_` are the same keys
/// shifted, and `ctrl+_` is what a legacy terminal sends for `ctrl+-`
/// (`0x1f`). `ctrl+0` is the one ctrl-digit taken, because no page has ever
/// been sent it by a browser; `ctrl+1`..`ctrl+9` stay the page's. What the
/// alt forms shadow is an `accesskey` on `=`, `-` or `0`.
///
/// The tab keys past the digits argue the same way, and more strongly,
/// because delivery is not the question — every chord below arrives
/// distinctly under the flags this program pushes — but whether the terminal
/// keeps the key for itself, and each default table was read:
///
/// | chord | tOS | Kitty | WezTerm | Ghostty (Linux) |
/// | --- | --- | --- | --- | --- |
/// | `ctrl+shift+pageup`/`pagedown` | scrolls a page | scrolls a page | moves its tab | moves its tab |
/// | `ctrl+shift+a` | free | a chord prefix (opacity) | free | select all |
/// | `alt+9` | free | free | free | its last tab |
/// | `alt+shift+pageup`/`pagedown` | free | free | free | free |
/// | `alt+a` | free | free | free | free |
///
/// So `ctrl+shift+pageup`/`pagedown`, which move the tab in front in Chrome,
/// reach the pane in none of the four, and `ctrl+shift+a`, Chrome's tab
/// search, in two. They are bound all the same, for a person who has rebound
/// their terminal, and each has an `alt` form that reaches the program
/// everywhere: `alt+a` for the list, `alt+shift+pageup`/`pagedown` for the
/// move. `ctrl+a` without shift stays the page's select-all, and `alt+shift+a`
/// is nobody's. `alt+9` is the last tab rather than the ninth, as it is in
/// Chrome, Firefox and Ghostty — whose own `alt+9` means the same, so the
/// meaning agrees even where the key does not arrive.
fn command(key: &KeyInput) -> Option<Command> {
    if key.action == KeyAction::Release {
        return None;
    }
    if key.mods.ctrl() && !key.mods.alt() {
        return match key.key {
            Key::Char('q') => Some(Command::Quit),
            Key::Char('l') => Some(Command::EditUrl),
            Key::Char('r') => Some(Command::Reload),
            Key::Char('t') => Some(Command::NewTab),
            Key::Char('w') => Some(Command::CloseTab),
            Key::Char('f') => Some(Command::Find),
            Key::Char('a' | 'A') if key.mods.shift() => Some(Command::ListTabs),
            Key::PageUp if key.mods.shift() => Some(Command::MoveTab(-1)),
            Key::PageDown if key.mods.shift() => Some(Command::MoveTab(1)),
            Key::Tab if key.mods.shift() => Some(Command::PreviousTab),
            Key::Tab => Some(Command::NextTab),
            Key::Char('=' | '+') => Some(Command::ZoomIn),
            Key::Char('-' | '_') => Some(Command::ZoomOut),
            Key::Char('0') => Some(Command::ZoomReset),
            _ => None,
        };
    }
    if key.mods.alt() && !key.mods.ctrl() {
        return match key.key {
            Key::Left => Some(Command::Back),
            Key::Right => Some(Command::Forward),
            Key::Char('9') => Some(Command::LastTab),
            Key::Char(digit @ '1'..='8') => Some(Command::SelectTab(digit as usize - '0' as usize)),
            Key::Char('a') if !key.mods.shift() => Some(Command::ListTabs),
            Key::PageUp if key.mods.shift() => Some(Command::MoveTab(-1)),
            Key::PageDown if key.mods.shift() => Some(Command::MoveTab(1)),
            Key::Char('c') => Some(Command::CopySelection),
            Key::Char('u') => Some(Command::CopyUrl),
            Key::Char('=' | '+') => Some(Command::ZoomIn),
            Key::Char('-' | '_') => Some(Command::ZoomOut),
            Key::Char('0') => Some(Command::ZoomReset),
            _ => None,
        };
    }
    None
}

/// A new level for the tab in front: told to the engine, remembered for the
/// host, and the row redrawn.
///
/// No screencast restart and no screen clear: a running cast follows the new
/// size by itself (measured, 45 frames at the new size in the next 1.5 s and
/// none at the old), and the picture that is up stays up until the first of
/// them replaces it. The motion clock is reset, so that the page laid out
/// again gets its lossless still, and a still already asked for is dropped,
/// because it is of the page at the old level. A scroll in flight carries on
/// at a point brought back inside the new CSS size, as after a resize, and
/// what is under the pointer is asked again, as after a scroll.
///
/// A page with no host is zoomed all the same, on its tab alone; see
/// [`crate::zoom::host_key`]. A level that cannot be written to the profile
/// is still the level the page is at.
fn rezoom(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    chrome: &mut Chrome,
    level: Zoom,
) -> Result<(), String> {
    let Some(tab) = tabs.active_mut() else {
        return Ok(());
    };
    if tab.zoom == level {
        return Ok(());
    }
    tab.zoom = level;
    if let Some(host) = zoom::host_key(&tab.url) {
        let _ = chrome.zooms.set(&host, level);
    }
    let viewport = viewport(chrome, tab);
    let stopped = tab.dialog.is_some();
    if let Err(why) = emulate(&mut tab.connection, viewport, stopped) {
        tab.note = Some(why);
    }
    chrome.motion.reset(Instant::now());
    chrome.still = None;
    chrome
        .wheel
        .resized((viewport.css.0 as i32, viewport.css.1 as i32));
    // The page is laid out again under a pointer that did not move, as a
    // wheel moves it: an ask that is out is about the old layout and is
    // dropped, and the same place is asked about once more. The link on the
    // row stays until the answer says otherwise, as it does after a scroll.
    chrome.asking = None;
    chrome.hover.scrolled();
    redraw_row(pane, tabs, chrome)
}

/// Type into the url bar. Returns `false` only if the person quit.
fn edit_url(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    chrome: &mut Chrome,
    key: KeyInput,
) -> Result<bool, String> {
    if key.action == KeyAction::Release {
        return Ok(true);
    }
    let typed = chrome
        .bar
        .as_ref()
        .map(|bar| bar.line.text().to_string())
        .unwrap_or_default();
    if copy_from_row(pane, tabs, &key, &typed)? {
        return Ok(true);
    }
    let Some(bar) = chrome.bar.as_mut() else {
        return Ok(true);
    };
    match bar_step(bar, &chrome.history, &key) {
        Edit::Typing | Edit::Inserted | Edit::Previous | Edit::Next => {}
        Edit::Quit => return Ok(false),
        Edit::Cancel => chrome.bar = None,
        Edit::Go => {
            let url = destination(bar.line.text(), chrome.search_url.as_deref());
            chrome.bar = None;
            if let Some(tab) = tabs.active_mut() {
                tab.url = url.clone();
                tab.note = Some(format!("loading {url}"));
                tab.loading = true;
            }
            if let Some(why) = navigate(tabs, chrome, &url) {
                if let Some(tab) = tabs.active_mut() {
                    tab.note = Some(why);
                }
            }
        }
    }
    redraw_row(pane, tabs, chrome)?;
    Ok(true)
}

/// The copy keys, while a line is being typed on the row: `true` if `key`
/// was one of them and has been done.
///
/// Before the editor sees the key: they leave the line as it is, the
/// selection included. What is on the row while the line is open is the
/// line, so that is what `alt+c` copies, as `typed`; `alt+u` is still the
/// page's url.
fn copy_from_row(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    key: &KeyInput,
    typed: &str,
) -> Result<bool, String> {
    match command(key) {
        Some(Command::CopySelection) => {
            copy_out(pane, tabs, typed, Copied::Text)?;
            Ok(true)
        }
        Some(Command::CopyUrl) => {
            let url = tabs.active().map(|tab| tab.url.clone()).unwrap_or_default();
            copy_out(pane, tabs, &url, Copied::Url)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Open the find prompt on the tab in front, offering the last needle.
///
/// The script's world is made now, in two calls ([`make_world`]), because
/// the person has just pressed a key and the first letter they type is about
/// to need it. A page that will not answer them in [`WORLD_TIMEOUT`] is a page
/// that would not answer a search either: the prompt still opens, and the
/// first search says so. With a needle remembered, the search for it goes at
/// once, so that its highlights come straight back.
fn open_find(tabs: &mut Tabs<Client>, chrome: &mut Chrome) {
    let Some(tab) = tabs.active_mut() else {
        return;
    };
    let finder = find::Finder::open(&chrome.last_needle);
    let wanted = finder.initial();
    chrome.find = Some(Find {
        target: tab.target.clone(),
        context: make_world(&mut tab.connection),
        finder,
        pending: None,
        wanted,
        remade: false,
    });
    pump_find(tabs, chrome);
}

/// The find script's world in the page's main frame: `Page.getFrameTree` for
/// the frame, then `Page.createIsolatedWorld`, which answers with the same
/// world for as long as the document lasts. `None` for a page that did not
/// answer.
fn make_world(client: &mut Client) -> Option<i64> {
    let tree = client
        .call_within("Page.getFrameTree", Json::empty(), WORLD_TIMEOUT)
        .ok()?;
    let frame = find::main_frame(&tree)?;
    let world = client
        .call_within(
            "Page.createIsolatedWorld",
            find::world_params(&frame),
            WORLD_TIMEOUT,
        )
        .ok()?;
    find::context(&world)
}

/// Type into the find prompt. Returns `false` only if the person quit.
///
/// Every key comes here first while the prompt is open, as it does to the url
/// bar: the tab keys, `alt+←` and the rest do nothing until Escape, and
/// `ctrl+f` moves the cursor. What the key asks of the page is folded into
/// what is waiting and sent by [`pump_find`] when nothing is out.
fn edit_find(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    chrome: &mut Chrome,
    key: KeyInput,
) -> Result<bool, String> {
    if key.action == KeyAction::Release {
        return Ok(true);
    }
    let typed = chrome
        .find
        .as_ref()
        .map(|find| find.finder.line.text().to_string())
        .unwrap_or_default();
    if copy_from_row(pane, tabs, &key, &typed)? {
        return Ok(true);
    }
    let Some(find) = chrome.find.as_mut() else {
        return Ok(true);
    };
    match find.finder.step(&key) {
        find::Step::Typing(None) => {}
        find::Step::Typing(Some(ask)) => find.want(ask),
        find::Step::Close => close_find(tabs, chrome),
        find::Step::Quit => return Ok(false),
    }
    pump_find(tabs, chrome);
    redraw_row(pane, tabs, chrome)?;
    Ok(true)
}

/// Collect the answer to a search if it has come, and send the next one if
/// one is wanted and none is out. Never waits: a search is the still's shape,
/// not a `call` (see [`crate::find`]). Returns whether the row should be drawn
/// again.
///
/// One search out at a time, so that the answers arrive in the order the keys
/// were typed and the count on the row is always for a needle that was on
/// it. A world that went with its document — a navigation this program did
/// not hear, because its event was binned with a tab's frames — is made again
/// once and the search asked again; a second time is reported. Whatever goes
/// wrong goes on the tab's note, which the row shows once the prompt closes.
fn pump_find(tabs: &mut Tabs<Client>, chrome: &mut Chrome) -> bool {
    let Some(find) = chrome.find.as_mut() else {
        return false;
    };
    // A tab that is no longer in front is [`switched`]'s to close.
    if tabs.active_target() != Some(find.target.as_str()) {
        return false;
    }
    let Some(tab) = tabs.active_mut() else {
        return false;
    };
    let mut changed = false;
    if let Some((pending, ask, sent)) = find.pending.take() {
        match tab.connection.take_reply(&pending) {
            // A page stopped behind a dialog answers when it has been
            // answered; the wait for the person is not the page's.
            None if tab.dialog.is_some() => find.pending = Some((pending, ask, Instant::now())),
            None if sent.elapsed() < FIND_TIMEOUT => find.pending = Some((pending, ask, sent)),
            None => {
                tab.note = Some("the page did not answer the search".to_string());
                changed = true;
            }
            Some(Ok(reply)) => {
                find.finder.matches = find::matches(&reply);
                find.remade = false;
                changed = true;
            }
            Some(Err(why)) if find::stale_world(&why) && !find.remade => {
                find.context = make_world(&mut tab.connection);
                find.remade = true;
                find.wanted = Some(match find.wanted.take() {
                    Some(newer) => ask.merge(newer),
                    None => ask,
                });
            }
            Some(Err(why)) => {
                tab.note = Some(why);
                changed = true;
            }
        }
    }
    if find.pending.is_none() {
        if let Some(ask) = find.wanted.take() {
            let sent = match find.context {
                Some(context) => tab
                    .connection
                    .send("Runtime.callFunctionOn", find::call_params(context, &ask)),
                None => Err("the page did not answer the search".to_string()),
            };
            match sent {
                Ok(pending) => find.pending = Some((pending, ask, Instant::now())),
                Err(why) => {
                    tab.note = Some(why);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Close the find prompt, from wherever it is closed: Escape, a tab switch,
/// a navigation of the page it was searching. The needle is remembered for
/// the next `ctrl+f`.
///
/// The page is told to clear as a notification: there is nothing to collect,
/// and it is queued behind any search still out, so the page ends clear
/// whichever of the two it reads first. Dropping the search still out
/// withdraws the claim on its answer. Quitting does not come here: the
/// engine is being stopped, and there is no page to leave clear.
fn close_find(tabs: &mut Tabs<Client>, chrome: &mut Chrome) {
    let Some(find) = chrome.find.take() else {
        return;
    };
    chrome.last_needle = find.finder.line.text().to_string();
    let (Some(context), Some(index)) = (find.context, tabs.index_of(&find.target)) else {
        return;
    };
    if let Some(tab) = tabs.get_mut(index) {
        let _ = tab
            .connection
            .notify("Runtime.callFunctionOn", find::clear_params(context));
    }
}

/// Clear the page's rows for the tab list: the picture goes, and the rows
/// are the list's until it closes.
///
/// A Kitty-protocol placement with no `z` is drawn above text, so text
/// written under the page's picture would not be seen in Kitty, WezTerm or
/// Ghostty. What a tab switch does is the right thing here too: delete the
/// placement, clear the rows, and let the next frame or still put the page
/// back — which [`close_list`] arranges. A negative `z` was the alternative,
/// and would be a change to every frame for an overlay that is up for a
/// second.
fn open_list_screen(pane: &mut Pane, chrome: &mut Chrome) -> Result<(), String> {
    // A still in flight is of a picture that is not going to be drawn.
    chrome.still = None;
    pane.write(&crate::graphics::clear_command())
        .map_err(|e| e.to_string())?;
    pane.write(b"\x1b[2;1H\x1b[J").map_err(|e| e.to_string())
}

/// Close the tab list and give the page its rows back. The caller redraws
/// the row.
///
/// The same three things a resize does: the rows are cleared, and the
/// motion clock is reset with no still out, so that a page at rest gets its
/// still within [`crate::motion`]'s rest interval and a page in motion is
/// repainted by its next frame.
fn close_list(pane: &mut Pane, chrome: &mut Chrome) -> Result<(), String> {
    if chrome.list.take().is_none() {
        return Ok(());
    }
    chrome.list_first.set(0);
    chrome.motion.reset(Instant::now());
    chrome.still = None;
    pane.write(b"\x1b[2;1H\x1b[J").map_err(|e| e.to_string())
}

/// Type into the tab list. Returns `false` only if the person quit.
///
/// Every key comes here first while the list is open, as it does to the url
/// bar and the find prompt: the program's other keys do nothing until it
/// closes. `alt+c` copies the filter and `alt+u` the url, as on the other
/// lines.
fn edit_list(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    chrome: &mut Chrome,
    key: KeyInput,
) -> Result<bool, String> {
    if key.action == KeyAction::Release {
        return Ok(true);
    }
    let typed = chrome
        .list
        .as_ref()
        .map(|list| list.line.text().to_string())
        .unwrap_or_default();
    if copy_from_row(pane, tabs, &key, &typed)? {
        return Ok(true);
    }
    let page = chrome.metrics.usable_rows() as usize;
    let Some(list) = chrome.list.as_mut() else {
        return Ok(true);
    };
    let matched: Vec<usize> = list.matches(tabs).iter().map(|entry| entry.index).collect();
    let step = list.step(&key, &matched, page);
    list_step(pane, tabs, browser, chrome, step)
}

/// A press while the tab list is open: a row picks the tab on it, and
/// anything else — the row itself, past the last match — is nothing.
fn click_list(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    chrome: &mut Chrome,
    report: &MouseInput,
) -> Result<bool, String> {
    let cell = chrome.metrics.cell;
    let point = crate::input::page_point(report, chrome.parser.pixel_coordinates(), cell, 1);
    if point.1 < 0 {
        return Ok(true);
    }
    let row = point.1 as usize / cell.1.max(1) as usize;
    let rows = chrome.metrics.usable_rows() as usize;
    let first = chrome.list_first.get();
    let Some(list) = chrome.list.as_mut() else {
        return Ok(true);
    };
    let matched: Vec<usize> = list.matches(tabs).iter().map(|entry| entry.index).collect();
    let window = list.window(matched.len(), rows, first);
    let step = list.click(row, &window, &matched);
    list_step(pane, tabs, browser, chrome, step)
}

/// What a key or a click did to the tab list, done.
fn list_step(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    chrome: &mut Chrome,
    step: tablist::Step,
) -> Result<bool, String> {
    match step {
        tablist::Step::Typing => {}
        tablist::Step::Quit => return Ok(false),
        tablist::Step::Close | tablist::Step::Pick(None) => close_list(pane, chrome)?,
        tablist::Step::Pick(Some(index)) => {
            close_list(pane, chrome)?;
            let was = tabs.active_target().map(str::to_string);
            if tabs.switch_to(index) {
                switched(pane, tabs, browser, chrome, was)?;
            }
        }
    }
    redraw_row(pane, tabs, chrome)?;
    Ok(true)
}

/// Whether a mouse report is the tab list's while it is open: a press, which
/// picks a row. Motion and the wheel are nobody's then, since the page is
/// not on the screen to be pointed at.
fn routes_to_list(report: &MouseInput, list_open: bool) -> bool {
    list_open && report.kind == MouseKind::Press
}

/// A paste, put wherever the typing is.
///
/// The url bar if it is open, because the person pressed `ctrl+l` and that
/// is where they are typing; then the find prompt; then a `prompt()`'s line
/// on the page in front; then the path for its file input; then the page —
/// [`typing_line`]'s order. A paste on an alert, a confirm or a "leave this
/// page?" is dropped: a paste is not "any key", and answering "delete these
/// files?" with the clipboard's contents would be worse than not pasting at
/// all.
///
/// Into the page it is one `Input.insertText` carrying the whole paste, and
/// never keystrokes. Measured against `chrome-headless-shell` 153, that is
/// the engine doing exactly what a paste should: a `<textarea>` keeps the
/// newlines and the tabs, an `<input>` turns each newline into a space, `\r\n`
/// and `\r` come out as `\n`, and nothing anywhere fires a `keydown`, a
/// `keypress` or a form's `submit` — where the Enter key into the same
/// `<input>` fires all three. It goes into the focused element, in whichever
/// frame has the focus, which is where the person's last click put it. A
/// notification rather than a call, because the most a paste within
/// [`crate::input::PASTE_LIMIT`] costs is about five seconds of renderer (the
/// table is in [`crate::input`]), and a loop sitting in a call for them would
/// be a loop not reading the terminal.
fn paste(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    chrome: &mut Chrome,
    text: &str,
) -> Result<(), String> {
    match typing_line(tabs, chrome) {
        Some(Typing::Url) => {
            if let Some(bar) = chrome.bar.as_mut() {
                if paste_into_line(&mut bar.line, text) {
                    // As a typed character does: the walk through history
                    // ends, and what was there to take is asked for again.
                    bar.walk = None;
                    bar.line.suggest(chrome.history.complete(bar.line.text()));
                }
            }
            return redraw_row(pane, tabs, chrome);
        }
        Some(Typing::Find) => {
            if let Some(find) = chrome.find.as_mut() {
                // As a typed character does: a search for what is there now.
                if paste_into_line(&mut find.finder.line, text) {
                    let needle = find.finder.line.text().to_string();
                    find.want(find::Ask { needle, step: 0 });
                }
            }
            pump_find(tabs, chrome);
            return redraw_row(pane, tabs, chrome);
        }
        Some(Typing::List) => {
            if let Some(list) = chrome.list.as_mut() {
                paste_into_line(&mut list.line, text);
            }
            return redraw_row(pane, tabs, chrome);
        }
        Some(Typing::Prompt | Typing::Upload) | None => {}
    }
    if asking(tabs) {
        let pasted = tabs
            .active_mut()
            .and_then(|tab| tab.dialog.as_mut())
            .is_some_and(|dialog| paste_into_dialog(dialog, text));
        if pasted {
            redraw_row(pane, tabs, chrome)?;
        }
        return Ok(());
    }
    // A file input's path, which is where a long path most often comes from:
    // the same order as [`row_owner`]'s, and the same one-line rule as the
    // lines above.
    if let Some(prompt) = tabs.active_mut().and_then(|tab| tab.upload.as_mut()) {
        prompt.paste(&clipboard::one_line(text), &upload::Disk);
        return redraw_row(pane, tabs, chrome);
    }
    if let Some(tab) = tabs.active_mut() {
        let _ = tab
            .connection
            .notify("Input.insertText", keys::insert_text(text));
    }
    Ok(())
}

/// A paste into a line being typed: the url bar's, the find prompt's, or a
/// `prompt()`'s.
///
/// What is kept of it is [`clipboard::one_line`], put in at the cursor by
/// [`Line::insert_str`] — which replaces a line still offered whole, as the
/// first key would: a url pasted over the address `ctrl+l` showed is the url,
/// not the two glued together. A paste that is nothing once it is one line
/// changes nothing, the selection included, and says so by returning `false`.
fn paste_into_line(line: &mut Line, text: &str) -> bool {
    let text = clipboard::one_line(text);
    !text.is_empty() && line.insert_str(&text)
}

/// A paste into a dialog: into its line if it is a `prompt()`, and `false`,
/// with nothing changed, for every other kind.
fn paste_into_dialog(dialog: &mut Dialog, text: &str) -> bool {
    if !dialog.typing() {
        return false;
    }
    paste_into_line(&mut dialog.line, text);
    true
}

/// What a copy was of, for the sentence that says it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Copied {
    Url,
    Text,
}

/// Put `text` on the host's clipboard, by way of the terminal, and say so on
/// the active tab's row.
///
/// The sentence says what was copied and how much, never the text itself.
/// The row is the one place a page's words could talk to the terminal, and a
/// copy has no reason to put a selection there. Over
/// [`clipboard::MAX_COPY`] nothing is written and the sentence says why. Nor
/// for nothing at all — an empty line, a page with no url yet — because an
/// empty OSC 52 is, to some terminals, an instruction to clear the clipboard,
/// and a copy of nothing is not a request for that.
fn copy_out(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    text: &str,
    what: Copied,
) -> Result<(), String> {
    if text.is_empty() {
        note(tabs, "nothing to copy");
        return Ok(());
    }
    let Some(bytes) = clipboard::osc52(text) else {
        note(
            tabs,
            format!("copy refused: over {} KiB", clipboard::MAX_COPY / 1024),
        );
        return Ok(());
    };
    pane.write(&bytes).map_err(|e| e.to_string())?;
    note(tabs, copied(text, what));
    Ok(())
}

/// The sentence for a copy that went out.
fn copied(text: &str, what: Copied) -> String {
    match what {
        Copied::Url => "copied url".to_string(),
        Copied::Text => match text.chars().count() {
            1 => "copied 1 character".to_string(),
            n => format!("copied {n} characters"),
        },
    }
}

/// A sentence on the active tab's row, in place of its title until the next
/// landing, as "nothing to go back to" is.
fn note(tabs: &mut Tabs<Client>, sentence: impl Into<String>) {
    if let Some(tab) = tabs.active_mut() {
        tab.note = Some(sentence.into());
    }
}

/// What one key does to the url bar, given the pages visited: the part of
/// [`edit_url`] that talks to nothing, so that it can be tested a key at a
/// time.
///
/// A suggestion is asked for when text went in and at no other time — a
/// suggestion made again after a backspace is one the backspace cannot
/// delete. Up and Down walk the pages that match what was typed when the
/// walk began; any edit that changes the text ends the walk, so that the
/// next Up walks what is there now.
fn bar_step(bar: &mut UrlBar, history: &History, key: &KeyInput) -> Edit {
    let before = bar.line.text().len();
    let edit = bar.line.step(key);
    match edit {
        Edit::Inserted => {
            bar.walk = None;
            bar.line.suggest(history.complete(bar.line.text()));
        }
        Edit::Previous | Edit::Next => {
            let walk = bar
                .walk
                .get_or_insert_with(|| history::Walk::new(history, bar.line.text()));
            let moved = if edit == Edit::Previous {
                walk.up()
            } else {
                walk.down()
            };
            if let Some(url) = moved {
                bar.line.set_text(url);
            }
        }
        Edit::Typing if bar.line.text().len() != before => bar.walk = None,
        _ => {}
    }
    edit
}

/// Walk the active tab's history by one entry.
///
/// The list and the index come from the engine every time rather than being
/// tracked here: a page that pushed state, a redirect, or a link opened in the
/// same tab all change the history without this program being told, and a
/// cached index would send the person somewhere they have never been.
fn go(tabs: &mut Tabs<Client>, delta: i64) {
    let Some(tab) = tabs.active_mut() else {
        return;
    };
    let Ok(history) = tab
        .connection
        .call("Page.getNavigationHistory", Json::empty())
    else {
        return;
    };
    let index = history
        .get("currentIndex")
        .and_then(Json::as_i64)
        .unwrap_or(0);
    let entries = history
        .get("entries")
        .and_then(Json::as_array)
        .unwrap_or(&[]);
    let wanted = index + delta;
    if wanted < 0 || wanted as usize >= entries.len() {
        tab.note = Some(match delta {
            d if d < 0 => "nothing to go back to".to_string(),
            _ => "nothing to go forward to".to_string(),
        });
        return;
    }
    let entry = entries[wanted as usize].get("id").and_then(Json::as_i64);
    if let Some(id) = entry {
        let _ = tab.connection.call(
            "Page.navigateToHistoryEntry",
            Json::object(vec![("entryId", Json::number(id as f64))]),
        );
    }
}

fn send_key(client: &mut Client, key: &KeyInput) {
    match keys::dispatch(key) {
        Some(params) => {
            let _ = client.notify("Input.dispatchKeyEvent", params);
        }
        // A character with no key of its own — an input method's output, an
        // emoji — is text that was produced rather than a key that was
        // pressed, and that is exactly what insertText is for.
        None => {
            if key.action != KeyAction::Release {
                if let Some(c) = key.text {
                    let _ = client.notify("Input.insertText", keys::insert_text(&c.to_string()));
                }
            }
        }
    }
}

/// Presses close enough together in time and place to be one gesture.
#[derive(Default)]
struct Clicks {
    at: Option<(Instant, i32, i32, u32)>,
    count: u32,
}

impl Clicks {
    fn press(&mut self, button: u32, x: i32, y: i32) -> u32 {
        let now = Instant::now();
        let same = match self.at {
            Some((when, px, py, pb)) => {
                pb == button
                    && now.duration_since(when) < DOUBLE_CLICK
                    && (px - x).abs() <= DOUBLE_CLICK_SLOP
                    && (py - y).abs() <= DOUBLE_CLICK_SLOP
            }
            None => false,
        };
        self.count = if same { self.count + 1 } else { 1 };
        self.at = Some((now, x, y, button));
        self.count
    }
}

fn button_name(button: Option<u32>) -> &'static str {
    match button {
        Some(0) => "left",
        Some(1) => "middle",
        Some(2) => "right",
        _ => "none",
    }
}

/// The `buttons` mask CDP wants: left 1, right 2, middle 4.
fn button_bit(button: Option<u32>) -> u32 {
    match button {
        Some(0) => 1,
        Some(1) => 4,
        Some(2) => 2,
        _ => 0,
    }
}

/// One mouse report as the page's `Input.dispatchMouseEvent`.
///
/// Measured in the terminal's pixels until it goes on the wire — whether it
/// is on the status row, and whether two presses are close enough to be a
/// double click, are about the screen — and then in the page's, through
/// [`Viewport::css_point`]. `cell` is the terminal's, which turns a report
/// into a point, and `viewport` is the page as the engine was told it.
fn send_mouse(
    client: &mut Client,
    pixel_coordinates: bool,
    clicks: &mut Clicks,
    buttons: &mut u32,
    cell: (u32, u32),
    viewport: Viewport,
    report: MouseInput,
) {
    let (x, y) = crate::input::page_point(&report, pixel_coordinates, cell, 1);
    if y < 0 {
        // The status row is this program's, and a click on it is not the
        // page's business. A release is still forwarded, so that a drag that
        // ended up there does not leave the page with a button held down.
        if report.kind != MouseKind::Release {
            return;
        }
    }
    let y = y.max(0);

    let (kind, extra) = match report.kind {
        MouseKind::Press => {
            *buttons |= button_bit(report.button);
            let count = clicks.press(report.button.unwrap_or(0), x, y);
            ("mousePressed", vec![("clickCount", Json::number(count))])
        }
        MouseKind::Release => {
            *buttons &= !button_bit(report.button);
            ("mouseReleased", vec![("clickCount", Json::number(1))])
        }
        MouseKind::Move => ("mouseMoved", Vec::new()),
        // A wheel notch never reaches here. One dispatched `mouseWheel` of
        // 120 pixels moves the page in a single frame, so a notch is a curve
        // and about fourteen smaller events instead. See [`scroll`] and
        // [`crate::scroll`].
        MouseKind::Wheel => return,
    };

    let (x, y) = css_point(viewport, (x, y));
    let mut fields = vec![
        ("type", Json::string(kind)),
        ("x", Json::number(x)),
        ("y", Json::number(y)),
        ("modifiers", Json::number(report.mods.cdp())),
        ("button", Json::string(button_name(report.button))),
        ("buttons", Json::number(*buttons)),
    ];
    fields.extend(extra);
    let _ = client.notify("Input.dispatchMouseEvent", Json::object(fields));
}

/// What a person typed, turned into something `Page.navigate` will take.
///
/// A scheme is left alone. Anything else gets `https://`, because a bare
/// `example.com` is what people type and a `Page.navigate` without a scheme
/// fails with an error rather than guessing — except a host that can only be
/// this machine: `localhost`, anything under `.localhost`, `127.0.0.0/8` and
/// `[::1]`, with or without a port, get `http://`. Nothing on a loopback has a
/// certificate, and `https://localhost:3000` is a connection refused where
/// the development server a person typed that for is answering plain http.
///
/// What comes out is plain text first ([`crate::text::sanitize`]), because
/// it is going to be both shown on the row and sent to the engine, and what
/// went in may have been pasted from anywhere.
pub fn normalise(input: &str) -> String {
    let text = crate::text::sanitize(input);
    let text = text.trim();
    if text.is_empty() {
        return "about:blank".to_string();
    }
    if spelled_out(text) {
        return text.to_string();
    }
    if text.starts_with('/') {
        return format!("file://{text}");
    }
    if local_host(host_of(text).0) {
        return format!("http://{text}");
    }
    format!("https://{text}")
}

/// Where the url bar goes with what was typed: [`normalise`], unless a
/// search url was given and what was typed is words rather than a place.
///
/// Off unless `--search-url` names one. Sending what somebody typed to a
/// third party because it did not parse as a host is a decision about their
/// privacy — a mistyped intranet name, a half-pasted token — and it is theirs
/// to make, once, on the command line; without it, nothing typed is sent
/// anywhere but where it names. With it, the words replace the first `%s` in
/// the search url, percent-encoded. What counts as words is decided by `is_search`, below.
pub fn destination(input: &str, search_url: Option<&str>) -> String {
    let Some(search_url) = search_url else {
        return normalise(input);
    };
    let text = crate::text::sanitize(input);
    let text = text.trim();
    if text.is_empty() || spelled_out(text) || text.starts_with('/') || !is_search(text) {
        return normalise(input);
    }
    search_url.replacen("%s", &percent_encode(text), 1)
}

/// Whether `text` already says what it is: a scheme, or `about:` or `data:`.
fn spelled_out(text: &str) -> bool {
    text.contains("://") || text.starts_with("about:") || text.starts_with("data:")
}

/// Whether `text` — trimmed, not empty, no scheme and not a path — is
/// something to search for rather than somewhere to go.
///
/// A place, in order: nothing with a space in it is one, since a url has no
/// spaces. This machine is one ([`local_host`]), and so is anything in
/// brackets, which is an IPv6 address. A host made only of numbers is one if
/// it is an IPv4 address and words otherwise — `3.14` is a number somebody
/// wants explained. Otherwise a host of two or more labels, each of letters,
/// digits and hyphens (any script's letters, for an internationalised name),
/// the last with a letter in it, is a place: `example.com`, `例え.jp`,
/// `a.b/c?d`; and `rust`, `what?`, `foo.123`, `.com` and `a..b` are words. A
/// single label is a place only with a port, which nobody types by accident:
/// `myhost:8080`.
fn is_search(text: &str) -> bool {
    if text.chars().any(char::is_whitespace) {
        return true;
    }
    let (host, port) = host_of(text);
    if local_host(host) || host.starts_with('[') {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let number = |label: &&str| !label.is_empty() && label.bytes().all(|b| b.is_ascii_digit());
    if labels.iter().all(number) {
        let ipv4 = labels.len() == 4 && labels.iter().all(|label| label.parse::<u8>().is_ok());
        return !ipv4;
    }
    if labels.len() < 2 {
        return !port;
    }
    let label_ok =
        |label: &&str| !label.is_empty() && label.chars().all(|c| c.is_alphanumeric() || c == '-');
    let last_has_letter = labels
        .last()
        .is_some_and(|label| label.chars().any(char::is_alphabetic));
    !(labels.iter().all(label_ok) && last_has_letter)
}

/// The host of something typed without a scheme — what comes before the
/// first `/`, `?` or `#` — without its port, and whether it had one. An IPv6
/// address keeps its brackets, and the colons inside them are not a port.
fn host_of(text: &str) -> (&str, bool) {
    let end = text.find(['/', '?', '#']).unwrap_or(text.len());
    let authority = &text[..end];
    if authority.starts_with('[') {
        return match authority.find(']') {
            Some(close) => (
                &authority[..=close],
                authority[close + 1..].starts_with(':'),
            ),
            None => (authority, false),
        };
    }
    match authority.split_once(':') {
        Some((host, _)) => (host, true),
        None => (authority, false),
    }
}

/// Whether a host can only be this machine: `localhost` and its subdomains
/// (which RFC 6761 reserves for loopback, and which Chromium resolves there
/// without asking), `127.0.0.0/8`, and `[::1]`.
fn local_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host == "[::1]" {
        return true;
    }
    let octets: Vec<&str> = host.split('.').collect();
    octets.len() == 4
        && octets[0] == "127"
        && octets.iter().all(|octet| {
            !octet.is_empty()
                && octet.bytes().all(|b| b.is_ascii_digit())
                && octet.parse::<u8>().is_ok()
        })
}

/// Words made safe to put in a url's query: the unreserved characters of
/// RFC 3986 as they are, and every other byte of the UTF-8 as `%XX`.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Seconds since the epoch, for the history; 0 on a clock set before it.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Mods;

    fn key(k: Key, mods: u32) -> KeyInput {
        KeyInput {
            key: k,
            mods: Mods(mods),
            action: KeyAction::Press,
            text: None,
        }
    }

    #[test]
    fn the_programs_own_keys_and_nothing_else() {
        assert_eq!(
            command(&key(Key::Char('q'), Mods::CTRL)),
            Some(Command::Quit)
        );
        assert_eq!(
            command(&key(Key::Char('l'), Mods::CTRL)),
            Some(Command::EditUrl)
        );
        assert_eq!(
            command(&key(Key::Char('r'), Mods::CTRL)),
            Some(Command::Reload)
        );
        assert_eq!(command(&key(Key::Left, Mods::ALT)), Some(Command::Back));
        assert_eq!(command(&key(Key::Right, Mods::ALT)), Some(Command::Forward));

        // Everything else belongs to the page.
        assert_eq!(command(&key(Key::Char('q'), 0)), None);
        assert_eq!(command(&key(Key::Char('l'), Mods::ALT)), None);
        assert_eq!(command(&key(Key::Left, Mods::CTRL)), None);
        assert_eq!(command(&key(Key::Char('a'), Mods::CTRL)), None);
        assert_eq!(command(&key(Key::Tab, 0)), None, "tab is the page's");

        // And a release of one of them is not a second command.
        let mut released = key(Key::Char('q'), Mods::CTRL);
        released.action = KeyAction::Release;
        assert_eq!(command(&released), None);
    }

    #[test]
    fn the_tab_keys_are_the_ones_a_browser_taught_and_the_compositor_left() {
        assert_eq!(
            command(&key(Key::Char('t'), Mods::CTRL)),
            Some(Command::NewTab)
        );
        assert_eq!(
            command(&key(Key::Char('w'), Mods::CTRL)),
            Some(Command::CloseTab)
        );
        assert_eq!(command(&key(Key::Tab, Mods::CTRL)), Some(Command::NextTab));
        assert_eq!(
            command(&key(Key::Tab, Mods::CTRL | Mods::SHIFT)),
            Some(Command::PreviousTab)
        );
        for n in 1..=8usize {
            let digit = Key::Char((b'0' + n as u8) as char);
            assert_eq!(
                command(&key(digit, Mods::ALT)),
                Some(Command::SelectTab(n)),
                "alt+{n}"
            );
        }
        assert_eq!(
            command(&key(Key::Char('9'), Mods::ALT)),
            Some(Command::LastTab),
            "alt+9 is the last tab"
        );
        // There is no tab zero — alt+0 is back to 100% — and a digit on its
        // own is still typing.
        assert_eq!(
            command(&key(Key::Char('0'), Mods::ALT)),
            Some(Command::ZoomReset)
        );
        assert_eq!(command(&key(Key::Char('1'), 0)), None);
        // ctrl+digit stays the page's: a page may bind it, and the compositor
        // puts its own workspace digits on ctrl+shift and on super.
        assert_eq!(command(&key(Key::Char('1'), Mods::CTRL)), None);
        // alt+tab is the window manager's everywhere, and is not taken here.
        assert_eq!(command(&key(Key::Tab, Mods::ALT)), None);
    }

    #[test]
    fn the_tab_keys_include_the_last_tab_the_list_and_moving_a_tab() {
        assert_eq!(
            command(&key(Key::Char('9'), Mods::ALT)),
            Some(Command::LastTab)
        );
        for n in 1..=8u8 {
            assert!(matches!(
                command(&key(Key::Char((b'0' + n) as char), Mods::ALT)),
                Some(Command::SelectTab(_))
            ));
        }
        // The list: Chrome's chord in either spelling a terminal may send
        // it, and the `alt` form every terminal lets through.
        for (k, mods) in [
            (Key::Char('a'), Mods::CTRL | Mods::SHIFT),
            (Key::Char('A'), Mods::CTRL | Mods::SHIFT),
            (Key::Char('a'), Mods::ALT),
        ] {
            assert_eq!(command(&key(k, mods)), Some(Command::ListTabs), "{k:?}");
        }
        // `ctrl+a` is the page's select-all, and the rest are nobody's.
        for (k, mods) in [
            (Key::Char('a'), Mods::CTRL),
            (Key::Char('a'), Mods::ALT | Mods::SHIFT),
            (Key::Char('a'), Mods::CTRL | Mods::ALT),
        ] {
            assert_eq!(command(&key(k, mods)), None, "{k:?} {mods}");
        }
        for mods in [Mods::CTRL | Mods::SHIFT, Mods::ALT | Mods::SHIFT] {
            assert_eq!(command(&key(Key::PageUp, mods)), Some(Command::MoveTab(-1)));
            assert_eq!(
                command(&key(Key::PageDown, mods)),
                Some(Command::MoveTab(1))
            );
        }
        // A page key with one modifier is still the page's.
        for mods in [Mods::CTRL, Mods::SHIFT, Mods::ALT] {
            assert_eq!(command(&key(Key::PageUp, mods)), None, "{mods}");
            assert_eq!(command(&key(Key::PageDown, mods)), None, "{mods}");
        }
        // And a release of any of them is nothing.
        for (k, mods) in [
            (Key::Char('9'), Mods::ALT),
            (Key::Char('a'), Mods::ALT),
            (Key::PageUp, Mods::ALT | Mods::SHIFT),
        ] {
            let mut released = key(k, mods);
            released.action = KeyAction::Release;
            assert_eq!(command(&released), None);
        }
    }

    #[test]
    fn the_tab_keys_and_quit_survive_a_dialog_and_the_rest_wait() {
        let survives = |k: Key, mods: u32| command(&key(k, mods)).map(survives_dialog);
        // Away from the question, or out of the program.
        assert_eq!(survives(Key::Char('q'), Mods::CTRL), Some(true));
        assert_eq!(survives(Key::Char('t'), Mods::CTRL), Some(true));
        assert_eq!(survives(Key::Char('w'), Mods::CTRL), Some(true));
        assert_eq!(survives(Key::Tab, Mods::CTRL), Some(true));
        assert_eq!(survives(Key::Tab, Mods::CTRL | Mods::SHIFT), Some(true));
        for n in 1..=9u8 {
            assert_eq!(
                survives(Key::Char((b'0' + n) as char), Mods::ALT),
                Some(true)
            );
        }
        // The list and moving a tab: going elsewhere, and reordering around
        // the question, do nothing to the page that is asking.
        assert_eq!(survives(Key::Char('a'), Mods::ALT), Some(true));
        assert_eq!(
            survives(Key::Char('a'), Mods::CTRL | Mods::SHIFT),
            Some(true)
        );
        assert_eq!(survives(Key::PageUp, Mods::ALT | Mods::SHIFT), Some(true));
        assert_eq!(
            survives(Key::PageDown, Mods::CTRL | Mods::SHIFT),
            Some(true)
        );
        // Things done to the page that is asking wait until it has an answer.
        assert_eq!(survives(Key::Char('l'), Mods::CTRL), Some(false));
        assert_eq!(survives(Key::Char('r'), Mods::CTRL), Some(false));
        assert_eq!(survives(Key::Left, Mods::ALT), Some(false));
        assert_eq!(survives(Key::Right, Mods::ALT), Some(false));
        // And a key that is not the program's is not a command at all: it is
        // what answers the dialog.
        assert_eq!(survives(Key::Char('y'), 0), None);
        assert_eq!(survives(Key::Enter, 0), None);
        assert_eq!(survives(Key::Escape, 0), None);
    }

    #[test]
    fn the_tab_keys_and_quit_survive_an_upload_prompt_and_the_rest_wait() {
        // The same set as a dialog's, and the same reasons: see
        // `survives_dialog`. Checked here from the upload's side so that a
        // change to one is a decision about both.
        let survives = |k: Key, mods: u32| command(&key(k, mods)).map(survives_dialog);
        for (k, mods) in [
            (Key::Char('q'), Mods::CTRL),
            (Key::Char('t'), Mods::CTRL),
            (Key::Char('w'), Mods::CTRL),
            (Key::Tab, Mods::CTRL),
            (Key::Char('2'), Mods::ALT),
            (Key::Char('u'), Mods::ALT),
        ] {
            assert_eq!(survives(k, mods), Some(true), "{k:?}");
        }
        for (k, mods) in [
            (Key::Char('l'), Mods::CTRL),
            (Key::Char('r'), Mods::CTRL),
            (Key::Left, Mods::ALT),
            (Key::Right, Mods::ALT),
        ] {
            assert_eq!(survives(k, mods), Some(false), "{k:?}");
        }
        // Tab, Enter and Escape are the path's.
        assert_eq!(survives(Key::Tab, 0), None);
        assert_eq!(survives(Key::Enter, 0), None);
        assert_eq!(survives(Key::Escape, 0), None);
    }

    #[test]
    fn the_row_goes_to_the_url_bar_then_find_then_the_list_then_a_dialog_then_a_file_input() {
        let owner_with =
            |tabs: &Tabs<()>, bar: Option<&UrlBar>, list: Option<&TabList>| match row_owner(
                tabs, bar, None, list,
            ) {
                Some(RowOwner::Bar(_)) => "bar",
                Some(RowOwner::Find(_)) => "find",
                Some(RowOwner::List(_)) => "list",
                Some(RowOwner::Dialog(_)) => "dialog",
                Some(RowOwner::Upload(_)) => "upload",
                None => "page",
            };
        let owner = |tabs: &Tabs<()>, bar: Option<&UrlBar>| owner_with(tabs, bar, None);
        let mut tabs = Tabs::new(Tab::new("a", (), "https://a.example/"));
        let bar = UrlBar::new(Line::empty());
        assert_eq!(owner(&tabs, None), "page");

        let chooser = upload::Chooser {
            backend_node_id: 3,
            multiple: false,
            frame_id: "F".to_string(),
        };
        let tab = tabs.active_mut().expect("a tab");
        tab.upload = Some(Upload::new(chooser, PathBuf::from("/work"), None));
        assert_eq!(owner(&tabs, None), "upload");
        assert_eq!(owner(&tabs, Some(&bar)), "bar", "the person's typing first");

        // An alert while the path is half typed: the page is stopped, so the
        // alert has the row, and the path is underneath it.
        let alert = Json::parse(r#"{"type":"alert","message":"m","url":"u"}"#).expect("JSON");
        let tab = tabs.active_mut().expect("a tab");
        tab.dialog = Dialog::opening(&alert);
        assert_eq!(owner(&tabs, None), "dialog");
        assert_eq!(owner(&tabs, Some(&bar)), "bar");
        // The list is the person's, over the dialog and the path; the bar is
        // over it, although the keyboard never has both open.
        let list = TabList::open(0);
        assert_eq!(owner_with(&tabs, None, Some(&list)), "list");
        assert_eq!(owner_with(&tabs, Some(&bar), Some(&list)), "bar");
        let tab = tabs.active_mut().expect("a tab");
        tab.dialog = None;
        assert_eq!(owner_with(&tabs, None, Some(&list)), "list");
        assert_eq!(owner(&tabs, None), "upload", "and back when it is answered");

        // A tab behind with a prompt does not own the row in front.
        tabs.open(Tab::new("b", (), "https://b.example/"));
        assert_eq!(owner(&tabs, None), "page");
    }

    #[test]
    fn the_zoom_keys_are_the_browsers_and_the_ones_every_terminal_lets_through() {
        for mods in [Mods::CTRL, Mods::ALT, Mods::CTRL | Mods::SHIFT] {
            for (c, wanted) in [
                ('=', Command::ZoomIn),
                ('+', Command::ZoomIn),
                ('-', Command::ZoomOut),
                ('_', Command::ZoomOut),
                ('0', Command::ZoomReset),
            ] {
                assert_eq!(
                    command(&key(Key::Char(c), mods)),
                    Some(wanted),
                    "{c} {mods:?}"
                );
            }
        }
        // `ctrl+-` in a legacy terminal is `0x1f`, which is ctrl+_.
        match Parser::new().feed(&[0x1f]).as_slice() {
            [Input::Key(legacy)] => assert_eq!(command(legacy), Some(Command::ZoomOut)),
            other => panic!("{other:?}"),
        }
        // Unmodified they are typing, both at once is the page's, and the
        // other ctrl-digits stay the page's.
        assert_eq!(command(&key(Key::Char('='), 0)), None);
        assert_eq!(command(&key(Key::Char('-'), Mods::SHIFT)), None);
        assert_eq!(command(&key(Key::Char('0'), Mods::CTRL | Mods::ALT)), None);
        for n in 1..=9u8 {
            assert_eq!(
                command(&key(Key::Char((b'0' + n) as char), Mods::CTRL)),
                None
            );
        }
    }

    #[test]
    fn the_zoom_keys_wait_for_a_dialog_and_a_file_inputs_path() {
        // One rule for both, `survives_dialog`, which `handle_input` asks for
        // a dialog and for a path alike: a level changed under either would
        // lay the page out again behind something the person is answering.
        for command in [Command::ZoomIn, Command::ZoomOut, Command::ZoomReset] {
            assert!(!survives_dialog(command), "{command:?}");
        }
    }

    #[test]
    fn the_words_at_the_end_of_the_row_are_what_there_is_two_spaces_apart() {
        assert_eq!(words(&[None, None]), None);
        assert_eq!(words(&[Some("150%"), None]).as_deref(), Some("150%"));
        assert_eq!(
            words(&[Some("downloading report.pdf 42%"), Some("150%")]).as_deref(),
            Some("downloading report.pdf 42%  150%")
        );
        assert_eq!(
            words(&[Some("150%"), Some("https://example.com")]).as_deref(),
            Some("150%  https://example.com")
        );
    }

    #[test]
    fn copying_is_alt_c_and_alt_u_and_ctrl_c_and_ctrl_v_stay_the_pages() {
        assert_eq!(
            command(&key(Key::Char('c'), Mods::ALT)),
            Some(Command::CopySelection)
        );
        assert_eq!(
            command(&key(Key::Char('u'), Mods::ALT)),
            Some(Command::CopyUrl)
        );
        // The engine's own copy and paste, within the page.
        assert_eq!(command(&key(Key::Char('c'), Mods::CTRL)), None);
        assert_eq!(command(&key(Key::Char('v'), Mods::CTRL)), None);
        assert_eq!(command(&key(Key::Char('v'), Mods::ALT)), None);
        assert_eq!(command(&key(Key::Char('c'), Mods::CTRL | Mods::ALT)), None);
        assert_eq!(command(&key(Key::Char('c'), 0)), None);

        // The url is this program's to copy whatever the page is doing; the
        // selection has to be asked of a page that may be stopped.
        assert!(survives_dialog(Command::CopyUrl));
        assert!(!survives_dialog(Command::CopySelection));
    }

    #[test]
    fn ctrl_f_is_find_and_does_not_survive_a_dialog() {
        assert_eq!(
            command(&key(Key::Char('f'), Mods::CTRL)),
            Some(Command::Find)
        );
        // It asks the page, and a page behind a dialog answers nothing.
        assert!(!survives_dialog(Command::Find));
        // Only ctrl: `f` is typing, and the others are the page's.
        assert_eq!(command(&key(Key::Char('f'), 0)), None);
        assert_eq!(command(&key(Key::Char('f'), Mods::ALT)), None);
        assert_eq!(command(&key(Key::Char('f'), Mods::CTRL | Mods::ALT)), None);
        // The prompt's own next and previous are the prompt's, not commands
        // a page loses when it is closed.
        assert_eq!(command(&key(Key::Char('g'), Mods::CTRL)), None);
    }

    #[test]
    fn the_find_prompt_draws_its_count_at_the_end_and_the_url_bar_is_unchanged() {
        let line = Line::selected("fox");
        assert_eq!(
            typing_row_beside(40, "url: ", &line, ""),
            screen::prompt_line(40, "url: ", "fox", "", 3)
        );
        let row = String::from_utf8(typing_row_beside(40, "find: ", &line, "3/17"))
            .expect("a row is UTF-8");
        assert!(row.contains("find: fox "), "{row:?}");
        assert!(row.contains("  3/17\x1b[0m"), "{row:?}");
        assert!(row.ends_with("\x1b[1;10H\x1b[?25h"), "{row:?}");
    }

    #[test]
    fn a_paste_into_a_line_is_one_line_and_replaces_what_was_offered() {
        let mut line = Line::selected("https://example.com/offered");
        assert!(paste_into_line(&mut line, "https://pasted.example/\r\n"));
        assert_eq!(line.text(), "https://pasted.example/");
        assert!(paste_into_line(&mut line, "a\tb\x1b[2J"));
        assert_eq!(line.text(), "https://pasted.example/ab[2J");
        // A paste that is nothing once it is one line changes nothing, and
        // leaves the line still offered whole.
        let mut line = Line::selected("offered");
        assert!(!paste_into_line(&mut line, "\r\n"));
        assert_eq!(line.text(), "offered");
        assert!(line.whole());
    }

    #[test]
    fn a_prompt_takes_a_paste_and_the_other_dialogs_do_not() {
        let asked = |kind: &str| {
            Dialog::opening(
                &Json::parse(&format!(
                    r#"{{"type":"{kind}","message":"m","url":"u","defaultPrompt":"default"}}"#
                ))
                .expect("the test's own JSON"),
            )
            .expect("a dialog")
        };
        let mut prompt = asked("prompt");
        assert!(paste_into_dialog(&mut prompt, "pasted\n"));
        assert_eq!(prompt.line.text(), "pasted", "over the default, as a key");
        assert!(!prompt.line.whole());
        assert!(paste_into_dialog(&mut prompt, " more"));
        assert_eq!(prompt.line.text(), "pasted more");
        // And it is still a question waiting on a key: a paste is not Enter.
        assert_eq!(
            prompt.step(&key(Key::Char('x'), Mods::SHIFT)),
            Answer::Waiting
        );

        for kind in ["alert", "confirm", "beforeunload"] {
            let mut dialog = asked(kind);
            let before = dialog.clone();
            assert!(!paste_into_dialog(&mut dialog, "y\n"), "{kind}");
            assert_eq!(dialog, before, "{kind}");
        }
    }

    #[test]
    fn a_copy_says_how_much_and_never_what() {
        assert_eq!(copied("https://example.com", Copied::Url), "copied url");
        assert_eq!(copied("日本語", Copied::Text), "copied 3 characters");
        assert_eq!(copied("a", Copied::Text), "copied 1 character");
        assert!(!copied("\x1b]0;secret\x07", Copied::Text).contains("secret"));
    }

    #[test]
    fn what_a_person_types_becomes_a_url_the_engine_takes() {
        assert_eq!(normalise("example.com"), "https://example.com");
        assert_eq!(
            normalise("example.com:8443/a"),
            "https://example.com:8443/a"
        );
        assert_eq!(normalise("  example.com/a b "), "https://example.com/a b");
        assert_eq!(normalise("http://example.com"), "http://example.com");
        assert_eq!(normalise("https://example.com"), "https://example.com");
        assert_eq!(normalise("about:blank"), "about:blank");
        assert_eq!(normalise("data:text/html,hi"), "data:text/html,hi");
        assert_eq!(normalise("/etc/hostname"), "file:///etc/hostname");
        assert_eq!(normalise(""), "about:blank");
        assert_eq!(normalise("   "), "about:blank");
        // A paste is plain text before it is a url: what is sent is what is
        // shown, and neither can be an override or an escape.
        assert_eq!(normalise("exa\u{202e}mple.com"), "https://example.com");
        assert_eq!(
            normalise("\x1b]0;x\x07example.com"),
            "https://]0;xexample.com"
        );
        assert_eq!(normalise("example.com\r\n"), "https://example.com");
        // This machine has no certificate: plain http, port or no port.
        for local in [
            "localhost",
            "localhost:3000",
            "LocalHost:3000/app",
            "app.localhost/x",
            "127.0.0.1:8000/",
            "127.1.2.3",
            "[::1]:8080",
            "[::1]",
        ] {
            assert_eq!(normalise(local), format!("http://{local}"), "{local}");
        }
        // A name that only starts like one is somebody else's machine.
        for remote in [
            "localhost.example.com",
            "mylocalhost",
            "127.0.0.1.example.com",
            "128.0.0.1",
            "[::2]",
        ] {
            assert_eq!(normalise(remote), format!("https://{remote}"), "{remote}");
        }
        // And a scheme typed is a scheme kept.
        assert_eq!(
            normalise("https://localhost:3000"),
            "https://localhost:3000"
        );
    }

    /// What is typed, and whether a search url makes it a search.
    const TYPED: [(&str, bool); 24] = [
        ("example.com", false),
        ("example.com/a b", true),
        ("rust borrow checker", true),
        ("rust", true),
        ("what?", true),
        ("3.14", true),
        ("foo.123", true),
        (".com", true),
        ("a..b", true),
        ("\u{4f8b}\u{3048}.jp", false),
        ("a.b/c?d", false),
        ("my-site.co.uk", false),
        ("192.168.1.1", false),
        ("192.168.1.300", true),
        ("localhost", false),
        ("localhost:3000", false),
        ("app.localhost", false),
        ("[::1]:8080", false),
        ("[2001:db8::1]", false),
        ("myhost:8080", false),
        ("https://example.com/?q=a b", false),
        ("about:blank", false),
        ("/etc/hostname", false),
        ("", false),
    ];

    #[test]
    fn with_a_search_url_words_are_searched_and_hosts_are_still_hosts() {
        let search = Some("https://search.example/?q=%s&x=%s");
        for (typed, words) in TYPED {
            let went = destination(typed, search);
            if words {
                assert!(
                    went.starts_with("https://search.example/?q="),
                    "{typed:?}: {went}"
                );
                assert!(went.ends_with("&x=%s"), "the first %s only: {went}");
            } else {
                assert_eq!(went, normalise(typed), "{typed:?}");
            }
        }
        assert_eq!(
            destination("  a b&c  ", search),
            "https://search.example/?q=a%20b%26c&x=%s"
        );
        assert_eq!(
            destination("caf\u{e9}?", Some("https://s.example/%s")),
            "https://s.example/caf%C3%A9%3F"
        );
        // What is searched is the plain text of it, as what is sent always is.
        assert_eq!(
            destination("a\u{202e} b", Some("https://s.example/%s")),
            "https://s.example/a%20b"
        );
    }

    #[test]
    fn without_a_search_url_nothing_typed_leaves_for_a_third_party() {
        for (typed, _) in TYPED {
            assert_eq!(destination(typed, None), normalise(typed), "{typed:?}");
        }
        assert_eq!(destination("rust", None), "https://rust");
    }

    fn typed(c: char) -> KeyInput {
        KeyInput {
            key: Key::Char(c),
            mods: Mods::default(),
            action: KeyAction::Press,
            text: Some(c),
        }
    }

    #[test]
    fn the_url_bar_offers_what_was_visited_and_walks_it_with_up_and_down() {
        let mut history = History::in_memory();
        let _ = history.visited("https://example.com/docs", "Docs", 1);
        let _ = history.visited("https://rust-lang.org/", "Rust", 2);
        let mut bar = UrlBar::new(Line::selected("https://old.example/"));

        // Typing over the offered url asks for a suggestion.
        for c in "exa".chars() {
            assert_eq!(bar_step(&mut bar, &history, &typed(c)), Edit::Inserted);
        }
        assert_eq!(bar.line.text(), "exa");
        assert_eq!(bar.line.hint(), "mple.com/docs");
        // A backspace takes it away and does not bring it back.
        bar_step(&mut bar, &history, &key(Key::Backspace, 0));
        assert_eq!(bar.line.text(), "ex");
        assert_eq!(bar.line.hint(), "");
        // Typing again does, and Tab takes it.
        bar_step(&mut bar, &history, &typed('a'));
        bar_step(&mut bar, &history, &key(Key::Tab, 0));
        assert_eq!(bar.line.text(), "example.com/docs");
        assert_eq!(bar.line.hint(), "");

        // Up on an empty bar walks back through where you have been, and Down
        // comes back to what was typed.
        let mut bar = UrlBar::new(Line::empty());
        bar_step(&mut bar, &history, &key(Key::Up, 0));
        assert_eq!(bar.line.text(), "https://rust-lang.org/");
        assert_eq!(bar.line.hint(), "", "no suggestion while walking");
        bar_step(&mut bar, &history, &key(Key::Up, 0));
        assert_eq!(bar.line.text(), "https://example.com/docs");
        bar_step(&mut bar, &history, &key(Key::Up, 0));
        assert_eq!(bar.line.text(), "https://example.com/docs", "the oldest");
        bar_step(&mut bar, &history, &key(Key::Down, 0));
        bar_step(&mut bar, &history, &key(Key::Down, 0));
        assert_eq!(bar.line.text(), "");

        // Up after typing walks what matches it.
        let mut bar = UrlBar::new(Line::empty());
        for c in "docs".chars() {
            bar_step(&mut bar, &history, &typed(c));
        }
        bar_step(&mut bar, &history, &key(Key::Up, 0));
        assert_eq!(bar.line.text(), "https://example.com/docs");
        assert!(bar.walk.is_some());
        // And an edit ends the walk, so the next Up walks what is there now.
        bar_step(&mut bar, &history, &key(Key::Backspace, 0));
        assert!(bar.walk.is_none());
    }

    #[test]
    fn a_page_is_the_pane_less_the_status_row() {
        let metrics = Metrics {
            cols: 80,
            rows: 24,
            cell: (8, 16),
        };
        assert_eq!(page_pixels(metrics), (640, 368));
        assert_eq!(page_cells(metrics), Cells { cols: 80, rows: 23 });
        // 23 rows of 16 pixels is what the page is told it has, and 23 rows is
        // what the placement asks for: the two have to agree or the picture is
        // resampled every frame.
        assert_eq!(
            page_cells(metrics).rows * metrics.cell.1,
            page_pixels(metrics).1
        );
        // At 200% — a HiDPI scale, a zoom, or one of each — the page is laid
        // out in half that, and the middle of cell (11, 4) is half as far in.
        let viewport = Viewport::fit(page_pixels(metrics), 2.0);
        assert_eq!(viewport.css, (320, 184));
        assert_eq!(viewport.css_point(84, 40), (42.0, 20.0));
        // And the placement does not move: the cells are the pane's.
        assert_eq!(page_cells(metrics), Cells { cols: 80, rows: 23 });
    }

    #[test]
    fn two_quick_presses_in_the_same_place_are_a_double_click() {
        let mut clicks = Clicks::default();
        assert_eq!(clicks.press(0, 10, 10), 1);
        assert_eq!(clicks.press(0, 10, 10), 2);
        assert_eq!(clicks.press(0, 12, 11), 3, "a little movement is allowed");
        assert_eq!(clicks.press(0, 100, 10), 1, "a lot is not");
        assert_eq!(clicks.press(0, 100, 10), 2);
        assert_eq!(clicks.press(2, 100, 10), 1, "and the other button is new");
    }

    #[test]
    fn the_button_names_and_the_mask_agree_with_each_other() {
        assert_eq!(button_name(Some(0)), "left");
        assert_eq!(button_name(Some(1)), "middle");
        assert_eq!(button_name(Some(2)), "right");
        assert_eq!(button_name(None), "none");
        assert_eq!(button_bit(Some(0)), 1);
        assert_eq!(button_bit(Some(2)), 2, "right is two, not four");
        assert_eq!(button_bit(Some(1)), 4);
        assert_eq!(button_bit(None), 0);
    }

    #[test]
    fn the_status_line_says_what_is_known() {
        let mut tab: Tab<()> = Tab::new("t", (), "");
        assert_eq!(tab.line(), "blinkterm");
        tab.url = "https://example.com".to_string();
        assert_eq!(tab.line(), "https://example.com");
        tab.title = "Example".to_string();
        assert_eq!(tab.line(), "Example  —  https://example.com");
        tab.note = Some("loading".to_string());
        assert_eq!(tab.line(), "loading");
        tab.note = None;
        tab.url = "http://wiki.corp/".to_string();
        tab.title = "Wiki".to_string();
        tab.trust = load::Trust::Insecure;
        assert_eq!(tab.line(), "not secure  —  Wiki  —  http://wiki.corp/");
    }

    #[test]
    fn escape_goes_to_whatever_has_the_row_then_the_load_then_the_page() {
        let alert = Json::parse(r#"{"type":"alert","message":"m","url":"u"}"#).expect("JSON");
        let chooser = upload::Chooser {
            backend_node_id: 3,
            multiple: false,
            frame_id: "F".to_string(),
        };
        let bar = UrlBar::new(Line::empty());
        let find = Find {
            target: "a".to_string(),
            finder: find::Finder::open(""),
            context: None,
            pending: None,
            wanted: None,
            remade: false,
        };
        // Every combination of the four things that can have the row, and a
        // load under them or not: the answer is the first of them in
        // `row_owner`'s order, and the load only when none of them is there.
        let list = TabList::open(0);
        for mask in 0..64u32 {
            let [open_bar, open_find, dialog, path, loading, open_list] =
                [0, 1, 2, 3, 4, 5].map(|bit| mask & (1 << bit) != 0);
            let mut tab = Tab::new("a", (), "https://a.example/");
            tab.loading = loading;
            if dialog {
                tab.dialog = Dialog::opening(&alert);
            }
            if path {
                tab.upload = Some(Upload::new(chooser.clone(), PathBuf::from("/work"), None));
            }
            let tabs = Tabs::new(tab);
            let wanted = if open_bar {
                Escapes::UrlBar
            } else if open_find {
                Escapes::Find
            } else if open_list {
                Escapes::List
            } else if dialog {
                Escapes::Dialog
            } else if path {
                Escapes::Upload
            } else if loading {
                Escapes::StopLoading
            } else {
                Escapes::Page
            };
            let owner = row_owner(
                &tabs,
                open_bar.then_some(&bar),
                open_find.then_some(&find),
                open_list.then_some(&list),
            );
            assert_eq!(
                escapes(owner.as_ref(), loading),
                wanted,
                "bar {open_bar}, find {open_find}, list {open_list}, dialog {dialog}, \
                 path {path}, loading {loading}"
            );
        }
        // Escape is not one of the program's commands: when nothing wants
        // it, it is the page's key, as it always was.
        assert_eq!(command(&key(Key::Escape, 0)), None);
    }

    #[test]
    fn a_bare_motion_is_the_hovers_and_a_drag_is_still_the_pages() {
        let report = |bytes: &[u8]| match Parser::new().feed(bytes).as_slice() {
            [Input::Mouse(report)] => *report,
            other => panic!("{bytes:?} is not one mouse report: {other:?}"),
        };
        // 35 is motion (32) with no button (3): the pointer moving.
        assert!(routes_to_hover(&report(b"\x1b[<35;10;5M")));
        assert!(routes_to_hover(&report(b"\x1b[<39;10;5M")), "with shift");
        // 32 is motion with the left button held: a drag, a selection.
        assert!(!routes_to_hover(&report(b"\x1b[<32;10;5M")));
        assert!(!routes_to_hover(&report(b"\x1b[<34;10;5M")), "right drag");
        // Presses, releases and the wheel go where they always went.
        assert!(!routes_to_hover(&report(b"\x1b[<0;10;5M")));
        assert!(!routes_to_hover(&report(b"\x1b[<0;10;5m")));
        assert!(!routes_to_hover(&report(b"\x1b[<64;10;5M")));
    }

    #[test]
    fn a_click_while_the_list_is_open_is_the_lists() {
        let report = |bytes: &[u8]| match Parser::new().feed(bytes).as_slice() {
            [Input::Mouse(report)] => *report,
            other => panic!("{bytes:?} is not one mouse report: {other:?}"),
        };
        let press = report(b"\x1b[<0;10;5M");
        assert!(routes_to_list(&press, true));
        assert!(!routes_to_list(&press, false), "the page's when it is shut");
        assert!(
            routes_to_list(&report(b"\x1b[<1;10;5M"), true),
            "a middle press too"
        );
        assert!(
            !routes_to_list(&report(b"\x1b[<64;10;5M"), true),
            "the wheel"
        );
        assert!(
            !routes_to_list(&report(b"\x1b[<35;10;5M"), true),
            "a motion"
        );
        assert!(
            !routes_to_list(&report(b"\x1b[<0;10;5m"), true),
            "a release"
        );
    }

    #[test]
    fn a_click_a_notch_and_the_hover_are_the_same_point() {
        let metrics = Metrics {
            cols: 80,
            rows: 24,
            cell: (8, 16),
        };
        let report = MouseInput {
            kind: MouseKind::Move,
            button: None,
            mods: Mods::default(),
            x: 3,
            y: 2,
            wheel: (0, 0),
        };
        // In cells: the middle of the third cell of the first page row.
        let cells = crate::input::page_point(&report, false, metrics.cell, 1);
        assert_eq!(cells, (20, 8));
        // In pixels: the pixel itself, less the row.
        assert_eq!(
            crate::input::page_point(&report, true, metrics.cell, 1),
            (2, -15)
        );
        // Then the page's pixels, at the level the page is at: the same
        // point at 100%, half as far in at 200% — a HiDPI scale, a zoom, or
        // one of each — whichever of the three is asking.
        let at = |factor: f64| css_point(Viewport::fit(page_pixels(metrics), factor), cells);
        assert_eq!(at(1.0), (20.0, 8.0));
        assert_eq!(at(2.0), (10.0, 4.0));
    }
}
