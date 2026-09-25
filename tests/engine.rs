//! The end this crate exists for: a real engine, real frames, and a real
//! terminal parsing what would go down the pane's pseudoterminal.
//!
//! The terminal here is `tos_term::Terminal` with the compositor's own
//! `ImageFiles` installed, which is exactly what `Pane::spawn` gives a pane —
//! so the `t=s` path is the real one, names and unlinking included, and the
//! frames go through the store that holds them in a session. What is missing
//! compared with a booted tOS is the renderer and the PTY, neither of which
//! can say anything about whether the protocol is right.
//!
//! Every test here runs only when `BLINKTERM_ENGINE` names the engine to
//! use, and skips otherwise — not when a Chromium happens to be on `PATH`.
//! The program itself searches `PATH`, because a person who installed a
//! browser wants it found; a test is different. A machine that builds tOS is
//! not a machine that agreed to run whatever browser its image carries for
//! some other job, and the first run on GitHub's runner found one, started
//! it, and watched it abort — a Chromium of somebody else's, with sandbox
//! rules of somebody else's, proving nothing about this crate either way.
//! Naming the engine is the consent. The skips are not quiet: the reason is
//! printed, so that a run which proved nothing does not read like a run which
//! proved something.
//!
//! Run them one at a time — `--test-threads=1`. Each test here starts a
//! Chromium, and the ones that assert on how smoothly a scroll moves are
//! measuring an animation against the wall clock: a second engine painting on
//! the same two cores is noise that reads as a lurch.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use blinkterm::cdp::{Client, Pending};
use blinkterm::engine::{self, Engine};
use blinkterm::find::{self, Matches};
use blinkterm::graphics::{Painter, Raw, IMAGE_ID};
use blinkterm::input::{Key, KeyAction, KeyInput, Mods};
use blinkterm::json::Json;
use blinkterm::keys;
use blinkterm::motion::{self, Motion};
use blinkterm::profile::{Choice, Profile};
use blinkterm::scroll::{self, Animator, Dispatch, Step, Wheel};
use blinkterm::tabs::{Outcome, Tab, Tabs};
use tos_compositor::ImageFiles;
use tos_preview::fit::Cells;

/// The page the frames come from.
///
/// It has to be a page whose frames cost what a real one's do: a flat colour
/// compresses to four kilobytes and would make the terminal look faster than
/// it is, and a full-screen gradient to four hundred, which would make it look
/// slower. So this is what a page mostly is — black text on white, a screenful
/// of it — with one coloured block that moves every animation frame so the
/// engine has a reason to repaint. That lands near the 58 kB per frame the
/// engine was measured at. The two listeners put what they were sent into the
/// title, where a test can read it back.
const PAGE: &str = "data:text/html,\
<body style='margin:0;height:100vh;font:12px monospace;overflow:hidden;background:%23fff'>\
<div id=b style='position:absolute;width:160px;height:40px;background:%23c33'></div>\
<div id=t></div><script>\
document.title='ready';\
addEventListener('keydown',function(e){document.title='key '+e.key+' '+e.code+' '+e.keyCode});\
addEventListener('mousedown',function(e){document.title='click '+e.button+' '+e.clientX+' '+e.clientY});\
var rows=[];for(var i=0;i<28;i++){rows.push(i+' the quick brown fox jumps over the lazy dog \
0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ')}\
document.getElementById('t').innerText=rows.join(String.fromCharCode(10));\
var b=document.getElementById('b'),n=0;\
function f(){n=n>400?0:n+3;b.style.left=n+'px';b.style.top=(n/2)+'px';\
requestAnimationFrame(f)}f();\
</script></body>";

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const CELL: (u32, u32) = (8, 16);

/// The same, with the target id the page's session is attached to, for the
/// tests that are about which targets exist.
fn connect_with_target() -> Option<(Engine, Client, String)> {
    connect_in_with_target(Profile::temporary().expect("a temporary profile"))
}

/// Connect to a fresh engine, or say why the test is not running.
///
/// On a temporary profile, so that no test reads or writes the one a person
/// browses with.
fn connect() -> Option<(Engine, Client)> {
    connect_in(Profile::temporary().expect("a temporary profile"))
}

/// The same, on the profile given.
fn connect_in(profile: Profile) -> Option<(Engine, Client)> {
    let (engine, client, _) = connect_in_with_target(profile)?;
    Some((engine, client))
}

/// The one all three are: on the profile given, with the page's target id.
fn connect_in_with_target(profile: Profile) -> Option<(Engine, Client, String)> {
    if std::env::var_os(engine::ENGINE_ENV).is_none() {
        eprintln!(
            "skipped: {} is not set; name a Chromium to run this against",
            engine::ENGINE_ENV
        );
        return None;
    }
    match engine::locate() {
        Ok(path) => eprintln!("engine: {}", path.display()),
        Err(why) => {
            eprintln!("skipped: {why}");
            return None;
        }
    }
    let engine = Engine::launch(profile, Duration::from_secs(30)).expect("the engine starts");
    // What `app::run` does: the browser's client finds the page and attaches
    // to it, and the page's session is the client the test drives. The
    // browser's client goes when this returns, which leaves the page's
    // session where it was and lets `tabbed` make another.
    let mut browser = engine.browser().expect("the browser's client");
    let target = match engine::first_page_target(&mut browser, Duration::from_secs(20)) {
        Ok(target) => target,
        Err(why) => panic!("{why}; the engine said: {}", engine.tail().join(" / ")),
    };
    let client = browser
        .attach(&target, Duration::from_secs(10))
        .expect("a session on the page");
    Some((engine, client, target))
}

/// Get the page ready: sized, loaded, and painting.
fn prepare(client: &mut Client) {
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    client
        .call(
            "Emulation.setDeviceMetricsOverride",
            Json::object(vec![
                ("width", Json::number(WIDTH)),
                ("height", Json::number(HEIGHT)),
                ("deviceScaleFactor", Json::number(1)),
                ("mobile", Json::Bool(false)),
            ]),
        )
        .expect("the viewport");
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(PAGE))]),
        )
        .expect("the page loads");
    wait_for_title(client, "ready", Duration::from_secs(10));
}

fn title(client: &mut Client) -> String {
    client
        .call_within(
            "Runtime.evaluate",
            Json::object(vec![
                ("expression", Json::string("document.title")),
                ("returnByValue", Json::Bool(true)),
            ]),
            Duration::from_secs(5),
        )
        .ok()
        .and_then(|value| {
            value
                .path(&["result", "value"])
                .and_then(Json::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn wait_for_title(client: &mut Client, wanted: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = title(client);
        if last.starts_with(wanted) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    last
}

fn a_terminal(dir: &std::path::Path) -> tos_term::Terminal {
    sized_terminal(dir, WIDTH, HEIGHT)
}

/// A terminal with the compositor's own file reader, one row taller than the
/// page so that the status line has somewhere to go.
fn sized_terminal(dir: &std::path::Path, width: u32, height: u32) -> tos_term::Terminal {
    let mut terminal = tos_term::Terminal::new(
        (width / CELL.0) as usize,
        (height / CELL.1) as usize + 1,
        tos_term::TerminalConfig::default(),
    );
    terminal.set_medium_reader(Box::new(ImageFiles::at(
        vec![dir.to_path_buf()],
        dir.to_path_buf(),
    )));
    terminal
}

/// The next screencast frame as the engine sent it, with the capture time it
/// carries. Acknowledges everything it takes, as the program does.
fn take_frames(client: &mut Client) -> Vec<(Vec<u8>, Option<f64>)> {
    let mut frames = Vec::new();
    for event in client.events() {
        if event.method != "Page.screencastFrame" {
            continue;
        }
        if let Some(session) = event.params.get("sessionId").and_then(Json::as_i64) {
            let _ = client.notify(
                "Page.screencastFrameAck",
                Json::object(vec![("sessionId", Json::number(session as f64))]),
            );
        }
        let Some(data) = event.params.get("data").and_then(Json::as_str) else {
            continue;
        };
        let stamp = event
            .params
            .path(&["metadata", "timestamp"])
            .and_then(Json::as_f64);
        if let Ok(bytes) = blinkterm::base64::decode(data.as_bytes()) {
            frames.push((bytes, stamp));
        }
    }
    frames
}

/// Start a screencast in one format at one size.
fn cast(client: &mut Client, format: &str, quality: Option<u32>, width: u32, height: u32) {
    let mut fields = vec![
        ("format", Json::string(format)),
        ("maxWidth", Json::number(width)),
        ("maxHeight", Json::number(height)),
        ("everyNthFrame", Json::number(1)),
    ];
    if let Some(quality) = quality {
        fields.push(("quality", Json::number(quality)));
    }
    client
        .call("Page.startScreencast", Json::object(fields))
        .expect("the screencast starts");
}

fn temp_dir(what: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("blinkterm-it-{what}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory");
    dir
}

/// A screencast, decoded here and drawn, with the numbers it cost.
#[test]
fn frames_reach_a_terminal_through_shared_memory() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);

    let dir = temp_dir("shm");
    let mut painter = Painter::at(&dir);
    let mut terminal = a_terminal(&dir);
    let cells = Cells {
        cols: WIDTH / CELL.0,
        rows: HEIGHT / CELL.1,
    };

    cast(&mut client, "jpeg", Some(motion::QUALITY), WIDTH, HEIGHT);

    let run_for = Duration::from_secs(3);
    let started = Instant::now();
    let (mut frames, mut bytes, mut escape_bytes) = (0usize, 0usize, 0usize);
    let (mut decoding, mut drawing) = (Duration::ZERO, Duration::ZERO);

    while started.elapsed() < run_for {
        for (jpeg, stamp) in take_frames(&mut client) {
            assert_eq!(&jpeg[..2], b"\xff\xd8", "the engine promised JPEG");
            // The clock the ordering rule in `blinkterm::motion` leans on:
            // CDP says seconds since the epoch, and the engine is a child of
            // this process, so it had better be this epoch.
            let stamp = stamp.expect("a frame says when it was captured");
            let drift = (motion::now_seconds() - stamp).abs();
            assert!(
                drift < 60.0,
                "metadata.timestamp is {stamp}, which is {drift:.1} s from this clock"
            );

            let at = Instant::now();
            let image = tos_term::jpeg::decode(&jpeg, 64 * 1024 * 1024).expect("a frame decodes");
            decoding += at.elapsed();
            assert_eq!((image.width, image.height), (WIDTH, HEIGHT));

            let at = Instant::now();
            let raw = Raw::rgb(&image.rgb, image.width, image.height);
            let sequence = painter.frame(raw, cells, 2, 1);
            terminal.advance(&sequence);
            drawing += at.elapsed();

            frames += 1;
            bytes += jpeg.len();
            escape_bytes += sequence.len();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = client.call("Page.stopScreencast", Json::empty());

    assert!(frames > 10, "only {frames} frames in three seconds");
    eprintln!(
        "shared memory: {frames} frames in {:?} ({:.1}/s), {} bytes of JPEG on average, \
         {} bytes down the pane per frame, {:?} decoding and {:?} in the terminal per frame",
        started.elapsed(),
        frames as f64 / started.elapsed().as_secs_f64(),
        bytes / frames,
        escape_bytes / frames,
        decoding / frames as u32,
        drawing / frames as u32,
    );

    // One image and one placement, however many frames went through: the
    // whole argument for a fixed id.
    let store = terminal.graphics();
    assert_eq!(store.placements().count(), 1, "a placement per frame leaks");
    let image = store.image(IMAGE_ID).expect("the frame is in the store");
    assert_eq!((image.width, image.height), (WIDTH, HEIGHT));
    let placement = store.placements().next().expect("one placement");
    assert_eq!(
        (placement.cols as u32, placement.rows as u32),
        (cells.cols, cells.rows)
    );
    assert_eq!(placement.row, 1, "row two, counted from zero");

    // And nothing is left in the directory that stands in for /dev/shm: the
    // terminal unlinked every name it read.
    let left: Vec<_> = dir.read_dir().expect("readable").flatten().collect();
    assert!(left.len() <= 16, "{} names left behind", left.len());
    painter.clean_up();
    std::fs::remove_dir_all(&dir).ok();

    client.close();
    engine.kill();
}

/// The same, inline, which is the path a terminal that cannot read `/dev/shm`
/// takes — and the one whose cost is worth knowing, because raw pixels made
/// it four times what it was.
///
/// The fallback sends the decoded pixels rather than the encoded frame,
/// because the encoded frame is a JPEG and no terminal's graphics path reads
/// one. `src/graphics.rs` argues that; this measures it.
#[test]
fn frames_also_reach_a_terminal_inline() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);

    let dir = temp_dir("inline");
    let mut terminal = a_terminal(&dir);
    let cells = Cells {
        cols: WIDTH / CELL.0,
        rows: HEIGHT / CELL.1,
    };
    cast(&mut client, "jpeg", Some(motion::QUALITY), WIDTH, HEIGHT);

    let started = Instant::now();
    let (mut frames, mut escape_bytes) = (0usize, 0usize);
    while started.elapsed() < Duration::from_secs(2) {
        for (jpeg, _) in take_frames(&mut client) {
            let image = tos_term::jpeg::decode(&jpeg, 64 * 1024 * 1024).expect("a frame decodes");
            let raw = Raw::rgb(&image.rgb, image.width, image.height);
            let sequence = blinkterm::graphics::inline_command(&raw, cells);
            terminal.advance(&sequence);
            frames += 1;
            escape_bytes += sequence.len();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = client.call("Page.stopScreencast", Json::empty());

    assert!(frames > 2, "only {frames} frames inline");
    eprintln!(
        "inline: {frames} frames in {:?} ({:.1}/s), {} bytes down the pane per frame",
        started.elapsed(),
        frames as f64 / started.elapsed().as_secs_f64(),
        escape_bytes / frames
    );
    let store = terminal.graphics();
    assert_eq!(store.placements().count(), 1);
    assert_eq!(
        store.image(IMAGE_ID).map(|i| (i.width, i.height)),
        Some((WIDTH, HEIGHT))
    );
    std::fs::remove_dir_all(&dir).ok();

    client.close();
    engine.kill();
}

/// The key table, checked against a page rather than against itself.
#[test]
fn a_key_arrives_as_the_key_the_page_expects() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);

    let cases: &[(KeyInput, &str)] = &[
        (
            KeyInput {
                key: Key::Char('a'),
                mods: Mods::default(),
                action: KeyAction::Press,
                text: Some('a'),
            },
            "key a KeyA 65",
        ),
        (
            KeyInput {
                key: Key::Enter,
                mods: Mods::default(),
                action: KeyAction::Press,
                text: None,
            },
            "key Enter Enter 13",
        ),
        (
            KeyInput {
                key: Key::Left,
                mods: Mods::default(),
                action: KeyAction::Press,
                text: None,
            },
            "key ArrowLeft ArrowLeft 37",
        ),
        (
            KeyInput {
                key: Key::Char('7'),
                mods: Mods::default(),
                action: KeyAction::Press,
                text: Some('7'),
            },
            "key 7 Digit7 55",
        ),
        (
            KeyInput {
                key: Key::Function(5),
                mods: Mods::default(),
                action: KeyAction::Press,
                text: None,
            },
            "key F5 F5 116",
        ),
    ];

    for (input, expected) in cases {
        client
            .call(
                "Runtime.evaluate",
                Json::object(vec![
                    ("expression", Json::string("document.title='waiting'")),
                    ("returnByValue", Json::Bool(true)),
                ]),
            )
            .expect("the title is reset");
        let params = keys::dispatch(input).expect("a key with a name");
        client
            .call("Input.dispatchKeyEvent", params)
            .expect("the key is dispatched");
        let seen = wait_for_title(&mut client, "key ", Duration::from_secs(5));
        assert_eq!(&seen, expected, "for {input:?}");
    }

    client.close();
    engine.kill();
}

/// A click lands where the terminal said it did.
#[test]
fn a_click_lands_where_the_cell_was() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);

    // Cell column 11, row 4 of a pane whose first row is the status line: the
    // middle of that cell, one row up.
    let report = blinkterm::input::MouseInput {
        kind: blinkterm::input::MouseKind::Press,
        button: Some(0),
        mods: Mods::default(),
        x: 11,
        y: 4,
        wheel: (0, 0),
    };
    let (x, y) = blinkterm::input::page_point(&report, false, CELL, 1);
    assert_eq!((x, y), (84, 40));

    client
        .call(
            "Input.dispatchMouseEvent",
            Json::object(vec![
                ("type", Json::string("mousePressed")),
                ("x", Json::number(x)),
                ("y", Json::number(y)),
                ("button", Json::string("left")),
                ("buttons", Json::number(1)),
                ("clickCount", Json::number(1)),
                ("modifiers", Json::number(0)),
            ]),
        )
        .expect("the click is dispatched");

    let seen = wait_for_title(&mut client, "click ", Duration::from_secs(5));
    assert_eq!(seen, format!("click 0 {x} {y}"));

    client.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// The clipboard
// ---------------------------------------------------------------------------

/// A form with a textarea and a one-line input, a paragraph to select, and a
/// log of every key and every submission, which is what a paste must not
/// cause.
const FORM: &str = "data:text/html,<body style='margin:0'>\
<form id=f><textarea id=t></textarea><input id=i></form>\
<p id=p style='font:16px monospace'>Some paragraph text to select, and more after it.</p>\
<script>window.log=[];\
f.addEventListener('submit',function(e){e.preventDefault();log.push('submit')});\
document.addEventListener('keydown',function(){log.push('keydown')});\
document.title='ready';</script></body>";

/// Evaluate an expression in the page and hand back its value.
fn evaluate(client: &mut Client, expression: &str) -> Json {
    client
        .call_within(
            "Runtime.evaluate",
            Json::object(vec![
                ("expression", Json::string(expression)),
                ("returnByValue", Json::Bool(true)),
            ]),
            Duration::from_secs(5),
        )
        .ok()
        .and_then(|reply| reply.path(&["result", "value"]).cloned())
        .unwrap_or(Json::Null)
}

fn form(client: &mut Client) {
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(FORM))]),
        )
        .expect("the page loads");
    assert_eq!(
        wait_for_title(client, "ready", Duration::from_secs(10)),
        "ready"
    );
}

/// A paste is one `Input.insertText`, exactly as the loop sends it, and the
/// engine takes it as text: kept whole in a textarea, one line in an input,
/// and not a key or a submission in either.
#[test]
fn a_multi_line_paste_lands_in_a_textarea_and_submits_nothing() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    form(&mut client);

    let pasted = "line one\nline two\n\nline four\ttabbed";
    evaluate(&mut client, "t.focus()");
    client
        .call("Input.insertText", keys::insert_text(pasted))
        .expect("the paste is sent");
    assert_eq!(evaluate(&mut client, "t.value").as_str(), Some(pasted));

    // tOS sends every newline as `\r`, and Kitty what the clipboard held; the
    // engine makes them all `\n`.
    evaluate(&mut client, "t.value=''");
    client
        .call("Input.insertText", keys::insert_text("a\r\nb\rc"))
        .expect("the paste is sent");
    assert_eq!(evaluate(&mut client, "t.value").as_str(), Some("a\nb\nc"));

    evaluate(&mut client, "i.focus()");
    client
        .call("Input.insertText", keys::insert_text("first\nsecond\n"))
        .expect("the paste is sent");
    // The input's value is read after whatever the paste might have queued.
    assert_eq!(
        evaluate(&mut client, "i.value").as_str(),
        Some("first second")
    );
    assert_eq!(
        evaluate(&mut client, "log.join(',')").as_str(),
        Some(""),
        "a paste fired no key and submitted nothing"
    );

    // The control: an Enter key into the same input does both, so the log
    // was listening. With its `\r`, which is what makes the engine run the
    // form's implicit submission; a `keyDown` without text is only a keydown.
    let enter = KeyInput {
        key: Key::Enter,
        mods: Mods::default(),
        action: KeyAction::Press,
        text: Some('\r'),
    };
    client
        .call(
            "Input.dispatchKeyEvent",
            keys::dispatch(&enter).expect("enter has a name"),
        )
        .expect("the key is sent");
    assert_eq!(
        evaluate(&mut client, "log.join(',')").as_str(),
        Some("keydown,submit")
    );

    client.close();
    engine.kill();
}

/// What the person dragged over is what `alt+c` asks the page for, and the
/// bytes it becomes are read by a terminal as that text on its clipboard.
#[test]
fn a_selection_copies_out_as_the_bytes_a_terminal_reads_as_a_clipboard() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    form(&mut client);

    let number = |json: Json| json.as_f64().expect("a number");
    let top = number(evaluate(&mut client, "p.getBoundingClientRect().top"));
    let left = number(evaluate(&mut client, "p.getBoundingClientRect().left"));
    let (y, from) = (top + 8.0, left + 1.0);
    for (kind, x, buttons) in [
        ("mousePressed", from, 1),
        ("mouseMoved", from + 60.0, 1),
        ("mouseMoved", from + 120.0, 1),
        ("mouseReleased", from + 120.0, 0),
    ] {
        client
            .call(
                "Input.dispatchMouseEvent",
                Json::object(vec![
                    ("type", Json::string(kind)),
                    ("x", Json::number(x)),
                    ("y", Json::number(y)),
                    ("button", Json::string("left")),
                    ("buttons", Json::number(buttons)),
                    ("clickCount", Json::number(1)),
                    ("modifiers", Json::number(0)),
                ]),
            )
            .expect("the drag is sent");
    }
    let reply = client
        .call_within(
            "Runtime.evaluate",
            blinkterm::clipboard::selection_params(),
            Duration::from_secs(5),
        )
        .expect("the page answers");
    let selected = blinkterm::clipboard::selection(&reply).expect("a string");
    assert!(
        selected.starts_with("Some para") && selected.len() < 30,
        "{selected:?}"
    );

    let mut terminal = tos_term::Terminal::new(80, 24, tos_term::TerminalConfig::default());
    terminal.advance(&blinkterm::clipboard::osc52(&selected).expect("under the limit"));
    let stored: Vec<_> = terminal
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            tos_term::TermEvent::ClipboardStore { selection, data } => Some((selection, data)),
            _ => None,
        })
        .collect();
    assert_eq!(stored, vec![('c', selected.into_bytes())]);

    // A range selected inside a textarea is the same question's answer.
    evaluate(
        &mut client,
        "getSelection().removeAllRanges();t.value='alpha beta gamma';t.focus();\
         t.setSelectionRange(6,10)",
    );
    let reply = client
        .call_within(
            "Runtime.evaluate",
            blinkterm::clipboard::selection_params(),
            Duration::from_secs(5),
        )
        .expect("the page answers");
    assert_eq!(
        blinkterm::clipboard::selection(&reply).as_deref(),
        Some("beta")
    );

    client.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------

/// A page with a link that asks for a window of its own, and the page behind
/// it.
///
/// Served over HTTP rather than handed over as a `data:` url like [`PAGE`]. A
/// data: url is an opaque origin and Chromium refuses a top-level navigation
/// to one, so a link out of a data: page into a new tab would fail for a
/// reason that has nothing to do with this crate. The link is positioned and
/// sized so that a click at a known point lands on it without the test having
/// to ask the page where anything is.
const FIRST_PAGE: &str = "<!doctype html><body style='margin:0;background:#fff'>\
<a id=l href='/second' target=_blank \
style='position:absolute;left:0;top:0;width:240px;height:80px;background:#cc3'>open</a>\
<script>document.title='first'</script></body>";

