//! What a page may say on the row, and what it may not.
//!
//! The status row is the one place this program writes text to the
//! terminal, and most of that text is the page's: `document.title`, which a
//! script sets to anything it likes; the url; the message of an `alert()`.
//! A terminal executes what it is sent, so a title of `\x1b]0;x\x07` is not
//! a title but a command — set the window title, here, and with other
//! sequences, write the clipboard, or query the terminal and have the answer
//! typed back as keystrokes. The page body cannot do this: it arrives as
//! pixels and goes out as a graphics payload. The row can, and this module
//! is what stops it.
//!
//! It is one pure function applied in two places. Where a string comes off
//! the engine's pipe and becomes a `Tab`'s title or url or a `Dialog`'s
//! message — the parsers in [`crate::load`], [`crate::tabs`],
//! [`crate::dialog`] and [`crate::cdp`] — so that the rest of the program
//! holds plain text and never has to remember. And again in
//! [`crate::screen`], as the row is built, so that a string which reached
//! the row by a path nobody sanitized — a test, a paste, a module written
//! next year — is still only text. The rule is idempotent, so the second
//! pass costs a scan and changes nothing. The second pass is a filter and not
//! a `debug_assert`, on purpose: a release build has no `debug_assert`, and
//! with `panic = "abort"` an assertion that did fire would hand a hostile
//! title the one thing it should never get, which is a way to stop the
//! browser.
//!
//! What goes is decided by what a terminal does with it, not by a list of
//! known attacks. A line break of any kind — tab, newline, carriage return,
//! form feed, NEL, the Unicode line and paragraph separators — becomes one
//! space, because the row is one line and a page writes `Delete\n3 files?`
//! for a box that wraps; gluing the halves together misreads, a space does
//! not. Every other control character goes: C0, DEL and C1, which are the
//! bytes a terminal executes. C1 is included although it is rarely sent,
//! because a terminal in UTF-8 mode may take U+009B as CSI and U+009D as OSC,
//! and which terminal this runs in is not this program's to assume. Unicode's
//! bidi controls go, because a url is the one thing on the row the person is
//! meant to trust and `https://evil.example/\u{202e}moc.knab` reads as
//! `bank.com` in a terminal that lays out bidi text; nothing is lost by it,
//! since a terminal that does bidi at all does it from the characters' own
//! directionality and a title in Hebrew still reads. And the invisible format
//! characters go — zero-width spaces and joiners, the soft hyphen, the byte
//! order mark, the tag characters — because they have no glyph, so a url with
//! one in it looks like a url without it while being a different string, and
//! [`crate::screen::width`] counts each as a cell, so the row comes out short.
//! Dropping a zero-width joiner splits an emoji family into its members, which
//! is the honest rendering for a row that already says it knows no emoji
//! sequences.
//!
//! What stays is everything with a glyph. Visible text is the page's to write
//! and the person's to read, as in any browser's tab strip: a Cyrillic `а`
//! that looks like a Latin `a` is not this program's problem any more than it
//! is Firefox's, since the page body shows the same page. Combining marks
//! stay, and [`crate::screen::width`] counts them with the letter they sit
//! on. U+FFFD, the no-break space, the
//! ideographic space and the private-use characters that a patched font draws
//! as icons all stay.
//!
//! What it deliberately does not do: collapse runs of spaces, which is
//! [`crate::dialog`]'s presentation rule and not a security one; trim; clip;
//! or mark what it removed. A U+FFFD in place of a stray control byte would be
//! a claim about intent the byte does not merit, and it is ambiguous-width in
//! East Asian terminals, which is the reason the strip's own marks are ASCII.

use std::borrow::Cow;

/// The text with everything a terminal would execute, or a reader would be
/// misled by, taken out. Borrowed when there was nothing to take.
///
/// Borrowed because the common case — a title that is plain text — is what
/// the row asks for on every redraw, and one scan with no allocation is what
/// that should cost. A caller that keeps the result calls `into_owned`.
pub fn sanitize(text: &str) -> Cow<'_, str> {
    if text.chars().all(is_plain) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match place(c) {
            Place::Keep => out.push(c),
            Place::Space => out.push(' '),
            Place::Drop => {}
        }
    }
    Cow::Owned(out)
}

