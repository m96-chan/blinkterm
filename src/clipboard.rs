//! The host's clipboard: what goes out to it, and what a paste may put in a
//! line.
//!
//! The engine has a clipboard of its own, and `ctrl+c` and `ctrl+v` on a page
//! already use it — measured: select a paragraph, send `ctrl+c`, focus a
//! textarea, send `ctrl+v`, and the textarea receives the paragraph as an
//! `insertFromPaste`. What that clipboard never does is touch the person's.
//! `navigator.clipboard` does not exist on a `data:` page, which is not a
//! secure context, and on an `http://127.0.0.1` page its `writeText` rejects
//! with `NotAllowedError`, gesture or none. So nothing gets out of a page on
//! its own, and this module is how something does.
//!
//! Out is OSC 52, written to the terminal, which puts it on the host's
//! clipboard: a terminal is the only thing this program can reach that can.
//! In is the terminal's own paste key — `ctrl+shift+v`, a middle click — which
//! every terminal this runs in keeps for itself, and which arrives as a
//! bracketed paste ([`crate::input`]). This program never asks a terminal for
//! its clipboard (`OSC 52 ; c ; ?`): tOS answers that with nothing unless
//! `allow_clipboard_read` is on, Kitty asks the person, and a program that
//! read a clipboard behind somebody's back is what those refusals are for.
//!
//! What is copied is the page's selection or the url, on `alt+c` and `alt+u`;
//! [`crate::app`] has why those keys. The selection is asked of the page with
//! [`SELECTION`], because what the person dragged over is the engine's to
//! know, and the answer goes out as base64 — which is `[A-Za-z0-9+/=]` and
//! nothing else, the one alphabet a terminal cannot be spoken to in, so a
//! selection needs no sanitizing on the way out whatever the page put in it.
//!
//! Nothing here talks to the engine or the terminal. It builds the bytes and
//! reads the answers, so that each can be tested without either.

use crate::json::Json;
use crate::text;

/// The most a copy may be, in bytes of text.
///
/// tOS's `MAX_CLIPBOARD_BYTES`, above which its compositor refuses the write
/// whole and says so. Refusing here first means the person reads one sentence
/// about it rather than two, and a terminal with no limit of its own is not
/// handed a megabyte of base64 on the say-so of a page's selection. The same
/// number as [`crate::input::PASTE_LIMIT`], for the same compositor.
pub const MAX_COPY: usize = 64 * 1024;

/// The bytes that put `text` on the host's clipboard: `OSC 52 ; c ; base64
/// ST`. `None` over [`MAX_COPY`] — refused whole, never cut, as a paste is.
///
/// `c` and only `c`: the clipboard, which is what an explicit copy writes.
/// `p` is the primary selection, where a mouse puts what it drags over, and
/// the mouse here selects on the page and not on the terminal's grid, so
/// there is no primary to keep in step. `ST` (`ESC \`) rather than `BEL` to
/// end it: tOS's terminal, Kitty, WezTerm and Ghostty all take it, and it
/// cannot ring.
pub fn osc52(text: &str) -> Option<Vec<u8>> {
    if text.len() > MAX_COPY {
        return None;
    }
    let mut out = b"\x1b]52;c;".to_vec();
    out.extend_from_slice(crate::base64::encode(text.as_bytes()).as_bytes());
    out.extend_from_slice(b"\x1b\\");
    Some(out)
}

/// What the page has selected, as a script for `Runtime.evaluate`.
///
/// The document's selection as text, from the focused frame: a selection in a
/// same-origin iframe is invisible from the top document (measured), so the
/// focus is followed down through `activeElement` first, eight frames deep at
/// most and not across an origin, where `contentDocument` is `null`. Then, if
/// that is empty, the selected range of a focused `<textarea>` or `<input>` —
/// Chromium already returns that from `getSelection()` (measured: `"beta"`
/// out of `"alpha beta gamma"`), and the fallback is for an engine that does
/// not.
pub const SELECTION: &str = "(function(){var d=document;for(var i=0;i<8;i++){\
var a=d.activeElement;if(!a||a.tagName!=='IFRAME')break;try{var c=a.contentDocument;\
if(!c)break;d=c}catch(e){break}}var s=String(d.getSelection&&d.getSelection());\
if(!s){var f=d.activeElement;if(f&&typeof f.selectionStart==='number'&&\
f.selectionEnd>f.selectionStart){s=f.value.substring(f.selectionStart,f.selectionEnd)}}\
return s})()";