const SECOND_PAGE: &str = "<!doctype html><body style='margin:0;background:#39c'>\
<script>document.title='second'</script></body>";

/// Serve those two pages on a port of the kernel's choosing, for as long as
/// the test binary runs.
fn serve() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to serve on");
    let address = listener.local_addr().expect("an address");
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut head = [0u8; 2048];
            let read = stream.read(&mut head).unwrap_or(0);
            let request = String::from_utf8_lossy(&head[..read]).to_string();
            let body = if request.starts_with("GET /second") {
                SECOND_PAGE
            } else {
                FIRST_PAGE
            };
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(answer.as_bytes());
        }
    });
    format!("http://{address}/")
}

/// The viewport the program would set, on whichever tab is being driven.
fn viewport(client: &mut Client) {
    client
        .call(
            "Emulation.setDeviceMetricsOverride",
            Json::object(vec![
                ("width", Json::number(WIDTH)),
                ("height", Json::number(HEIGHT)),
                ("deviceScaleFactor", Json::number(1)),
                ("mobile", Json::Bool(false)),
            ]),
        )
        .expect("the viewport");
}

/// The browser-level connection, with target discovery on, and the tab list
/// the program would be holding.
fn tabbed(engine: &Engine, page: Client, target: String) -> (Client, Tabs<Client>) {
    let mut browser = engine.browser().expect("the browser's client");
    browser
        .call(
            "Target.setDiscoverTargets",
            Json::object(vec![("discover", Json::Bool(true))]),
        )
        .expect("discovery");
    (browser, Tabs::new(Tab::new(target, page, "about:blank")))
}

/// Feed what the browser connection has said into the tab list until `done` is
/// satisfied or the time is up: what `app::handle_target_events` does, with
/// the drawing left out.
fn pump(
    browser: &mut Client,
    tabs: &mut Tabs<Client>,
    timeout: Duration,
    done: impl Fn(&Tabs<Client>) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        for event in browser.events() {
            let outcome = tabs.take(&event, |target| {
                browser.attach(target, Duration::from_secs(5))
            });
            match outcome {
                Outcome::Failed(why) => panic!("a tab that would not open: {why}"),
                Outcome::Gone { mut tab, why } => {
                    if let Some(why) = why {
                        eprintln!("a tab went: {why}");
                    }
                    tab.connection.close();
                }
                _ => {}
            }
        }
        if done(tabs) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// An engine with two tabs in it: the second opened by a click on a
/// `target=_blank` link in the first, which is how a person opens one.
fn two_tabs() -> Option<(Engine, Client, Tabs<Client>)> {
    let (engine, page, target) = connect_with_target()?;
    let base = serve();
    let (mut browser, mut tabs) = tabbed(&engine, page, target);

    {
        let first = tabs.active_mut().expect("the first tab");
        first
            .connection
            .call("Page.enable", Json::empty())
            .expect("Page.enable");
        viewport(&mut first.connection);
        first
            .connection
            .call(
                "Page.navigate",
                Json::object(vec![("url", Json::string(&base))]),
            )
            .expect("the page loads");
        assert_eq!(
            wait_for_title(&mut first.connection, "first", Duration::from_secs(10)),
            "first"
        );

        // A click on the link, dispatched the way a terminal's mouse report
        // would be: press and release at a point inside it.
        for (kind, buttons) in [("mousePressed", 1), ("mouseReleased", 0)] {
            first
                .connection
                .call(
                    "Input.dispatchMouseEvent",
                    Json::object(vec![
                        ("type", Json::string(kind)),
                        ("x", Json::number(40)),
                        ("y", Json::number(20)),
                        ("button", Json::string("left")),
                        ("buttons", Json::number(buttons)),
                        ("clickCount", Json::number(1)),
                        ("modifiers", Json::number(0)),
                    ]),
                )
                .expect("the click is dispatched");
        }
    }

    assert!(
        pump(
            &mut browser,
            &mut tabs,
            Duration::from_secs(15),
            |tabs| tabs.len() == 2
        ),
        "the link with target=_blank opened no tab"
    );
    Some((engine, browser, tabs))
}

/// The whole reason tabs exist: a link that wants a window gets a tab, that
/// tab is the one in front, and it is the one painting.
#[test]
fn a_link_that_wants_a_window_becomes_the_tab_in_front() {
    let Some((mut engine, mut browser, mut tabs)) = two_tabs() else {
        return;
    };
    assert_eq!(tabs.len(), 2);
    assert_eq!(
        tabs.active_index(),
        1,
        "a tab the person opened is the one they are taken to"
    );

    // The urls come from the browser connection, with nothing asked of either
    // page — and the second tab's is the one the link pointed at.
    assert!(
        pump(&mut browser, &mut tabs, Duration::from_secs(10), |tabs| {
            tabs.iter()
                .nth(1)
                .is_some_and(|tab| tab.url.ends_with("/second"))
        }),
        "the second tab's url never arrived: {:?}",
        tabs.iter().map(|tab| tab.url.clone()).collect::<Vec<_>>()
    );

    // The titles come from the pages, which is the only place they are right:
    // the browser connection would have said "127.0.0.1:NNNN/second" here.
    let mut titles = Vec::new();
    for index in 0..tabs.len() {
        let tab = tabs.get_mut(index).expect("a tab");
        titles.push(
            blinkterm::app::page_title(&mut tab.connection).unwrap_or_else(|| "?".to_string()),
        );
    }
    assert_eq!(titles, ["first", "second"]);

    // And it paints: raised, sized, cast, and a PNG comes out of it.
    let target = tabs.active_target().expect("a target").to_string();
    browser
        .call(
            "Target.activateTarget",
            Json::object(vec![("targetId", Json::string(&target))]),
        )
        .expect("the tab is raised");
    let second = tabs.active_mut().expect("the second tab");
    second
        .connection
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut second.connection);
    second
        .connection
        .call(
            "Page.startScreencast",
            Json::object(vec![
                ("format", Json::string("png")),
                ("maxWidth", Json::number(WIDTH)),
                ("maxHeight", Json::number(HEIGHT)),
                ("everyNthFrame", Json::number(1)),
            ]),
        )
        .expect("the screencast starts");
    let png = wait_for_frame(&mut second.connection, Duration::from_secs(10))
        .expect("the tab in front paints");
    assert_eq!(&png[..4], b"\x89PNG");

    browser.close();
    drop(tabs);
    engine.kill();
}

/// `ctrl+t`: a target this program asked for, attached to by the id the
/// engine gave back, and not announced twice as a tab.
#[test]
fn a_tab_this_program_opens_is_reachable_and_counted_once() {
    let Some((mut engine, page, target)) = connect_with_target() else {
        return;
    };
    let base = serve();
    let (mut browser, mut tabs) = tabbed(&engine, page, target);

    let created = browser
        .call(
            "Target.createTarget",
            Json::object(vec![("url", Json::string("about:blank"))]),
        )
        .expect("a new target");
    let opened = created
        .get("targetId")
        .and_then(Json::as_str)
        .expect("the engine says which")
        .to_string();
    let connection = browser
        .attach(&opened, Duration::from_secs(10))
        .expect("a session on the page the engine opened");
    tabs.open(Tab::new(opened, connection, "about:blank"));
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs.active_index(), 1);

    // The `Target.targetCreated` for it has no opener, so the list must not
    // take it as a second tab for the same page.
    assert!(!pump(
        &mut browser,
        &mut tabs,
        Duration::from_secs(3),
        |tabs| tabs.len() > 2,
    ));
    assert_eq!(
        tabs.len(),
        2,
        "the tab this program opened was counted twice"
    );

    // And it drives like any other tab.
    let tab = tabs.active_mut().expect("the new tab");
    tab.connection
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    tab.connection
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(&base))]),
        )
        .expect("it navigates");
    assert_eq!(
        wait_for_title(&mut tab.connection, "first", Duration::from_secs(10)),
        "first"
    );
    let mut gone = tabs.close(1).expect("the tab");
    gone.connection.close();

    browser.close();
    drop(tabs);
    engine.kill();
}

/// `ctrl+w`: the engine closes the page, the list loses the tab, and what is
/// left is the tab it was opened from.
#[test]
fn closing_a_tab_leaves_the_one_it_was_opened_from() {
    let Some((mut engine, mut browser, mut tabs)) = two_tabs() else {
        return;
    };
    let closing = tabs.active_target().expect("a target").to_string();

    // What `Command::CloseTab` does: the target in the engine, then the
    // session.
    let index = tabs.active_index();
    let mut tab = tabs.close(index).expect("the tab");
    browser
        .call(
            "Target.closeTarget",
            Json::object(vec![("targetId", Json::string(&closing))]),
        )
        .expect("the target closes");
    tab.connection.close();

    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs.active_index(), 0);
    // And the engine agrees. Its `targetDestroyed` arrives for a tab that has
    // already gone, which must not be an error or a second tab lost.
    assert!(
        pump(
            &mut browser,
            &mut tabs,
            Duration::from_secs(10),
            |tabs| tabs.len() == 1,
        ),
        "the engine's own news about the closed target upset the list"
    );
    let left = tabs.active_mut().expect("the tab that is left");
    assert_eq!(
        blinkterm::app::page_title(&mut left.connection).as_deref(),
        Some("first"),
        "what is left is not the page the link was on"
    );

    browser.close();
    drop(tabs);
    engine.kill();
}

/// A page that closes itself takes its tab with it, with no key pressed.
#[test]
fn a_page_that_calls_window_close_removes_its_own_tab() {
    let Some((mut engine, mut browser, mut tabs)) = two_tabs() else {
        return;
    };
    let closing = tabs.active_target().expect("a target").to_string();
    {
        let second = tabs.active_mut().expect("the second tab");
        // A page may close a window that was opened by script, which is what
        // the link with target=_blank made this one. `notify` rather than
        // `call`: the reply to an evaluation that closes the page never comes.
        let _ = second.connection.notify(
            "Runtime.evaluate",
            Json::object(vec![("expression", Json::string("window.close()"))]),
        );
    }

    assert!(
        pump(
            &mut browser,
            &mut tabs,
            Duration::from_secs(10),
            |tabs| tabs.len() == 1,
        ),
        "window.close() left the tab where it was"
    );
    assert_eq!(tabs.index_of(&closing), None);
    assert_eq!(tabs.active_index(), 0);

    browser.close();
    drop(tabs);
    engine.kill();
}

/// The next screencast frame, decoded, or nothing within the time.
fn wait_for_frame(client: &mut Client, timeout: Duration) -> Option<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for event in client.events() {
            if event.method != "Page.screencastFrame" {
                continue;
            }
            if let Some(session) = event.params.get("sessionId").and_then(Json::as_i64) {
                let _ = client.notify(
                    "Page.screencastFrameAck",
                    Json::object(vec![("sessionId", Json::number(session as f64))]),
                );
            }
            if let Some(data) = event.params.get("data").and_then(Json::as_str) {
                if let Ok(png) = blinkterm::base64::decode(data.as_bytes()) {
                    return Some(png);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}
/// The engine is a wrapper, a browser and a handful of helpers, and stopping
/// it has to be the end of all of them.
///
/// On Debian — a tOS rootfs is Debian — `/usr/bin/chromium-shell` is a shell
/// script that runs `/usr/lib/chromium/chromium-shell` as its child, so the
/// pid `spawn` returns is `/bin/sh` and a signal to that pid alone leaves a
/// browser behind with the page still painting — and, when this program still
/// drove it over a port, the debugging port still open. That is what was found on an installed machine: seven sessions, seven
/// engines, none of them being looked at. This test is the shape of that bug —
/// the group is read before the engine is dropped, and has to be empty after.
#[test]
fn killing_the_engine_leaves_nothing_of_its_process_group() {
    let Some((engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);
    let group = engine
        .group()
        .expect("the engine is started in a group of its own");
    assert_ne!(group, own_group(), "the engine's group is not this test's");

    let before = group_members(group);
    assert!(
        before.len() >= 2,
        "an engine is a wrapper and a browser at least, and this group has {}",
        describe(&before)
    );
    eprintln!("group {group}: {}", describe(&before));

    drop(client);
    drop(engine);

    // Two seconds: the polite stop inside `Engine::kill` is allowed half of
    // one, and the SIGKILL after it is not something a process can be slow
    // about.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut left = group_members(group);
    while !left.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        left = group_members(group);
    }
    assert!(
        left.is_empty(),
        "the engine was killed and these are still running: {}",
        describe(&left)
    );
}

/// Issue #5: the engine's debugging endpoint was a port on loopback, and any
/// process on the machine could drive the browser through it. With
/// `--remote-debugging-pipe` there must be nothing listening at all — not the
/// browser, and not any of the helpers it forks.
///
/// Asked of the kernel rather than of a connect: every socket descriptor every
/// process in the engine's group holds, against every TCP socket in the
/// `LISTEN` state (`0A` in `/proc/net/tcp`). A machine with no IPv6 has no
/// `/proc/net/tcp6`, and that is read as no listeners rather than an error.
#[test]
fn the_engine_listens_on_no_port() {
    let Some((engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);
    let group = engine
        .group()
        .expect("the engine is started in a group of its own");
    let members = group_members(group);
    assert!(
        members.len() >= 2,
        "an engine is a wrapper and a browser at least, and this group has {}",
        describe(&members)
    );

    let listening: std::collections::HashSet<u64> = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .flat_map(|table| listening_inodes(table))
        .collect();
    let mut sockets = 0;
    let mut found = Vec::new();
    for (pid, command) in &members {
        for inode in socket_inodes(*pid) {
            sockets += 1;
            if listening.contains(&inode) {
                found.push(format!("{pid} ({command}) listens on socket:[{inode}]"));
            }
        }
    }
    eprintln!(
        "group {group}: {} processes, {sockets} sockets between them, {} listening sockets \
         on the machine, {} of them the engine's",
        members.len(),
        listening.len(),
        found.len()
    );
    assert!(found.is_empty(), "the engine is listening: {found:?}");

    drop(client);
    drop(engine);
}

/// The inodes of the sockets in `LISTEN` in one of `/proc/net/tcp{,6}`.
fn listening_inodes(table: &str) -> Vec<u64> {
    let Ok(text) = std::fs::read_to_string(table) else {
        return Vec::new();
    };
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // sl, local, remote, st, queues, tr, retrnsmt, uid, timeout, inode
            (fields.get(3) == Some(&"0A"))
                .then(|| fields.get(9)?.parse().ok())
                .flatten()
        })
        .collect()
}

/// The inodes of every socket `pid` holds a descriptor to.
fn socket_inodes(pid: i32) -> Vec<u64> {
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let target = std::fs::read_link(entry.path()).ok()?;
            let target = target.to_string_lossy();
            target
                .strip_prefix("socket:[")?
                .strip_suffix(']')?
                .parse()
                .ok()
        })
        .collect()
}

/// Every process in `group` that is still running, as pid and command line.
///
/// A process that has exited and not yet been waited for is still in the
/// group as far as the kernel is concerned, and is not what this is looking
/// for: the browser this test is about reparents to init, which reaps it in
/// its own time. So state `Z` is not a member here.
fn group_members(group: i32) -> Vec<(i32, String)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Some((state, pgrp)) = state_and_group(&entry.path()) else {
            continue;
        };
        if pgrp == group && state != 'Z' {
            found.push((pid, command_of(&entry.path())));
        }
    }
    found.sort();
    found
}

/// The group this test process is in, read the same way as anybody else's.
fn own_group() -> i32 {
    state_and_group(std::path::Path::new("/proc/self"))
        .expect("this process has a /proc entry")
        .1
}

/// The run state and process group out of `/proc/<pid>/stat`.
///
/// The second field is the command in brackets and may contain spaces and
/// brackets of its own, so the fields are counted from the last `)` rather
/// than from the start of the line.
fn state_and_group(dir: &std::path::Path) -> Option<(char, i32)> {
    let text = std::fs::read_to_string(dir.join("stat")).ok()?;
    let after_command = &text[text.rfind(')')? + 1..];
    let mut fields = after_command.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let _parent = fields.next()?;
    let group = fields.next()?.parse().ok()?;
    Some((state, group))
}

/// What a process was started as, short enough to put in a failure.
fn command_of(dir: &std::path::Path) -> String {
    let Ok(raw) = std::fs::read(dir.join("cmdline")) else {
        return String::from("(gone)");
    };
    let line = String::from_utf8_lossy(&raw).replace('\0', " ");
    let line = line.trim().to_string();
    match line.char_indices().nth(90) {
        Some((at, _)) => format!("{}...", &line[..at]),
        None => line,
    }
}

fn describe(members: &[(i32, String)]) -> String {
    members
        .iter()
        .map(|(pid, command)| format!("{pid} {command}"))
        .collect::<Vec<_>>()
        .join("; ")

    // ---------------------------------------------------------------------------
    // The format the frames go in
    // ---------------------------------------------------------------------------
}

/// A page the shape of a real article, which is what the format tests need.
///
/// [`PAGE`] is a screenful of monospace text and one moving block, which is
/// right for measuring a frame path and wrong for measuring a format: it is
/// almost all sharp black-on-white edges, which is the worst case a JPEG ever
/// meets, and at 640x360 it is dense enough to come out at 28 dB — a number
/// about that page rather than about quality 85. So this is prose at a
/// reading size, in a column, with a picture beside it and links in it, which
/// is what the measurement in `docs/design/browser.md` was taken on.
///
/// It is still until `f()` is called, and then it scrolls, which is the two
/// things the two tests below want: a frame that can be captured twice and
/// get the same picture, and a viewport that changes completely every frame.
const ARTICLE: &str = "data:text/html,\
<body style='margin:0;font:13px/17px serif;background:%23fff;color:%23202122'>\
<div style='position:absolute;right:20px;top:60px;width:320px;height:240px;\
background:linear-gradient(135deg,%23c33,%233c3,%2333c,%23fc0)'></div>\
<div id=t style='padding:8px 340px 8px 16px'></div><script>\
var w='the quick brown fox jumps over a lazy dog while Blink lays out a page of \
prose and the encoder works out what it costs to send'.split(' ');\
var rows=[];for(var i=0;i<400;i++){var s=[];for(var j=0;j<28;j++){\
s.push(w[(i*7+j*3)%25w.length])}\
rows.push('<p style=margin:4px>'+i+' '+s.join(' ')+' <a href=%23 \
style=color:%230645ad>a link</a></p>')}\
document.getElementById('t').innerHTML=rows.join('');\
document.title='article';\
var n=0;function f(){n=(n+4)%252000;scrollTo(0,n);requestAnimationFrame(f)}\
</script></body>";

/// Load [`ARTICLE`] at `width` by `height` and wait for it to be there.
fn article(client: &mut Client, width: u32, height: u32) {
    client
        .call(
            "Emulation.setDeviceMetricsOverride",
            Json::object(vec![
                ("width", Json::number(width)),
                ("height", Json::number(height)),
                ("deviceScaleFactor", Json::number(1)),
                ("mobile", Json::Bool(false)),
            ]),
        )
        .expect("a pane-sized viewport");
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(ARTICLE))]),
        )
        .expect("the article loads");
    assert_eq!(
        wait_for_title(client, "article", Duration::from_secs(15)),
        "article"
    );
}

/// One `Page.captureScreenshot`, in whichever format.
fn screenshot(client: &mut Client, format: &str, quality: Option<u32>) -> Vec<u8> {
    let mut fields = vec![("format", Json::string(format))];
    if let Some(quality) = quality {
        fields.push(("quality", Json::number(quality)));
    }
    let answer = client
        .call("Page.captureScreenshot", Json::object(fields))
        .expect("a screenshot");
    let data = answer
        .get("data")
        .and_then(Json::as_str)
        .expect("a screenshot carries its picture");
    blinkterm::base64::decode(data.as_bytes()).expect("valid base64")
}

/// What quality 85 costs, against the lossless picture of the same frame.
///
/// This is the number `docs/design/browser.md`'s JPEG section rests on: the
/// frames are only allowed to be lossy while the page is moving, and how
/// lossy is a thing to measure rather than to trust. 35 dB is the floor and
/// the measured figure on this page is 37; anything near the floor means
/// either the encoder's defaults moved or `tos_term::jpeg` has a bug the
/// fixtures did not catch.
///
/// Read the floor as being about this page. A screenful of small monospace
/// text is 28 dB at the same quality and is not a worse decoder or a worse
/// encoder — it is what 4:2:0 and a quantisation table do to sharp edges, and
/// it is exactly the case the motion-and-rest policy exists to keep off the
/// screen while somebody is reading.
#[test]
fn a_jpeg_frame_at_quality_85_is_the_png_of_the_same_frame() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);
    article(&mut client, WIDE, TALL);
    // A frame after the load event is not necessarily a frame that has been
    // painted; one screenshot forces one.
    let _ = screenshot(&mut client, "png", None);

    let png = screenshot(&mut client, "png", None);
    let jpeg = screenshot(&mut client, "jpeg", Some(motion::QUALITY));
    assert_eq!(&png[..4], b"\x89PNG");
    assert_eq!(&jpeg[..2], b"\xff\xd8");

    let lossless = tos_term::png::decode(&png, 64 * 1024 * 1024).expect("the PNG decodes");
    let at = Instant::now();
    let lossy = tos_term::jpeg::decode(&jpeg, 64 * 1024 * 1024).expect("the JPEG decodes");
    let decoding = at.elapsed();
    assert_eq!(
        (lossy.width, lossy.height),
        (lossless.width, lossless.height)
    );

    // Mean squared error over every channel of every pixel, and the
    // peak-signal-to-noise ratio that is the usual way of saying it.
    let mut squares = 0f64;
    let mut samples = 0usize;
    for (rgba, rgb) in lossless.rgba.chunks_exact(4).zip(lossy.rgb.chunks_exact(3)) {
        for channel in 0..3 {
            let error = rgba[channel] as f64 - rgb[channel] as f64;
            squares += error * error;
            samples += 1;
        }
    }
    let mse = squares / samples as f64;
    let psnr = if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    };
    eprintln!(
        "quality {} at {}x{}: {psnr:.1} dB, {} kB of JPEG against {} kB of PNG, \
         decoded in {decoding:?}",
        motion::QUALITY,
        lossy.width,
        lossy.height,
        jpeg.len() / 1024,
        png.len() / 1024,
    );
    assert!(
        psnr >= 35.0,
        "quality {} came out at {psnr:.1} dB",
        motion::QUALITY
    );

    client.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// What the branch is for: frames a second, at the size a pane actually is
// ---------------------------------------------------------------------------

/// A pane's worth of page: 160 by 48 cells on the 8x16 face, with a row left
/// over for the status line. The measurement that chose JPEG was taken at
/// 1280x770; 768 is the same pane rounded to a whole number of cells, which
/// is the only height a pane can actually have.
const WIDE: u32 = 1280;
const TALL: u32 = 768;

