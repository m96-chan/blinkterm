//! Whether the page in front has taken an element fullscreen, heard from the
//! page by one long-standing `Runtime.evaluate`, and the pane given to it
//! whole while it has.
//!
//! # What the engine does
//!
//! Measured against `chrome-headless-shell` 153: `element.requestFullscreen()`
//! from a click (a dispatched mouse event is a user gesture) resolves, the
//! page's `fullscreenchange` fires about 13 ms later, `fullscreenElement` is
//! the element, and the element covers the viewport — a screenshot of a
//! 200x100 red box gone fullscreen is the whole viewport red. What does not
//! happen is any CDP event: no `Page.frameResized`, no change in the
//! screencast, nothing on the browser session. An Escape dispatched to the
//! page does not leave it either, because the browser's Escape handling is
//! the browser window's, which the shell does not have;
//! `document.exitFullscreen()` does, and so does a navigation.
//!
//! # How it is heard
//!
//! By asking the page to answer when it changes. [`watch_params`] is a
//! `Runtime.evaluate` with `awaitPromise`, sent — never called — into the
//! same kind of isolated world [`crate::find`] makes, of a promise that
//! resolves with `"in"` or `"out"` at the next `fullscreenchange`. The reply
//! comes 20 ms after the change; a navigation answers it with an error
//! (`Inspected target navigated or closed`), which is [`Heard::Gone`]; a
//! dialog holds it until the dialog is answered; other commands are answered
//! as usual while it waits. One command out per page, no polling, no
//! `Runtime.enable`, and nothing on the page's `window` that the page could
//! see or call.
//!
//! Two watches armed on one world are not both answered — measured: the
//! older one gets the change and the newer is never answered. A watch the
//! loop has let go of (its tab went behind) is still waiting in the page, so
//! the expression settles the one before it, with `"stale"`, before it
//! starts to wait: through a function kept on the isolated world's own
//! global, which the page cannot see (measured: `typeof` it from the page is
//! `undefined`). The stale answer goes to a claim nobody holds any more and
//! is dropped.
//!
//! # What it does to the screen
//!
//! [`layout`] is the one rule: the whole pane when the page in front is
//! fullscreen and nothing owns the row, else the pane less the row. The loop
//! gives a change of layout the resize path, so the page is told its new
//! size, the picture is placed from its new row, and the status row comes
//! and goes with it.

use tos_preview::fit::{Cells, Metrics};

use crate::json::Json;

/// The expression armed in the isolated world: resolves with `"in"` or
/// `"out"` at the next `fullscreenchange`, or at once with the current state
/// if `document.fullscreenElement` disagrees with `expected` — so that a
/// watch armed after the fact (a tab coming to the front, a relaunch)
/// catches up without a change having to happen. It settles any watch still
/// waiting in the same world first, with `"stale"` (see the module's second
/// section).
pub fn watch_expression(expected: bool) -> String {
    format!(
        "new Promise(function (resolve) {{\
         var held = self.__blinktermFullscreen;\
         if (typeof held === 'function') held('stale');\
         var done = false;\
         var settle = function (word) {{\
         if (done) return; done = true;\
         document.removeEventListener('fullscreenchange', change);\
         if (self.__blinktermFullscreen === settle) self.__blinktermFullscreen = null;\
         resolve(word);\
         }};\
         var change = function () {{ settle(document.fullscreenElement ? 'in' : 'out'); }};\
         self.__blinktermFullscreen = settle;\
         if (!!document.fullscreenElement !== {expected}) {{ change(); return; }}\
         document.addEventListener('fullscreenchange', change);\
         }})"
    )
}

/// `Runtime.evaluate` parameters for [`watch_expression`] in world
/// `context`: the promise awaited, the answer by value.
pub fn watch_params(context: i64, expected: bool) -> Json {
    Json::object(vec![
        ("expression", Json::string(watch_expression(expected))),
        ("contextId", Json::number(context as f64)),
        ("awaitPromise", Json::Bool(true)),
        ("returnByValue", Json::Bool(true)),
    ])
}

/// The expression that leaves: `document.exitFullscreen()`, with its
/// promise's rejection swallowed, since leaving when not in is a rejection.
pub const EXIT: &str = "document.exitFullscreen().catch(function () {})";

/// `Runtime.evaluate` parameters for [`EXIT`]: in world `context` when one
/// is known, else in the page's main world.
pub fn exit_params(context: Option<i64>) -> Json {
    let mut fields = vec![("expression", Json::string(EXIT))];
    if let Some(context) = context {
        fields.push(("contextId", Json::number(context as f64)));
    }
    Json::object(fields)
}

/// What a watch's reply said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// The page took an element fullscreen.
    In,
    /// It let it go.
    Out,
    /// The document went — a navigation, a crash, a closed target — or the
    /// reply was not one of the two words. Nothing is fullscreen after that,
    /// and a new watch needs a new world.
    Gone,
}

/// A watch's reply, read. Matched against the two words and never drawn.
pub fn heard(reply: &Result<Json, String>) -> Heard {
    let Ok(reply) = reply else {
        return Heard::Gone;
    };
    if reply.get("exceptionDetails").is_some() {
        return Heard::Gone;
    }
    match reply.path(&["result", "value"]).and_then(Json::as_str) {
        Some("in") => Heard::In,
        Some("out") => Heard::Out,
        _ => Heard::Gone,
    }
}

