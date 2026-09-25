//! A page's `alert`, `confirm`, `prompt` and "leave this page?", on the row.
//!
//! A JavaScript dialog is not something a page draws. It is the browser's: the
//! page calls `alert()`, the renderer stops in the middle of the script that
//! called it, and the engine asks whoever is driving it — over CDP, as
//! `Page.javascriptDialogOpening` — what the person said. Until somebody
//! answers with `Page.handleJavaScriptDialog` the page does nothing at all: no
//! frames, no `Runtime.evaluate`, no screenshot. So a dialog cannot be left
//! for later, and it cannot be answered by the program on the person's behalf
//! either — which is what this program did, `accept: false` to every one of
//! them, and what made a "do you want to delete this?" a silent no and a
//! `prompt()` a silent `null` (issue #7).
//!
//! What this module is instead is the dialog as a thing on the status row: a
//! sentence, a hint at which keys answer it, and — for a `prompt()` — a line
//! being typed into. The row is the one place this program draws text of its
//! own, the url bar already taught it to be a line with a cursor in it, and a
//! dialog is a question the size of a line. Drawing a box over the page would
//! be a picture of a dialog laid over a picture of a page, with nothing
//! underneath either of them that could take a click.
//!
//! Nothing here talks to the engine or draws anything. It reads the event,
//! decides what a key means, and says what the engine is to be told, so that
//! every one of those can be tested a key at a time; [`crate::app`] does the
//! talking and [`crate::screen`] the drawing.

use crate::input::{Key, KeyAction, KeyInput};
use crate::json::Json;
use crate::line::{Edit, Line};

/// Which of the four questions a page can ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `alert()`: something to be read, and nothing to decide.
    Alert,
    /// `confirm()`: yes or no.
    Confirm,
    /// `prompt()`: a line of text, or nothing.
    Prompt,
    /// A `beforeunload` handler that asked: stay on this page, or leave it.
    BeforeUnload,
}

/// A dialog a page has open, waiting on the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub kind: Kind,
    /// What the page asked, exactly as it asked it — newlines and all. What is
    /// drawn is [`Dialog::caption`], which is this made to fit on one row.
    pub message: String,
    /// The page that asked, which is not always the page in front of it: an
    /// iframe can call `alert()` too.
    pub url: String,
    /// What is being typed into a `prompt()`, starting with the default the
    /// page offered, all of it selected. Empty and unused for every other
    /// kind.
    pub line: Line,
}

/// What a key did to a dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Nothing decided yet: a key that means nothing here, a key let go, or a
    /// character typed into a prompt.
    Waiting,
    /// OK, yes, leave: the button a browser would draw on the right.
    Accept,
    /// Cancel, no, stay.
    Dismiss,
    /// `ctrl+q`, which quits from a dialog as it does from the url bar. The
    /// page is not answered: the engine is about to be stopped, and it goes
    /// with it.
    Quit,
}