/// The frame path as it was before this branch: the encoded file itself,
/// named in `/dev/shm`, decoded by the terminal on its parse loop.
///
/// Built here rather than kept in `graphics.rs`, because the program no
/// longer has this path and a dead branch kept alive for a benchmark is a
/// branch that rots. It is byte for byte what `Painter::frame` used to emit.
fn png_through_the_terminal(
    dir: &std::path::Path,
    counter: &mut u64,
    png: &[u8],
    cells: Cells,
) -> Vec<u8> {
    *counter += 1;
    let name = format!("blinkterm-before-{}-{counter}", std::process::id());
    let partial = dir.join(format!("{name}.part"));
    std::fs::write(&partial, png).expect("a frame file");
    std::fs::rename(&partial, dir.join(&name)).expect("renamed into place");
    let control = format!(
        "a=T,f=100,i={IMAGE_ID},p=1,c={},r={},C=1,q=2",
        cells.cols, cells.rows
    );
    let payload = blinkterm::base64::encode(format!("/{name}").as_bytes());
    format!("\x1b[2;1H\x1b_G{control},t=s;{payload}\x1b\\").into_bytes()
}

/// What the frame path costs, before and after, at the size a pane is.
///
/// "Before" is a PNG screencast handed to the terminal as a file, which is
/// what `main` did until this branch: the engine encodes a PNG, the terminal
/// decodes it on the thread that parses escape sequences. "After" is a JPEG
/// screencast at quality 85 decoded in this process and handed over as raw
/// pixels. Both run against the same page, scrolling, at the same size,
/// through the same `/dev/shm` transport and the same terminal, for the same
/// three seconds.
///
/// **What is asserted is the terminal's cost, not the frame rate**, and the
/// reason is worth knowing. `docs/design/browser.md` records 33.8 fps for PNG
/// against 57.8 for JPEG, measured on a slower machine against a real
/// ja.wikipedia page; on a fast host with `--cpus=2` and this page the
/// engine's PNG encoder keeps up and both formats arrive at very nearly 60,
/// so a frame-rate assertion here would be asserting something about the
/// machine. The cost this branch actually controls is the compositor's: a PNG
/// decode on the parse loop against a copy out of tmpfs, which is the same
/// several-fold difference whatever the engine manages. Both numbers are
/// printed; only the one that is a property of the code is asserted.
#[test]
fn raw_pixels_cost_the_terminal_a_fraction_of_what_a_png_frame_did() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);
    article(&mut client, WIDE, TALL);
    client
        .call(
            "Runtime.evaluate",
            Json::object(vec![("expression", Json::string("f()"))]),
        )
        .expect("the page starts scrolling");

    let cells = Cells {
        cols: WIDE / CELL.0,
        rows: TALL / CELL.1,
    };
    let run_for = Duration::from_secs(3);

    // Before: PNG, decoded by the terminal on its parse loop.
    let before_dir = temp_dir("before");
    let mut terminal = sized_terminal(&before_dir, WIDE, TALL);
    let mut counter = 0u64;
    cast(&mut client, "png", None, WIDE, TALL);
    let started = Instant::now();
    let (mut before_frames, mut before_bytes) = (0usize, 0usize);
    let mut before_terminal = Duration::ZERO;
    while started.elapsed() < run_for {
        for (png, _) in take_frames(&mut client) {
            let sequence = png_through_the_terminal(&before_dir, &mut counter, &png, cells);
            let at = Instant::now();
            terminal.advance(&sequence);
            before_terminal += at.elapsed();
            before_frames += 1;
            before_bytes += png.len();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let before_seconds = started.elapsed().as_secs_f64();
    let _ = client.call("Page.stopScreencast", Json::empty());
    // `Page.stopScreencast` returns before the last frames do, and the ones
    // still coming are in the old format. Only a test that changes format
    // mid-session meets this — the program picks one at `Page.enable` and
    // keeps it — but here it is the difference between a measurement and a
    // panic, so the queue is drained until it stays empty.
    let give_up = Instant::now() + Duration::from_secs(2);
    let mut quiet_since = Instant::now();
    while Instant::now() < give_up && quiet_since.elapsed() < Duration::from_millis(300) {
        if !take_frames(&mut client).is_empty() {
            quiet_since = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(before_frames > 5, "only {before_frames} PNG frames");
    assert_eq!(
        terminal
            .graphics()
            .image(IMAGE_ID)
            .map(|i| (i.width, i.height)),
        Some((WIDE, TALL)),
        "the before path did not put a frame in the store"
    );

    // After: JPEG at quality 85, decoded here, raw pixels over.
    let after_dir = temp_dir("after");
    let mut painter = Painter::at(&after_dir);
    let mut terminal = sized_terminal(&after_dir, WIDE, TALL);
    cast(&mut client, "jpeg", Some(motion::QUALITY), WIDE, TALL);
    let started = Instant::now();
    let (mut after_frames, mut after_bytes) = (0usize, 0usize);
    let (mut decoding, mut after_terminal) = (Duration::ZERO, Duration::ZERO);
    let mut stragglers = 0usize;
    while started.elapsed() < run_for {
        for (jpeg, _) in take_frames(&mut client) {
            if jpeg.get(..2) != Some(b"\xff\xd8") {
                // A PNG from the pass above that outlived the drain. Counted
                // rather than ignored, because a lot of them would mean the
                // measurement below is of the wrong thing.
                stragglers += 1;
                continue;
            }
            let at = Instant::now();
            let image = tos_term::jpeg::decode(&jpeg, 64 * 1024 * 1024).expect("a frame decodes");
            decoding += at.elapsed();
            let raw = Raw::rgb(&image.rgb, image.width, image.height);
            let sequence = painter.frame(raw, cells, 2, 1);
            let at = Instant::now();
            terminal.advance(&sequence);
            after_terminal += at.elapsed();
            after_frames += 1;
            after_bytes += jpeg.len();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let after_seconds = started.elapsed().as_secs_f64();
    let _ = client.call("Page.stopScreencast", Json::empty());
    assert!(after_frames > 5, "only {after_frames} JPEG frames");
    assert!(
        stragglers < after_frames / 10,
        "{stragglers} frames of the old format against {after_frames} of the new"
    );
    assert_eq!(
        terminal
            .graphics()
            .image(IMAGE_ID)
            .map(|i| (i.width, i.height)),
        Some((WIDE, TALL)),
        "the after path did not put a frame in the store"
    );

    let before_fps = before_frames as f64 / before_seconds;
    let after_fps = after_frames as f64 / after_seconds;
    let before_each = before_terminal / before_frames as u32;
    let after_each = after_terminal / after_frames as u32;
    eprintln!(
        "{WIDE}x{TALL} end to end:\n  \
         before  png  decoded by the terminal: {before_fps:.1} fps, \
         {} kB a frame, {before_each:?} in the terminal\n  \
         after   jpeg q{} decoded here:        {after_fps:.1} fps, \
         {} kB a frame, {:?} decoding, {after_each:?} in the terminal\n  \
         the terminal's share is {:.1}x smaller",
        before_bytes / before_frames / 1024,
        motion::QUALITY,
        after_bytes / after_frames / 1024,
        decoding / after_frames as u32,
        before_each.as_secs_f64() / after_each.as_secs_f64().max(f64::EPSILON),
    );

    assert!(
        after_each * 2 < before_each,
        "the terminal spent {after_each:?} a frame on raw pixels against \
         {before_each:?} on a PNG: the decode was supposed to leave the \
         compositor's thread"
    );
    // And the path as a whole keeps up with something worth calling a
    // browser, whichever format the engine is fast enough to manage.
    assert!(after_fps > 25.0, "only {after_fps:.1} fps end to end");

    painter.clean_up();
    std::fs::remove_dir_all(&before_dir).ok();
    std::fs::remove_dir_all(&after_dir).ok();
    client.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// When the lossless still is taken, and what it costs the loop
// ---------------------------------------------------------------------------

/// How far down the page is, asked of the page.
fn scroll_y(client: &mut Client) -> f64 {
    client
        .call_within(
            "Runtime.evaluate",
            Json::object(vec![
                ("expression", Json::string("window.scrollY")),
                ("returnByValue", Json::Bool(true)),
            ]),
            Duration::from_secs(5),
        )
        .ok()
        .and_then(|value| value.path(&["result", "value"]).and_then(Json::as_f64))
        .expect("the page says where it is")
}

/// Where the page was and when it was captured, for every screencast frame
/// that has arrived — without decoding the picture, because these tests are
/// about where the page is rather than what it looks like.
///
/// `Page.screencastFrame` carries `metadata.scrollOffsetY`, which is the
/// number the whole of this section is about: it is what the person sees move.
/// Acknowledges everything it takes, as the program does.
fn take_offsets(client: &mut Client) -> Vec<(f64, Option<f64>)> {
    let mut frames = Vec::new();
    for event in client.events() {
        if event.method != "Page.screencastFrame" {
            continue;
        }
        if let Some(session) = event.params.get("sessionId").and_then(Json::as_i64) {
            let _ = client.notify(
                "Page.screencastFrameAck",
                Json::object(vec![("sessionId", Json::number(session as f64))]),
            );
        }
        let Some(offset) = event
            .params
            .path(&["metadata", "scrollOffsetY"])
            .and_then(Json::as_f64)
        else {
            continue;
        };
        let stamp = event
            .params
            .path(&["metadata", "timestamp"])
            .and_then(Json::as_f64);
        frames.push((offset, stamp));
    }
    frames
}

/// The program's own dispatch, with a count of what went through it.
///
/// `blinkterm::app::Wire` is the one implementation of
/// `scroll::Dispatch` that is not a fake, so the events these tests put on the
/// wire are the ones the program puts on it, built by the same code.
struct Counted {
    wire: blinkterm::app::Wire,
    sent: AtomicUsize,
}

impl Dispatch for Counted {
    fn send(&self, step: Step) -> Result<(), String> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        self.wire.send(step)
    }
}

/// One step of the animation, sent down the middle of the page exactly as the
/// animator thread sends one: one `mouseWheel`, no reply waited for.
fn wheel_step(client: &mut Client, step: Step) {
    client
        .notify(
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
        .expect("the wheel event goes out");
}

/// What one run of the wheel did to the page.
struct Roll {
    /// Every screencast frame between the first notch and the end, as how far
    /// the page moved since the frame before it. A zero is a frame in which
    /// the page stood still, which is the whole of what pulsing looks like.
    frames: Vec<(f64, Instant)>,
    /// When each notch was turned.
    notches: Vec<Instant>,
    /// When the last notch was turned.
    last_notch: Instant,
    /// Every moment `motion` would have asked for a lossless still.
    wanted: Vec<Instant>,
    /// How many `mouseWheel` events the animation cost.
    events: usize,
}

impl Roll {
    /// The frames in which the page actually moved, with the first one's index
    /// and the last one's.
    fn movement(&self) -> (usize, usize) {
        let first = self
            .frames
            .iter()
            .position(|(step, _)| *step != 0.0)
            .expect("the wheel moved the page");
        let last = self
            .frames
            .iter()
            .rposition(|(step, _)| *step != 0.0)
            .expect("the wheel moved the page");
        (first, last)
    }

    /// How long the movement lasted, first moving frame to last.
    fn span(&self) -> Duration {
        let (first, last) = self.movement();
        self.frames[last].1.duration_since(self.frames[first].1)
    }

    /// The longest the page stood still in the middle of the scroll, and how
    /// many frames in a row it did.
    ///
    /// A frame that carries the same offset as the one before it is a frame in
    /// which the page did not move. One of those on its own is the screencast's
    /// cadence beating against the animation's: frames come every 16.7 ms on
    /// this host and ticks every [`blinkterm::scroll::TICK`], so about twice
    /// a second a frame falls in a gap and the next one carries two ticks.
    /// That is a sixtieth of a second, and on the machine this is really for —
    /// where a frame is 24 to 27 ms and every one of them holds a tick or two
    /// — it cannot happen at all. What the person saw and called pulsing is
    /// the page stopping long enough to be a *pause*, which is what this
    /// measures.
    fn longest_stall(&self) -> (Duration, usize) {
        let (first, last) = self.movement();
        let mut longest = Duration::ZERO;
        let mut in_a_row = 0usize;
        let mut worst_row = 0usize;
        let mut moved_at = self.frames[first].1;
        for (step, at) in &self.frames[first + 1..=last] {
            if *step == 0.0 {
                in_a_row += 1;
                worst_row = worst_row.max(in_a_row);
                continue;
            }
            in_a_row = 0;
            longest = longest.max(at.duration_since(moved_at));
            moved_at = *at;
        }
        (longest, worst_row)
    }

    /// The biggest and the smallest a frame moved over the middle of the run,
    /// and the ratio between them — which is what a steady hand's evenness
    /// *is*.
    ///
    /// The middle is from the second notch to the last one: the hand rolling
    /// steadily, with the ramp-up in front of it and the settling behind it
    /// left out, because neither is supposed to look like the middle.
    ///
    /// One frame at each end is forgiven, for the reason [`Roll::longest_stall`]
    /// forgives one: the screencast's cadence beats against the tick's — 16.7
    /// against 16 ms on the host these are measured on — so about twice a
    /// second one frame carries two ticks or none, and that is the cadence
    /// rather than the curve. The raw figures are printed beside the forgiven
    /// ones so that a run which needed the forgiveness says so.
    fn swing(&self) -> (f64, f64, f64) {
        let from = *self.notches.get(1).unwrap_or(&self.last_notch);
        let mut steps: Vec<f64> = self
            .frames
            .iter()
            .filter(|(_, at)| *at >= from && *at <= self.last_notch)
            .map(|(step, _)| step.abs())
            .collect();
        assert!(
            steps.len() > 4,
            "only {} frames in the middle of the run to judge it by",
            steps.len()
        );
        steps.sort_by(|a, b| a.partial_cmp(b).expect("a frame moved by a number"));
        let (low, high) = (steps[1], steps[steps.len() - 2]);
        (high, low, high / low)
    }

    /// The same without the forgiveness: the largest and smallest frame in the
    /// middle, whatever caused them.
    fn raw_swing(&self) -> (f64, f64) {
        let from = *self.notches.get(1).unwrap_or(&self.last_notch);
        let steps = self
            .frames
            .iter()
            .filter(|(_, at)| *at >= from && *at <= self.last_notch)
            .map(|(step, _)| step.abs());
        steps.fold((0.0f64, f64::INFINITY), |(high, low), step| {
            (high.max(step), low.min(step))
        })
    }

    /// How long after the last notch the page finally stopped.
    fn settled_after(&self) -> Duration {
        let (_, last) = self.movement();
        self.frames[last]
            .1
            .saturating_duration_since(self.last_notch)
    }

    /// The step profile, for `--nocapture`.
    fn profile(&self) -> String {
        self.frames
            .iter()
            .map(|(step, _)| {
                if *step == 0.0 {
                    "[0]".to_string()
                } else {
                    format!("{}", step.round() as i64)
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Turn the wheel `notches` times, `apart` milliseconds between them, through
/// the real [`Wheel`] — its thread, its clock, its `mouseWheel` events — with
/// this loop doing exactly what `app::drive` does around it: it adds a notch,
/// reads `Wheel::activity` into the still's clock, and takes whatever frames
/// have arrived.
///
/// Driving the thread rather than ticking an [`Animator`] here is the point of
/// the test. What the person saw on the installed machine was the animation
/// sharing a thread with a loop that spends nine milliseconds decoding a frame
/// and writes 2.9 MB of pixels; these profiles are only worth anything if the
/// ticks are timed the way the program times them.
///
/// It runs until the animation has finished *and* the still policy has asked
/// for a picture, which is `motion::INPUT_QUIET` past the last tick — long
/// enough that every frame the wheel caused is in hand.
fn roll(client: &mut Client, notches: u32, apart: Duration) -> Roll {
    let at = (WIDE as i32 / 2, TALL as i32 / 2);
    let counted = Arc::new(Counted {
        wire: blinkterm::app::Wire::new(client.notifier()),
        sent: AtomicUsize::new(0),
    });
    let wheel = Wheel::start();
    let mut rest = Motion::new(Instant::now());
    let mut sent = 0u32;
    let mut next_notch = Instant::now();
    let mut last_notch = next_notch;
    let mut was = scroll_y(client);
    let mut roll = Roll {
        frames: Vec::new(),
        notches: Vec::new(),
        last_notch,
        wanted: Vec::new(),
        events: 0,
    };

    let give_up = Instant::now() + Duration::from_secs(30);
    while Instant::now() < give_up {
        let now = Instant::now();
        if sent < notches && now >= next_notch {
            wheel.notch(
                "the tab in front",
                counted.clone(),
                at,
                (0.0, blinkterm::app::WHEEL_PIXELS),
            );
            rest.input(now);
            roll.notches.push(now);
            last_notch = now;
            sent += 1;
            next_notch = now + apart;
        }
        if let Some(when) = wheel.activity() {
            rest.input(when);
        }
        for (offset, stamp) in take_offsets(client) {
            let seen = Instant::now();
            rest.motion_frame(stamp, seen);
            roll.frames.push((offset - was, seen));
            was = offset;
        }
        if rest.wants_still(Instant::now()) {
            // Nothing is ever sent for it; what is recorded is that the policy
            // would have, which is what `app::rest_shot` asks every pass.
            roll.wanted.push(Instant::now());
            rest.still_requested();
            rest.still_failed();
        }
        if sent == notches && wheel.owed() == (0.0, 0.0) && !roll.wanted.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(sent, notches, "the notches never all went out");
    roll.last_notch = last_notch;
    roll.events = counted.sent.load(Ordering::SeqCst);
    roll
}

/// A page loaded, painted, casting, and quiet: everything the wheel tests do
/// before they touch the wheel.
fn a_page_to_scroll(client: &mut Client) {
    prepare(client);
    article(client, WIDE, TALL);
    // A page that has loaded is not necessarily a page that has painted; one
    // screenshot forces the first paint, so the frames below are the wheel's.
    let _ = screenshot(client, "png", None);
    cast(client, "jpeg", Some(motion::QUALITY), WIDE, TALL);
    // And the first frames of the screencast are the page arriving rather than
    // the page scrolling.
    std::thread::sleep(Duration::from_millis(400));
    let _ = take_offsets(client);
    assert_eq!(scroll_y(client), 0.0, "the page starts at the top");
}

/// Ask for the lossless still without waiting for it, as the loop does.
fn ask_for_a_still(client: &mut Client) -> Pending {
    client
        .send(
            "Page.captureScreenshot",
            Json::object(vec![("format", Json::string("png"))]),
        )
        .expect("the still goes out")
}

/// The picture out of a still's reply, or nothing if it carried none.
fn still_picture(answer: Result<Json, String>) -> Option<Vec<u8>> {
    answer
        .ok()
        .and_then(|reply| reply.get("data").and_then(Json::as_str).map(str::to_string))
        .and_then(|data| blinkterm::base64::decode(data.as_bytes()).ok())
}

/// Twelve notches, 50 ms apart: a flick, faster than a hand really rolls —
/// the 150 to 300 ms a hand leaves between notches is the comfortable case,
/// and this is the one a scroll animation has to survive.
const NOTCHES: u32 = 12;
const EVERY: Duration = Duration::from_millis(50);

/// Six notches, 100 ms apart: a steady hand, which is what a person rolling a
/// wheel actually produces and the case the exponential lost.
const STEADY: u32 = 6;
const STEADILY: Duration = Duration::from_millis(100);

/// A still photographs itself into the screencast, exactly once.
///
/// `motion::SHUTTER_FRAMES` is the number the whole rest policy is built on,
/// and it is a property of the engine rather than of this crate: a
/// `Page.captureScreenshot` forces a capture of the page's surface, and the
/// screencast is watching that same surface. Taking it for granted is what
/// made the first version of the policy loop — the frame the still provoked
/// was read as the page moving, which cleared the rest, which asked for
/// another still. So it is asserted here, on a page nothing at all is
/// happening to, along with where in the still's window the frame lands.
#[test]
fn a_still_photographs_itself_into_the_screencast_exactly_once() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);
    article(&mut client, WIDE, TALL);
    let _ = screenshot(&mut client, "png", None);
    cast(&mut client, "jpeg", Some(motion::QUALITY), WIDE, TALL);
    // Let the load's own frames go by, and check that a page nobody is
    // touching then produces none of its own.
    let settle = Instant::now() + Duration::from_secs(2);
    while Instant::now() < settle {
        take_frames(&mut client);
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut idle = 0usize;
    let quiet = Instant::now() + Duration::from_secs(2);
    while Instant::now() < quiet {
        idle += take_frames(&mut client).len();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(idle, 0, "the page moved on its own, so this proves nothing");

    for round in 0..5 {
        let requested = motion::now_seconds();
        let pending = ask_for_a_still(&mut client);
        let mut answer = None;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(reply) = client.take_reply(&pending) {
                answer = Some(reply);
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let replied = motion::now_seconds();
        assert!(
            still_picture(answer.expect("the still came back")).is_some(),
            "a still with no picture in it"
        );
        // Everything the screenshot provoked, including anything that was
        // already queued when the reply was taken.
        let mut stamps = Vec::new();
        let until = Instant::now() + Duration::from_millis(600);
        while Instant::now() < until {
            stamps.extend(
                take_frames(&mut client)
                    .into_iter()
                    .filter_map(|(_, at)| at),
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        eprintln!(
            "still {round}: {:.0} ms to the reply, {} frame(s) at [{}] ms from the request",
            (replied - requested) * 1000.0,
            stamps.len(),
            stamps
                .iter()
                .map(|at| format!("{:+.0}", (at - requested) * 1000.0))
                .collect::<Vec<_>>()
                .join(", "),
        );
        assert_eq!(
            stamps.len(),
            motion::SHUTTER_FRAMES as usize,
            "a still provoked {} screencast frames, not {}",
            stamps.len(),
            motion::SHUTTER_FRAMES,
        );
        // And it is stamped inside the still's own window, which is what makes
        // crediting the still with its reply enough to keep it off the screen.
        assert!(
            stamps[0] >= requested && stamps[0] <= replied,
            "the shutter frame is stamped outside the still it belongs to"
        );
    }

    let _ = client.call("Page.stopScreencast", Json::empty());
    client.close();
    engine.kill();
}

/// One notch is an animation that arrives and stops.
///
/// This is the difference a person sees as "scrolling is choppy", and it is
/// not a frame rate: one `Input.dispatchMouseEvent` of `deltaY: 120` moves the
/// page 120 pixels in a **single** screencast frame, whatever the screencast
/// is capable of. `Input.synthesizeScrollGesture` was the answer to that for
/// one branch, and it brought a worse problem with it — see
/// [`notches_faster_than_the_engine_never_stop_the_page`].
///
/// So a notch is a curve of its own and the animation is this program's. What
/// is asserted is the shape of it: enough frames that it is an animation, a
/// page that ends exactly one notch down rather than approaching it for ever,
/// and a settling that is over inside 320 ms — `scroll::D` plus the engine's
/// own lag, with room for a host slower than the one this was measured on.
#[test]
fn one_notch_is_an_animation_and_not_a_jump() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    a_page_to_scroll(&mut client);

    let run = roll(&mut client, 1, Duration::ZERO);
    let _ = client.call("Page.stopScreencast", Json::empty());
    let (first, last) = run.movement();
    eprintln!(
        "one notch at D={:?}: {} wheel events, {} frames over {:?}\n  {}",
        scroll::D,
        run.events,
        last + 1 - first,
        run.span(),
        run.profile(),
    );
    eprintln!(
        "  the page stood still for at most {:?}",
        run.longest_stall().0
    );

    assert!(
        last + 1 - first >= 6,
        "only {} frames for one notch: the page jumped\n  {}",
        last + 1 - first,
        run.profile()
    );
    assert!(
        run.settled_after() <= Duration::from_millis(320),
        "one notch was still moving {:?} after it: {}",
        run.settled_after(),
        run.profile()
    );
    assert!(
        run.span() >= Duration::from_millis(100),
        "one notch took {:?}: {}",
        run.span(),
        run.profile()
    );
    assert_eq!(
        scroll_y(&mut client),
        blinkterm::app::WHEEL_PIXELS,
        "an animated notch still ends exactly one notch down"
    );
    assert!(
        run.wanted.iter().all(|at| *at > run.frames[last].1),
        "a still was asked for while the page was still moving"
    );

    client.close();
    engine.kill();
}

/// A steady hand moves the page steadily. This is the case the exponential
/// lost.
///
/// A notch every 100 ms is what a person rolling a wheel actually does, and it
/// was the undoing of both animations that came before this one.
/// `Input.synthesizeScrollGesture`, one gesture per coalesced pile, gave the
/// offset each frame carried as:
///
/// ```text
/// 12 12 12 11 12 9 [0] 12 23 23 24 23 18 [0] 10 12 23 25 22 …
/// ```
///
/// — every `[0]` a frame in which the page stood still, because a gesture is
/// an animation with its own beginning and end and the hand's notches do not
/// fall on them. The exponential that replaced it never stopped, but it
/// front-loaded every notch:
///
/// ```text
/// 25 18 12 9 6 4 | 39 27 19 13 9 7 | 41 28 20 14 10 | 43 30 21 15 …
/// ```
///
/// — a tenfold swing inside every notch, ten times a second, which on the VM
/// the person saw as the page shaking up and down. Neither is a stall and
/// neither is a frame rate: both are the *shape* of the delivery.
///
/// So what is asserted here is the shape. Each notch is its own ease-out over
/// `scroll::D` and the curves overlap, so no frame in the middle of the run
/// may be more than three times any other — which is the difference between a
/// page that moves and a page that lurches.
#[test]
fn a_steady_hand_moves_the_page_steadily() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    a_page_to_scroll(&mut client);

    let run = roll(&mut client, STEADY, STEADILY);
    let _ = client.call("Page.stopScreencast", Json::empty());
    let (first, last) = run.movement();
    let (high, low, swing) = run.swing();
    let (raw_high, raw_low) = run.raw_swing();
    eprintln!(
        "{STEADY} notches every {STEADILY:?} at D={:?}: {} wheel events, \
         {} frames, settled {:?} after the last notch\n  {}",
        scroll::D,
        run.events,
        last + 1 - first,
        run.settled_after(),
        run.profile(),
    );
    eprintln!(
        "  the middle of the run swings {low:.0} to {high:.0} ({swing:.1}x); \
         raw {raw_low:.0} to {raw_high:.0}"
    );

    let (stall, in_a_row) = run.longest_stall();
    eprintln!("  the page stood still for at most {stall:?}, {in_a_row} frames in a row");
    assert!(
        swing <= 3.0,
        "a frame moved {high:.0} pixels and another {low:.0} — {swing:.1}x — in the \
         middle of a steady hand, which is the lurching\n  {}",
        run.profile()
    );
    assert!(
        stall <= Duration::from_millis(60) && in_a_row <= 1,
        "the page stood still for {stall:?} ({in_a_row} frames in a row) in the \
         middle of a scroll, which is the pulsing\n  {}",
        run.profile()
    );
    assert_eq!(
        scroll_y(&mut client),
        STEADY as f64 * blinkterm::app::WHEEL_PIXELS,
        "the animation lost a notch, or invented one"
    );
    assert!(
        run.wanted.iter().all(|at| *at > run.frames[last].1),
        "a still was asked for while the page was still moving"
    );

    client.close();
    engine.kill();
}

/// A hand on the wheel gets JPEG frames and nothing else, and the page stops
/// when the hand does.
///
/// Two things at once, because they are the same run. Twelve notches 50 ms
/// apart is faster than a hand really rolls and is the case a gesture handled
/// worst: `… 48 70 25 44 [0] 45 46 47 … 47 [116] 12 [0] 5 5 13 17 …` — stops,
/// a 116-pixel jump, another stop, and a slow tail that went on after the
/// wheel had stopped. That tail is the other half of what the person reported:
/// "when I stop the wheel I want it to stop."
///
/// Every notch is a curve of `scroll::D` and nothing else, so the last one to
/// arrive is the last one to finish whatever else is running and however much
/// it all comes to: a big pile moves *further* rather than for longer, and
/// there is no ceiling anywhere to give the scrolling a speed limit.
/// **350 ms after the last notch is the deadline** — `scroll::D` and the
/// engine's own lag — and it holds for any pile.
///
/// The still policy is asserted on the same run, because it is the flicker
/// this branch's predecessor fixed and the thing most likely to break when the
/// wheel changes shape: with the first rule that shipped — a still after
/// 150 ms of frame quiet, with no notice taken of the wheel — every notch
/// ended in a lossless PNG and a page with colour in it flashed several times
/// a second. No still may be *asked for* until the animation is over, and then
/// exactly one.
#[test]
fn a_hand_on_the_wheel_gets_no_still_until_it_stops() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    a_page_to_scroll(&mut client);

    let run = roll(&mut client, NOTCHES, EVERY);
    let _ = client.call("Page.stopScreencast", Json::empty());
    let (first, last) = run.movement();
    eprintln!(
        "{NOTCHES} notches every {EVERY:?} at D={:?}: {} wheel events, \
         {} frames, settled {:?} after the last notch, {} stills wanted\n  {}",
        scroll::D,
        run.events,
        last + 1 - first,
        run.settled_after(),
        run.wanted.len(),
        run.profile(),
    );

    assert!(
        last + 1 - first > 20,
        "only {} frames for {NOTCHES} notches: the page jumped",
        last + 1 - first
    );
    let (stall, in_a_row) = run.longest_stall();
    eprintln!("  the page stood still for at most {stall:?}, {in_a_row} frames in a row");
    assert!(
        stall <= Duration::from_millis(60) && in_a_row <= 1,
        "the page stood still for {stall:?} ({in_a_row} frames in a row) in the \
         middle of a scroll, which is the pulsing\n  {}",
        run.profile()
    );
    assert!(
        run.settled_after() <= Duration::from_millis(350),
        "the page went on moving for {:?} after the wheel stopped",
        run.settled_after()
    );
    assert_eq!(
        scroll_y(&mut client),
        NOTCHES as f64 * blinkterm::app::WHEEL_PIXELS,
        "the animation lost a notch, or invented one"
    );
    assert!(
        run.wanted.iter().all(|at| *at > run.frames[last].1),
        "a still was asked for while the page was still moving"
    );
    assert_eq!(
        run.wanted.len(),
        1,
        "the scroll should cost one lossless still, at the end of it"
    );

    client.close();
    engine.kill();
}

/// The loop keeps its hands free while the engine draws a still.
///
/// `Page.captureScreenshot` at a pane's size is 66 to 98 ms on the VirtualBox
/// machine this was measured on, and the first version took it with a blocking
/// call — so every key pressed in that window arrived a tenth of a second
/// late, and a key that seemed to need pressing twice was the report that
/// found it. Here the still goes out with `Client::send`, the key goes out
/// immediately afterwards, and the page has acted on it before the still's
/// reply is collected.
#[test]
fn a_key_is_handled_while_the_still_is_in_flight() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);
    // A pane's worth of viewport, because that is what makes the screenshot
    // slow enough to be worth not waiting for.
    client
        .call(
            "Emulation.setDeviceMetricsOverride",
            Json::object(vec![
                ("width", Json::number(WIDE)),
                ("height", Json::number(TALL)),
                ("deviceScaleFactor", Json::number(1)),
                ("mobile", Json::Bool(false)),
            ]),
        )
        .expect("a pane-sized viewport");
    client
        .call(
            "Runtime.evaluate",
            Json::object(vec![
                ("expression", Json::string("document.title='waiting'")),
                ("returnByValue", Json::Bool(true)),
            ]),
        )
        .expect("the title is reset");

    let at = Instant::now();
    let pending = ask_for_a_still(&mut client);
    // What the loop does next is read the terminal, not wait.
    let params = keys::dispatch(&KeyInput {
        key: Key::Char('k'),
        mods: Mods::default(),
        action: KeyAction::Press,
        text: Some('k'),
    })
    .expect("a key with a name");
    client
        .notify("Input.dispatchKeyEvent", params)
        .expect("the key is dispatched");
    let dispatched = at.elapsed();
    let early = client.take_reply(&pending);
    assert!(
        early.is_none(),
        "the still replied before a key could even be sent, so this proves \
         nothing about the loop"
    );

    // The page acts on the key while the engine is still drawing.
    let seen = wait_for_title(&mut client, "key ", Duration::from_secs(5));
    assert_eq!(&seen, "key k KeyK 75");

    // And the still comes back afterwards, whole.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut answer = None;
    while Instant::now() < deadline {
        if let Some(reply) = client.take_reply(&pending) {
            answer = Some(reply);
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let png = still_picture(answer.expect("the still came back")).expect("a picture");
    assert_eq!(&png[..4], b"\x89PNG");
    eprintln!(
        "the key went out {dispatched:?} after the still was asked for; \
         the whole still took {:?} and is {} kB of PNG",
        at.elapsed(),
        png.len() / 1024,
    );
    assert!(
        dispatched < Duration::from_millis(20),
        "sending a key took {dispatched:?}, which is not \"immediately\""
    );

    client.close();
    engine.kill();
}

/// What a few seconds of an ordinary page leaves behind in the mailbox.
///
/// The two commands this program sends by the thousand go out with
/// `Client::notify` — a `Page.screencastFrameAck` per frame, sixty times a
/// second, and fourteen `Input.dispatchMouseEvent` per wheel notch — and Chromium
/// answers every one of them. Filing those answers under their ids was a leak
/// with a rate: an hour of reading was hundreds of thousands of entries. The
/// claim now is that nothing is kept for a command nobody will come back for,
/// and the only place to prove it is against an engine that really does reply.
///
/// A still asked for and given up on is the other half: the reply is a
/// megabyte of PNG, and dropping its `Pending` has to be enough to be rid of
/// it however late it arrives.
#[test]
fn nothing_is_kept_for_the_acknowledgements_and_the_wheel() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    a_page_to_scroll(&mut client);
    assert_eq!(
        (client.replies_held(), client.replies_wanted()),
        (0, 0),
        "the page was loaded with calls, and a call collects its own reply"
    );

    let at = (WIDE as i32 / 2, TALL as i32 / 2);
    let mut animator = Animator::default();
    let mut notches = 0u32;
    let mut next_notch = Instant::now() + Duration::from_millis(400);
    let mut frames = 0usize;
    let mut events = 0usize;

    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        let now = Instant::now();
        if notches < NOTCHES && now >= next_notch {
            animator.notch(at, (0.0, blinkterm::app::WHEEL_PIXELS), now);
            notches += 1;
            next_notch = now + EVERY;
        }
        while let Some(step) = animator.tick(Instant::now()) {
            wheel_step(&mut client, step);
            events += 1;
        }
        frames += take_offsets(&mut client).len();
        std::thread::sleep(Duration::from_millis(1));
    }
    // Whatever the engine was still saying about the last of them.
    std::thread::sleep(Duration::from_millis(500));
    frames += take_offsets(&mut client).len();

    assert_eq!(notches, NOTCHES, "the notches never all went out");
    assert!(
        frames > 20,
        "only {frames} frames in three seconds; the page was not casting, so \
         this proves nothing about the acknowledgements"
    );
    eprintln!(
        "{frames} frames acknowledged and {events} wheel events sent; the \
         mailbox holds {} replies and wants {}",
        client.replies_held(),
        client.replies_wanted()
    );
    assert_eq!(
        (client.replies_held(), client.replies_wanted()),
        (0, 0),
        "{} replies to commands nobody asked about",
        client.replies_held()
    );

    // And the still that is given up on.
    let pending = ask_for_a_still(&mut client);
    assert_eq!(
        client.replies_wanted(),
        1,
        "the still is the one thing outstanding"
    );
    drop(pending);
    std::thread::sleep(Duration::from_millis(1000));
    let _ = take_offsets(&mut client);
    assert_eq!(
        (client.replies_held(), client.replies_wanted()),
        (0, 0),
        "a still nobody is waiting for was kept anyway"
    );

    client.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------
//
// What `--profile` is for, against the real engine: a cookie that survives a
// quit. There is deliberately no test here that starts two engines on one
// directory — the headless shell has no singleton and would run both happily,
// so it would pass and prove nothing; the lock that prevents it is this
// program's and is tested in `profile.rs` without an engine. Nor is there one
// asserting that a `SIGTERM` loses the cookie: that is a fact about Chromium
// 141 worth knowing (`profile.rs` has the table), not a behaviour of this
// crate worth pinning.

/// The cookie, from `Network.getCookies` on `client`, if the engine has it.
fn the_cookie(client: &mut Client) -> Option<String> {
    let reply = client
        .call(
            "Network.getCookies",
            Json::object(vec![(
                "urls",
                Json::Array(vec![Json::string("https://example.com/")]),
            )]),
        )
        .expect("the cookies");
    reply
        .get("cookies")
        .and_then(Json::as_array)
        .unwrap_or_default()
        .iter()
        .find(|cookie| cookie.get("name").and_then(Json::as_str) == Some("blinkterm"))
        .and_then(|cookie| cookie.get("value"))
        .and_then(Json::as_str)
        .map(str::to_string)
}

/// The whole point of issue #6: a login is a cookie, and a cookie set in one
/// run is there in the next — provided the engine is stopped the way
/// `app::run` stops it, with `Browser.close` and a wait, rather than killed.
#[test]
fn a_cookie_set_in_one_session_is_there_in_the_next() {
    let root = temp_dir("profile-kept");
    let dir = root.join("profile");
    let _ = std::fs::remove_dir_all(&dir);

    let profile = Profile::take(Choice::At(dir.clone())).expect("the profile");
    let Some((mut engine, mut client)) = connect_in(profile) else {
        let _ = std::fs::remove_dir_all(&root);
        return;
    };
    // A day from now: a session cookie is not written to disk by design, so
    // it would be lost however the engine stopped and prove nothing.
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs_f64()
        + 86_400.0;
    let set = client
        .call(
            "Network.setCookie",
            Json::object(vec![
                ("name", Json::string("blinkterm")),
                ("value", Json::string("kept")),
                ("url", Json::string("https://example.com/")),
                ("expires", Json::number(expires)),
            ]),
        )
        .expect("the cookie is set");
    assert_eq!(set.get("success").and_then(Json::as_bool), Some(true));
    assert_eq!(the_cookie(&mut client).as_deref(), Some("kept"));

    let mut browser = engine.browser().expect("the browser's client");
    let asked = Instant::now();
    // The reply and the end of the connection race, and either is an answer.
    let _ = browser.call_within("Browser.close", Json::empty(), Duration::from_secs(5));
    assert!(
        engine.wait_for_exit(Duration::from_secs(5)),
        "the engine was asked to close and was still running five seconds later"
    );
    eprintln!("Browser.close to gone: {:?}", asked.elapsed());
    drop(browser);
    drop(client);
    drop(engine);
    assert!(
        dir.join("Default").is_dir(),
        "the engine wrote no profile into {}",
        dir.display()
    );

    let profile = Profile::take(Choice::At(dir.clone()))
        .expect("the profile is free once the engine that had it is gone");
    let (engine, mut client) = connect_in(profile).expect("a second engine");
    assert_eq!(
        the_cookie(&mut client).as_deref(),
        Some("kept"),
        "the cookie did not survive a Browser.close"
    );
    drop(client);
    drop(engine);
    let _ = std::fs::remove_dir_all(&root);
}

/// `--temp-profile`, and every engine test: the directory is the program's to
/// make and the program's to remove, whichever engine it is — full Chromium
/// left to itself leaves a whole profile in `/tmp` after every run.
#[test]
fn a_temporary_profile_leaves_nothing_on_disk() {
    let Some((engine, mut client)) = connect() else {
        return;
    };
    let dir = engine.profile().dir().to_path_buf();
    assert!(engine.profile().is_temporary());
    assert!(dir.is_dir(), "{} was never made", dir.display());
    prepare(&mut client);

    drop(client);
    drop(engine);
    assert!(!dir.exists(), "{} is still there", dir.display());
}

// ---------------------------------------------------------------------------
// Failed loads
// ---------------------------------------------------------------------------

use blinkterm::hover;
use blinkterm::load::{self, Landing, Loaded, Problem};

/// A server with the troubles a page can have, and a port that has none of
/// anything.
///
/// `/404` and `/500` are error statuses with bodies of their own — a body is
/// what makes the engine show the site's page rather than its own — `/redir`
/// is a 302 into the closed port, `/link` is a page whose top-left corner is
/// a link into it, and anything else is a page that is fine. The closed port
/// is one the kernel handed out and this test gave back, so nothing is on it
/// and nothing will be while the test runs. The base comes back without a
/// trailing slash, so that `base + "/404"` is the url.
///
/// And the slow ones: `/hang` accepts and never answers, `/slowbody` sends
/// its headers and half a body and then nothing, and `/leave` is a page
/// whose top-left corner is a link into `/hang`. `/links` is the hover's
/// page: links of several kinds at known places. Each connection is served
/// on a thread of its own, so a hang holds its own socket and nobody else's.
fn serve_troubles() -> (String, u16) {
    use std::io::{Read, Write};
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to close");
        listener.local_addr().expect("an address").port()
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to serve on");
    let address = listener.local_addr().expect("an address");
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut head = [0u8; 2048];
                let read = stream.read(&mut head).unwrap_or(0);
                let request = String::from_utf8_lossy(&head[..read]).to_string();
                let path = request.split(' ').nth(1).unwrap_or("/").to_string();
                let dead = format!("http://127.0.0.1:{closed}/");
                if path == "/hang" {
                    std::thread::sleep(Duration::from_secs(60));
                    return;
                }
                if path == "/slowbody" {
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 100000\r\n\
                      Connection: close\r\n\r\n<!doctype html><title>slowbody</title>\
                      <p>the first half of a page that never finishes ",
                    );
                    let _ = stream.flush();
                    std::thread::sleep(Duration::from_secs(60));
                    return;
                }
                let (status, extra, body) = match path.as_str() {
                    "/404" => (
                        "404 Not Found",
                        String::new(),
                        "<!doctype html><title>nope</title><p>not here".to_string(),
                    ),
                    "/500" => (
                        "500 Internal Server Error",
                        String::new(),
                        "<!doctype html><title>broken</title><p>broken".to_string(),
                    ),
                    "/redir" => ("302 Found", format!("Location: {dead}\r\n"), String::new()),
                    "/link" => (
                        "200 OK",
                        String::new(),
                        format!(
                            "<!doctype html><title>link</title><body style='margin:0'>\
                         <a href='{dead}' style='display:block;position:absolute;\
                         left:0;top:0;width:240px;height:80px;background:#cc3'>dead</a>"
                        ),
                    ),
                    "/leave" => (
                        "200 OK",
                        String::new(),
                        "<!doctype html><title>leave</title><body style='margin:0'>\
                     <a href='/hang' style='display:block;position:absolute;\
                     left:0;top:0;width:240px;height:80px;background:#cc3'>hang</a>"
                            .to_string(),
                    ),
                    "/links" => ("200 OK", String::new(), LINKS.to_string()),
                    _ => (
                        "200 OK",
                        String::new(),
                        "<!doctype html><title>fine</title><p>fine".to_string(),
                    ),
                };
                let answer = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\n{extra}\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes());
            });
        }
    });
    (format!("http://{address}"), closed)
}

/// A page connection with `Page.enable` and nothing else, which is all the
/// program has on a tab either — no `Network`, no `Log` — held in a tab the
/// way the program holds it.
fn failing_tab() -> Option<(Engine, Tab<Client>)> {
    let (engine, mut client) = connect()?;
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    Some((engine, Tab::new("t", client, "about:blank")))
}

/// Navigate as `app::navigate` does — the problem cleared, the call made, the
/// reply handed to `app::navigated` — and hand the reply back to be looked at.
fn navigate_tab(tab: &mut Tab<Client>, url: &str) -> Json {
    let _ = tab.connection.events();
    tab.url = url.to_string();
    tab.note = Some(format!("loading {url}"));
    tab.loading = true;
    tab.problem = None;
    let reply = tab
        .connection
        .call_within(
            "Page.navigate",
            Json::object(vec![("url", Json::string(url))]),
            Duration::from_secs(20),
        )
        .expect("the engine answers the navigation");
    blinkterm::app::navigated(tab, url, &reply);
    reply
}

/// What `app::handle_page_events` does with a tab's events, until a main
/// frame has landed and the load event after it has fired: the landings seen,
/// in order.
fn follow(tab: &mut Tab<Client>, timeout: Duration) -> Vec<Landing> {
    let deadline = Instant::now() + timeout;
    let mut landings = Vec::new();
    let mut loaded = false;
    // `loaded` is only ever set once something has landed, and a landing
    // after it sets it back.
    while !loaded && Instant::now() < deadline {
        for event in tab.connection.events() {
            match event.method.as_str() {
                "Page.frameNavigated" => {
                    if let Some(landing) = load::landing(&event.params) {
                        landings.push(landing.clone());
                        tab.landed(landing);
                        loaded = false;
                    }
                }
                "Page.loadEventFired" if !landings.is_empty() => {
                    tab.loading = false;
                    if let Some(answer) = blinkterm::app::page_loaded(&mut tab.connection) {
                        tab.loaded(answer);
                    }
                    loaded = true;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        loaded,
        "no load event after the landing within {timeout:?}: {landings:?}"
    );
    landings
}

/// How many entries the tab's history has.
fn history_length(client: &mut Client) -> usize {
    client
        .call("Page.getNavigationHistory", Json::empty())
        .expect("the history")
        .get("entries")
        .and_then(Json::as_array)
        .map_or(0, <[Json]>::len)
}

/// The quickest failure there is, and the one the resolver would otherwise
/// be asked about: `.invalid` is reserved never to resolve, and the engine
/// knows it without asking anyone. What matters is that the reason is in the
/// reply to `Page.navigate`, which is the only place it is, and that the
/// error page's landing afterwards keeps it.
#[test]
fn a_host_that_does_not_exist_is_explained_before_the_resolver_gives_up() {
    let Some((mut engine, mut tab)) = failing_tab() else {
        return;
    };
    let url = "https://nonexistent.invalid/";
    let started = Instant::now();
    let reply = navigate_tab(&mut tab, url);
    let elapsed = started.elapsed();
    let code = load::failed(&reply).expect("a reply with an errorText in it");
    eprintln!("{url}: {code} in {elapsed:?}");
    assert!(
        elapsed < Duration::from_secs(10),
        "{elapsed:?} to say a reserved name does not exist"
    );
    assert_eq!(
        tab.line(),
        format!("can't reach nonexistent.invalid: {}", load::reason(&code))
    );

    let landings = follow(&mut tab, Duration::from_secs(5));
    assert_eq!(landings, [Landing::Unreachable(url.to_string())]);
    assert_eq!(tab.url, url, "the address, never chrome-error://");
    // Behind a proxy the engine never asks a resolver and the code is the
    // proxy's; the sentence still names the host, and the words are checked
    // exactly only for the code this was measured with.
    if code == "net::ERR_NAME_NOT_RESOLVED" {
        assert_eq!(
            tab.line(),
            "can't reach nonexistent.invalid: name not resolved"
        );
    } else {
        eprintln!(
            "not the resolver's answer, so probably a proxy's: {}",
            tab.line()
        );
        assert!(tab.line().starts_with("can't reach nonexistent.invalid: "));
    }

    tab.connection.close();
    engine.kill();
}

/// A closed port is refused, and a redirect into one fails where it ended:
/// the reply's reason is the last hop's, and the landing is at the last hop's
/// url, which is what the row names. Going to the failed url again — which is
/// what `ctrl+r` does on an error page — is checked here not to add a history
/// entry.
#[test]
fn a_port_nobody_listens_on_is_refused_and_a_redirect_into_it_names_where_it_ended() {
    let Some((mut engine, mut tab)) = failing_tab() else {
        return;
    };
    let (base, closed) = serve_troubles();
    let dead = format!("http://127.0.0.1:{closed}/");
    let refused = format!("can't reach 127.0.0.1:{closed}: connection refused");

    // Somewhere to have been, so that the history has a before.
    navigate_tab(&mut tab, &format!("{base}/"));
    follow(&mut tab, Duration::from_secs(10));

    let reply = navigate_tab(&mut tab, &dead);
    assert_eq!(
        load::failed(&reply).as_deref(),
        Some("net::ERR_CONNECTION_REFUSED")
    );
    assert_eq!(
        follow(&mut tab, Duration::from_secs(5)),
        [Landing::Unreachable(dead.clone())]
    );
    assert_eq!(tab.line(), refused);
    let before = history_length(&mut tab.connection);

    // `ctrl+r` on the error page: the same url again, through `Page.navigate`.
    let again = navigate_tab(&mut tab, &dead);
    assert_eq!(
        load::failed(&again).as_deref(),
        Some("net::ERR_CONNECTION_REFUSED")
    );
    follow(&mut tab, Duration::from_secs(5));
    assert_eq!(tab.line(), refused);
    let after = history_length(&mut tab.connection);
    eprintln!("history: {before} entries before going again, {after} after");
    assert_eq!(
        after, before,
        "going to the same failed url again is a reload"
    );

    // Through a redirect.
    let reply = navigate_tab(&mut tab, &format!("{base}/redir"));
    assert_eq!(
        load::failed(&reply).as_deref(),
        Some("net::ERR_CONNECTION_REFUSED"),
        "the last hop's reason"
    );
    assert_eq!(
        follow(&mut tab, Duration::from_secs(5)),
        [Landing::Unreachable(dead.clone())],
        "and the last hop's url"
    );
    assert_eq!(tab.url, dead);
    assert_eq!(tab.line(), refused);

    tab.connection.close();
    engine.kill();
}

/// A link is not a `Page.navigate`, so there is no reply and no reason — and
/// the landing still says the page did not come, which is the half that used
/// to show `chrome-error://`. A reload of the error page lands again.
#[test]
fn a_link_into_a_dead_host_is_a_failure_the_page_reports_by_itself() {
    let Some((mut engine, mut tab)) = failing_tab() else {
        return;
    };
    let (base, closed) = serve_troubles();
    let dead = format!("http://127.0.0.1:{closed}/");

    navigate_tab(&mut tab, &format!("{base}/link"));
    follow(&mut tab, Duration::from_secs(10));
    assert_eq!(tab.title, "link");
    assert_eq!(tab.problem, None);

    let clicked = Instant::now();
    for (kind, buttons) in [("mousePressed", 1), ("mouseReleased", 0)] {
        tab.connection
            .call(
                "Input.dispatchMouseEvent",
                Json::object(vec![
                    ("type", Json::string(kind)),
                    ("x", Json::number(20)),
                    ("y", Json::number(20)),
                    ("button", Json::string("left")),
                    ("buttons", Json::number(buttons)),
                    ("clickCount", Json::number(1)),
                    ("modifiers", Json::number(0)),
                ]),
            )
            .expect("the click is dispatched");
    }
    let landings = follow(&mut tab, Duration::from_secs(5));
    eprintln!("the link's failure landed within {:?}", clicked.elapsed());
    assert_eq!(landings, [Landing::Unreachable(dead.clone())]);
    assert_eq!(tab.url, dead);
    assert_eq!(tab.line(), format!("can't reach 127.0.0.1:{closed}"));

    let _ = tab.connection.events();
    tab.connection
        .call("Page.reload", Json::empty())
        .expect("the reload is taken");
    assert_eq!(
        follow(&mut tab, Duration::from_secs(5)),
        [Landing::Unreachable(dead.clone())],
        "a reload of an error page is a second landing"
    );
    assert_eq!(tab.line(), format!("can't reach 127.0.0.1:{closed}"));

    tab.connection.close();
    engine.kill();
}

/// The status comes out of the same evaluation as the title, from the
/// navigation timing entry, with no `Network` domain enabled.
#[test]
fn the_status_of_the_document_comes_with_its_title() {
    let Some((mut engine, mut tab)) = failing_tab() else {
        return;
    };
    let (base, closed) = serve_troubles();

    let reply = navigate_tab(&mut tab, &format!("{base}/404"));
    assert_eq!(
        load::failed(&reply),
        None,
        "a 404 is not a failure to the engine"
    );
    follow(&mut tab, Duration::from_secs(10));
    assert_eq!(
        blinkterm::app::page_loaded(&mut tab.connection),
        Some(Loaded {
            title: "nope".to_string(),
            status: Some(404)
        })
    );
    assert_eq!(tab.problem, Some(Problem::Status(404)));
    assert_eq!(tab.line(), format!("404 not found  —  nope  —  {base}/404"));

    navigate_tab(&mut tab, &format!("{base}/500"));
    follow(&mut tab, Duration::from_secs(10));
    assert_eq!(
        blinkterm::app::page_loaded(&mut tab.connection).and_then(|loaded| loaded.status),
        Some(500)
    );
    assert_eq!(tab.problem, Some(Problem::Status(500)));

    navigate_tab(&mut tab, &format!("{base}/"));
    follow(&mut tab, Duration::from_secs(10));
    assert_eq!(
        blinkterm::app::page_loaded(&mut tab.connection),
        Some(Loaded {
            title: "fine".to_string(),
            status: None
        })
    );
    assert_eq!(tab.problem, None);

    navigate_tab(&mut tab, &format!("http://127.0.0.1:{closed}/"));
    follow(&mut tab, Duration::from_secs(5));
    assert_eq!(
        blinkterm::app::page_loaded(&mut tab.connection),
        Some(Loaded {
            title: String::new(),
            status: None
        }),
        "an error page has no title and no status"
    );

    tab.connection.close();
    engine.kill();
}

/// The hover's page: a link with markup inside it at the top left, a
/// `javascript:` link under it, a `<div>` that only looks like one, a text
/// field, and a link whose href tries to speak to the terminal.
const LINKS: &str = "<!doctype html><title>links</title><body style='margin:0'>\
<a id=a href='/target?x=1' style='position:absolute;left:0;top:0;width:100px;height:40px;\
background:#cc3'><span><b>nested</b> text</span></a>\
<a id=b href='javascript:void(0)' style='position:absolute;left:0;top:50px;width:100px;\
height:40px;background:#3cc'>js</a>\
<div id=d style='position:absolute;left:0;top:150px;width:100px;height:40px;\
background:#999;cursor:pointer'>div pointer</div>\
<input id=e style='position:absolute;left:0;top:200px;width:100px;height:30px'>\
<a id=g href='http://\u{202e}evil.example/\u{1b}]0;x\u{7}' style='position:absolute;\
left:200px;top:50px;width:100px;height:40px;background:#cc3'>hostile</a>";

/// Tell the page the pointer is at `(x, y)` as `tick_hover` does, ask it
/// what is there as `tick_hover` does — sent, and collected rather than
/// waited on — and read the answer. With how long the answer took.
fn hover_at(client: &mut Client, x: i32, y: i32) -> (hover::Hover, Duration) {
    client
        .notify(
            "Input.dispatchMouseEvent",
            Json::object(vec![
                ("type", Json::string("mouseMoved")),
                ("x", Json::number(x)),
                ("y", Json::number(y)),
                ("modifiers", Json::number(0)),
                ("button", Json::string("none")),
                ("buttons", Json::number(0)),
            ]),
        )
        .expect("the move is sent");
    let asked = Instant::now();
    let pending = client
        .send("Runtime.evaluate", hover::ask(x, y))
        .expect("the ask is sent");
    let deadline = asked + Duration::from_secs(2);
    loop {
        if let Some(reply) = client.take_reply(&pending) {
            let took = asked.elapsed();
            let reply = reply.expect("the page answers");
            let answer = hover::answer(&reply)
                .unwrap_or_else(|| panic!("not an answer at ({x}, {y}): {reply}"));
            return (answer, took);
        }
        assert!(
            Instant::now() < deadline,
            "no answer about ({x}, {y}) in 2 s"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// What the row says for a link is the engine's resolved href, and the shape
/// the terminal is told is one of the table's. A nested element inside an
/// anchor is the anchor; a pointer cursor on something that is not a link is
/// a hand and no href; empty page is nothing at all.
#[test]
fn hovering_a_link_reports_its_href_and_a_hand_and_a_plain_spot_reports_neither() {
    let Some((mut engine, mut tab)) = failing_tab() else {
        return;
    };
    let (base, _) = serve_troubles();
    navigate_tab(&mut tab, &format!("{base}/links"));
    follow(&mut tab, Duration::from_secs(10));
    assert_eq!(tab.title, "links");

    let (link, _) = hover_at(&mut tab.connection, 50, 20);
    assert_eq!(
        link,
        hover::Hover {
            href: format!("{base}/target?x=1"),
            shape: hover::Shape::Pointer
        }
    );
    assert_eq!(hover::words(&link.href), format!("link: {base}/target?x=1"));

    let (script, _) = hover_at(&mut tab.connection, 50, 70);
    assert_eq!(script.href, "javascript:void(0)", "shown as written");

    let (div, _) = hover_at(&mut tab.connection, 50, 170);
    assert_eq!(
        div,
        hover::Hover {
            href: String::new(),
            shape: hover::Shape::Pointer
        },
        "a hand, and no link"
    );
    let (field, _) = hover_at(&mut tab.connection, 50, 215);
    assert_eq!(field.shape, hover::Shape::Text);
    let (nothing, _) = hover_at(&mut tab.connection, 500, 340);
    assert_eq!(nothing, hover::Hover::default());

    let (hostile, _) = hover_at(&mut tab.connection, 250, 70);
    eprintln!("the hostile href came back as {:?}", hostile.href);
    // The engine IDNA-encodes the host with the override in it and
    // percent-encodes the escape; either way it is a link, and plain.
    assert!(hostile.href.contains(".example/"), "{hostile:?}");
    assert_eq!(hostile.shape, hover::Shape::Pointer);
    assert_eq!(
        blinkterm::text::sanitize(&hostile.href),
        hostile.href.as_str(),
        "plain text already"
    );
    assert!(!hostile.href.chars().any(char::is_control));

    // Timing, printed to be read against the table in `hover.rs`.
    let mut took: Vec<Duration> = (0..30)
        .map(|n| hover_at(&mut tab.connection, 10 + n, 20).1)
        .collect();
    took.sort();
    eprintln!("median of thirty asks: {:?}", took[took.len() / 2]);

    tab.connection.close();
    engine.kill();
}

/// Where the tab's main frame is, as `connect_tab` reads it.
fn learn_frame(tab: &mut Tab<Client>) {
    let tree = tab
        .connection
        .call("Page.getFrameTree", Json::empty())
        .expect("the frame tree");
    tab.frame = load::main_frame(&tree);
    assert!(tab.frame.is_some(), "no main frame in {tree}");
}

/// What `app::navigate` does after `edit_url` has set the tab up: sent, not
/// waited for, with the clock started.
fn send_navigation(tab: &mut Tab<Client>, url: &str) -> Pending {
    let _ = tab.connection.events();
    tab.url = url.to_string();
    tab.note = Some(format!("loading {url}"));
    tab.loading = true;
    tab.problem = None;
    tab.since = Some(Instant::now());
    tab.committed = false;
    tab.connection
        .send(
            "Page.navigate",
            Json::object(vec![("url", Json::string(url))]),
        )
        .expect("the navigation is sent")
}

/// What `handle_page_events` does with the loading events, for `within`:
/// the methods seen, in order.
fn watch_loading(tab: &mut Tab<Client>, within: Duration) -> Vec<String> {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        for event in tab.connection.events() {
            let main = load::is_main(&event.params, tab.frame.as_deref());
            match event.method.as_str() {
                "Page.frameStartedNavigating" => {
                    if let Some(url) = load::started(&event.params, tab.frame.as_deref()) {
                        tab.started(url, Instant::now());
                    }
                }
                "Page.frameNavigated" => {
                    if let Some(landing) = load::landing(&event.params) {
                        tab.trust = load::trust(&event.params);
                        tab.landed(landing);
                    }
                }
                "Page.frameStoppedLoading" if main => tab.stopped_loading(),
                _ => {}
            }
            // The load event names no frame; it is only ever the page's.
            if main
                || matches!(
                    event.method.as_str(),
                    "Page.frameNavigated" | "Page.loadEventFired"
                )
            {
                seen.push(event.method);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    seen
}

/// `esc` on a page whose next page never comes: the load stops, and the tab
/// is the page it was — its url, its title, its history — rather than a url
/// that was typed and a note that it is loading. And past the commit, where
/// no load event is ever coming: the half page, with its title.
#[test]
fn escape_while_a_page_hangs_stops_the_load_and_leaves_the_page_where_it_was() {
    let Some((mut engine, mut tab)) = failing_tab() else {
        return;
    };
    let (base, _) = serve_troubles();
    navigate_tab(&mut tab, &format!("{base}/"));
    follow(&mut tab, Duration::from_secs(10));
    learn_frame(&mut tab);
    assert_eq!(tab.title, "fine");
    let entries = history_length(&mut tab.connection);

    let hang = format!("{base}/hang");
    let pending = send_navigation(&mut tab, &hang);
    let before = watch_loading(&mut tab, Duration::from_millis(500));
    eprintln!("before the stop: {before:?}");
    assert!(
        before
            .iter()
            .any(|method| method == "Page.frameStartedNavigating"
                || method == "Page.frameStartedLoading"),
        "the departure is announced: {before:?}"
    );
    assert!(
        !before.iter().any(|method| method == "Page.frameNavigated"),
        "nothing landed: {before:?}"
    );
    assert!(tab.loading);
    assert!(!tab.committed);
    assert_eq!(tab.line(), format!("loading {hang}"));
    assert!(
        tab.connection.take_reply(&pending).is_none(),
        "the navigation is held"
    );

    let stopped = Instant::now();
    blinkterm::app::stop(&mut tab);
    let after = watch_loading(&mut tab, Duration::from_secs(1));
    eprintln!("after the stop, {:?}: {after:?}", stopped.elapsed());
    assert!(
        after
            .iter()
            .any(|method| method == "Page.frameStoppedLoading"),
        "the main frame stopped: {after:?}"
    );
    let reply = tab
        .connection
        .take_reply(&pending)
        .expect("the navigation answered once stopped")
        .expect("with a reply, not an error");
    assert_eq!(
        reply.get("errorText").and_then(Json::as_str),
        Some("net::ERR_ABORTED")
    );
    assert_eq!(load::failed(&reply), None, "which is not a failure to say");
    assert!(!tab.loading);
    assert_eq!(tab.note, None);
    assert_eq!(tab.url, format!("{base}/"));
    assert_eq!(tab.title, "fine");
    assert_eq!(tab.line(), format!("fine  —  {base}/"));
    assert_eq!(history_length(&mut tab.connection), entries);

    // Past the commit: headers and half a body, and then nothing.
    let slow = format!("{base}/slowbody");
    let _pending = send_navigation(&mut tab, &slow);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !tab.committed && Instant::now() < deadline {
        watch_loading(&mut tab, Duration::from_millis(50));
    }
    assert!(tab.committed, "the half page committed");
    assert!(tab.loading);
    blinkterm::app::stop(&mut tab);
    let after = watch_loading(&mut tab, Duration::from_secs(1));
    eprintln!("after stopping the half page: {after:?}");
    assert!(
        after
            .iter()
            .any(|method| method == "Page.frameStoppedLoading"),
        "{after:?}"
    );
    assert!(
        !after.iter().any(|method| method == "Page.loadEventFired"),
        "no load event is coming: {after:?}"
    );
    assert!(!tab.loading);
    assert_eq!(tab.url, slow);
    assert_eq!(tab.title, "slowbody");

    // And a stop on a page that is not loading is nothing at all.
    tab.connection
        .call("Page.stopLoading", Json::empty())
        .expect("a stop with nothing to stop is answered");
    let idle = watch_loading(&mut tab, Duration::from_millis(300));
    assert!(
        !idle
            .iter()
            .any(|method| method == "Page.frameStoppedLoading"),
        "{idle:?}"
    );
    engine.check().expect("the engine is still there");
    assert!(blinkterm::app::page_loaded(&mut tab.connection).is_some());

    tab.connection.close();
    engine.kill();
}

/// A link clicked into a host that says nothing is announced, with its url,
/// before any byte comes back — which is what puts `loading …` on the row at
/// once rather than never — and `esc` takes it back off.
#[test]
fn a_click_that_leaves_is_announced_before_anything_comes_back() {
    let Some((mut engine, mut tab)) = failing_tab() else {
        return;
    };
    let (base, _) = serve_troubles();
    navigate_tab(&mut tab, &format!("{base}/leave"));
    follow(&mut tab, Duration::from_secs(10));
    learn_frame(&mut tab);
    assert_eq!(tab.title, "leave");
    let _ = tab.connection.events();

    for (kind, buttons) in [("mousePressed", 1), ("mouseReleased", 0)] {
        tab.connection
            .notify(
                "Input.dispatchMouseEvent",
                Json::object(vec![
                    ("type", Json::string(kind)),
                    ("x", Json::number(20)),
                    ("y", Json::number(20)),
                    ("button", Json::string("left")),
                    ("buttons", Json::number(buttons)),
                    ("clickCount", Json::number(1)),
                    ("modifiers", Json::number(0)),
                ]),
            )
            .expect("the click is dispatched");
    }
    let seen = watch_loading(&mut tab, Duration::from_millis(500));
    eprintln!("after the click: {seen:?}");
    if seen
        .iter()
        .any(|method| method == "Page.frameStartedNavigating")
    {
        assert_eq!(tab.line(), format!("loading {base}/hang"));
        assert!(!tab.committed);
    } else {
        eprintln!(
            "skipped the url: this engine sent no Page.frameStartedNavigating \
             (older than Chromium 132)"
        );
    }
    assert!(tab.loading, "either way the tab is loading: {seen:?}");

    blinkterm::app::stop(&mut tab);
    let after = watch_loading(&mut tab, Duration::from_secs(1));
    assert!(
        after
            .iter()
            .any(|method| method == "Page.frameStoppedLoading"),
        "{after:?}"
    );
    assert!(!tab.loading);
    assert_eq!(tab.line(), format!("leave  —  {base}/leave"));

    tab.connection.close();
    engine.kill();
}

/// A page on this machine over plain http is left unmarked, as a desktop
/// browser leaves it, and the reason is the engine's: it calls loopback a
/// secure context. The named-host case needs DNS or a resolver flag this
/// program does not pass, so the `Insecure` branch is the unit test on the
/// measured JSON; what this checks is that the event still has the shape
/// that JSON has, so that a Chromium that renamed the field fails here
/// rather than quietly un-marking every page.
#[test]
fn an_http_page_on_this_machine_is_not_marked_and_the_engine_says_why() {
    let Some((mut engine, mut tab)) = failing_tab() else {
        return;
    };
    let (base, _) = serve_troubles();
    let _ = send_navigation(&mut tab, &format!("{base}/"));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut landed = None;
    while landed.is_none() && Instant::now() < deadline {
        for event in tab.connection.events() {
            if event.method == "Page.frameNavigated" && load::landing(&event.params).is_some() {
                landed = Some(event.params);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let params = landed.expect("the page landed");
    let context = params
        .path(&["frame", "secureContextType"])
        .and_then(Json::as_str);
    assert_eq!(context, Some("SecureLocalhost"), "{params}");
    assert_eq!(load::trust(&params), load::Trust::Plain);
    tab.trust = load::trust(&params);
    if let Some(landing) = load::landing(&params) {
        tab.landed(landing);
    }
    std::thread::sleep(Duration::from_millis(200));
    if let Some(loaded) = blinkterm::app::page_loaded(&mut tab.connection) {
        tab.loaded(loaded);
    }
    assert!(!tab.line().contains("not secure"), "{}", tab.line());

    tab.connection.close();
    engine.kill();
}

/// Whether a row, fed to the compositor's own terminal, only ever wrote text:
/// no title set, no question answered, and nothing below a space between the
/// row's own escapes. Returns what the top row reads as.
fn a_terminal_reads_only_text_in(row: &[u8]) -> String {
    let mut terminal = tos_term::Terminal::new(80, 24, tos_term::TerminalConfig::default());
    terminal.advance(&blinkterm::screen::enter_sequence());
    let _ = terminal.take_output();
    terminal.advance(row);
    assert_eq!(
        terminal.title(),
        "",
        "the row set the window title: {row:?}"
    );
    assert!(
        terminal.take_output().is_empty(),
        "the row asked the terminal something: {row:?}"
    );
    // And byte for byte: nothing below a space between the framing.
    let body = String::from_utf8_lossy(row);
    let body = body
        .trim_start_matches("\x1b[1;1H\x1b[K\x1b[7m")
        .trim_end_matches("\x1b[0m\x1b[?25l")
        .replace("\x1b[27m", "")
        .replace("\x1b[7m", "");
    assert!(body.bytes().all(|b| b >= 0x20 && b != 0x7f), "{body:?}");
    terminal.grid().row(0).to_text()
}

/// A page whose title is an OSC sequence, and whose dialog is one too. What
/// is checked is not the title the engine reports — it reports what the
/// script set, escape and all, as `\u001b` in its JSON — but that nothing of
/// it survives into the row, measured the way the frame tests measure: the
/// bytes this program would write, parsed by the compositor's own terminal.
#[test]
fn a_page_that_titles_itself_with_an_escape_sequence_cannot_reach_the_terminal() {
    use blinkterm::screen::{self, TabLabel};
    let Some((mut engine, mut tab)) = failing_tab() else {
        return;
    };
    // Set from a script rather than in the url, so that the url's own
    // canonicalisation is not what is being tested.
    const HOSTILE: &str = "data:text/html,<title>plain</title><script>\
document.title=String.fromCharCode(27)+']0;pwned'+String.fromCharCode(7)\
+' a'+String.fromCharCode(13)+'b '+String.fromCharCode(0x202e)+'moc.elpmaxe';\
</script>";
    navigate_tab(&mut tab, HOSTILE);
    follow(&mut tab, Duration::from_secs(10));
    // `document.title`'s getter collapses ASCII whitespace itself, so the
    // `\r` is a space before this program sees it; ESC, BEL and the override
    // are not whitespace and arrive whole, as `\u001b`, `\u0007`, `\u202e`.
    // The expected string is the same whichever does the collapsing, on
    // purpose.
    let loaded = blinkterm::app::page_loaded(&mut tab.connection).expect("the page answers");
    assert_eq!(loaded.title, "]0;pwned a b moc.elpmaxe");
    assert_eq!(tab.title, loaded.title, "and that is what the tab holds");

    let status = screen::status_line(80, &tab.line());
    let text = a_terminal_reads_only_text_in(&status);
    assert!(text.starts_with("]0;pwned a b moc.elpmaxe"), "{text:?}");

    let label = tab.label();
    let strip = screen::tab_line(
        80,
        &[
            TabLabel {
                title: &label,
                active: true,
                dialog: false,
            },
            TabLabel {
                title: "\x1b]2;x\x07",
                active: false,
                dialog: true,
            },
        ],
        &tab.url,
    );
    let text = a_terminal_reads_only_text_in(&strip);
    assert!(text.starts_with("1 ]0;pwned"), "{text:?}");

    // The same through a dialog, which is the other thing a page writes.
    raise(
        &mut tab.connection,
        "alert(String.fromCharCode(27)+']0;pwned'+String.fromCharCode(7))",
    );
    let dialog = wait_for_dialog(&tab.connection, Duration::from_secs(5));
    assert_eq!(dialog.caption(), "alert: ]0;pwned");
    let row = screen::dialog_line(80, &dialog.caption(), dialog.hint());
    let text = a_terminal_reads_only_text_in(&row);
    assert!(text.starts_with("alert: ]0;pwned"), "{text:?}");
    answer(&mut tab.connection, dialog, &[press(Key::Enter)]);

    tab.connection.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------
//
// A page with a dialog open is a page whose renderer is stopped inside the
// script that opened it, so nothing below asks it anything while one is up:
// not its title, not a screenshot. Those would sit out their deadlines and
// prove only that the page was stopped, which is the one thing already known.
// What is asked is the engine — the dialog opening, the dialog closing, and
// what the page did with the answer once it had one.

/// A page with nothing on it but a title, which is where the scripts below
/// write what their dialogs returned.
const QUIET_PAGE: &str = "data:text/html,<title>ready</title><body>";

/// Load a page that can be asked to open dialogs.
fn a_page_that_asks(client: &mut Client, url: &str, title: &str) {
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(url))]),
        )
        .expect("the page loads");
    assert_eq!(
        wait_for_title(client, title, Duration::from_secs(10)),
        title
    );
    let _ = client.events();
}

/// Run a script that opens a dialog.
///
/// `notify` rather than `call`: the evaluation does not answer until the
/// script is over, and the script is not over until the dialog is answered —
/// a `call` here would wait out its own deadline and then report the page as
/// broken.
fn raise(client: &mut Client, expression: &str) {
    client
        .notify(
            "Runtime.evaluate",
            Json::object(vec![("expression", Json::string(expression))]),
        )
        .expect("the script is sent");
}

/// The next dialog the page opens, read the way the program reads it.
fn wait_for_dialog(client: &Client, timeout: Duration) -> blinkterm::dialog::Dialog {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for event in client.events() {
            if event.method == "Page.javascriptDialogOpening" {
                return blinkterm::dialog::Dialog::opening(&event.params)
                    .unwrap_or_else(|| panic!("a dialog of no known kind: {}", event.params));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no dialog opened in {timeout:?}");
}

/// Press `keys` at a dialog until one of them answers it, tell the engine
/// what the program would tell it, and wait for the engine to say the dialog
/// has closed with that answer.
fn answer(
    client: &mut Client,
    mut dialog: blinkterm::dialog::Dialog,
    keys: &[KeyInput],
) -> blinkterm::dialog::Answer {
    use blinkterm::dialog::Answer;
    let mut answered = Answer::Waiting;
    for key in keys {
        answered = dialog.step(key);
        if answered != Answer::Waiting {
            break;
        }
    }
    assert!(
        matches!(answered, Answer::Accept | Answer::Dismiss),
        "{keys:?} did not answer the {:?}",
        dialog.kind
    );
    client
        .call("Page.handleJavaScriptDialog", dialog.reply(answered))
        .expect("the engine takes the answer");

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        for event in client.events() {
            if event.method == "Page.javascriptDialogClosed" {
                assert_eq!(
                    event.params.get("result").and_then(Json::as_bool),
                    Some(answered == Answer::Accept),
                    "the engine closed it with a different answer: {}",
                    event.params
                );
                return answered;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the dialog never said it had closed");
}

fn press(key: Key) -> KeyInput {
    KeyInput::press(key)
}

fn letter(c: char) -> KeyInput {
    KeyInput {
        key: Key::Char(c),
        mods: Mods::default(),
        action: KeyAction::Press,
        text: Some(c),
    }
}

/// `alert()` used to be answered no on the page's behalf and never seen. Now
/// it is seen, it says what it said, and the key that dismisses it is what
/// lets the rest of the script run.
#[test]
fn an_alert_is_seen_and_any_key_lets_the_page_carry_on() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    a_page_that_asks(&mut client, QUIET_PAGE, "ready");

    raise(
        &mut client,
        "alert('saved\\nthree files'); document.title = 'after the alert'",
    );
    let dialog = wait_for_dialog(&client, Duration::from_secs(5));
    assert_eq!(dialog.kind, blinkterm::dialog::Kind::Alert);
    // The newline the page wrote is a space once the event has been read:
    // the message is plain text before it is anything else.
    assert_eq!(dialog.message, "saved three files");
    assert_eq!(dialog.caption(), "alert: saved three files");
    assert!(dialog.url.starts_with("data:text/html"), "{}", dialog.url);

    // Shift on its own is not an answer; the x after it is.
    let shift = KeyInput {
        key: Key::Other(57441),
        mods: Mods(Mods::SHIFT),
        action: KeyAction::Press,
        text: None,
    };
    answer(&mut client, dialog, &[shift, letter('x')]);
    assert_eq!(
        wait_for_title(&mut client, "after the alert", Duration::from_secs(5)),
        "after the alert",
        "the script that opened the alert never finished"
    );

    client.close();
    engine.kill();
}

/// `confirm()` returns what the person said, which is the thing that was
/// broken: a "delete this?" was a silent no.
#[test]
fn a_confirm_answered_both_ways_reaches_the_page() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    a_page_that_asks(&mut client, QUIET_PAGE, "ready");

    for (keys, returned) in [
        (vec![letter('x'), letter('y')], "true"),
        (vec![letter('n')], "false"),
        (vec![press(Key::Escape)], "false"),
        (vec![press(Key::Enter)], "true"),
    ] {
        raise(
            &mut client,
            "document.title = 'confirm ' + confirm('Delete three files?')",
        );
        let dialog = wait_for_dialog(&client, Duration::from_secs(5));
        assert_eq!(dialog.kind, blinkterm::dialog::Kind::Confirm);
        assert_eq!(dialog.caption(), "Delete three files?");
        answer(&mut client, dialog, &keys);
        let wanted = format!("confirm {returned}");
        assert_eq!(
            wait_for_title(&mut client, &wanted, Duration::from_secs(5)),
            wanted,
            "{keys:?}"
        );
    }

    client.close();
    engine.kill();
}

/// `prompt()` returns what was typed, the default when nothing was, and
/// `null` when it was dismissed — the three answers a page can tell apart.
#[test]
fn a_prompt_sends_back_what_was_typed_or_nothing() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    a_page_that_asks(&mut client, QUIET_PAGE, "ready");

    for (keys, returned) in [
        (
            vec![letter('x'), letter('y'), press(Key::Enter)],
            "prompt xy",
        ),
        (vec![press(Key::Enter)], "prompt default"),
        (vec![letter('z'), press(Key::Escape)], "prompt null"),
    ] {
        raise(
            &mut client,
            "document.title = 'prompt ' + prompt('Your name?', 'default')",
        );
        let dialog = wait_for_dialog(&client, Duration::from_secs(5));
        assert_eq!(dialog.kind, blinkterm::dialog::Kind::Prompt);
        assert_eq!(
            dialog.line.text(),
            "default",
            "the page's default is offered"
        );
        assert!(dialog.line.whole(), "and selected");
        answer(&mut client, dialog, &keys);
        assert_eq!(
            wait_for_title(&mut client, returned, Duration::from_secs(5)),
            returned,
            "{keys:?}"
        );
    }

    client.close();
    engine.kill();
}

/// A page with something unsaved asks before it is left, and the answer
/// decides whether it is.
///
/// The navigation goes out with `send`, as `app::navigate` sends it, because
/// this is the case that made it stop being a `call`: the engine holds the
/// reply to `Page.navigate` until the question has been answered, and a
/// program that waited for the reply would never draw the question.
#[test]
fn a_page_that_asks_before_unloading_is_asked_and_answered() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    const DIRTY: &str = "data:text/html,<title>stay</title>\
        <body style='height:100vh'>a form with something typed in it<script>\
        addEventListener('beforeunload', function (e) { e.preventDefault(); e.returnValue = ''; })\
        </script>";
    const AWAY: &str = "data:text/html,<title>left</title><body>";
    a_page_that_asks(&mut client, DIRTY, "stay");

    // Chromium asks only on behalf of a page somebody has touched, so it is
    // touched: a click, dispatched as the program would dispatch one.
    let touch = |client: &mut Client| {
        for (kind, buttons) in [("mousePressed", 1), ("mouseReleased", 0)] {
            client
                .call(
                    "Input.dispatchMouseEvent",
                    Json::object(vec![
                        ("type", Json::string(kind)),
                        ("x", Json::number(40)),
                        ("y", Json::number(20)),
                        ("button", Json::string("left")),
                        ("buttons", Json::number(buttons)),
                        ("clickCount", Json::number(1)),
                        ("modifiers", Json::number(0)),
                    ]),
                )
                .expect("the click is dispatched");
        }
    };
    let reply_to = |client: &Client, pending: &Pending| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(reply) = client.take_reply(pending) {
                return reply;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("Page.navigate was never answered after its dialog was");
    };

    for (key, stays) in [(letter('n'), true), (letter('y'), false)] {
        touch(&mut client);
        let _ = client.events();
        let pending = client
            .send(
                "Page.navigate",
                Json::object(vec![("url", Json::string(AWAY))]),
            )
            .expect("the navigation is sent");
        let dialog = wait_for_dialog(&client, Duration::from_secs(5));
        assert_eq!(dialog.kind, blinkterm::dialog::Kind::BeforeUnload);
        // Whatever the page put in `returnValue`, the engine does not pass it
        // on, which is why the row asks its own question.
        assert_eq!(dialog.message, "");
        assert_eq!(dialog.caption(), "leave this page?");
        assert!(
            client.take_reply(&pending).is_none(),
            "the engine answered the navigation before the question, so it \
             would not have held a program that waited for it"
        );

        answer(&mut client, dialog, &[key]);
        let reply = reply_to(&client, &pending);
        let wanted = if stays { "stay" } else { "left" };
        assert_eq!(
            wait_for_title(&mut client, wanted, Duration::from_secs(5)),
            wanted,
            "the navigation's reply was {reply:?}"
        );
    }

    client.close();
    engine.kill();
}

/// A tab that is not in front can open a dialog too. It is heard — its queue
/// is drained every pass, and the dialog kept on the tab — and it is still
/// there to be answered when the person goes to it.
#[test]
fn a_dialog_on_a_tab_that_is_not_in_front_is_still_heard() {
    let Some((mut engine, mut browser, mut tabs)) = two_tabs() else {
        return;
    };
    assert_eq!(tabs.active_index(), 1, "the second tab is in front");
    {
        let first = tabs.get_mut(0).expect("the first tab");
        let _ = first.connection.events();
        raise(
            &mut first.connection,
            "document.title = 'confirm ' + confirm('Leave the others?')",
        );
    }

    // What `app::handle_page_events` does with every tab's queue, with the
    // drawing left out.
    let deadline = Instant::now() + Duration::from_secs(5);
    while tabs.iter().all(|tab| tab.dialog.is_none()) {
        assert!(
            Instant::now() < deadline,
            "the tab behind never said it had a question"
        );
        for index in 0..tabs.len() {
            let tab = tabs.get_mut(index).expect("a tab");
            for event in tab.connection.events() {
                tab.dialog_event(&event);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let asking: Vec<bool> = tabs.iter().map(|tab| tab.dialog.is_some()).collect();
    assert_eq!(
        asking,
        [true, false],
        "the question is on the tab that asked"
    );
    assert_eq!(
        tabs.active_index(),
        1,
        "and asking did not bring it forward"
    );

    // The person goes to it, and answers.
    assert!(tabs.select(1));
    let tab = tabs.active_mut().expect("the first tab");
    let dialog = tab.dialog.take().expect("still waiting");
    assert_eq!(dialog.caption(), "Leave the others?");
    answer(&mut tab.connection, dialog, &[letter('y')]);
    assert_eq!(
        wait_for_title(&mut tab.connection, "confirm", Duration::from_secs(5)),
        "confirm true"
    );

    browser.close();
    drop(tabs);
    engine.kill();
}

// ---------------------------------------------------------------------------
// File inputs
// ---------------------------------------------------------------------------

use blinkterm::upload::{Chooser, Disk, Outcome as Typed, Upload};

/// A page whose file input sits in the first pixels of the page, and which
/// writes into its title what it was given — each file's name and size, as a
/// page reads them — or that it heard `cancel`.
fn upload_page(multiple: bool) -> String {
    format!(
        "data:text/html,<title>ready</title><body style='margin:0'>\
<input id=f type=file {} style='position:absolute;left:0;top:0;width:200px;height:32px'>\
<script>f.addEventListener('change',function(){{var a=[];\
for(var x of f.files)a.push(x.name+' '+x.size);document.title='files '+a.join(', ')}});\
f.addEventListener('cancel',function(){{document.title='cancelled'}});</script></body>",
        if multiple { "multiple" } else { "" }
    )
}

/// What `/report.pdf` holds for these tests, and so what size the page says.
const UPLOADED: &[u8] = b"%PDF-1.4 a small report";

/// A page with its file inputs asked about, as `app::connect_tab` sets one
/// up, and a directory with two files in it to choose from.
fn an_upload_page(client: &mut Client, multiple: bool) -> std::path::PathBuf {
    a_page_that_asks(client, &upload_page(multiple), "ready");
    client
        .call(
            "Page.setInterceptFileChooserDialog",
            Json::object(vec![("enabled", Json::Bool(true))]),
        )
        .expect("interception");
    let dir = temp_dir(if multiple { "uploads" } else { "upload" });
    std::fs::write(dir.join("report.pdf"), UPLOADED).expect("a file");
    std::fs::write(dir.join("notes.txt"), b"one line\n").expect("a file");
    dir
}

/// A left click at a point of the page, pressed and let go, as `send_mouse`
/// sends one. The click is what gives the page the activation a chooser
/// needs: from a script with none, the engine refuses to open one.
fn click_at(client: &mut Client, x: u32, y: u32) {
    for (kind, buttons) in [("mousePressed", 1), ("mouseReleased", 0)] {
        client
            .call(
                "Input.dispatchMouseEvent",
                Json::object(vec![
                    ("type", Json::string(kind)),
                    ("x", Json::number(x)),
                    ("y", Json::number(y)),
                    ("button", Json::string("left")),
                    ("buttons", Json::number(buttons)),
                    ("clickCount", Json::number(1)),
                    ("modifiers", Json::number(0)),
                ]),
            )
            .expect("the click is dispatched");
    }
}

/// The next file input the page asks about, read the way the program reads
/// it.
fn wait_for_chooser(client: &Client, timeout: Duration) -> Chooser {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for event in client.events() {
            if event.method == "Page.fileChooserOpened" {
                return Chooser::opening(&event.params)
                    .unwrap_or_else(|| panic!("a chooser with no input: {}", event.params));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no file chooser opened in {timeout:?}");
}

/// Keys at an upload prompt, until one of them ends it.
fn type_path(upload: &mut Upload, keys: &[KeyInput]) -> Typed {
    let mut outcome = Typed::Waiting;
    for key in keys {
        outcome = upload.step(key, &Disk);
        if outcome != Typed::Waiting {
            break;
        }
    }
    outcome
}

fn letters(text: &str) -> Vec<KeyInput> {
    text.chars().map(letter).collect()
}

/// A `<input type=file>` used to be a click that did nothing — headless has
/// no picker, and the engine told the page `cancel` at once. Now the click
/// is a path on the row, and what is typed and confirmed reaches the page as
/// the file: its name and its size, read the way the page reads them. And
/// Escape sends nothing, and the page hears `cancel` as it would from a real
/// chooser.
#[test]
fn a_file_typed_on_the_row_reaches_the_pages_file_input() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let dir = an_upload_page(&mut client, false);

    click_at(&mut client, 8, 8);
    let chooser = wait_for_chooser(&client, Duration::from_secs(5));
    assert!(!chooser.multiple);

    // `rep`, Tab completes to `report.pdf`, Enter sends.
    let mut upload = Upload::new(chooser, dir.clone(), None);
    let mut keys = letters("rep");
    keys.extend([press(Key::Tab), press(Key::Enter)]);
    assert_eq!(type_path(&mut upload, &keys), Typed::Send);
    assert_eq!(upload.line.text(), format!("{}/report.pdf", dir.display()));
    client
        .call("DOM.setFileInputFiles", upload.reply())
        .expect("the engine takes the file");
    let wanted = format!("files report.pdf {}", UPLOADED.len());
    assert_eq!(
        wait_for_title(&mut client, &wanted, Duration::from_secs(5)),
        wanted
    );

    // Escape: nothing sent, and the page hears cancel.
    click_at(&mut client, 8, 8);
    let chooser = wait_for_chooser(&client, Duration::from_secs(5));
    let mut upload = Upload::new(chooser, dir.clone(), None);
    let keys = [letter('n'), press(Key::Escape)];
    assert_eq!(type_path(&mut upload, &keys), Typed::Cancel);
    blinkterm::app::cancel_chooser(&mut client, upload.chooser.backend_node_id);
    assert_eq!(
        wait_for_title(&mut client, "cancelled", Duration::from_secs(5)),
        "cancelled"
    );
    assert_eq!(
        evaluate(&mut client, "f.files.length"),
        Json::number(1),
        "the file sent before is untouched"
    );

    std::fs::remove_dir_all(&dir).ok();
    client.close();
    engine.kill();
}

/// A `multiple` input is asked once per file, and an Enter with nothing
/// typed sends what was entered, in the order it was.
#[test]
fn a_multiple_input_takes_every_path_entered_until_an_empty_enter() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let dir = an_upload_page(&mut client, true);

    click_at(&mut client, 8, 8);
    let chooser = wait_for_chooser(&client, Duration::from_secs(5));
    assert!(chooser.multiple);

    let mut upload = Upload::new(chooser, dir.clone(), None);
    let mut keys = letters("report.pdf");
    keys.push(press(Key::Enter));
    keys.extend(letters("notes.txt"));
    keys.extend([press(Key::Enter), press(Key::Enter)]);
    assert_eq!(type_path(&mut upload, &keys), Typed::Send);
    assert_eq!(upload.taken.len(), 2);
    client
        .call("DOM.setFileInputFiles", upload.reply())
        .expect("the engine takes the files");
    let wanted = format!("files report.pdf {}, notes.txt 9", UPLOADED.len());
    assert_eq!(
        wait_for_title(&mut client, &wanted, Duration::from_secs(5)),
        wanted
    );

    std::fs::remove_dir_all(&dir).ok();
    client.close();
    engine.kill();
}

use blinkterm::download::{self, Downloads};

/// What `/report.pdf` sends, which is what the saved file must hold.
const REPORT: &[u8] = b"%PDF-1.4 hello report\n";

/// How big `/big` is, and how it is paced: 128 kB every 100 ms, so that the
/// whole takes about a second and a half and the engine's half-second
/// progress events land in the middle of it.
const BIG: usize = 2 * 1024 * 1024;
const BIG_STEP: usize = 128 * 1024;

/// A server with files to offer: a page with a link to one, the file, a big
/// one that comes slowly, and one that breaks off. Every connection has a
/// thread of its own, because the slow one would otherwise hold up the engine
/// asking for anything else. The base comes back without a trailing slash.
fn serve_files() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to serve on");
    let address = listener.local_addr().expect("an address");
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut head = [0u8; 2048];
                let read = stream.read(&mut head).unwrap_or(0);
                let request = String::from_utf8_lossy(&head[..read]).to_string();
                let path = request.split(' ').nth(1).unwrap_or("/").to_string();
                let attachment = |name: &str, kind: &str, length: usize| {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\n\
                         Content-Disposition: attachment; filename=\"{name}\"\r\n\
                         Content-Length: {length}\r\nConnection: close\r\n\r\n"
                    )
                };
                match path.as_str() {
                    "/report.pdf" => {
                        let head = attachment("report.pdf", "application/pdf", REPORT.len());
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(REPORT);
                    }
                    "/big" => {
                        let head = attachment("big.bin", "application/octet-stream", BIG);
                        let _ = stream.write_all(head.as_bytes());
                        let chunk = vec![b'b'; BIG_STEP];
                        for _ in 0..BIG / BIG_STEP {
                            if stream.write_all(&chunk).is_err() {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                    "/broken" => {
                        let head =
                            attachment("broken.bin", "application/octet-stream", 4 * 1024 * 1024);
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(&vec![b'x'; 100_000]);
                        // And the socket closes with four megabytes promised.
                    }
                    _ => {
                        let body = "<!doctype html><title>page</title>\
                             <body style='margin:0'><a href='/report.pdf' \
                             style='display:block;position:absolute;left:0;top:0;\
                             width:240px;height:80px;background:#cc3'>report</a>";
                        let answer = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(answer.as_bytes());
                    }
                }
            });
        }
    });
    format!("http://{address}")
}

/// A directory for downloads to go to that does not exist yet, under one
/// that does and that the test removes: never the person's `~/Downloads`.
fn download_dir(what: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "blinkterm-it-download-{what}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("a directory");
    let dir = base.join("Downloads");
    (base, dir)
}

/// An engine told to save into `dir`, as `app::run` tells it, with its tab
/// ready the way the program has one: enabled and sized.
fn downloading(dir: &std::path::Path) -> Option<(Engine, Client, Tabs<Client>, Downloads)> {
    let (engine, page, target) = connect_with_target()?;
    let (mut browser, mut tabs) = tabbed(&engine, page, target);
    download::enable(&mut browser, dir).expect("the engine is told where");
    let tab = tabs.active_mut().expect("the tab");
    tab.connection
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut tab.connection);
    Some((engine, browser, tabs, Downloads::new(dir.to_path_buf())))
}

/// [`pump`], with what `app::handle_target_events` does with a download's
/// events first: every event the browser connection has is handed to the
/// downloads, and `seen` is told what the row says each time the downloads
/// say it changed. Until `done` or the time is up.
fn pump_downloads(
    browser: &mut Client,
    tabs: &mut Tabs<Client>,
    downloads: &mut Downloads,
    timeout: Duration,
    mut seen: impl FnMut(Option<String>),
    done: impl Fn(&Downloads) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        for event in browser.events() {
            if downloads.take(&event, Instant::now()) {
                seen(downloads.line(Instant::now()));
            }
            let outcome = tabs.take(&event, |target| {
                browser.attach(target, Duration::from_secs(5))
            });
            if let Outcome::Gone { mut tab, .. } = outcome {
                tab.connection.close();
            }
        }
        if done(downloads) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The names in a directory, sorted; none for one that is not there.
fn names_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The names in a directory once it has emptied, or when the time is up.
///
/// The engine removes a cancelled download's partial file itself, but after
/// it has sent `canceled` rather than before — measured, the file is still
/// there when the event is read — so an empty directory is waited for.
fn emptied(dir: &std::path::Path, timeout: Duration) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let names = names_in(dir);
        if names.is_empty() || Instant::now() >= deadline {
            return names;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn saved(downloads: &Downloads) -> bool {
    downloads
        .line(Instant::now())
        .is_some_and(|line| line.starts_with("saved "))
}

/// The acceptance test for #10: a url that is a file is saved, under its own
/// name, with what the server sent, and the page it was asked for from stays
/// where it was — typed, typed again, and clicked.
#[test]
fn an_attachment_is_saved_under_its_own_name_with_its_contents() {
    use std::os::unix::fs::PermissionsExt;
    let (base, dir) = download_dir("attachment");
    let Some((mut engine, mut browser, mut tabs, mut downloads)) = downloading(&dir) else {
        return;
    };
    let server = serve_files();
    let page = format!("{server}/page");
    {
        let tab = tabs.active_mut().expect("the tab");
        navigate_tab(tab, &page);
        follow(tab, Duration::from_secs(10));
        assert_eq!(tab.url, page);
    }

    for (round, wanted) in ["report.pdf", "report (1).pdf"].into_iter().enumerate() {
        let tab = tabs.active_mut().expect("the tab");
        let reply = navigate_tab(tab, &format!("{server}/report.pdf"));
        assert!(download::became_download(&reply), "{reply:?}");
        // What `app::navigated` did with that reply: the page stayed, so the
        // loading note is off and the url is the page's again.
        assert_eq!(tab.note, None);
        assert!(!tab.loading);
        assert_eq!(tab.url, page);
        assert_eq!(tab.problem, None);

        assert!(
            pump_downloads(
                &mut browser,
                &mut tabs,
                &mut downloads,
                Duration::from_secs(10),
                |_| {},
                saved
            ),
            "round {round}: never saved: {:?}",
            downloads.line(Instant::now())
        );
        assert_eq!(
            std::fs::read(dir.join(wanted)).expect(wanted),
            REPORT,
            "{wanted}"
        );
        // So that the next round's `saved` is its own.
        downloads.expire(Instant::now() + download::NOTICE_FOR);
    }
    let mode = std::fs::metadata(&dir)
        .expect("the directory")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700, "{mode:o}");

    // A click on the link: the way most files are asked for.
    {
        let tab = tabs.active_mut().expect("the tab");
        let _ = tab.connection.events();
        for (kind, buttons) in [("mousePressed", 1), ("mouseReleased", 0)] {
            tab.connection
                .call(
                    "Input.dispatchMouseEvent",
                    Json::object(vec![
                        ("type", Json::string(kind)),
                        ("x", Json::number(40)),
                        ("y", Json::number(20)),
                        ("button", Json::string("left")),
                        ("buttons", Json::number(buttons)),
                        ("clickCount", Json::number(1)),
                        ("modifiers", Json::number(0)),
                    ]),
                )
                .expect("the click is dispatched");
        }
    }
    assert!(
        pump_downloads(
            &mut browser,
            &mut tabs,
            &mut downloads,
            Duration::from_secs(10),
            |_| {},
            saved
        ),
        "the click saved nothing: {:?}",
        downloads.line(Instant::now())
    );
    assert_eq!(
        std::fs::read(dir.join("report (2).pdf")).expect("the third"),
        REPORT
    );
    let tab = tabs.active_mut().expect("the tab");
    let events = tab.connection.events();
    let navigated: Vec<_> = events
        .iter()
        .filter(|event| event.method == "Page.frameNavigated")
        .collect();
    assert!(
        navigated.is_empty(),
        "the page went somewhere: {navigated:?}"
    );
    // The click was announced as a departure, and the departure that turned
    // into a file ends like any load: nothing on the row says it is still
    // going (#15).
    for event in &events {
        match event.method.as_str() {
            "Page.frameStartedNavigating" => {
                if let Some(url) = load::started(&event.params, None) {
                    tab.started(url, Instant::now());
                }
            }
            "Page.frameStoppedLoading" => tab.stopped_loading(),
            _ => {}
        }
    }
    let methods: Vec<&str> = events.iter().map(|event| event.method.as_str()).collect();
    eprintln!("the click that became a file: {methods:?}");
    assert!(!tab.loading, "{methods:?}");
    assert_eq!(tab.note, None, "{methods:?}");
    assert_eq!(tab.url, page);

    // Only the three, under their names: no guid left over, no partial.
    assert_eq!(
        names_in(&dir),
        ["report (1).pdf", "report (2).pdf", "report.pdf"]
    );

    browser.close();
    drop(tabs);
    engine.kill();
    let _ = std::fs::remove_dir_all(&base);
}

/// A download that takes a while says so while it does, and the row is only
/// redrawn when what it says has changed.
#[test]
fn a_download_says_how_far_it_has_come_and_then_where_it_went() {
    let (base, dir) = download_dir("progress");
    let Some((mut engine, mut browser, mut tabs, mut downloads)) = downloading(&dir) else {
        return;
    };
    let server = serve_files();
    let tab = tabs.active_mut().expect("the tab");
    let reply = navigate_tab(tab, &format!("{server}/big"));
    assert!(download::became_download(&reply), "{reply:?}");

    let mut lines: Vec<Option<String>> = Vec::new();
    assert!(
        pump_downloads(
            &mut browser,
            &mut tabs,
            &mut downloads,
            Duration::from_secs(15),
            |line| lines.push(line),
            saved
        ),
        "never saved: {lines:?}"
    );
    let percents: Vec<u64> = lines
        .iter()
        .flatten()
        .filter_map(|line| {
            line.strip_prefix("downloading big.bin ")?
                .strip_suffix('%')?
                .parse()
                .ok()
        })
        .collect();
    assert!(
        percents.iter().any(|&n| n > 0 && n < 100),
        "nothing between the start and the end: {lines:?}"
    );
    assert!(
        percents.windows(2).all(|pair| pair[0] <= pair[1]),
        "went backwards: {lines:?}"
    );
    assert!(
        lines.windows(2).all(|pair| pair[0] != pair[1]),
        "redrawn with nothing new to say: {lines:?}"
    );
    let last = lines.last().cloned().flatten().expect("a last word");
    assert!(
        last.starts_with("saved ") && last.ends_with("/big.bin"),
        "{last}"
    );
    assert_eq!(
        std::fs::metadata(dir.join("big.bin"))
            .expect("the file")
            .len(),
        BIG as u64
    );

    browser.close();
    drop(tabs);
    engine.kill();
    let _ = std::fs::remove_dir_all(&base);
}

/// A server that stops sending halfway: the engine tries again, gives up,
/// and says `canceled`; the row says the file did not arrive and nothing of
/// it is left.
#[test]
fn a_download_that_breaks_off_is_reported_and_leaves_nothing() {
    let (base, dir) = download_dir("broken");
    let Some((mut engine, mut browser, mut tabs, mut downloads)) = downloading(&dir) else {
        return;
    };
    let server = serve_files();
    let tab = tabs.active_mut().expect("the tab");
    navigate_tab(tab, &format!("{server}/broken"));
    assert!(
        pump_downloads(
            &mut browser,
            &mut tabs,
            &mut downloads,
            Duration::from_secs(10),
            |_| {},
            |downloads| !downloads.all().is_empty() && downloads.in_flight().next().is_none()
        ),
        "never ended: {:?}",
        downloads.line(Instant::now())
    );
    assert_eq!(
        downloads.line(Instant::now()).as_deref(),
        Some("couldn't save broken.bin")
    );
    assert_eq!(
        emptied(&dir, Duration::from_secs(2)),
        Vec::<String>::new(),
        "the engine left its partial file"
    );

    browser.close();
    drop(tabs);
    engine.kill();
    let _ = std::fs::remove_dir_all(&base);
}

/// What the way out does: cancelled, the engine removes its own partial and
/// the row does not call that a failure; killed without a cancel — the
/// temporary profile's way out, or an engine that stopped answering — the
/// partials this run saw begin are removed by name afterwards.
#[test]
fn quitting_with_a_download_coming_leaves_no_partial_file() {
    let (base, dir) = download_dir("quit");
    let Some((mut engine, mut browser, mut tabs, mut downloads)) = downloading(&dir) else {
        return;
    };
    let server = serve_files();
    let start = |tabs: &mut Tabs<Client>, browser: &mut Client, downloads: &mut Downloads| {
        let tab = tabs.active_mut().expect("the tab");
        navigate_tab(tab, &format!("{server}/big"));
        assert!(
            pump_downloads(
                browser,
                tabs,
                downloads,
                Duration::from_secs(10),
                |_| {},
                |downloads| downloads.in_flight().any(|download| download.received > 0)
            ),
            "the download never started"
        );
    };

    start(&mut tabs, &mut browser, &mut downloads);
    downloads.cancel_all(&mut browser);
    pump_downloads(
        &mut browser,
        &mut tabs,
        &mut downloads,
        Duration::from_secs(2),
        |_| {},
        |downloads| downloads.in_flight().next().is_none(),
    );
    assert_eq!(downloads.line(Instant::now()), None, "not a failure");
    assert_eq!(
        emptied(&dir, Duration::from_secs(2)),
        Vec::<String>::new(),
        "the engine left its partial file"
    );
    assert_eq!(
        downloads.partials().len(),
        1,
        "still named, for an engine killed before it got round to it"
    );
    browser.close();
    drop(tabs);
    engine.kill();

    let Some((mut engine, mut browser, mut tabs, mut downloads)) = downloading(&dir) else {
        return;
    };
    start(&mut tabs, &mut browser, &mut downloads);
    let partials = downloads.partials();
    assert_eq!(partials.len(), 1, "{partials:?}");
    browser.close();
    drop(tabs);
    engine.kill();
    for partial in partials {
        let _ = std::fs::remove_file(partial);
    }
    assert_eq!(names_in(&dir), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------------------
// Enter
// ---------------------------------------------------------------------------

/// Enter does what Enter does on a page: it submits the form a text field is
/// in, and it starts a new line in a textarea.
///
/// Both are the engine's default actions for a `keypress` of `"\r"`, not for
/// the `keydown` a page's own listener sees, and Chromium only makes the one
/// out of the other when the key comes with text. The terminal reports Enter
/// with none — it is `Key::Enter`, `text: None`, as `\r` or `CSI 13 u` — so
/// this is sent exactly the way the program sends it, press and release.
#[test]
fn enter_submits_a_form_and_breaks_a_line_in_a_textarea() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let page = "data:text/html,<title>ready</title><form onsubmit=\"document.title='submitted';return false\">\
<input id=i autofocus></form><textarea id=t></textarea>";
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(page))]),
        )
        .expect("the page loads");
    wait_for_title(&mut client, "ready", Duration::from_secs(10));

    let press = |action| KeyInput {
        key: Key::Enter,
        mods: Mods::default(),
        action,
        text: None,
    };
    let enter = |client: &mut Client| {
        for action in [KeyAction::Press, KeyAction::Release] {
            let params = keys::dispatch(&press(action)).expect("Enter has a name");
            client
                .call("Input.dispatchKeyEvent", params)
                .expect("the key is dispatched");
        }
    };
    let evaluate = |client: &mut Client, expression: &str| {
        client
            .call(
                "Runtime.evaluate",
                Json::object(vec![
                    ("expression", Json::string(expression)),
                    ("returnByValue", Json::Bool(true)),
                ]),
            )
            .expect("the page answers")
    };

    evaluate(&mut client, "document.getElementById('i').focus()");
    enter(&mut client);
    assert_eq!(
        wait_for_title(&mut client, "submitted", Duration::from_secs(5)),
        "submitted",
        "Enter in a text field submits its form"
    );

    evaluate(
        &mut client,
        "var t=document.getElementById('t');t.focus();t.value='a';t.setSelectionRange(1,1)",
    );
    enter(&mut client);
    let value = evaluate(&mut client, "document.getElementById('t').value");
    assert_eq!(
        value
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(Json::as_str),
        Some("a\n"),
        "Enter in a textarea starts a new line"
    );

    client.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// Find in page (#12): the script in `blinkterm::find`, run the way the
// program runs it — in an isolated world, through `Runtime.callFunctionOn` —
// against pages served over HTTP, with what the page and a screenshot say
// afterwards as the evidence.

/// Serve the pages `build` makes, given the port they will be served on, for
/// as long as the test binary runs; the port comes back.
///
/// A thread per connection, unlike [`serve`]: Chromium opens a speculative
/// second connection that sends nothing, and with one thread its read held up
/// every request after it — which is what the first measurement of find ran
/// into. A path nobody made is a 404.
fn serve_pages(build: impl FnOnce(u16) -> Vec<(String, String)>) -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to serve on");
    let port = listener.local_addr().expect("an address").port();
    let pages = Arc::new(build(port));
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let pages = Arc::clone(&pages);
            std::thread::spawn(move || {
                let mut head = [0u8; 2048];
                let read = stream.read(&mut head).unwrap_or(0);
                let request = String::from_utf8_lossy(&head[..read]).to_string();
                let path = request.split(' ').nth(1).unwrap_or("/").to_string();
                let page = pages.iter().find(|(served, _)| *served == path);
                let (status, body) = match page {
                    Some((_, body)) => ("200 OK", body.as_str()),
                    None => ("404 Not Found", "<title>not found</title>"),
                };
                let answer = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes());
            });
        }
    });
    port
}

/// A page with `fox` where a person can see it six times — three spellings in
/// one paragraph, one split across `<b>`, two far down — and where nobody can
/// in four more: a closed `<details>`, `display:none`, `visibility:hidden`
/// and a `<textarea>`. The page `find`'s module doc was measured on.
fn fox_page() -> String {
    let filler: String = (0..200)
        .map(|i| format!("<p>filler line {i}</p>"))
        .collect();
    format!(
        "<!doctype html><meta charset=utf-8><title>foxes</title>\
         <body style='margin:0;font:16px monospace;background:#fff;color:#000'>\
         <h1>Top of the page</h1>\
         <p>The quick brown fox jumps over the lazy dog. A Fox is here too, and a FOX.</p>\
         <details><summary>closed</summary><p>hidden fox inside details</p></details>\
         <p style='display:none'>display none fox</p>\
         <p style='visibility:hidden'>visibility hidden fox</p>\
         <textarea>fox in a textarea</textarea>\
         <p>Split <b>fo</b>x across nodes.</p>\
         {filler}\
         <p id=deep>A deep fox near the bottom.</p>\
         <p>and the last fox.</p></body>"
    )
}

/// An engine on a page this test serves, sized as the pane would be, with the
/// find script's world made in it.
fn finding(url: &str, title: &str) -> Option<(Engine, Client, i64)> {
    let (engine, mut client) = connect()?;
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    open(&mut client, url, title);
    let context = find_world(&mut client);
    Some((engine, client, context))
}

/// Go to `url` and wait for it to call itself `title`.
fn open(client: &mut Client, url: &str, title: &str) {
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(url))]),
        )
        .expect("the page loads");
    assert_eq!(
        wait_for_title(client, title, Duration::from_secs(15)),
        title,
        "{url}"
    );
}

