//! What is under the pointer: where a link goes, and what the pointer should
//! look like.
//!
//! A link's text and its href are two different things, and the page chooses
//! both. A desktop browser shows the href in a corner when the pointer is on
//! the link, and that corner is the one defence a person has against a link
//! whose words say one place and whose href says another. The status row is
//! this program's corner, so this module asks the page what is under the
//! pointer and hands the loop an answer it can put there: the href, resolved
//! by the engine and made plain text ([`crate::text::sanitize`]), and a
//! pointer shape chosen from a table this program owns.
//!
//! # How the page is asked, and what that costs
//!
//! One `Runtime.evaluate` of [`ASK`] — `document.elementFromPoint`, the
//! nearest `a[href]` above it, and the element's computed `cursor` — with
//! `returnByValue`, on the footing [`crate::load::LOADED`] already stands on:
//! `Page.enable` and no other domain. Measured against `chrome-headless-shell`
//! 153:
//!
//! ```text
//!                                                      min      median  p90      max
//! static page, page idle, n=60                         0.33 ms  0.40 ms 0.52 ms  0.57 ms
//! animating page, JPEG screencast running, n=60        0.74 ms  1.01 ms 1.25 ms  5.06 ms
//! ```
//!
//! Beside the second row the screencast delivered 64 frames in 1035 ms, so an
//! ask every sixteen milliseconds costs the frame rate nothing measurable; the
//! [`Tracker`] keeps it to one in flight and at most one every [`ASK_EVERY`]
//! while the pointer moves, and none while it rests. What it answers, per
//! element: a `<b>` inside an `<a>` is the anchor's resolved href (`closest`
//! walks up); `javascript:` and `mailto:` come back as written; an SVG `<a>`,
//! whose `href` is an `SVGAnimatedString`, is its `baseVal` resolved against
//! the document; `#frag` is the page's url and the fragment; an `<a>` without
//! an `href` is not a link; a `<div>` with `cursor: pointer` is no link but a
//! hand; an `<input>` is an I-beam. A hostile href —
//! `http://<U+202E>evil.example/ESC]0;xBEL` — came back from the engine as
//! `http://xn--evil-uga52ak87q.example/%1B]0;x`, the host already IDNA-encoded
//! and the control byte percent-encoded, and it goes through the sanitizer
//! regardless: the engine's spelling is the engine's business, and what
//! reaches the row is this program's.
//!
//! `elementFromPoint` from the top document stops at an `<iframe>`. The ask
//! descends into a frame whose document it can reach — a same-origin one,
//! where a site's own navigation often is — by subtracting the frame's
//! rectangle and asking again inside; a cross-origin frame throws on
//! `contentDocument` and stays the frame. Eight levels, which is more nesting
//! than any page anybody reads.
//!
//! # What was measured and not used
//!
//! `DOM.getNodeForLocation` answers in 0.3 ms, but with a `backendNodeId` and
//! nothing else: the href is a second round trip and a domain that is not
//! otherwise paid for. A binding — `Runtime.addBinding` and a `mousemove`
//! listener that calls it — reports 5 ms after the move and coalesces nicely,
//! but it needs `Runtime.enable`, and on a page that logs every ten
//! milliseconds that is 200 `Runtime.consoleAPICalled` and 59.5 kB of JSON in
//! 2.6 s through the tab's mailbox, which drops the oldest at 512 — and what
//! it would drop is frames and `Page.frameNavigated`. A hover url is not worth
//! the load events. So the page is asked, and asked sparingly.
//!
//! # The one measurement that shapes the rest
//!
//! While a cross-document navigation is pending — typed, or a link clicked
//! into a server that has not answered — the engine holds every command meant
//! for the page's renderer. A `Runtime.evaluate` sent during a hang was not
//! answered in ten seconds, and was answered only after `Page.stopLoading`,
//! with the old document's answer. (`Page.captureScreenshot` and
//! `Page.getNavigationHistory` are not held.) So the ask is never a `call`,
//! which would freeze the loop on every slow host; it is not sent while a
//! navigation has not committed ([`crate::tabs::Tab::committed`]); and one
//! that is out when the page leaves or lands is dropped.
//!
//! # The pointer's shape
//!
//! A terminal that understands OSC 22 — Kitty since 0.31, Ghostty — can be
//! told the pointer should be a hand or an I-beam, and every other terminal
//! drops an OSC it does not know. What is written is never the page's string:
//! the page's `cursor` value only chooses among the names of [`Shape`], a
//! closed set, and anything outside it — `url(…)`, `auto`, a typo, an escape
//! sequence — is the arrow.