impl Dialog {
    /// Read a `Page.javascriptDialogOpening`.
    ///
    /// `None` for a type this program has no answer for. The protocol has had
    /// the same four since it had any, but a fifth would be a question with no
    /// keys to answer it, and a row that showed one would be a page stopped
    /// behind a sentence nobody can get rid of; left unread, it stays what it
    /// is in the engine and the tab can still be closed.
    pub fn opening(params: &Json) -> Option<Dialog> {
        let kind = match params.get("type").and_then(Json::as_str)? {
            "alert" => Kind::Alert,
            "confirm" => Kind::Confirm,
            "prompt" => Kind::Prompt,
            "beforeunload" => Kind::BeforeUnload,
            _ => return None,
        };
        let text = |key: &str| {
            params
                .get(key)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let line = match kind {
            Kind::Prompt => Line::selected(text("defaultPrompt")),
            _ => Line {
                text: String::new(),
                whole: false,
            },
        };
        Some(Dialog {
            kind,
            message: text("message"),
            url: text("url"),
            line,
        })
    }

    /// What one key does to the dialog.
    ///
    /// The keys are the ones a dialog box has always had, read off the buttons
    /// a browser would have drawn: an alert has one, so any key is it; a
    /// confirm has two, and `y` and `n` are what a terminal has taught for
    /// those longer than any browser has — with Enter and Escape for the
    /// reflex that expects a dialog's default and its cancel. A prompt is a
    /// line being typed, so every key but Enter and Escape is typing.
    ///
    /// A key let go is nothing, whatever the kind — it follows a press that
    /// has already been answered, and an alert that took the release of the
    /// key that opened the tab would be gone before it was seen. So is a
    /// modifier pressed on its own, which the Kitty protocol reports as a key
    /// of its own: somebody reaching for shift to type a capital into a prompt
    /// has not answered anything yet.
    pub fn step(&mut self, key: &KeyInput) -> Answer {
        if key.action == KeyAction::Release || matches!(key.key, Key::Other(_)) {
            return Answer::Waiting;
        }
        if key.key == Key::Char('q') && key.mods.ctrl() {
            return Answer::Quit;
        }
        match self.kind {
            Kind::Alert => Answer::Accept,
            Kind::Confirm | Kind::BeforeUnload => {
                let plain = !key.mods.ctrl() && !key.mods.alt();
                match key.key {
                    Key::Enter => Answer::Accept,
                    Key::Escape => Answer::Dismiss,
                    Key::Char('y' | 'Y') if plain => Answer::Accept,
                    Key::Char('n' | 'N') if plain => Answer::Dismiss,
                    _ => Answer::Waiting,
                }
            }
            Kind::Prompt => match self.line.step(key) {
                Edit::Typing => Answer::Waiting,
                Edit::Go => Answer::Accept,
                Edit::Cancel => Answer::Dismiss,
                Edit::Quit => Answer::Quit,
            },
        }
    }

    /// The parameters of the `Page.handleJavaScriptDialog` that answers it.
    ///
    /// `promptText` only for a prompt that was accepted: it is what
    /// `prompt()` returns, and a prompt that was dismissed returns `null`
    /// whatever was typed. Anything that is not [`Answer::Accept`] is a no —
    /// there is no third button, and a page asked is a page answered.
    pub fn reply(&self, answer: Answer) -> Json {
        let accept = answer == Answer::Accept;
        let mut fields = vec![("accept", Json::Bool(accept))];
        if accept && self.kind == Kind::Prompt {
            fields.push(("promptText", Json::string(&self.line.text)));
        }
        Json::object(fields)
    }

    /// The question, as one line of text.
    ///
    /// A page writes a dialog's message for a box that wraps it, so newlines
    /// and tabs in it are common and meant — and on a status row every one of
    /// them is a cursor sent somewhere else. So every run of whitespace and
    /// control characters becomes one space, and the ends are trimmed.
    ///
    /// An alert says it is one, because a sentence alone on the row reads as
    /// the page's title and the page is not going to move until a key is
    /// pressed. A confirm and a prompt need no label: they end in the keys
    /// that answer them. The page leaving asks its own question, because
    /// Chromium no longer passes on what a `beforeunload` handler asked for —
    /// the message is empty from any engine this was tried against — and a
    /// bare "y/n" would be a question about nothing.
    pub fn caption(&self) -> String {
        let message = one_line(&self.message);
        match self.kind {
            Kind::Alert if message.is_empty() => "alert".to_string(),
            Kind::Alert => format!("alert: {message}"),
            Kind::BeforeUnload if message.is_empty() => "leave this page?".to_string(),
            Kind::BeforeUnload => format!("leave this page? {message}"),
            Kind::Confirm | Kind::Prompt => message,
        }
    }

    /// Which keys answer it, as few words as will say so.
    pub fn hint(&self) -> &'static str {
        match self.kind {
            Kind::Alert => "any key",
            Kind::Confirm | Kind::BeforeUnload => "y/n",
            Kind::Prompt => "enter/esc",
        }
    }

    /// Whether the row is a line being typed into, and so has the cursor.
    ///
    /// Only a prompt. The others are answered by a key and have nowhere for a
    /// cursor to be; showing one would say there was something to type.
    pub fn typing(&self) -> bool {
        self.kind == Kind::Prompt
    }
}

