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

use blinkterm::bindings::{Bindings, Keymap, Lookup};
use blinkterm::cdp::{Client, Pending};
use blinkterm::engine::{self, Engine};
use blinkterm::find::{self, Matches};
use blinkterm::fit::Cells;
use blinkterm::graphics::{Painter, Raw, IMAGE_ID};
use blinkterm::identity::Identity;
use blinkterm::input::{Key, KeyAction, KeyInput, Mods};
use blinkterm::json::Json;
use blinkterm::keys;
use blinkterm::motion::{self, Motion};
use blinkterm::profile::{Choice, Profile};
use blinkterm::route::{Payload, Placement, Route, Wrap};
use blinkterm::scroll::{self, Animator, Dispatch, Step, Wheel};
use blinkterm::sites::{self, Sites};
use blinkterm::tabs::{Outcome, Tab, Tabs};

// tOS's `t=s` reader, the one a real tOS pane installs, copied from
// `tos-compositor` at `c7677bde` and kept as it is there so that the two can
// be compared line for line — which is why this crate's lints stop here.
#[path = "support/imagefile.rs"]
#[allow(clippy::undocumented_unsafe_blocks)]
mod imagefile;
use imagefile::ImageFiles;

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

/// Whether timing assertions are running in GitHub's shared macOS VM.
///
/// A local Mac deliberately keeps the strict assertions. The workflow sets
/// this marker because the whole hosted VM can be descheduled for over 100 ms,
/// which no timer or thread priority inside the guest can prevent.
fn shared_macos_runner() -> bool {
    cfg!(target_os = "macos") && std::env::var_os("BLINKTERM_SHARED_RUNNER").is_some()
}

/// Whether to skip a test whose assertions are about time, saying so.
///
/// How evenly a scroll's frames arrive and how soon it settles are tests of
/// the machine as much as of the program, and on GitHub's shared macOS VM the
/// whole guest can be descheduled for over 100 ms. Loosened limits did not
/// hold: two runs of one commit failed two different ones of these tests
/// (#46). So there they are skipped, not loosened. They still run on Linux
/// CI, and on a real Mac, which does not set the marker.
fn skip_timing_on_shared_runner(test: &str) -> bool {
    if shared_macos_runner() {
        eprintln!("skipped on the shared macOS runner: {test} asserts on time");
        return true;
    }
    false
}

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

