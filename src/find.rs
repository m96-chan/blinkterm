//! Find in page: a `find:` prompt on the row, and a script in a world of its
//! own that counts, highlights and scrolls
//! ([#12](https://github.com/m96-chan/blinkterm/issues/12)).
//!
//! The engine has a find of its own, `window.find()`, and the headless shell
//! has it: measured against `chrome-headless-shell` 153, `find("fox")` is
//! `true`, case-insensitive, finds a word split across `<b>fo</b>x`, finds
//! `東京`, wraps when asked, and scrolls a 300-paragraph page from 0 to 14054
//! to reach its match. It is still the wrong tool. It answers one boolean per
//! step and never a count; it highlights nothing but the one match; it stops
//! on text nobody can see — a closed `<details>`, a `display:none`
//! paragraph and a `<textarea>` were each a "match", selected and invisible;
//! and what it draws is the page's selection, so the text a person dragged
//! over for `alt+c` is gone the moment they search.
//!
//! So the matching is [`SCRIPT`]'s. It walks the text nodes of the document
//! with a `TreeWalker`, leaves out the elements whose text is not the page's
//! words (`SCRIPT`, `STYLE`, `TEXTAREA` and the rest) and every element
//! `checkVisibility({visibilityProperty: true})` says is hidden, joins what is
//! left the way the page shows it — a run of whitespace as one space, a
//! newline between blocks so that no match is glued across a paragraph, a
//! word split across inline elements still one word — folds the case without
//! changing any length, and `indexOf`s the needle through it. Every match is
//! counted; the first [`MAX_HIGHLIGHTED`] become `Range`s. Measured on a page
//! with `fox` in three visible spellings, a split `<b>fo</b>x`, two paragraphs
//! far down, and in a closed `<details>`, a `display:none` paragraph, a
//! `visibility:hidden` one and a `<textarea>`: six, which is the number a
//! person can see.
//!
//! The ranges are painted with the CSS Custom Highlight API, which has been in
//! Chromium since 105: a `Highlight` registered as `blinkterm-find` in yellow
//! and the current match as `blinkterm-find-current` in orange, from one
//! constructed stylesheet adopted by the document. Nothing in the page is
//! changed — no `<span>` wrapped round a match, no `<style>` added, the DOM's
//! `innerHTML` the same before and after, and `getSelection()` exactly what it
//! was, measured with a selection made before the search and read after it.
//! A `<span>`-wrapping walk was the other way to paint, and it mutates the
//! page, breaks its selectors and its event handlers, and takes more code to
//! undo than to do.
//!
//! The script runs in an isolated world (`Page.createIsolatedWorld`, named
//! [`WORLD`]) rather than the page's own. The highlights it registers paint
//! exactly as the page's would — the registry and the adopted sheets are the
//! document's, not the world's: six matches painted 1203 yellow pixels from
//! the isolated world and 985 from the main one, at two scroll positions of
//! the same page — but its state, on the world's
//! `globalThis.__blinktermFind`, is invisible to the page, where `typeof
//! __blinktermFind` is `undefined`. What the page can see is that
//! `CSS.highlights.size` is 2 and one sheet is adopted while a search is on,
//! and a page that clears them loses the highlights until the next key, which
//! puts them back in 0.9 ms. A world is made once per document: asked for
//! again, the engine answers with the same `executionContextId`; after a
//! navigation the old one answers `Cannot find context with specified id`
//! ([`stale_world`]), and a `history.pushState` keeps it. It is made when the
//! prompt opens rather than in every document of every tab through
//! `Page.addScriptToEvaluateOnNewDocument`, which works too and costs a script
//! in every page whether or not anybody ever searches it; the lazy way costs
//! 0.4 to 2.2 ms per call, twice, on the first `ctrl+f`.
//!
//! The script goes whole with every call, as the `functionDeclaration` of a
//! `Runtime.callFunctionOn`, with the needle and the step as its `arguments`.
//! Eight kilobytes a keystroke is a seventh of one screencast frame, and it
//! buys no install step, no bookkeeping of which document has the function,
//! and no needle ever spliced into JavaScript source: the needle is a JSON
//! value, so a quote in it is a quote.
//!
//! A search is not free, and that decides how it is sent. On a page of 2 000
//! paragraphs (240 k characters, a long encyclopaedia article) the first
//! search is 31 to 61 ms of renderer and each keystroke after it about as
//! much; on 20 000 paragraphs (2.4 M characters, more than anybody reads) it is
//! 350 ms to a second. The walk is the cost — `checkVisibility` is two thirds
//! of it, and it is what keeps the hidden text out — not the `indexOf`, which
//! is 3 to 45 ms even at 2.4 M. A step to the next match is 1 to 8 ms once the
//! all-matches `Highlight` is cached per needle (it was 253 ms on 160 000
//! matches rebuilt every step), and painting 10 000 highlighted ranges costs a
//! screenshot nothing measurable. So a search is the still's shape and not
//! `page_loaded`'s: sent, and collected on a later pass, with the keys typed
//! meanwhile folded into one [`Ask`] that goes when the answer is in. A loop
//! sitting in a call for half a second is a loop not reading the terminal.
//!
//! The scope is the top document and its same-origin frames, eight deep: a
//! search on a page with a same-origin frame, a cross-origin one and a
//! `srcdoc` one counted four, the cross-origin frame's `contentDocument` being
//! `null` rather than an exception, and a step into the same-origin frame
//! scrolled the frame and left the page where it was. A cross-origin frame
//! cannot be searched from any script, and the `DOM` and `Overlay` domains
//! that could would stream events into a mailbox that holds 512 and drops the
//! oldest — the argument [`crate::load`] already made against `Network`.
//!
//! What it deliberately does not do: search the value of an `<input>` or a
//! `<textarea>`, which Chromium's own find bar does, and open a closed
//! `<details>` to reach a match (`hidden=until-found`), because both reach
//! into the page's form state or change it. Nor is there a case-sensitive,
//! whole-word or regex toggle: the row has no room for a checkbox and the
//! reflex `ctrl+f` calls up has none. Case-insensitive always, as every
//! browser's find bar is; smart case, where a capital makes it sensitive, is
//! an editor's rule. The highlight colours are fixed: `#ff0` and `#f80` with
//! black text read on a light page and on a dark one.
//!
//! Nothing here talks to the engine or the terminal. It holds the script,
//! builds the parameters, reads the replies, decides what a key means to the
//! prompt and what the row says about the answer, so that all of it is
//! tested without either. [`crate::app`] sends and collects.