/// The world the program makes on `ctrl+f`: the main frame, then a world of
/// [`find::WORLD`]'s name in it.
fn find_world(client: &mut Client) -> i64 {
    let tree = client
        .call("Page.getFrameTree", Json::empty())
        .expect("the frame tree");
    let frame = find::main_frame(&tree).expect("a main frame");
    let world = client
        .call("Page.createIsolatedWorld", find::world_params(&frame))
        .expect("an isolated world");
    find::context(&world).expect("the world's id")
}

/// One search or step, waited for, as the program sends it.
fn search(client: &mut Client, context: i64, needle: &str, step: i32) -> Matches {
    let ask = find::Ask {
        needle: needle.to_string(),
        step,
    };
    let reply = client
        .call_within(
            "Runtime.callFunctionOn",
            find::call_params(context, &ask),
            Duration::from_secs(5),
        )
        .expect("the page answers the search");
    find::matches(&reply).unwrap_or_else(|| panic!("an answer of the script's shape: {reply}"))
}

/// How many yellow (`#ff0`, every match) and orange (`#f80`, the current
/// one) pixels a PNG has, within 8 of each channel, inside `area` — `(x, y,
/// width, height)` — or everywhere.
fn highlight_pixels(png: &[u8], area: Option<(u32, u32, u32, u32)>) -> (usize, usize) {
    let image = tos_term::png::decode(png, 64 * 1024 * 1024).expect("the PNG decodes");
    let (x0, y0, w, h) = area.unwrap_or((0, 0, image.width, image.height));
    let near = |pixel: &[u8], rgb: [u8; 3]| {
        pixel
            .iter()
            .zip(rgb)
            .all(|(&have, want)| have.abs_diff(want) <= 8)
    };
    let (mut yellow, mut orange) = (0, 0);
    for y in y0..(y0 + h).min(image.height) {
        for x in x0..(x0 + w).min(image.width) {
            let at = ((y * image.width + x) * 4) as usize;
            let pixel = &image.rgba[at..at + 3];
            if near(pixel, [0xff, 0xff, 0x00]) {
                yellow += 1;
            } else if near(pixel, [0xff, 0x88, 0x00]) {
                orange += 1;
            }
        }
    }
    (yellow, orange)
}