use std::time::{Duration, Instant};

use crate::json::Json;
use crate::text;

/// What is under the pointer, as the page answers it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hover {
    /// The link's resolved href, plain text; empty when there is no link.
    pub href: String,
    /// The pointer the page asked for, mapped to one this program will name.
    pub shape: Shape,
}

/// The pointer shapes this program will ever name to a terminal.
///
/// A closed set on purpose: the name goes to the terminal inside an OSC, and
/// the CSS `cursor` value it is derived from is the page's. These are the
/// CSS names that Kitty's pointer-shape protocol and Ghostty share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shape {
    #[default]
    Default,
    Pointer,
    Text,
    Crosshair,
    Move,
    Grab,
    Grabbing,
    NotAllowed,
    Wait,
    Progress,
    ColResize,
    RowResize,
    NsResize,
    EwResize,
    NeswResize,
    NwseResize,
}

impl Shape {
    /// Every shape, for the tests that check each name.
    pub const ALL: [Shape; 16] = [
        Shape::Default,
        Shape::Pointer,
        Shape::Text,
        Shape::Crosshair,
        Shape::Move,
        Shape::Grab,
        Shape::Grabbing,
        Shape::NotAllowed,
        Shape::Wait,
        Shape::Progress,
        Shape::ColResize,
        Shape::RowResize,
        Shape::NsResize,
        Shape::EwResize,
        Shape::NeswResize,
        Shape::NwseResize,
    ];

    /// The name a terminal is told: a `&'static str` out of this table and
    /// nothing else.
    pub fn name(self) -> &'static str {
        match self {
            Shape::Default => "default",
            Shape::Pointer => "pointer",
            Shape::Text => "text",
            Shape::Crosshair => "crosshair",
            Shape::Move => "move",
            Shape::Grab => "grab",
            Shape::Grabbing => "grabbing",
            Shape::NotAllowed => "not-allowed",
            Shape::Wait => "wait",
            Shape::Progress => "progress",
            Shape::ColResize => "col-resize",
            Shape::RowResize => "row-resize",
            Shape::NsResize => "ns-resize",
            Shape::EwResize => "ew-resize",
            Shape::NeswResize => "nesw-resize",
            Shape::NwseResize => "nwse-resize",
        }
    }
}

/// The CSS `cursor` a page computed, as one of [`Shape`]'s, or the arrow.
///
/// A custom cursor is `url(…) x y, fallback`, and the fallback — the last
/// comma-separated keyword — is what is read. The one-sided resizes are
/// folded into the two-sided ones, which is what a terminal's pointer theme
/// draws for either anyway.
pub fn shape(css: &str) -> Shape {
    let keyword = css.rsplit(',').next().unwrap_or_default().trim();
    match keyword.to_ascii_lowercase().as_str() {
        "pointer" => Shape::Pointer,
        "text" | "vertical-text" => Shape::Text,
        "crosshair" => Shape::Crosshair,
        "move" | "all-scroll" => Shape::Move,
        "grab" => Shape::Grab,
        "grabbing" => Shape::Grabbing,
        "not-allowed" | "no-drop" => Shape::NotAllowed,
        "wait" => Shape::Wait,
        "progress" => Shape::Progress,
        "col-resize" => Shape::ColResize,
        "row-resize" => Shape::RowResize,
        "ns-resize" | "n-resize" | "s-resize" => Shape::NsResize,
        "ew-resize" | "e-resize" | "w-resize" => Shape::EwResize,
        "nesw-resize" | "ne-resize" | "sw-resize" => Shape::NeswResize,
        "nwse-resize" | "nw-resize" | "se-resize" => Shape::NwseResize,
        _ => Shape::Default,
    }
}

/// The question, as a function of the point; [`ask`] applies it.
///
/// The frame walk subtracts each frame's rectangle and border, so the point
/// asked of the inner document is in its own coordinates. `new URL` is for
/// the SVG branch, whose `baseVal` is written rather than resolved; an HTML
/// anchor's `href` is already absolute and comes through it unchanged.
pub const ASK: &str = "(function(x,y){var d=document,e=null,o=[0,0];\
for(var i=0;i<8;i++){e=d.elementFromPoint(x-o[0],y-o[1]);if(!e)break;\
if(e.tagName==='IFRAME'||e.tagName==='FRAME'){var c=null;try{c=e.contentDocument}catch(_){}\
if(!c)break;var r=e.getBoundingClientRect();o=[o[0]+r.left+e.clientLeft,o[1]+r.top+e.clientTop];d=c;continue}\
break}\
if(!e)return['',''];var a=e.closest('a[href]'),h='';\
if(a){h=a.href;if(h&&typeof h!=='string'){h=h.baseVal||''}try{h=new URL(h,d.baseURI).href}catch(_){}}\
var s='';try{s=getComputedStyle(e).cursor}catch(_){}return[h,s]})";