use crate::input::{Key, KeyAction, KeyInput};
use crate::json::Json;
use crate::line::{Edit, Line};

/// The name of the isolated world the script runs in, and the stem of the
/// highlights it registers (`blinkterm-find`, `blinkterm-find-current`).
pub const WORLD: &str = "blinkterm";

/// The most `Range`s the script highlights; every match is counted.
///
/// A range costs memory and a place in a `Highlight`, and the count comes from
/// the search rather than from the ranges, so it is exact past the cap:
/// `160000` for `e` on the 2.4 M-character page, with ten thousand of them
/// yellow. Nobody steps through more than this one at a time.
pub const MAX_HIGHLIGHTED: u32 = 10_000;

/// The script, as the `functionDeclaration` of a `Runtime.callFunctionOn`:
/// `function (needle, step)`, answering `[count, current, scrolled,
/// highlighted]`.
///
/// A new needle is searched from scratch and its current match is the first
/// one in the viewport, else the first below it, else the first on the page —
/// what a browser does from where the person is looking, and what stops every
/// keystroke jumping back to the top. The same needle again moves the current
/// match by `step`, wrapping either way. An empty needle clears everything
/// the script did. The current match is scrolled to only when it is not
/// already wholly in view, by `scrollIntoView` on its parent element — which
/// scrolls every scroller above it, a box with an `overflow` of its own and a
/// frame included — and then the window, if the parent is taller than the
/// viewport. Without the Custom Highlight API (a Chromium before 105, not
/// measured) the current match is selected instead, so that a person still
/// sees where they are.
///
/// Sent whole with every call and kept with its comments: they are bytes
/// nobody will notice and they explain forty lines of index arithmetic.
pub const SCRIPT: &str = r#"function (needle, step) {
  // State lives on the isolated world's global, which the page cannot see.
  var W = globalThis, S = W.__blinktermFind;
  if (!S) S = W.__blinktermFind = { needle: '', ranges: [], at: -1, count: 0,
                                    sheets: [], all: new Map(), frames: [] };
  var NAME = 'blinkterm-find', CUR = 'blinkterm-find-current';
  var CSSTEXT = '::highlight(' + NAME + '){background-color:#ff0;color:#000}' +
                '::highlight(' + CUR + '){background-color:#f80;color:#000}';
  // Elements whose text is not the page's words.
  var SKIP = { SCRIPT: 1, STYLE: 1, NOSCRIPT: 1, TEMPLATE: 1, TEXTAREA: 1,
               TITLE: 1, HEAD: 1, SELECT: 1 };
  // Elements that do not break a run of text: a match may cross them.
  var INLINE = { A: 1, ABBR: 1, B: 1, BDI: 1, BDO: 1, BIG: 1, CITE: 1, CODE: 1,
    DATA: 1, DEL: 1, DFN: 1, EM: 1, FONT: 1, I: 1, INS: 1, KBD: 1, LABEL: 1,
    MARK: 1, Q: 1, RT: 1, RUBY: 1, S: 1, SAMP: 1, SMALL: 1, SPAN: 1, STRIKE: 1,
    STRONG: 1, SUB: 1, SUP: 1, TIME: 1, TT: 1, U: 1, VAR: 1, WBR: 1 };
  var MAX_RANGES = 10000, MAX_DEPTH = 8;

  // Lower-case without changing the length, so that an index into the
  // folded text is an index into the original: a character whose lower case
  // is longer (U+0130) is kept as it is.
  function fold(t) {
    var l = t.toLowerCase();
    if (l.length === t.length) return l;
    var o = '';
    for (var c of t) { var d = c.toLowerCase(); o += d.length === c.length ? d : c; }
    return o;
  }
  // The nearest ancestor that is not inline: two text nodes with different
  // ones are in different blocks, and a newline goes between them.
  function block(n) {
    var e = n.parentNode;
    while (e && e.nodeType === 1 && INLINE[e.tagName]) e = e.parentNode;
    return e;
  }
  function isWs(c) { return c === 32 || c === 9 || c === 10 || c === 13 || c === 12 || c === 0xa0; }

  // The visible text of `doc` and of the same-origin frames in it, as one
  // string with runs of whitespace as one space, plus for every character
  // which text node it came from and at what offset.
  function Out() { this.parts = []; this.len = 0; this.nodes = []; this.starts = []; this.off = new Int32Array(1 << 16); }
  Out.prototype.push = function (ch, n, i) {
    if (this.len === this.off.length) { var g = new Int32Array(this.len * 2); g.set(this.off); this.off = g; }
    this.off[this.len++] = n ? i : -1;
    this.parts.push(ch);
  };
  function collect(doc, depth, out, frames) {
    var root = doc.body || doc.documentElement;
    if (!root) return;
    var walker = doc.createTreeWalker(root, 1 | 4, { acceptNode: function (n) {
      if (n.nodeType === 1) {
        if (SKIP[n.tagName]) return 2;                                       // reject the subtree
        if (n.checkVisibility && !n.checkVisibility({ visibilityProperty: true })) return 2;
        return n.tagName === 'IFRAME' ? 1 : 3;                              // frames are visited; other elements only descended
      }
      return 1;
    } });
    var prevBlock = null, n;
    while ((n = walker.nextNode())) {
      if (n.nodeType === 1) {
        if (depth < MAX_DEPTH) {
          var cd = null;
          try { cd = n.contentDocument; } catch (e) { }                     // null across an origin
          if (cd) { frames.push(cd); collect(cd, depth + 1, out, frames); prevBlock = null; }
        }
        continue;
      }
      var data = n.data;
      if (!data) continue;
      var b = block(n);
      if (prevBlock && b !== prevBlock) out.push('\n', null, 0);
      prevBlock = b;
      out.nodes.push(n); out.starts.push(out.len);
      var ws = false;
      for (var i = 0; i < data.length; i++) {
        var c = data.charCodeAt(i);
        if (isWs(c)) { if (ws) continue; ws = true; out.push(' ', n, i); }
        else { ws = false; out.push(data[i], n, i); }
      }
    }
  }
  function nodeAt(out, idx) {
    var lo = 0, hi = out.starts.length - 1;
    while (lo < hi) { var m = (lo + hi + 1) >> 1; if (out.starts[m] <= idx) lo = m; else hi = m - 1; }
    return out.nodes[lo];
  }
  function build(needle) {
    var out = new Out(), frames = [];
    collect(W.document, 0, out, frames);
    var hay = fold(out.parts.join('')), nd = fold(needle);
    var ranges = [], count = 0, i = 0;
    while ((i = hay.indexOf(nd, i)) !== -1) {
      count++;
      if (ranges.length < MAX_RANGES) {
        var e = i + nd.length - 1, a = nodeAt(out, i), z = nodeAt(out, e);
        var r = a.ownerDocument.createRange();
        r.setStart(a, out.off[i]); r.setEnd(z, out.off[e] + 1);
        ranges.push(r);
      }
      i += nd.length;
    }
    S.frames = frames;
    return { ranges: ranges, count: count };
  }
  // One constructed stylesheet per document, adopted; put back if the page
  // took it away.
  function styled(doc) {
    var win = doc.defaultView; if (!win || !win.CSSStyleSheet) return;
    for (var k = 0; k < S.sheets.length; k++) if (S.sheets[k].doc === doc) {
      if (doc.adoptedStyleSheets.indexOf(S.sheets[k].sheet) === -1)
        doc.adoptedStyleSheets = doc.adoptedStyleSheets.concat([S.sheets[k].sheet]);
      return;
    }
    var sheet = new win.CSSStyleSheet(); sheet.replaceSync(CSSTEXT);
    doc.adoptedStyleSheets = doc.adoptedStyleSheets.concat([sheet]);
    S.sheets.push({ doc: doc, sheet: sheet });
  }
  function paint() {
    var api = !!(W.Highlight && W.CSS && W.CSS.highlights);
    var docs = [W.document].concat(S.frames);
    for (var d = 0; d < docs.length; d++) {
      var doc = docs[d], win = doc.defaultView;
      if (!win || !win.CSS || !win.CSS.highlights) continue;
      var mine = S.ranges.filter(function (r) { return r.startContainer.ownerDocument === doc; });
      if (!mine.length) { win.CSS.highlights.delete(NAME); win.CSS.highlights.delete(CUR); continue; }
      styled(doc);
      var all = S.all.get(doc);
      if (!all || all.needle !== S.needle) {
        var h = new win.Highlight(); for (var k = 0; k < mine.length; k++) h.add(mine[k]);
        all = { needle: S.needle, h: h }; S.all.set(doc, all);
      }
      if (win.CSS.highlights.get(NAME) !== all.h) win.CSS.highlights.set(NAME, all.h);
      var cur = S.ranges[S.at];
      if (cur && cur.startContainer.ownerDocument === doc) win.CSS.highlights.set(CUR, new win.Highlight(cur));
      else win.CSS.highlights.delete(CUR);
    }
    if (!api && S.at >= 0) {                                                // an engine before Chromium 105
      var cur2 = S.ranges[S.at], sel = cur2.startContainer.ownerDocument.getSelection();
      sel.removeAllRanges(); sel.addRange(cur2);
    }
    return api;
  }
  function clear() {
    var docs = [W.document].concat(S.frames);
    for (var d = 0; d < docs.length; d++) {
      var win = docs[d].defaultView; if (!win || !win.CSS || !win.CSS.highlights) continue;
      win.CSS.highlights.delete(NAME); win.CSS.highlights.delete(CUR);
    }
    for (var k = 0; k < S.sheets.length; k++) {
      var doc = S.sheets[k].doc, i = doc.adoptedStyleSheets.indexOf(S.sheets[k].sheet);
      if (i !== -1) { var a = doc.adoptedStyleSheets.slice(); a.splice(i, 1); doc.adoptedStyleSheets = a; }
    }
    S.sheets = []; S.ranges = []; S.needle = ''; S.at = -1; S.count = 0; S.all = new Map(); S.frames = [];
  }
  // The first match in view, else the first below the fold, else the first.
  function firstInView(ranges) {
    for (var k = 0; k < ranges.length; k++) {
      var b = ranges[k].getBoundingClientRect(), vh = ranges[k].startContainer.ownerDocument.defaultView.innerHeight;
      if (b.bottom >= 0 && b.top < vh) return k;
      if (b.top >= vh) return k;
    }
    return 0;
  }
  // Bring a match into view if it is not wholly there: its parent element
  // centred (which scrolls every scroller above it, frames included), then
  // the range itself if the parent is taller than the viewport.
  function reveal(r) {
    var win = r.startContainer.ownerDocument.defaultView;
    var b = r.getBoundingClientRect(), vh = win.innerHeight, vw = win.innerWidth;
    if (b.top >= 0 && b.bottom <= vh && b.left >= 0 && b.right <= vw && (b.width || b.height)) return false;
    var el = r.startContainer.parentElement;
    if (el) el.scrollIntoView({ block: 'center', inline: 'nearest' });
    b = r.getBoundingClientRect();
    if (b.top < 0 || b.bottom > vh) win.scrollBy(0, b.top - vh / 2 + b.height / 2);
    return true;
  }

  if (!needle) { clear(); return [0, 0, false, true]; }
  var scrolled = false;
  if (needle !== S.needle) {
    var got = build(needle);
    S.needle = needle; S.ranges = got.ranges; S.count = got.count;
    S.at = S.ranges.length ? firstInView(S.ranges) : -1;
  } else if (S.ranges.length) {
    var n = S.ranges.length; S.at = ((S.at + step) % n + n) % n;
  }
  var api = paint();
  if (S.at >= 0) scrolled = reveal(S.ranges[S.at]);
  return [S.count, S.at + 1, scrolled, api];
}"#;

