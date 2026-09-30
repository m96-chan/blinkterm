//! Reader mode: the article, without the page around it
//! ([#64](https://github.com/m96-chan/blinkterm/issues/64)).
//!
//! `alt+r` finds the part of the page that is the article — its title, its
//! byline, its paragraphs, its images and its links — and shows that alone,
//! at a width a line can be read at, in the colours the page is already
//! told to prefer. `alt+r` again puts the page back exactly as it was.
//!
//! # How the article is found
//!
//! By [`SCRIPT`], one function written here, in the manner of Arc90's
//! Readability and not a copy of anybody's: every paragraph — a `<p>`, a
//! `<pre>`, a `<blockquote>`, a cell, or a `<div>` holding text and no block
//! of its own — with 25 characters or more of text scores one point, one per
//! comma, and one per hundred characters up to three, for its parent, half
//! that for its grandparent and a sixth for the one above. A candidate starts
//! from its tag (an `<article>` or `<main>` well ahead, a `<div>` a little, a
//! list or a heading behind) and from its class and id (`content`, `post`,
//! `story` up; `comment`, `sidebar`, `cookie`, `share` down), and ends
//! multiplied by how little of its text is links. The article is the best
//! candidate with at least [`MIN_CHARS`] of text, with the siblings that
//! score near it or are plainly paragraphs of prose.
//!
//! Three things there were measured into it. List items are not paragraphs:
//! scored, Wikipedia's references — hundreds of `<li>`s full of commas —
//! out-scored the article. The third level is there because MDN cuts an
//! article into a `<section>` per heading, and two levels chose one section.
//! And the best candidate has to be long enough before it is best, because
//! on the BBC a headline card scored 33 over the article's text blocks and
//! had 246 characters.
//!
//! What comes out has to be prose, too: [`MIN_CHARS`] of text that is
//! neither a link nor a heading, or it is [`Answered::Nothing`], which the
//! row says rather than showing an empty column. That is a login page, a
//! video page, and a front page, whose best block is headlines that are all
//! links (the Guardian's and the BBC's both said so).
//!
//! What is kept of the article is a copy, cleaned: no scripts, styles,
//! forms, frames, embeds, `<nav>`, `<aside>` or `<footer>`; nothing the page
//! had hidden (each element of the copy is paired with the original's
//! `checkVisibility`); no block whose class says it is furniture and which is
//! mostly links; and on what is left, no attribute but the handful that
//! carry meaning — an anchor's `href`, an image's `src`, `srcset`, `alt` and
//! size, a cell's spans, a `<time>`'s `datetime`, `lang` and `dir`. Every url
//! is made absolute against the page's own base, a `javascript:` or `data:`
//! link loses its `href` and keeps its words, and a lazy image's `data-src`
//! becomes its `src`, since nothing is left in the copy to swap it in.
//!
//! # Where it is shown
//!
//! In the page, not instead of it. The script appends one same-origin
//! `<iframe>` (`about:blank`, [`FRAME_ID`]) to `<body>`, writes the article
//! into it with `document.open`/`write`/`close`, sizes the frame to what it
//! holds, and hides every other child of `<body>` with one constructed
//! stylesheet adopted by the document — constructed, so that a page's
//! Content-Security-Policy has no say, as with [`crate::appearance`]'s sheet
//! and [`crate::find`]'s. Nothing is reloaded and nothing navigates: the url
//! on the row, the history, the cookies, the scroll position to come back to
//! and the page's own state all stay, and the article's images come from
//! the same origin with the same cookies, so what the page could show the
//! reader can. `alt+r` again removes the frame and the sheet and scrolls
//! back to where the page was.
//!
//! A `data:` page was the other way, and it loses all of that: a history
//! entry for a page nobody visited, an opaque origin whose images need the
//! page's cookies and do not get them, and a url on the row that is not the
//! page's. A shadow root in the page was a third, and the page's own CSS
//! reaches into it by inheritance and custom properties; a frame's document
//! is styled by nothing but what [`SCRIPT`] writes. And a same-origin frame
//! is already reached by everything that reads the page — find
//! ([`crate::find`]), the hints ([`crate::hints`]) and the hover
//! ([`crate::hover`]) — so all three work in the reader as they do in the
//! page, and so do zoom, the saves, `--alpha` and `--force-dark`.
//!
//! The frame's links carry `<base target="_top">`: a link followed from the
//! reader loads in the tab, leaving reader mode with the document, as any
//! navigation does ([`crate::tabs::Tab::landed`]).
//!
//! # What a page can see
//!
//! One `<iframe id=blinkterm-reader>` at the end of its `<body>`, one more
//! adopted stylesheet, and the scroll going to the top — the `MutationObserver`
//! records of a child appended and later removed. It does not see the
//! script or its state: that runs in the isolated world [`WORLD`], shared
//! with find and the hints, and `__blinktermReader` is `undefined` to the
//! page, as `__blinktermFind` is. Nothing is fetched that the page had not
//! already asked for, and nothing is kept: no file, no profile state.
//!
//! # Colours and size
//!
//! No setting of its own. The reader's document says `color-scheme: light
//! dark` and has a dark rule under `prefers-color-scheme: dark`, so it
//! follows whatever the page is told — `--color-scheme`, or the terminal's
//! answer — and `--force-dark` darkens it as it would any light page. Under
//! `--alpha` its backgrounds are transparent, so the terminal's shows
//! through. Its text size is the zoom keys'. The iframe element itself says
//! `color-scheme: light dark` too, because a frame whose scheme differs from
//! its embedder's is given an opaque backdrop: without it, under `--alpha`
//! and a dark preference, the corner's alpha was 255 where the terminal
//! should have shown through.
//!
//! # How it is sent
//!
//! Like the hints: the world is made (or found again) on the key, and one
//! `Runtime.callFunctionOn` is sent and collected on a later pass, never
//! waited for, the tab's reader flag set from the answer. The walk is find's
//! kind of cost — one pass over the paragraphs and their visibility — and
//! is measured below. A navigation takes the frame with the document, and
//! [`crate::tabs::Tab::landed`] says reader mode is off.
//!
//! # Measured
//!
//! Against `chrome-headless-shell` 153, in the engine suite's reader tests
//! and beside them. On the fixture — a nav, a sidebar, an eight-paragraph
//! article, a footer and a fixed cookie box — the toggle on is 11 ms and off
//! 2 ms, answered. On a page of 2 000 paragraphs (find's long article) on is
//! 58 to 65 ms and off 3 to 5, so the walk is sent and collected rather than
//! waited for, and [`crate::app`] gives it five seconds. A wheel over the
//! frame scrolls the page, 200 px for 200 asked: the frame's own root does
//! not scroll, and the frame is as tall as what it holds, so the wheel
//! chains to the page as it would from any element that cannot scroll. The
//! `ResizeObserver` is the frame window's own, observing the frame's
//! `<body>`: 500 px added to the article grew the frame by 490 within one
//! poll. `--force-dark` darkens the light reader (corner luminance 0.01),
//! and a dark scheme on top of it is not inverted back (still 0.01). Under
//! `--alpha` the reader's corner is see-through in both schemes. A link
//! followed from the reader loads in the tab, and back from there is the
//! page as it loads, without the reader: the engine keeps no document in a
//! back-forward cache to bring the frame back with.
//!
//! On real pages, on 1 October 2026: a Wikipedia article (Quokka) is its
//! text, infobox and pictures, 46 ms; an MDN reference page is the whole
//! article, every section, 20 ms; a Rust blog post is its text, with the
//! title cut from its `og:title`; a Guardian and a BBC article are their
//! text and pictures with the byline or the date, 10 to 35 ms; the
//! Guardian's and the BBC's front pages and a bot check's "Just a moment"
//! page are "no article on this page"; `example.com` is its paragraphs.
//!
//! # Limits
//!
//! A page whose scripts rebuild `<body>` can take the frame away while the
//! row still says `reader`; `alt+r` then puts the page back (there was
//! nothing left to take away but the sheet), and `alt+r` again builds the
//! frame anew — measured: one frame and one sheet, not two. An article in a
//! cross-origin frame cannot be read from the top document, and a video is
//! not kept. A world gone stale under the key (a document replaced since it
//! was made) is said on the row, and the next press makes a new one. What
//! is left for later: `esc` leaving the mode, a normal-mode letter,
//! remembering the mode per site, a width or font setting, and the mode
//! surviving a navigation.