/// Whether [`sanitize`] keeps this character as it is.
///
/// For a caller that is building text a character at a time — a line being
/// typed into — and would rather refuse a character than have it vanish from
/// the row later.
pub fn is_plain(c: char) -> bool {
    matches!(place(c), Place::Keep)
}

/// What [`sanitize`] does with one character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Keep,
    Space,
    Drop,
}

fn place(c: char) -> Place {
    match c {
        // A break in a line, on a row that is one line: a space.
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}' => Place::Space,
        // C0, DEL and C1: the bytes a terminal executes.
        c if c.is_control() => Place::Drop,
        // Unicode's Bidi_Control set, which can make one url read as another.
        '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => {
            Place::Drop
        }
        // Format characters with no glyph: invisible, counted as a cell, and
        // able to make two different strings look the same.
        '\u{ad}'
        | '\u{34f}'
        | '\u{180e}'
        | '\u{200b}'..='\u{200d}'
        | '\u{2060}'..='\u{2064}'
        | '\u{206a}'..='\u{206f}'
        | '\u{feff}'
        | '\u{fff9}'..='\u{fffb}'
        | '\u{e0000}'..='\u{e007f}' => Place::Drop,
        _ => Place::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_escape_sequence_in_a_title_is_only_its_letters() {
        assert_eq!(sanitize("\x1b]0;x\x07"), "]0;x");
        assert_eq!(sanitize("a\rb"), "a b");
        assert_eq!(sanitize("Delete\n3 files?"), "Delete 3 files?");
        assert_eq!(sanitize("\x1b[2J\x1b[H"), "[2J[H");
    }

    #[test]
    fn c1_controls_go_whether_they_came_as_bytes_or_escapes() {
        assert_eq!(sanitize("\u{9b}2J"), "2J");
        assert_eq!(sanitize("\u{9d}0;x\u{9c}"), "0;x");
        assert_eq!(sanitize("\u{85}"), " ", "NEL is a line break");
        assert_eq!(sanitize("\u{7f}"), "");
    }

    #[test]
    fn a_bidi_override_cannot_turn_one_url_into_another() {
        assert_eq!(
            sanitize("https://evil.example/\u{202e}moc.knab"),
            "https://evil.example/moc.knab"
        );
        // All twelve of Unicode's Bidi_Control property: the three marks, the
        // five embeddings and overrides, and the four isolates.
        let bidi_controls = [
            '\u{61c}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}',
            '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ];
        for c in bidi_controls {
            assert_eq!(sanitize(&c.to_string()), "", "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn invisible_characters_go_and_visible_ones_stay() {
        for invisible in [
            "\u{200b}",
            "\u{200d}",
            "\u{feff}",
            "\u{ad}",
            "\u{e0001}\u{e0041}",
        ] {
            assert_eq!(sanitize(invisible), "", "{invisible:?}");
        }
        for visible in [
            "日本語",
            "café",
            "😀",
            "\u{fffd}",
            "a\u{a0}b",
            "\u{3000}",
            "\u{e000}",
            "ﾃｽﾄ",
            // Right-to-left without an override is text, and reads.
            "שלום",
        ] {
            assert_eq!(sanitize(visible), visible);
        }
    }

    #[test]
    fn plain_text_is_handed_back_without_a_copy() {
        assert!(matches!(
            sanitize("Example  —  https://example.com"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(sanitize("a\rb"), Cow::Owned(_)));
    }

    #[test]
    fn every_character_is_kept_spaced_or_dropped_and_sanitizing_twice_is_once() {
        for c in (0..=0x10ffff).filter_map(char::from_u32) {
            let place = place(c);
            assert_eq!(is_plain(c), place == Place::Keep, "U+{:04X}", c as u32);
            assert!(
                !(is_plain(c) && c.is_control()),
                "U+{:04X} is a control and was kept",
                c as u32
            );
        }
        assert!(is_plain(' '), "the space a break becomes is itself plain");
        for sample in [
            "\x1b]0;x\x07 a\r\nb\u{202e}c\u{200b}d\u{9b}e",
            "plain",
            "\t\t\u{2028}",
            "日本\u{feff}語\u{e0041}",
            "",
        ] {
            let once = sanitize(sample);
            assert_eq!(sanitize(&once), once, "{sample:?}");
            assert!(matches!(sanitize(&once), Cow::Borrowed(_)), "{sample:?}");
        }
    }
}
