//! Link hints: the clickable things in view, labelled by the page from a
//! world of its own, and clicked by typing the label
//! ([#13](https://github.com/m96-chan/blinkterm/issues/13)).
//!
//! `f` in normal mode ([`crate::normal`]) asks the page in front what can be
//! clicked. [`SCRIPT`] takes `a[href]`, buttons, inputs that are not hidden,
//! selects, textareas, `summary`, `details`, labels, anything with `onclick`,
//! `contenteditable`, `tabindex` or a `role` a person clicks (link, button,
//! checkbox, radio, tab, the menu items, option, switch, treeitem, combobox,
//! slider, textbox), media with controls — and an element whose computed
//! `cursor` is `pointer` when its parent's is not and nothing inside it is one
//! of the above: the root of a region that is clickable by CSS alone, and not
//! a card around a link, whose link gets the hint instead. Open shadow roots
//! are walked; same-origin frames are entered with their offset and clipped to
//! what of the frame is on the screen. Of those, a hint is what
//! `checkVisibility` says is visible, has a rectangle at least 2x2 inside the
//! viewport, and is what `elementFromPoint` at the visible part's centre
//! answers — the element, something inside it, something around it, or its
//! `<label>` — so a link under a cookie banner is not labelled for a click
//! that would land on the banner. Measured against `chrome-headless-shell`
//! 153 on a page of every kind above: 18 hints at 640x360, 27 at 640x720,
//! and none for `display:none`, `visibility:hidden`, `opacity:0`, a closed
//! `<details>`, a covered link, a disabled button, a cross-origin frame or an
//! image map's `<area>`, which Chromium gives no rectangle at all.
//!
//! The labels are drawn by the page. `show` appends one `<blinkterm-hints>`
//! element to `<html>` — not `<body>`, whose `innerHTML` stays byte for byte
//! what it was — styled `all:initial`, fixed, at the top of every stacking
//! context and deaf to the pointer, with a *closed* shadow root holding the
//! labels, and `clear` takes it away. That is one mutation a page can see: a
//! `MutationObserver` on `<html>` gets two records, and a page that removes
//! the element loses the labels until the next key draws them again. What it
//! cannot see is the labels themselves, the script's state (which lives in
//! the isolated world, [`crate::find::WORLD`], shared with find under a name
//! of its own), its focus or its selection. The other route was a second
//! Kitty placement over the frame, and it was measured and turned down: a
//! glyph table in this crate, a CSS-to-terminal mapping nothing else needs, a
//! full-viewport transmission per `f`, and labels that do not follow a frame
//! inside the page. The page's own labels cost 2 ms, land in the screencast
//! and the still through the pipeline that exists, and are right at every
//! zoom because they are sized from the cell: `px` is how many CSS pixels a
//! terminal cell is tall, and a label's font is seven tenths of it.
//!
//! [`SCRIPT`] goes whole as the `functionDeclaration` of each
//! `Runtime.callFunctionOn`, with its arguments by value, as find's does:
//! nothing is installed and nothing is interpolated into source. `collect`
//! is the walk — 2 to 12 ms on the test page, 30 to 60 on four thousand
//! elements, about half a second on forty thousand, because it is
//! `getComputedStyle` on every element that is not a candidate by selector —
//! so it is sent and its answer collected on a later pass; `show`, `narrow`
//! and `clear` answer nothing anybody needs and are told.
//!
//! A hint is clicked by three `Input.dispatchMouseEvent`s at the centre the
//! page reported ([`click_params`]), which is a real click with all of a real
//! click's side effects — focus, `:active`, the page's `mousedown` handlers —
//! and not `el.click()`. The point is already in the page's pixels and goes
//! through no mapping here. `F` does not ctrl+click, which would hand the
//! choice to whatever click listener the page has: it opens the link's href
//! through this program's own `Target.createTarget`,
//! [`crate::app::open_behind`], in a tab behind the one in front — where a
//! middle click on the same link would put it.
//!
//! The hrefs are the only strings read off the page, and they go to the
//! engine, never to the terminal: the row says a count.

use crate::input::{Key, KeyAction, KeyInput};
use crate::json::Json;
use crate::text;

/// The letters labels are made of: the home row and the keys beside it, so
/// that a hand typing a label stays where it is.
pub const ALPHABET: &str = "sadfjklewcmpgh";

/// The most hints labelled: three letters of the alphabet cover 2744.
pub const MAX_HINTS: usize = 2000;