/// The parameters of the `Runtime.evaluate` that runs [`SELECTION`].
///
/// `returnByValue`, so that the reply carries the string rather than a handle
/// to it in the page.
pub fn selection_params() -> Json {
    Json::object(vec![
        ("expression", Json::string(SELECTION)),
        ("returnByValue", Json::Bool(true)),
    ])
}

/// The selection out of the reply to [`selection_params`]; `None` for a reply
/// that is not a string, which is a page that threw.
pub fn selection(reply: &Json) -> Option<String> {
    reply
        .path(&["result", "value"])
        .and_then(Json::as_str)
        .map(str::to_string)
}

/// What a paste into a line keeps: a url bar's, or a `prompt()`'s.
///
/// The text with everything [`crate::text::sanitize`] would not keep as it
/// is taken out — and taken out, not replaced. Where the sanitizer makes a
/// line break a space, because the row is one line and a sentence split over
/// two still reads with a space between the halves, a line has a different
/// reason: a url copied with its trailing newline is the common case, and a
/// url with a space in the middle is a different url. So `\r`, `\n` and tab
/// go, with the controls, the bidi overrides and the invisible characters the
/// sanitizer drops anyway, and what is left is plain text as the row will
/// show it: what is seen is what will be sent.
pub fn one_line(pasted: &str) -> String {
    pasted.chars().filter(|&c| text::is_plain(c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_is_osc_52_to_the_clipboard_ended_by_st() {
        assert_eq!(osc52("hi"), Some(b"\x1b]52;c;aGk=\x1b\\".to_vec()));
        assert_eq!(osc52(""), Some(b"\x1b]52;c;\x1b\\".to_vec()));
    }

    #[test]
    fn a_copy_over_the_limit_is_refused_whole() {
        assert!(osc52(&"a".repeat(MAX_COPY)).is_some());
        assert_eq!(osc52(&"a".repeat(MAX_COPY + 1)), None);
    }

    /// The terminal has to make of the bytes what they were meant as.
    #[test]
    fn a_terminal_reads_a_copy_as_a_clipboard_store() {
        for copied in ["hi", "日本語 and \x1b]0;x\x07 and a\nnewline", ""] {
            let mut terminal = tos_term::Terminal::new(80, 24, tos_term::TerminalConfig::default());
            terminal.advance(&osc52(copied).expect("under the limit"));
            let stored: Vec<_> = terminal
                .take_events()
                .into_iter()
                .filter_map(|event| match event {
                    tos_term::TermEvent::ClipboardStore { selection, data } => {
                        Some((selection, data))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                stored,
                vec![('c', copied.as_bytes().to_vec())],
                "{copied:?}"
            );
            // And nothing of it reached the screen or was answered.
            assert!(terminal.take_output().is_empty());
        }
    }

    #[test]
    fn the_selection_is_read_out_of_the_reply() {
        let reply = Json::parse(r#"{"result":{"type":"string","value":"some words"}}"#)
            .expect("the test's own JSON");
        assert_eq!(selection(&reply), Some("some words".to_string()));
        let empty = Json::parse(r#"{"result":{"type":"string","value":""}}"#).expect("JSON");
        assert_eq!(selection(&empty), Some(String::new()));
        let threw =
            Json::parse(r#"{"result":{"type":"object","subtype":"error"},"exceptionDetails":{}}"#)
                .expect("JSON");
        assert_eq!(selection(&threw), None);
        assert_eq!(
            selection_params()
                .get("returnByValue")
                .and_then(Json::as_bool),
            Some(true)
        );
    }

    #[test]
    fn a_line_keeps_the_text_of_a_paste_and_none_of_its_breaks() {
        assert_eq!(one_line("https://x/\r\n"), "https://x/");
        assert_eq!(one_line("https://x/\n"), "https://x/");
        assert_eq!(one_line("a\tb"), "ab");
        assert_eq!(one_line("\x1b]0;x\x07example.com"), "]0;xexample.com");
        assert_eq!(one_line("exa\u{202e}mple\u{200b}.com"), "example.com");
        assert_eq!(one_line("line\u{2028}two"), "linetwo");
        assert_eq!(
            one_line("日本語 の url"),
            "日本語 の url",
            "spaces and CJK stay"
        );
        // What is left is what the row will show, unchanged.
        for pasted in ["a\r\nb\u{9b}c\u{feff}", "plain", "\t\n"] {
            let line = one_line(pasted);
            assert_eq!(text::sanitize(&line), line.as_str(), "{pasted:?}");
        }
    }
}