/// Acknowledge a screencast frame, which is what keeps the next one coming.
/// Anything else is nothing.
fn acknowledge_frame(client: &mut Client, event: &blinkterm::cdp::Event) {
    if event.method != "Page.screencastFrame" {
        return;
    }
    if let Some(session) = event.params.get("sessionId").and_then(Json::as_i64) {
        let _ = client.notify(
            "Page.screencastFrameAck",
            Json::object(vec![("sessionId", Json::number(session as f64))]),
        );
    }
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
            let image = blinkterm::jpeg::decode(&jpeg, 64 * 1024 * 1024).expect("a frame decodes");
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
            let image = blinkterm::jpeg::decode(&jpeg, 64 * 1024 * 1024).expect("a frame decodes");
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

/// A `cmd` chord the Mac keymap leaves alone reaches the page as the key
/// with Meta held, and types nothing: the contract the Mac keymap depends
/// on for every `cmd` chord it does not take — `cmd+a`, `cmd+z`, `cmd+x` —
/// which a page's editor expects to see as `metaKey`.
#[test]
fn a_cmd_chord_the_mac_keymap_leaves_alone_reaches_the_page_with_meta_held() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    form(&mut client);
    evaluate(
        &mut client,
        "window.seen=[];t.addEventListener('keydown',function(e){\
         seen.push([e.key,e.metaKey,e.ctrlKey,e.altKey].join(' '))});t.focus()",
    );
    let press = KeyInput {
        key: Key::Char('a'),
        mods: Mods(Mods::SUPER),
        action: KeyAction::Press,
        text: None,
    };
    let bindings = Bindings::on(Keymap::Mac, vec![]);
    assert_eq!(
        bindings.lookup(&press),
        Lookup::Default,
        "cmd+a is not the Mac keymap's"
    );
    let params = keys::dispatch(&press).expect("a key with a name");
    client
        .call("Input.dispatchKeyEvent", params)
        .expect("the key is dispatched");
    assert_eq!(
        evaluate(&mut client, "seen.join(',')").as_str(),
        Some("a true false false")
    );
    assert_eq!(
        evaluate(&mut client, "t.value").as_str(),
        Some(""),
        "a chord with meta held types nothing"
    );

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

/// A page of the ways a click can ask for another page, at known points: a
/// plain link at (40, 30), something that is not a link at (340, 30), and a
/// link with `target=_blank` at (40, 130). What the page's own listeners saw
/// is kept in `window.log`, and a box moves every animation frame so that the
/// screencast has something to send.
const OPENS_PAGE: &str = "<!doctype html><title>opens</title>\
<body style='margin:0;background:#fff'>\
<a href='/plain' style='position:absolute;left:0;top:0;width:240px;height:60px;\
background:#cc3'>plain</a>\
<div style='position:absolute;left:300px;top:0;width:200px;height:60px;\
background:#ccc'>not a link</div>\
<a href='/plain' target=_blank style='position:absolute;left:0;top:100px;\
width:240px;height:60px;background:#3c3'>blank</a>\
<div id=box style='position:absolute;left:0;top:200px;width:40px;height:40px;\
background:#c33'></div>\
<script>window.log=[];\
for(const t of ['click','auxclick'])addEventListener(t,e=>log.push(t+':'+e.button+':'+e.ctrlKey+':'+e.metaKey));\
let x=0;(function f(){x=(x+4)%600;box.style.left=x+'px';requestAnimationFrame(f)})();\
</script></body>";

/// Where the links on [`OPENS_PAGE`] go.
const PLAIN_PAGE: &str = "<!doctype html><title>plain</title><body>plain</body>";

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
            } else if request.starts_with("GET /opens") {
                OPENS_PAGE
            } else if request.starts_with("GET /plain") {
                PLAIN_PAGE
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
                Outcome::Gone { mut tab } => tab.connection.close(),
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

/// A press and a release at `at`, as `send_mouse` sends them: `button` with
/// its bit in `buttons` on the press and none on the release, and the
/// modifiers as CDP counts them (Alt 1, Ctrl 2, Meta 4, Shift 8).
fn click_with(client: &mut Client, at: (i32, i32), button: &str, bit: u32, modifiers: u32) {
    for (kind, buttons) in [("mousePressed", bit), ("mouseReleased", 0)] {
        client
            .call(
                "Input.dispatchMouseEvent",
                Json::object(vec![
                    ("type", Json::string(kind)),
                    ("x", Json::number(at.0)),
                    ("y", Json::number(at.1)),
                    ("button", Json::string(button)),
                    ("buttons", Json::number(buttons)),
                    ("clickCount", Json::number(1)),
                    ("modifiers", Json::number(modifiers)),
                ]),
            )
            .expect("the click is dispatched");
    }
}

/// The modifier mask this program sends when a terminal reports a ctrl+click.
///
/// Not the constant 2: on a Mac the engine reads the tab-opening modifier as
/// Meta and a literal ctrl+click opens nothing, so the program sends 4 there.
/// Asking [`Mods::cdp_mouse`] rather than writing the number keeps these
/// tests measuring the gesture — a ctrl+click opens a tab behind — instead of
/// the spelling it happens to go out with.
fn ctrl_click() -> u32 {
    Mods::default().with(Mods::CTRL).cdp_mouse()
}

/// What [`OPENS_PAGE`]'s log says for a left click made with [`ctrl_click`].
///
/// That page logs `ctrlKey` and `metaKey` both, because which of the two a
/// ctrl+click reaches the page as is the platform's business: a Mac is told
/// Meta, everywhere else is told Ctrl, and either way the page saw the click
/// with the tab-opening modifier held, which is what these tests are about.
fn ctrl_click_logged() -> String {
    let mac = cfg!(target_os = "macos");
    format!("click:0:{}:{}", !mac, mac)
}

/// Everything a page's session says for `within`, every screencast frame
/// acknowledged as `handle_page_events` acknowledges them: how many frames
/// came, and the other events.
fn watch_page(client: &mut Client, within: Duration) -> (usize, Vec<blinkterm::cdp::Event>) {
    let deadline = Instant::now() + within;
    let mut frames = 0;
    let mut others = Vec::new();
    while Instant::now() < deadline {
        for event in client.events() {
            if event.method != "Page.screencastFrame" {
                others.push(event);
                continue;
            }
            frames += 1;
            if let Some(session) = event.params.get("sessionId").and_then(Json::as_i64) {
                let _ = client.notify(
                    "Page.screencastFrameAck",
                    Json::object(vec![("sessionId", Json::number(session as f64))]),
                );
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    (frames, others)
}

/// What the page's own listeners saw, from [`OPENS_PAGE`]'s `window.log`.
fn page_log(client: &mut Client) -> String {
    match evaluate(client, "JSON.stringify(window.log)") {
        Json::String(log) => log,
        other => panic!("the page's log: {other:?}"),
    }
}

/// A middle click and a ctrl+click on a link: the engine opens the page, the
/// target it announces has no opener, and it becomes a tab *behind* the one
/// in front — which goes on casting and is never switched from. Measured in
/// `src/tabs.rs`; this is that measurement kept true. Before this, such a
/// target was refused for having no opener, and the page loaded for nobody.
#[test]
fn a_middle_click_and_a_ctrl_click_on_a_link_open_a_tab_behind_the_one_in_front() {
    let Some((mut engine, page, target)) = connect_with_target() else {
        return;
    };
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
                Json::object(vec![("url", Json::string(format!("{base}opens")))]),
            )
            .expect("the page loads");
        assert_eq!(
            wait_for_title(&mut first.connection, "opens", Duration::from_secs(10)),
            "opens"
        );
        cast(&mut first.connection, "jpeg", None, WIDTH, HEIGHT);
        let (settling, _) = watch_page(&mut first.connection, Duration::from_millis(500));
        assert!(settling > 0, "the page in front never cast a frame");

        // The middle button on the plain link.
        click_with(&mut first.connection, (40, 30), "middle", 4, 0);
        let (frames, events) = watch_page(&mut first.connection, Duration::from_secs(1));
        let dispositions: Vec<String> = events
            .iter()
            .filter(|event| event.method == "Page.frameRequestedNavigation")
            .filter_map(|event| event.params.get("disposition").and_then(Json::as_str))
            .map(str::to_string)
            .collect();
        assert!(
            dispositions.iter().any(|word| word == "newTab"),
            "the engine did not say newTab for a middle click: {dispositions:?}"
        );
        assert!(
            frames >= 10,
            "the page in front stopped casting after a tab opened behind it: {frames} frames"
        );
    }

    assert!(
        pump(
            &mut browser,
            &mut tabs,
            Duration::from_secs(15),
            |tabs| tabs.len() == 2
        ),
        "a middle click on a link opened no tab"
    );
    assert_eq!(tabs.active_index(), 0, "the tab opened behind");
    assert!(
        pump(&mut browser, &mut tabs, Duration::from_secs(10), |tabs| {
            tabs.iter()
                .nth(1)
                .is_some_and(|tab| tab.url.ends_with("/plain"))
        }),
        "the tab behind never said where it was: {:?}",
        tabs.iter().map(|tab| tab.url.clone()).collect::<Vec<_>>()
    );
    let log = page_log(&mut tabs.active_mut().expect("a tab").connection);
    assert!(log.contains("auxclick:1"), "{log}");

    // It loads, and has its title, without ever being brought to the front.
    {
        let behind = tabs.get_mut(1).expect("the tab behind");
        behind
            .connection
            .call("Page.enable", Json::empty())
            .expect("Page.enable");
        assert_eq!(
            wait_for_title(&mut behind.connection, "plain", Duration::from_secs(10)),
            "plain"
        );
    }

    // The ctrl key on the left button: the same.
    let first = &mut tabs.active_mut().expect("the first tab").connection;
    click_with(first, (40, 30), "left", 1, ctrl_click());
    let _ = watch_page(first, Duration::from_millis(200));
    assert!(
        pump(
            &mut browser,
            &mut tabs,
            Duration::from_secs(15),
            |tabs| tabs.len() == 3
        ),
        "a ctrl+click on a link opened no tab"
    );
    assert_eq!(tabs.active_index(), 0, "still behind");
    let first = &mut tabs.active_mut().expect("the first tab").connection;
    let log = page_log(first);
    assert!(log.contains(&ctrl_click_logged()), "{log}");

    // A ctrl+click on something that is not a link is the page's, and opens
    // nothing.
    click_with(first, (340, 30), "left", 1, ctrl_click());
    let _ = watch_page(first, Duration::from_millis(200));
    assert!(
        !pump(&mut browser, &mut tabs, Duration::from_secs(2), |tabs| tabs
            .len()
            > 3),
        "a ctrl+click on nothing opened a tab"
    );
    let first = &mut tabs.active_mut().expect("the first tab").connection;
    let wanted = ctrl_click_logged();
    let clicks = |log: &str| log.matches(&wanted).count();
    let after = page_log(first);
    assert_eq!(clicks(&after), clicks(&log) + 1, "{after}");

    // And a link that asks for a window still comes to the front.
    click_with(first, (40, 130), "left", 1, 0);
    let _ = watch_page(first, Duration::from_millis(200));
    assert!(
        pump(
            &mut browser,
            &mut tabs,
            Duration::from_secs(15),
            |tabs| tabs.len() == 4
        ),
        "the target=_blank link opened no tab"
    );
    assert_eq!(
        tabs.active_index(),
        3,
        "a page that asked for a window is in front"
    );

    browser.close();
    drop(tabs);
    engine.kill();
}

/// [`blinkterm::app::open_behind`]: the program's own way to a tab behind,
/// for a url rather than a click. The engine accepts `background: true`,
/// which is documented as Chrome's only; the engine's announcement of the
/// target is not a second tab; the page loads with nobody looking; and
/// moving a tab in the strip is this program's order, not the engine's.
#[test]
fn a_tab_opened_behind_by_this_program_loads_without_being_looked_at() {
    let Some((mut engine, page, target)) = connect_with_target() else {
        return;
    };
    let base = serve();
    let (mut browser, mut tabs) = tabbed(&engine, page, target);
    let appearance = blinkterm::appearance::Appearance::new(
        blinkterm::appearance::Choice::Auto,
        false,
        blinkterm::appearance::Alpha::Off,
    );

    let index = blinkterm::app::open_behind(
        &mut tabs,
        &mut browser,
        &appearance,
        &Identity::new(None, None, "C"),
        &Sites::none(),
        &format!("{base}plain"),
    )
    .expect("the engine opens a page behind");
    assert_eq!(index, 1);
    assert_eq!(tabs.active_index(), 0, "the tab in front stays in front");
    assert!(
        !pump(&mut browser, &mut tabs, Duration::from_secs(3), |tabs| tabs
            .len()
            > 2),
        "the tab this program opened behind was counted twice"
    );
    assert_eq!(tabs.active_index(), 0, "and was not pulled forward");
    let behind = tabs.get_mut(1).expect("the tab behind");
    assert_eq!(
        wait_for_title(&mut behind.connection, "plain", Duration::from_secs(10)),
        "plain"
    );

    // The engine's order of its targets, before and after the strip's
    // changes: a future engine that reordered on activation would show up
    // here as a mismatch nobody expected.
    let order = |browser: &mut Client| -> Vec<String> {
        let reply = browser
            .call("Target.getTargets", Json::empty())
            .expect("the targets");
        reply
            .get("targetInfos")
            .and_then(Json::as_array)
            .map(|infos| {
                infos
                    .iter()
                    .filter(|info| info.get("type").and_then(Json::as_str) == Some("page"))
                    .filter_map(|info| info.get("targetId").and_then(Json::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let before = order(&mut browser);
    let targets: Vec<String> = tabs.iter().map(|tab| tab.target.clone()).collect();
    assert!(tabs.move_active(1));
    assert_eq!(tabs.active_index(), 1);
    assert_eq!(
        tabs.iter()
            .map(|tab| tab.target.clone())
            .collect::<Vec<_>>(),
        [targets[1].clone(), targets[0].clone()]
    );
    assert_eq!(order(&mut browser), before, "the engine's order is its own");

    browser.close();
    drop(tabs);
    engine.kill();
}

/// Issue #70: a page opened straight into a new tab may never tell this
/// program it has loaded. `Target.createTarget` with a url starts the
/// navigation before the attach that follows it has enabled `Page`, and a
/// page quick enough is finished before anything is listening — measured
/// against `chrome-headless-shell` 153 with a `data:` url, where the whole
/// load but `Page.frameStoppedLoading` was gone. What is left after that, and
/// after the bin [`activate`] makes of the queue when the tab comes to the
/// front, is the page itself: it still says it has finished
/// (`document.readyState`), which is what [`blinkterm::app::page_loaded`] now
/// carries back so the visit can be recorded.
#[test]
fn a_tab_whose_queued_events_went_in_the_bin_still_says_its_page_finished() {
    let Some((mut engine, page, target)) = connect_with_target() else {
        return;
    };
    let (mut browser, mut tabs) = tabbed(&engine, page, target);
    let appearance = blinkterm::appearance::Appearance::new(
        blinkterm::appearance::Choice::Auto,
        false,
        blinkterm::appearance::Alpha::Off,
    );
    // A `data:` url rather than [`serve`]: what is tested is a target made
    // with a url it loads on its own, and one that needs no socket cannot be
    // a test of the socket. It is also the quickest page there is, which is
    // what loses the load event.
    let index = blinkterm::app::open_behind(
        &mut tabs,
        &mut browser,
        &appearance,
        &Identity::new(None, None, "C"),
        &Sites::none(),
        "data:text/html,<title>plain</title><body>plain",
    )
    .expect("the engine opens a page");

    let tab = tabs.get_mut(index).expect("the tab");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut finished = None;
    while Instant::now() < deadline && finished.is_none() {
        // The bin, which is what `activate` does to a tab coming to the
        // front: every event read and dropped, the load event among them if
        // it ever came at all.
        let _ = tab.connection.events();
        let loaded = blinkterm::app::page_loaded(&mut tab.connection).expect("the page answers");
        if loaded.complete {
            finished = Some(loaded);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let finished = finished.expect("the page never said it had finished");
    assert_eq!(finished.title, "plain");
    assert_eq!(finished.status, None);

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

/// A press on the strip, as a terminal sends it: read against the spans of
/// the row this program would draw, which are checked against what the
/// compositor's own terminal shows in those columns; a left press on a tab
/// switches to it, a middle press on a tab that is *not* in front closes
/// that one, and the url opens the bar.
///
/// `handle_input` is the loop's and needs a pane on a tty, as every test
/// here works around; `click_row` and the release rule are held by the unit
/// tests in `src/app.rs`, and this holds the rest against a real engine.
#[test]
fn a_press_on_the_strip_switches_to_that_tab_a_middle_press_closes_it_and_the_url_opens_the_bar() {
    use blinkterm::input::{page_point, row_cell, Input, MouseInput, Parser};
    use blinkterm::screen::{self, TabLabel};
    use blinkterm::strip::{self, Part, Step as Pressed};
    let Some((mut engine, mut browser, mut tabs)) = two_tabs() else {
        return;
    };
    assert_eq!(
        tabs.active_index(),
        1,
        "the tab the link opened is in front"
    );
    // The url the row would end with. The tab's own is filled in by the
    // loop's navigation events, which these tests do not run; what matters
    // here is where the row puts it, not which it is.
    let url = "http://127.0.0.1/second.html";

    // The labels exactly as `redraw_row` makes them.
    let names: Vec<_> = tabs.iter().map(|tab| tab.label().into_owned()).collect();
    let labels: Vec<TabLabel> = names
        .iter()
        .zip(tabs.iter())
        .enumerate()
        .map(|(index, (name, tab))| TabLabel {
            title: name,
            active: index == tabs.active_index(),
            dialog: tab.asks(),
        })
        .collect();
    let drawn = screen::tab_line_from(80, &labels, url, 0);
    let text = a_terminal_reads_only_text_in(&drawn.bytes);
    let at = |span: &strip::Span| -> String {
        text.chars()
            .skip(span.from)
            .take(span.to - span.from)
            .collect()
    };
    let span_of = |part: Part| -> strip::Span {
        *drawn
            .spans
            .iter()
            .find(|span| span.part == part)
            .unwrap_or_else(|| panic!("no {part:?} in {:?} for {text:?}", drawn.spans))
    };
    let (first, second, bar) = (
        span_of(Part::Tab(0)),
        span_of(Part::Tab(1)),
        span_of(Part::Url),
    );
    assert!(at(&first).starts_with("1 "), "{text:?} {first:?}");
    assert!(at(&second).starts_with("2 "), "{text:?} {second:?}");
    assert!(
        url.starts_with(&at(&bar)[..at(&bar).len().min(8)]),
        "{text:?} {bar:?}"
    );

    let report = |bytes: String| -> MouseInput {
        match Parser::new().feed(bytes.as_bytes()).as_slice() {
            [Input::Mouse(report)] => *report,
            other => panic!("{bytes:?} is not one mouse report: {other:?}"),
        }
    };
    // In cells and in Kitty's pixels, the middle of the first tab's label.
    let column = (first.from + first.to) / 2;
    for (bytes, pixels) in [
        (format!("\x1b[<0;{};1M", column + 1), false),
        (format!("\x1b[<0;{};9M", column * 8 + 4), true),
    ] {
        let press = report(bytes);
        assert!(page_point(&press, pixels, CELL, 1).1 < 0, "on the row");
        assert_eq!(row_cell(&press, pixels, CELL), (column, 0));
        let part = strip::hit(&drawn.spans, column);
        assert_eq!(part, Some(Part::Tab(0)));
        assert_eq!(
            strip::step(part, press.button, tabs.len()),
            Pressed::Switch(0)
        );
    }
    assert!(tabs.switch_to(0));
    let front = tabs.active_mut().expect("the first tab");
    assert_eq!(
        blinkterm::app::page_title(&mut front.connection).as_deref(),
        Some("first")
    );

    // A middle press over the second label, with the first in front: the
    // tab under the pointer goes, not the one in front.
    let column = (second.from + second.to) / 2;
    let press = report(format!("\x1b[<1;{};1M", column + 1));
    let part = strip::hit(&drawn.spans, row_cell(&press, false, CELL).0);
    let Pressed::Close(index) = strip::step(part, press.button, tabs.len()) else {
        panic!("a middle press on {part:?} closed nothing");
    };
    assert_eq!(index, 1);
    // What `close_tab` does: the target in the engine, then the session.
    let mut tab = tabs.close(index).expect("the tab");
    browser
        .call(
            "Target.closeTarget",
            Json::object(vec![("targetId", Json::string(&tab.target))]),
        )
        .expect("the target closes");
    tab.connection.close();
    assert!(
        pump(
            &mut browser,
            &mut tabs,
            Duration::from_secs(10),
            |tabs| tabs.len() == 1,
        ),
        "the engine's news about the closed target upset the list"
    );
    let left = tabs.active_mut().expect("the tab that is left");
    assert_eq!(
        blinkterm::app::page_title(&mut left.connection).as_deref(),
        Some("first"),
        "the tab in front went instead of the one under the pointer"
    );

    // The url opens the bar; a gap is nothing; and with one tab left a
    // middle press closes nothing.
    let column = (bar.from + bar.to) / 2;
    let press = report(format!("\x1b[<0;{};1M", column + 1));
    let part = strip::hit(&drawn.spans, column);
    assert_eq!(strip::step(part, press.button, 2), Pressed::EditUrl);
    let gap = first.to;
    assert_eq!(strip::hit(&drawn.spans, gap), None, "{text:?}");
    assert_eq!(strip::step(None, Some(0), 2), Pressed::Nothing);
    assert_eq!(
        strip::step(Some(Part::Tab(0)), Some(1), tabs.len()),
        Pressed::Nothing
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
/// A Mac has no `/proc`, and asks `lsof(8)` the same question.
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

    let (census, found) = listening_sockets_of(group, &members);
    eprintln!(
        "group {group}: {} processes, {census}, {} of them the engine's",
        members.len(),
        found.len()
    );
    assert!(found.is_empty(), "the engine is listening: {found:?}");

    drop(client);
    drop(engine);
}

/// Which of `members` listen on a TCP socket, one sentence each, and a
/// count of what was looked at for the log.
#[cfg(target_os = "linux")]
fn listening_sockets_of(_group: i32, members: &[(i32, String)]) -> (String, Vec<String>) {
    let listening: std::collections::HashSet<u64> = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .flat_map(|table| listening_inodes(table))
        .collect();
    let mut sockets = 0;
    let mut found = Vec::new();
    for (pid, command) in members {
        for inode in socket_inodes(*pid) {
            sockets += 1;
            if listening.contains(&inode) {
                found.push(format!("{pid} ({command}) listens on socket:[{inode}]"));
            }
        }
    }
    let census = format!(
        "{sockets} sockets between them, {} listening sockets on the machine",
        listening.len()
    );
    (census, found)
}

/// The same from `lsof(8)`, which is how a Mac lists sockets: every TCP
/// socket in `LISTEN` held by a process in `group` (`-a` ands the three
/// selections), as `p<pid>` and `n<address>` lines. Nothing matching is an
/// exit status of 1 with nothing on either stream; anything else on stderr
/// is `lsof` failing, which must not read as a pass.
#[cfg(not(target_os = "linux"))]
fn listening_sockets_of(group: i32, members: &[(i32, String)]) -> (String, Vec<String>) {
    let out = std::process::Command::new("lsof")
        .args(["-nP", "-a", "-iTCP", "-sTCP:LISTEN"])
        .arg(format!("-g{group}"))
        .args(["-F", "pn"])
        .output()
        .expect("lsof runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() || (stdout.trim().is_empty() && stderr.trim().is_empty()),
        "lsof failed ({}): {stderr}",
        out.status
    );
    let mut pid = None;
    let mut found = Vec::new();
    for line in stdout.lines() {
        if let Some(number) = line.strip_prefix('p') {
            pid = number.parse::<i32>().ok();
        } else if let Some(address) = line.strip_prefix('n') {
            let command = members
                .iter()
                .find(|(member, _)| Some(*member) == pid)
                .map_or("?", |(_, command)| command.as_str());
            found.push(format!("{pid:?} ({command}) listens on {address}"));
        }
    }
    (format!("lsof -g{group} asked"), found)
}

/// The inodes of the sockets in `LISTEN` in one of `/proc/net/tcp{,6}`.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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

/// The same from `ps(1)`, which is how a Mac reads its process table:
/// `-axo pid=,pgid=,stat=,command=` is one line a process, no header, and a
/// `Z` at the front of `stat` is a zombie here as it is in `/proc`.
#[cfg(not(target_os = "linux"))]
fn group_members(group: i32) -> Vec<(i32, String)> {
    let out = std::process::Command::new("ps")
        .args(["-axo", "pid=,pgid=,stat=,command="])
        .output()
        .expect("ps runs");
    let mut found: Vec<(i32, String)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid: i32 = fields.next()?.parse().ok()?;
            let pgid: i32 = fields.next()?.parse().ok()?;
            let state = fields.next()?;
            (pgid == group && !state.starts_with('Z')).then(|| {
                let line = fields.collect::<Vec<_>>().join(" ");
                (pid, shorten(line))
            })
        })
        .collect();
    found.sort();
    found
}

/// The group this test process is in, read the same way as anybody else's.
#[cfg(target_os = "linux")]
fn own_group() -> i32 {
    state_and_group(std::path::Path::new("/proc/self"))
        .expect("this process has a /proc entry")
        .1
}

/// The group this test process is in, from the kernel, there being no
/// `/proc` to read it the way anybody else's is read.
#[cfg(not(target_os = "linux"))]
fn own_group() -> i32 {
    // SAFETY: `getpgrp(2)` takes nothing, reads no memory and cannot fail.
    unsafe { libc::getpgrp() }
}

/// The run state and process group out of `/proc/<pid>/stat`.
///
/// The second field is the command in brackets and may contain spaces and
/// brackets of its own, so the fields are counted from the last `)` rather
/// than from the start of the line.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn command_of(dir: &std::path::Path) -> String {
    let Ok(raw) = std::fs::read(dir.join("cmdline")) else {
        return String::from("(gone)");
    };
    let line = String::from_utf8_lossy(&raw).replace('\0', " ");
    shorten(line.trim().to_string())
}

/// A command line cut to ninety characters, for a failure message.
fn shorten(line: String) -> String {
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
/// either the encoder's defaults moved or `blinkterm::jpeg` has a bug the
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

    let lossless = blinkterm::png::decode(&png, 64 * 1024 * 1024).expect("the PNG decodes");
    let at = Instant::now();
    let lossy = blinkterm::jpeg::decode(&jpeg, 64 * 1024 * 1024).expect("the JPEG decodes");
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
    let (mut before_terminal, mut png_decoding) = (Duration::ZERO, Duration::ZERO);
    while started.elapsed() < run_for {
        for (png, _) in take_frames(&mut client) {
            // What a PNG moving frame would cost this process to decode,
            // printed and not asserted: the number `--alpha` was weighed on.
            let at = Instant::now();
            let _ = blinkterm::png::decode(&png, 64 * 1024 * 1024).expect("a frame decodes");
            png_decoding += at.elapsed();
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
            let image = blinkterm::jpeg::decode(&jpeg, 64 * 1024 * 1024).expect("a frame decodes");
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
         {} kB a frame, {before_each:?} in the terminal, \
         {:?} if decoded here\n  \
         after   jpeg q{} decoded here:        {after_fps:.1} fps, \
         {} kB a frame, {:?} decoding, {after_each:?} in the terminal\n  \
         the terminal's share is {:.1}x smaller",
        before_bytes / before_frames / 1024,
        png_decoding / before_frames as u32,
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
    //
    // 20 on macOS: the hosted arm64 runner measured 24.1 once (#46), and a
    // browser at 20 frames a second is still one.
    let floor = if cfg!(target_os = "macos") {
        20.0
    } else {
        25.0
    };
    assert!(after_fps > floor, "only {after_fps:.1} fps end to end");

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
            rest.still_requested(motion::now_seconds());
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

/// Stills of a page nobody is touching, at a device scale of `scale`: for
/// each, when it was asked for, when its reply was taken, and the stamps of
/// every screencast frame it provoked, in wall-clock seconds.
fn shutter_rounds(client: &mut Client, scale: f64) -> Vec<(f64, f64, Vec<f64>)> {
    prepare(client);
    article(client, WIDE, TALL);
    client
        .call(
            "Emulation.setDeviceMetricsOverride",
            Json::object(vec![
                ("width", Json::number(WIDE as f64 / scale)),
                ("height", Json::number(TALL as f64 / scale)),
                ("deviceScaleFactor", Json::number(scale)),
                ("mobile", Json::Bool(false)),
            ]),
        )
        .expect("the scale");
    let _ = screenshot(client, "png", None);
    // The cast at the page's CSS size, as the program asks for it.
    let css = (
        (f64::from(WIDE) / scale) as u32,
        (f64::from(TALL) / scale) as u32,
    );
    cast(client, "jpeg", Some(motion::QUALITY), css.0, css.1);
    // Let the load's own frames go by, and check that a page nobody is
    // touching then produces none of its own.
    let settle = Instant::now() + Duration::from_secs(2);
    while Instant::now() < settle {
        take_frames(client);
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut idle = 0usize;
    let quiet = Instant::now() + Duration::from_secs(2);
    while Instant::now() < quiet {
        idle += take_frames(client).len();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(idle, 0, "the page moved on its own, so this proves nothing");

    let mut rounds = Vec::new();
    for round in 0..5 {
        let requested = motion::now_seconds();
        let pending = ask_for_a_still(client);
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
            stamps.extend(take_frames(client).into_iter().filter_map(|(_, at)| at));
            std::thread::sleep(Duration::from_millis(5));
        }
        eprintln!(
            "scale {scale}, still {round}: {:.0} ms to the reply, {} frame(s) at [{}] ms from the request",
            (replied - requested) * 1000.0,
            stamps.len(),
            stamps
                .iter()
                .map(|at| format!("{:+.0}", (at - requested) * 1000.0))
                .collect::<Vec<_>>()
                .join(", "),
        );
        rounds.push((requested, replied, stamps));
    }
    let _ = client.call("Page.stopScreencast", Json::empty());
    rounds
}

/// A still photographs itself into the screencast, exactly once, at a
/// device scale of 1.
///
/// The number of frames a still provokes is the number the whole rest policy
/// is built on, and it is a property of the engine rather than of this crate:
/// a `Page.captureScreenshot` forces a capture of the page's surface, and the
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
    // How many frames a still provokes is the engine's behaviour; where
    // they are stamped is a measurement of time, which the shared macOS VM
    // cannot be trusted with (#46). There the count is still asserted.
    let timed = !skip_timing_on_shared_runner("the shutter frame's stamp");
    for (requested, replied, stamps) in shutter_rounds(&mut client, 1.0) {
        assert_eq!(stamps.len(), 1, "a still provoked {} frames", stamps.len());
        // And it is stamped inside the still's own window, which is what
        // makes crediting the still with its reply enough to keep it off the
        // screen.
        assert!(
            !timed || (stamps[0] >= requested && stamps[0] <= replied),
            "the shutter frame is stamped outside the still it belongs to"
        );
    }
    client.close();
    engine.kill();
}

/// At a device scale of 2 a still provokes up to two frames, and they can be
/// stamped after its reply — the loop found on a Retina Mac. What the policy
/// forgives is asserted here: no more than `motion::SHUTTER_FRAMES`, and none
/// later than the still's window after its reply.
#[test]
fn at_scale_two_a_still_photographs_itself_at_most_twice_and_within_its_window() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    // The count is the engine's; the window is time, skipped on the shared
    // macOS VM (#46), where a frame was once stamped 184 ms late.
    let timed = !skip_timing_on_shared_runner("the shutter frames' window");
    for (requested, replied, stamps) in shutter_rounds(&mut client, 2.0) {
        assert!(
            stamps.len() <= motion::SHUTTER_FRAMES as usize,
            "a still provoked {} frames",
            stamps.len()
        );
        let took = (replied - requested).max(motion::SHUTTER_GRACE.as_secs_f64());
        for stamp in stamps {
            assert!(
                !timed || (stamp >= requested && stamp <= replied + took),
                "a shutter frame at {:+.0} ms, past the window",
                (stamp - requested) * 1000.0
            );
        }
    }
    client.close();
    engine.kill();
}

/// What makes [`ARTICLE`] change once, late: a timer's black box at (200,
/// 100) to (280, 150) CSS, over the text.
const LATE_BOX: &str = "setTimeout(function(){var d=document.createElement('div');\
d.style.cssText='position:absolute;z-index:9;left:200px;top:100px;width:80px;height:50px;background:#000';\
document.body.appendChild(d)},700)";

/// The program's loop, cut down to what paints: frames told to the policy
/// and painted when it says so, stills asked for when it says so and
/// painted when it says so. What it returns is what the pane ends on — the
/// last still, if the last thing painted was a still — and how many stills
/// were asked for in the last `tail` of the run.
fn run_the_rest_policy(
    client: &mut Client,
    run: Duration,
    tail: Duration,
) -> (Option<Vec<u8>>, usize) {
    let started = Instant::now();
    let mut rest = Motion::new(started);
    let mut in_flight: Option<Pending> = None;
    let mut on_screen: Option<Vec<u8>> = None;
    let mut asked = Vec::new();
    while started.elapsed() < run {
        let now = Instant::now();
        for (_, stamp) in take_frames(client) {
            if rest.motion_frame(stamp, Instant::now()) {
                on_screen = None;
            }
        }
        if let Some(pending) = &in_flight {
            if let Some(reply) = client.take_reply(pending) {
                in_flight = None;
                if rest.still_arrived(motion::now_seconds()) {
                    match still_picture(reply) {
                        Some(png) => on_screen = Some(png),
                        None => rest.still_failed(),
                    }
                }
            }
        }
        if in_flight.is_none() && rest.wants_still(now) {
            in_flight = Some(ask_for_a_still(client));
            rest.still_requested(motion::now_seconds());
            asked.push(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let late = asked.iter().filter(|at| **at + tail >= run).count();
    eprintln!("stills asked for at {asked:?}");
    (on_screen, late)
}

/// After a load that paints late, at scale 1 and at 2, under forced
/// transparency: the pane ends on a still, the still shows the late change,
/// its forced-transparent parts are clear, and once the page is quiet no
/// more stills are asked for.
#[test]
fn a_page_that_paints_late_ends_on_a_still_at_either_scale_and_stops_asking() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    // The second at a Retina pane's size, 3200x1760, where the second
    // shutter was found: it comes more often the more a still costs.
    for (scale, pane) in [(1.0, (WIDE, TALL)), (2.0, (3200, 1760))] {
        let css = (
            (f64::from(pane.0) / scale) as u32,
            (f64::from(pane.1) / scale) as u32,
        );
        client
            .call(
                "Emulation.setDeviceMetricsOverride",
                Json::object(vec![
                    ("width", Json::number(css.0)),
                    ("height", Json::number(css.1)),
                    ("deviceScaleFactor", Json::number(scale)),
                    ("mobile", Json::Bool(false)),
                ]),
            )
            .expect("the scale");
        transparent(&mut client, false);
        cast(&mut client, "jpeg", Some(motion::QUALITY), css.0, css.1);
        client
            .call(
                "Page.navigate",
                Json::object(vec![("url", Json::string(ARTICLE))]),
            )
            .expect("the article loads");
        assert_eq!(
            wait_for_title(&mut client, "article", Duration::from_secs(15)),
            "article"
        );
        evaluate(&mut client, LATE_BOX);
        let (still, late) =
            run_the_rest_policy(&mut client, Duration::from_secs(5), Duration::from_secs(2));
        let _ = client.call("Page.stopScreencast", Json::empty());
        let png = still.expect("the pane ends on a still, not a moving frame");
        let image = blinkterm::png::decode(&png, 64 << 20).expect("a still decodes");
        let pixel = |x: f64, y: f64| {
            let at = (((y * scale) as u32 * image.width + (x * scale) as u32) * 4) as usize;
            [
                image.rgba[at],
                image.rgba[at + 1],
                image.rgba[at + 2],
                image.rgba[at + 3],
            ]
        };
        let (box_, bare) = (pixel(240.0, 125.0), pixel(5.0, 165.0));
        eprintln!("scale {scale}: the late box {box_:?}, the bare page {bare:?}, {late} still(s) at the end");
        assert_eq!(
            box_,
            [0, 0, 0, 255],
            "scale {scale}: the still shows the change"
        );
        assert_eq!(bare[3], 0, "scale {scale}: and the forced-transparent page");
        assert_eq!(late, 0, "scale {scale}: a quiet page asks for nothing");
    }
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
    if skip_timing_on_shared_runner("one_notch_is_an_animation_and_not_a_jump") {
        return;
    }
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
    if skip_timing_on_shared_runner("a_steady_hand_moves_the_page_steadily") {
        return;
    }
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
    if skip_timing_on_shared_runner("a_hand_on_the_wheel_gets_no_still_until_it_stops") {
        return;
    }
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
        last + 1 - first >= 21,
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
    // What this needs is a page that was casting, not a frame rate: on the
    // shared macOS VM three seconds of scrolling came to 18 frames (#46),
    // which is casting all the same.
    let casting = if shared_macos_runner() { 10 } else { 20 };
    assert!(
        frames > casting,
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
                    // Something in each of the ways a console hears.
                    "/console" => (
                        "200 OK",
                        String::new(),
                        format!(
                            "<!doctype html><title>start</title><script>\
                         console.log('hello',1,{{a:2}});\
                         setTimeout(function(){{throw new Error('boom')}},0);\
                         onload=function(){{setTimeout(function(){{document.title='done'}},100)}};\
                         </script><img src='/404'><img src='{dead}x.png'>"
                        ),
                    ),
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
                // A crashed page's new renderer, which is what makes the
                // landing after it the page back (`Tab::reviving`).
                "Inspector.targetReloadedAfterCrash" if tab.is_crashed() => {
                    tab.reviving = true;
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
            status: Some(404),
            complete: true
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
            status: None,
            complete: true
        })
    );
    assert_eq!(tab.problem, None);

    navigate_tab(&mut tab, &format!("http://127.0.0.1:{closed}/"));
    follow(&mut tab, Duration::from_secs(5));
    assert_eq!(
        blinkterm::app::page_loaded(&mut tab.connection),
        Some(Loaded {
            title: String::new(),
            status: None,
            complete: true
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
                return Chooser::opening(&event)
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
    blinkterm::app::cancel_chooser(&mut client, None, upload.chooser.backend_node_id);
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

/// The child of the page below: an input filling the whole frame, on a site
/// of its own.
const FRAME_PAGE: &str = "<!doctype html><title>frame</title><body style='margin:0'>\
<input id=f type=file style='position:absolute;left:0;top:0;width:400px;height:300px'>\
<script>f.addEventListener('change',function(){var a=[];\
for(var x of f.files)a.push(x.name+' '+x.size);document.title='files '+a.join(', ')});\
f.addEventListener('cancel',function(){document.title='cancelled'});</script></body>";

/// Serve [`FRAME_PAGE`] to anything under `/frame` and a page holding it in
/// an iframe to anything else, and say which port.
///
/// Its own server rather than [`serve`] because this one is answered on two
/// host names, which is what makes the frame cross-site, and because it
/// answers each connection on a thread of its own: Chromium opens a socket
/// for a navigation and another it may send nothing on, and a server that
/// reads them one after another can be left waiting on the quiet one.
fn serve_two_sites() -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to serve on");
    let port = listener.local_addr().expect("an address").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut stream = stream;
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut head = [0u8; 2048];
                let read = stream.read(&mut head).unwrap_or(0);
                let request = String::from_utf8_lossy(&head[..read]).to_string();
                let body = if request.starts_with("GET /frame") {
                    FRAME_PAGE.to_string()
                } else {
                    format!(
                        "<!doctype html><title>holder</title><body style='margin:0'>\
<iframe src='http://frame.test:{port}/frame' width=400 height=300 \
style='border:0;position:absolute;left:0;top:0'></iframe></body>"
                    )
                };
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes());
            });
        }
    });
    port
}

/// Issue #57: the input is inside a **cross-site iframe**, which is a target
/// of its own in a renderer of its own. Its `Page.fileChooserOpened` does not
/// come through the page's session — measured, it never arrived at all — so
/// the click opened the engine's own picker, which headless does not have and
/// cancels at once: the person saw nothing happen and the page heard
/// `cancel`.
///
/// What this asserts is the whole road back: the engine attaches to the frame
/// because `connect_tab` asked it to, the tab adopts that session, the
/// chooser arrives on it, and the file typed on the row reaches the input in
/// the frame — read out of the frame's own document, on the frame's own
/// session.
#[test]
fn a_file_typed_on_the_row_reaches_an_input_in_a_cross_site_iframe() {
    if std::env::var_os(engine::ENGINE_ENV).is_none() {
        eprintln!(
            "skipped: {} is not set; name a Chromium to run this against",
            engine::ENGINE_ENV
        );
        return;
    }
    let port = serve_two_sites();
    // Two names for the one loopback server: `holder.test` and `frame.test`
    // are different sites, which is what makes the frame out-of-process.
    // `--site-per-process` so that it is one wherever the engine would
    // otherwise decide it was not worth a process.
    let launch = engine::Launch {
        args: vec![
            "--site-per-process".to_string(),
            "--host-resolver-rules=MAP *.test 127.0.0.1".to_string(),
        ],
        ..engine::Launch::default()
    };
    let mut engine = match Engine::launch_with(
        Profile::temporary().expect("a temporary profile"),
        Duration::from_secs(30),
        &launch,
    ) {
        Ok(engine) => engine,
        Err(why) => {
            eprintln!("skipped: {why}");
            return;
        }
    };
    let mut browser = engine.browser().expect("the browser's client");
    browser
        .call(
            "Target.setDiscoverTargets",
            Json::object(vec![("discover", Json::Bool(true))]),
        )
        .expect("discovery");
    let first = engine::first_page_target(&mut browser, Duration::from_secs(20)).expect("a page");
    let page = browser
        .attach(&first, Duration::from_secs(10))
        .expect("a session on the page");
    let mut tabs = Tabs::new(Tab::new(first, page, "about:blank"));
    let appearance = blinkterm::appearance::Appearance::new(
        blinkterm::appearance::Choice::Auto,
        false,
        blinkterm::appearance::Alpha::Off,
    );
    // Through the program's own way of opening a url in front, which is
    // where the engine is asked to attach to the page's frames, and in front
    // because a page the engine has never raised is a hidden page and a
    // click on one lands nowhere.
    let opened = blinkterm::app::open_delivered(
        &mut tabs,
        &mut browser,
        &appearance,
        &Identity::new(None, None, "C"),
        &Sites::none(),
        &[Ok(format!("http://holder.test:{port}/holder"))],
    );
    assert!(opened.iter().all(Result::is_ok), "{opened:?}");
    let index = tabs.active_index();
    let raised = tabs.get_mut(index).expect("the tab").target.clone();
    browser
        .call(
            "Target.activateTarget",
            Json::object(vec![("targetId", Json::string(&raised))]),
        )
        .expect("the tab comes to the front");
    let tab = tabs.get_mut(index).expect("the tab");
    viewport(&mut tab.connection);
    // And painting, as `activate` leaves it. This is not decoration: a click
    // that has to be routed into another process is hit-tested from what the
    // compositor drew, and a page nobody is drawing is a page a click lands
    // nowhere on — measured, the input in the frame did not even see the
    // `click` until the screencast was running.
    cast(&mut tab.connection, "jpeg", Some(60), WIDTH, HEIGHT);

    // What `handle_page_events` does with this tab's queue: the frames
    // acknowledged, and a `Target.attachedToTarget` given to the code that
    // adopts the frame's session and turns interception on in it.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut frames = 0;
    while frames == 0 && Instant::now() < deadline {
        for event in tab.connection.events() {
            acknowledge_frame(&mut tab.connection, &event);
            if event.method == "Target.attachedToTarget"
                && blinkterm::app::frame_attached(tab, &event)
            {
                frames += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        frames, 1,
        "the engine never attached to the cross-site frame"
    );

    // The click, in the top left of the page, which is the frame. Repeated
    // until it is answered: the browser routes a click into another process
    // from what the compositor drew, so a page that has not been painted yet
    // is a page the click lands nowhere on.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut clicked: Option<Instant> = None;
    let chooser = loop {
        assert!(
            Instant::now() < deadline,
            "the click never reached the input in the frame"
        );
        if clicked.is_none_or(|at| at.elapsed() >= Duration::from_millis(500)) {
            click_at(&mut tab.connection, 8, 8);
            clicked = Some(Instant::now());
        }
        let mut opened = None;
        for event in tab.connection.events() {
            acknowledge_frame(&mut tab.connection, &event);
            if event.method == "Page.fileChooserOpened" {
                opened = Chooser::opening(&event);
            }
        }
        if let Some(chooser) = opened {
            break chooser;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let frame_session = chooser
        .session
        .clone()
        .expect("the chooser names the session it came in on");
    assert_ne!(
        Some(frame_session.as_str()),
        tab.connection.session(),
        "the question came in on the frame's session, not the page's"
    );

    let dir = temp_dir("upload-frame");
    std::fs::write(dir.join("report.pdf"), UPLOADED).expect("a file");
    let mut upload = Upload::new(chooser, dir.clone(), None);
    let mut keys = letters("rep");
    keys.extend([press(Key::Tab), press(Key::Enter)]);
    assert_eq!(type_path(&mut upload, &keys), Typed::Send);
    tab.connection
        .notify_on(
            Some(&frame_session),
            "DOM.setFileInputFiles",
            upload.reply(),
        )
        .expect("the answer goes out on the frame's session");

    // Read out of the frame's own document, on the frame's own session:
    // proof that the file reached the input the person clicked.
    let wanted = format!("files report.pdf {}", UPLOADED.len());
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut title = String::new();
    while title != wanted && Instant::now() < deadline {
        let answer = tab
            .connection
            .call_on(
                Some(&frame_session),
                "Runtime.evaluate",
                Json::object(vec![
                    ("expression", Json::string("document.title")),
                    ("returnByValue", Json::Bool(true)),
                ]),
                Duration::from_secs(5),
            )
            .expect("the frame answers");
        title = answer
            .path(&["result", "value"])
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string();
        if title != wanted {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    assert_eq!(
        title, wanted,
        "the file never reached the input in the frame"
    );

    std::fs::remove_dir_all(&dir).ok();
    browser.close();
    drop(tabs);
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

use blinkterm::picker::{self, Gui, Outcome as Picked};

/// A picker with a window, run as the loop runs one — spawned, polled and
/// pumped until it has exited — against the input `chooser` names.
fn run_picker(command: &str, dir: &std::path::Path, chooser: &Chooser) -> Picked {
    let command = picker::Command::parse("file-picker", command).expect("a command");
    let mut gui = Gui::spawn(&command, dir, "T", chooser).expect("the picker starts");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let readable = gui.fd().is_some_and(|fd| {
            blinkterm::tty::poll_readable(&[fd], 50)
                .expect("poll")
                .contains(&fd)
        });
        if let Some(outcome) = gui.pump(readable) {
            return outcome;
        }
        if gui.fd().is_none() {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    panic!("the picker did not finish");
}

/// What a picker printed, checked and sent as `app::finish_picker` sends
/// it.
fn send_picked(client: &mut Client, chooser: &Chooser, picked: Picked) {
    let Picked::Files(paths) = picked else {
        panic!("the picker chose nothing: {picked:?}");
    };
    let files = picker::accept(paths, chooser.multiple, &Disk).expect("the files are good");
    client
        .call(
            "DOM.setFileInputFiles",
            blinkterm::upload::reply(chooser.backend_node_id, &files),
        )
        .expect("the engine takes the files");
}

/// A file input answered by a program the settings name (#58): what it
/// prints is the file the page gets, relative to the directory it started
/// in, name and size as the page reads them. And a picker that exits
/// non-zero — osascript's -128, zenity's Cancel — sends nothing, and the
/// page hears `cancel`.
#[test]
fn a_gui_picker_hands_the_page_the_file_it_printed() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let dir = an_upload_page(&mut client, false);

    click_at(&mut client, 8, 8);
    let chooser = wait_for_chooser(&client, Duration::from_secs(5));
    let picked = run_picker("sh -c 'echo \"$1\"' sh {dir}/report.pdf", &dir, &chooser);
    assert_eq!(picked, Picked::Files(vec![dir.join("report.pdf")]));
    send_picked(&mut client, &chooser, picked);
    let wanted = format!("files report.pdf {}", UPLOADED.len());
    assert_eq!(
        wait_for_title(&mut client, &wanted, Duration::from_secs(5)),
        wanted
    );

    click_at(&mut client, 8, 8);
    let chooser = wait_for_chooser(&client, Duration::from_secs(5));
    assert_eq!(
        run_picker("sh -c 'echo notes.txt; exit 1'", &dir, &chooser),
        Picked::Cancel
    );
    blinkterm::app::cancel_chooser(&mut client, None, chooser.backend_node_id);
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

/// A `multiple` input gets every line the picker printed, in order, a
/// relative one and a `file://` one among them.
#[test]
fn a_multiple_input_takes_every_line_a_picker_printed() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let dir = an_upload_page(&mut client, true);

    click_at(&mut client, 8, 8);
    let chooser = wait_for_chooser(&client, Duration::from_secs(5));
    assert!(chooser.multiple);
    let picked = run_picker(
        "sh -c 'printf \"%s\\n\" report.pdf \"file://$1/notes.txt\"' sh {dir}",
        &dir,
        &chooser,
    );
    assert_eq!(
        picked,
        Picked::Files(vec![dir.join("report.pdf"), dir.join("notes.txt")])
    );
    send_picked(&mut client, &chooser, picked);
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
// Saving a page
// ---------------------------------------------------------------------------

use blinkterm::save::{self, Job, Progress, Saved};

/// Save what `client` shows into `dir` the way the loop does: begun, then
/// polled every 20 ms, as passes come, until it is over. The frames and
/// events that come meanwhile are drained, as the loop drains them, and
/// every screencast frame is handed to `frame` with whether the job said it
/// was capturing when it came.
fn save_page(
    client: &mut Client,
    kind: save::Kind,
    dir: &std::path::Path,
    title: &str,
    url: &str,
    mut frame: impl FnMut(&Json, bool),
) -> Result<Saved, String> {
    let mut job = Job::begin(
        client,
        kind,
        "the-target",
        title,
        url,
        save::Paper::A4,
        1.0,
        Instant::now(),
    )?;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
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
            frame(&event.params, job.capturing());
        }
        match job.poll(client, dir, Instant::now()) {
            Progress::Waiting => {}
            Progress::Done(done) => return done,
        }
        assert!(Instant::now() < deadline, "the save never ended");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The size a PNG says it is, from its `IHDR`.
fn png_size(png: &[u8]) -> (u32, u32) {
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "not a PNG");
    let word = |at: usize| u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]);
    (word(16), word(20))
}

/// `Page.getLayoutMetrics`'s `cssContentSize`, as [`save::content_size`]
/// reads it.
fn content_size(client: &mut Client) -> (u32, u32) {
    let metrics = client
        .call("Page.getLayoutMetrics", Json::empty())
        .expect("the metrics");
    save::content_size(&metrics).expect("a content size")
}

/// What the page says its viewport is: `innerWidth x innerHeight x
/// devicePixelRatio`.
fn inner_size(client: &mut Client) -> String {
    match evaluate(
        client,
        "innerWidth + 'x' + innerHeight + 'x' + devicePixelRatio",
    ) {
        Json::String(size) => size,
        other => panic!("no size: {other:?}"),
    }
}

/// A page of nothing, `height` CSS pixels tall, titled `tall`.
fn tall_page(client: &mut Client, height: u32) {
    let url = format!(
        "data:text/html,<title>tall</title><body style='margin:0'>\
         <div style='height:{height}px;background:linear-gradient(%23c33,%2333c)'></div>"
    );
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(url))]),
        )
        .expect("the page loads");
    assert_eq!(
        wait_for_title(client, "tall", Duration::from_secs(15)),
        "tall"
    );
}

/// The acceptance test for #63: a long article saved as a PDF, and the same
/// page as a picture of all of it, each under the page's title, the second
/// picture numbered; the page's viewport as it was afterwards, because the
/// engine puts it back itself and nothing here does.
#[test]
fn a_page_is_saved_as_a_pdf_and_as_a_picture_of_its_whole_height() {
    let Some((mut engine, mut client, _target)) = connect_with_target() else {
        return;
    };
    let (base, dir) = download_dir("save");
    std::fs::create_dir_all(&dir).expect("the directory");
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    article(&mut client, WIDTH, HEIGHT);

    let saved = save_page(
        &mut client,
        save::Kind::Pdf,
        &dir,
        "article",
        ARTICLE,
        |_, _| {},
    )
    .expect("a PDF");
    assert_eq!(saved.path, dir.join("article.pdf"));
    assert_eq!(saved.cut, None);
    let pdf = std::fs::read(&saved.path).expect("the PDF");
    assert!(pdf.starts_with(b"%PDF-"), "{:?}", &pdf[..pdf.len().min(16)]);
    assert!(pdf.len() > 20_000, "a long article is {} bytes", pdf.len());
    eprintln!("article.pdf: {} bytes", pdf.len());

    let (width, height) = content_size(&mut client);
    let (rows, cut) = save::rows_within(width, height, 1.0);
    let saved = save_page(
        &mut client,
        save::Kind::Screenshot,
        &dir,
        "article",
        ARTICLE,
        |_, _| {},
    )
    .expect("a picture");
    assert_eq!(saved.path, dir.join("article.png"));
    assert_eq!(saved.cut, cut.then_some((rows, height)));
    let png = std::fs::read(&saved.path).expect("the picture");
    assert_eq!(
        png_size(&png),
        (width, rows),
        "the page is {width}x{height}"
    );
    eprintln!(
        "article.png: {width}x{rows} of {height}, {} bytes",
        png.len()
    );
    assert_eq!(
        inner_size(&mut client),
        format!("{WIDTH}x{HEIGHT}x1"),
        "the viewport as it was"
    );

    let again = save_page(
        &mut client,
        save::Kind::Screenshot,
        &dir,
        "article",
        ARTICLE,
        |_, _| {},
    )
    .expect("a second picture");
    assert_eq!(again.path, dir.join("article (1).png"));
    assert_eq!(
        names_in(&dir),
        ["article (1).png", "article.pdf", "article.png"]
    );

    // A page shorter than the budget is all there, not cut.
    tall_page(&mut client, 3000);
    let (width, height) = content_size(&mut client);
    assert_eq!(height, 3000);
    let saved = save_page(
        &mut client,
        save::Kind::Screenshot,
        &dir,
        "",
        "data:,",
        |_, _| {},
    )
    .expect("a picture");
    assert_eq!(saved.path, dir.join("page.png"));
    assert_eq!(saved.cut, None);
    let png = std::fs::read(&saved.path).expect("the picture");
    assert_eq!(png_size(&png), (width, 3000));

    client.close();
    engine.kill();
    let _ = std::fs::remove_dir_all(&base);
}

/// A page taller than [`save::PIXELS`] allows at its width is saved to that
/// depth and not further, and the job says where it stopped: the budget is
/// what keeps the reply under the pipe's ceiling.
#[test]
fn a_page_taller_than_the_budget_is_saved_to_its_top_and_said_so() {
    let Some((mut engine, mut client, _target)) = connect_with_target() else {
        return;
    };
    let (base, dir) = download_dir("save-tall");
    std::fs::create_dir_all(&dir).expect("the directory");
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    let tall = (save::PIXELS / u64::from(WIDTH)) as u32 + 3000;
    tall_page(&mut client, tall);
    let (width, height) = content_size(&mut client);
    assert_eq!(height, tall);
    let (rows, cut) = save::rows_within(width, height, 1.0);
    assert!(cut && rows < tall, "{rows} of {tall}");

    let started = Instant::now();
    let saved = save_page(
        &mut client,
        save::Kind::Screenshot,
        &dir,
        "tall",
        "data:,",
        |_, _| {},
    )
    .expect("a picture");
    eprintln!("{width}x{rows} of {height} in {:?}", started.elapsed());
    assert_eq!(saved.path, dir.join("tall.png"));
    assert_eq!(saved.cut, Some((rows, tall)));
    let png = std::fs::read(&saved.path).expect("the picture");
    assert_eq!(png_size(&png), (width, rows));
    assert!(u64::from(width) * u64::from(rows) <= save::PIXELS);
    assert_eq!(
        inner_size(&mut client),
        format!("{WIDTH}x{HEIGHT}x1"),
        "the viewport as it was"
    );

    client.close();
    engine.kill();
    let _ = std::fs::remove_dir_all(&base);
}

/// What the loop's guard is for: a screencast that is running while the
/// whole page is photographed sends a frame of the page laid out at its
/// whole height, which painted into the pane would be the page squashed.
/// It comes while the job says it is capturing, and the frames after it are
/// the viewport again.
#[test]
fn a_frame_of_the_whole_page_comes_while_it_is_captured() {
    let Some((mut engine, mut client, _target)) = connect_with_target() else {
        return;
    };
    let (base, dir) = download_dir("save-frame");
    std::fs::create_dir_all(&dir).expect("the directory");
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    tall_page(&mut client, 8000);
    cast(&mut client, "jpeg", Some(85), WIDTH, HEIGHT);
    // The frames of the page as it is, before anything is asked.
    std::thread::sleep(Duration::from_millis(300));
    take_frames(&mut client);

    let mut seen: Vec<(f64, bool)> = Vec::new();
    save_page(
        &mut client,
        save::Kind::Screenshot,
        &dir,
        "tall",
        "data:,",
        |params, capturing| {
            let height = params
                .path(&["metadata", "deviceHeight"])
                .and_then(Json::as_f64)
                .unwrap_or(0.0);
            seen.push((height, capturing));
        },
    )
    .expect("a picture");
    eprintln!("frames (deviceHeight, capturing): {seen:?}");
    let whole: Vec<&(f64, bool)> = seen
        .iter()
        .filter(|(height, _)| *height > f64::from(HEIGHT))
        .collect();
    assert!(
        !whole.is_empty(),
        "no frame of the whole page, so the guard guards nothing: {seen:?}"
    );
    assert!(
        whole.iter().all(|(_, capturing)| *capturing),
        "a frame of the whole page came when the guard was down: {seen:?}"
    );

    client.close();
    engine.kill();
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
    let image = blinkterm::png::decode(png, 64 * 1024 * 1024).expect("the PNG decodes");
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
    let image = blinkterm::png::decode(&png, 64 * 1024 * 1024).expect("a still decodes");
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
    let image = blinkterm::jpeg::decode(&jpeg, 64 * 1024 * 1024).expect("a frame decodes");
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
        &blinkterm::appearance::Appearance::new(
            blinkterm::appearance::Choice::Auto,
            false,
            blinkterm::appearance::Alpha::Off,
        ),
        &Identity::new(None, None, "C"),
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

    let mut appearance = blinkterm::appearance::Appearance::new(
        blinkterm::appearance::Choice::Auto,
        false,
        blinkterm::appearance::Alpha::Off,
    );
    assert!(appearance.learned((0x1c, 0x1c, 0x1c)));
    blinkterm::app::prepare_session(&mut page, &appearance, &Identity::new(None, None, "C"));
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
    blinkterm::app::prepare_session(&mut second, &appearance, &Identity::new(None, None, "C"));
    viewport(&mut second);
    go_to(&mut second, SCHEME);
    assert!(prefers_dark(&mut second), "a tab opened later");

    // And the terminal turning light turns the first page light, live.
    assert!(appearance.learned((0xfd, 0xf6, 0xe3)));
    let mut tabs = tabs;
    let first = &mut tabs.active_mut().expect("the first tab").connection;
    blinkterm::app::prepare_session(first, &appearance, &Identity::new(None, None, "C"));
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

    let forced = blinkterm::appearance::Appearance::new(
        blinkterm::appearance::Choice::Auto,
        true,
        blinkterm::appearance::Alpha::Off,
    );
    blinkterm::app::prepare_session(&mut client, &forced, &Identity::new(None, None, "C"));
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

/// A page that paints no background of its own: a line of text, and a small
/// block moving in the top corner so that a screencast has frames to send.
/// Everything below and to the right of them is the canvas, which is what
/// `--alpha` is about.
const CLEAR: &str = "data:text/html,<body style='margin:0'><p>Some text on no background.</p>\
<div id=b style='position:absolute;left:0;top:40px;width:20px;height:20px;background:%23c33'></div>\
<script>var b=document.getElementById('b'),n=0;\
function f(){n=n>80?0:n+2;b.style.left=n+'px';requestAnimationFrame(f)}f();\
document.title='ready'</script></body>";

/// The still's pixel near the bottom right, where [`CLEAR`] has nothing, as
/// red, green, blue and alpha.
fn far_corner(client: &mut Client) -> [u8; 4] {
    let (rgba, width, height) = still(client);
    let at = (((height - 5) * width + width - 5) * 4) as usize;
    [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
}

/// [`WHITE`] with the background painted by the page itself, on `body`, as
/// most real pages paint theirs: what the override alone leaves opaque.
const PAINTED: &str = "data:text/html,<body style='margin:0;background:%23fff'>\
<p>Some text on a white page.</p><script>document.title='ready'</script></body>";

/// A page whose background is a container of its own covering the page,
/// which is not `html` or `body` and so stays under `--alpha`.
const CONTAINER: &str = "data:text/html,<body style='margin:0'>\
<div style='position:fixed;left:0;top:0;right:0;bottom:0;background:%23fff'></div>\
<script>document.title='ready'</script></body>";

/// The brightest of a still's text pixels — its first line, wherever
/// something at least half opaque was painted — and how many there were.
fn text_pixels(client: &mut Client) -> (f64, usize) {
    let (rgba, width, _) = still(client);
    rgba[..(38 * width * 4) as usize]
        .chunks_exact(4)
        .filter(|p| p[3] >= 128)
        .map(|p| blinkterm::appearance::luminance((p[0], p[1], p[2])))
        .fold((0.0, 0), |(most, count), l| (f64::max(most, l), count + 1))
}

/// The appearance `--alpha` gives a session, told the way the program tells
/// one.
fn transparent(client: &mut Client, force_dark: bool) {
    let alpha = blinkterm::appearance::Appearance::new(
        blinkterm::appearance::Choice::Auto,
        force_dark,
        blinkterm::appearance::Alpha::On(100),
    );
    blinkterm::app::prepare_session(client, &alpha, &Identity::new(None, None, "C"));
}

/// `--alpha`: the override is per session and lives through what happens on
/// it, like the scheme — so it is sent once, from `Appearance::commands`, and
/// never again on a navigation.
#[test]
fn a_page_with_no_background_is_transparent_under_the_override_and_stays_so_across_a_navigation() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    go_to(&mut client, CLEAR);
    let before = far_corner(&mut client);
    eprintln!("no override: {before:?}");
    assert_eq!(before, [255, 255, 255, 255], "the engine's white, opaque");

    transparent(&mut client, false);
    let alpha = |client: &mut Client| far_corner(client)[3];
    let started = Instant::now();
    assert_eq!(wait_until(&mut client, 0, alpha), 0, "transparent in place");
    eprintln!(
        "the override on the loaded page: {:?} after {:?}",
        far_corner(&mut client),
        started.elapsed()
    );

    go_to(&mut client, CONTAINER);
    assert_eq!(
        wait_until(&mut client, 255, alpha),
        255,
        "a page that paints a container of its own keeps it"
    );
    go_to(&mut client, CLEAR);
    let navigated = far_corner(&mut client);
    eprintln!("Page.navigate back: {navigated:?}");
    assert_eq!(navigated[3], 0, "still transparent after a navigation");

    evaluate(&mut client, "document.title='leaving'");
    client
        .call("Page.reload", Json::empty())
        .expect("the page reloads");
    assert_eq!(
        wait_for_title(&mut client, "ready", Duration::from_secs(10)),
        "ready"
    );
    let reloaded = far_corner(&mut client);
    eprintln!("Page.reload: {reloaded:?}");
    assert_eq!(reloaded[3], 0, "and after a reload");

    client.close();
    engine.kill();
}

/// Why the local route's frames are keyed under `--alpha` rather than asked
/// for transparent: the screencast keeps real transparency in a PNG, and a
/// JPEG — the local route's moving frame — has nowhere to put it and paints
/// it black. Over ssh and in tmux the frames are PNG, and this is what they
/// carry.
#[test]
fn the_screencast_carries_the_alpha_as_png_and_paints_it_black_as_jpeg() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    go_to(&mut client, CLEAR);
    transparent(&mut client, false);
    let alpha = |client: &mut Client| far_corner(client)[3];
    assert_eq!(wait_until(&mut client, 0, alpha), 0, "transparent");

    let first = |client: &mut Client, png: bool| -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let signature: &[u8] = if png { b"\x89PNG" } else { b"\xff\xd8" };
            if let Some((frame, _)) = take_frames(client)
                .into_iter()
                .find(|(frame, _)| frame.starts_with(signature))
            {
                return frame;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("no frame in five seconds");
    };

    // A bare page, and one whose own white `body` is forced away.
    for (name, page) in [("bare", CLEAR), ("painted", PAINTED)] {
        go_to(&mut client, page);
        assert_eq!(wait_until(&mut client, 0, alpha), 0, "{name}: transparent");

        cast(&mut client, "png", None, WIDTH, HEIGHT);
        let png = first(&mut client, true);
        let _ = client.call("Page.stopScreencast", Json::empty());
        let image = blinkterm::png::decode(&png, 64 * 1024 * 1024).expect("a frame decodes");
        let at = (((image.height - 5) * image.width + image.width - 5) * 4) as usize;
        let pixel = &image.rgba[at..at + 4];
        eprintln!(
            "{name}: png cast {}x{}: {pixel:?}",
            image.width, image.height
        );
        assert_eq!(pixel[3], 0, "{name}: the PNG cast keeps the transparency");

        cast(&mut client, "jpeg", Some(motion::QUALITY), WIDTH, HEIGHT);
        let jpeg = first(&mut client, false);
        let _ = client.call("Page.stopScreencast", Json::empty());
        let image = blinkterm::jpeg::decode(&jpeg, 64 * 1024 * 1024).expect("a frame decodes");
        let at = (((image.height - 5) * image.width + image.width - 5) * 3) as usize;
        let pixel = &image.rgb[at..at + 3];
        eprintln!(
            "{name}: jpeg cast {}x{}: {pixel:?}",
            image.width, image.height
        );
        assert!(
            pixel.iter().all(|&c| c < 8),
            "{name}: the JPEG cast paints the transparency black: {pixel:?}"
        );
    }

    client.close();
    engine.kill();
}

/// What the override does beside `--force-dark` and beside a page that asks
/// for a dark canvas itself — measured here, not decided in the design: the
/// canvas stays transparent in both, and only the text changes.
#[test]
fn the_override_beside_forced_dark_and_beside_a_page_that_says_it_is_dark() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    go_to(&mut client, CLEAR);
    transparent(&mut client, false);
    let alpha = |client: &mut Client| far_corner(client)[3];
    assert_eq!(wait_until(&mut client, 0, alpha), 0, "transparent");
    // The brightest of the text's pixels: the first line, above the block,
    // wherever something opaque was painted.
    let text = |client: &mut Client| text_pixels(client).0;
    let black_text = text(&mut client);
    eprintln!("the override alone: text at most {black_text:.3}");
    assert!(black_text < 0.1, "black text on nothing");

    let dark_page = "data:text/html,<html style='color-scheme:dark'><body style='margin:0'>\
<p>Some text</p><script>document.title='ready'</script></body></html>";
    go_to(&mut client, dark_page);
    let dark = far_corner(&mut client);
    let dark_text = text(&mut client);
    eprintln!("color-scheme: dark, under the override: {dark:?}, text at most {dark_text:.3}");
    assert_eq!(dark[3], 0, "the override, not Blink's dark canvas");
    assert!(dark_text > 0.9, "the page's own light text");

    go_to(&mut client, CLEAR);
    transparent(&mut client, true);
    let light = |client: &mut Client| text(client) > 0.5;
    assert!(wait_until(&mut client, true, light), "the text made light");
    let forced = far_corner(&mut client);
    eprintln!(
        "--force-dark and the override: {forced:?}, text at most {:.3}",
        text(&mut client)
    );
    assert_eq!(forced[3], 0, "auto dark leaves the canvas transparent");

    // A page that paints its own white, under both: the background forced
    // away, and auto dark still making its black text light.
    go_to(&mut client, PAINTED);
    let painted = far_corner(&mut client);
    let (painted_text, count) = text_pixels(&mut client);
    eprintln!(
        "a painted white page, --force-dark and --alpha: {painted:?}, \
         text at most {painted_text:.3} over {count} pixels"
    );
    assert_eq!(painted[3], 0, "the page's own white forced transparent");
    assert!(painted_text > 0.5, "and its text light");

    client.close();
    engine.kill();
}

/// `--alpha` on a page that paints its own background, as most do: the
/// override alone leaves it white, and the adopted stylesheet makes it see
/// through — on the page already loaded, and on every one after it.
#[test]
fn a_page_that_paints_its_own_background_is_see_through_under_forced_transparency_and_stays_so() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    go_to(&mut client, PAINTED);
    assert_eq!(far_corner(&mut client), [255, 255, 255, 255], "white");
    let sheets = evaluate(&mut client, "document.styleSheets.length").as_f64();

    evaluate(&mut client, "window.kept = 1");
    let started = Instant::now();
    transparent(&mut client, false);
    let alpha = |client: &mut Client| far_corner(client)[3];
    assert_eq!(wait_until(&mut client, 0, alpha), 0, "transparent in place");
    let flipped = started.elapsed();
    assert_eq!(
        evaluate(&mut client, "window.kept").as_f64(),
        Some(1.0),
        "the same document, not a reload: runImmediately"
    );
    eprintln!(
        "forced transparent on the loaded page: {:?} after {flipped:?}",
        far_corner(&mut client)
    );

    let (text, count) = text_pixels(&mut client);
    eprintln!("the text: at most {text:.3} over {count} opaque pixels");
    assert!(count > 0, "the text is still there, opaque");
    assert!(text < 0.1, "and black");

    go_to(&mut client, PAINTED);
    assert_eq!(far_corner(&mut client)[3], 0, "after a navigation");
    evaluate(&mut client, "document.title='leaving'");
    client
        .call("Page.reload", Json::empty())
        .expect("the page reloads");
    assert_eq!(
        wait_for_title(&mut client, "ready", Duration::from_secs(10)),
        "ready"
    );
    assert_eq!(far_corner(&mut client)[3], 0, "after a reload");

    // Registered again, as a colour re-send does: still one sheet.
    transparent(&mut client, false);
    std::thread::sleep(Duration::from_millis(200));

    // What the page can see of it: one adopted sheet, its own sheets as they
    // were, and nothing on its window.
    let flag = evaluate(&mut client, "typeof __blinktermAlpha");
    let own = evaluate(&mut client, "document.styleSheets.length").as_f64();
    let adopted = evaluate(&mut client, "document.adoptedStyleSheets.length").as_f64();
    eprintln!("the page sees: flag {flag}, styleSheets {own:?}, adoptedStyleSheets {adopted:?}");
    assert_eq!(flag.as_str(), Some("undefined"), "the flag is the world's");
    assert_eq!(own, sheets, "styleSheets unchanged");
    assert_eq!(adopted, Some(1.0), "one adopted sheet, however often told");

    client.close();
    engine.kill();
}

/// A page whose iframe, in the same process, paints its own white `body`
/// at (100, 100) to (300, 200) — with a script of its own when `script`.
fn framed(script: bool) -> String {
    let script = if script { "<script>1</script>" } else { "" };
    format!(
        "data:text/html,<body style='margin:0'>\
<iframe srcdoc='<body style=margin:0;background:%23fff>{script}</body>' \
style='position:absolute;left:100px;top:100px;width:200px;height:100px;border:0'></iframe>\
<script>document.title='ready'</script></body>"
    )
}

/// The alpha of the still's pixel in the middle of [`framed`]'s iframe.
fn inside_the_frame(client: &mut Client) -> u8 {
    let (rgba, width, _) = still(client);
    rgba[((150 * width + 200) * 4 + 3) as usize]
}

/// A same-process iframe is a frame of the same target, so the script
/// reaches it and its own white `body` goes too: the frame already loaded
/// when the script is registered, and one loaded after it.
///
/// With one exception, measured here and documented rather than defended:
/// the engine makes a frame's JavaScript context only when something needs
/// it, and a script registered for new documents runs when it is made — so
/// an iframe with no script of its own, that nothing on the page reaches
/// into, is never told, and keeps its white. Touching its document from the
/// page makes the context, and the script runs then.
#[test]
fn forced_transparency_reaches_a_same_process_iframe() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);

    go_to(&mut client, &framed(true));
    assert_eq!(
        wait_until(&mut client, 255, inside_the_frame),
        255,
        "the iframe's white"
    );
    transparent(&mut client, false);
    let in_place = wait_until(&mut client, 0, inside_the_frame);
    eprintln!("the iframe's pixel, in place: alpha {in_place}");
    assert_eq!(in_place, 0, "the frame already loaded");
    go_to(&mut client, &framed(true));
    let navigated = wait_until(&mut client, 0, inside_the_frame);
    eprintln!("the iframe's pixel, after a navigation: alpha {navigated}");
    assert_eq!(navigated, 0, "and a frame loaded after it");

    go_to(&mut client, &framed(false));
    std::thread::sleep(Duration::from_millis(500));
    let untouched = inside_the_frame(&mut client);
    evaluate(
        &mut client,
        "document.querySelector('iframe').contentDocument.nodeType",
    );
    let touched = wait_until(&mut client, 0, inside_the_frame);
    eprintln!(
        "an iframe with no script of its own: alpha {untouched} untouched, \
         {touched} once the page reaches into it"
    );
    assert_eq!(touched, 0, "told once its context is made");

    client.close();
    engine.kill();
}

/// alt+shift+s under `--alpha`: the PNG saved is the engine's, so the forced
/// transparency is in the file.
#[test]
fn a_saved_picture_under_forced_transparency_keeps_it() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    go_to(&mut client, PAINTED);
    transparent(&mut client, false);
    let alpha = |client: &mut Client| far_corner(client)[3];
    assert_eq!(wait_until(&mut client, 0, alpha), 0, "transparent");

    let answer = client
        .call(
            "Page.captureScreenshot",
            save::capture_params(WIDTH, HEIGHT),
        )
        .expect("a capture");
    let data = answer.get("data").and_then(Json::as_str).expect("data");
    let png = blinkterm::base64::decode(data.as_bytes()).expect("base64");
    let image = blinkterm::png::decode(&png, 64 * 1024 * 1024).expect("a PNG");
    let at = (((image.height - 5) * image.width + image.width - 5) * 4) as usize;
    let corner = &image.rgba[at..at + 4];
    eprintln!("the saved picture's corner: {corner:?}");
    assert_eq!(corner[3], 0, "the file keeps the transparency");

    client.close();
    engine.kill();
}

/// The appearance `--alpha` gives a session on the local route, where the
/// frames are keyed: the backgrounds painted [`blinkterm::chroma::KEY`].
fn keyed(client: &mut Client) {
    let mut alpha = blinkterm::appearance::Appearance::new(
        blinkterm::appearance::Choice::Auto,
        false,
        blinkterm::appearance::Alpha::On(100),
    );
    alpha.keyed = true;
    blinkterm::app::prepare_session(client, &alpha, &Identity::new(None, None, "C"));
}

/// Black text, a photograph, a green button, a box of vivid purple and a
/// half-black overlay, on a white page: what a key costs, in one place.
const MIXED: &str = "data:text/html,<body style='margin:0;background:%23fff;font:16px sans-serif'>\
<h1 style='margin:8px'>Black text, a heading</h1>\
<p style='margin:8px'>Body text in black on a white page, and \
<a href='%23' style='color:%231a0dab'>a blue link</a>.</p>\
<button style='position:absolute;left:8px;top:120px;width:120px;height:32px;background:%2334a853;color:%23fff;border:0'>Green</button>\
<div style='position:absolute;left:160px;top:120px;width:120px;height:32px;background:%23d500f9'></div>\
<canvas id=c width=320 height=200 style='position:absolute;left:20px;top:220px'></canvas>\
<div style='position:absolute;left:420px;top:240px;width:240px;height:120px;background:rgba(0,0,0,0.5)'></div>\
<script>var c=document.getElementById('c').getContext('2d');\
var g=c.createLinearGradient(0,0,320,200);g.addColorStop(0,'%2387ceeb');\
g.addColorStop(0.5,'%23228b22');g.addColorStop(1,'%23ffd700');c.fillStyle=g;c.fillRect(0,0,320,200);\
for(var i=0;i<400;i++){c.fillStyle='hsla('+(i*37%25360)+',70%25,'+(30+i%2540)+'%25,0.5)';\
c.beginPath();c.arc((i*53)%25320,(i*29)%25200,4+i%256,0,7);c.fill()}\
document.title='ready'</script></body>";

/// RGBA, its width and its height.
type Picture = (Vec<u8>, u32, u32);

/// A keyed JPEG frame of `page` at 1280x768 and the keyed still of the same
/// moment, as the program makes them, and how long the frame took to decode
/// plain and keyed.
fn keyed_frame_and_still(
    client: &mut Client,
    page: &str,
) -> (Picture, Picture, (Duration, Duration)) {
    go_to(client, page);
    let is_key = |client: &mut Client| {
        let (rgba, width, height) = still(client);
        let at = (((height - 5) * width + width - 5) * 4) as usize;
        rgba[at..at + 3] == [255, 0, 255]
    };
    assert!(wait_until(client, true, is_key), "painted on the key");
    cast(client, "jpeg", Some(motion::QUALITY), WIDE, TALL);
    let deadline = Instant::now() + Duration::from_secs(5);
    let jpeg = loop {
        if let Some((frame, _)) = take_frames(client).pop() {
            break frame;
        }
        assert!(Instant::now() < deadline, "no frame");
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = client.call("Page.stopScreencast", Json::empty());

    let runs = 30;
    let started = Instant::now();
    for _ in 0..runs {
        let _ = blinkterm::jpeg::decode(&jpeg, 64 << 20).expect("decodes");
    }
    let plain = started.elapsed() / runs;
    let started = Instant::now();
    let mut frame = None;
    for _ in 0..runs {
        let mut image =
            blinkterm::jpeg::decode_rgba_with(&jpeg, 64 << 20, blinkterm::chroma::key_pixel)
                .expect("decodes");
        blinkterm::chroma::despill(&mut image.rgba, image.width, image.height);
        frame = Some(image);
    }
    let keyed = started.elapsed() / runs;
    let frame = frame.expect("a frame");

    let (mut rgba, width, height) = still(client);
    blinkterm::chroma::key(&mut rgba, width, height);
    (
        (frame.rgba, frame.width, frame.height),
        (rgba, width, height),
        (plain, keyed),
    )
}

/// The share of the pixels of `rgba` inside `rect` (CSS pixels at scale 1)
/// that `keep` says yes to.
fn share(rgba: &[u8], width: u32, rect: (u32, u32, u32, u32), keep: impl Fn(&[u8]) -> bool) -> f64 {
    let (x0, y0, w, h) = rect;
    let mut yes = 0;
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let at = ((y * width + x) * 4) as usize;
            if keep(&rgba[at..at + 4]) {
                yes += 1;
            }
        }
    }
    f64::from(yes) / f64::from(w * h)
}

/// `--alpha` on the local route: the page painted on the key, a JPEG frame
/// keyed as it is decoded, and the still keyed the same way — so the page is
/// see-through while it moves as well as at rest, and looks the same in
/// both. What is measured: the bare page cleared, the text left opaque and
/// without a magenta fringe, and what the key costs in content and time.
#[test]
fn under_a_key_a_moving_frame_is_as_see_through_as_the_still() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
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
        .expect("the size");
    go_to(&mut client, PAINTED);
    keyed(&mut client);

    let ((frame, width, _), (still, still_width, _), (plain, keyed)) =
        keyed_frame_and_still(&mut client, PAINTED);
    assert_eq!(width, WIDE);
    assert_eq!(still_width, WIDE);
    let clear = |p: &[u8]| p[3] == 0;
    // Everything below the line of text is the bare page.
    let bare = (0, 60, WIDE, TALL - 60);
    let (frame_bare, still_bare) = (
        share(&frame, width, bare, clear),
        share(&still, width, bare, clear),
    );
    // The text: opaque, dark, and no key left in it.
    let text = (0, 0, 400, 40);
    let opaque = |p: &[u8]| p[3] >= 128;
    let fringe = frame
        .chunks_exact(4)
        .take((40 * width) as usize)
        .filter(|p| p[3] == 255)
        .map(|p| i32::from(p[0].min(p[2])) - i32::from(p[1]))
        .max()
        .unwrap_or(0);
    eprintln!(
        "keyed, white page: bare page clear {:.4} moving, {:.4} at rest; text opaque {:.4}; \
         the most key left in an opaque pixel {fringe}; decode {plain:?}, decode and key {keyed:?}",
        frame_bare,
        still_bare,
        share(&frame, width, text, opaque),
    );
    assert!(frame_bare > 0.99, "the moving frame is see-through");
    assert_eq!(still_bare, 1.0, "and the still");
    assert!(share(&frame, width, text, opaque) > 0.02, "the text stays");
    assert!(fringe < 32, "no magenta fringe: {fringe}");

    let ((frame, width, _), (still, _, _), _) = keyed_frame_and_still(&mut client, MIXED);
    let green = (8, 120, 120, 32);
    let purple = (160, 120, 120, 32);
    let photo = (20, 220, 320, 200);
    let overlay = (420, 240, 240, 120);
    let alpha_mean = |rgba: &[u8], rect: (u32, u32, u32, u32)| {
        let (x0, y0, w, h) = rect;
        let mut sum = 0u64;
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                sum += u64::from(rgba[((y * width + x) * 4 + 3) as usize]);
            }
        }
        sum as f64 / f64::from(w * h) / 255.0
    };
    eprintln!(
        "keyed, mixed page, moving / at rest: green button opaque {:.3} / {:.3}, \
         vivid purple box opaque {:.3} / {:.3}, photo opaque {:.3} / {:.3}, \
         50% overlay alpha {:.2} / {:.2}, bare page clear {:.4} / {:.4}",
        share(&frame, width, green, |p| p[3] == 255),
        share(&still, width, green, |p| p[3] == 255),
        share(&frame, width, purple, |p| p[3] == 255),
        share(&still, width, purple, |p| p[3] == 255),
        share(&frame, width, photo, |p| p[3] == 255),
        share(&still, width, photo, |p| p[3] == 255),
        alpha_mean(&frame, overlay),
        alpha_mean(&still, overlay),
        share(&frame, width, (700, 400, 500, 300), clear),
        share(&still, width, (700, 400, 500, 300), clear),
    );
    assert!(
        share(&frame, width, green, |p| p[3] == 255) > 0.95,
        "green stays"
    );
    assert!(
        share(&still, width, photo, |p| p[3] == 255) > 0.95,
        "a photo stays"
    );
    assert!(
        share(&still, width, purple, |p| p[3] == 255) < 0.1,
        "the key's own hue goes: the cost of the colour"
    );

    client.close();
    engine.kill();
}