/// The find prompt while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finder {
    /// What is being typed: the needle. Starts as the last needle, offered
    /// whole, or empty.
    pub line: Line,
    /// What the page last answered, once it has. Kept until the next answer
    /// replaces it, so that a fast typist sees the count settle rather than
    /// flicker to nothing between keys.
    pub matches: Option<Matches>,
}

/// What the page said about a needle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Matches {
    /// How many, exactly, however many were highlighted.
    pub count: u32,
    /// Which one is current, from one; 0 when there are none.
    pub current: u32,
    /// Whether the page could highlight at all (the Custom Highlight API);
    /// `false` is an older engine, where the current match is selected
    /// instead.
    pub highlighted: bool,
}

/// What the loop is to send the page: the needle as it is now, and how many
/// matches to move by.
///
/// A needle the page has not searched yet is a new search, and `step` is
/// spent by it: the current match is the first in view whatever the step. The
/// needle it last searched, with a `step` of 0, is a repaint — nothing moves,
/// and highlights a page took away are put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub needle: String,
    pub step: i32,
}

impl Ask {
    /// Fold a newer ask into one not yet sent: the newer needle always wins
    /// and the steps add, so `Enter Enter` while a search is out is one call
    /// that moves by two, and a letter typed after an `Enter` is a search for
    /// the new needle from the first match in view. The needle is always the
    /// line's text at the moment of the newest key, so a search never runs on
    /// a stale one.
    pub fn merge(self, newer: Ask) -> Ask {
        if newer.needle != self.needle {
            return newer;
        }
        Ask {
            needle: newer.needle,
            step: self.step.saturating_add(newer.step),
        }
    }
}