/// The `Runtime.evaluate` parameters that ask about `(x, y)`, in the page's
/// CSS pixels. `returnByValue`, without which the pair comes back as a handle
/// to an array in the page.
pub fn ask(x: i32, y: i32) -> Json {
    Json::object(vec![
        ("expression", Json::string(format!("{ASK}({x},{y})"))),
        ("returnByValue", Json::Bool(true)),
    ])
}

/// Read the answer to [`ask`].
///
/// `None` for anything that is not a pair of strings: an exception, or a page
/// that replaced `document.elementFromPoint` with something of its own. The
/// href comes out as plain text — it is the page's, and it is going on the
/// row — and the cursor comes out as one of [`Shape`]'s and never as itself.
pub fn answer(reply: &Json) -> Option<Hover> {
    let value = reply.path(&["result", "value"])?.as_array()?;
    if value.len() != 2 {
        return None;
    }
    let href = value[0].as_str()?;
    let cursor = value[1].as_str()?;
    Some(Hover {
        href: text::sanitize(href).trim().to_string(),
        shape: shape(cursor),
    })
}

/// What the row says for a link under the pointer.
pub fn words(href: &str) -> String {
    format!("link: {href}")
}

/// How often the page is told the pointer moved: the scroll thread's tick,
/// which is as often as the page can paint a hover anyway.
pub const TELL_EVERY: Duration = Duration::from_millis(16);

/// How often, at most, the page is asked what is under the pointer while it
/// moves: 25 asks a second, about 25 ms of renderer a second on the
/// animating page measured above.
pub const ASK_EVERY: Duration = Duration::from_millis(40);

/// How long an ask may be out before it is given up on. The page is not
/// stopped behind a dialog (none is sent then) and has committed (none is
/// sent before), so an answer that has not come in a second is a page busy
/// with something else, and the pointer will have moved by the time it came.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(1);

/// How far the pointer has to move before the page is asked again: half a
/// cell in pixel mode, so that a pointer wandering inside one cell is one
/// ask. In cell mode every report is a cell apart, which is further.
pub const ASK_STEP: i32 = 4;

/// The pointer's whereabouts and what has been asked about them, for the tab
/// in front.
///
/// Pure: positions and instants in, positions and answers out. The loop
/// decides what is on the wire, and this decides how much.
#[derive(Debug, Default)]
pub struct Tracker {
    /// The last report inside the page, in CSS pixels, and its modifiers.
    latest: Option<((i32, i32), u32)>,
    /// The last position the page was told, and when.
    told: Option<(i32, i32)>,
    told_at: Option<Instant>,
    /// The position the last ask was about, and when it went.
    asked: Option<(i32, i32)>,
    asked_at: Option<Instant>,
    /// What the row and the terminal currently reflect.
    shown: Hover,
    /// The page moved under a still pointer, so the last answer is old.
    stale: bool,
}

impl Tracker {
    /// A report inside the page: a bare motion, or a press.
    pub fn moved(&mut self, at: (i32, i32), modifiers: u32) {
        self.latest = Some((at, modifiers));
    }

    /// The pointer is no longer over this page: it is on the row, another tab
    /// is in front, the page has left or arrived, the row has been taken by
    /// something being typed, or the pane changed size. What was shown is
    /// forgotten; `true` when that changed what the row says.
    pub fn left(&mut self) -> bool {
        let changed = self.shown != Hover::default();
        self.latest = None;
        self.told = None;
        self.asked = None;
        self.shown = Hover::default();
        self.stale = false;
        changed
    }

    /// The page moved under the pointer — a wheel, a zoom — so what is under
    /// it is asked again once, however little the pointer itself has moved.
    pub fn scrolled(&mut self) {
        self.stale = true;
    }

    /// Where to tell the page the pointer is, if it has moved since the page
    /// was last told and [`TELL_EVERY`] has passed: one `mouseMoved` per
    /// pass at most, and the last position wins. With the modifiers of the
    /// report it came from.
    pub fn tell(&mut self, now: Instant) -> Option<((i32, i32), u32)> {
        let (at, modifiers) = self.latest?;
        if self.told == Some(at) {
            return None;
        }
        if self
            .told_at
            .is_some_and(|told| now.saturating_duration_since(told) < TELL_EVERY)
        {
            return None;
        }
        self.told = Some(at);
        self.told_at = Some(now);
        Some((at, modifiers))
    }

