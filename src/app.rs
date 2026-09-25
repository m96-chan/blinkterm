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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tos_platform::tty::{self, ReadOutcome};
use tos_preview::fit::{Cells, Metrics};

use crate::cdp::{Client, Event, Notifier, Pending};
use crate::dialog::{Answer, Kind};
use crate::engine::Engine;
use crate::graphics::{Painter, Raw};
use crate::input::{Input, Key, KeyAction, KeyInput, MouseInput, MouseKind, Parser};
use crate::json::Json;
use crate::keys;
use crate::line::{self, Edit};
use crate::load::{self, Loaded, Problem};
use crate::motion::{self, Motion};
use crate::profile::{Choice, Profile};
use crate::screen::{self, Pane};
use crate::scroll::{self, Step};
use crate::tabs::{Outcome, Tab, Tabs};

/// How long the engine gets to answer on its pipe.
const ENGINE_TIMEOUT: Duration = Duration::from_secs(20);

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

/// What the person asked for on the command line.
pub struct Options {
    pub url: String,
    /// Where cookies, logins and site data are kept; see [`crate::profile`].
    pub profile: Choice,
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
    editing: Option<String>,
    /// Whether that url is still the one the page had, untouched.
    ///
    /// A browser's `ctrl+l` selects the whole address, so the next thing typed
    /// replaces it and a backspace deletes it. There is no selection to draw in
    /// a status line, but the behaviour is what the reflex expects, and keeping
    /// the old url until then is what makes `ctrl+l` also a way to read where
    /// you are.
    editing_whole: bool,
    /// The `Page.navigate` that has been sent and not yet answered.
    navigation: Option<Navigation>,
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
    let mut engine = Engine::launch(profile, ENGINE_TIMEOUT)?;
    // The browser's own client, rather than a page's. It is the only one that
    // can hear about a target this program did not open — a `target=_blank`,
    // a `window.open` — and the only one that can open, close, raise or
    // attach to one, which is how every page's client is made.
    let mut browser = engine.browser()?;
    browser.call(
        "Target.setDiscoverTargets",
        Json::object(vec![("discover", Json::Bool(true))]),
    )?;
    let first = crate::engine::first_page_target(&mut browser, TARGET_TIMEOUT).map_err(|why| {
        let tail = engine.tail();
        if tail.is_empty() {
            why
        } else {
            format!("{why}; the engine said: {}", tail.join(" / "))
        }
    })?;
    let client = browser.attach(&first, CONNECT_TIMEOUT)?;
    let mut tabs = Tabs::new(Tab::new(first, client, "about:blank"));

    let mut pane = Pane::enter(0, 1).map_err(|e| format!("cannot take the terminal: {e}"))?;
    let outcome = drive(&mut pane, &mut tabs, &mut browser, &mut engine, options);
    pane.leave();
    // Dropping the tabs closes every page's session, which is all a tab is
    // once the engine is about to be killed anyway.
    drop(tabs);
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
    outcome
}