/// alt+shift+s under a key: the engine's PNG is of the page on magenta, and
/// the file written is keyed, transparent where the page is.
#[test]
fn a_saved_picture_under_a_key_is_keyed_before_it_is_written() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    go_to(&mut client, PAINTED);
    keyed(&mut client);
    let magenta = |client: &mut Client| far_corner(client) == [255, 0, 255, 255];
    assert!(wait_until(&mut client, true, magenta), "on the key");

    let answer = client
        .call(
            "Page.captureScreenshot",
            save::capture_params(WIDTH, HEIGHT),
        )
        .expect("a capture");
    let data = answer.get("data").and_then(Json::as_str).expect("data");
    let png = blinkterm::base64::decode(data.as_bytes()).expect("base64");
    let file = save::key_png(&png).expect("keyed");
    let image = blinkterm::png::decode(&file, 64 * 1024 * 1024).expect("a PNG");
    let at = (((image.height - 5) * image.width + image.width - 5) * 4) as usize;
    let corner = &image.rgba[at..at + 4];
    eprintln!(
        "the saved picture's corner: {corner:?}; {} bytes from the engine, {} written",
        png.len(),
        file.len()
    );
    assert_eq!(corner[3], 0, "the file is clear where the page is");

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

/// An engine started with extras, and a session on its first page: what
/// `connect_in_with_target` is for `Engine::launch`, for
/// `Engine::launch_with`. On a temporary profile, and skipping as the rest
/// do.
fn launched(launch: &engine::Launch) -> Option<(Engine, Client, Client)> {
    if std::env::var_os(engine::ENGINE_ENV).is_none() {
        eprintln!(
            "skipped: {} is not set; name a Chromium to run this against",
            engine::ENGINE_ENV
        );
        return None;
    }
    let profile = Profile::temporary().expect("a temporary profile");
    let engine =
        Engine::launch_with(profile, Duration::from_secs(30), launch).expect("the engine starts");
    let mut browser = engine.browser().expect("the browser's client");
    let target =
        engine::first_page_target(&mut browser, Duration::from_secs(20)).expect("a first page");
    let page = browser
        .attach(&target, Duration::from_secs(10))
        .expect("a session on the page");
    Some((engine, browser, page))
}

/// `--user-agent` and `--proxy` are engine flags and nothing else, measured
/// to cover the browser, the first page and a target made later; a proxy
/// that refuses connections is the fastest proof the proxy took, and a
/// `data:` url never goes through it.
#[test]
fn a_user_agent_and_a_proxy_reach_the_engine_and_the_agent_covers_a_later_tab() {
    let launch = engine::Launch {
        user_agent: Some("blinkterm-test/1".to_string()),
        proxy: Some("127.0.0.1:1".to_string()),
        ..engine::Launch::default()
    };
    let Some((mut engine, mut browser, mut page)) = launched(&launch) else {
        return;
    };
    let version = browser
        .call("Browser.getVersion", Json::empty())
        .expect("a version");
    assert_eq!(
        version.get("userAgent").and_then(Json::as_str),
        Some("blinkterm-test/1")
    );
    assert_eq!(
        evaluate(&mut page, "navigator.userAgent").as_str(),
        Some("blinkterm-test/1"),
        "the first page"
    );
    let created = browser
        .call(
            "Target.createTarget",
            Json::object(vec![("url", Json::string("about:blank"))]),
        )
        .expect("a new target");
    let later = created
        .get("targetId")
        .and_then(Json::as_str)
        .expect("the engine says which")
        .to_string();
    let mut later = browser
        .attach(&later, Duration::from_secs(10))
        .expect("a session on the later page");
    assert_eq!(
        evaluate(&mut later, "navigator.userAgent").as_str(),
        Some("blinkterm-test/1"),
        "a page made later"
    );

    let reply = page
        .call_within(
            "Page.navigate",
            Json::object(vec![("url", Json::string("http://example.test/"))]),
            Duration::from_secs(20),
        )
        .expect("a reply");
    let error = reply
        .get("errorText")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string();
    assert_eq!(error, "net::ERR_PROXY_CONNECTION_FAILED", "{reply:?}");
    assert_eq!(load::reason(&error), "the proxy would not connect");

    page.call(
        "Page.navigate",
        Json::object(vec![(
            "url",
            Json::string("data:text/html,<title>not proxied</title>"),
        )]),
    )
    .expect("a data url loads");
    assert_eq!(
        wait_for_title(&mut page, "not proxied", Duration::from_secs(10)),
        "not proxied"
    );

    later.close();
    page.close();
    browser.close();
    engine.kill();
}

/// An `--engine-arg` goes to the engine as written: `--accept-lang=ja` is
/// the language flag the headless shell was measured to honour.
#[test]
fn an_engine_arg_goes_through_as_written() {
    let launch = engine::Launch {
        args: vec!["--accept-lang=ja".to_string()],
        ..engine::Launch::default()
    };
    let Some((mut engine, mut browser, mut page)) = launched(&launch) else {
        return;
    };
    assert_eq!(
        evaluate(&mut page, "navigator.language").as_str(),
        Some("ja")
    );
    page.close();
    browser.close();
    engine.kill();
}