    /// Where to ask the page about, if anywhere: nothing while an ask is in
    /// flight or within [`ASK_EVERY`] of the last; otherwise the latest
    /// position if it is [`ASK_STEP`] from the last one asked about, or the
    /// page has moved under it.
    pub fn wants_ask(&self, now: Instant, in_flight: bool) -> Option<(i32, i32)> {
        if in_flight {
            return None;
        }
        let (at, _) = self.latest?;
        if self
            .asked_at
            .is_some_and(|asked| now.saturating_duration_since(asked) < ASK_EVERY)
        {
            return None;
        }
        if self.stale {
            return Some(at);
        }
        match self.asked {
            None => Some(at),
            Some((x, y)) => {
                let far = (at.0 - x).abs().max((at.1 - y).abs()) >= ASK_STEP;
                far.then_some(at)
            }
        }
    }

    /// An ask about `at` went out at `now`. Recorded whether or not it is
    /// answered, so that a page that does not answer is not asked about the
    /// same place again until the pointer moves.
    pub fn asked(&mut self, at: (i32, i32), now: Instant) {
        self.asked = Some(at);
        self.asked_at = Some(now);
        self.stale = false;
    }

    /// The answer to the ask about `at`. `true` when it changed what is
    /// shown. An answer that arrives after the pointer has left is dropped:
    /// it is about a page the pointer is not on.
    pub fn answered(&mut self, at: (i32, i32), hover: Hover) -> bool {
        if self.latest.is_none() || self.asked != Some(at) {
            return false;
        }
        if self.shown == hover {
            return false;
        }
        self.shown = hover;
        true
    }