/// Everything between taking the terminal and giving it back.
fn drive(
    pane: &mut Pane,
    tabs: &mut Tabs<Client>,
    browser: &mut Client,
    engine: &mut Engine,
    options: Options,
) -> Result<(), String> {
    let metrics = pane
        .metrics()
        .map_err(|e| format!("cannot measure the pane: {e}"))?;
    let mut chrome = Chrome {
        painter: Painter::new(),
        parser: Parser::new(),
        clicks: Clicks::default(),
        buttons: 0,
        metrics,
        motion: Motion::new(Instant::now()),
        still: None,
        wheel: scroll::Wheel::start(),
        editing: None,
        editing_whole: false,
        navigation: None,
    };

    activate(tabs, browser, &mut chrome)?;

    let url = normalise(&options.url);
    if let Some(tab) = tabs.active_mut() {
        tab.url = url.clone();
        tab.note = Some(format!("loading {url}"));
        tab.loading = true;
    }
    redraw_row(pane, tabs, &chrome)?;
    if let Some(why) = navigate(tabs, &mut chrome, &url) {
        if let Some(tab) = tabs.active_mut() {
            tab.note = Some(why);
        }
    }
    // Whether or not it was an error on the wire: a reply that says the page
    // did not come has replaced the loading note with why.
    redraw_row(pane, tabs, &chrome)?;

    let mut last_check = Instant::now();
    let mut buf = [0u8; 8192];

    while !QUIT.load(Ordering::SeqCst) {
        if tabs.is_empty() {
            // The last tab closed itself, which is the page saying the browser
            // is over — the same thing `ctrl+w` on the last tab means.
            return Ok(());
        }
        if RESIZED.swap(false, Ordering::SeqCst) {
            chrome.metrics = pane
                .metrics()
                .map_err(|e| format!("cannot measure the pane: {e}"))?;
            pane.write(b"\x1b[2J").map_err(|e| e.to_string())?;
            let metrics = chrome.metrics;
            if let Some(tab) = tabs.active_mut() {
                let stopped = tab.dialog.is_some();
                emulate(&mut tab.connection, metrics, stopped)?;
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
            // notches carry on. See `docs/design/browser.md`.
            let (width, height) = page_pixels(chrome.metrics);
            chrome.wheel.resized((width as i32, height as i32));
            redraw_row(pane, tabs, &chrome)?;
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
        reap_dead_tabs(pane, tabs, browser, &mut chrome)?;

        let wake = tabs.active().map(|tab| tab.connection.wake_fd());
        let mut watching = vec![pane.input_fd(), browser.wake_fd()];
        watching.extend(wake);
        let ready = tty::poll_readable(&watching, POLL_MS)
            .map_err(|e| format!("cannot wait for input: {e}"))?;

        if ready.contains(&pane.input_fd()) {
            match tty::read_available(pane.input_fd(), &mut buf) {
                Ok(ReadOutcome::Data(n)) => {
                    let inputs = chrome.parser.feed(&buf[..n]);
                    for input in inputs {
                        if !handle_input(pane, tabs, browser, &mut chrome, input)? {
                            return Ok(());
                        }
                    }
                }
                Ok(ReadOutcome::Eof) => return Ok(()),
                Ok(ReadOutcome::WouldBlock) => {}
                Err(err) => return Err(format!("cannot read the terminal: {err}")),
            }
        } else if let Some(input) = chrome.parser.flush() {
            // Nothing arrived, so a held escape was the Escape key after all.
            if !handle_input(pane, tabs, browser, &mut chrome, input)? {
                return Ok(());
            }
        }

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
        handle_target_events(pane, tabs, browser, &mut chrome)?;
        handle_page_events(pane, tabs, &mut chrome)?;
        // The answer to a navigation, which may have been held for as long as
        // a page's "leave this page?" was on the row. After the page's events,
        // so that a dialog which arrived on the same pass is already drawn.
        if chrome.navigation.is_some() {
            let before = tabs.active().map(Tab::line);
            collect_navigation(tabs, &mut chrome);
            if tabs.active().map(Tab::line) != before {
                redraw_row(pane, tabs, &chrome)?;
            }
        }
        // And last, because it is the thing to do when nothing else happened:
        // a page that has stopped moving gets its lossless picture.
        rest_shot(pane, tabs, &mut chrome)?;
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

/// Tell the engine how big the page is.
fn emulate(client: &mut Client, metrics: Metrics, stopped: bool) -> Result<(), String> {
    let (width, height) = page_pixels(metrics);
    tell(
        client,
        stopped,
        "Emulation.setDeviceMetricsOverride",
        Json::object(vec![
            ("width", Json::number(width)),
            ("height", Json::number(height)),
            ("deviceScaleFactor", Json::number(1)),
            ("mobile", Json::Bool(false)),
        ]),
        crate::cdp::CALL_TIMEOUT,
    )
}

/// Start the frames coming, in the format [`crate::motion`] argues for.
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
fn navigate(tabs: &mut Tabs<Client>, chrome: &mut Chrome, url: &str) -> Option<String> {
    let tab = tabs.active_mut()?;
    tab.problem = None;
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
pub fn navigated(tab: &mut Tab<Client>, url: &str, reply: &Json) {
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
    let Some(tab) = tabs.active_mut() else {
        return Ok(());
    };
    // First, whether the page is stopped behind a dialog, because that
    // decides whether anything below can be waited for. See [`tell`].
    bin_events(tab);
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
        bin_events(tab);
        if tab.dialog.is_none() {
            return Err(why);
        }
        stopped = true;
    }
    emulate(&mut tab.connection, metrics, stopped)?;
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
    bin_events(tab);
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

/// Throw away what a tab has queued, except what it says about a dialog.
///
/// A queue that is binned is binned for its frames, which are older than the
/// moment they would be painted in. A dialog is not like that: the page opened
/// it and is stopped until it is answered, however long ago that was, and a
/// `Page.javascriptDialogOpening` that went in the bin would be a tab that
/// had stopped with nothing on the row to say why and nothing to answer.
fn bin_events(tab: &mut Tab<Client>) {
    for event in tab.connection.events() {
        tab.dialog_event(&event);
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

/// Open a page in a new tab and switch to it.
fn open_tab(tabs: &mut Tabs<Client>, browser: &mut Client, url: &str) -> Result<(), String> {
    let created = browser.call(
        "Target.createTarget",
        Json::object(vec![("url", Json::string(url))]),
    )?;
    let target = created
        .get("targetId")
        .and_then(Json::as_str)
        .ok_or_else(|| "the engine opened a page and did not say which".to_string())?
        .to_string();
    let connection = connect_tab(browser, &target)?;
    tabs.open(Tab::new(target, connection, url));
    Ok(())
}

/// Attach to a target and start listening to its page, whether or not it is
/// the tab in front.
///
/// `Page.enable` from the moment the tab exists rather than from the moment it
/// is looked at: a tab that is loading in the background still has to tell the
/// strip when it has a name, and the events it sends before anybody asks are
/// the only notice there is.
fn connect_tab(browser: &mut Client, target: &str) -> Result<Client, String> {
    let mut connection = browser.attach(target, CONNECT_TIMEOUT)?;
    connection.call_within("Page.enable", Json::empty(), SWITCH_TIMEOUT)?;
    Ok(connection)
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
/// Four things share it, and which one is showing is a decision rather than a
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
fn redraw_row(pane: &mut Pane, tabs: &Tabs<Client>, chrome: &Chrome) -> Result<(), String> {
    let cols = chrome.metrics.cols;
    let Some(active) = tabs.active() else {
        return Ok(());
    };
    let bytes = if chrome.editing.is_some() {
        screen::status_line(cols, &active.line(), chrome.editing.as_deref())
    } else if let Some(dialog) = &active.dialog {
        let typed = dialog.typing().then_some(dialog.line.text.as_str());
        screen::dialog_line(cols, &dialog.caption(), dialog.hint(), typed)
    } else if tabs.len() < 2 {
        screen::status_line(cols, &active.line(), None)
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
                dialog: tab.dialog.is_some(),
            })
            .collect();
        screen::tab_line(cols, &labels, &active.url)
    };
    pane.write(&bytes).map_err(|e| e.to_string())
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
        let outcome = tabs.take(event, |target| connect_tab(browser, target));
        match outcome {
            Outcome::Ignored => {}
            Outcome::Opened | Outcome::Renamed => redraw = true,
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
                        tab.landed(landing);
                        redraw = true;
                    }
                }
                "Page.loadEventFired" => {
                    tab.loading = false;
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
                _ => {}
            }
        }

        if ask_title && tab.dialog.is_none() {
            if let Some(loaded) = page_loaded(&mut tab.connection) {
                tab.loaded(loaded);
            }
        }
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

/// Whether the row is a line being typed into: the url bar, or the answer to
/// a `prompt()` on the page in front.
fn row_owns_cursor(tabs: &Tabs<Client>, chrome: &Chrome) -> bool {
    chrome.editing.is_some()
        || tabs
            .active()
            .and_then(|tab| tab.dialog.as_ref())
            .is_some_and(|dialog| dialog.typing())
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
    let raw = Raw::rgba(&image.rgba, image.width, image.height);
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
fn request_still(tabs: &mut Tabs<Client>, chrome: &mut Chrome) {
    if asking(tabs) || !chrome.motion.wants_still(Instant::now()) {
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
    let distance = (
        report.wheel.0 as f64 * WHEEL_PIXELS,
        report.wheel.1 as f64 * WHEEL_PIXELS,
    );
    chrome.wheel.notch(
        &tab.target,
        Arc::new(Wire::new(tab.connection.notifier())),
        (x, y),
        distance,
    );
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
        Input::Key(key) => {
            // A key is a person working on this page, which is a reason not to
            // interrupt them with a screenshot — see [`motion::INPUT_QUIET`].
            // A release is not: it follows a press that has already been
            // counted, and a modifier let go on its own moves nothing.
            if key.action != KeyAction::Release {
                chrome.motion.input(Instant::now());
            }
            if chrome.editing.is_some() {
                return edit_url(pane, tabs, chrome, key);
            }
            let was = tabs.active_target().map(str::to_string);
            let command = command(&key);
            if asking(tabs) {
                // The page is waiting on an answer. The tab keys still work,
                // because leaving the question where it is — or closing the
                // tab it is on — is an answer too; the rest wait for it, and
                // every key that is not one of the program's is the answer.
                match command {
                    Some(command) if survives_dialog(command) => {}
                    Some(_) => return Ok(true),
                    None => return answer_dialog(pane, tabs, chrome, key),
                }
            }
            match command {
                Some(Command::Quit) => return Ok(false),
                Some(Command::EditUrl) => {
                    chrome.editing = tabs.active().map(|tab| tab.url.clone());
                    chrome.editing_whole = true;
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
                    match open_tab(tabs, browser, "about:blank") {
                        Ok(()) => {
                            switched(pane, tabs, browser, chrome, was)?;
                            // A new tab is a tab somebody is about to type an
                            // address into, so it opens with the cursor in the
                            // url bar — and with nothing in it, because there
                            // is no address here to replace.
                            chrome.editing = Some(String::new());
                            chrome.editing_whole = false;
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
                Some(what @ (Command::NextTab | Command::PreviousTab | Command::SelectTab(_))) => {
                    let moved = match what {
                        Command::NextTab => tabs.select_next(),
                        Command::PreviousTab => tabs.select_previous(),
                        Command::SelectTab(number) => tabs.select(number),
                        _ => false,
                    };
                    if moved {
                        switched(pane, tabs, browser, chrome, was)?;
                        redraw_row(pane, tabs, chrome)?;
                    }
                }
                None => {
                    if let Some(tab) = tabs.active_mut() {
                        send_key(&mut tab.connection, &key);
                    }
                }
            }
        }
        Input::Mouse(report) => {
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
                return Ok(true);
            }
            let metrics = chrome.metrics;
            let pixels = chrome.parser.pixel_coordinates();
            let clicks = &mut chrome.clicks;
            let buttons = &mut chrome.buttons;
            if let Some(tab) = tabs.active_mut() {
                send_mouse(
                    &mut tab.connection,
                    pixels,
                    clicks,
                    buttons,
                    metrics,
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
}

/// Whether a key the program keeps for itself still works while the page in
/// front has a dialog open.
///
/// Quitting, and everything about tabs: opening one, closing this one — which
/// closes its dialog with it, see [`close_tab`] — and going to another, which
/// leaves the question on its tab, marked in the strip, for later. What waits
/// is everything that would do something to the page that is asking. The url
/// bar would type over the question the person is meant to be reading; a
/// reload, a back or a forward to a page that is stopped would be queued
/// behind its dialog and done the moment it was answered, which is a
/// navigation nobody would remember asking for by then.
fn survives_dialog(command: Command) -> bool {
    match command {
        Command::Quit
        | Command::NewTab
        | Command::CloseTab
        | Command::NextTab
        | Command::PreviousTab
        | Command::SelectTab(_) => true,
        Command::EditUrl | Command::Reload | Command::Back | Command::Forward => false,
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

/// A page that asked before it was left and was told no.
///
/// It is where it was, and nothing about the tab should say otherwise: not
/// the "loading" note that went up when the url was typed, not that url in
/// place of the one the page still has. So the note goes, the tab is not
/// loading, and the url is asked of the engine's own history — which answers
/// at once, dialog or none, because it is the browser's rather than the
/// page's.
fn stayed(tab: &mut Tab<Client>) {
    tab.note = None;
    tab.loading = false;
    let Ok(history) =
        tab.connection
            .call_within("Page.getNavigationHistory", Json::empty(), SWITCH_TIMEOUT)
    else {
        return;
    };
    let index = history
        .get("currentIndex")
        .and_then(Json::as_i64)
        .unwrap_or(-1);
    let url = history
        .get("entries")
        .and_then(Json::as_array)
        .and_then(|entries| entries.get(usize::try_from(index).ok()?))
        .and_then(|entry| entry.get("url"))
        .and_then(Json::as_str);
    if let Some(url) = url {
        tab.url = url.to_string();
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
            Key::Tab if key.mods.shift() => Some(Command::PreviousTab),
            Key::Tab => Some(Command::NextTab),
            _ => None,
        };
    }
    if key.mods.alt() && !key.mods.ctrl() {
        return match key.key {
            Key::Left => Some(Command::Back),
            Key::Right => Some(Command::Forward),
            Key::Char(digit @ '1'..='9') => Some(Command::SelectTab(digit as usize - '0' as usize)),
            _ => None,
        };
    }
    None
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
    let whole = std::mem::take(&mut chrome.editing_whole);
    let Some(buffer) = chrome.editing.as_mut() else {
        return Ok(true);
    };
    match line::edit_step(buffer, whole, &key) {
        Edit::Typing => {}
        Edit::Quit => return Ok(false),
        Edit::Cancel => chrome.editing = None,
        Edit::Go => {
            let url = normalise(buffer);
            chrome.editing = None;
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

fn send_mouse(
    client: &mut Client,
    pixel_coordinates: bool,
    clicks: &mut Clicks,
    buttons: &mut u32,
    metrics: Metrics,
    report: MouseInput,
) {
    let (x, y) = crate::input::page_point(&report, pixel_coordinates, metrics.cell, 1);
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
/// fails with an error rather than guessing. What is deliberately not here is
/// a search engine: sending what somebody typed to a third party because it
/// did not parse as a host is a decision about their privacy, and it is not
/// this program's to make.
pub fn normalise(input: &str) -> String {
    let text = input.trim();
    if text.is_empty() {
        return "about:blank".to_string();
    }
    if text.contains("://") || text.starts_with("about:") || text.starts_with("data:") {
        return text.to_string();
    }
    if text.starts_with('/') {
        return format!("file://{text}");
    }
    format!("https://{text}")
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
        for n in 1..=9usize {
            let digit = Key::Char((b'0' + n as u8) as char);
            assert_eq!(
                command(&key(digit, Mods::ALT)),
                Some(Command::SelectTab(n)),
                "alt+{n}"
            );
        }
        // There is no tab zero, and a digit on its own is typing.
        assert_eq!(command(&key(Key::Char('0'), Mods::ALT)), None);
        assert_eq!(command(&key(Key::Char('1'), 0)), None);
        // ctrl+digit stays the page's: a page may bind it, and the compositor
        // puts its own workspace digits on ctrl+shift and on super.
        assert_eq!(command(&key(Key::Char('1'), Mods::CTRL)), None);
        // alt+tab is the window manager's everywhere, and is not taken here.
        assert_eq!(command(&key(Key::Tab, Mods::ALT)), None);
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
    fn what_a_person_types_becomes_a_url_the_engine_takes() {
        assert_eq!(normalise("example.com"), "https://example.com");
        assert_eq!(normalise("  example.com/a b "), "https://example.com/a b");
        assert_eq!(normalise("http://example.com"), "http://example.com");
        assert_eq!(normalise("https://example.com"), "https://example.com");
        assert_eq!(normalise("about:blank"), "about:blank");
        assert_eq!(normalise("data:text/html,hi"), "data:text/html,hi");
        assert_eq!(normalise("/etc/hostname"), "file:///etc/hostname");
        assert_eq!(normalise(""), "about:blank");
        assert_eq!(normalise("   "), "about:blank");
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
    }
}