/// What one key did to the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The line changed, or the cursor moved, or nothing happened; `Some` if
    /// the page should be asked something as a result.
    Typing(Option<Ask>),
    /// Escape: close, and clear the page.
    Close,
    /// `ctrl+q`.
    Quit,
}

impl Finder {
    /// A prompt offering `remembered` whole, if there is one: the first key
    /// replaces it, as `ctrl+l` offers the url.
    pub fn open(remembered: &str) -> Finder {
        let line = if remembered.is_empty() {
            Line::empty()
        } else {
            Line::selected(remembered)
        };
        Finder {
            line,
            matches: None,
        }
    }

    /// The ask that opening with a remembered needle sends at once, so that
    /// its highlights come straight back; `None` for an empty line.
    pub fn initial(&self) -> Option<Ask> {
        self.search()
    }

    /// What one key does.
    ///
    /// The prompt's own keys first, and then the line's, so that the find
    /// prompt and the url bar agree about every key they share: a release or
    /// a modifier on its own does nothing; Escape closes; `ctrl+q` quits;
    /// `enter` and `ctrl+g` go to the next match and, with shift, the one
    /// before. Then [`Line::step`], where Up and `ctrl+p` are the previous
    /// match and Down and `ctrl+n` the next — they walk a history in the url
    /// bar and there is none here — and any other key that changed the text is
    /// a search for it. `ctrl+f` is readline's forward-a-character in here,
    /// as it is in the url bar, rather than a second find: the line's rule,
    /// with no exception. A step on an empty needle asks nothing.
    pub fn step(&mut self, key: &KeyInput) -> Step {
        if key.action == KeyAction::Release || matches!(key.key, Key::Other(_)) {
            return Step::Typing(None);
        }
        let ctrl = key.mods.ctrl();
        let alt = key.mods.alt();
        let back = if key.mods.shift() { -1 } else { 1 };
        match key.key {
            Key::Escape => return Step::Close,
            Key::Char('q') if ctrl => return Step::Quit,
            Key::Enter => return Step::Typing(self.walk(back)),
            Key::Char('g' | 'G') if ctrl && !alt => return Step::Typing(self.walk(back)),
            _ => {}
        }
        let before = self.line.text().to_string();
        match self.line.step(key) {
            Edit::Previous => Step::Typing(self.walk(-1)),
            Edit::Next => Step::Typing(self.walk(1)),
            Edit::Typing | Edit::Inserted if self.line.text() != before => {
                Step::Typing(Some(Ask {
                    needle: self.line.text().to_string(),
                    step: 0,
                }))
            }
            // Enter, Escape and `ctrl+q` were taken above and cannot come
            // back from the line; named so that a new edit is a compile error
            // here rather than a key that silently does nothing.
            Edit::Typing | Edit::Inserted | Edit::Go | Edit::Cancel | Edit::Quit => {
                Step::Typing(None)
            }
        }
    }