/// The script, `function (what, labels, px)`, sent whole with every call.
///
/// `what` is `collect` (find the clickable elements in view and remember
/// them; answer `[[x, y, w, h, kind, cx, cy, href], ...]` in CSS pixels of the
/// top document's viewport, in reading order), `show` (`labels` one per hint
/// collected, drawn at `px`), `narrow` (`labels` is the prefix typed: the
/// rest are hidden) or `clear`.
pub const SCRIPT: &str = r#"function (what, labels, px) {
  var W = globalThis, S = W.__blinktermHints;
  if (!S) S = W.__blinktermHints = { hints: [], host: null, root: null, doc: null };
  var MAX_DEPTH = 8, MAX_HINTS = 2000;
  var SEL = 'a[href],button,input,select,textarea,summary,label,' +
    '[onclick],[contenteditable],[tabindex],[role=link],[role=button],[role=checkbox],' +
    '[role=radio],[role=tab],[role=menuitem],[role=menuitemcheckbox],[role=menuitemradio],' +
    '[role=option],[role=switch],[role=treeitem],[role=combobox],[role=slider],[role=textbox],' +
    'video[controls],audio[controls],details';
  var EDIT = { INPUT: 1, TEXTAREA: 1, SELECT: 1 };
  var NOEDIT = { button: 1, submit: 1, reset: 1, checkbox: 1, radio: 1, file: 1, image: 1, color: 1, range: 1 };

  function kindOf(e) {
    var t = e.tagName;
    if (t === 'A') return e.href && !/^javascript:/i.test(e.href) ? 'link' : 'click';
    if (t === 'INPUT') return NOEDIT[(e.type || 'text').toLowerCase()] ? 'click' : 'edit';
    if (t === 'TEXTAREA' || t === 'SELECT') return 'edit';
    if (e.isContentEditable) return 'edit';
    if (e.getAttribute('role') === 'textbox' || e.getAttribute('role') === 'combobox') return 'edit';
    return 'click';
  }
  // Every candidate in root (a document or a shadow root), open shadow
  // roots included, and same-origin frames with their offsets.
  function candidates(root, doc, off, clip, depth, out) {
    var els = root.querySelectorAll(SEL + ',*');
    for (var i = 0; i < els.length; i++) {
      var e = els[i];
      if (e.shadowRoot) candidates(e.shadowRoot, doc, off, clip, depth, out);
      if (e.tagName === 'IFRAME' || e.tagName === 'FRAME') {
        if (depth >= MAX_DEPTH) continue;
        var cd = null; try { cd = e.contentDocument; } catch (_) { }
        if (!cd) continue;
        var fr = e.getBoundingClientRect();
        var fo = [off[0] + fr.left + e.clientLeft, off[1] + fr.top + e.clientTop];
        // What of the frame is on the screen: its box, inside its parent's.
        var fc = [Math.max(clip[0], fo[0]), Math.max(clip[1], fo[1]),
                  Math.min(clip[2], fo[0] + e.clientWidth), Math.min(clip[3], fo[1] + e.clientHeight)];
        if (fc[2] - fc[0] < 2 || fc[3] - fc[1] < 2) continue;
        candidates(cd, cd, fo, fc, depth + 1, out);
        continue;
      }
      if (!e.matches(SEL)) {
        // A pointer-cursor element whose parent's cursor is not pointer:
        // the root of a region that is clickable by CSS alone.
        var cs;
        try { cs = getComputedStyle(e).cursor; } catch (_) { continue; }
        if (cs !== 'pointer') continue;
        var p = e.parentElement;
        if (p && getComputedStyle(p).cursor === 'pointer') continue;
        if (e.closest(SEL)) continue;
        // A pointer container around real controls (a card around its
        // link): the controls get the hints, the container does not.
        if (e.querySelector(SEL)) continue;
      }
      out.push({ e: e, doc: doc, off: off, clip: clip });
    }
  }
  function visible(c) {
    var e = c.e, doc = c.doc, win = doc.defaultView;
    if (e.disabled) return null;
    if (e.checkVisibility && !e.checkVisibility({ visibilityProperty: true, opacityProperty: true })) return null;
    var rects = e.getClientRects();
    if (!rects.length) return null;
    var vw = win.innerWidth, vh = win.innerHeight;
    // The first rect that is in the frame's viewport: an inline link that
    // wraps has several, and the label goes on the first visible one.
    for (var k = 0; k < rects.length; k++) {
      var r = rects[k];
      if (r.width < 1 || r.height < 1) continue;
      // Inside the frame's viewport, and inside what of the frame is on
      // the screen (clip, in the top document's coordinates).
      var x0 = Math.max(r.left, 0, c.clip[0] - c.off[0]), y0 = Math.max(r.top, 0, c.clip[1] - c.off[1]);
      var x1 = Math.min(r.right, vw, c.clip[2] - c.off[0]), y1 = Math.min(r.bottom, vh, c.clip[3] - c.off[1]);
      if (x1 - x0 < 2 || y1 - y0 < 2) continue;
      // Something else on top of it (a modal, a cookie banner, a sibling
      // absolutely positioned over it) means a click there would not reach
      // it. Asked at the visible part's centre; an element inside it, or
      // its label, counts.
      var cx = (x0 + x1) / 2, cy = (y0 + y1) / 2;
      // From the element's own root, so that an element in a shadow tree
      // is answered by the tree and not by its host.
      var top = e.getRootNode().elementFromPoint(cx, cy);
      if (top && top !== e && !e.contains(top) && !top.contains(e)) {
        if (!(top.tagName === 'LABEL' && top.control === e)) continue;
      }
      return [c.off[0] + x0, c.off[1] + y0, x1 - x0, y1 - y0, cx + c.off[0], cy + c.off[1]];
    }
    return null;
  }
  function collect() {
    var out = [];
    candidates(W.document, W.document, [0, 0], [0, 0, W.innerWidth, W.innerHeight], 0, out);
    var hints = [], seen = new Set();
    for (var i = 0; i < out.length && hints.length < MAX_HINTS; i++) {
      var e = out[i].e;
      if (seen.has(e)) continue;
      seen.add(e);
      var box = visible(out[i]);
      if (!box) continue;
      // A label whose control is also a hint: one hint, on the control.
      if (e.tagName === 'LABEL' && e.control) continue;
      hints.push({ e: e, doc: out[i].doc, box: box, kind: kindOf(e) });
    }
    // Reading order: top to bottom, then left to right, in rows of a
    // label's height so that a line of links reads across.
    var rowH = Math.max(8, px || 16);
    hints.sort(function (a, b) {
      var ra = Math.floor(a.box[1] / rowH), rb = Math.floor(b.box[1] / rowH);
      return ra - rb || a.box[0] - b.box[0];
    });
    S.hints = hints;
    return hints.map(function (h) {
      var href = '';
      if (h.kind === 'link') { try { href = String(h.e.href); } catch (_) { } }
      return [Math.round(h.box[0]), Math.round(h.box[1]), Math.round(h.box[2]), Math.round(h.box[3]), h.kind, Math.round(h.box[4]), Math.round(h.box[5]), href];
    });
  }
  function show(labels) {
    clear();
    var doc = W.document, html = doc.documentElement;
    if (!html) return 0;
    var host = doc.createElement('blinkterm-hints');
    host.setAttribute('style', 'all:initial!important;position:fixed!important;left:0!important;top:0!important;width:0!important;height:0!important;overflow:visible!important;z-index:2147483647!important;pointer-events:none!important');
    var root = host.attachShadow({ mode: 'closed' });
    var size = Math.max(6, (px || 16) * 0.7);
    var st = doc.createElement('style');
    st.textContent = '.h{position:fixed;display:block;box-sizing:border-box;font:bold ' + size + 'px/1.25 system-ui,sans-serif;' +
      'padding:0 ' + (size * 0.25) + 'px;color:#000;background:#ffd400;border:' + Math.max(1, size / 12) + 'px solid #806a00;' +
      'border-radius:' + (size * 0.2) + 'px;white-space:pre;letter-spacing:' + (size * 0.05) + 'px;box-shadow:0 0 ' + (size * 0.2) + 'px #0006;pointer-events:none}' +
      '.h span{color:#8a7400}';
    root.appendChild(st);
    var n = 0;
    for (var i = 0; i < S.hints.length && i < labels.length; i++) {
      var h = S.hints[i], d = doc.createElement('div');
      d.className = 'h';
      d.textContent = labels[i];
      // At the element's top-left, nudged inside the viewport.
      var x = Math.max(0, h.box[0] - size * 0.3), y = Math.max(0, h.box[1] - size * 0.3);
      d.style.left = x + 'px'; d.style.top = y + 'px';
      root.appendChild(d); n++;
    }
    html.appendChild(host);
    S.host = host; S.root = root; S.doc = doc;
    return n;
  }
  function narrow(prefix) {
    if (!S.root) return 0;
    var n = 0, ds = S.root.querySelectorAll('.h');
    for (var i = 0; i < ds.length; i++) {
      var d = ds[i], t = d.getAttribute('data-l') || d.textContent;
      d.setAttribute('data-l', t);
      if (t.indexOf(prefix) !== 0) { d.style.display = 'none'; continue; }
      d.style.display = 'block'; n++;
      d.textContent = '';
      var s = doc_span(S.doc, t.slice(0, prefix.length));
      d.appendChild(s); d.appendChild(S.doc.createTextNode(t.slice(prefix.length)));
    }
    return n;
  }
  function doc_span(doc, text) { var s = doc.createElement('span'); s.textContent = text; return s; }
  function clear() {
    if (S.host && S.host.parentNode) S.host.parentNode.removeChild(S.host);
    S.host = null; S.root = null;
    return true;
  }

  if (what === 'collect') return collect();
  if (what === 'show') return show(labels || []);
  if (what === 'narrow') return narrow(labels || '');
  if (what === 'clear') return clear();
  return null;
}"#;