/// What `drive` does with three urls, without the pane: the first tab told
/// to load, a tab each for the rest through `Target.createTarget`, the first
/// put back in front. Three tabs, the first in front and still a visible
/// page without being raised again, each at the url it was given, and none of the ones
/// this program made counted twice when the engine announces them.
#[test]
fn several_urls_open_several_tabs_with_the_first_in_front() {
    let Some((mut engine, page, target)) = connect_with_target() else {
        return;
    };
    let (mut browser, mut tabs) = tabbed(&engine, page, target);
    let urls = [
        "data:text/html,<title>one</title>",
        "data:text/html,<title>two</title>",
        "data:text/html,<title>three</title>",
    ];
    {
        let first = tabs.active_mut().expect("the first tab");
        first.url = urls[0].to_string();
        first
            .connection
            .call(
                "Page.navigate",
                Json::object(vec![("url", Json::string(urls[0]))]),
            )
            .expect("the first page loads");
    }
    for &url in &urls[1..] {
        let created = browser
            .call(
                "Target.createTarget",
                Json::object(vec![("url", Json::string(url))]),
            )
            .expect("a new target");
        let opened = created
            .get("targetId")
            .and_then(Json::as_str)
            .expect("the engine says which")
            .to_string();
        let connection = browser
            .attach(&opened, Duration::from_secs(10))
            .expect("a session on it");
        tabs.open(Tab::new(opened, connection, url));
    }
    assert!(tabs.select(1));

    assert!(!pump(
        &mut browser,
        &mut tabs,
        Duration::from_secs(2),
        |tabs| tabs.len() > 3
    ));
    assert_eq!(tabs.len(), 3);
    assert_eq!(tabs.active_index(), 0, "the first url is the tab in front");
    for (index, (url, name)) in urls.iter().zip(["one", "two", "three"]).enumerate() {
        let tab = tabs.get_mut(index).expect("a tab");
        assert_eq!(tab.url, *url, "tab {index}");
        assert_eq!(
            wait_for_title(&mut tab.connection, name, Duration::from_secs(10)),
            name,
            "tab {index} loaded what it was given"
        );
    }
    let front = tabs.active_mut().expect("the first tab");
    assert_eq!(
        evaluate(&mut front.connection, "document.visibilityState").as_str(),
        Some("visible"),
        "the tab in front is a page that paints"
    );

    drop(tabs);
    browser.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// Normal mode: link hints and the scroll keys
// ---------------------------------------------------------------------------

use blinkterm::hints::{self, Hint, Hints, Kind};
use blinkterm::normal;

/// The frame the hint page puts beside its own links: a link, a button, and
/// enough below them that the frame scrolls.
fn hint_frame() -> String {
    let filler: String = (0..60)
        .map(|i| format!("<p>frame filler {i}</p>"))
        .collect();
    format!(
        "<!doctype html><body style='margin:0;font:14px sans-serif'>\
         <p><a id=inner href='/inner-target'>a link inside the frame</a></p>\
         <p><button id=innerbtn>frame button</button></p>{filler}\
         <p><a id=innerdeep href='/inner-deep'>deep in the frame</a></p></body>"
    )
}

/// The page [`hints`]'s module doc was measured on: every kind of thing that
/// is clickable, and every way of being clickable and not a hint — hidden
/// three ways, inside a closed `<details>`, covered by another element, in a
/// cross-origin frame, an image map's area, and below the fold. At `/`, with
/// its frame at `/inner` and the same frame again on `localhost`, which is
/// another origin.
fn hint_pages(port: u16) -> Vec<(String, String)> {
    // Explicit block metrics keep the viewport boundary independent of the
    // platform font's line box (Helvetica on macOS, Liberation Sans in CI).
    let below: String = (0..80)
        .map(|i| {
            format!(
                "<p style='height:30px;margin:10px 0'><a href='/below/{i}'>\
                 below the fold link {i}</a></p>"
            )
        })
        .collect();
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>loading</title>\
         <body style='margin:0;font:14px sans-serif;background:#fff;color:#000'>\
         <h1>Hint targets</h1>\
         <p><a id=a1 href='/one'>plain link</a> and <a id=a2 href='/two'>another with <b>bold</b> inside</a>\
          and <a id=a3 href='javascript:void(0)'>a javascript: link</a> and <a>an anchor with no href</a>\
          and <a id=a4 href='#frag'>a fragment</a></p>\
         <p><a id=wrap href='/wrap' style='display:inline'>a link that is long enough to wrap onto a \
         second line when the viewport is six hundred and forty pixels wide, which this one is, so it \
         has two client rects</a></p>\
         <p><button id=b1>button</button> <button id=b2 disabled>disabled</button>\
          <input id=i1 type=text placeholder=text> <input id=i2 type=checkbox> <input type=hidden value=x>\
          <input id=i3 type=submit value=Go> <select id=s1><option>one</option></select> <textarea id=t1></textarea></p>\
         <p><span id=oc onclick='1'>onclick span</span> <span id=rl role=link tabindex=0>role=link</span>\
          <span id=rb role=button>role=button</span> <div id=ce contenteditable>editable div</div></p>\
         <p><label for=i1 id=lab>label for the text input</label> <label id=lab2><input id=i4 type=radio> radio in a label</label></p>\
         <div id=ptr style='cursor:pointer;width:100px;height:20px;background:#eee'><span>cursor:pointer div</span></div>\
         <div style='cursor:pointer;width:100px;height:20px'><div style='cursor:pointer'>nested pointer (one hint)</div></div>\
         <p style='display:none'><a id=hid1 href='/hidden'>display:none</a></p>\
         <p style='visibility:hidden'><a id=hid2 href='/hidden2'>visibility:hidden</a></p>\
         <p style='opacity:0'><a id=hid3 href='/hidden3'>opacity:0</a></p>\
         <details><summary id=sum>a summary</summary><a id=hid4 href='/closed'>inside closed details</a></details>\
         <div style='position:relative;height:30px'><a id=under href='/under' style='position:absolute;left:0;top:0'>covered link</a>\
          <div id=cover style='position:absolute;left:0;top:0;width:200px;height:30px;background:#ccc'></div></div>\
         <div id=sh></div>\
         <iframe id=same src='/inner' style='width:300px;height:100px;border:2px solid #000'></iframe>\
         <iframe id=cross src='http://localhost:{port}/inner' style='width:300px;height:100px'></iframe>\
         <map name=m><area id=ar shape=rect coords='0,0,50,50' href='/area'></map>\
         <img usemap='#m' width=60 height=60 alt='' style='display:block;background:#8cf'>\
         {below}\
         <script>\
         var sh = document.getElementById('sh').attachShadow({{mode:'open'}});\
         sh.innerHTML = '<a id=shadowlink href=\"/shadow\">a link in an open shadow root</a> <button id=shadowbtn>shadow button</button>';\
         onload = function () {{ document.title = 'ready'; }};\
         </script></body>"
    );
    vec![
        ("/".to_string(), page),
        ("/inner".to_string(), hint_frame()),
    ]
}

/// An engine on the hint page at `WIDTH` by `height`, with the world the
/// program makes for find and hints alike, and the page's url.
fn hinting(height: u32) -> Option<(Engine, Client, String, i64, String)> {
    let port = serve_pages(hint_pages);
    let (engine, mut client, target) = connect_with_target()?;
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    client
        .call(
            "Emulation.setDeviceMetricsOverride",
            Json::object(vec![
                ("width", Json::number(WIDTH)),
                ("height", Json::number(height)),
                ("deviceScaleFactor", Json::number(1)),
                ("mobile", Json::Bool(false)),
            ]),
        )
        .expect("the viewport");
    let url = format!("http://127.0.0.1:{port}/");
    open(&mut client, &url, "ready");
    let context = find_world(&mut client);
    Some((engine, client, target, context, url))
}

/// `f`'s question, waited for: the hints in view, labelled.
fn collect(client: &mut Client, context: i64, px: f64) -> Hints {
    let reply = client
        .call_within(
            "Runtime.callFunctionOn",
            hints::collect_params(context, px),
            Duration::from_secs(5),
        )
        .expect("the page answers the collect");
    Hints::from_reply(&reply, false)
        .unwrap_or_else(|| panic!("an answer of the script's shape: {reply}"))
}

/// Put the labels up, a cell of `px` CSS pixels tall.
fn show(client: &mut Client, context: i64, hints: &Hints, px: f64) {
    let reply = client
        .call_within(
            "Runtime.callFunctionOn",
            hints::show_params(context, &hints.labels, px),
            Duration::from_secs(5),
        )
        .expect("the page draws the labels");
    assert!(reply.get("exceptionDetails").is_none(), "{reply}");
}

/// Take them down.
fn clear(client: &mut Client, context: i64) {
    client
        .call_within(
            "Runtime.callFunctionOn",
            hints::clear_params(context),
            Duration::from_secs(5),
        )
        .expect("the page takes the labels away");
}

/// How many pixels of a PNG are the labels' yellow, `#ffd400`, within 8 of
/// each channel.
fn label_pixels(png: &[u8]) -> usize {
    let image = blinkterm::png::decode(png, 64 * 1024 * 1024).expect("the PNG decodes");
    image
        .rgba
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[..3]
                .iter()
                .zip([0xff, 0xd4, 0x00])
                .all(|(&have, want): (&u8, u8)| have.abs_diff(want) <= 8)
        })
        .count()
}

/// A still once the labels have had a frame to paint in.
fn labelled_still(client: &mut Client) -> Vec<u8> {
    std::thread::sleep(Duration::from_millis(200));
    screenshot(client, "png", None)
}

/// The three mouse events a typed label sends, as calls so that the test
/// knows they have landed.
fn click_hint(client: &mut Client, hint: &Hint) {
    for params in hints::click_params(hint.at) {
        client
            .call("Input.dispatchMouseEvent", params)
            .expect("the click is dispatched");
    }
}

/// Type `hint`'s label into a fresh set of labels, a key at a time, and hand
/// back what the typing chose — so that the label and the hint under it are
/// the program's, not the test's.
fn type_label(hints: &Hints, index: usize) -> Hint {
    let mut typing = hints.clone();
    let label = hints.labels[index].clone();
    let mut chosen = None;
    for c in label.chars() {
        let key = KeyInput {
            key: Key::Char(c),
            mods: Mods::default(),
            action: KeyAction::Press,
            text: Some(c),
        };
        match typing.step(&key) {
            hints::Typed::Chosen(hint) => chosen = Some(hint),
            hints::Typed::Narrowed(n) => assert!(n >= 1),
            other => panic!("{c} of {label}: {other:?}"),
        }
    }
    chosen.unwrap_or_else(|| panic!("{label} chose nothing"))
}

/// The first hint whose `at` is on the element `id` in the page.
fn hint_on(client: &mut Client, hints: &Hints, id: &str) -> usize {
    let mut rect = |what: &str| {
        page_number(
            client,
            &format!("document.getElementById('{id}').getBoundingClientRect().{what}"),
        )
    };
    let (left, top, right, bottom) = (rect("left"), rect("top"), rect("right"), rect("bottom"));
    hints
        .hints
        .iter()
        .position(|hint| (left..=right).contains(&hint.at.0) && (top..=bottom).contains(&hint.at.1))
        .unwrap_or_else(|| panic!("no hint on #{id}: {:?}", hints.hints))
}

/// The acceptance criterion of #13 for "collect the clickable elements in the
/// viewport": the measured set, no more and no less.
#[test]
fn the_clickable_things_in_view_are_found_and_the_hidden_covered_and_offscreen_ones_are_not() {
    let Some((mut engine, mut client, _, context, _)) = hinting(HEIGHT) else {
        return;
    };
    let found = collect(&mut client, context, 16.0);
    let count = |kind: Kind| found.hints.iter().filter(|hint| hint.kind == kind).count();
    eprintln!(
        "{} hints at {WIDTH}x{HEIGHT}: {:?}",
        found.hints.len(),
        found.hints
    );
    assert_eq!(found.hints.len(), 18, "{:?}", found.hints);
    assert_eq!(
        (count(Kind::Link), count(Kind::Edit), count(Kind::Click)),
        (4, 4, 10)
    );
    let first = &found.hints[0];
    assert!(
        (first.at.0 - 27.0).abs() <= 1.0 && (first.at.1 - 78.0).abs() <= 1.0,
        "the first hint, the first link, at {:?}",
        first.at
    );
    assert!(found.hints[0].href.ends_with("/one"));
    for hidden in [
        "/hidden", "/hidden2", "/hidden3", "/closed", "/under", "/area",
    ] {
        assert!(
            !found.hints.iter().any(|hint| hint.href.ends_with(hidden)),
            "{hidden} is not a hint"
        );
    }
    assert!(
        !found.hints.iter().any(|hint| hint.href.contains("/below/")),
        "nothing below the fold"
    );
    assert!(found
        .hints
        .iter()
        .all(|hint| hint.kind == Kind::Link || hint.href.is_empty()));
    assert!(
        !found
            .hints
            .iter()
            .any(|hint| hint.href.starts_with("javascript:")),
        "a javascript: link is a click, not a link to open"
    );

    // Taller, and the shadow root, the frames and the summary come into view.
    client
        .call(
            "Emulation.setDeviceMetricsOverride",
            Json::object(vec![
                ("width", Json::number(WIDTH)),
                ("height", Json::number(HEIGHT * 2)),
                ("deviceScaleFactor", Json::number(1)),
                ("mobile", Json::Bool(false)),
            ]),
        )
        .expect("the viewport");
    wait_until(&mut client, f64::from(HEIGHT * 2), |client| {
        page_number(client, "innerHeight")
    });
    let tall = collect(&mut client, context, 16.0);
    eprintln!("{} hints at {WIDTH}x{}", tall.hints.len(), HEIGHT * 2);
    // 26: the eighteen, the summary and the details, the shadow root's link
    // and button, the frame's link and button, and two links below the old
    // fold. (27 is what the same page has at 1280x720, which is 50%.)
    assert_eq!(tall.hints.len(), 26, "{:?}", tall.hints);
    assert!(
        tall.hints.iter().any(|hint| hint.href.ends_with("/shadow")),
        "a link in an open shadow root"
    );
    let rect = |client: &mut Client, what: &str| {
        page_number(
            client,
            &format!("document.getElementById('same').getBoundingClientRect().{what}"),
        )
    };
    let (left, top) = (rect(&mut client, "left"), rect(&mut client, "top"));
    let (right, bottom) = (rect(&mut client, "right"), rect(&mut client, "bottom"));
    let framed = tall
        .hints
        .iter()
        .find(|hint| hint.href.ends_with("/inner-target"))
        .expect("the same-origin frame's link");
    assert!(
        (left..=right).contains(&framed.at.0) && (top..=bottom).contains(&framed.at.1),
        "the frame's link is inside the frame's box: {:?} in {left},{top}..{right},{bottom}",
        framed.at
    );
    assert!(
        !tall
            .hints
            .iter()
            .any(|hint| hint.href.contains("localhost")),
        "nothing from the cross-origin frame"
    );

    client.close();
    engine.kill();
}

#[test]
fn labels_are_drawn_by_the_page_and_taken_away_without_a_trace() {
    let Some((mut engine, mut client, _, context, _)) = hinting(HEIGHT) else {
        return;
    };
    let body = page_number(&mut client, "document.body.innerHTML.length");
    assert_eq!(label_pixels(&labelled_still(&mut client)), 0);
    let found = collect(&mut client, context, 16.0);
    show(&mut client, context, &found, 16.0);
    let shown = label_pixels(&labelled_still(&mut client));
    eprintln!("{} labels, {shown} yellow pixels", found.labels.len());
    assert!(shown > 1000, "the labels are on the screen: {shown}");
    assert_eq!(
        page_number(&mut client, "document.documentElement.children.length"),
        3.0,
        "one element, on <html>"
    );
    assert_eq!(
        page_number(&mut client, "document.body.innerHTML.length"),
        body,
        "and nothing in the body"
    );
    assert_eq!(
        evaluate(
            &mut client,
            "document.querySelector('blinkterm-hints').shadowRoot === null"
        )
        .as_bool(),
        Some(true),
        "the page cannot reach the labels"
    );
    assert_eq!(
        evaluate(&mut client, "typeof __blinktermHints").as_str(),
        Some("undefined"),
        "nor the script's state"
    );

    // Narrowed to the labels starting with the first letter: fewer.
    let prefix = &found.labels[0][..1];
    client
        .call_within(
            "Runtime.callFunctionOn",
            hints::narrow_params(context, prefix),
            Duration::from_secs(5),
        )
        .expect("the page narrows");
    let narrowed = label_pixels(&labelled_still(&mut client));
    assert!(
        narrowed > 0 && narrowed < shown,
        "{narrowed} of {shown} after {prefix}"
    );

    clear(&mut client, context);
    assert_eq!(label_pixels(&labelled_still(&mut client)), 0);
    assert_eq!(
        page_number(&mut client, "document.documentElement.children.length"),
        2.0
    );
    assert_eq!(
        page_number(&mut client, "document.body.innerHTML.length"),
        body
    );

    client.close();
    engine.kill();
}