/// A number the page works out.
fn page_number(client: &mut Client, expression: &str) -> f64 {
    evaluate(client, expression)
        .as_f64()
        .unwrap_or_else(|| panic!("{expression} is a number"))
}

/// The acceptance criterion of #12: a needle is counted, and a match below the
/// fold is brought into view — checked by where the page is, not by trusting
/// the script's own word for it.
#[test]
fn a_needle_is_counted_and_the_current_match_is_scrolled_into_view() {
    let port = serve_pages(|_| {
        let paragraphs: String = (0..300)
            .map(|i| {
                format!(
                    "<p id=p{i}>{i} The quick brown fox jumps over the lazy dog; \
                     Lorem ipsum dolor sit amet, <b>consectetur</b> adipiscing elit.</p>"
                )
            })
            .collect();
        vec![(
            "/".to_string(),
            format!(
                "<!doctype html><meta charset=utf-8><title>paragraphs</title>\
                 <body style='font:14px sans-serif;background:#fff'>{paragraphs}</body>"
            ),
        )]
    });
    let url = format!("http://127.0.0.1:{port}/");
    let Some((mut engine, mut client, context)) = finding(&url, "paragraphs") else {
        return;
    };

    let fox = search(&mut client, context, "fox", 0);
    assert_eq!((fox.count, fox.current), (300, 1));
    assert!(fox.highlighted, "the Custom Highlight API is there");
    assert_eq!(scroll_y(&mut client), 0.0, "the first is already in view");

    let far = search(&mut client, context, "250 The", 0);
    assert_eq!((far.count, far.current), (1, 1));
    assert!(scroll_y(&mut client) > 0.0, "the page moved to it");
    let top = page_number(
        &mut client,
        "document.getElementById('p250').getBoundingClientRect().top",
    );
    assert!(
        (0.0..HEIGHT as f64).contains(&top),
        "paragraph 250 is on the screen: its top is at {top}"
    );
    let (_, orange) = highlight_pixels(&screenshot(&mut client, "png", None), None);
    assert!(orange > 0, "the current match is painted");

    client.close();
    engine.kill();
}