    /// What the right-hand end of the row says: `3/17`, `no matches`, or
    /// nothing while the needle is empty or the page has not answered yet.
    /// Numbers and this program's own words only — never a page's.
    pub fn count_text(&self) -> String {
        match self.matches {
            _ if self.line.text().is_empty() => String::new(),
            None => String::new(),
            Some(Matches { count: 0, .. }) => "no matches".to_string(),
            Some(Matches { count, current, .. }) => format!("{current}/{count}"),
        }
    }

    /// A search for the line as it is, if there is anything in it.
    fn search(&self) -> Option<Ask> {
        self.walk(0)
    }

    /// A move by `step` through the matches of the line as it is.
    fn walk(&self, step: i32) -> Option<Ask> {
        let needle = self.line.text();
        (!needle.is_empty()).then(|| Ask {
            needle: needle.to_string(),
            step,
        })
    }
}

/// `Page.createIsolatedWorld` parameters for `frame`: the world named
/// [`WORLD`], and no universal access — the script reaches what a script in
/// the page could, and no further.
pub fn world_params(frame: &str) -> Json {
    Json::object(vec![
        ("frameId", Json::string(frame)),
        ("worldName", Json::string(WORLD)),
    ])
}

/// The main frame's id out of a `Page.getFrameTree` reply.
pub fn main_frame(reply: &Json) -> Option<String> {
    reply
        .path(&["frameTree", "frame", "id"])
        .and_then(Json::as_str)
        .map(str::to_string)
}