/// What a hint is on: decides what typing its label does afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `<a href>` to somewhere: `F` opens the href in a new tab.
    Link,
    /// A text field, a select, an editable region: clicking it is insert mode.
    Edit,
    /// Anything else clickable.
    Click,
}

/// One clickable thing, as the page reported it: CSS pixels of the top
/// document's viewport.
#[derive(Debug, Clone, PartialEq)]
pub struct Hint {
    pub kind: Kind,
    /// Where a click goes: the centre of the visible part.
    pub at: (f64, f64),
    /// The resolved href, plain text, for [`Kind::Link`]; empty otherwise.
    pub href: String,
}

/// The hints of one showing, their labels, and what has been typed.
#[derive(Debug, Clone, PartialEq)]
pub struct Hints {
    pub hints: Vec<Hint>,
    /// One per hint, in the same order: [`labels`].
    pub labels: Vec<String>,
    /// The letters typed so far, lower case.
    pub typed: String,
    /// `F`: a link's href goes to a new tab.
    pub new_tab: bool,
}

/// What one key did to the labels.
#[derive(Debug, Clone, PartialEq)]
pub enum Typed {
    /// Nothing matched, or the key is not a letter or backspace: unchanged.
    Nothing,
    /// The prefix changed and this many labels still match: the page is told
    /// to narrow.
    Narrowed(usize),
    /// A label was typed whole: this hint.
    Chosen(Hint),
    /// Escape.
    Cancel,
}