#[test]
fn typing_a_label_clicks_the_thing_under_it_and_a_field_is_insert_mode() {
    let Some((mut engine, mut client, _, context, url)) = hinting(HEIGHT) else {
        return;
    };
    let found = collect(&mut client, context, 16.0);
    let link = type_label(&found, hint_on(&mut client, &found, "a1"));
    assert_eq!(link.kind, Kind::Link);
    click_hint(&mut client, &link);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut href = String::new();
    while Instant::now() < deadline {
        href = evaluate(&mut client, "location.href")
            .as_str()
            .unwrap_or_default()
            .to_string();
        if href.ends_with("/one") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(href.ends_with("/one"), "the link was followed: {href}");

    open(&mut client, &url, "ready");
    let context = find_world(&mut client);
    let found = collect(&mut client, context, 16.0);
    let field = type_label(&found, hint_on(&mut client, &found, "i1"));
    assert_eq!(field.kind, Kind::Edit, "a text input is typed into");
    click_hint(&mut client, &field);
    assert_eq!(
        evaluate(&mut client, "document.activeElement.id").as_str(),
        Some("i1")
    );
    // And the question asked after a click in normal mode says so.
    let focused = client
        .call("Runtime.evaluate", hints::focused_params())
        .expect("the page answers");
    assert_eq!(hints::focused_editable(&focused), Some(true));

    let checkbox = type_label(&found, hint_on(&mut client, &found, "i2"));
    assert_eq!(checkbox.kind, Kind::Click);
    click_hint(&mut client, &checkbox);
    assert_eq!(
        evaluate(&mut client, "document.getElementById('i2').checked").as_bool(),
        Some(true)
    );
    let focused = client
        .call("Runtime.evaluate", hints::focused_params())
        .expect("the page answers");
    assert_eq!(
        hints::focused_editable(&focused),
        Some(false),
        "a checkbox is not a field"
    );

    client.close();
    engine.kill();
}

#[test]
fn a_hint_inside_a_same_origin_frame_is_clicked_in_the_frame() {
    let Some((mut engine, mut client, _, context, url)) = hinting(HEIGHT * 2) else {
        return;
    };
    let found = collect(&mut client, context, 16.0);
    let index = found
        .hints
        .iter()
        .position(|hint| hint.href.ends_with("/inner-target"))
        .expect("the frame's link");
    let hint = type_label(&found, index);
    click_hint(&mut client, &hint);
    let inner = |client: &mut Client| {
        evaluate(
            client,
            "document.getElementById('same').contentWindow.location.href",
        )
        .as_str()
        .unwrap_or_default()
        .ends_with("/inner-target")
    };
    assert!(wait_until(&mut client, true, inner), "the frame navigated");
    assert_eq!(
        evaluate(&mut client, "location.href").as_str(),
        Some(url.as_str()),
        "and the page did not"
    );

    client.close();
    engine.kill();
}

/// `F`: the href goes to a tab this program opens behind the one in front,
/// the way `app::open_behind` opens one, and the engine's announcement of it
/// — which has no opener — is not taken for a second tab. And a ctrl+click
/// on a hint's point, for comparison, is the tab behind that #19 adopts.
#[test]
fn a_hint_opened_in_a_new_tab_is_a_target_this_program_made() {
    let Some((mut engine, mut client, target, context, _)) = hinting(HEIGHT) else {
        return;
    };
    let found = collect(&mut client, context, 16.0);
    let hint = type_label(&found, hint_on(&mut client, &found, "a1"));
    assert!(hint.href.ends_with("/one"));
    let (mut browser, mut tabs) = tabbed(&engine, client, target);

    let created = browser
        .call(
            "Target.createTarget",
            Json::object(vec![
                ("url", Json::string(&hint.href)),
                ("background", Json::Bool(true)),
            ]),
        )
        .expect("a new target");
    let opened = created
        .get("targetId")
        .and_then(Json::as_str)
        .expect("the engine says which")
        .to_string();
    let connection = browser
        .attach(&opened, Duration::from_secs(10))
        .expect("a session on the new page");
    let index = tabs.open_behind(Tab::new(opened, connection, &hint.href));
    assert_eq!((index, tabs.active_index()), (1, 0), "behind the first");
    assert!(!pump(
        &mut browser,
        &mut tabs,
        Duration::from_secs(2),
        |tabs| tabs.len() > 2
    ));
    assert_eq!(tabs.len(), 2, "its announcement is not a second tab");
    assert_eq!(tabs.active_index(), 0, "and does not bring it to the front");
    let tab = tabs.get_mut(1).expect("the new tab");
    let arrived = |client: &mut Client| {
        evaluate(client, "location.href")
            .as_str()
            .unwrap_or_default()
            .ends_with("/one")
    };
    assert!(
        wait_until(&mut tab.connection, true, arrived),
        "the new tab is at the href"
    );

    // Now the ctrl+click, on the other link, from the first tab.
    assert_eq!(tabs.active_index(), 0);
    let two = found
        .hints
        .iter()
        .find(|hint| hint.href.ends_with("/two"))
        .expect("the second link")
        .clone();
    let page = &mut tabs.active_mut().expect("the first tab").connection;
    for params in hints::click_params(two.at) {
        let Json::Object(mut fields) = params else {
            panic!("the params are an object");
        };
        for (name, value) in fields.iter_mut() {
            if name == "modifiers" {
                *value = Json::number(ctrl_click());
            }
        }
        page.call("Input.dispatchMouseEvent", Json::Object(fields))
            .expect("the ctrl+click is dispatched");
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut adopted = None;
    while adopted.is_none() && Instant::now() < deadline {
        for event in browser.events() {
            let created = event.method == "Target.targetCreated"
                && event
                    .params
                    .path(&["targetInfo", "type"])
                    .and_then(Json::as_str)
                    == Some("page");
            if !created {
                continue;
            }
            let id = event
                .params
                .path(&["targetInfo", "targetId"])
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string();
            if tabs.index_of(&id).is_some() {
                continue;
            }
            let opener = event.params.path(&["targetInfo", "openerId"]).is_some();
            let outcome = tabs.take(&event, |target| {
                browser.attach(target, Duration::from_secs(5))
            });
            adopted = Some((
                opener,
                matches!(outcome, Outcome::OpenedBehind { index: 2 }),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!("a ctrl+click's target: (has an opener, a tab behind) = {adopted:?}");
    assert_eq!(
        adopted,
        Some((false, true)),
        "a ctrl+click makes a target with no opener, which is a tab behind"
    );
    assert_eq!((tabs.len(), tabs.active_index()), (3, 0));

    browser.close();
    drop(tabs);
    engine.kill();
}

/// The scroll keys through the program's own wheel thread and dispatch, with
/// the distances [`normal::Scroll`] names: a notch, half a screen, and the two
/// ends, which the engine clamps.
#[test]
fn the_scroll_keys_move_the_page_through_the_wheel() {
    let Some((mut engine, mut client, target)) = connect_with_target() else {
        return;
    };
    a_page_to_scroll(&mut client);
    let _ = client.call("Page.stopScreencast", Json::empty());
    let viewport = blinkterm::zoom::Viewport::fit((WIDE, TALL), 1.0);
    let at = (WIDE as i32 / 2, TALL as i32 / 2);
    let wheel = Wheel::start();
    let wire = Arc::new(blinkterm::app::Wire::new(client.notifier()));
    let key = |client: &mut Client, scroll: normal::Scroll, wanted: f64| {
        let distance = match scroll {
            normal::Scroll::Notch(n) => viewport.notch((0, n), blinkterm::app::WHEEL_PIXELS),
            normal::Scroll::HalfPage(n) => (0.0, f64::from(n) * f64::from(viewport.css.1) / 2.0),
            normal::Scroll::End(n) => (0.0, f64::from(n) * normal::FAR),
        };
        wheel.notch(&target, wire.clone(), at, distance);
        let landed = wait_until(client, wanted, scroll_y);
        assert_eq!(landed, wanted, "{scroll:?}");
        // The page is where it was sent, but a curve for one of the ends
        // pays out the rest of its ten million pixels for the rest of
        // `scroll::D`, clamped by the engine; the next key waits it out, as
        // a person's would.
        let deadline = Instant::now() + Duration::from_secs(2);
        while wheel.owed() != (0.0, 0.0) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    key(&mut client, normal::Scroll::Notch(1), 120.0);
    key(
        &mut client,
        normal::Scroll::HalfPage(1),
        120.0 + f64::from(TALL) / 2.0,
    );
    let bottom = page_number(
        &mut client,
        "document.documentElement.scrollHeight - innerHeight",
    );
    key(&mut client, normal::Scroll::End(1), bottom);
    key(&mut client, normal::Scroll::Notch(-1), bottom - 120.0);
    key(&mut client, normal::Scroll::End(-1), 0.0);

    client.close();
    engine.kill();
}

/// A label is about a cell tall on the screen at every level, because it is
/// sized from the CSS pixels a cell is: at 50% the page's text is half the
/// height and the labels are not.
#[test]
fn labels_stay_a_cell_tall_at_every_zoom_level() {
    let Some((mut engine, mut client, _, context, _)) = hinting(HEIGHT) else {
        return;
    };
    for factor in [1.0, 0.5, 2.0] {
        zoom_to(&mut client, factor);
        wait_until(&mut client, factor, |client| page_metrics(client).2);
        let px = f64::from(CELL.1) / factor;
        let found = collect(&mut client, context, px);
        show(&mut client, context, &found, px);
        let yellow = label_pixels(&labelled_still(&mut client));
        let each = yellow / found.hints.len().max(1);
        eprintln!(
            "at {factor}: {} hints, {yellow} yellow pixels, {each} a label",
            found.hints.len()
        );
        assert!(!found.hints.is_empty(), "at {factor}");
        assert!(
            (100..=300).contains(&each),
            "{each} pixels a label at {factor}"
        );
        clear(&mut client, context);
    }

    client.close();
    engine.kill();
}

#[test]
fn hints_on_about_blank_and_the_error_page_are_none_and_no_exception() {
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
    assert!(collect(&mut client, context, 16.0).hints.is_empty());

    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string("http://127.0.0.1:1/"))]),
        )
        .expect("the navigation is answered");
    let deadline = Instant::now() + Duration::from_secs(10);
    let answer = loop {
        // As for find: until the error page's document has arrived, the world
        // asked for may be the one about to go.
        let context = find_world(&mut client);
        match client.call_within(
            "Runtime.callFunctionOn",
            hints::collect_params(context, 16.0),
            Duration::from_secs(5),
        ) {
            Ok(reply) => break Hints::from_reply(&reply, false),
            Err(why) if find::stale_world(&why) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(why) => panic!("{why}"),
        }
    };
    let error_page = answer.expect("the script's answer, not an exception");
    assert!(
        error_page.hints.is_empty(),
        "the error page: {:?}",
        error_page.hints
    );

    client.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// Crashed pages, the session, and dormant tabs (#18)
// ---------------------------------------------------------------------------

use blinkterm::fit::Metrics;
use blinkterm::session::{self, Session, Snapshot, State};

/// A page that paints every frame, so that a screencast of it is a count.
/// No `%` and no `#` in it: this is a url, and `#` would start its fragment.
const ANIMATED: &str = "data:text/html,<title>anim</title>\
<div id=b style='position:absolute;width:40px;height:40px;background:red'></div>\
<script>var n=0,b=document.getElementById('b');\
function f(){n=n>400?0:n+3;b.style.left=n+'px';requestAnimationFrame(f)}f()</script>";

/// How many screencast frames arrive in `window`, acknowledged as they come.
fn frames_in(client: &mut Client, window: Duration) -> usize {
    let deadline = Instant::now() + window;
    let mut count = 0;
    while Instant::now() < deadline {
        count += take_frames(client).len();
        std::thread::sleep(Duration::from_millis(10));
    }
    count
}

/// How long a renderer's death may take to reach the browser. On a
/// workstation it is about 20 ms; on the CI runner it took more than the
/// second these tests first allowed, probably because the kernel pipes the
/// dead renderer's core to the host's crash collector before the process is
/// gone. The program reads the event whenever it comes, so only the tests
/// wait on it, and they print how long it took.
const CRASH_NOTICE: Duration = Duration::from_secs(20);

/// The browser's events until one is `method` about `target`, or the time is
/// up; everything read on the way is handed to `seen` as well.
fn browser_event(
    browser: &mut Client,
    method: &str,
    target: &str,
    timeout: Duration,
    mut seen: impl FnMut(&blinkterm::cdp::Event),
) -> Option<blinkterm::cdp::Event> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for event in browser.events() {
            seen(&event);
            let about = event
                .params
                .get("targetId")
                .and_then(Json::as_str)
                .map(str::to_string);
            if event.method == method && about.as_deref() == Some(target) {
                return Some(event);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// The page session's events for `window`, by method.
fn page_events(client: &mut Client, window: Duration) -> Vec<blinkterm::cdp::Event> {
    let deadline = Instant::now() + window;
    let mut events = Vec::new();
    while Instant::now() < deadline {
        events.extend(client.events());
        std::thread::sleep(Duration::from_millis(10));
    }
    events
}

/// A renderer that dies leaves its tab, as a sad tab does in Chrome, and
/// `ctrl+r` brings the page back: at the size it was, and — once the
/// screencast is started again, which is `app::revive` — painting.
///
/// Not done here, and documented instead: sending
/// `Emulation.setDeviceMetricsOverride` to the crashed tab. Measured in the
/// #18 design, it takes the whole browser down with SIGSEGV every time,
/// which is why the program never does; proving it again on every run would
/// prove only that the engine still has the bug.
#[test]
fn a_crashed_page_keeps_its_tab_and_a_reload_brings_it_back_casting() {
    let Some((mut engine, mut page, target)) = connect_with_target() else {
        return;
    };
    page.call("Page.enable", Json::empty())
        .expect("Page.enable");
    let (mut browser, mut tabs) = tabbed(&engine, page, target.clone());
    browser
        .call(
            "Target.activateTarget",
            Json::object(vec![("targetId", Json::string(&target))]),
        )
        .expect("the tab in front");
    {
        let tab = tabs.active_mut().expect("the tab");
        viewport(&mut tab.connection);
        navigate_tab(tab, ANIMATED);
        follow(tab, Duration::from_secs(5));
        cast(&mut tab.connection, "jpeg", Some(85), WIDTH, HEIGHT);
        let before = frames_in(&mut tab.connection, Duration::from_secs(1));
        eprintln!("frames in a second before the crash: {before}");
        assert!(before >= 10, "{before} frames before the crash");
    }

    let _crash = tabs
        .active_mut()
        .expect("the tab")
        .connection
        .send("Page.crash", Json::empty())
        .expect("Page.crash sent");
    let asked = Instant::now();
    let crashed = browser_event(
        &mut browser,
        "Target.targetCrashed",
        &target,
        CRASH_NOTICE,
        |_| {},
    )
    .expect("Target.targetCrashed");
    eprintln!("Target.targetCrashed after {:?}", asked.elapsed());
    eprintln!("crashed: {}", crashed.params);
    assert_eq!(
        crashed.params.get("status").and_then(Json::as_str),
        Some("crashed")
    );
    assert!(
        crashed.params.get("errorCode").is_some(),
        "{}",
        crashed.params
    );
    assert!(matches!(
        tabs.take(&crashed, |_| Err("no new tab".to_string())),
        Outcome::Crashed { index: 0 }
    ));
    let tab = tabs.active_mut().expect("the tab stays");
    assert!(tab.is_crashed());
    assert_eq!(tab.line(), "this page crashed; ctrl+r reloads it");
    assert!(tab.connection.ended().is_none(), "the session goes on");

    let events = page_events(&mut tab.connection, Duration::from_millis(300));
    let inspector = events
        .iter()
        .find(|event| event.method == "Inspector.targetCrashed")
        .expect("the page session's own word of it, without Inspector.enable");
    assert_eq!(inspector.params, Json::empty());

    // What the program no longer asks a crashed page: it would wait.
    let held = tab
        .connection
        .send(
            "Runtime.evaluate",
            Json::object(vec![("expression", Json::string("1"))]),
        )
        .expect("sent");
    std::thread::sleep(Duration::from_millis(500));
    let answer = tab.connection.take_reply(&held);
    eprintln!("Runtime.evaluate on the crashed page after 500 ms: {answer:?}");
    assert!(answer.is_none(), "a dead renderer answered: {answer:?}");
    drop(held);
    let dead = frames_in(&mut tab.connection, Duration::from_secs(1));
    assert_eq!(dead, 0, "frames from a dead renderer");

    // What `ctrl+r` does.
    let started = Instant::now();
    tab.connection
        .call_within("Page.reload", Json::empty(), Duration::from_secs(1))
        .expect("Page.reload answers on a crashed page");
    eprintln!("Page.reload answered in {:?}", started.elapsed());
    follow(tab, Duration::from_secs(2));
    assert_eq!(tab.problem, None, "the landing brought it back");
    assert!(!tab.is_crashed());
    assert_eq!(tab.title, "anim");
    let width = evaluate(&mut tab.connection, "innerWidth");
    assert_eq!(
        width.as_f64(),
        Some(WIDTH as f64),
        "the size told before the crash is kept across the reload: {width}"
    );

    // Whether the screencast survives is not something to lean on either way. The
    // design's probe saw none until it was started again; here, with every
    // frame acknowledged as it came, the cast carried on across the reload
    // (61 frames in the second). An acknowledgement owed when the renderer
    // died is the likely difference. So it is printed, not asserted, and
    // `revive` starts the cast again whatever happened, which is idempotent.
    let unstarted = frames_in(&mut tab.connection, Duration::from_secs(1));
    eprintln!("frames in a second after the reload, the cast untouched: {unstarted}");
    blinkterm::app::revive(
        &mut tab.connection,
        &blinkterm::appearance::Appearance::new(
            blinkterm::appearance::Choice::Auto,
            false,
            blinkterm::appearance::Alpha::Off,
        ),
        blinkterm::zoom::Viewport::fit((WIDTH, HEIGHT), 1.0),
        Metrics {
            cols: WIDTH / CELL.0,
            rows: HEIGHT / CELL.1 + 1,
            cell: CELL,
        },
        motion::Cast::default(),
        &Identity::new(None, None, "C"),
        &Sites::none(),
        &mut Vec::new(),
    );
    let after = frames_in(&mut tab.connection, Duration::from_secs(1));
    eprintln!("frames in a second after revive: {after}");
    assert!(after >= 10, "{after} frames after revive");
    engine.check().expect("the engine lived through all of it");

    drop(tabs);
    browser.close();
    engine.kill();
}

/// `chrome://crash` is the same crash by navigation: the navigation's reply
/// is `net::ERR_ABORTED`, which is not a failure to report, and the crash
/// follows. What `alt+left` does on the crashed tab — the browser's history
/// and a step through it — answers and lands, and closing a crashed tab is
/// what closing any tab is.
#[test]
fn chrome_crash_is_the_same_crash_with_its_navigation_aborted() {
    let Some((mut engine, mut page, target)) = connect_with_target() else {
        return;
    };
    page.call("Page.enable", Json::empty())
        .expect("Page.enable");
    let (mut browser, mut tabs) = tabbed(&engine, page, target.clone());
    let tab = tabs.active_mut().expect("the tab");
    viewport(&mut tab.connection);
    navigate_tab(tab, "data:text/html,<title>one</title>one");
    follow(tab, Duration::from_secs(5));
    navigate_tab(tab, "data:text/html,<title>two</title>two");
    follow(tab, Duration::from_secs(5));

    let reply = navigate_tab(tab, "chrome://crash");
    eprintln!("chrome://crash: {reply}");
    assert_eq!(
        reply.get("errorText").and_then(Json::as_str),
        Some("net::ERR_ABORTED")
    );
    assert_eq!(load::failed(&reply), None, "not a failure to report");

    let asked = Instant::now();
    let crashed = browser_event(
        &mut browser,
        "Target.targetCrashed",
        &target,
        CRASH_NOTICE,
        |_| {},
    )
    .expect("Target.targetCrashed");
    eprintln!("Target.targetCrashed after {:?}", asked.elapsed());
    assert!(
        crashed.params.get("errorCode").is_some(),
        "{}",
        crashed.params
    );
    assert!(matches!(
        tabs.take(&crashed, |_| Err("no new tab".to_string())),
        Outcome::Crashed { index: 0 }
    ));
    let tab = tabs.active_mut().expect("the tab stays");
    let events = page_events(&mut tab.connection, Duration::from_millis(300));
    assert!(
        events
            .iter()
            .any(|event| event.method == "Page.frameStoppedLoading"),
        "the aborted load stops"
    );
    assert!(tab.is_crashed());

    // Back: the browser's history answers on a crashed tab, and a step
    // through it is a new renderer landing.
    let started = Instant::now();
    let history = tab
        .connection
        .call_within(
            "Page.getNavigationHistory",
            Json::empty(),
            Duration::from_secs(1),
        )
        .expect("the history, from the browser side");
    let index = history
        .get("currentIndex")
        .and_then(Json::as_i64)
        .expect("an index");
    let entries = history
        .get("entries")
        .and_then(Json::as_array)
        .expect("entries");
    assert!(index >= 1, "somewhere to go back to: {history}");
    let id = entries[index as usize - 1]
        .get("id")
        .and_then(Json::as_i64)
        .expect("an entry id");
    tab.connection
        .call_within(
            "Page.navigateToHistoryEntry",
            Json::object(vec![("entryId", Json::number(id as f64))]),
            Duration::from_secs(1),
        )
        .expect("the step answers on a crashed page");
    eprintln!("history and the step in {:?}", started.elapsed());
    follow(tab, Duration::from_secs(5));
    assert_eq!(tab.problem, None, "the entry landed");
    assert!(!tab.is_crashed());

    // Crash once more, and close it the way `ctrl+w` does.
    let _crash = tab
        .connection
        .send("Page.crash", Json::empty())
        .expect("Page.crash sent");
    browser_event(
        &mut browser,
        "Target.targetCrashed",
        &target,
        Duration::from_secs(1),
        |_| {},
    )
    .expect("crashed again");
    let started = Instant::now();
    let closed = browser
        .call_within(
            "Target.closeTarget",
            Json::object(vec![("targetId", Json::string(&target))]),
            Duration::from_secs(1),
        )
        .expect("closeTarget answers on a crashed tab");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(closed.get("success").and_then(Json::as_bool), Some(true));
    let tab = tabs.active_mut().expect("the tab, until the list hears");
    let deadline = Instant::now() + Duration::from_secs(1);
    while tab.connection.ended().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let ended = tab.connection.ended().expect("the session ends");
    assert!(ended.contains("detached"), "{ended}");

    drop(tabs);
    browser.close();
    engine.kill();
}

/// The engine dying outright ends every session on the pipe at once, and the
/// session file it leaves says the run did not quit — which is what the next
/// start reads to offer the tabs back.
#[test]
fn an_engine_that_dies_ends_every_session_at_once_and_leaves_the_session_file_open() {
    let root = temp_dir("session-died");
    let dir = root.join("profile");
    let profile = Profile::take(Choice::At(dir.clone())).expect("a kept profile");
    let Some((mut engine, page)) = connect_in(profile) else {
        let _ = std::fs::remove_dir_all(&root);
        return;
    };
    let browser = engine.browser().expect("the browser's client");

    let mut kept = Session::load(&dir);
    let two = Snapshot {
        tabs: vec![
            session::Entry {
                url: "https://a.example/".to_string(),
                title: "A".to_string(),
            },
            session::Entry {
                url: "https://b.example/".to_string(),
                title: String::new(),
            },
        ],
        active: 1,
    };
    let now = Instant::now();
    kept.record(two.clone(), now);
    kept.flush(now).expect("written");

    let group = engine.group().expect("a group of its own");
    let killed = std::process::Command::new("kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .status()
        .expect("kill runs");
    assert!(killed.success());
    let deadline = Instant::now() + Duration::from_millis(500);
    while (browser.ended().is_none() || page.ended().is_none()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let ended = browser.ended().expect("the browser's client ends");
    assert!(ended.contains("closed its end"), "{ended}");
    let ended = page.ended().expect("the page's session ends");
    assert!(ended.contains("closed its end"), "{ended}");
    let deadline = Instant::now() + Duration::from_secs(1);
    while engine.check().is_ok() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let why = engine.check().unwrap_err();
    eprintln!("{why}");

    let saved = Session::load(&dir).saved().cloned().expect("a session");
    assert_eq!(saved.state, State::Open, "the run did not quit");
    assert_eq!(saved.snapshot, two);
    kept.finish(false);
    assert_eq!(
        Session::load(&dir).saved().map(|saved| saved.state),
        Some(State::Open)
    );
    kept.finish(true);
    assert_eq!(
        Session::load(&dir).saved().map(|saved| saved.state),
        Some(State::Closed)
    );

    drop(page);
    drop(browser);
    drop(engine);
    let _ = std::fs::remove_dir_all(&root);
}

/// A restored tab is a blank target wearing the saved url and title: the
/// engine's own news about the blank page does not change them, nothing is
/// loaded until it is asked for, and once asked for it lands like any page.
#[test]
fn a_dormant_tab_is_a_blank_target_that_keeps_its_name_and_loads_when_asked() {
    let Some((mut engine, mut page, target)) = connect_with_target() else {
        return;
    };
    page.call("Page.enable", Json::empty())
        .expect("Page.enable");
    let (mut browser, mut tabs) = tabbed(&engine, page, target);
    let saved = session::Entry {
        url: "data:text/html,<title>saved</title>saved".to_string(),
        title: "Saved".to_string(),
    };

    // As `open_dormant` does it.
    let created = browser
        .call(
            "Target.createTarget",
            Json::object(vec![("url", Json::string("about:blank"))]),
        )
        .expect("a target");
    let id = created
        .get("targetId")
        .and_then(Json::as_str)
        .expect("its id")
        .to_string();
    let mut connection = browser
        .attach(&id, Duration::from_secs(5))
        .expect("a session on it");
    connection
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    let mut tab = Tab::new(id.clone(), connection, "about:blank");
    tab.url = saved.url.clone();
    tab.title = saved.title.clone();
    tab.dormant = true;
    tabs.open(tab);

    let deadline = Instant::now() + Duration::from_millis(300);
    let mut about_it = 0;
    let mut started = 0;
    while Instant::now() < deadline {
        for event in browser.events() {
            let ours = event
                .params
                .path(&["targetInfo", "targetId"])
                .and_then(Json::as_str)
                == Some(id.as_str());
            let outcome = tabs.take(&event, |_| Err("no new tab".to_string()));
            if ours {
                about_it += 1;
                assert!(
                    matches!(outcome, Outcome::Ignored),
                    "{}: {}",
                    event.method,
                    event.params
                );
            }
        }
        let tab = tabs.active_mut().expect("the dormant tab");
        started += tab
            .connection
            .events()
            .iter()
            .filter(|event| event.method == "Page.frameStartedNavigating")
            .count();
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!("{about_it} events about the blank target while it slept");
    let tab = tabs.active_mut().expect("the dormant tab");
    assert_eq!(tab.url, saved.url, "the saved url stands");
    assert_eq!(tab.title, saved.title, "and the saved title");
    assert_eq!(started, 0, "nothing was loaded");

    // Woken: the flag first, then the page.
    tab.dormant = false;
    navigate_tab(tab, &saved.url);
    let landings = follow(tab, Duration::from_secs(2));
    assert!(
        matches!(landings.as_slice(), [Landing::Document(_)]),
        "{landings:?}"
    );
    assert_eq!(tab.title, "saved");
    assert!(!tab.dormant);

    drop(tabs);
    browser.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// The engine started again in place when it dies (#18)
// ---------------------------------------------------------------------------

use blinkterm::app::Booted;

/// Whether an engine is named for these tests; says why not when it is not.
/// For the tests that start their engine with `app::boot`, as the program
/// does, rather than through [`connect_in_with_target`].
fn engine_named() -> bool {
    if std::env::var_os(engine::ENGINE_ENV).is_none() {
        eprintln!(
            "skipped: {} is not set; name a Chromium to run this against",
            engine::ENGINE_ENV
        );
        return false;
    }
    match engine::locate() {
        Ok(path) => {
            eprintln!("engine: {}", path.display());
            true
        }
        Err(why) => {
            eprintln!("skipped: {why}");
            false
        }
    }
}

/// How many page targets the engine has.
fn page_targets(browser: &mut Client) -> usize {
    let reply = browser
        .call("Target.getTargets", Json::empty())
        .expect("the targets");
    reply
        .get("targetInfos")
        .and_then(Json::as_array)
        .map(|infos| {
            infos
                .iter()
                .filter(|info| info.get("type").and_then(Json::as_str) == Some("page"))
                .count()
        })
        .unwrap_or(0)
}

/// Every step `app::relaunch` takes, in its order, through the public
/// pieces: an engine with three tabs killed from outside, its clients
/// dropped, the engine retired with the profile's lock still held, a second
/// engine booted on the same profile, the live list's tabs restored on it
/// dormant, and the one in front woken as `activate` wakes it.
#[test]
fn an_engine_killed_under_a_session_is_started_again_on_its_profile_with_the_tabs_back() {
    if !engine_named() {
        return;
    }
    let root = temp_dir("relaunch");
    let dir = root.join("profile");
    let downloads = root.join("downloads");
    std::fs::create_dir_all(&downloads).expect("a download directory");
    let appearance = blinkterm::appearance::Appearance::new(
        blinkterm::appearance::Choice::Auto,
        false,
        blinkterm::appearance::Alpha::Off,
    );
    let launch = engine::Launch::default();
    let profile = Profile::take(Choice::At(dir.clone())).expect("a kept profile");
    let Booted {
        engine,
        mut browser,
        mut tabs,
        identity,
    } = blinkterm::app::boot(
        profile,
        &launch,
        &downloads,
        &appearance,
        &Allowed::in_memory(),
        None,
        &Sites::none(),
        None,
    )
    .expect("the engine boots");
    let base = serve();

    // Three tabs, as the program would have them once each had landed: the
    // first in front, two opened behind it.
    {
        let tab = tabs.active_mut().expect("the first tab");
        tab.connection
            .call(
                "Page.navigate",
                Json::object(vec![("url", Json::string(&base))]),
            )
            .expect("the first page");
        assert_eq!(
            wait_for_title(&mut tab.connection, "first", Duration::from_secs(10)),
            "first"
        );
        tab.url = base.clone();
        tab.title = "first".to_string();
    }
    for name in ["second", "plain"] {
        let index = blinkterm::app::open_behind(
            &mut tabs,
            &mut browser,
            &appearance,
            &identity,
            &Sites::none(),
            &format!("{base}{name}"),
        )
        .expect("a tab behind");
        let tab = tabs.get_mut(index).expect("the tab behind");
        assert_eq!(
            wait_for_title(&mut tab.connection, name, Duration::from_secs(10)),
            name
        );
        tab.title = name.to_string();
    }
    let snapshot = Snapshot::of(&tabs);
    assert_eq!(snapshot.tabs.len(), 3);
    assert_eq!(snapshot.active, 0);

    let group = engine.group().expect("a group of its own");
    let killed = std::process::Command::new("kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .status()
        .expect("kill runs");
    assert!(killed.success());
    let deadline = Instant::now() + CRASH_NOTICE;
    while browser.ended().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        browser.ended().is_some(),
        "the pipe says the engine is gone"
    );

    // What `relaunch` does with the dead engine: its clients go first,
    // sending nothing, and the profile comes out of it still locked.
    drop(tabs);
    drop(browser);
    let retired = Instant::now();
    let profile = engine.retire();
    eprintln!("retired in {:?}", retired.elapsed());
    match Profile::take(Choice::At(dir.clone())) {
        Ok(_) => panic!("the profile's lock was let go between the two engines"),
        Err(why) => assert!(
            why.contains(&std::process::id().to_string()),
            "refused, as held by this process: {why}"
        ),
    }

    let started = Instant::now();
    let Booted {
        mut engine,
        mut browser,
        mut tabs,
        identity,
    } = blinkterm::app::boot(
        profile,
        &launch,
        &downloads,
        &appearance,
        &Allowed::in_memory(),
        None,
        &Sites::none(),
        None,
    )
    .expect("a second engine on the same profile");
    blinkterm::app::restore_tabs(
        &mut tabs,
        &mut browser,
        &appearance,
        &identity,
        &Sites::none(),
        snapshot.clone(),
    );
    let took = started.elapsed();
    eprintln!("second engine up with the tabs back in {took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert_eq!(
        tabs.len(),
        3,
        "the blank tab was adopted, not left before them"
    );
    assert_eq!(
        tabs.iter()
            .map(|tab| tab.title.as_str())
            .collect::<Vec<_>>(),
        ["first", "second", "plain"]
    );
    assert_eq!(
        Snapshot::of(&tabs),
        snapshot,
        "the same tabs, in the same order"
    );
    assert_eq!(tabs.active_index(), 0);
    assert!(tabs.iter().all(|tab| tab.dormant));
    assert_eq!(page_targets(&mut browser), 3);
    assert!(engine.check().is_ok());

    // What `activate` and `wake_dormant` do to the tab in front.
    let target = tabs.active_target().expect("a tab in front").to_string();
    browser
        .call(
            "Target.activateTarget",
            Json::object(vec![("targetId", Json::string(&target))]),
        )
        .expect("raised");
    let tab = tabs.active_mut().expect("the tab in front");
    viewport(&mut tab.connection);
    tab.dormant = false;
    let url = tab.url.clone();
    tab.connection
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(&url))]),
        )
        .expect("the page again");
    assert_eq!(
        wait_for_title(&mut tab.connection, "first", Duration::from_secs(10)),
        "first"
    );
    cast(&mut tab.connection, "png", None, WIDTH, HEIGHT);
    assert!(
        wait_for_frame(&mut tab.connection, Duration::from_secs(10)).is_some(),
        "the page is painting on the new engine"
    );
    assert!(
        Profile::take(Choice::At(dir.clone())).is_err(),
        "the new engine holds the profile"
    );

    drop(tabs);
    browser.close();
    engine.kill();
    let _ = std::fs::remove_dir_all(&root);
}

/// `Engine::retire` on an engine that is still running, which is the path a
/// relaunch takes when the death was the pipe's rather than the process's:
/// the group is stopped, and the profile is handed back still locked and
/// ready for a second engine.
#[test]
fn an_engine_retired_alive_is_stopped_and_its_profile_is_free_to_start_another() {
    let root = temp_dir("retire");
    let dir = root.join("profile");
    let profile = Profile::take(Choice::At(dir.clone())).expect("a kept profile");
    let Some((engine, page)) = connect_in(profile) else {
        let _ = std::fs::remove_dir_all(&root);
        return;
    };
    let group = engine.group().expect("a group of its own");
    drop(page);
    let profile = engine.retire();

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut left = group_members(group);
    while !left.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        left = group_members(group);
    }
    assert!(
        left.is_empty(),
        "retired and these are still running: {}",
        describe(&left)
    );
    assert!(
        Profile::take(Choice::At(dir.clone())).is_err(),
        "the lock is still this program's"
    );
    assert!(dir.exists(), "a kept profile is not removed by a retire");

    let mut engine =
        Engine::launch(profile, Duration::from_secs(30)).expect("a second engine starts on it");
    assert!(engine.check().is_ok());
    engine.kill();
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Sound, permissions and fullscreen (#20)
// ---------------------------------------------------------------------------

use blinkterm::fullscreen::{self, Heard};
use blinkterm::permissions::{self, Allowed, Permission};

/// A page with a button that takes a box fullscreen, or lets it go.
const FULLSCREEN_PAGE: &str = "<!doctype html><title>fs</title>\
    <body style='margin:0;background:#fff'>\
    <div id=box style='width:200px;height:100px;background:#c33'></div>\
    <button id=go style='position:absolute;left:0;top:200px;width:100px;height:40px'>go</button>\
    <script>document.getElementById('go').onclick = function () {\
    if (document.fullscreenElement) document.exitFullscreen();\
    else document.getElementById('box').requestFullscreen(); };</script>";

/// A page with an `<audio>` of [`tone`], not playing.
const AUDIO_PAGE: &str = "<!doctype html><title>audio</title>\
    <body style='margin:0'><audio id=a src='/tone.wav'></audio>";

/// Two seconds of a 440 Hz tone, 8 kHz mono 16-bit PCM, as a WAV: a few
/// lines of packing rather than a file in the repository.
fn tone() -> Vec<u8> {
    let rate: u32 = 8000;
    let samples: Vec<i16> = (0..rate * 2)
        .map(|n| {
            let t = n as f64 / rate as f64;
            ((t * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16
        })
        .collect();
    let data = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes()); // bytes a second
    wav.extend_from_slice(&2u16.to_le_bytes()); // bytes a frame
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits a sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

/// Serve [`FULLSCREEN_PAGE`] at `/fs`, [`AUDIO_PAGE`] at `/audio`, the tone
/// at `/tone.wav` and [`PLAIN_PAGE`] for anything else, on a loopback port:
/// an origin, which a `data:` page is not and the engine grants nothing to.
/// The origin comes back without a trailing slash, as
/// [`permissions::origin_of`] writes one.
fn serve_media() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to serve on");
    let address = listener.local_addr().expect("an address");
    let wav = tone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut head = [0u8; 2048];
            let read = stream.read(&mut head).unwrap_or(0);
            let request = String::from_utf8_lossy(&head[..read]).to_string();
            let (kind, body): (&str, &[u8]) = if request.starts_with("GET /fs") {
                ("text/html", FULLSCREEN_PAGE.as_bytes())
            } else if request.starts_with("GET /audio") {
                ("text/html", AUDIO_PAGE.as_bytes())
            } else if request.starts_with("GET /tone.wav") {
                ("audio/wav", &wav)
            } else {
                ("text/html", PLAIN_PAGE.as_bytes())
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    format!("http://{address}")
}

/// Evaluate an expression whose value is a promise, and hand back what it
/// resolved to.
fn evaluate_awaited(client: &mut Client, expression: &str) -> Json {
    client
        .call_within(
            "Runtime.evaluate",
            Json::object(vec![
                ("expression", Json::string(expression)),
                ("awaitPromise", Json::Bool(true)),
                ("returnByValue", Json::Bool(true)),
            ]),
            CRASH_NOTICE,
        )
        .ok()
        .and_then(|reply| reply.path(&["result", "value"]).cloned())
        .unwrap_or(Json::Null)
}

/// What `navigator.permissions.query` says for each name, as `name=state`
/// words: the state a site reads before deciding whether to ask.
fn permission_states(client: &mut Client, names: &[&str]) -> String {
    let names: Vec<String> = names.iter().map(|name| format!("'{name}'")).collect();
    let expression = format!(
        "Promise.all([{}].map(function (n) {{ return navigator.permissions.query({{name: n}})\
         .then(function (s) {{ return n + '=' + s.state; }}, \
         function (e) {{ return n + '=!' + e.name; }}); }}))\
         .then(function (a) {{ return a.join(' '); }})",
        names.join(",")
    );
    match evaluate_awaited(client, &expression) {
        Json::String(states) => states,
        other => panic!("no answer from permissions.query: {other:?}"),
    }
}

/// Navigate a page's session and wait for the title it should land with.
fn land_on(client: &mut Client, url: &str, title: &str) {
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(url))]),
        )
        .expect("the page loads");
    assert_eq!(wait_for_title(client, title, CRASH_NOTICE), title, "{url}");
}

/// Ask until `states` answers `wanted` or [`CRASH_NOTICE`] is up, and hand
/// back the last answer: a notification on the browser's connection is in
/// force for the next page command, but a test that waits costs nothing.
fn states_become(client: &mut Client, names: &[&str], wanted: &str) -> String {
    let deadline = Instant::now() + CRASH_NOTICE;
    loop {
        let states = permission_states(client, names);
        if states == wanted || Instant::now() >= deadline {
            return states;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Boot as the program does, with the browser-wide deny and one origin
/// allowed the camera, and read what pages are told: `denied` for every name
/// the program sets, `granted` for what the person allowed, the same in a
/// second tab on that origin, `denied` on a `data:` page; then the allowance
/// taken back by the allow line's commands, and `denied` again.
#[test]
fn a_fresh_engine_says_denied_to_every_page_and_granted_to_an_origin_the_person_allowed() {
    if !engine_named() {
        return;
    }
    let origin = serve_media();
    let root = temp_dir("permissions");
    let downloads = root.join("downloads");
    std::fs::create_dir_all(&downloads).expect("a download directory");
    let appearance = blinkterm::appearance::Appearance::new(
        blinkterm::appearance::Choice::Auto,
        false,
        blinkterm::appearance::Alpha::Off,
    );
    let mut allowed = Allowed::in_memory();
    allowed
        .set(&origin, &[Permission::Camera])
        .expect("kept in memory");
    assert_eq!(
        permissions::origin_of(&format!("{origin}/plain")).as_deref(),
        Some(origin.as_str())
    );
    let Booted {
        engine: _engine,
        mut browser,
        mut tabs,
        identity: _identity,
    } = blinkterm::app::boot(
        Profile::temporary().expect("a temporary profile"),
        &engine::Launch::default(),
        &downloads,
        &appearance,
        &allowed,
        None,
        &Sites::none(),
        None,
    )
    .expect("the engine boots");
    let names = [
        "camera",
        "microphone",
        "geolocation",
        "notifications",
        "clipboard-read",
        "clipboard-write",
    ];
    let wanted = "camera=granted microphone=denied geolocation=denied notifications=denied \
                  clipboard-read=denied clipboard-write=denied";
    {
        let tab = tabs.active_mut().expect("the first tab");
        land_on(&mut tab.connection, &format!("{origin}/plain"), "plain");
        assert_eq!(permission_states(&mut tab.connection, &names), wanted);
        // What the engine answers when nobody set anything is `default`;
        // with the deny it is `denied`, at once.
        assert_eq!(
            evaluate_awaited(&mut tab.connection, "Notification.requestPermission()"),
            Json::string("denied")
        );
    }
    let behind = blinkterm::app::open_behind(
        &mut tabs,
        &mut browser,
        &appearance,
        &Identity::new(None, None, "C"),
        &Sites::none(),
        &format!("{origin}/fs"),
    )
    .expect("a tab behind");
    {
        let tab = tabs.get_mut(behind).expect("the tab behind");
        assert_eq!(
            wait_for_title(&mut tab.connection, "fs", CRASH_NOTICE),
            "fs"
        );
        assert_eq!(
            permission_states(&mut tab.connection, &names),
            wanted,
            "a second tab on the origin"
        );
    }
    let tab = tabs.active_mut().expect("the first tab");
    land_on(
        &mut tab.connection,
        "data:text/html,<title>opaque</title>",
        "opaque",
    );
    assert_eq!(
        permission_states(&mut tab.connection, &["camera"]),
        "camera=denied",
        "an opaque origin is denied browser-wide too"
    );
    land_on(&mut tab.connection, &format!("{origin}/plain"), "plain");
    // The allow line with nothing on it: every name denied by origin, which
    // is what takes a grant back.
    for (method, params) in permissions::origin_commands(&origin, &[]) {
        browser.notify(method, params).expect("told");
    }
    assert_eq!(
        states_become(&mut tab.connection, &["camera"], "camera=denied"),
        "camera=denied"
    );
    // And a word given again is granted again, by origin.
    for (method, params) in permissions::origin_commands(&origin, &[Permission::Clipboard]) {
        browser.notify(method, params).expect("told");
    }
    let wanted = "clipboard-read=granted clipboard-write=granted camera=denied";
    assert_eq!(
        states_become(
            &mut tab.connection,
            &["clipboard-read", "clipboard-write", "camera"],
            wanted
        ),
        wanted
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The isolated world a watch is armed in, made as `app` makes it.
fn fullscreen_world(client: &mut Client) -> i64 {
    let tree = client
        .call("Page.getFrameTree", Json::empty())
        .expect("the frame tree");
    let frame = find::main_frame(&tree).expect("a main frame");
    let world = client
        .call("Page.createIsolatedWorld", find::world_params(&frame))
        .expect("a world");
    find::context(&world).expect("the world's id")
}

/// Arm a watch as `pump_fullscreen` does: sent, never called.
fn arm(client: &mut Client, world: i64, expected: bool) -> Pending {
    client
        .send(
            "Runtime.evaluate",
            fullscreen::watch_params(world, expected),
        )
        .expect("the watch is sent")
}

/// The watch's answer, waited for up to [`CRASH_NOTICE`] — the CI runner has
/// taken seconds to deliver what a workstation delivers in milliseconds —
/// with how long it took printed.
fn heard_within(client: &mut Client, pending: &Pending, what: &str) -> Option<Heard> {
    let started = Instant::now();
    while started.elapsed() < CRASH_NOTICE {
        if let Some(reply) = client.take_reply(pending) {
            eprintln!("{what}: heard in {:?}", started.elapsed());
            return Some(fullscreen::heard(&reply));
        }
        // What the page says meanwhile is nothing to this test.
        let _ = client.events();
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

/// The watch hears a click take an element fullscreen and `esc`'s exit let
/// it go; a watch armed after the fact catches up at once; a watch left
/// behind by a tab switch does not starve the next one; and a navigation
/// answers `Gone`.
#[test]
fn a_page_going_fullscreen_is_heard_and_esc_s_exit_is_heard_and_a_navigation_says_gone() {
    let Some((_engine, mut client)) = connect() else {
        return;
    };
    let origin = serve_media();
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    land_on(&mut client, &format!("{origin}/fs"), "fs");
    let world = fullscreen_world(&mut client);
    let fullscreen = |client: &mut Client| evaluate(client, "!!document.fullscreenElement");

    // Nothing happens, so nothing is answered.
    let watch = arm(&mut client, world, false);
    std::thread::sleep(Duration::from_millis(300));
    assert!(client.take_reply(&watch).is_none(), "a watch waits");
    // A click on the button is a gesture, and the page goes fullscreen.
    click_with(&mut client, (50, 220), "left", 1, 0);
    assert_eq!(heard_within(&mut client, &watch, "in"), Some(Heard::In));
    assert_eq!(fullscreen(&mut client), Json::Bool(true));

    // `esc`: the exit, in the watch's world, as a notification.
    let watch = arm(&mut client, world, true);
    client
        .notify("Runtime.evaluate", fullscreen::exit_params(Some(world)))
        .expect("the exit is sent");
    assert_eq!(heard_within(&mut client, &watch, "out"), Some(Heard::Out));
    assert_eq!(fullscreen(&mut client), Json::Bool(false));

    // Armed expecting fullscreen on a page that is not: it catches up at
    // once, which is how a tab coming to the front is caught up.
    let watch = arm(&mut client, world, true);
    assert_eq!(
        heard_within(&mut client, &watch, "catch-up"),
        Some(Heard::Out)
    );

    // A watch let go of — its tab went behind — is still waiting in the page;
    // the next one armed settles it, and is the one answered.
    let left = arm(&mut client, world, false);
    std::thread::sleep(Duration::from_millis(100));
    drop(left);
    let watch = arm(&mut client, world, false);
    click_with(&mut client, (50, 220), "left", 1, 0);
    assert_eq!(
        heard_within(&mut client, &watch, "after a stale watch"),
        Some(Heard::In)
    );

    // A navigation takes the document, and the watch hears that.
    let watch = arm(&mut client, world, true);
    let navigation = client
        .send(
            "Page.navigate",
            Json::object(vec![("url", Json::string(format!("{origin}/plain")))]),
        )
        .expect("the navigation is sent");
    assert_eq!(heard_within(&mut client, &watch, "gone"), Some(Heard::Gone));
    drop(navigation);
    assert_eq!(wait_for_title(&mut client, "plain", CRASH_NOTICE), "plain");
    assert_eq!(fullscreen(&mut client), Json::Bool(false), "and nothing is");
}

/// Chrome's autoplay rule, and nothing of this program's in its way: a
/// `play()` before any gesture is refused, a dispatched click is the
/// gesture, and after it the audio plays — its clock runs — with the engine
/// muted or not. This does not test a speaker; it tests that a page is not
/// silent for a reason this program caused.
#[test]
fn a_click_is_the_gesture_a_video_needs_and_the_engine_starts_its_audio_service() {
    let origin = serve_media();
    for mute in [false, true] {
        let launch = engine::Launch {
            mute,
            ..engine::Launch::default()
        };
        let Some((_engine, _browser, mut page)) = launched(&launch) else {
            return;
        };
        page.call("Page.enable", Json::empty())
            .expect("Page.enable");
        viewport(&mut page);
        land_on(&mut page, &format!("{origin}/audio"), "audio");
        let play = "document.getElementById('a').play()\
                    .then(function () { return 'played'; }, function (e) { return e.name; })";
        assert_eq!(
            evaluate_awaited(&mut page, play),
            Json::string("NotAllowedError"),
            "mute {mute}: no gesture, no sound"
        );
        click_with(&mut page, (5, 5), "left", 1, 0);
        assert_eq!(
            evaluate_awaited(&mut page, play),
            Json::string("played"),
            "mute {mute}: a click is the gesture"
        );
        let started = Instant::now();
        let mut time = 0.0;
        while started.elapsed() < CRASH_NOTICE {
            time = evaluate(&mut page, "document.getElementById('a').currentTime")
                .as_f64()
                .unwrap_or(0.0);
            if time > 0.0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!(
            "mute {mute}: currentTime {time} after {:?}",
            started.elapsed()
        );
        assert!(time > 0.0, "mute {mute}: the audio's clock runs");
    }
}

/// How long a paced test waits for a frame the engine owes it. Locally the
/// next frame after an acknowledgement is about 60 ms away; in docker on a
/// GitHub runner an engine event has taken seconds.
const FRAME_WAIT: Duration = Duration::from_secs(15);

/// The route the program takes over ssh: the engine's PNG, inline, at the
/// cursor.
fn png_route(placement: Placement, wrap: Wrap) -> Route {
    Route {
        transport: blinkterm::graphics::Transport::Inline,
        payload: Payload::Png,
        placement,
        wrap,
        every_nth: 1,
    }
}

/// The screencast frames in `client`'s queue, *not* acknowledged, with the
/// number each would be acknowledged with and its capture time.
fn unacked_frames(client: &mut Client) -> Vec<(i64, Vec<u8>, f64)> {
    let mut frames = Vec::new();
    for event in client.events() {
        if event.method != "Page.screencastFrame" {
            continue;
        }
        let session = event.params.get("sessionId").and_then(Json::as_i64);
        let data = event.params.get("data").and_then(Json::as_str);
        let stamp = event
            .params
            .path(&["metadata", "timestamp"])
            .and_then(Json::as_f64);
        if let (Some(session), Some(data), Some(stamp)) = (session, data, stamp) {
            let png = blinkterm::base64::decode(data.as_bytes()).expect("base64");
            frames.push((session, png, stamp));
        }
    }
    frames
}

/// A PNG cast with `every_nth`, at the page's size.
fn png_cast(client: &mut Client, every_nth: u32) {
    client
        .call(
            "Page.startScreencast",
            Json::object(vec![
                ("format", Json::string("png")),
                ("maxWidth", Json::number(WIDTH)),
                ("maxHeight", Json::number(HEIGHT)),
                ("everyNthFrame", Json::number(every_nth)),
            ]),
        )
        .expect("the screencast starts");
}

/// tmux's passthrough, as a model: each `ESC P tmux;` … `ESC \` loses its
/// wrapper and has its doubled escapes halved. The unit tests in
/// `src/graphics.rs` hold it to what tmux 3.4 was recorded emitting.
fn tmux_unwrap(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let Some(body) = rest.strip_prefix(b"\x1bPtmux;") else {
            out.push(rest[0]);
            rest = &rest[1..];
            continue;
        };
        let mut i = 0;
        loop {
            match (body.get(i), body.get(i + 1)) {
                (Some(0x1b), Some(0x1b)) => {
                    out.push(0x1b);
                    i += 2;
                }
                (Some(0x1b), Some(b'\\')) => {
                    i += 2;
                    break;
                }
                (Some(&byte), _) => {
                    out.push(byte);
                    i += 1;
                }
                (None, _) => panic!("an unterminated wrapper"),
            }
        }
        rest = &body[i..];
    }
    out
}

/// The route over ssh, end to end: the engine's PNG frames and its PNG still
/// go to the terminal as they came, and the terminal makes the picture of
/// them. Nothing is decoded on this side at all.
#[test]
fn a_png_cast_reaches_the_terminal_as_pngs_it_can_decode() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);
    let dir = temp_dir("png");
    let mut painter = Painter::at_with(&dir, png_route(Placement::Direct, Wrap::None));
    assert_eq!(painter.transport(), blinkterm::graphics::Transport::Inline);
    let mut terminal = a_terminal(&dir);
    let cells = Cells {
        cols: WIDTH / CELL.0,
        rows: HEIGHT / CELL.1,
    };

    png_cast(&mut client, 1);
    let started = Instant::now();
    let (mut frames, mut bytes) = (0usize, 0usize);
    while started.elapsed() < Duration::from_secs(2)
        || (frames < 3 && started.elapsed() < FRAME_WAIT)
    {
        for (png, _) in take_frames(&mut client) {
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "the engine promised PNG");
            let sequence = painter.png_frame(&png, cells, 2, 1);
            terminal.advance(&sequence);
            frames += 1;
            bytes += sequence.len();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = client.call("Page.stopScreencast", Json::empty());
    assert!(frames >= 3, "only {frames} frames");
    eprintln!(
        "png: {frames} frames in {:?}, {} bytes down the pane per frame",
        started.elapsed(),
        bytes / frames
    );
    let store = terminal.graphics();
    assert_eq!(store.placements().count(), 1);
    assert_eq!(
        store.image(IMAGE_ID).map(|i| (i.width, i.height)),
        Some((WIDTH, HEIGHT)),
        "the terminal read the size out of the file"
    );

    // The still goes the same way.
    let reply = client
        .call_within(
            "Page.captureScreenshot",
            Json::object(vec![("format", Json::string("png"))]),
            FRAME_WAIT,
        )
        .expect("a still");
    let png = blinkterm::base64::decode(
        reply
            .get("data")
            .and_then(Json::as_str)
            .expect("data")
            .as_bytes(),
    )
    .expect("base64");
    terminal.advance(&painter.png_frame(&png, cells, 2, 1));
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

/// What the pacing in `app::tick_frames` rests on: with no acknowledgement
/// the engine casts a few frames and then waits, and one acknowledgement
/// brings a frame of the page as it is then — not one that was queued.
#[test]
fn withholding_the_ack_holds_the_engine_to_three_frames_and_one_ack_brings_a_current_one() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);
    png_cast(&mut client, 1);

    let mut held = Vec::new();
    let deadline = Instant::now() + FRAME_WAIT;
    while held.is_empty() && Instant::now() < deadline {
        held.extend(unacked_frames(&mut client));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!held.is_empty(), "no frame at all");
    // Two seconds more with nothing acknowledged: the engine stops.
    let quiet = Instant::now();
    while quiet.elapsed() < Duration::from_secs(2) {
        held.extend(unacked_frames(&mut client));
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!("frames without an acknowledgement: {}", held.len());
    assert!(
        held.len() <= 3,
        "{} frames arrived unacknowledged on a page animating at 60 fps",
        held.len()
    );

    let session = held.last().expect("a frame held").0;
    let acked_at = motion::now_seconds();
    client
        .notify(
            "Page.screencastFrameAck",
            Json::object(vec![("sessionId", Json::number(session as f64))]),
        )
        .expect("sent");
    let mut next = Vec::new();
    let deadline = Instant::now() + FRAME_WAIT;
    while next.is_empty() && Instant::now() < deadline {
        next.extend(unacked_frames(&mut client));
        std::thread::sleep(Duration::from_millis(5));
    }
    let (_, png, stamp) = next.first().expect("a frame after the acknowledgement");
    assert_eq!(&png[..4], b"\x89PNG");
    eprintln!(
        "the frame after the acknowledgement was captured {:+.3} s from it",
        stamp - acked_at
    );
    assert!(
        *stamp >= acked_at - 0.05,
        "captured {:.3} s before the acknowledgement: a queued frame, not a current one",
        acked_at - stamp
    );

    // And a cast started again owes nothing: the throttle restarts it at a
    // new size with frames it will never acknowledge still counted against
    // the old one, and the new one must not wait for them.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let _ = unacked_frames(&mut client);
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = client.call("Page.stopScreencast", Json::empty());
    png_cast(&mut client, 1);
    let mut after = Vec::new();
    let deadline = Instant::now() + FRAME_WAIT;
    while after.is_empty() && Instant::now() < deadline {
        after.extend(unacked_frames(&mut client));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !after.is_empty(),
        "a restarted cast waited on the old one's acknowledgements"
    );
    let _ = client.call("Page.stopScreencast", Json::empty());
    client.close();
    engine.kill();
}

/// `everyNthFrame`, which is how a route's frame-rate cap reaches the engine.
#[test]
fn every_nth_frame_divides_the_cast() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);
    let count = |client: &mut Client, every_nth: u32| {
        png_cast(client, every_nth);
        // The first frame, however long the engine takes to it; then two
        // seconds of them, every one acknowledged.
        let deadline = Instant::now() + FRAME_WAIT;
        while take_frames(client).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let started = Instant::now();
        let mut n = 0;
        while started.elapsed() < Duration::from_secs(2) {
            n += take_frames(client).len();
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = client.call("Page.stopScreencast", Json::empty());
        std::thread::sleep(Duration::from_millis(200));
        let _ = take_frames(client);
        n
    };
    let every = count(&mut client, 1);
    let sixth = count(&mut client, 6);
    eprintln!("two seconds: {every} frames at every frame, {sixth} at every sixth");
    assert!(sixth >= 2, "{sixth} frames at every sixth");
    assert!(
        sixth * 3 <= every + 6,
        "{sixth} at every sixth against {every} at every one"
    );
    client.close();
    engine.kill();
}

/// The route through tmux, to the terminal behind it: the frame wrapped,
/// through the model of tmux's passthrough, into the real parser, as a
/// virtual placement under the placeholder id.
#[test]
fn the_wrapped_frame_parses_once_the_tmux_model_has_unwrapped_it() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    prepare(&mut client);
    let dir = temp_dir("tmux");
    let mut painter = Painter::at_with(&dir, png_route(Placement::Unicode, Wrap::Tmux));
    let mut terminal = a_terminal(&dir);
    let cells = Cells {
        cols: WIDTH / CELL.0,
        rows: HEIGHT / CELL.1,
    };
    png_cast(&mut client, 1);
    let mut frames = 0;
    let deadline = Instant::now() + FRAME_WAIT;
    while frames < 3 && Instant::now() < deadline {
        for (png, _) in take_frames(&mut client) {
            let wrapped = painter.png_frame(&png, cells, 2, 1);
            assert!(wrapped.starts_with(b"\x1bPtmux;"));
            assert!(
                wrapped.len() <= blinkterm::graphics::DCS_LIMIT,
                "one wrapper"
            );
            let unwrapped = tmux_unwrap(&wrapped);
            assert!(unwrapped.starts_with(b"\x1b_Ga=T,f=100,i=16,U=1,"));
            terminal.advance(&unwrapped);
            frames += 1;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = client.call("Page.stopScreencast", Json::empty());
    assert!(frames >= 3, "only {frames} frames");
    let image = terminal
        .graphics()
        .image(blinkterm::graphics::PLACEHOLDER_IMAGE_ID)
        .expect("stored under the placeholder id");
    assert_eq!((image.width, image.height), (WIDTH, HEIGHT));
    assert!(
        terminal.graphics().image(IMAGE_ID).is_none(),
        "nothing under the direct route's id"
    );
    std::fs::remove_dir_all(&dir).ok();
    client.close();
    engine.kill();
}

/// A person is driving, so the page is not told a program is.
///
/// Blink sets `navigator.webdriver` to say the browser is under automation,
/// and a page reads it to decide it is talking to a crawler; here it would be
/// a false statement, and `--disable-blink-features=AutomationControlled` in
/// [`engine::flags`] is what makes it false. Asked through the engine this
/// program starts, rather than of the switch list, because the switch is only
/// worth anything if it survives the launch.
///
/// The other headless signals are not asserted: `navigator.plugins` is empty,
/// `window.chrome` is missing and `Notification.permission` is `denied`, and
/// no switch changes them (#48). This one is the false claim; those are true
/// ones about a headless browser.
#[test]
fn the_page_is_not_told_that_a_program_is_driving() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let webdriver = evaluate(&mut client, "navigator.webdriver");
    eprintln!("navigator.webdriver = {webdriver:?}");
    assert_eq!(
        webdriver,
        Json::Bool(false),
        "navigator.webdriver is not false; is \
         --disable-blink-features=AutomationControlled still in engine::flags?"
    );
    client.close();
    engine.kill();
}

/// The page is told who is asking: Chromium, and this program by name.
///
/// Not the headless token the engine calls itself by — a person is driving,
/// and what renders is the same Chromium a desktop Chrome is — and not a
/// desktop Chrome either, because this is a terminal browser and says so.
/// The client hints are asserted beside the string because they are the half
/// `--user-agent=` cannot reach: they are built from the engine's own version
/// and only `Network.setUserAgentOverride` carries them (#48).
#[test]
fn a_page_is_told_chromium_and_this_program_and_nothing_headless() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    blinkterm::app::prepare_session(
        &mut client,
        &blinkterm::appearance::Appearance::new(
            blinkterm::appearance::Choice::Auto,
            false,
            blinkterm::appearance::Alpha::Off,
        ),
        &Identity::new(engine.agent(), None, "ja_JP.UTF-8"),
    );
    // On a page served over http from 127.0.0.1, which is a secure context:
    // `navigator.userAgentData` is undefined on `about:blank` and on the
    // `data:` url the other tests use, and the override is meant to survive
    // a navigation anyway.
    let base = serve();
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(format!("{base}plain")))]),
        )
        .expect("the page loads");
    wait_for_title(&mut client, "plain", Duration::from_secs(10));
    let agent = evaluate(&mut client, "navigator.userAgent");
    let agent = agent.as_str().expect("a user agent");
    eprintln!("navigator.userAgent = {agent}");
    assert!(
        !agent.contains("Headless"),
        "the headless token survived: {agent}"
    );
    assert!(agent.contains("Chrome/"), "{agent}");
    assert!(
        agent.contains(&format!(
            "{}/{}",
            blinkterm::identity::PRODUCT,
            blinkterm::identity::VERSION
        )),
        "this program is not named: {agent}"
    );

    // The hints say the same, which is what the switch could not do.
    let brands = evaluate(
        &mut client,
        "navigator.userAgentData.brands.map(b=>b.brand).join(',')",
    );
    let brands = brands.as_str().expect("the brands");
    eprintln!("userAgentData.brands = {brands}");
    assert!(
        !brands.contains("Headless"),
        "the hints still say headless: {brands}"
    );
    assert!(brands.contains("Chromium"), "{brands}");
    assert!(brands.contains(blinkterm::identity::PRODUCT), "{brands}");

    // And the languages are the locale's, English last, with no q-values.
    let langs = evaluate(&mut client, "navigator.languages.join(',')");
    eprintln!("navigator.languages = {langs:?}");
    assert_eq!(langs.as_str(), Some("ja-JP,ja,en"));

    client.close();
    engine.kill();
}

/// An `--engine-arg --accept-lang=…` still chooses the languages once the
/// session has been told who is asking. The override's `acceptLanguage`
/// replaces what the engine was started with, so the person's switch has to
/// be carried into it; before it was, `fr` here read as the locale's list.
#[test]
fn an_accept_lang_of_the_persons_own_survives_the_override() {
    let launch = engine::Launch {
        args: vec!["--accept-lang=fr".to_string()],
        ..engine::Launch::default()
    };
    let Some((mut engine, _browser, mut page)) = launched(&launch) else {
        return;
    };
    assert_eq!(
        evaluate(&mut page, "navigator.languages.join(',')").as_str(),
        Some("fr"),
        "the engine did not take the switch"
    );
    let identity = Identity::new(engine.agent(), None, "ja_JP.UTF-8")
        .with_accept_language(blinkterm::identity::accept_lang_arg(&launch.args));
    blinkterm::app::prepare_session(
        &mut page,
        &blinkterm::appearance::Appearance::new(
            blinkterm::appearance::Choice::Auto,
            false,
            blinkterm::appearance::Alpha::Off,
        ),
        &identity,
    );
    let languages = evaluate(&mut page, "navigator.languages.join(',')");
    eprintln!("navigator.languages = {languages:?}");
    assert_eq!(languages.as_str(), Some("fr"));
    page.close();
    engine.kill();
}

/// `blinkterm --remote`: what a sender hands over the socket is opened as
/// the running program opens it — the first url accepted in a tab in front,
/// loaded, and counted once when the engine announces it — and a url that is
/// not one to open is answered as refused and opens nothing.
#[test]
fn a_url_handed_over_the_socket_becomes_the_tab_in_front() {
    use blinkterm::remote::{self, Delivered, Listener};
    if !engine_named() {
        return;
    }
    let root = temp_dir("remote");
    let dir = root.join("profile");
    let downloads = root.join("downloads");
    std::fs::create_dir_all(&downloads).expect("a download directory");
    let appearance = blinkterm::appearance::Appearance::new(
        blinkterm::appearance::Choice::Auto,
        false,
        blinkterm::appearance::Alpha::Off,
    );
    let profile = Profile::take(Choice::At(dir.clone())).expect("a kept profile");
    // Bound where the program binds it: under the lock, before the engine.
    let mut listener = Listener::bind(&dir).expect("listening");
    let Booted {
        mut engine,
        mut browser,
        mut tabs,
        identity,
    } = blinkterm::app::boot(
        profile,
        &engine::Launch::default(),
        &downloads,
        &appearance,
        &Allowed::in_memory(),
        None,
        &Sites::none(),
        None,
    )
    .expect("the engine boots");
    let base = serve();
    assert_eq!(tabs.len(), 1);

    let sender = {
        let dir = dir.clone();
        let urls = vec![format!("{base}second"), "javascript:x".to_string()];
        std::thread::spawn(move || remote::deliver(&dir, &urls))
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let delivery = loop {
        let mut deliveries = listener.accept_ready().expect("accepting");
        if !deliveries.is_empty() {
            break deliveries.remove(0);
        }
        assert!(Instant::now() < deadline, "the sender never connected");
        std::thread::sleep(Duration::from_millis(10));
    };
    let opened = blinkterm::app::open_delivered(
        &mut tabs,
        &mut browser,
        &appearance,
        &identity,
        &Sites::none(),
        &delivery.lines,
    );
    assert_eq!(opened[0], Ok(format!("{base}second")));
    assert!(opened[1].is_err());
    delivery.answer(&opened);

    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs.active_index(), 1, "the url from outside is in front");
    {
        let tab = tabs.active_mut().expect("the tab in front");
        assert!(tab.loading, "it is loading, as a first url is");
        assert_eq!(
            wait_for_title(&mut tab.connection, "second", Duration::from_secs(10)),
            "second"
        );
    }
    assert!(
        !pump(&mut browser, &mut tabs, Duration::from_secs(3), |tabs| tabs
            .len()
            > 2),
        "the engine's announcement of the tab made a second one"
    );
    match sender.join().expect("the sender") {
        Ok(Delivered::Refused(reasons)) => {
            assert_eq!(reasons.len(), 1, "{reasons:?}");
            assert!(reasons[0].contains("javascript"), "{reasons:?}");
        }
        other => panic!("the sender heard {other:?}"),
    }

    drop(listener);
    browser.close();
    drop(tabs);
    engine.kill();
    std::fs::remove_dir_all(&root).ok();
}

use blinkterm::block::{self, Blocker};

/// A page on this machine that loads three scripts from its own address
/// and three from `x.ads.test`, which `--host-resolver-rules` sends here
/// too, and puts in its title how many loaded and how many failed.
///
/// Scripts rather than images, because the server is text and a script's
/// `load` needs only a 200: `0;` is a script.
fn page_with_ads(port: u16) -> String {
    format!(
        "<title>waiting</title><script>var ok=0,er=0;\
         function n(){{if(ok+er==6)document.title='done ok='+ok+' er='+er}}\
         ['127.0.0.1','127.0.0.1','127.0.0.1','x.ads.test','x.ads.test','x.ads.test']\
         .forEach(function(h,i){{var s=document.createElement('script');\
         s.src='http://'+h+':{port}/s'+i+'.js';\
         s.onload=function(){{ok++;n()}};s.onerror=function(){{er++;n()}};\
         document.head.appendChild(s)}});</script>"
    )
}

/// A page of `count` scripts from its own address, which says in its title
/// when every one has answered.
fn heavy_page(port: u16, count: usize) -> String {
    format!(
        "<title>waiting</title><script>var left={count};\
         for(var i=0;i<{count};i++){{var s=document.createElement('script');\
         s.src='http://127.0.0.1:{port}/h'+i+'.js';\
         s.onload=s.onerror=function(){{if(--left==0)document.title='heavy done'}};\
         document.head.appendChild(s)}}</script>"
    )
}

const HEAVY: usize = 300;

/// The engine booted as the program boots it, with `--host-resolver-rules`
/// sending `*.test` to this machine and the blocker given or not.
fn booted_blocking(blocker: Option<&Arc<Blocker>>) -> Booted {
    booted_with(blocker, None)
}

/// The same, with the console's recorder given or not.
fn booted_with(blocker: Option<&Arc<Blocker>>, console: Option<&Arc<Recorder>>) -> Booted {
    let downloads = temp_dir("block-downloads");
    let appearance = blinkterm::appearance::Appearance::new(
        blinkterm::appearance::Choice::Auto,
        false,
        blinkterm::appearance::Alpha::Off,
    );
    let launch = engine::Launch {
        args: vec!["--host-resolver-rules=MAP *.test 127.0.0.1".to_string()],
        ..engine::Launch::default()
    };
    blinkterm::app::boot(
        Profile::temporary().expect("a temporary profile"),
        &launch,
        &downloads,
        &appearance,
        &Allowed::in_memory(),
        blocker,
        &Sites::none(),
        console,
    )
    .expect("the engine boots")
}

/// Navigate and wait for the page's title to start with `wanted`.
fn load_titled(client: &mut Client, url: &str, wanted: &str) -> String {
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(url))]),
        )
        .expect("the navigation is answered");
    wait_for_title(client, wanted, Duration::from_secs(20))
}

/// How long the heavy page takes to say it is done, from the navigation.
fn heavy_load(client: &mut Client, url: &str) -> Duration {
    let started = Instant::now();
    let title = load_titled(client, url, "heavy done");
    assert_eq!(title, "heavy done");
    started.elapsed()
}

/// The blocker as `app::boot` installs it: every page's requests paused on
/// the pipe's reader thread and answered there, a listed host failed on
/// every tab — the first, one opened later — while the page's own load, and
/// the row's count is per tab. An unblocked site loads everything, and a
/// listed site's own document fails with the engine's
/// `ERR_BLOCKED_BY_CLIENT` until its site is unblocked.
#[test]
fn a_listed_host_fails_on_every_tab_an_allowed_one_loads_and_the_row_counts() {
    if !engine_named() {
        return;
    }
    let port = serve_pages(|port| {
        let mut pages = vec![
            ("/".to_string(), page_with_ads(port)),
            ("/heavy".to_string(), heavy_page(port, HEAVY)),
        ];
        pages.extend((0..6).map(|i| (format!("/s{i}.js"), "0;".to_string())));
        pages.extend((0..HEAVY).map(|i| (format!("/h{i}.js"), "0;".to_string())));
        pages
    });
    let page = format!("http://127.0.0.1:{port}/");
    let mut hosts = std::collections::HashSet::new();
    block::parse("0.0.0.0 ads.test\n", &mut hosts);
    let blocker = Arc::new(Blocker::new(hosts, Vec::new()));
    let Booted {
        mut engine,
        mut browser,
        mut tabs,
        identity: _,
    } = booted_blocking(Some(&blocker));

    let first = tabs.active_mut().expect("the first tab");
    let first_session = first.connection.session().expect("a page").to_string();
    assert_eq!(
        load_titled(&mut first.connection, &page, "done"),
        "done ok=3 er=3"
    );
    assert_eq!(
        blocker.words(Some(&first_session)).as_deref(),
        Some("3 blocked")
    );

    // A target this program attaches to later: the hook enables it as it
    // is attached, whichever path attached it.
    let created = browser
        .call(
            "Target.createTarget",
            Json::object(vec![("url", Json::string("about:blank"))]),
        )
        .expect("a second target");
    let target = created
        .get("targetId")
        .and_then(Json::as_str)
        .expect("its id")
        .to_string();
    let mut second = browser
        .attach(&target, Duration::from_secs(10))
        .expect("a session on it");
    second
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    let second_session = second.session().expect("a page").to_string();
    assert_eq!(load_titled(&mut second, &page, "done"), "done ok=3 er=3");
    assert_eq!(
        blocker.words(Some(&second_session)).as_deref(),
        Some("3 blocked")
    );
    assert_eq!(
        blocker.words(Some(&first_session)).as_deref(),
        Some("3 blocked"),
        "the first tab's count is its own"
    );

    // `alt+b` on the first tab's site, then a reload: everything.
    blocker.set_unblocked("127.0.0.1", true);
    let first = tabs.active_mut().expect("the first tab");
    first
        .connection
        .call("Page.reload", Json::empty())
        .expect("the reload");
    assert_eq!(
        wait_for_title(&mut first.connection, "done", Duration::from_secs(20)),
        "done ok=6 er=0"
    );
    assert_eq!(
        blocker.words(Some(&first_session)).as_deref(),
        Some("unblocked")
    );

    // A listed site's own document is the engine's error page, and the
    // navigation's reply says why; unblocked, it loads.
    let ads = format!("http://ads.test:{port}/");
    let reply = first
        .connection
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(&ads))]),
        )
        .expect("the navigation is answered");
    assert_eq!(
        reply.get("errorText").and_then(Json::as_str),
        Some("net::ERR_BLOCKED_BY_CLIENT"),
        "{reply}"
    );
    blocker.set_unblocked("ads.test", true);
    assert_eq!(
        load_titled(&mut first.connection, &ads, "done"),
        "done ok=6 er=0"
    );

    // What every request round-tripping through the pipe costs a heavy
    // page, printed and not asserted: a wall clock on a shared machine is
    // not a test.
    let heavy = format!("http://127.0.0.1:{port}/heavy");
    let with = heavy_load(&mut second, &heavy);
    engine.check().expect("the engine lived through all of it");
    second.close();
    drop(tabs);
    browser.close();
    engine.kill();

    let Booted {
        engine: mut plain,
        browser: mut plain_browser,
        tabs: mut plain_tabs,
        identity: _,
    } = booted_blocking(None);
    let tab = plain_tabs.active_mut().expect("the first tab");
    let without = heavy_load(&mut tab.connection, &heavy);
    eprintln!(
        "{HEAVY} scripts: {:.2} s with every request paused and answered, \
         {:.2} s with nothing paused",
        with.as_secs_f64(),
        without.as_secs_f64()
    );
    drop(plain_tabs);
    plain_browser.close();
    plain.kill();
}