/// Where the picture goes and how big the page is: the pane less the status
/// row, or the whole pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Layout {
    pub whole: bool,
}

impl Layout {
    /// The row the picture starts on, one-based: 2 under the status row, 1
    /// with the whole pane.
    pub fn page_row(self) -> u32 {
        if self.whole {
            1
        } else {
            2
        }
    }

    /// Rows reserved above the page: 1, or 0.
    pub fn reserved_rows(self) -> u32 {
        self.page_row() - 1
    }

    /// The page's size in cells, which is what the placement asks for.
    pub fn cells(self, metrics: Metrics) -> Cells {
        Cells {
            cols: metrics.cols.max(1),
            rows: if self.whole {
                metrics.rows.max(1)
            } else {
                metrics.usable_rows()
            },
        }
    }

    /// The page's size in pixels: exactly its cells, so that the picture is
    /// never resampled into them.
    pub fn pixels(self, metrics: Metrics) -> (u32, u32) {
        let cells = self.cells(metrics);
        (
            cells.cols.saturating_mul(metrics.cell.0).max(1),
            cells.rows.saturating_mul(metrics.cell.1).max(1),
        )
    }
}

/// The one rule: the whole pane when the page in front is fullscreen and
/// nothing owns the row — a dialog, the url bar, the allow line need the
/// row, and the page is given the shorter viewport again while they have
/// it. Pure; the row owner's answer comes in as a bool so that this module
/// needs nothing of `app`.
pub fn layout(fullscreen: bool, row_owned: bool) -> Layout {
    Layout {
        whole: fullscreen && !row_owned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_is_the_whole_pane_only_while_fullscreen_and_nothing_owns_the_row() {
        assert_eq!(layout(true, false), Layout { whole: true });
        assert_eq!(layout(true, true), Layout { whole: false });
        assert_eq!(layout(false, false), Layout { whole: false });
        assert_eq!(layout(false, true), Layout { whole: false });
        assert_eq!(Layout::default(), layout(false, false));
    }

    #[test]
    fn the_page_starts_on_row_two_less_a_row_or_on_row_one_with_them_all() {
        let metrics = Metrics {
            cols: 80,
            rows: 24,
            cell: (8, 16),
        };
        let under = Layout { whole: false };
        assert_eq!((under.page_row(), under.reserved_rows()), (2, 1));
        assert_eq!(under.cells(metrics), Cells { cols: 80, rows: 23 });
        assert_eq!(under.pixels(metrics), (640, 368));
        assert_eq!(under.pixels(metrics), metrics.usable_pixels());
        let whole = Layout { whole: true };
        assert_eq!((whole.page_row(), whole.reserved_rows()), (1, 0));
        assert_eq!(whole.cells(metrics), Cells { cols: 80, rows: 24 });
        assert_eq!(whole.pixels(metrics), (640, 384));
        // A pane of nothing is still a page of something.
        let none = Metrics {
            cols: 0,
            rows: 0,
            cell: (8, 16),
        };
        assert_eq!(whole.cells(none), Cells { cols: 1, rows: 1 });
        assert_eq!(whole.pixels(none), (8, 16));
    }

    #[test]
    fn a_reply_is_in_out_or_gone_and_an_error_or_a_stranger_is_gone() {
        let value = |word: &str| {
            Ok(Json::object(vec![(
                "result",
                Json::object(vec![
                    ("type", Json::string("string")),
                    ("value", Json::string(word)),
                ]),
            )]))
        };
        assert_eq!(heard(&value("in")), Heard::In);
        assert_eq!(heard(&value("out")), Heard::Out);
        assert_eq!(heard(&value("stale")), Heard::Gone);
        assert_eq!(heard(&value("\x1b]0;x\x07")), Heard::Gone);
        assert_eq!(
            heard(&Err(
                "Runtime.evaluate: Inspected target navigated or closed".to_string()
            )),
            Heard::Gone
        );
        let thrown =
            Json::parse(r#"{"result":{"type":"object"},"exceptionDetails":{"text":"Uncaught"}}"#)
                .expect("JSON");
        assert_eq!(heard(&Ok(thrown)), Heard::Gone);
        assert_eq!(heard(&Ok(Json::empty())), Heard::Gone);
    }

    #[test]
    fn the_watch_expression_names_the_expected_state_and_the_world() {
        assert!(watch_expression(false).contains("!!document.fullscreenElement !== false"));
        assert!(watch_expression(true).contains("!!document.fullscreenElement !== true"));
        let params = watch_params(42, true);
        assert_eq!(params.get("contextId").and_then(Json::as_i64), Some(42));
        assert_eq!(
            params.get("awaitPromise").and_then(Json::as_bool),
            Some(true)
        );
        assert_eq!(
            params.get("returnByValue").and_then(Json::as_bool),
            Some(true)
        );
        assert_eq!(
            params.get("expression").and_then(Json::as_str),
            Some(watch_expression(true).as_str())
        );
        assert_eq!(
            exit_params(Some(7)).get("contextId").and_then(Json::as_i64),
            Some(7)
        );
        assert_eq!(exit_params(None).get("contextId"), None);
        assert_eq!(
            exit_params(None).get("expression").and_then(Json::as_str),
            Some(EXIT)
        );
    }
}