impl Hints {
    /// The hints out of a `collect` reply, labelled; `None` for a reply of
    /// another shape (a page that threw). An empty list is `Some`.
    ///
    /// An entry that is not the eight values the script answers is skipped
    /// rather than failing the rest; a page cannot reach this world's
    /// script, so one would be this program's own mistake.
    pub fn from_reply(reply: &Json, new_tab: bool) -> Option<Hints> {
        if reply.get("exceptionDetails").is_some() {
            return None;
        }
        let entries = reply.path(&["result", "value"])?.as_array()?;
        let hints: Vec<Hint> = entries
            .iter()
            .filter_map(|entry| {
                let [_, _, _, _, kind, cx, cy, href] = entry.as_array()? else {
                    return None;
                };
                let kind = match kind.as_str()? {
                    "link" => Kind::Link,
                    "edit" => Kind::Edit,
                    _ => Kind::Click,
                };
                let at = (cx.as_f64()?, cy.as_f64()?);
                if !at.0.is_finite() || !at.1.is_finite() {
                    return None;
                }
                let href = match kind {
                    Kind::Link => text::sanitize(href.as_str().unwrap_or_default())
                        .trim()
                        .to_string(),
                    _ => String::new(),
                };
                Some(Hint { kind, at, href })
            })
            .take(MAX_HINTS)
            .collect();
        Some(Hints {
            labels: labels(hints.len()),
            hints,
            typed: String::new(),
            new_tab,
        })
    }

    /// How many labels still match what was typed.
    pub fn remaining(&self) -> usize {
        self.labels
            .iter()
            .filter(|label| label.starts_with(&self.typed))
            .count()
    }