    /// What the row and the terminal should currently reflect.
    pub fn shown(&self) -> &Hover {
        &self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Json {
        Json::parse(text).expect("the test's own JSON")
    }

    fn reply(value: &str) -> Option<Hover> {
        answer(&json(&format!(
            r#"{{"result":{{"type":"object","value":{value}}}}}"#
        )))
    }

    fn link(href: &str) -> Hover {
        Hover {
            href: href.to_string(),
            shape: Shape::Pointer,
        }
    }

    #[test]
    fn the_answer_is_a_link_and_a_shape_and_nothing_else_is_no_answer() {
        assert_eq!(reply(r#"["http://x/","pointer"]"#), Some(link("http://x/")));
        assert_eq!(reply(r#"["",""]"#), Some(Hover::default()));
        assert_eq!(
            reply(r#"["","text"]"#),
            Some(Hover {
                href: String::new(),
                shape: Shape::Text
            })
        );
        assert_eq!(reply(r#""just a string""#), None);
        assert_eq!(reply(r#"["only one"]"#), None);
        assert_eq!(reply(r#"["a","b","c"]"#), None);
        assert_eq!(reply(r#"[1,2]"#), None);
        assert_eq!(answer(&json(r#"{"exceptionDetails":{}}"#)), None);
    }

    #[test]
    fn a_hostile_href_is_its_letters() {
        // What the engine actually answered for the hostile link: host
        // IDNA-encoded, control byte percent-encoded. Already plain.
        assert_eq!(
            reply(r#"["http://xn--evil-uga52ak87q.example/%1B]0;x","pointer"]"#),
            Some(link("http://xn--evil-uga52ak87q.example/%1B]0;x"))
        );
        // And the raw spelling, as a page that got past the engine would
        // send it: an escape, a bidi override, zero-width spaces.
        assert_eq!(
            reply(r#"["http://\u202eevil.example/\u001b]0;x\u0007\u200b","pointer"]"#),
            Some(link("http://evil.example/]0;x"))
        );
    }

    #[test]
    fn a_cursor_the_terminal_does_not_know_is_the_arrow() {
        assert_eq!(shape("pointer"), Shape::Pointer);
        assert_eq!(shape("text"), Shape::Text);
        assert_eq!(shape("grab"), Shape::Grab);
        assert_eq!(shape(" Pointer "), Shape::Pointer);
        assert_eq!(
            shape("url(x.cur) 4 4, pointer"),
            Shape::Pointer,
            "a custom cursor is its fallback"
        );
        for unknown in ["auto", "none", "zoom-in", "", "url(x.cur)", "\x1b]0;x\x07"] {
            assert_eq!(shape(unknown), Shape::Default, "{unknown:?}");
        }
        for each in Shape::ALL {
            let name = each.name();
            assert!(
                !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'),
                "{name:?}"
            );
            assert_eq!(shape(name), each, "every name reads back as itself");
        }
    }

    #[test]
    fn one_ask_at_a_time_and_the_last_position_wins() {
        let start = Instant::now();
        let mut tracker = Tracker::default();
        tracker.moved((10, 10), 0);
        tracker.moved((20, 10), 0);
        tracker.moved((30, 10), 4);
        assert_eq!(tracker.tell(start), Some(((30, 10), 4)), "the latest");
        tracker.moved((40, 10), 0);
        assert_eq!(
            tracker.tell(start + Duration::from_millis(5)),
            None,
            "not twice within a tick"
        );
        assert_eq!(
            tracker.tell(start + TELL_EVERY),
            Some(((40, 10), 0)),
            "and then the latest again"
        );
        assert_eq!(tracker.tell(start + TELL_EVERY * 3), None, "not moved");

        assert_eq!(tracker.wants_ask(start, false), Some((40, 10)));
        assert_eq!(tracker.wants_ask(start, true), None, "one in flight");
        tracker.asked((40, 10), start);
        tracker.moved((80, 10), 0);
        let later = start + ASK_EVERY;
        assert_eq!(
            tracker.wants_ask(start + Duration::from_millis(10), false),
            None
        );
        // The answer about where the pointer was is still an answer, and
        // the place it is now is still wanted.
        assert!(tracker.answered((40, 10), link("http://a/")));
        assert_eq!(tracker.shown(), &link("http://a/"));
        assert_eq!(tracker.wants_ask(later, false), Some((80, 10)));
        // An answer for a position not asked about is not taken.
        assert!(!tracker.answered((1, 1), link("http://b/")));
        assert_eq!(tracker.shown(), &link("http://a/"));
    }

    #[test]
    fn a_pointer_inside_one_cell_is_not_asked_again_and_a_scroll_asks_once() {
        let start = Instant::now();
        let mut tracker = Tracker::default();
        tracker.moved((100, 100), 0);
        tracker.asked((100, 100), start);
        let later = start + ASK_EVERY * 2;
        tracker.moved((102, 97), 0);
        assert_eq!(tracker.wants_ask(later, false), None, "inside the step");
        tracker.moved((100 + ASK_STEP, 100), 0);
        assert_eq!(tracker.wants_ask(later, false), Some((104, 100)));

        tracker.moved((100, 100), 0);
        tracker.asked((100, 100), later);
        let after = later + ASK_EVERY;
        assert_eq!(tracker.wants_ask(after, false), None);
        tracker.scrolled();
        assert_eq!(tracker.wants_ask(after, false), Some((100, 100)));
        tracker.asked((100, 100), after);
        assert_eq!(
            tracker.wants_ask(after + ASK_EVERY, false),
            None,
            "a scroll asks once"
        );
    }

    #[test]
    fn leaving_clears_what_was_shown() {
        let now = Instant::now();
        let mut tracker = Tracker::default();
        assert!(!tracker.left(), "nothing shown, nothing changed");
        tracker.moved((5, 5), 0);
        tracker.asked((5, 5), now);
        assert!(tracker.answered((5, 5), link("http://a/")));
        assert!(
            !tracker.answered((5, 5), link("http://a/")),
            "the same answer changes nothing"
        );
        assert!(tracker.left());
        assert_eq!(tracker.shown(), &Hover::default());
        assert_eq!(tracker.wants_ask(now + ASK_EVERY, false), None);
        assert_eq!(tracker.tell(now + TELL_EVERY), None);
        // An answer that comes back after the pointer left is not shown.
        assert!(!tracker.answered((5, 5), link("http://a/")));
        assert_eq!(tracker.shown(), &Hover::default());
    }

    #[test]
    fn the_words_are_the_link_and_a_label() {
        assert_eq!(words("https://a/"), "link: https://a/");
    }

    #[test]
    fn the_question_is_asked_by_value_about_the_point_given() {
        let params = ask(12, -3);
        let expression = params
            .get("expression")
            .and_then(Json::as_str)
            .expect("an expression");
        assert!(expression.starts_with(ASK));
        assert!(expression.ends_with("(12,-3)"), "{expression}");
        assert_eq!(
            params.get("returnByValue").and_then(Json::as_bool),
            Some(true)
        );
    }
}