use crate::json::Json;

/// The isolated world the script runs in: find's, shared by name, as the
/// hints' is.
pub const WORLD: &str = crate::find::WORLD;

/// The id of the frame [`SCRIPT`] writes the article into.
pub const FRAME_ID: &str = "blinkterm-reader";

/// Less text than this, in the page or in the best candidate, and there is
/// no article: a login page, a search results page, a video.
pub const MIN_CHARS: usize = 500;

/// The script, as the `functionDeclaration` of a `Runtime.callFunctionOn`:
/// `function (on, alpha)`, answering `["on"]`, `["off"]` or `["nothing"]`.
///
/// `on` false takes everything it did away and scrolls back; `on` true with
/// the frame still in the document changes nothing; `on` true with the frame
/// gone (a page that rebuilt its body) builds it again. `alpha` makes the
/// reader's backgrounds transparent. Sent whole with every call, comments
/// and all, like find's.
pub const SCRIPT: &str = r#"function (on, alpha) {
var ID = 'blinkterm-reader', MIN = 500;
var s = globalThis.__blinktermReader;
// Everything this script did, undone; the scroll is the caller's.
var undo = function () {
  if (!s) return;
  if (s.frame.isConnected) s.frame.remove();
  document.adoptedStyleSheets = document.adoptedStyleSheets.filter(function (x) { return x !== s.sheet; });
  if (s.observer) s.observer.disconnect();
  if (s.timer) clearInterval(s.timer);
  delete globalThis.__blinktermReader;
};
if (!on) {
  if (s) { var back = s.scrollY; undo(); scrollTo(0, back); }
  return ['off'];
}
if (s && s.frame.isConnected) return ['on'];
// A frame the page's own scripts took away: start again, and come back to
// where the page was before the first time.
var kept = s ? s.scrollY : scrollY;
undo();
var body = document.body;
var norm = function (t) { return (t || '').replace(/\s+/g, ' ').trim(); };
if (!body || norm(body.textContent).length < MIN) return ['nothing'];
var visible = function (el) {
  return !el.checkVisibility || el.checkVisibility({visibilityProperty: true});
};
var POS = /article|body|content|entry|hentry|main|page|post|text|blog|story/i;
var NEG = /-ad-|banner|breadcrumb|comment|community|cookie|disqus|extra|footer|gdpr|header|menu|nav|pager|pagination|popup|promo|related|remark|reply|rss|share|sidebar|social|sponsor|supplemental/i;
var weight = function (el) {
  var w = 0;
  [el.getAttribute('class') || '', el.id || ''].forEach(function (v) {
    if (!v) return;
    if (NEG.test(v)) w -= 25;
    if (POS.test(v)) w += 25;
  });
  return w;
};
var base = function (el) {
  var t = el.tagName, b = 0;
  if (t === 'ARTICLE' || t === 'MAIN' || el.getAttribute('role') === 'main' ||
      /(^|\s)articleBody(\s|$)/.test(el.getAttribute('itemprop') || '')) b = 25;
  else if (t === 'DIV') b = 5;
  else if (t === 'PRE' || t === 'TD' || t === 'BLOCKQUOTE') b = 3;
  else if (/^(ADDRESS|OL|UL|DL|DD|DT|LI|FORM)$/.test(t)) b = -3;
  else if (/^(H[1-6]|TH)$/.test(t)) b = -5;
  return b + weight(el);
};
var linkDensity = function (el) {
  var all = norm(el.textContent).length;
  if (!all) return 0;
  var links = 0;
  el.querySelectorAll('a').forEach(function (a) { links += norm(a.textContent).length; });
  return Math.min(1, links / all);
};
// The paragraphs: the elements that hold prose, and the divs used as
// paragraphs, holding text and no block of their own.
var BLOCKS = 'address,article,aside,blockquote,dd,div,dl,dt,fieldset,figure,footer,form,h1,h2,h3,h4,h5,h6,header,hr,li,main,nav,ol,p,pre,section,table,ul';
var paras = Array.prototype.slice.call(body.querySelectorAll('p, td, pre, blockquote'));
body.querySelectorAll('div').forEach(function (d) { if (!d.querySelector(BLOCKS)) paras.push(d); });
var scores = new Map();
var add = function (el, points) {
  if (!el || el.nodeType !== 1 || el.tagName === 'HTML') return;
  if (!scores.has(el)) scores.set(el, base(el));
  scores.set(el, scores.get(el) + points);
};
paras.forEach(function (p) {
  var text = norm(p.textContent);
  if (text.length < 25 || !visible(p)) return;
  var points = 1 + (text.split(',').length - 1) + Math.min(Math.floor(text.length / 100), 3);
  // The parent all of it, the grandparent half, and the one above a
  // sixth: an article cut into sections still adds up to the article.
  var up = p.parentElement;
  [1, 2, 6].forEach(function (divider) {
    if (!up) return;
    add(up, points / divider);
    up = up.parentElement;
  });
});
// The best of those long enough to be an article: a short block that
// scores well (a card, a pull quote) is a sibling to be joined, not the
// article.
var winner = null, top = -Infinity, final = new Map();
scores.forEach(function (score, el) {
  var f = score * (1 - linkDensity(el));
  final.set(el, f);
  if (f > top && norm(el.textContent).length >= MIN) { top = f; winner = el; }
});
if (!winner) return ['nothing'];
// The winner's siblings that belong with it: those that score near it, and
// plain paragraphs of prose.
var pieces = [winner];
if (winner.parentElement && winner !== body) {
  var near = Math.max(10, top * 0.2);
  pieces = [];
  Array.prototype.forEach.call(winner.parentElement.children, function (sib) {
    if (sib === winner) { pieces.push(sib); return; }
    if (/^(NAV|ASIDE|FOOTER|HEADER|SCRIPT|STYLE)$/.test(sib.tagName) || !visible(sib)) return;
    var text = norm(sib.textContent);
    if ((final.get(sib) || -Infinity) >= near ||
        (sib.tagName === 'P' && text.length > 80 && linkDensity(sib) < 0.25)) pieces.push(sib);
  });
}
// The title, the byline and the date, drawn once above the text.
var title = '';
var h1 = winner.querySelector('h1');
var before = winner.previousElementSibling;
if (!h1 && before) h1 = before.tagName === 'H1' ? before : before.querySelector('h1');
if (!h1) {
  var h1s = document.querySelectorAll('h1');
  if (h1s.length === 1) h1 = h1s[0];
}
if (h1) title = norm(h1.textContent);
if (!title) {
  var og = document.querySelector('meta[property="og:title"]');
  title = norm(og && og.getAttribute('content')) || norm(document.title);
  var cut = Math.max(title.lastIndexOf(' - '), title.lastIndexOf(' | '), title.lastIndexOf(' — '));
  if (cut >= 15) title = title.slice(0, cut);
}
var by = '', byNode = null;
var bys = document.querySelectorAll('[rel~=author], [itemprop~=author], .byline, .author');
for (var i = 0; i < bys.length && !by; i++) {
  var t = norm(bys[i].textContent);
  if (t && t.length <= 100 && visible(bys[i])) { by = t; byNode = bys[i]; }
}
if (!by) {
  var ma = document.querySelector('meta[name=author]');
  var mt = norm(ma && ma.getAttribute('content'));
  if (mt.length <= 100) by = mt;
}
var time = winner.querySelector('time[datetime]') || document.querySelector('time[datetime]');
var date = time ? norm(time.textContent) : '';
// A copy of each piece, cleaned. Paired with the original element by
// element first, since only the original can say whether it shows.
var article = document.createElement('div');
pieces.forEach(function (piece) {
  var copy = piece.cloneNode(true);
  var from = piece.querySelectorAll('*'), to = copy.querySelectorAll('*'), gone = [];
  for (var i = 0; i < from.length; i++) {
    if (from[i] === byNode || !visible(from[i])) gone.push(to[i]);
  }
  gone.forEach(function (el) { el.remove(); });
  article.appendChild(copy);
});
article.querySelectorAll('script, style, link, meta, noscript, template, iframe, frame, object, embed, canvas, svg, video, audio, form, input, button, select, textarea, nav, aside, footer, dialog, [hidden], [aria-hidden=true], [role=navigation], [role=complementary], [role=banner], [role=dialog]').forEach(function (el) { el.remove(); });
article.querySelectorAll('div, section, ul, ol, table').forEach(function (el) {
  var text = norm(el.textContent).length;
  if ((NEG.test((el.getAttribute('class') || '') + ' ' + (el.id || '')) && linkDensity(el) > 0.33) ||
      (text < 25 && !el.querySelector('img'))) el.remove();
});
article.querySelectorAll('p, div').forEach(function (el) {
  if (!norm(el.textContent) && !el.querySelector('img')) el.remove();
});
// And what is left has to be prose: text that is neither a link nor a
// heading. A front page's best block is headlines, every one a link.
var prose = 0, walk = document.createTreeWalker(article, NodeFilter.SHOW_TEXT), node;
while ((node = walk.nextNode())) {
  if (!node.parentElement.closest('a, h1, h2, h3, h4, h5, h6')) prose += norm(node.data).length;
}
if (prose < MIN) return ['nothing'];
var first = article.querySelector('h1, h2, h3');
if (first && title && norm(first.textContent) === title) first.remove();
// Only the attributes that mean something, and every url absolute.
var abs = function (v) {
  try { return new URL(v, document.baseURI).href; } catch (e) { return null; }
};
article.querySelectorAll('img').forEach(function (img) {
  var src = img.getAttribute('src') || '';
  var lazy = img.getAttribute('data-src') || img.getAttribute('data-lazy-src') || img.getAttribute('data-original');
  if (lazy && (!src || /^data:/i.test(src))) img.setAttribute('src', lazy);
  var lazySet = img.getAttribute('data-srcset');
  if (lazySet && !img.getAttribute('srcset')) img.setAttribute('srcset', lazySet);
});
var KEEP = {A: ['href'], IMG: ['src', 'srcset', 'alt', 'width', 'height'], TD: ['colspan', 'rowspan'], TH: ['colspan', 'rowspan'], TIME: ['datetime']};
var all = [article].concat(Array.prototype.slice.call(article.querySelectorAll('*')));
all.forEach(function (el) {
  var keep = KEEP[el.tagName] || [];
  Array.prototype.slice.call(el.attributes).forEach(function (a) {
    if (a.name !== 'lang' && a.name !== 'dir' && keep.indexOf(a.name) < 0) el.removeAttribute(a.name);
  });
  if (el.tagName === 'A' && el.hasAttribute('href')) {
    var href = abs(el.getAttribute('href'));
    if (!href || /^(javascript|data):/i.test(href)) el.removeAttribute('href');
    else el.setAttribute('href', href);
  }
  if (el.tagName === 'IMG') {
    if (el.hasAttribute('src')) {
      var src = abs(el.getAttribute('src'));
      if (src) el.setAttribute('src', src); else el.removeAttribute('src');
    }
    if (el.hasAttribute('srcset')) {
      el.setAttribute('srcset', el.getAttribute('srcset').split(/,\s+/).map(function (part) {
        var bits = part.trim().split(/\s+/);
        bits[0] = abs(bits[0]) || '';
        return bits.join(' ');
      }).join(', '));
    }
  }
});
var esc = function (t) {
  return t.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
};
var meta = [by, date].filter(Boolean).map(esc).join(' · ');
var css = ':root{color-scheme:light dark} html{overflow:hidden}' +
  " body{margin:0;background:#fff;color:#1c1c1c;font:18px/1.6 Georgia,'Times New Roman',serif}" +
  ' @media (prefers-color-scheme: dark){body{background:#1c1c1c;color:#e6e6e6} a{color:#8ab4f8}}' +
  ' article{max-width:38em;margin:0 auto;padding:1.5em 1em} img{max-width:100%;height:auto}' +
  ' pre{overflow:auto;font-size:.85em} h1{font:700 1.6em/1.25 system-ui,sans-serif}' +
  ' .meta{opacity:.7;font-size:.9em}' +
  (alpha ? ' html,body{background:transparent !important}' : '');