/// A renderer that dies keeps its session, and the session keeps `Fetch`:
/// the page `ctrl+r` brings back has its requests paused and answered like
/// the first, with nothing sent again. Measured before it was relied on,
/// since a session whose `Fetch` came back off would be a page that loads
/// its ads, and one whose pauses came back unanswered a page that hangs.
#[test]
fn a_page_that_crashed_is_still_blocked_after_its_reload() {
    if !engine_named() {
        return;
    }
    let port = serve_pages(|port| {
        let mut pages = vec![("/".to_string(), page_with_ads(port))];
        pages.extend((0..6).map(|i| (format!("/s{i}.js"), "0;".to_string())));
        pages
    });
    let mut hosts = std::collections::HashSet::new();
    block::parse("ads.test\n", &mut hosts);
    let blocker = Arc::new(Blocker::new(hosts, Vec::new()));
    let Booted {
        mut engine,
        mut browser,
        mut tabs,
        identity: _,
    } = booted_blocking(Some(&blocker));
    let tab = tabs.active_mut().expect("the first tab");
    let page = format!("http://127.0.0.1:{port}/");
    assert_eq!(
        load_titled(&mut tab.connection, &page, "done"),
        "done ok=3 er=3"
    );

    let _ = tab.connection.events();
    let _crash = tab
        .connection
        .send("Page.crash", Json::empty())
        .expect("Page.crash sent");
    let deadline = Instant::now() + CRASH_NOTICE;
    let mut crashed = false;
    while !crashed && Instant::now() < deadline {
        crashed = tab
            .connection
            .events()
            .iter()
            .any(|event| event.method == "Inspector.targetCrashed");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(crashed, "the renderer did not die");
    tab.connection
        .call_within("Page.reload", Json::empty(), Duration::from_secs(5))
        .expect("Page.reload answers on a crashed page");
    assert_eq!(
        wait_for_title(&mut tab.connection, "done", Duration::from_secs(20)),
        "done ok=3 er=3"
    );
    let session = tab.connection.session().map(str::to_string);
    assert_eq!(
        blocker.words(session.as_deref()).as_deref(),
        Some("3 blocked")
    );
    engine.check().expect("the engine lived through it");
    drop(tabs);
    browser.close();
    engine.kill();
}

use blinkterm::login::{self, Filled, Login, Site};

/// The password command every login test runs: a dummy password and a user
/// name, printed the way `pass` prints them.
const PRINTS_A_LOGIN: &str = "sh -c 'printf \"secret\\nlogin: me\\n\"'";

/// A password command with a window, run as the loop runs one — spawned,
/// polled and pumped until it has exited — for `site`: what it printed.
fn fetch_login(site: &Site) -> Login {
    let command = picker::Command::parse("password-command", PRINTS_A_LOGIN).expect("a command");
    let mut gui = login::Gui::spawn(&command, site, "T", &site.url, &std::env::temp_dir())
        .expect("the command starts");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let readable = gui.fd().is_some_and(|fd| {
            blinkterm::tty::poll_readable(&[fd], 50)
                .expect("poll")
                .contains(&fd)
        });
        match gui.pump(readable) {
            Some(login::Outcome::Found(login)) => return login,
            Some(other) => panic!("the command printed no login: {other:?}"),
            None => {}
        }
        if gui.fd().is_none() {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    panic!("the password command did not finish");
}

/// The fill, as `app::finish_login` sends it: the script in the world the
/// find prompt uses, the login as its arguments.
fn fill(client: &mut Client, context: i64, site: &Site, login: &Login) -> Option<Filled> {
    let reply = client
        .call_within(
            "Runtime.callFunctionOn",
            login::fill_params(context, site, login),
            Duration::from_secs(5),
        )
        .expect("the page answers the fill");
    login::filled(&reply)
}

/// A login form whose own scripts, in the page's world, say what they
/// heard: every `input` event that reaches the document is counted, the
/// title shows what the fields hold as their listeners see it, a submit is
/// noted and stopped, and the user field's `value` is replaced on the
/// element itself the way React replaces it, noting every write through it.
fn login_form(title: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>loading</title>\
         <body style='margin:0;background:#fff'>\
         <form id=f action=/done>\
         <input id=u name=username autocomplete=username>\
         <input id=p type=password name=password>\
         <button>Sign in</button></form>\
         <script>\
         window.inputs=0;window.changes=0;window.submitted=false;window.trapped=0;\
         var u=document.getElementById('u'),p=document.getElementById('p');\
         var own=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value');\
         Object.defineProperty(u,'value',{{configurable:true,\
         get:function(){{return own.get.call(this)}},\
         set:function(v){{window.trapped++;own.set.call(this,v)}}}});\
         document.addEventListener('input',function(){{inputs++}});\
         document.addEventListener('change',function(){{changes++}});\
         var show=function(){{document.title='user='+u.value+' pass='+p.value}};\
         u.addEventListener('input',show);p.addEventListener('input',show);\
         document.getElementById('f').addEventListener('submit',function(e){{\
         e.preventDefault();submitted=true}});\
         onload=function(){{document.title='{title}'}};\
         </script></body>"
    )
}

/// An engine on a page this test serves, sized as the pane would be.
fn logging_in(url: &str, title: &str) -> Option<(Engine, Client)> {
    let (engine, mut client) = connect()?;
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    open(&mut client, url, title);
    Some((engine, client))
}

/// `fill-login` on a login form at `http://localhost` (#65): the command's
/// first line reaches the password field and its `login:` line the user
/// field, through the engine's own setter rather than the one the page put
/// on the element, and the page's own listeners — on the fields and on the
/// document — hear `input` and `change` as they hear a person typing. The
/// form is not submitted.
#[test]
fn a_login_form_on_localhost_is_filled_from_what_the_command_printed() {
    let port = serve_pages(|_| vec![("/login".to_string(), login_form("ready"))]);
    let url = format!("http://localhost:{port}/login");
    let Some((mut engine, mut client)) = logging_in(&url, "ready") else {
        return;
    };
    let site = login::site(&url).expect("localhost may be filled");
    assert_eq!(site.host, "localhost");
    let login = fetch_login(&site);
    let context = find_world(&mut client);

    assert_eq!(
        fill(&mut client, context, &site, &login),
        Some(Filled::Both)
    );
    assert_eq!(
        wait_for_title(&mut client, "user=", Duration::from_secs(5)),
        "user=me pass=secret",
        "the page's listeners saw both values"
    );
    assert_eq!(evaluate(&mut client, "inputs").as_f64(), Some(2.0));
    assert_eq!(evaluate(&mut client, "changes").as_f64(), Some(2.0));
    assert_eq!(
        evaluate(&mut client, "trapped").as_f64(),
        Some(0.0),
        "the page's own value setter was not the one used"
    );
    assert_eq!(
        evaluate(&mut client, "document.activeElement.id").as_str(),
        Some("p"),
        "left in the password field, as a person would be"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(evaluate(&mut client, "submitted").as_bool(), Some(false));
    assert_eq!(
        evaluate(&mut client, "location.pathname").as_str(),
        Some("/login"),
        "nothing was submitted"
    );
    assert_eq!(
        login::sentence(Filled::Both, &site.host),
        "filled login for localhost"
    );

    client.close();
    engine.kill();
}

/// A page on plain `http` to another machine is refused before any command
/// runs ([`login::site`]); and the script, handed a login for that host
/// anyway, refuses it too, because it checks each document's own scheme.
/// `example.test` is sent to this machine by the engine's resolver, so that
/// a host that is not a local one can be served here.
#[test]
fn a_page_without_https_is_refused_before_any_command_runs_and_by_the_script() {
    let port = serve_pages(|_| vec![("/login".to_string(), login_form("ready"))]);
    let launch = engine::Launch {
        args: vec!["--host-resolver-rules=MAP example.test 127.0.0.1".to_string()],
        ..engine::Launch::default()
    };
    let Some((mut engine, browser, mut client)) = launched(&launch) else {
        return;
    };
    drop(browser);
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    let url = format!("http://example.test:{port}/login");
    open(&mut client, &url, "ready");

    assert_eq!(login::site(&url), Err(login::Refused::NotSecure));
    // What the script would be given had the check above been skipped.
    let site = Site {
        host: "example.test".to_string(),
        domain: "example.test".to_string(),
        url: url.clone(),
    };
    let login = fetch_login(&site);
    let context = find_world(&mut client);
    assert_eq!(
        fill(&mut client, context, &site, &login),
        Some(Filled::Refused)
    );
    assert_eq!(
        evaluate(&mut client, "document.getElementById('p').value").as_str(),
        Some("")
    );
    assert_eq!(evaluate(&mut client, "inputs").as_f64(), Some(0.0));

    // And a login fetched for another host is not given to this one, on
    // a page it could otherwise fill.
    let port = serve_pages(|_| vec![("/login".to_string(), login_form("again"))]);
    open(
        &mut client,
        &format!("http://localhost:{port}/login"),
        "again",
    );
    let elsewhere = login::site("http://127.0.0.1/").expect("a site");
    let context = find_world(&mut client);
    assert_eq!(
        fill(&mut client, context, &elsewhere, &login),
        Some(Filled::Refused),
        "the host the secret was fetched for is not this page's"
    );

    client.close();
    engine.kill();
}

/// A login form inside a frame of the page's own origin is filled; one in a
/// frame of another origin is out of reach, and the page with only that has
/// no password field as far as the fill can tell.
#[test]
fn a_same_origin_frame_is_filled_and_a_cross_origin_one_is_not() {
    let port = serve_pages(|port| {
        vec![
            ("/login".to_string(), login_form("inner")),
            (
                "/same".to_string(),
                format!(
                    "<!doctype html><title>loading</title><body>\
                     <p>A page with its login in a frame.</p>\
                     <iframe id=frame src='http://127.0.0.1:{port}/login'></iframe>\
                     <script>onload=function(){{document.title='same'}}</script></body>"
                ),
            ),
            (
                "/cross".to_string(),
                format!(
                    "<!doctype html><title>loading</title><body>\
                     <iframe id=frame src='http://localhost:{port}/login'></iframe>\
                     <script>onload=function(){{document.title='cross'}}</script></body>"
                ),
            ),
        ]
    });
    let url = format!("http://127.0.0.1:{port}/same");
    let Some((mut engine, mut client)) = logging_in(&url, "same") else {
        return;
    };
    let site = login::site(&url).expect("127.0.0.1 may be filled");
    let login = fetch_login(&site);
    let context = find_world(&mut client);
    assert_eq!(
        fill(&mut client, context, &site, &login),
        Some(Filled::Both)
    );
    let inner = "document.getElementById('frame').contentDocument";
    assert_eq!(
        evaluate(&mut client, &format!("{inner}.title")).as_str(),
        Some("user=me pass=secret")
    );
    assert_eq!(
        evaluate(
            &mut client,
            "document.getElementById('frame').contentWindow.inputs"
        )
        .as_f64(),
        Some(2.0),
        "the frame's own listeners heard it"
    );

    let url = format!("http://127.0.0.1:{port}/cross");
    open(&mut client, &url, "cross");
    assert_eq!(
        evaluate(
            &mut client,
            "document.getElementById('frame').contentDocument === null"
        )
        .as_bool(),
        Some(true),
        "the frame really is another origin"
    );
    let context = find_world(&mut client);
    assert_eq!(
        fill(&mut client, context, &site, &login),
        Some(Filled::NoField)
    );
    assert_eq!(
        login::sentence(Filled::NoField, &site.host),
        "no password field on this page"
    );

    client.close();
    engine.kill();
}

/// With two login forms on a page, the one the focus is in is the one
/// filled; with the focus nowhere, the first.
#[test]
fn the_form_with_the_focus_is_the_one_filled() {
    let page = "<!doctype html><meta charset=utf-8><title>loading</title><body>\
         <form id=a><input id=au name=email type=email><input id=ap type=password></form>\
         <form id=b><input id=search name=q>\
         <input id=bu name=login><input id=bx name=other><input id=bp type=password></form>\
         <input id=hidden type=password style='display:none'>\
         <script>onload=function(){document.title='two'}</script></body>"
        .to_string();
    let port = serve_pages(|_| vec![("/two".to_string(), page)]);
    let url = format!("http://127.0.0.1:{port}/two");
    let Some((mut engine, mut client)) = logging_in(&url, "two") else {
        return;
    };
    let site = login::site(&url).expect("a site");
    let login = fetch_login(&site);
    let values = |client: &mut Client| {
        evaluate(
            client,
            "['au','ap','search','bu','bx','bp','hidden']\
             .map(function(id){return document.getElementById(id).value}).join(',')",
        )
        .as_str()
        .unwrap_or_default()
        .to_string()
    };

    evaluate(&mut client, "document.getElementById('bx').focus()");
    let context = find_world(&mut client);
    assert_eq!(
        fill(&mut client, context, &site, &login),
        Some(Filled::Both)
    );
    assert_eq!(
        values(&mut client),
        ",,,me,,secret,",
        "the focused form, and its field that says login rather than the nearest"
    );

    open(&mut client, &url, "two");
    evaluate(&mut client, "document.activeElement.blur()");
    let context = find_world(&mut client);
    assert_eq!(
        fill(&mut client, context, &site, &login),
        Some(Filled::Both)
    );
    assert_eq!(values(&mut client), "me,secret,,,,,");

    client.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// Site styles and scripts
// ---------------------------------------------------------------------------

/// A directory of site files, written afresh with the modes the program
/// accepts: the directory 0755, each file 0644.
fn site_files(what: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir(what);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    write_site_files(&dir, files);
    dir
}

/// Write (or overwrite) files in a site directory, 0644.
fn write_site_files(dir: &std::path::Path, files: &[(&str, &str)]) {
    use std::os::unix::fs::PermissionsExt;
    for (name, text) in files {
        let path = dir.join(name);
        std::fs::write(&path, text).expect("a site file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    }
}

/// A session set up the way `connect_tab` sets one up, with the site files
/// registered on it as a new session's are; the identifiers come back.
fn told_sites(client: &mut Client, sites: &Sites) -> Vec<String> {
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(client);
    let appearance = blinkterm::appearance::Appearance::new(
        blinkterm::appearance::Choice::Auto,
        false,
        blinkterm::appearance::Alpha::Off,
    );
    blinkterm::app::prepare_session(client, &appearance, &Identity::new(None, None, "C"));
    sites::install(client, sites, true)
}

/// The pages the site tests are served, on 127.0.0.1: `/plain`, which says
/// `ready`; `/sites`, whose own inline script writes what it could see into
/// its title; `/framed`, with a `srcdoc` iframe that has a script of its
/// own; and `/csp`, which forbids every script of its own.
fn serve_site_pages() -> String {
    let port = serve_pages(|_| {
        vec![
            (
                "/plain".to_string(),
                "<!doctype html><body>plain<script>document.title='ready'</script></body>"
                    .to_string(),
            ),
            (
                "/again".to_string(),
                "<!doctype html><body>again<script>document.title='ready'</script></body>"
                    .to_string(),
            ),
            (
                "/sites".to_string(),
                "<!doctype html><body><script>const s = sessionStorage;\
                 document.title = 'ready ' + s.site + typeof hidden + window.fromMain + ' ' + s.order\
                 </script></body>"
                    .to_string(),
            ),
            (
                "/framed".to_string(),
                "<!doctype html><body style='margin:0'>\
                 <iframe srcdoc='<body>framed<script>1</script></body>'></iframe>\
                 <script>document.title='ready'</script></body>"
                    .to_string(),
            ),
            (
                "/csp".to_string(),
                "<!doctype html><meta http-equiv=Content-Security-Policy content=\"script-src 'none'\">\
                 <title>ready</title><body>csp<script>window.pageRan = 1</script></body>"
                    .to_string(),
            ),
        ]
    });
    format!("http://127.0.0.1:{port}")
}

/// Evaluate `expression` until it is `wanted`, for five seconds, and say
/// what it was last.
fn wait_for_value(client: &mut Client, expression: &str, wanted: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let now = evaluate(client, expression)
            .as_str()
            .unwrap_or_default()
            .to_string();
        if now == wanted || Instant::now() >= deadline {
            return now;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

const BODY_BACKGROUND: &str = "getComputedStyle(document.body).backgroundColor";
const ALL_MARK: &str =
    "getComputedStyle(document.documentElement).getPropertyValue('--blinkterm-all').trim()";

/// A host's style is on that host's pages from document start, through a
/// navigation and a reload, and not on another; `all` is on every page, a
/// `data:` one included. What the page can see of it is
/// `adoptedStyleSheets`, and nothing else.
#[test]
fn a_site_style_changes_its_host_on_load_and_after_a_navigation_and_leaves_another_host_alone() {
    let dir = site_files(
        "sites-style",
        &[
            ("127.0.0.1.css", "body{background:rgb(1,2,3)!important}"),
            ("all.css", "html{--blinkterm-all:1}"),
        ],
    );
    let sites = Sites::read(&dir);
    assert_eq!(sites.styles().len(), 2, "{:?}", sites.skipped);
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let identifiers = told_sites(&mut client, &sites);
    assert_eq!(identifiers.len(), 1, "every style in one registration");
    let base = serve_site_pages();

    go_to(&mut client, &format!("{base}/plain"));
    let check_host = |client: &mut Client, when: &str| {
        assert_eq!(
            evaluate(client, BODY_BACKGROUND).as_str(),
            Some("rgb(1, 2, 3)"),
            "{when}"
        );
        assert_eq!(evaluate(client, ALL_MARK).as_str(), Some("1"), "{when}");
        assert_eq!(
            evaluate(client, "document.adoptedStyleSheets.length").as_f64(),
            Some(2.0),
            "{when}"
        );
        assert_eq!(
            evaluate(client, "document.styleSheets.length").as_f64(),
            Some(0.0),
            "{when}: nothing in the page's own sheets"
        );
        assert_eq!(
            evaluate(client, "typeof __blinktermSiteSheets").as_str(),
            Some("undefined"),
            "{when}: nothing on the page's window"
        );
    };
    check_host(&mut client, "on load");
    go_to(&mut client, &format!("{base}/again"));
    check_host(&mut client, "after a navigation");
    evaluate(&mut client, "document.title='leaving'");
    client
        .call("Page.reload", Json::empty())
        .expect("the page reloads");
    assert_eq!(
        wait_for_title(&mut client, "ready", Duration::from_secs(10)),
        "ready"
    );
    check_host(&mut client, "after a reload");

    go_to(
        &mut client,
        "data:text/html,<body><script>document.title='ready'</script></body>",
    );
    assert_eq!(
        evaluate(&mut client, BODY_BACKGROUND).as_str(),
        Some("rgba(0, 0, 0, 0)"),
        "another host is left alone"
    );
    assert_eq!(
        evaluate(&mut client, ALL_MARK).as_str(),
        Some("1"),
        "all is every page"
    );
    assert_eq!(
        evaluate(&mut client, "document.adoptedStyleSheets.length").as_f64(),
        Some(1.0)
    );

    client.close();
    engine.kill();
}

/// A script runs before the page's own first script, in a world the page
/// cannot see into — unless the file asked for the page's — in the order
/// the files are ranked; and the page's CSP does not stop one in the page's
/// world.
///
/// What the scripts leave for the page is in `sessionStorage`, which both
/// worlds share, and not on `document.documentElement`: at the start of a
/// document after the first there is no `<html>` yet, and a script that
/// reaches for it throws. (The first document of a new session is the
/// exception, because its context is made late; a test that used the
/// element passed on the first page and failed on every one after it.)
#[test]
fn a_site_script_runs_at_document_start_in_a_world_the_page_cannot_see_unless_it_asked_for_the_page_s(
) {
    let dir = site_files(
        "sites-script",
        &[
            (
                "all.js",
                "sessionStorage.site = 'yes'; sessionStorage.order = 'a'; globalThis.hidden = 1",
            ),
            ("*.0.0.1.js", "sessionStorage.order += 'b'"),
            (
                "127.0.0.1.js",
                "// @world main\nwindow.fromMain = 2;\nsessionStorage.order += 'c'",
            ),
            ("example.com.js", "window.notHere = 1"),
            ("example.org.js", "window.notHere = 1"),
        ],
    );
    let sites = Sites::read(&dir);
    assert_eq!(sites.scripts().len(), 5, "{:?}", sites.skipped);
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let started = Instant::now();
    let identifiers = told_sites(&mut client, &sites);
    eprintln!(
        "{} registrations, and the session's setup, in {:?}",
        identifiers.len(),
        started.elapsed()
    );
    assert_eq!(identifiers.len(), 5);
    let again = Instant::now();
    let more = sites::install(&mut client, &sites, false);
    eprintln!("five scripts registered alone: {:?}", again.elapsed());
    sites::remove(&mut client, &more);
    let base = serve_site_pages();

    evaluate(&mut client, "document.title='leaving'");
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(format!("{base}/sites")))]),
        )
        .expect("the page loads");
    assert_eq!(
        wait_for_title(&mut client, "ready ", Duration::from_secs(10)),
        "ready yesundefined2 abc",
        "at document start, isolated unless asked, in rank order"
    );
    go_to(&mut client, &format!("{base}/plain"));
    assert_eq!(
        evaluate(&mut client, "sessionStorage.order + window.fromMain").as_str(),
        Some("abc2"),
        "and on the next document"
    );
    assert_eq!(
        evaluate(&mut client, "typeof notHere").as_str(),
        Some("undefined")
    );

    go_to(&mut client, &format!("{base}/csp"));
    assert_eq!(
        evaluate(&mut client, "typeof pageRan").as_str(),
        Some("undefined"),
        "the page's own script was refused"
    );
    let under_csp = evaluate(&mut client, "window.fromMain");
    eprintln!("a main-world site script under script-src 'none': fromMain = {under_csp}");
    assert_eq!(under_csp.as_f64(), Some(2.0));

    client.close();
    engine.kill();
}

/// `reload-sites`, as the program does it: the old registrations taken back,
/// the new ones made without running the scripts. The style changes on the
/// document where it stands, one sheet and not two; the old script never
/// runs again, and the new one runs from the next load.
#[test]
fn reload_sites_replaces_the_styles_where_the_page_stands_and_the_old_script_never_runs_again() {
    const VERSION: &str = "sessionStorage.v = (sessionStorage.v || '') + ";
    let dir = site_files(
        "sites-reload",
        &[
            ("127.0.0.1.css", "body{background:rgb(255,0,0)!important}"),
            ("127.0.0.1.js", &format!("{VERSION}'1'")),
        ],
    );
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let old = told_sites(&mut client, &Sites::read(&dir));
    assert_eq!(old.len(), 2);
    let base = serve_site_pages();
    go_to(&mut client, &format!("{base}/plain"));
    assert_eq!(
        evaluate(&mut client, BODY_BACKGROUND).as_str(),
        Some("rgb(255, 0, 0)")
    );
    assert_eq!(
        evaluate(&mut client, "sessionStorage.v").as_str(),
        Some("1")
    );
    evaluate(&mut client, "window.kept = 1");

    write_site_files(
        &dir,
        &[
            ("127.0.0.1.css", "body{background:rgb(0,0,255)!important}"),
            ("127.0.0.1.js", &format!("{VERSION}'2'")),
        ],
    );
    let sites = Sites::read(&dir);
    let started = Instant::now();
    sites::remove(&mut client, &old);
    let new = sites::install(&mut client, &sites, false);
    assert_eq!(new.len(), sites.params(false).len());
    let blue = wait_for_value(&mut client, BODY_BACKGROUND, "rgb(0, 0, 255)");
    eprintln!("the new style in place after {:?}", started.elapsed());
    assert_eq!(blue, "rgb(0, 0, 255)", "in place");
    assert_eq!(
        evaluate(&mut client, "window.kept").as_f64(),
        Some(1.0),
        "the same document"
    );
    assert_eq!(
        evaluate(&mut client, "document.adoptedStyleSheets.length").as_f64(),
        Some(1.0),
        "replaced, not added to"
    );
    assert_eq!(
        evaluate(&mut client, "sessionStorage.v").as_str(),
        Some("1"),
        "the new script waits for the next document"
    );

    go_to(&mut client, &format!("{base}/plain"));
    assert_eq!(
        evaluate(&mut client, "sessionStorage.v").as_str(),
        Some("12"),
        "the new script, and the old one never again"
    );
    assert_eq!(
        evaluate(&mut client, BODY_BACKGROUND).as_str(),
        Some("rgb(0, 0, 255)")
    );

    client.close();
    engine.kill();
}

/// A same-process iframe is a frame of the same target, so the style
/// reaches it; and a `srcdoc` frame, whose own `location` has no host, is
/// matched by the host of the page it inherited its base url from.
#[test]
fn a_site_style_reaches_a_same_process_iframe_of_the_page() {
    let dir = site_files(
        "sites-frame",
        &[
            ("all.css", "body{background:rgb(4,5,6)!important}"),
            ("127.0.0.1.css", "body{color:rgb(7,8,9)!important}"),
        ],
    );
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    told_sites(&mut client, &Sites::read(&dir));
    let base = serve_site_pages();

    let inside = |what: &str| {
        format!(
            "(() => {{ const d = document.querySelector('iframe').contentDocument; \
             return d && d.body ? getComputedStyle(d.body).{what} : ''; }})()"
        )
    };
    go_to(&mut client, &format!("{base}/framed"));
    assert_eq!(
        wait_for_value(&mut client, &inside("backgroundColor"), "rgb(4, 5, 6)"),
        "rgb(4, 5, 6)",
        "all, in the iframe"
    );
    let color = wait_for_value(&mut client, &inside("color"), "rgb(7, 8, 9)");
    eprintln!("a srcdoc iframe of 127.0.0.1, its colour: {color}");
    assert_eq!(color, "rgb(7, 8, 9)", "the parent's host, by its base url");

    go_to(&mut client, &framed(true));
    assert_eq!(
        wait_for_value(&mut client, &inside("backgroundColor"), "rgb(4, 5, 6)"),
        "rgb(4, 5, 6)",
        "a data: page's iframe too"
    );

    client.close();
    engine.kill();
}

/// One registration per script file, so a file that does not parse is that
/// file's problem and nobody else's.
#[test]
fn a_syntax_error_in_one_site_script_does_not_stop_the_others() {
    let dir = site_files(
        "sites-syntax",
        &[
            ("all.js", "this is not js"),
            ("127.0.0.1.js", "sessionStorage.ok = '1'"),
        ],
    );
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let identifiers = told_sites(&mut client, &Sites::read(&dir));
    eprintln!(
        "registrations accepted, one of them not JavaScript: {}",
        identifiers.len()
    );
    let base = serve_site_pages();
    go_to(&mut client, &format!("{base}/plain"));
    assert_eq!(
        evaluate(&mut client, "sessionStorage.ok").as_str(),
        Some("1")
    );

    client.close();
    engine.kill();
}

/// What a large style costs every document, on a page it fits and on one it
/// does not — the table is carried into both: measured, not asserted. The
/// page's own clock, from the start of the navigation to its
/// `domInteractive`, averaged over five loads.
#[test]
fn a_large_site_style_is_measured_at_document_start() {
    let rules: String = (0..8000)
        .map(|i| format!(".rule-{i} {{ color: red; }}\n"))
        .collect();
    let fits = site_files("sites-large", &[("all.css", &rules)]);
    let elsewhere = site_files("sites-large-elsewhere", &[("example.com.css", &rules)]);
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    let base = serve_site_pages();
    let url = format!("{base}/plain");
    let loads = |client: &mut Client| {
        go_to(client, &url);
        let mut total = 0.0;
        for _ in 0..5 {
            go_to(client, &url);
            total += evaluate(
                client,
                "performance.getEntriesByType('navigation')[0].domInteractive",
            )
            .as_f64()
            .unwrap_or(f64::NAN);
        }
        total / 5.0
    };
    let bare = loads(&mut client);
    let registered = sites::install(&mut client, &Sites::read(&elsewhere), true);
    let other_host = loads(&mut client);
    sites::remove(&mut client, &registered);
    sites::install(&mut client, &Sites::read(&fits), true);
    let this_host = loads(&mut client);
    assert_eq!(
        evaluate(&mut client, "document.adoptedStyleSheets.length").as_f64(),
        Some(1.0)
    );
    eprintln!(
        "{} bytes of css; to domInteractive: {bare:.1} ms bare, {other_host:.1} ms carried \
         to a page it does not fit, {this_host:.1} ms adopted",
        rules.len()
    );

    client.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// Reader mode: the article alone, in a frame of its own in the page.
// ---------------------------------------------------------------------------

use blinkterm::reader::{self, Answered};

/// A page with an article in the middle of what a page mostly is: a nav of
/// twelve links, a sidebar of six, a footer of links, and a fixed cookie
/// box. The article has a title, a byline, eight paragraphs with `quokka`
/// once, a picture and a relative link; the footer has `quokka` too, which
/// must not be found once the reader is on.
fn reader_page() -> String {
    let nav: String = (0..12)
        .map(|i| format!("<a href='/n{i}'>Section {i}</a> "))
        .collect();
    let side: String = (0..6)
        .map(|i| format!("<li><a href='/s{i}'>Related story number {i}</a></li>"))
        .collect();
    let paragraphs: String = (0..8)
        .map(|i| {
            let animal = if i == 3 { "a quokka" } else { "an animal" };
            format!(
                "<p>Paragraph {i} of the article, about {animal}, written at length so that \
                 it reads as prose, with commas, clauses, and a full stop.</p>"
            )
        })
        .collect();
    format!(
        "<!doctype html><meta charset=utf-8><title>loading</title>\
         <body style='margin:8px;font:16px sans-serif;background:#fff'>\
         <nav id=nav>{nav}</nav>\
         <aside class=sidebar><ul>{side}</ul></aside>\
         <article><h1>The article</h1><p class=byline>By Someone</p>\
         {paragraphs}\
         <img src=/pic.png alt=pic width=40 height=30>\
         <a id=more href=/more>more</a></article>\
         <footer><a href='/about'>About</a> <a href='/terms'>Terms</a> \
         <p>A footer line that mentions the quokka again, with a link or two.</p></footer>\
         <div id=cookie style='position:fixed;left:0;bottom:0;width:100%;background:#ccc'>cookies</div>\
         <div style='height:2000px'></div>\
         <script>onload=function(){{document.title='ready'}}</script></body>"
    )
}

/// A page with nothing to read: a nav and a login form.
fn nothing_page() -> String {
    "<!doctype html><meta charset=utf-8><title>loading</title><body>\
     <nav><a href=/a>Home</a> <a href=/b>About</a></nav>\
     <form><input name=user><input type=password><button>Sign in</button></form>\
     <script>onload=function(){document.title='ready'}</script></body>"
        .to_string()
}

/// The reader's pages, served, and an engine on one of them with the shared
/// world made in it, and the port they are served on.
fn reading(path: &str) -> Option<(Engine, Client, i64, u16)> {
    let port = serve_pages(|_| {
        vec![
            ("/".to_string(), reader_page()),
            ("/none".to_string(), nothing_page()),
            ("/more".to_string(), "<title>more</title>".to_string()),
        ]
    });
    let (engine, client, context) = finding(&format!("http://localhost:{port}{path}"), "ready")?;
    Some((engine, client, context, port))
}

/// The toggle, as the program sends it, waited for.
fn toggle(client: &mut Client, context: i64, on: bool, alpha: bool) -> Option<Answered> {
    let reply = client
        .call_within(
            "Runtime.callFunctionOn",
            reader::call_params(context, on, alpha),
            Duration::from_secs(5),
        )
        .expect("the page answers the reader");
    reader::answered(&reply)
}

/// Something about the reader's own document, `d`, asked in the page.
fn in_reader(client: &mut Client, expression: &str) -> Json {
    evaluate(
        client,
        &format!(
            "(function(){{var d=document.getElementById('{}').contentDocument;return {expression};}})()",
            reader::FRAME_ID
        ),
    )
}

/// A list of numbers the page answered.
fn numbers(json: &Json) -> Vec<f64> {
    json.as_array()
        .map(|all| all.iter().filter_map(Json::as_f64).collect())
        .unwrap_or_default()
}

/// On: the article is in the frame, cleaned, its urls absolute, and
/// everything else hidden, the page at the top. On again changes nothing.
/// Off: the page exactly as it was, at the same place. A wheel over the
/// frame moves the page, and a reload is the page without it.
#[test]
fn reader_mode_keeps_the_article_hides_the_rest_and_comes_off_leaving_the_page_as_it_was() {
    let Some((mut engine, mut client, context, port)) = reading("/") else {
        return;
    };
    evaluate(&mut client, "scrollTo(0, 300)");
    assert_eq!(scroll_y(&mut client), 300.0);
    let sheets = evaluate(&mut client, "document.adoptedStyleSheets.length")
        .as_f64()
        .expect("a count");

    let started = Instant::now();
    assert_eq!(
        toggle(&mut client, context, true, false),
        Some(Answered::On)
    );
    eprintln!("reader on: {:?}", started.elapsed());
    assert_eq!(
        evaluate(&mut client, "!!document.getElementById('blinkterm-reader')").as_bool(),
        Some(true)
    );
    for hidden in ["nav", "cookie"] {
        assert_eq!(
            evaluate(
                &mut client,
                &format!("getComputedStyle(document.getElementById('{hidden}')).display")
            )
            .as_str(),
            Some("none"),
            "{hidden} is hidden"
        );
    }
    assert_eq!(
        evaluate(&mut client, "typeof __blinktermReader").as_str(),
        Some("undefined"),
        "the page does not see the script's state"
    );
    assert_eq!(
        evaluate(&mut client, "document.adoptedStyleSheets.length").as_f64(),
        Some(sheets + 1.0)
    );
    assert_eq!(scroll_y(&mut client), 0.0, "the article from its top");

    assert_eq!(
        in_reader(&mut client, "d.querySelectorAll('p').length").as_f64(),
        Some(9.0),
        "eight paragraphs and the line under the title; the byline is not repeated"
    );
    assert_eq!(
        in_reader(&mut client, "d.querySelector('.meta').textContent").as_str(),
        Some("By Someone")
    );
    assert_eq!(
        in_reader(
            &mut client,
            "d.querySelectorAll('nav, footer, aside').length"
        )
        .as_f64(),
        Some(0.0)
    );
    assert_eq!(
        in_reader(
            &mut client,
            "d.querySelectorAll('h1').length + ':' + d.querySelector('h1').textContent"
        )
        .as_str(),
        Some("1:The article"),
        "the title, once"
    );
    assert_eq!(
        in_reader(
            &mut client,
            "d.querySelector('a[href]').getAttribute('href')"
        )
        .as_str(),
        Some(format!("http://localhost:{port}/more").as_str()),
        "a link made absolute"
    );
    assert_eq!(
        in_reader(&mut client, "d.querySelector('img').getAttribute('src')").as_str(),
        Some(format!("http://localhost:{port}/pic.png").as_str())
    );
    assert_eq!(
        in_reader(&mut client, "d.querySelector('base').target").as_str(),
        Some("_top")
    );
    assert_eq!(
        in_reader(
            &mut client,
            "d.body.querySelectorAll('[style], [class]:not(.meta), [id]').length"
        )
        .as_f64(),
        Some(0.0),
        "no attribute but the ones that mean something"
    );
    let heights = numbers(&evaluate(
        &mut client,
        "(function(){var f=document.getElementById('blinkterm-reader');\
         return [f.getBoundingClientRect().height, f.contentDocument.documentElement.scrollHeight]})()",
    ));
    eprintln!(
        "frame {} tall for a document {} tall",
        heights[0], heights[1]
    );
    assert!(
        heights[0] >= heights[1] && heights[0] > HEIGHT as f64,
        "{heights:?}"
    );

    // What the frame holds growing — an image arriving late — grows the
    // frame, through the observer on its body.
    let tall = |client: &mut Client| {
        evaluate(
            client,
            "document.getElementById('blinkterm-reader').getBoundingClientRect().height",
        )
        .as_f64()
        .unwrap_or_default()
    };
    let before = tall(&mut client);
    in_reader(
        &mut client,
        "d.querySelector('article').appendChild(d.createElement('div')).style.height='500px'",
    );
    assert!(
        wait_until(&mut client, true, |client| tall(client) > before + 400.0),
        "the frame follows what it holds: {before} then {}",
        tall(&mut client)
    );

    // A wheel over the frame moves the page: the frame's own document does
    // not scroll, and the page is as tall as the article.
    client
        .call(
            "Input.dispatchMouseEvent",
            Json::object(vec![
                ("type", Json::string("mouseWheel")),
                ("x", Json::number(WIDTH / 2)),
                ("y", Json::number(HEIGHT / 2)),
                ("deltaX", Json::number(0)),
                ("deltaY", Json::number(200)),
            ]),
        )
        .expect("the wheel is dispatched");
    let moved = wait_until(&mut client, true, |client| scroll_y(client) > 0.0);
    eprintln!(
        "a wheel over the frame: the page at {}",
        scroll_y(&mut client)
    );
    assert!(moved, "a wheel over the reader scrolls the page");

    assert_eq!(
        toggle(&mut client, context, true, false),
        Some(Answered::On),
        "on again is on"
    );
    assert_eq!(
        evaluate(&mut client, "document.querySelectorAll('iframe').length").as_f64(),
        Some(1.0),
        "and adds nothing"
    );

    let started = Instant::now();
    assert_eq!(
        toggle(&mut client, context, false, false),
        Some(Answered::Off)
    );
    eprintln!("reader off: {:?}", started.elapsed());
    assert_eq!(
        evaluate(&mut client, "!!document.getElementById('blinkterm-reader')").as_bool(),
        Some(false)
    );
    assert_eq!(
        evaluate(&mut client, "document.adoptedStyleSheets.length").as_f64(),
        Some(sheets)
    );
    assert_eq!(
        evaluate(
            &mut client,
            "getComputedStyle(document.getElementById('nav')).display"
        )
        .as_str(),
        Some("block")
    );
    assert_eq!(scroll_y(&mut client), 300.0, "back where the page was");
    assert_eq!(
        toggle(&mut client, context, false, false),
        Some(Answered::Off),
        "off again is off"
    );

    assert_eq!(
        toggle(&mut client, context, true, false),
        Some(Answered::On)
    );
    evaluate(&mut client, "document.title='leaving'");
    client
        .call("Page.reload", Json::empty())
        .expect("the page reloads");
    assert_eq!(
        wait_for_title(&mut client, "ready", Duration::from_secs(10)),
        "ready"
    );
    assert_eq!(
        evaluate(&mut client, "!!document.getElementById('blinkterm-reader')").as_bool(),
        Some(false),
        "a reload is the page without the reader"
    );

    client.close();
    engine.kill();
}

/// A login page: nothing to read, nothing changed, and the row's sentence.
#[test]
fn a_page_with_no_article_is_left_alone_and_the_reader_says_so() {
    let Some((mut engine, mut client, context, _)) = reading("/none") else {
        return;
    };
    let sheets = evaluate(&mut client, "document.adoptedStyleSheets.length");
    assert_eq!(
        toggle(&mut client, context, true, false),
        Some(Answered::Nothing)
    );
    assert_eq!(
        evaluate(&mut client, "document.querySelectorAll('iframe').length").as_f64(),
        Some(0.0)
    );
    assert_eq!(
        evaluate(&mut client, "document.adoptedStyleSheets.length"),
        sheets
    );
    assert_eq!(reader::NOTHING, "no article on this page");

    client.close();
    engine.kill();
}

/// Find, and the hover, reach into the reader's frame as they reach into
/// any same-origin frame: the article's `quokka` is found and the hidden
/// footer's is not, and the link under the pointer is the absolute one.
#[test]
fn the_reader_document_is_searched_by_find_and_its_links_are_hovered_and_resolved() {
    let Some((mut engine, mut client, context, port)) = reading("/") else {
        return;
    };
    assert_eq!(search(&mut client, context, "quokka", 0).count, 2);
    search(&mut client, context, "", 0);
    assert_eq!(
        toggle(&mut client, context, true, false),
        Some(Answered::On)
    );
    assert_eq!(
        search(&mut client, context, "quokka", 0).count,
        1,
        "the article's, and not the footer's, which is hidden"
    );
    search(&mut client, context, "", 0);

    let centre = numbers(&evaluate(
        &mut client,
        "(function(){var f=document.getElementById('blinkterm-reader');\
         var a=f.contentDocument.querySelector('a[href]');a.scrollIntoView({block:'center'});\
         var o=f.getBoundingClientRect(),r=a.getBoundingClientRect();\
         return [o.left+r.left+r.width/2, o.top+r.top+r.height/2]})()",
    ));
    let (hover, _) = hover_at(&mut client, centre[0] as i32, centre[1] as i32);
    assert_eq!(hover.href, format!("http://localhost:{port}/more"));
    assert_eq!(hover.shape, hover::Shape::Pointer, "a hand");

    // Followed, the link loads in the tab, not in the frame; and back is the
    // page as it loads, without the reader.
    in_reader(&mut client, "d.querySelector('a[href]').click()");
    assert_eq!(
        wait_for_title(&mut client, "more", Duration::from_secs(10)),
        "more"
    );
    let history = client
        .call("Page.getNavigationHistory", Json::empty())
        .expect("the history");
    let current = history
        .get("currentIndex")
        .and_then(Json::as_i64)
        .expect("an index") as usize;
    let back = history
        .get("entries")
        .and_then(Json::as_array)
        .and_then(|entries| entries.get(current.checked_sub(1)?))
        .and_then(|entry| entry.get("id"))
        .and_then(Json::as_i64)
        .expect("an entry before");
    client
        .call(
            "Page.navigateToHistoryEntry",
            Json::object(vec![("entryId", Json::number(back as f64))]),
        )
        .expect("back");
    assert_eq!(
        wait_for_title(&mut client, "ready", Duration::from_secs(10)),
        "ready"
    );
    assert_eq!(
        evaluate(
            &mut client,
            "document.querySelectorAll('iframe').length + document.adoptedStyleSheets.length"
        )
        .as_f64(),
        Some(0.0),
        "the page came back without the reader"
    );

    client.close();
    engine.kill();
}

/// The reader takes the page's scheme: light by default, dark when the page
/// is told dark; and under `--alpha` it paints no background in either.
#[test]
fn the_reader_is_dark_when_the_page_is_told_dark_and_light_otherwise() {
    let Some((mut engine, mut client, context, _)) = reading("/") else {
        return;
    };
    assert_eq!(
        toggle(&mut client, context, true, false),
        Some(Answered::On)
    );
    let light = corner_luminance(&mut client);
    eprintln!("the reader, light: {light}");
    assert!(light > 0.9, "{light}");

    client
        .call(
            "Emulation.setEmulatedMedia",
            blinkterm::appearance::media_params(blinkterm::appearance::Scheme::Dark),
        )
        .expect("the scheme is told");
    assert!(prefers_dark(&mut client));
    let dark = wait_until(&mut client, true, |client| corner_luminance(client) < 0.184);
    eprintln!("the reader, dark: {}", corner_luminance(&mut client));
    assert!(dark, "the reader follows the page's scheme");

    assert_eq!(
        toggle(&mut client, context, false, false),
        Some(Answered::Off)
    );
    transparent(&mut client, false);
    assert_eq!(toggle(&mut client, context, true, true), Some(Answered::On));
    let corner = |client: &mut Client| {
        let (rgba, width, _) = still(client);
        rgba[(5 * width as usize + 5) * 4 + 3]
    };
    assert_eq!(wait_until(&mut client, 0, corner), 0, "see-through, dark");
    client
        .call(
            "Emulation.setEmulatedMedia",
            blinkterm::appearance::media_params(blinkterm::appearance::Scheme::Light),
        )
        .expect("the scheme is told");
    assert_eq!(wait_until(&mut client, 0, corner), 0, "and light");

    client.close();
    engine.kill();
}

// ---------------------------------------------------------------------------
// The console
// ---------------------------------------------------------------------------
//
// What `ctrl+shift+j` shows, recorded the way the program records it: a
// `console::Recorder` in front of the routing on the pipe's reader thread,
// and `Runtime.enable` and `Log.enable` on the page's session, as
// `app::prepare_session` sends them.

use blinkterm::console::{self as page_console, Level, Recorder, Source};

/// The console's hook on this engine and its two domains on this page's
/// session, as the program has them; the recorder, to read back.
fn consoled(engine: &Engine, client: &mut Client) -> Arc<Recorder> {
    let recorder = Arc::new(Recorder::new());
    engine.intercept(Some(
        Arc::clone(&recorder) as Arc<dyn blinkterm::cdp::Intercept>
    ));
    for method in ["Page.enable", "Runtime.enable", "Log.enable"] {
        client.call(method, Json::empty()).expect(method);
    }
    // What the engine's own first page said, replayed by the enable, as
    // `app::boot` forgets it.
    recorder.forget(client.session().expect("a page's session"));
    recorder
}

/// The entries on `session` once `count` of them are there, or whatever is
/// there when `timeout` runs out.
fn entries_when(
    recorder: &Recorder,
    session: &str,
    count: usize,
    timeout: Duration,
) -> Vec<page_console::Entry> {
    let deadline = Instant::now() + timeout;
    loop {
        let entries = recorder.entries(session);
        if entries.len() >= count || Instant::now() >= deadline {
            return entries;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The methods the console's domains send, none of which a mailbox should
/// ever hold.
const CONSOLE_EVENTS: [&str; 5] = [
    "Runtime.consoleAPICalled",
    "Runtime.exceptionThrown",
    "Log.entryAdded",
    "Runtime.executionContextCreated",
    "Runtime.executionContextDestroyed",
];

/// A page that says something in each of the ways a console hears: a
/// `console.log` of a string, a number and an object, an exception nobody
/// caught, an image the server answers 404 for and an image on a port
/// nobody listens on. One entry each, and none of it in the page's mailbox.
#[test]
fn a_page_that_logs_throws_and_404s_an_image_fills_the_console_with_one_entry_each() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let recorder = consoled(&engine, &mut client);
    let session = client.session().expect("a page's session").to_string();
    viewport(&mut client);
    let (base, closed) = serve_troubles();
    let broken = format!("{base}/404");
    let refused = format!("http://127.0.0.1:{closed}/x.png");
    let page = format!("{base}/console");
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(&page))]),
        )
        .expect("the page loads");
    assert_eq!(
        wait_for_title(&mut client, "done", Duration::from_secs(10)),
        "done"
    );
    let entries = entries_when(&recorder, &session, 4, Duration::from_secs(3));
    // Anything late would have come by now.
    std::thread::sleep(Duration::from_millis(300));
    let entries = if entries.len() < recorder.entries(&session).len() {
        recorder.entries(&session)
    } else {
        entries
    };
    for entry in &entries {
        eprintln!(
            "{:?} {:?} {:?} {:?}",
            entry.level, entry.source, entry.text, entry.place
        );
    }

    let logged: Vec<_> = entries
        .iter()
        .filter(|entry| entry.source == Source::Console)
        .collect();
    assert_eq!(logged.len(), 1, "{entries:?}");
    assert_eq!(logged[0].level, Level::Log);
    assert_eq!(logged[0].text, "hello 1 {a: 2}");
    assert_eq!(logged[0].place, format!("{page}:1"));

    let thrown: Vec<_> = entries
        .iter()
        .filter(|entry| entry.source == Source::Exception)
        .collect();
    assert_eq!(thrown.len(), 1, "{entries:?}");
    assert_eq!(thrown[0].level, Level::Error);
    assert!(
        thrown[0].text.starts_with("Uncaught Error: boom"),
        "{:?}",
        thrown[0].text
    );
    assert_eq!(thrown[0].place, format!("{page}:1"));

    let failed: Vec<_> = entries
        .iter()
        .filter(|entry| entry.source == Source::Network)
        .collect();
    let not_found: Vec<_> = failed
        .iter()
        .filter(|entry| entry.place == broken)
        .collect();
    assert_eq!(not_found.len(), 1, "{entries:?}");
    assert_eq!(not_found[0].level, Level::Error);
    assert!(not_found[0].text.contains("404"), "{:?}", not_found[0].text);
    let refusal: Vec<_> = failed
        .iter()
        .filter(|entry| entry.place == refused)
        .collect();
    assert_eq!(refusal.len(), 1, "{entries:?}");
    assert_eq!(refusal[0].level, Level::Error);
    assert!(
        refusal[0].text.contains("ERR_CONNECTION_REFUSED"),
        "{:?}",
        refusal[0].text
    );
    assert_eq!(
        entries.len(),
        4,
        "one entry each and nothing twice: {entries:?}"
    );

    let heard: Vec<String> = client
        .events()
        .into_iter()
        .map(|event| event.method)
        .filter(|method| CONSOLE_EVENTS.contains(&method.as_str()))
        .collect();
    assert!(heard.is_empty(), "the mailbox heard {heard:?}");

    assert_eq!(recorder.words(Some(&session)).as_deref(), Some("3 errors"));
    recorder.opened(&session);
    assert_eq!(recorder.words(Some(&session)), None);

    // Told again, as a revived page or a colour re-learn tells it: nothing
    // is reported twice.
    for method in ["Runtime.enable", "Log.enable"] {
        client.call(method, Json::empty()).expect(method);
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(recorder.entries(&session).len(), entries.len());
    client.close();
    engine.kill();
}

/// Two thousand lines logged by the document's own script, between its
/// landing and its load: without the recorder they are two thousand events
/// in a mailbox that keeps 512, and the landing would be the first to go.
#[test]
fn a_burst_of_two_thousand_logs_keeps_the_newest_thousand_and_loses_no_page_event() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let recorder = consoled(&engine, &mut client);
    let session = client.session().expect("a page's session").to_string();
    viewport(&mut client);
    let mut tab = Tab::new("t", client, "about:blank");
    let page = "data:text/html,<title>burst</title><script>\
for(var i=0;i<2000;i++){console.log('line '+i)}</script>";
    navigate_tab(&mut tab, page);
    let landings = follow(&mut tab, Duration::from_secs(10));
    assert!(!landings.is_empty(), "the landing came");
    let entries = entries_when(
        &recorder,
        &session,
        page_console::CAP,
        Duration::from_secs(3),
    );
    assert_eq!(entries.len(), page_console::CAP);
    assert_eq!(entries[0].text, "line 1000");
    assert_eq!(
        entries.last().map(|entry| entry.text.as_str()),
        Some("line 1999")
    );
    tab.connection.close();
    engine.kill();
}

/// Whether a page can tell its console is being listened to — the checks
/// pages use to find an open DevTools: a getter that notes it was read, on
/// an error's `stack` (a), an element's `id` and `className` (b), a plain
/// accessor (c), a proxy's traps (d), `Symbol.toStringTag` (e), a regexp's
/// `toString` (f), an object's `toString` and `valueOf` under `%s` and `%d`
/// (g). Measured with the two domains off and on; what it finds is what
/// `docs/design.md` says. The time is printed and not held to anything: it
/// is the one difference, and it is noise-sized.
#[test]
fn no_getter_a_page_sets_tells_it_the_console_is_heard() {
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    let page = "data:text/html,<title>start</title><script>\
var seen='';function mark(c){if(seen.indexOf(c)<0)seen+=c}\
var e=new Error('x');\
Object.defineProperty(e,'stack',{get:function(){mark('a');return 'x'}});\
console.log(e);\
var d=document.createElement('div');\
Object.defineProperty(d,'id',{get:function(){mark('b');return 'x'}});\
Object.defineProperty(d,'className',{get:function(){mark('b');return 'x'}});\
console.log(d);\
console.log({get x(){mark('c');return 1}});\
console.log(new Proxy({},{get:function(){mark('d')},ownKeys:function(){mark('d');return []},\
getOwnPropertyDescriptor:function(){mark('d')},getPrototypeOf:function(){mark('d');return null}}));\
var t={};Object.defineProperty(t,Symbol.toStringTag,{get:function(){mark('e');return 'T'}});\
console.log(t);\
var r=/a/;r.toString=function(){mark('f');return ''};console.log(r);\
var o={toString:function(){mark('g');return ''},valueOf:function(){mark('g');return 1}};\
console.log(o);console.log('%s',o);console.log('%d',o);\
var big=[];for(var i=0;i<1000;i++){big.push({i:i,s:'text',n:[1,2,3]})}\
var t0=performance.now();for(var j=0;j<200;j++){console.log(big)}\
var took=Math.round(performance.now()-t0);\
setTimeout(function(){document.title='seen ['+seen+'] '+took+' ms'},200)</script>";
    let look = |client: &mut Client| {
        client
            .call(
                "Page.navigate",
                Json::object(vec![("url", Json::string(page))]),
            )
            .expect("the page loads");
        wait_for_title(client, "seen", Duration::from_secs(10))
    };
    let off = look(&mut client);
    let recorder = Arc::new(Recorder::new());
    engine.intercept(Some(
        Arc::clone(&recorder) as Arc<dyn blinkterm::cdp::Intercept>
    ));
    for method in ["Runtime.enable", "Log.enable"] {
        client.call(method, Json::empty()).expect(method);
    }
    let on = look(&mut client);
    eprintln!("the console not heard: {off:?}; heard: {on:?}");
    let marks = |title: &str| {
        title
            .split(['[', ']'])
            .nth(1)
            .map(str::to_string)
            .unwrap_or_else(|| panic!("the page did not finish: {title:?}"))
    };
    // Blink's own formatting reads these whoever is listening.
    assert_eq!(marks(&off), "efg");
    assert_eq!(marks(&on), marks(&off), "a getter told the page");
    client.close();
    engine.kill();
}

/// A message from the page with an escape sequence, a bell and a direction
/// override in it, through the recorder and the panel's rows into the
/// compositor's own terminal: letters, and nothing a terminal would do.
#[test]
fn a_console_message_with_an_escape_sequence_cannot_reach_the_terminal() {
    use blinkterm::screen::{self, ListItem};
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    let recorder = consoled(&engine, &mut client);
    let session = client.session().expect("a page's session").to_string();
    let page = "data:text/html,<title>start</title><script>\
console.log(String.fromCharCode(27)+']0;pwned'+String.fromCharCode(7)\
+String.fromCharCode(0x202e)+'moc');document.title='done'</script>";
    client
        .call(
            "Page.navigate",
            Json::object(vec![("url", Json::string(page))]),
        )
        .expect("the page loads");
    wait_for_title(&mut client, "done", Duration::from_secs(10));
    let entries = entries_when(&recorder, &session, 1, Duration::from_secs(3));
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].text, "]0;pwnedmoc");
    let items = [ListItem {
        lead: entries[0].lead(),
        title: &entries[0].text,
        url: &entries[0].place,
        picked: true,
    }];
    let rows = screen::list_rows(80, 2, 3, &items);
    let text = a_terminal_reads_only_text_in_rows(&rows);
    assert!(text.starts_with("  log  ]0;pwnedmoc"), "{text:?}");
    client.close();
    engine.kill();
}