#[test]
fn next_and_previous_walk_the_matches_and_wrap() {
    let port = serve_pages(|_| vec![("/".to_string(), fox_page())]);
    let url = format!("http://127.0.0.1:{port}/");
    let Some((mut engine, mut client, context)) = finding(&url, "foxes") else {
        return;
    };

    let first = search(&mut client, context, "fox", 0);
    assert_eq!((first.count, first.current), (6, 1));
    assert_eq!(scroll_y(&mut client), 0.0);
    let mut went = Vec::new();
    for _ in 2..=6 {
        let step = search(&mut client, context, "fox", 1);
        went.push((step.current, scroll_y(&mut client)));
    }
    let currents: Vec<u32> = went.iter().map(|(current, _)| *current).collect();
    assert_eq!(currents, [2, 3, 4, 5, 6]);
    // The first four are on the first screen; the fifth is two hundred lines
    // down, and the page goes to it.
    assert_eq!(went[2].1, 0.0, "{went:?}");
    assert!(went[3].1 > 0.0, "the fifth scrolled the page: {went:?}");
    let wrapped = search(&mut client, context, "fox", 1);
    assert_eq!(wrapped.current, 1, "past the last is the first");
    assert_eq!(scroll_y(&mut client), 0.0, "and the page went back up");
    let back = search(&mut client, context, "fox", -1);
    assert_eq!(back.current, 6, "before the first is the last");
    assert!(scroll_y(&mut client) > 0.0);
    // A step of any size wraps, either way.
    assert_eq!(search(&mut client, context, "fox", 13).current, 1);
    assert_eq!(search(&mut client, context, "fox", -7).current, 6);

    client.close();
    engine.kill();
}