var html = '<!doctype html><html><head><meta charset=utf-8><base target="_top"><title>' + esc(title) +
  '</title><style>' + css + '</style></head><body><article><header>' +
  (title ? '<h1>' + esc(title) + '</h1>' : '') + (meta ? '<p class=meta>' + meta + '</p>' : '') +
  '</header>' + article.innerHTML + '</article></body></html>';
// The frame, the sheet that hides the rest, and the frame kept as tall as
// what it holds, so that the page scrolls and the frame never does.
var frame = document.createElement('iframe');
frame.id = ID;
frame.style.cssText = 'display:block;width:100%;height:100vh;border:0;margin:0;color-scheme:light dark';
body.appendChild(frame);
var fd = frame.contentDocument;
fd.open();
fd.write(html);
fd.close();
var sheet = new CSSStyleSheet();
sheet.replaceSync('body > :not(#blinkterm-reader){display:none !important}' +
  ' html,body{overflow:visible !important;height:auto !important;min-height:0 !important}' +
  ' body{display:block !important;margin:0 !important;padding:0 !important;max-width:none !important;width:auto !important}' +
  ' #blinkterm-reader{display:block !important;position:static !important;visibility:visible !important;' +
  'width:100% !important;max-width:none !important;min-height:100vh !important;margin:0 !important;' +
  'border:0 !important;opacity:1 !important;transform:none !important}');