/// [`a_terminal_reads_only_text_in`] for the rows under the status row: no
/// title set, no question answered, and nothing below a space between the
/// rows' own framing. Returns what the first of them reads as.
fn a_terminal_reads_only_text_in_rows(rows: &[u8]) -> String {
    let mut terminal = tos_term::Terminal::new(80, 24, tos_term::TerminalConfig::default());
    terminal.advance(&blinkterm::screen::enter_sequence());
    let _ = terminal.take_output();
    terminal.advance(rows);
    assert_eq!(
        terminal.title(),
        "",
        "the rows set the window title: {rows:?}"
    );
    assert!(
        terminal.take_output().is_empty(),
        "the rows asked the terminal something: {rows:?}"
    );
    let mut body = String::from_utf8_lossy(rows)
        .replace("\x1b[7m", "")
        .replace("\x1b[0m", "");
    for row in 1..=24 {
        body = body.replace(&format!("\x1b[{row};1H\x1b[K"), "");
    }
    assert!(body.bytes().all(|b| b >= 0x20 && b != 0x7f), "{body:?}");
    terminal.grid().row(1).to_text()
}

/// What recording the console costs a page, from its landing to its load:
/// a page of links and a page that logs five hundred lines, with the two
/// domains off and on, alternately, three times each. Printed, and held to
/// no worse than twice as slow, which is loose on purpose: the assertion is
/// that nobody would see it, and the numbers are for the docs.
#[test]
fn the_console_costs_a_heavy_page_nothing_a_person_can_see() {
    if skip_timing_on_shared_runner("the_console_costs_a_heavy_page_nothing_a_person_can_see") {
        return;
    }
    let Some((mut engine, mut client)) = connect() else {
        return;
    };
    client
        .call("Page.enable", Json::empty())
        .expect("Page.enable");
    viewport(&mut client);
    let recorder = Arc::new(Recorder::new());
    engine.intercept(Some(
        Arc::clone(&recorder) as Arc<dyn blinkterm::cdp::Intercept>
    ));
    let (base, _) = serve_troubles();
    let links = format!("{base}/links");
    let loud = "data:text/html,<title>loud</title><script>\
for(var i=0;i<500;i++){console.log('line',i,{i:i,s:'some text'})}</script>";
    let mut tab = Tab::new("t", client, "about:blank");
    let mut times = [[Duration::MAX; 2]; 2];
    for round in 0..6 {
        let on = round % 2 == 1;
        for method in if on {
            ["Runtime.enable", "Log.enable"]
        } else {
            ["Runtime.disable", "Log.disable"]
        } {
            tab.connection.call(method, Json::empty()).expect(method);
        }
        for (which, url) in [links.as_str(), loud].into_iter().enumerate() {
            let started = Instant::now();
            navigate_tab(&mut tab, url);
            follow(&mut tab, Duration::from_secs(10));
            let took = started.elapsed();
            let best = &mut times[which][usize::from(on)];
            *best = (*best).min(took);
        }
    }
    for (which, name) in ["links", "500 lines"].into_iter().enumerate() {
        let [off, on] = times[which];
        eprintln!("{name}: {off:?} with the console off, {on:?} with it on");
        assert!(
            on <= off * 2 + Duration::from_millis(20),
            "{name}: {on:?} against {off:?}"
        );
    }
    tab.connection.close();
    engine.kill();
}