#[test]
fn hidden_text_is_not_a_match_and_the_page_is_left_as_it_was() {
    let port = serve_pages(|_| vec![("/".to_string(), fox_page())]);
    let url = format!("http://127.0.0.1:{port}/");
    let Some((mut engine, mut client, context)) = finding(&url, "foxes") else {
        return;
    };
    let before = evaluate(&mut client, "document.body.innerHTML");
    let selected = evaluate(
        &mut client,
        "var r=document.createRange();r.selectNodeContents(document.querySelector('h1'));\
         getSelection().removeAllRanges();getSelection().addRange(r);String(getSelection())",
    );
    assert_eq!(selected.as_str(), Some("Top of the page"));

    let fox = search(&mut client, context, "fox", 0);
    assert_eq!(
        fox.count, 6,
        "details, display:none, visibility:hidden and the textarea are not matches; \
         the split fo|x is"
    );
    let (yellow, orange) = highlight_pixels(&screenshot(&mut client, "png", None), None);
    assert!(yellow > 0 && orange > 0, "{yellow} yellow, {orange} orange");

    // Nothing in the page changed, and nothing of the script is in its world.
    assert_eq!(evaluate(&mut client, "document.body.innerHTML"), before);
    assert_eq!(
        evaluate(&mut client, "typeof __blinktermFind").as_str(),
        Some("undefined")
    );
    assert_eq!(
        evaluate(&mut client, "String(getSelection())").as_str(),
        Some("Top of the page"),
        "the person's selection is theirs"
    );
    // Whitespace as the page shows it, and never across a block.
    assert_eq!(search(&mut client, context, "lazy  dog", 0).count, 0);
    assert_eq!(search(&mut client, context, "lazy dog", 0).count, 1);
    assert_eq!(search(&mut client, context, "page the", 0).count, 0);

    let cleared = search(&mut client, context, "", 0);
    assert_eq!((cleared.count, cleared.current), (0, 0));
    assert_eq!(
        evaluate(&mut client, "CSS.highlights.size").as_f64(),
        Some(0.0)
    );
    assert_eq!(
        evaluate(&mut client, "document.adoptedStyleSheets.length").as_f64(),
        Some(0.0)
    );
    let (yellow, orange) = highlight_pixels(&screenshot(&mut client, "png", None), None);
    assert_eq!((yellow, orange), (0, 0), "nothing left painted");
    assert_eq!(evaluate(&mut client, "document.body.innerHTML"), before);

    client.close();
    engine.kill();
}