document.adoptedStyleSheets = document.adoptedStyleSheets.concat([sheet]);
var fit = function () {
  if (!fd.body) return;
  frame.style.setProperty('height', Math.ceil(fd.body.getBoundingClientRect().height) + 'px', 'important');
};
fit();
var RO = (frame.contentWindow && frame.contentWindow.ResizeObserver) || ResizeObserver;
var observer = new RO(fit);
observer.observe(fd.body);
fd.addEventListener('load', fit, true);
scrollTo(0, 0);
globalThis.__blinktermReader = {frame: frame, sheet: sheet, observer: observer, timer: 0, scrollY: kept};
return ['on'];
}"#;

/// The `Runtime.callFunctionOn` parameters for [`SCRIPT`] in world
/// `context`: on or off and whether under `--alpha` as its `arguments`, the
/// answer by value, and `silent`, so that nothing about the call reaches the
/// page's console.
pub fn call_params(context: i64, on: bool, alpha: bool) -> Json {
    Json::object(vec![
        ("functionDeclaration", Json::string(SCRIPT)),
        ("executionContextId", Json::number(context as f64)),
        (
            "arguments",
            Json::Array(vec![
                Json::object(vec![("value", Json::Bool(on))]),
                Json::object(vec![("value", Json::Bool(alpha))]),
            ]),
        ),
        ("returnByValue", Json::Bool(true)),
        ("silent", Json::Bool(true)),
    ])
}