    /// What a key does to the labels; a release does nothing.
    ///
    /// Typing is case-folded, so a shift held by mistake still matches; a
    /// letter that would leave no label matching is not taken, so one wrong
    /// key costs nothing and the next right one carries on.
    pub fn step(&mut self, key: &KeyInput) -> Typed {
        if key.action == KeyAction::Release {
            return Typed::Nothing;
        }
        match key.key {
            Key::Escape => Typed::Cancel,
            Key::Backspace if key.mods.ctrl() || key.mods.alt() || key.mods.meta() => {
                Typed::Nothing
            }
            Key::Backspace => match self.typed.pop() {
                Some(_) => Typed::Narrowed(self.remaining()),
                None => Typed::Nothing,
            },
            Key::Char(c) if !key.mods.ctrl() && !key.mods.alt() && !key.mods.meta() => {
                let c = key.text.unwrap_or(c).to_ascii_lowercase();
                if !ALPHABET.contains(c) {
                    return Typed::Nothing;
                }
                self.typed.push(c);
                if let Some(index) = self.labels.iter().position(|label| *label == self.typed) {
                    return Typed::Chosen(self.hints[index].clone());
                }
                match self.remaining() {
                    0 => {
                        self.typed.pop();
                        Typed::Nothing
                    }
                    n => Typed::Narrowed(n),
                }
            }
            _ => Typed::Nothing,
        }
    }

    /// The words on the row: `12 hints`, `12 hints to a new tab`, `1 hint`.
    /// A count and this program's own words, and nothing of the page's.
    pub fn words(&self) -> String {
        let n = self.remaining();
        let noun = if n == 1 { "hint" } else { "hints" };
        if self.new_tab {
            format!("{n} {noun} to a new tab")
        } else {
            format!("{n} {noun}")
        }
    }
}

/// Labels for `n` hints: all of one length, from [`ALPHABET`], in order.
///
/// One length, so that no label is the start of another and a label typed
/// whole is chosen the moment its last letter is typed, with no timeout and
/// no Enter: `n` up to 14 is one letter, up to 196 two, up to 2744 three.
/// Label `i` is `i` written in base 14 with the alphabet as its digits.
pub fn labels(n: usize) -> Vec<String> {
    let alphabet: Vec<char> = ALPHABET.chars().collect();
    let base = alphabet.len();
    let mut length = 1;
    let mut capacity = base;
    while capacity < n {
        length += 1;
        capacity *= base;
    }
    (0..n)
        .map(|i| {
            let mut digits = vec![alphabet[0]; length];
            let mut rest = i;
            for place in (0..length).rev() {
                digits[place] = alphabet[rest % base];
                rest /= base;
            }
            digits.into_iter().collect()
        })
        .collect()
}

/// `Runtime.callFunctionOn` parameters for [`SCRIPT`] in world `context`,
/// its three arguments by value and the answer by value.
fn call_params(context: i64, what: &str, labels: Json, px: f64) -> Json {
    Json::object(vec![
        ("functionDeclaration", Json::string(SCRIPT)),
        ("executionContextId", Json::number(context as f64)),
        (
            "arguments",
            Json::Array(vec![
                Json::object(vec![("value", Json::string(what))]),
                Json::object(vec![("value", labels)]),
                Json::object(vec![("value", Json::number(px))]),
            ]),
        ),
        ("returnByValue", Json::Bool(true)),
    ])
}

/// Find the clickable things in view; `px` sets the height of a row of the
/// reading order.
pub fn collect_params(context: i64, px: f64) -> Json {
    call_params(context, "collect", Json::Null, px)
}

/// Draw `labels`, one per hint the last collect found, a cell tall: `px` is
/// how many CSS pixels a terminal cell is.
pub fn show_params(context: i64, labels: &[String], px: f64) -> Json {
    let labels = Json::Array(labels.iter().map(Json::string).collect());
    call_params(context, "show", labels, px)
}

/// Hide the labels that do not start with `typed`.
pub fn narrow_params(context: i64, typed: &str) -> Json {
    call_params(context, "narrow", Json::string(typed), 0.0)
}

/// Take the labels off the page.
pub fn clear_params(context: i64) -> Json {
    call_params(context, "clear", Json::Null, 0.0)
}