#[test]
fn cjk_text_is_found_counted_and_highlighted() {
    let port = serve_pages(|_| {
        vec![(
            "/".to_string(),
            "<!doctype html><meta charset=utf-8><title>nihongo</title>\
             <body style='margin:0;font:20px sans-serif;background:#fff'>\
             <p>東京は日本の首都です。</p>\
             <p>日本語のテキストです。東京タワー。</p>\
             <p>京都と東京と大阪。日本。</p>\
             <p>TOKYO in capitals.</p></body>"
                .to_string(),
        )]
    });
    let url = format!("http://127.0.0.1:{port}/");
    let Some((mut engine, mut client, context)) = finding(&url, "nihongo") else {
        return;
    };

    assert_eq!(search(&mut client, context, "東京", 0).count, 3);
    assert_eq!(search(&mut client, context, "日本", 0).count, 3);
    assert_eq!(search(&mut client, context, "日本の首都", 0).count, 1);
    // A glyph box paints its background whether or not a font drew the glyph.
    let (yellow, orange) = highlight_pixels(&screenshot(&mut client, "png", None), None);
    assert!(yellow + orange > 0, "the match is painted");
    assert_eq!(
        search(&mut client, context, "tokyo", 0).count,
        1,
        "any case"
    );

    client.close();
    engine.kill();
}

/// Why a search is sent rather than called, and what the program does when
/// the document it was searching has gone.
#[test]
fn a_search_is_collected_on_a_later_pass_and_a_stale_world_is_told_apart() {
    let port = serve_pages(|_| {
        let paragraphs: String = (0..2000)
            .map(|i| {
                format!(
                    "<p>{i} The quick brown fox jumps over the lazy dog; Lorem ipsum \
                     dolor sit amet, <b>consectetur</b> adipiscing elit.</p>"
                )
            })
            .collect();
        vec![
            (
                "/".to_string(),
                format!("<!doctype html><title>long</title><body>{paragraphs}</body>"),
            ),
            (
                "/other".to_string(),
                "<!doctype html><title>other</title><body><p>a fox elsewhere</p></body>"
                    .to_string(),
            ),
        ]
    });
    let url = format!("http://127.0.0.1:{port}/");
    let Some((mut engine, mut client, context)) = finding(&url, "long") else {
        return;
    };

    let ask = find::Ask {
        needle: "fox".to_string(),
        step: 0,
    };
    let pending: Pending = client
        .send("Runtime.callFunctionOn", find::call_params(context, &ask))
        .expect("the search goes out");
    assert!(
        client.take_reply(&pending).is_none(),
        "a walk of 2000 paragraphs is not back the moment it was sent"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let answer = loop {
        if let Some(answer) = client.take_reply(&pending) {
            break answer;
        }
        assert!(Instant::now() < deadline, "the search answered within 5 s");
        std::thread::sleep(Duration::from_millis(5));
    };
    let fox = find::matches(&answer.expect("a reply")).expect("the script's answer");
    assert_eq!(fox.count, 2000);

    open(&mut client, &format!("{url}other"), "other");
    let gone = client
        .call_within(
            "Runtime.callFunctionOn",
            find::call_params(context, &ask),
            Duration::from_secs(5),
        )
        .expect_err("the world went with its document");
    assert!(find::stale_world(&gone), "{gone}");
    let context = find_world(&mut client);
    assert_eq!(search(&mut client, context, "fox", 0).count, 1);

    client.close();
    engine.kill();
}

#[test]
fn a_match_in_a_same_origin_frame_is_reached_and_a_cross_origin_one_is_not() {
    let port = serve_pages(|port| {
        let inner: String = (0..30)
            .map(|i| format!("<p>inner line {i}</p>"))
            .collect::<String>()
            + "<p>an inner fox</p>";
        let outer: String = (0..40).map(|i| format!("<p>outer line {i}</p>")).collect();
        vec![
            (
                "/inner".to_string(),
                format!(
                    "<!doctype html><meta charset=utf-8>\
                     <body style='font:16px monospace;margin:0;background:#fff'>{inner}</body>"
                ),
            ),
            (
                "/".to_string(),
                format!(
                    "<!doctype html><meta charset=utf-8><title>loading</title>\
                     <body style='font:16px monospace;margin:0;background:#fff'>\
                     <p>outer fox</p>\
                     <iframe id=same src='http://127.0.0.1:{port}/inner' \
                     style='width:300px;height:120px'></iframe>\
                     <iframe id=cross src='http://localhost:{port}/inner' \
                     style='width:300px;height:120px'></iframe>\
                     <iframe id=doc srcdoc='<p>srcdoc fox</p>'></iframe>\
                     {outer}<p>last outer fox</p>\
                     <script>onload=function(){{document.title='frames'}}</script></body>"
                ),
            ),
        ]
    });
    let url = format!("http://127.0.0.1:{port}/");
    let Some((mut engine, mut client, context)) = finding(&url, "frames") else {
        return;
    };
    assert_eq!(
        evaluate(
            &mut client,
            "document.getElementById('cross').contentDocument === null"
        )
        .as_bool(),
        Some(true),
        "the second frame really is another origin"
    );

    let fox = search(&mut client, context, "fox", 0);
    assert_eq!(
        (fox.count, fox.current),
        (4, 1),
        "outer, the same-origin frame, srcdoc, outer again; not the other origin"
    );
    let into = search(&mut client, context, "fox", 1);
    assert_eq!(into.current, 2);
    assert!(
        page_number(
            &mut client,
            "document.getElementById('same').contentWindow.scrollY"
        ) > 0.0,
        "the frame scrolled to its match"
    );
    assert_eq!(scroll_y(&mut client), 0.0, "and the page stayed");
    let area = |client: &mut Client, what: &str| {
        page_number(
            client,
            &format!("document.getElementById('same').getBoundingClientRect().{what}"),
        ) as u32
    };
    let frame = (
        area(&mut client, "left"),
        area(&mut client, "top"),
        area(&mut client, "width"),
        area(&mut client, "height"),
    );
    let (_, orange) = highlight_pixels(&screenshot(&mut client, "png", None), Some(frame));
    assert!(orange > 0, "the current match is painted inside the frame");

    client.close();
    engine.kill();
}

#[test]
fn a_search_that_asks_nothing_of_the_page_still_answers_on_about_blank_and_the_error_page() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string("about:blank"))]),
        )
        .expect("about:blank");
    let context = find_world(&mut client);
    let blank = search(&mut client, context, "fox", 0);
    assert_eq!((blank.count, blank.current), (0, 0));
    assert_eq!(search(&mut client, context, "fox", 1).current, 0);

    // Port 1 is one the engine refuses to connect to, and says so with its
    // own error page, at once.
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string("http://127.0.0.1:1/"))]),
        )
        .expect("the navigation is answered");
    let deadline = Instant::now() + Duration::from_secs(10);
    let answer = loop {
        // The error page's document arrives after the reply; until it has,
        // the world asked for may be the one that is about to go.
        let context = find_world(&mut client);
        let ask = find::Ask {
            needle: "fox".to_string(),
            step: 0,
        };
        match client.call_within(
            "Runtime.callFunctionOn",
            find::call_params(context, &ask),
            Duration::from_secs(5),
        ) {
            Ok(reply) => break find::matches(&reply),
            Err(why) if find::stale_world(&why) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(why) => panic!("{why}"),
        }
    };
    let error_page = answer.expect("the script's answer");
    assert_eq!(error_page.count, 0);

    client.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// Zoom, the HiDPI scale, and light and dark
// ---------------------------------------------------------------------------

/// A box at CSS (100..150, 50..100) that says when it is pressed, on a page
/// tall enough to scroll. Every press puts where the page saw it, and whether
/// it was on the box, into the title.
const BOXES: &str = "data:text/html,<body style='margin:0;height:3000px;background:%23fff'>\
<div id=b style='position:absolute;left:100px;top:50px;width:50px;height:50px;background:%23c33'></div>\
<script>document.title='ready';\
addEventListener('mousedown',function(e){document.title=(e.target.id==='b'?'box ':'miss ')+e.clientX+' '+e.clientY});\
</script></body>";

/// A page that is white, or black when it is told dark is preferred.
const SCHEME: &str = "data:text/html,<style>body{margin:0;background:white}\
@media (prefers-color-scheme: dark){body{background:black}}</style>\
<body><script>document.title='ready'</script></body>";

/// A page with no dark style at all: white, whatever it is told.
const WHITE: &str = "data:text/html,<body style='margin:0'><p>Some text on a white page.</p>\
<script>document.title='ready'</script></body>";

/// The override the program sends for `factor` on the test's pane.
fn zoom_to(client: &mut Client, factor: f64) -> blinkterm::zoom::Viewport {
    let viewport = blinkterm::zoom::Viewport::fit((WIDTH, HEIGHT), factor);
    client
        .call(
            "Emulation.setDeviceMetricsOverride",
            viewport.metrics_params(),
        )
        .expect("the zoom");
    viewport
}

/// [`prepare`], and then the page zoomed to `factor`.
fn prepare_at(client: &mut Client, factor: f64) -> blinkterm::zoom::Viewport {
    prepare(client);
    zoom_to(client, factor)
}

/// Go to `url` and wait for it to say it is ready — having said something
/// else first, so that the page being left is not taken for it.
fn go_to(client: &mut Client, url: &str) {
    evaluate(client, "document.title='leaving'");
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(url))]),
        )
        .expect("the page loads");
    assert_eq!(
        wait_for_title(client, "ready", Duration::from_secs(10)),
        "ready"
    );
}

/// `innerWidth`, `innerHeight` and `devicePixelRatio`, as the page sees them.
fn page_metrics(client: &mut Client) -> (f64, f64, f64) {
    let answer = evaluate(client, "[innerWidth, innerHeight, devicePixelRatio]");
    let numbers: Vec<f64> = answer
        .as_array()
        .unwrap_or(&[])
        .iter()
        .filter_map(Json::as_f64)
        .collect();
    match numbers.as_slice() {
        [w, h, ratio] => (*w, *h, *ratio),
        _ => panic!("the page said {answer}"),
    }
}

/// A still, decoded.
fn still(client: &mut Client) -> (Vec<u8>, u32, u32) {
    let png = screenshot(client, "png", None);
    let image = tos_term::png::decode(&png, 64 * 1024 * 1024).expect("a still decodes");
    (image.rgba, image.width, image.height)
}

/// A press and a release at a CSS point, as `send_mouse` sends them.
fn click_css(client: &mut Client, (x, y): (f64, f64)) {
    for (kind, buttons) in [("mousePressed", 1), ("mouseReleased", 0)] {
        client
            .call(
                "Input.dispatchMouseEvent",
                Json::object(vec![
                    ("type", Json::string(kind)),
                    ("x", Json::number(x)),
                    ("y", Json::number(y)),
                    ("button", Json::string("left")),
                    ("buttons", Json::number(buttons)),
                    ("clickCount", Json::number(1)),
                    ("modifiers", Json::number(0)),
                ]),
            )
            .expect("the click is dispatched");
    }
}

/// Whether the page is being told dark is preferred.
fn prefers_dark(client: &mut Client) -> bool {
    evaluate(client, "matchMedia('(prefers-color-scheme: dark)').matches").as_bool() == Some(true)
}

/// Wait for `wanted` to be what `ask` says, and say what it said last.
fn wait_until<T: PartialEq + Copy>(
    client: &mut Client,
    wanted: T,
    ask: impl Fn(&mut Client) -> T,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let now = ask(client);
        if now == wanted || Instant::now() >= deadline {
            return now;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The still's pixel at (5, 5), as a luminance.
fn corner_luminance(client: &mut Client) -> f64 {
    let (rgba, width, _) = still(client);
    let at = (5 * width as usize + 5) * 4;
    blinkterm::appearance::luminance((rgba[at], rgba[at + 1], rgba[at + 2]))
}

/// 200%: the page is laid out for half the pane and draws itself at the
/// whole of it. The still is the pane's size and goes into the same cells as
/// ever; a moving frame is the CSS viewport's size, and goes into the same
/// cells too.
#[test]
fn zoom_changes_what_the_page_sees_and_the_still_stays_the_panes_size() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare_at(&mut client, 2.0);
    assert_eq!(page_metrics(&mut client), (320.0, 180.0, 2.0));

    let dir = temp_dir("zoom");
    let mut painter = Painter::at(&dir);
    let mut terminal = a_terminal(&dir);
    let cells = Cells {
        cols: WIDTH / CELL.0,
        rows: HEIGHT / CELL.1,
    };
    let (rgba, width, height) = still(&mut client);
    assert_eq!((width, height), (WIDTH, HEIGHT), "the still is the pane's");
    terminal.advance(&painter.frame(Raw::rgba(&rgba, width, height), cells, 2, 1));
    let store = terminal.graphics();
    let image = store.image(IMAGE_ID).expect("the still is in the store");
    assert_eq!((image.width, image.height), (WIDTH, HEIGHT));
    let placement = store.placements().next().expect("one placement");
    assert_eq!(
        (placement.cols as u32, placement.rows as u32),
        (cells.cols, cells.rows)
    );

    cast(&mut client, "jpeg", Some(motion::QUALITY), WIDTH, HEIGHT);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut frame = None;
    while frame.is_none() && Instant::now() < deadline {
        frame = take_frames(&mut client).pop();
        std::thread::sleep(Duration::from_millis(20));
    }
    let (jpeg, _) = frame.expect("a frame");
    let _ = client.call("Page.stopScreencast", Json::empty());
    let image = tos_term::jpeg::decode(&jpeg, 64 * 1024 * 1024).expect("a frame decodes");
    assert_eq!(
        (image.width, image.height),
        (WIDTH / 2, HEIGHT / 2),
        "a moving frame is the CSS viewport's size"
    );
    terminal.advance(&painter.frame(Raw::rgb(&image.rgb, image.width, image.height), cells, 2, 1));
    let store = terminal.graphics();
    assert_eq!(store.placements().count(), 1);
    let placement = store.placements().next().expect("one placement");
    assert_eq!(
        (placement.cols as u32, placement.rows as u32),
        (cells.cols, cells.rows),
        "and goes into the same cells, for the terminal to scale"
    );

    // A fractional level: the ratio is the level, and the still fits the
    // pane exactly once it has been fitted.
    zoom_to(&mut client, 1.5);
    let (_, _, ratio) = wait_until(&mut client, (427.0, 240.0, 1.5), page_metrics);
    assert_eq!(ratio, 1.5);
    let (rgba, width, height) = still(&mut client);
    eprintln!("a still at 150% on {WIDTH}x{HEIGHT} came {width}x{height}");
    let fitted = blinkterm::zoom::fit(&rgba, width, height, 4, (WIDTH, HEIGHT)).unwrap_or(rgba);
    assert_eq!(fitted.len(), (WIDTH * HEIGHT * 4) as usize);

    painter.clean_up();
    std::fs::remove_dir_all(&dir).ok();
    client.close();
    engine.kill();
}

/// A point on the screen is the same element at every level: the terminal's
/// pixels divided by the factor, and nothing else.
#[test]
fn a_click_lands_on_the_same_element_at_every_zoom() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let viewport = prepare_at(&mut client, 2.0);
    // The cell `a_click_lands_where_the_cell_was` clicks, at 200%.
    let report = blinkterm::input::MouseInput {
        kind: blinkterm::input::MouseKind::Press,
        button: Some(0),
        mods: Mods::default(),
        x: 11,
        y: 4,
        wheel: (0, 0),
    };
    let (x, y) = blinkterm::input::page_point(&report, false, CELL, 1);
    assert_eq!((x, y), (84, 40));
    assert_eq!(viewport.css_point(x, y), (42.0, 20.0));
    click_css(&mut client, viewport.css_point(x, y));
    assert_eq!(
        wait_for_title(&mut client, "click ", Duration::from_secs(5)),
        "click 0 42 20"
    );

    go_to(&mut client, BOXES);
    for (factor, point, hit) in [
        (2.0, (250, 150), true),
        (0.5, (60, 30), true),
        (1.0, (250, 150), false),
    ] {
        let viewport = zoom_to(&mut client, factor);
        wait_until(&mut client, factor, |client| page_metrics(client).2);
        evaluate(&mut client, "document.title='waiting'");
        click_css(&mut client, viewport.css_point(point.0, point.1));
        let seen = wait_for_title(
            &mut client,
            if hit { "box " } else { "miss " },
            Duration::from_secs(5),
        );
        assert!(
            seen.starts_with(if hit { "box " } else { "miss " }),
            "{point:?} at {factor}: {seen}"
        );
    }

    client.close();
    engine.kill();
}

/// A notch is 120 pixels of the screen at every level, which at 200% is 60
/// of the page's.
#[test]
fn a_wheel_notch_moves_the_page_the_same_distance_on_screen_at_any_zoom() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);
    go_to(&mut client, BOXES);
    let wire = blinkterm::app::Wire::new(client.notifier());
    for (factor, css) in [(2.0, 60.0), (1.0, 120.0), (0.5, 240.0)] {
        let viewport = zoom_to(&mut client, factor);
        wait_until(&mut client, factor, |client| page_metrics(client).2);
        evaluate(&mut client, "scrollTo(0,0)");
        let at = viewport.css_point(320, 180);
        wire.send(Step {
            at: (at.0.round() as i32, at.1.round() as i32),
            delta: viewport.notch((0, 1), blinkterm::app::WHEEL_PIXELS),
        })
        .expect("the wheel event goes out");
        let moved = wait_until(&mut client, css, scroll_y);
        assert_eq!(moved, css, "at {factor}");
        assert_eq!(moved * factor, 120.0, "on the screen, at {factor}");
    }

    client.close();
    engine.kill();
}

/// The override is the session's: a navigation keeps it, and a session made
/// afterwards starts without it — which is why the program sends it every
/// time a tab comes to the front.
#[test]
fn the_zoom_survives_a_navigation_and_a_new_tab_starts_at_its_hosts_level() {
    let Some((mut engine, mut page, target)) = connect_with_target() else {
        return;
    };
    prepare_at(&mut page, 2.0);
    for url in [BOXES, PAGE] {
        go_to(&mut page, url);
        assert_eq!(page_metrics(&mut page).0, 320.0, "after going to {url}");
    }

    let (mut browser, tabs) = tabbed(&engine, page, target);
    let created = browser
        .call(
            "Target.createTarget",
            Json::object(vec![("url", Json::string("about:blank"))]),
        )
        .expect("a new target");
    let opened = created
        .get("targetId")
        .and_then(Json::as_str)
        .expect("the engine says which")
        .to_string();
    let mut second = browser
        .attach(&opened, Duration::from_secs(10))
        .expect("a session on it");
    second
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    blinkterm::app::prepare_session(
        &mut second,
        &blinkterm::appearance::Appearance::new(blinkterm::appearance::Choice::Auto, false),
    );
    let (width, _, ratio) = page_metrics(&mut second);
    eprintln!("a new session, told nothing about its size, is {width} wide");
    assert_ne!(width, 320.0, "a new session starts at the engine's size");
    assert_eq!(ratio, 1.0);
    // Which `activate` then tells it.
    zoom_to(&mut second, 2.0);
    assert_eq!(page_metrics(&mut second), (320.0, 180.0, 2.0));

    second.close();
    browser.close();
    drop(tabs);
    engine.kill();
}

/// A dark terminal's answer makes a loaded page dark where it stands, the
/// next page dark, and a tab opened afterwards dark; a light answer after it
/// makes the page light again, and none of it is a reload.
#[test]
fn a_dark_terminal_gets_dark_pages_and_so_does_a_tab_opened_later() {
    let Some((mut engine, mut page, target)) = connect_with_target() else {
        return;
    };
    page.call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut page);
    go_to(&mut page, SCHEME);
    assert!(!prefers_dark(&mut page), "the engine's own answer is light");
    assert!(corner_luminance(&mut page) > 0.9);

    let mut appearance =
        blinkterm::appearance::Appearance::new(blinkterm::appearance::Choice::Auto, false);
    assert!(appearance.learned((0x1c, 0x1c, 0x1c)));
    blinkterm::app::prepare_session(&mut page, &appearance);
    assert!(
        wait_until(&mut page, true, prefers_dark),
        "told on the loaded page"
    );
    evaluate(&mut page, "window.kept=1");
    assert!(corner_luminance(&mut page) < 0.05);

    // Which survives the page being left.
    go_to(&mut page, SCHEME);
    assert!(prefers_dark(&mut page), "after a navigation");
    evaluate(&mut page, "window.kept=1");

    // A tab made afterwards is told when it is made.
    let (mut browser, tabs) = tabbed(&engine, page, target);
    let created = browser
        .call(
            "Target.createTarget",
            Json::object(vec![("url", Json::string("about:blank"))]),
        )
        .expect("a new target");
    let opened = created
        .get("targetId")
        .and_then(Json::as_str)
        .expect("the engine says which")
        .to_string();
    let mut second = browser
        .attach(&opened, Duration::from_secs(10))
        .expect("a session on it");
    second
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    blinkterm::app::prepare_session(&mut second, &appearance);
    viewport(&mut second);
    go_to(&mut second, SCHEME);
    assert!(prefers_dark(&mut second), "a tab opened later");

    // And the terminal turning light turns the first page light, live.
    assert!(appearance.learned((0xfd, 0xf6, 0xe3)));
    let mut tabs = tabs;
    let first = &mut tabs.active_mut().expect("the first tab").connection;
    blinkterm::app::prepare_session(first, &appearance);
    assert!(!wait_until(first, false, prefers_dark), "told light");
    assert_eq!(
        evaluate(first, "window.kept").as_f64(),
        Some(1.0),
        "and not by a reload"
    );

    second.close();
    browser.close();
    drop(tabs);
    engine.kill();
}

/// `--force-dark`: a page with no dark style of its own is painted dark, and
/// stays dark on the next page.
#[test]
fn forced_dark_paints_a_white_page_dark() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    go_to(&mut client, WHITE);
    assert!(corner_luminance(&mut client) > 0.9, "white to begin with");

    let forced = blinkterm::appearance::Appearance::new(blinkterm::appearance::Choice::Auto, true);
    blinkterm::app::prepare_session(&mut client, &forced);
    let dark = |client: &mut Client| corner_luminance(client) < 0.1;
    assert!(wait_until(&mut client, true, dark), "painted dark");
    go_to(&mut client, WHITE);
    assert!(
        wait_until(&mut client, true, dark),
        "and after a navigation"
    );

    client.close();
    engine.kill();
}

/// At every fractional level in the table that the engine rounds, the still
/// is made exactly the pane's size, which is what keeps the terminal from
/// resampling it.
#[test]
fn the_still_at_a_fractional_level_is_cut_to_the_pane() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);
    for factor in [1.1, 1.5, 1.75, 3.0] {
        let viewport = zoom_to(&mut client, factor);
        wait_until(&mut client, factor, |client| page_metrics(client).2);
        let (rgba, width, height) = still(&mut client);
        eprintln!(
            "at {factor}: css {:?}, the still came {width}x{height}",
            viewport.css
        );
        assert!(
            width.abs_diff(WIDTH) <= blinkterm::zoom::SLACK
                && height.abs_diff(HEIGHT) <= blinkterm::zoom::SLACK,
            "{width}x{height} at {factor}"
        );
        let fitted = blinkterm::zoom::fit(&rgba, width, height, 4, (WIDTH, HEIGHT)).unwrap_or(rgba);
        assert_eq!(fitted.len(), (WIDTH * HEIGHT * 4) as usize, "at {factor}");
    }

    client.close();
    engine.kill();
}