/// What [`SCRIPT`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answered {
    /// The article is showing, alone.
    On,
    /// The page is back as it was.
    Off,
    /// There was no article to show, and nothing was changed.
    Nothing,
}

/// Read the answer to [`SCRIPT`]: `None` for an exception or anything not
/// of its shape.
pub fn answered(reply: &Json) -> Option<Answered> {
    if reply.get("exceptionDetails").is_some() {
        return None;
    }
    let answer = reply.path(&["result", "value"])?.as_array()?;
    if answer.len() != 1 {
        return None;
    }
    match answer.first()?.as_str()? {
        "on" => Some(Answered::On),
        "off" => Some(Answered::Off),
        "nothing" => Some(Answered::Nothing),
        _ => None,
    }
}

/// What the row says when the page has nothing to read.
pub const NOTHING: &str = "no article on this page";

/// What the row says when the page did not answer, threw, or had no world
/// to answer from.
pub const NOT_ANSWERED: &str = "the page did not answer the reader";

/// What the row says when the key comes before the page has.
pub const NOT_YET: &str = "the page has not come yet";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_call_names_the_script_the_world_and_whether_it_is_on_and_under_alpha() {
        let params = call_params(7, true, false);
        assert_eq!(
            params.get("functionDeclaration").and_then(Json::as_str),
            Some(SCRIPT)
        );
        assert_eq!(
            params.get("executionContextId").and_then(Json::as_i64),
            Some(7)
        );
        let arguments: Vec<Option<bool>> = params
            .get("arguments")
            .and_then(Json::as_array)
            .expect("arguments")
            .iter()
            .map(|argument| argument.get("value").and_then(Json::as_bool))
            .collect();
        assert_eq!(arguments, vec![Some(true), Some(false)]);
        assert_eq!(
            params.get("returnByValue").and_then(Json::as_bool),
            Some(true)
        );
        assert_eq!(params.get("silent").and_then(Json::as_bool), Some(true));
        assert_eq!(WORLD, crate::find::WORLD);
    }

    #[test]
    fn an_answer_is_on_off_or_nothing_and_an_exception_or_a_stranger_is_none() {
        let value = |json: &str| {
            Json::parse(&format!(
                r#"{{"result":{{"type":"object","value":{json}}}}}"#
            ))
            .expect("JSON")
        };
        assert_eq!(answered(&value(r#"["on"]"#)), Some(Answered::On));
        assert_eq!(answered(&value(r#"["off"]"#)), Some(Answered::Off));
        assert_eq!(answered(&value(r#"["nothing"]"#)), Some(Answered::Nothing));
        assert_eq!(answered(&value(r#"["maybe"]"#)), None);
        assert_eq!(answered(&value(r#"["on", 1]"#)), None);
        assert_eq!(answered(&value(r#""on""#)), None);
        assert_eq!(answered(&value("[]")), None);
        let thrown =
            Json::parse(r#"{"result":{"type":"object"},"exceptionDetails":{"text":"Uncaught"}}"#)
                .expect("JSON");
        assert_eq!(answered(&thrown), None);
        assert_eq!(answered(&Json::empty()), None);
    }

    #[test]
    fn the_script_hides_everything_but_its_own_frame_and_paints_nothing_under_alpha() {
        assert!(SCRIPT.contains("body > :not(#blinkterm-reader)"));
        assert!(SCRIPT.contains("prefers-color-scheme: dark"));
        assert!(SCRIPT.contains(r#"base target="_top""#));
        assert!(SCRIPT.contains(&format!("'{FRAME_ID}'")));
        assert!(SCRIPT.contains("html,body{background:transparent !important}"));
        assert!(SCRIPT.contains(&format!("MIN = {MIN_CHARS}")));
    }

    #[test]
    fn the_sentences_are_short_and_the_one_for_a_page_not_come_is_the_hints_own() {
        assert_eq!(NOTHING, "no article on this page");
        assert_eq!(NOT_YET, "the page has not come yet");
        assert!(NOT_ANSWERED.starts_with("the page did not"));
    }
}