/// The world's id out of a `Page.createIsolatedWorld` reply.
pub fn context(reply: &Json) -> Option<i64> {
    reply.get("executionContextId").and_then(Json::as_i64)
}

/// `Runtime.callFunctionOn` parameters: [`SCRIPT`] in world `context`, with
/// `ask`'s needle and step as arguments and the answer by value.
pub fn call_params(context: i64, ask: &Ask) -> Json {
    Json::object(vec![
        ("functionDeclaration", Json::string(SCRIPT)),
        ("executionContextId", Json::number(context as f64)),
        (
            "arguments",
            Json::Array(vec![
                Json::object(vec![("value", Json::string(&ask.needle))]),
                Json::object(vec![("value", Json::number(ask.step))]),
            ]),
        ),
        ("returnByValue", Json::Bool(true)),
    ])
}

/// The same with an empty needle: clear everything the script did — the
/// highlights, the sheet it adopted, and its own state.
pub fn clear_params(context: i64) -> Json {
    call_params(
        context,
        &Ask {
            needle: String::new(),
            step: 0,
        },
    )
}

/// What a `Runtime.callFunctionOn` of [`SCRIPT`] answered: `[count, current,
/// scrolled, highlighted]`. `None` for a page that threw, or a reply of any
/// other shape. `scrolled` is not kept: the screencast shows the move, and
/// the engine tests read it.
pub fn matches(reply: &Json) -> Option<Matches> {
    if reply.get("exceptionDetails").is_some() {
        return None;
    }
    let value = reply.path(&["result", "value"]).and_then(Json::as_array)?;
    let [count, current, scrolled, highlighted] = value else {
        return None;
    };
    let number = |value: &Json| {
        value
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0 && *n <= u32::MAX as f64)
            .map(|n| n as u32)
    };
    scrolled.as_bool()?;
    Some(Matches {
        count: number(count)?,
        current: number(current)?,
        highlighted: highlighted.as_bool()?,
    })
}