/// Whitespace and control characters, in runs, as one space each.
fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut gap = false;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            gap = true;
            continue;
        }
        if gap && !out.is_empty() {
            out.push(' ');
        }
        gap = false;
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Mods;

    fn opening(params: &str) -> Dialog {
        Dialog::opening(&Json::parse(params).expect("the test's own JSON")).expect("a dialog")
    }

    fn of(kind: &str) -> Dialog {
        opening(&format!(
            r#"{{"url":"https://example.com/","message":"sure?","type":"{kind}",
                "hasBrowserHandler":false,"defaultPrompt":""}}"#
        ))
    }

    fn typed(c: char) -> KeyInput {
        KeyInput {
            key: Key::Char(c),
            mods: Mods::default(),
            action: KeyAction::Press,
            text: Some(c),
        }
    }

    fn key(k: Key, mods: u32) -> KeyInput {
        KeyInput {
            key: k,
            mods: Mods(mods),
            action: KeyAction::Press,
            text: None,
        }
    }

    fn released(mut key: KeyInput) -> KeyInput {
        key.action = KeyAction::Release;
        key
    }

    /// Shift, pressed on its own, as the Kitty protocol reports it.
    const LEFT_SHIFT: u32 = 57441;

    #[test]
    fn the_opening_event_says_which_kind_it_is_and_what_it_asked() {
        // As `chromium-shell` sends it.
        let dialog = opening(
            r#"{"url":"https://example.com/form","frameId":"F","message":"Your name?",
                "type":"prompt","hasBrowserHandler":false,"defaultPrompt":"someone"}"#,
        );
        assert_eq!(dialog.kind, Kind::Prompt);
        assert_eq!(dialog.message, "Your name?");
        assert_eq!(dialog.url, "https://example.com/form");
        assert_eq!(dialog.line, Line::selected("someone"));

        assert_eq!(of("alert").kind, Kind::Alert);
        assert_eq!(of("confirm").kind, Kind::Confirm);
        assert_eq!(of("beforeunload").kind, Kind::BeforeUnload);
        // Only a prompt has a line; the others' is empty and not selected, so
        // nothing about them looks like typing.
        assert_eq!(of("confirm").line.text, "");
        assert!(!of("confirm").line.whole);

        // A kind nobody has keys for is not a dialog this program shows.
        let odd = Json::parse(r#"{"type":"payment","message":"x","url":"y"}"#).expect("JSON");
        assert_eq!(Dialog::opening(&odd), None);
        assert_eq!(Dialog::opening(&Json::empty()), None);

        // And a field the engine left out is empty rather than a failure.
        let bare = Json::parse(r#"{"type":"alert"}"#).expect("JSON");
        let bare = Dialog::opening(&bare).expect("an alert");
        assert_eq!((bare.message.as_str(), bare.url.as_str()), ("", ""));
    }

    #[test]
    fn an_alert_is_dismissed_by_any_key_but_a_bare_modifier() {
        for answer in [
            typed('x'),
            typed('n'),
            key(Key::Enter, 0),
            key(Key::Escape, 0),
            key(Key::Char(' '), 0),
            key(Key::Down, 0),
        ] {
            assert_eq!(of("alert").step(&answer), Answer::Accept, "{answer:?}");
        }
        let mut alert = of("alert");
        assert_eq!(alert.step(&key(Key::Other(LEFT_SHIFT), 0)), Answer::Waiting);
        assert_eq!(alert.step(&released(typed('x'))), Answer::Waiting);
        // An alert has one button, so there is no no: accepting is all that
        // any key can mean, and it is what the page is told.
        assert_eq!(
            alert.reply(Answer::Accept).to_string(),
            r#"{"accept":true}"#
        );
    }

    #[test]
    fn a_confirm_takes_y_n_enter_and_escape_and_ignores_the_rest() {
        for kind in ["confirm", "beforeunload"] {
            for (pressed, wanted) in [
                (typed('y'), Answer::Accept),
                (typed('Y'), Answer::Accept),
                (key(Key::Enter, 0), Answer::Accept),
                (typed('n'), Answer::Dismiss),
                (typed('N'), Answer::Dismiss),
                (key(Key::Escape, 0), Answer::Dismiss),
                (typed('x'), Answer::Waiting),
                (key(Key::Char(' '), 0), Answer::Waiting),
                (key(Key::Tab, 0), Answer::Waiting),
                (key(Key::Char('y'), Mods::CTRL), Answer::Waiting),
                (key(Key::Char('n'), Mods::ALT), Answer::Waiting),
                (key(Key::Other(LEFT_SHIFT), Mods::SHIFT), Answer::Waiting),
                (released(typed('y')), Answer::Waiting),
            ] {
                assert_eq!(of(kind).step(&pressed), wanted, "{kind}: {pressed:?}");
            }
        }
        // A key that means nothing leaves the question where it was.
        let mut confirm = of("confirm");
        confirm.step(&typed('x'));
        assert_eq!(confirm, of("confirm"));
    }

    #[test]
    fn a_prompt_starts_with_its_default_selected_and_sends_back_what_was_typed() {
        let asked = r#"{"url":"u","message":"name?","type":"prompt","defaultPrompt":"default"}"#;

        // Enter straight away: the default is the answer.
        let mut prompt = opening(asked);
        assert_eq!(prompt.step(&key(Key::Enter, 0)), Answer::Accept);
        assert_eq!(
            prompt.reply(Answer::Accept).to_string(),
            r#"{"accept":true,"promptText":"default"}"#
        );

        // Typing replaces it, because it was selected; a shift on the way to a
        // capital does not spend the selection.
        let mut prompt = opening(asked);
        assert_eq!(
            prompt.step(&key(Key::Other(LEFT_SHIFT), Mods::SHIFT)),
            Answer::Waiting
        );
        assert!(prompt.line.whole);
        for c in ['X', 'y'] {
            assert_eq!(prompt.step(&typed(c)), Answer::Waiting);
        }
        assert_eq!(prompt.line.text, "Xy");
        // A y in a prompt is a letter, not a yes.
        assert_eq!(prompt.step(&key(Key::Backspace, 0)), Answer::Waiting);
        assert_eq!(prompt.line.text, "X");
        assert_eq!(prompt.step(&key(Key::Enter, 0)), Answer::Accept);
        assert_eq!(
            prompt.reply(Answer::Accept).to_string(),
            r#"{"accept":true,"promptText":"X"}"#
        );

        // Escape is `null`, whatever was typed: nothing of it is sent.
        let mut prompt = opening(asked);
        prompt.step(&typed('z'));
        assert_eq!(prompt.step(&key(Key::Escape, 0)), Answer::Dismiss);
        assert_eq!(
            prompt.reply(Answer::Dismiss).to_string(),
            r#"{"accept":false}"#
        );
    }

    #[test]
    fn ctrl_q_quits_from_every_kind_of_dialog() {
        for kind in ["alert", "confirm", "prompt", "beforeunload"] {
            assert_eq!(
                of(kind).step(&key(Key::Char('q'), Mods::CTRL)),
                Answer::Quit,
                "{kind}"
            );
        }
        // And a q on its own is a q: an alert's any key, a confirm's nothing,
        // a prompt's letter.
        assert_eq!(of("alert").step(&typed('q')), Answer::Accept);
        assert_eq!(of("confirm").step(&typed('q')), Answer::Waiting);
        assert_eq!(of("prompt").step(&typed('q')), Answer::Waiting);
    }

    #[test]
    fn a_message_is_one_line_however_the_page_wrote_it() {
        let mut dialog = of("confirm");
        dialog.message = "  Delete\n\n3 files?\r\n\tThis\u{7}cannot be undone.  ".to_string();
        assert_eq!(dialog.caption(), "Delete 3 files? This cannot be undone.");
        // Wide characters are characters, and are kept.
        dialog.message = "\u{524a}\u{9664}\u{3057}\u{307e}\u{3059}\u{304b}\u{ff1f}\n".to_string();
        assert_eq!(
            dialog.caption(),
            "\u{524a}\u{9664}\u{3057}\u{307e}\u{3059}\u{304b}\u{ff1f}"
        );

        // The kinds that need a word of their own have one.
        let mut alert = of("alert");
        alert.message = "saved\n".to_string();
        assert_eq!(alert.caption(), "alert: saved");
        alert.message = "\n".to_string();
        assert_eq!(alert.caption(), "alert");
        let mut leaving = of("beforeunload");
        leaving.message.clear();
        assert_eq!(leaving.caption(), "leave this page?");
        assert_eq!(of("prompt").caption(), "sure?");

        assert_eq!(of("alert").hint(), "any key");
        assert_eq!(of("confirm").hint(), "y/n");
        assert_eq!(of("beforeunload").hint(), "y/n");
        assert_eq!(of("prompt").hint(), "enter/esc");
        assert!(of("prompt").typing());
        assert!(!of("confirm").typing());
    }

    #[test]
    fn the_reply_is_what_the_engine_is_told() {
        let confirm = of("confirm");
        assert_eq!(
            confirm.reply(Answer::Accept).to_string(),
            r#"{"accept":true}"#
        );
        assert_eq!(
            confirm.reply(Answer::Dismiss).to_string(),
            r#"{"accept":false}"#
        );
        let leaving = of("beforeunload");
        assert_eq!(
            leaving.reply(Answer::Accept).to_string(),
            r#"{"accept":true}"#,
            "yes is leave"
        );
        let mut prompt = of("prompt");
        prompt.step(&typed('"'));
        assert_eq!(
            prompt.reply(Answer::Accept).to_string(),
            r#"{"accept":true,"promptText":"\""}"#,
            "what was typed is sent as a string, quotes and all"
        );
        // An empty answer is an answer: `prompt()` returns "" and not null.
        let prompt = of("prompt");
        assert_eq!(
            prompt.reply(Answer::Accept).to_string(),
            r#"{"accept":true,"promptText":""}"#
        );
    }
}