/// The console as `app::boot` installs it, beside the blocker: the engine's
/// own first page leaves nothing behind (on a Mac it is a directory listing
/// whose script throws, and the enable replays that), a page's requests the
/// blocker fails are in the console as what they are, and the blocker's
/// count is untouched by the hook in front of it.
#[test]
fn the_console_booted_beside_the_blocker_starts_empty_and_hears_what_was_blocked() {
    if !engine_named() {
        return;
    }
    let port = serve_pages(|port| {
        let mut pages = vec![("/".to_string(), page_with_ads(port))];
        pages.extend((0..6).map(|i| (format!("/s{i}.js"), "0;".to_string())));
        pages
    });
    let page = format!("http://127.0.0.1:{port}/");
    let mut hosts = std::collections::HashSet::new();
    block::parse("0.0.0.0 ads.test\n", &mut hosts);
    let blocker = Arc::new(Blocker::new(hosts, Vec::new()));
    let recorder = Arc::new(Recorder::new());
    let Booted {
        mut engine,
        browser,
        mut tabs,
        identity,
    } = booted_with(Some(&blocker), Some(&recorder));
    assert!(identity.console, "the sessions are told to report");
    let first = tabs.active_mut().expect("the first tab");
    let session = first.connection.session().expect("a page").to_string();
    assert!(
        recorder.entries(&session).is_empty(),
        "the engine's own page: {:?}",
        recorder.entries(&session)
    );
    assert_eq!(recorder.words(Some(&session)), None);

    assert_eq!(
        load_titled(&mut first.connection, &page, "done"),
        "done ok=3 er=3"
    );
    assert_eq!(blocker.words(Some(&session)).as_deref(), Some("3 blocked"));
    let entries = entries_when(&recorder, &session, 3, Duration::from_secs(3));
    let blocked: Vec<_> = entries
        .iter()
        .filter(|entry| entry.source == Source::Network && entry.text.contains("BLOCKED_BY_CLIENT"))
        .collect();
    assert_eq!(blocked.len(), 3, "{entries:?}");
    assert!(blocked.iter().all(|entry| entry.place.contains("ads.test")));
    assert_eq!(recorder.words(Some(&session)).as_deref(), Some("3 errors"));
    let heard: Vec<String> = first
        .connection
        .events()
        .into_iter()
        .map(|event| event.method)
        .filter(|method| CONSOLE_EVENTS.contains(&method.as_str()))
        .collect();
    assert!(heard.is_empty(), "the mailbox heard {heard:?}");
    drop(tabs);
    drop(browser);
    engine.kill();
}