/// The three `Input.dispatchMouseEvent` parameter sets of a left click at
/// `at` (CSS pixels): moved, pressed, released, in that order — what a real
/// click is, the pointer arriving included, so that a page which reacts to
/// the hover before the press sees one.
pub fn click_params(at: (f64, f64)) -> [Json; 3] {
    let event = |kind: &str, button: &str, buttons: u32| {
        let mut fields = vec![
            ("type", Json::string(kind)),
            ("x", Json::number(at.0)),
            ("y", Json::number(at.1)),
            ("modifiers", Json::number(0)),
            ("button", Json::string(button)),
            ("buttons", Json::number(buttons)),
        ];
        if kind != "mouseMoved" {
            fields.push(("clickCount", Json::number(1)));
        }
        Json::object(fields)
    };
    [
        event("mouseMoved", "none", 0),
        event("mousePressed", "left", 1),
        event("mouseReleased", "left", 0),
    ]
}

/// The question asked after a click in normal mode: whether focus landed in
/// something editable — a text-like input, a textarea, a select,
/// `contenteditable`, `role=textbox` or `combobox` — followed down through
/// open shadow roots and same-origin frames to the element that has it.
/// Asked in the page's own world, and it leaves nothing behind there.
pub const FOCUSED: &str = "(function(){var e=document.activeElement;\
for(var i=0;i<16&&e;i++){if(e.shadowRoot&&e.shadowRoot.activeElement){e=e.shadowRoot.activeElement;continue}\
if(e.tagName==='IFRAME'||e.tagName==='FRAME'){var d=null;try{d=e.contentDocument}catch(_){}\
if(d&&d.activeElement){e=d.activeElement;continue}}break}\
if(!e)return false;var t=e.tagName;\
if(t==='INPUT')return!/^(button|submit|reset|checkbox|radio|file|image|color|range|hidden)$/i.test(e.type||'text');\
if(t==='TEXTAREA'||t==='SELECT')return true;if(e.isContentEditable)return true;\
var r=e.getAttribute&&e.getAttribute('role');return r==='textbox'||r==='combobox'})()";

/// The `Runtime.evaluate` parameters for [`FOCUSED`].
pub fn focused_params() -> Json {
    Json::object(vec![
        ("expression", Json::string(FOCUSED)),
        ("returnByValue", Json::Bool(true)),
    ])
}