/// Whether an error off the wire says the world went with its document —
/// `Cannot find context with specified id`, which is what the engine answered
/// a call on a world whose page had navigated — and is a reason to make it
/// again rather than to report.
pub fn stale_world(why: &str) -> bool {
    why.contains("Cannot find context with specified id")
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

    fn search(needle: &str) -> Step {
        Step::Typing(Some(Ask {
            needle: needle.to_string(),
            step: 0,
        }))
    }

    fn walk(needle: &str, step: i32) -> Step {
        Step::Typing(Some(Ask {
            needle: needle.to_string(),
            step,
        }))
    }

    #[test]
    fn a_letter_typed_asks_for_a_search_and_a_move_does_not() {
        let mut finder = Finder::open("");
        assert_eq!(finder.step(&typed('f')), search("f"));
        assert_eq!(finder.step(&typed('o')), search("fo"));
        assert_eq!(finder.step(&typed('x')), search("fox"));
        assert_eq!(finder.step(&key(Key::Left, 0)), Step::Typing(None));
        // Readline's forward-a-character, as in the url bar: not a second
        // find.
        assert_eq!(
            finder.step(&key(Key::Char('f'), Mods::CTRL)),
            Step::Typing(None)
        );
        assert_eq!(finder.step(&key(Key::Home, 0)), Step::Typing(None));
        assert_eq!(finder.step(&key(Key::End, 0)), Step::Typing(None));
        assert_eq!(finder.step(&key(Key::Backspace, 0)), search("fo"));
        // A key that means nothing in a line, and a release.
        assert_eq!(
            finder.step(&key(Key::Char('t'), Mods::CTRL)),
            Step::Typing(None)
        );
        assert_eq!(finder.step(&key(Key::Tab, 0)), Step::Typing(None));
        let mut released = typed('z');
        released.action = KeyAction::Release;
        assert_eq!(finder.step(&released), Step::Typing(None));
        assert_eq!(finder.line.text(), "fo");
        // Emptied, it is a search for nothing, which clears the page.
        finder.step(&key(Key::Backspace, 0));
        assert_eq!(finder.step(&key(Key::Backspace, 0)), search(""));
    }

    #[test]
    fn enter_and_ctrl_g_step_forward_and_with_shift_back() {
        let mut finder = Finder::open("");
        assert_eq!(finder.step(&key(Key::Enter, 0)), Step::Typing(None));
        assert_eq!(
            finder.step(&key(Key::Char('g'), Mods::CTRL)),
            Step::Typing(None),
            "nothing typed is nothing to step through"
        );
        for c in "fox".chars() {
            finder.step(&typed(c));
        }
        assert_eq!(finder.step(&key(Key::Enter, 0)), walk("fox", 1));
        assert_eq!(finder.step(&key(Key::Enter, Mods::SHIFT)), walk("fox", -1));
        assert_eq!(
            finder.step(&key(Key::Char('g'), Mods::CTRL)),
            walk("fox", 1)
        );
        assert_eq!(
            finder.step(&key(Key::Char('g'), Mods::CTRL | Mods::SHIFT)),
            walk("fox", -1)
        );
        assert_eq!(
            finder.step(&key(Key::Char('G'), Mods::CTRL | Mods::SHIFT)),
            walk("fox", -1)
        );
        assert_eq!(finder.step(&key(Key::Down, 0)), walk("fox", 1));
        assert_eq!(finder.step(&key(Key::Up, 0)), walk("fox", -1));
        assert_eq!(
            finder.step(&key(Key::Char('n'), Mods::CTRL)),
            walk("fox", 1)
        );
        assert_eq!(
            finder.step(&key(Key::Char('p'), Mods::CTRL)),
            walk("fox", -1)
        );
        assert_eq!(finder.line.text(), "fox", "and nothing typed");
    }

    #[test]
    fn escape_closes_and_ctrl_q_quits_whatever_was_typed() {
        for start in ["", "fox"] {
            let mut finder = Finder::open(start);
            assert_eq!(finder.step(&key(Key::Escape, 0)), Step::Close);
            let mut finder = Finder::open(start);
            finder.step(&typed('a'));
            assert_eq!(finder.step(&key(Key::Char('q'), Mods::CTRL)), Step::Quit);
        }
    }

    #[test]
    fn the_last_needle_is_offered_whole_and_searched_at_once() {
        let finder = Finder::open("fox");
        assert!(finder.line.whole());
        assert_eq!(
            finder.initial(),
            Some(Ask {
                needle: "fox".to_string(),
                step: 0
            })
        );
        // Enter walks it again without spending what was offered.
        let mut finder = Finder::open("fox");
        assert_eq!(finder.step(&key(Key::Enter, 0)), walk("fox", 1));
        // The first letter replaces it.
        assert_eq!(finder.step(&typed('b')), search("b"));
        assert_eq!(finder.line.text(), "b");
        assert_eq!(Finder::open("").initial(), None);
    }

    #[test]
    fn asks_coalesce_with_the_newest_needle_and_added_steps() {
        let ask = |needle: &str, step: i32| Ask {
            needle: needle.to_string(),
            step,
        };
        assert_eq!(ask("fo", 0).merge(ask("fox", 0)), ask("fox", 0));
        assert_eq!(ask("fox", 1).merge(ask("fox", 1)), ask("fox", 2));
        assert_eq!(ask("fox", 1).merge(ask("foxy", 0)), ask("foxy", 0));
        assert_eq!(ask("fox", 0).merge(ask("fox", -1)), ask("fox", -1));
        assert_eq!(ask("fox", 2).merge(ask("fox", -3)), ask("fox", -1));
        assert_eq!(
            ask("fox", i32::MAX).merge(ask("fox", 1)),
            ask("fox", i32::MAX),
            "a held key cannot overflow the step"
        );
    }

    #[test]
    fn the_count_is_numbers_or_this_programs_own_words() {
        let mut finder = Finder::open("");
        finder.matches = Some(Matches {
            count: 17,
            current: 3,
            highlighted: true,
        });
        assert_eq!(finder.count_text(), "", "an empty line counts nothing");
        finder.step(&typed('x'));
        assert_eq!(finder.count_text(), "3/17");
        finder.matches = None;
        assert_eq!(finder.count_text(), "", "before the first answer");
        finder.matches = Some(Matches {
            count: 0,
            current: 0,
            highlighted: true,
        });
        assert_eq!(finder.count_text(), "no matches");
        for count in [(3, 17), (0, 0), (160_000, 10_000)] {
            finder.matches = Some(Matches {
                count: count.0,
                current: count.1,
                highlighted: false,
            });
            let text = finder.count_text();
            assert_eq!(crate::text::sanitize(&text), text.as_str());
        }
    }

    #[test]
    fn the_call_carries_the_script_the_needle_and_the_step_by_value() {
        let params = call_params(
            7,
            &Ask {
                needle: "日本\"x".to_string(),
                step: 2,
            },
        );
        assert_eq!(
            params.get("functionDeclaration").and_then(Json::as_str),
            Some(SCRIPT)
        );
        assert_eq!(
            params.get("executionContextId").and_then(Json::as_i64),
            Some(7)
        );
        let arguments = params
            .get("arguments")
            .and_then(Json::as_array)
            .expect("arguments");
        // A quote survives because it is a JSON value, not source.
        assert_eq!(
            arguments[0].get("value").and_then(Json::as_str),
            Some("日本\"x")
        );
        assert_eq!(arguments[1].get("value").and_then(Json::as_i64), Some(2));
        assert_eq!(
            params.get("returnByValue").and_then(Json::as_bool),
            Some(true)
        );
        // And on the wire, too: what is written parses back to the same.
        let wire = Json::parse(&params.to_string()).expect("the params are JSON");
        assert_eq!(wire, params);

        let cleared = clear_params(7);
        let arguments = cleared
            .get("arguments")
            .and_then(Json::as_array)
            .expect("arguments");
        assert_eq!(arguments[0].get("value").and_then(Json::as_str), Some(""));
    }

    #[test]
    fn the_reply_is_read_and_a_page_that_threw_is_not() {
        let read = |text: &str| matches(&Json::parse(text).expect("the test's own JSON"));
        assert_eq!(
            read(r#"{"result":{"type":"object","value":[17,3,true,true]}}"#),
            Some(Matches {
                count: 17,
                current: 3,
                highlighted: true
            })
        );
        assert_eq!(
            read(r#"{"result":{"type":"object","value":[0,0,false,true]}}"#),
            Some(Matches {
                count: 0,
                current: 0,
                highlighted: true
            })
        );
        assert_eq!(
            read(r#"{"result":{"type":"object","value":[4,1,false,false]}}"#)
                .map(|m| m.highlighted),
            Some(false)
        );
        assert_eq!(
            read(
                r#"{"result":{"type":"object","subtype":"error"},"exceptionDetails":{"text":"x"}}"#
            ),
            None
        );
        assert_eq!(read(r#"{"result":{"type":"string","value":"3/17"}}"#), None);
        assert_eq!(read(r#"{"result":{"type":"object","value":[0]}}"#), None);
        assert_eq!(
            read(r#"{"result":{"type":"object","value":[-1,0,false,true]}}"#),
            None
        );
        assert_eq!(
            read(r#"{"result":{"type":"object","value":["3",1,false,true]}}"#),
            None
        );
    }

    #[test]
    fn a_world_that_went_with_its_document_is_told_apart() {
        assert!(stale_world("Cannot find context with specified id"));
        assert!(stale_world(
            "Runtime.callFunctionOn: Cannot find context with specified id"
        ));
        assert!(!stale_world("Runtime.callFunctionOn: the session ended"));
        assert!(!stale_world(""));
    }

    #[test]
    fn the_world_and_the_frame_are_read_off_their_replies() {
        let parse = |text: &str| Json::parse(text).expect("the test's own JSON");
        assert_eq!(context(&parse(r#"{"executionContextId":4}"#)), Some(4));
        assert_eq!(context(&parse(r#"{}"#)), None);
        assert_eq!(
            main_frame(&parse(
                r#"{"frameTree":{"frame":{"id":"F","url":"about:blank"},"childFrames":[]}}"#
            )),
            Some("F".to_string())
        );
        assert_eq!(main_frame(&parse(r#"{"frameTree":{}}"#)), None);
        let params = world_params("F");
        assert_eq!(params.get("frameId").and_then(Json::as_str), Some("F"));
        assert_eq!(params.get("worldName").and_then(Json::as_str), Some(WORLD));
        assert!(
            params.get("grantUniveralAccess").is_none(),
            "no more than the page's own reach"
        );
    }

    #[test]
    fn the_script_is_a_function_of_a_needle_and_a_step() {
        assert!(SCRIPT.starts_with("function (needle, step) {"));
        assert!(SCRIPT.trim_end().ends_with('}'));
        assert!(SCRIPT.contains("'blinkterm-find'"));
        assert!(SCRIPT.contains("'blinkterm-find-current'"));
        assert!(SCRIPT.contains(&format!("MAX_RANGES = {MAX_HIGHLIGHTED}")));
        // Nothing the pipe or a JavaScript parser could take for something
        // else: the pipe's messages end at a NUL, and a line or paragraph
        // separator ends a line in older engines' string literals.
        assert!(!SCRIPT.contains(['\0', '\u{2028}', '\u{2029}']));
        assert!(SCRIPT.is_ascii());
    }
}