/// Read the answer to [`FOCUSED`]: `None` for anything but a boolean.
pub fn focused_editable(reply: &Json) -> Option<bool> {
    if reply.get("exceptionDetails").is_some() {
        return None;
    }
    reply.path(&["result", "value"])?.as_bool()
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

    fn typed(c: char) -> KeyInput {
        KeyInput {
            key: Key::Char(c),
            mods: Mods::default(),
            action: KeyAction::Press,
            text: Some(c),
        }
    }

    fn some_hints(n: usize) -> Hints {
        let hints: Vec<Hint> = (0..n)
            .map(|i| Hint {
                kind: Kind::Click,
                at: (i as f64, 0.0),
                href: String::new(),
            })
            .collect();
        Hints {
            labels: labels(n),
            hints,
            typed: String::new(),
            new_tab: false,
        }
    }

    #[test]
    fn labels_are_all_one_length_from_the_alphabet_and_no_label_is_a_prefix_of_another() {
        assert_eq!(labels(0), Vec::<String>::new());
        assert_eq!(labels(1), vec!["s".to_string()]);
        let one: Vec<String> = ALPHABET.chars().map(String::from).collect();
        assert_eq!(labels(14), one);
        for (n, length) in [(15, 2), (196, 2), (197, 3), (MAX_HINTS, 3)] {
            let made = labels(n);
            assert_eq!(made.len(), n);
            assert!(made.iter().all(|label| label.chars().count() == length));
            assert!(made
                .iter()
                .all(|label| label.chars().all(|c| ALPHABET.contains(c))));
            let mut sorted = made.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), n, "distinct at {n}");
            for a in &made {
                for b in &made {
                    assert!(a == b || !b.starts_with(a.as_str()), "{a} starts {b}");
                }
            }
        }
        assert_eq!(labels(15)[0], "ss");
        assert_eq!(labels(15)[14], "as");
    }

    #[test]
    fn a_letter_narrows_a_miss_changes_nothing_and_backspace_widens() {
        let mut hints = some_hints(20);
        assert_eq!(hints.remaining(), 20);
        assert_eq!(hints.step(&typed('s')), Typed::Narrowed(14));
        assert_eq!(hints.typed, "s");
        // Not a letter of the alphabet at all.
        assert_eq!(hints.step(&typed('z')), Typed::Nothing);
        assert_eq!(hints.typed, "s");
        assert_eq!(hints.step(&key(Key::Backspace, 0)), Typed::Narrowed(20));
        assert_eq!(hints.typed, "");
        assert_eq!(hints.step(&key(Key::Backspace, 0)), Typed::Nothing);
        // A letter no label starts with: `f` is the fourth, and 20 labels
        // start with only the first two.
        assert_eq!(hints.step(&typed('f')), Typed::Nothing);
        assert_eq!(hints.typed, "");
        let mut released = typed('s');
        released.action = KeyAction::Release;
        assert_eq!(hints.step(&released), Typed::Nothing);
        assert_eq!(hints.typed, "");
    }

    #[test]
    fn a_label_typed_whole_is_the_hint_under_it_whatever_the_case() {
        let mut hints = some_hints(20);
        assert_eq!(hints.step(&typed('s')), Typed::Narrowed(14));
        assert_eq!(
            hints.step(&typed('a')),
            Typed::Chosen(hints.hints[1].clone())
        );

        let mut hints = some_hints(20);
        let shifted = KeyInput {
            key: Key::Char('s'),
            mods: Mods(Mods::SHIFT),
            action: KeyAction::Press,
            text: Some('S'),
        };
        assert_eq!(hints.step(&shifted), Typed::Narrowed(14));
        assert_eq!(
            hints.step(&typed('A')),
            Typed::Chosen(hints.hints[1].clone())
        );

        // One letter each: chosen at once.
        let mut few = some_hints(3);
        assert_eq!(few.step(&typed('d')), Typed::Chosen(few.hints[2].clone()));
    }

    #[test]
    fn escape_cancels_and_other_keys_are_nothing() {
        let mut hints = some_hints(20);
        assert_eq!(hints.step(&key(Key::Escape, 0)), Typed::Cancel);
        assert_eq!(hints.step(&key(Key::Enter, 0)), Typed::Nothing);
        assert_eq!(hints.step(&key(Key::Char('s'), Mods::CTRL)), Typed::Nothing);
        assert_eq!(hints.step(&key(Key::Char('s'), Mods::ALT)), Typed::Nothing);
        assert_eq!(hints.step(&key(Key::Down, 0)), Typed::Nothing);
        assert_eq!(hints.step(&key(Key::Tab, 0)), Typed::Nothing);
        assert_eq!(hints.typed, "");
    }

    #[test]
    fn the_reply_is_read_hrefs_are_plain_text_and_another_shape_is_none() {
        let read = |text: &str, new_tab: bool| {
            Hints::from_reply(&Json::parse(text).expect("the test's own JSON"), new_tab)
        };
        let hints = read(
            r#"{"result":{"type":"object","value":[[0,70,54,16,"link",27,78,"http://x/\u001b]0;x\u0007"]]}}"#,
            true,
        )
        .expect("one hint");
        assert_eq!(
            hints.hints,
            vec![Hint {
                kind: Kind::Link,
                at: (27.0, 78.0),
                href: "http://x/]0;x".to_string()
            }]
        );
        assert_eq!(hints.labels, vec!["s".to_string()]);
        assert!(hints.new_tab);

        let kinds = read(
            r#"{"result":{"type":"object","value":[
                [0,0,1,1,"edit",1,1,""],[0,0,1,1,"click",2,2,""],[0,0,1,1,"other",3,3,"x"]]}}"#,
            false,
        )
        .expect("three hints");
        let kinds: Vec<Kind> = kinds.hints.iter().map(|hint| hint.kind).collect();
        assert_eq!(kinds, vec![Kind::Edit, Kind::Click, Kind::Click]);

        assert_eq!(
            read(
                r#"{"result":{"type":"object","subtype":"error"},"exceptionDetails":{"text":"x"}}"#,
                false
            ),
            None
        );
        assert_eq!(
            read(r#"{"result":{"type":"string","value":"x"}}"#, false),
            None
        );
        let none = read(r#"{"result":{"type":"object","value":[]}}"#, false).expect("empty");
        assert!(none.hints.is_empty());
        assert!(none.labels.is_empty());
    }

    #[test]
    fn a_click_is_a_move_a_press_and_a_release_at_the_point_in_css_pixels() {
        let events = click_params((27.0, 78.0));
        let field = |event: &Json, name: &str| event.get(name).cloned();
        let kinds: Vec<_> = events
            .iter()
            .map(|event| event.get("type").and_then(Json::as_str).map(str::to_string))
            .collect();
        assert_eq!(
            kinds,
            ["mouseMoved", "mousePressed", "mouseReleased"].map(|k| Some(k.to_string()))
        );
        for event in &events {
            assert_eq!(event.get("x").and_then(Json::as_f64), Some(27.0));
            assert_eq!(event.get("y").and_then(Json::as_f64), Some(78.0));
            assert_eq!(event.get("modifiers").and_then(Json::as_i64), Some(0));
        }
        for event in &events[1..] {
            assert_eq!(field(event, "button"), Some(Json::string("left")));
            assert_eq!(event.get("clickCount").and_then(Json::as_i64), Some(1));
        }
        assert_eq!(events[1].get("buttons").and_then(Json::as_i64), Some(1));
        assert_eq!(events[2].get("buttons").and_then(Json::as_i64), Some(0));
    }

    #[test]
    fn the_calls_carry_the_script_the_stage_and_the_labels_by_value() {
        let argument = |params: &Json, n: usize| {
            params
                .get("arguments")
                .and_then(Json::as_array)
                .and_then(|arguments| arguments.get(n))
                .and_then(|argument| argument.get("value"))
                .cloned()
        };
        let collect = collect_params(7, 16.0);
        assert_eq!(
            collect.get("functionDeclaration").and_then(Json::as_str),
            Some(SCRIPT)
        );
        assert_eq!(
            collect.get("executionContextId").and_then(Json::as_i64),
            Some(7)
        );
        assert_eq!(argument(&collect, 0), Some(Json::string("collect")));
        assert_eq!(argument(&collect, 2).and_then(|v| v.as_i64()), Some(16));
        assert_eq!(
            collect.get("returnByValue").and_then(Json::as_bool),
            Some(true)
        );

        let show = show_params(7, &["sa".to_string(), "sd".to_string()], 8.0);
        assert_eq!(argument(&show, 0), Some(Json::string("show")));
        assert_eq!(
            argument(&show, 1),
            Some(Json::Array(vec![Json::string("sa"), Json::string("sd")]))
        );
        assert_eq!(argument(&show, 2).and_then(|v| v.as_i64()), Some(8));

        let narrow = narrow_params(7, "sa");
        assert_eq!(argument(&narrow, 0), Some(Json::string("narrow")));
        assert_eq!(argument(&narrow, 1), Some(Json::string("sa")));
        assert_eq!(argument(&clear_params(7), 0), Some(Json::string("clear")));
        // And on the wire, too: what is written parses back to the same.
        let wire = Json::parse(&show.to_string()).expect("the params are JSON");
        assert_eq!(wire, show);
    }

    #[test]
    fn the_script_is_a_function_of_what_labels_and_px() {
        assert!(SCRIPT.starts_with("function (what, labels, px) {"));
        assert!(SCRIPT.trim_end().ends_with('}'));
        assert!(SCRIPT.contains("'blinkterm-hints'"));
        assert!(SCRIPT.contains("mode: 'closed'"));
        assert!(SCRIPT.contains(&format!("MAX_HINTS = {MAX_HINTS}")));
        assert!(!SCRIPT.contains(['\0', '\u{2028}', '\u{2029}', '`']));
        assert!(SCRIPT.is_ascii());
        assert!(!FOCUSED.contains(['\0', '\u{2028}', '\u{2029}', '`']));
    }

    #[test]
    fn the_words_on_the_row_are_a_count_and_this_programs_own() {
        let mut hints = some_hints(12);
        assert_eq!(hints.words(), "12 hints");
        hints.new_tab = true;
        assert_eq!(hints.words(), "12 hints to a new tab");
        let one = some_hints(1);
        assert_eq!(one.words(), "1 hint");
        for words in [hints.words(), one.words()] {
            assert_eq!(text::sanitize(&words), words.as_str());
        }
    }

    #[test]
    fn what_has_focus_is_read_as_editable_or_not() {
        let read = |text: &str| focused_editable(&Json::parse(text).expect("the test's own JSON"));
        assert_eq!(
            read(r#"{"result":{"type":"boolean","value":true}}"#),
            Some(true)
        );
        assert_eq!(
            read(r#"{"result":{"type":"boolean","value":false}}"#),
            Some(false)
        );
        assert_eq!(read(r#"{"result":{"type":"string","value":"x"}}"#), None);
        assert_eq!(
            read(r#"{"result":{},"exceptionDetails":{"text":"x"}}"#),
            None
        );
        assert_eq!(
            focused_params().get("expression").and_then(Json::as_str),
            Some(FOCUSED)
        );
    }
}
