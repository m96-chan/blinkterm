//! What is on the status row, and what a press on it means.
//!
//! The status row is the one part of the screen that is this program's and
//! not the page's: everything under it is the page's picture, and a press
//! there is forwarded as the page's click. With more than one tab the row is
//! the strip — `1 title  2 title  [3 title]  https://…`, with `+N` at either
//! end when not every tab fits — and a press on it is answered with the same
//! commands the keys have: a left press on a tab's label switches to it (as
//! `alt+3` does), a middle press closes *that* tab (as `ctrl+w` does the one
//! in front), a left press on `+N` switches to the nearest tab past that end
//! (which brings it into the window, since the window always holds the tab
//! in front), and a left press on the url opens the url bar with it (as
//! `ctrl+l` does). With a single tab the whole row is the url.
//!
//! # Read against the last draw
//!
//! The press is not measured against the state as it is when the press
//! arrives: it is read against the layout the last draw put on the row, in
//! cells ([`Span`]), which the draw records as it writes the bytes. The
//! width a label was given and the columns a press on it is looked for in
//! then come out of one pass and cannot disagree. A press that lands between
//! a change and the redraw that shows it is read against what the person
//! saw, which is the right answer: they clicked what was on the screen.
//!
//! When anything else owns the row — the url bar, find, a list, a question,
//! the offer to restore — the layout is empty and a press on the row does
//! nothing: the strip is not there to be clicked.
//!
//! # Press, not release
//!
//! A tab is switched on the press, as a browser's tab strip does it on
//! mousedown: the feedback is immediate and a press that wanders off the
//! label before it is let go still switched. The release that follows is
//! swallowed — the page was never told of the press, and a release on its
//! own is half a click the page has no use for.
//!
//! # The last tab
//!
//! A middle press never closes the last tab. `ctrl+w` on the last tab quits,
//! which is what a key held on purpose means; a stray middle click on the
//! only label in the row — or a paste button that happened to be over it —
//! should not end the program. With one tab there is no label anyway: the
//! row is the url.
//!
//! # Left for later
//!
//! The wheel over the strip (a trackpad sends storms of notches that would
//! need a debounce measured on hardware), a highlight of the tab under the
//! pointer, dragging to reorder, close boxes on each tab, and a double click
//! on empty strip for a new tab.

/// One thing a run of the strip stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// `+N` at the left: `N` tabs before the window.
    Before(usize),
    /// A tab's label, by its index in the strip (zero-based).
    Tab(usize),
    /// `+N` at the right: `N` tabs after the window.
    After(usize),
    /// The words at the right-hand end with the url in them, or the whole
    /// row when there is one tab.
    Url,
}

/// The cells a part was drawn in: `from..to`, zero-based columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// What was drawn there.
    pub part: Part,
    /// The first column it covers.
    pub from: usize,
    /// One past the last column it covers.
    pub to: usize,
}

/// What a press did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Bring this tab, by index, to the front.
    Switch(usize),
    /// Close this tab, by index.
    Close(usize),
    /// Open the url bar with the url of the tab in front.
    EditUrl,
    /// The press was on nothing, or with a button that means nothing here.
    Nothing,
}

/// The part drawn in `column`, if any: the gaps between the runs are
/// nobody's, so a press there does nothing.
pub fn hit(spans: &[Span], column: usize) -> Option<Part> {
    spans
        .iter()
        .find(|span| span.from <= column && column < span.to)
        .map(|span| span.part)
}

/// What a press of `button` (as SGR numbers it: 0 left, 1 middle, 2 right)
/// on `part` does, with `tabs` tabs open.
pub fn step(part: Option<Part>, button: Option<u32>, tabs: usize) -> Step {
    match (part, button) {
        (Some(Part::Tab(index)), Some(0)) => Step::Switch(index),
        (Some(Part::Before(n)), Some(0)) if n > 0 => Step::Switch(n - 1),
        (Some(Part::After(n)), Some(0)) if n > 0 && n <= tabs => Step::Switch(tabs - n),
        (Some(Part::Url), Some(0)) => Step::EditUrl,
        (Some(Part::Tab(index)), Some(1)) if tabs >= 2 => Step::Close(index),
        _ => Step::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans() -> Vec<Span> {
        vec![
            Span {
                part: Part::Before(3),
                from: 0,
                to: 2,
            },
            Span {
                part: Part::Tab(3),
                from: 4,
                to: 14,
            },
            Span {
                part: Part::Tab(4),
                from: 16,
                to: 25,
            },
            Span {
                part: Part::After(2),
                from: 27,
                to: 29,
            },
            Span {
                part: Part::Url,
                from: 31,
                to: 40,
            },
        ]
    }

    #[test]
    fn a_press_lands_on_the_span_that_holds_its_column_and_nowhere_in_a_gap() {
        let spans = spans();
        assert_eq!(hit(&spans, 0), Some(Part::Before(3)));
        assert_eq!(hit(&spans, 1), Some(Part::Before(3)));
        assert_eq!(hit(&spans, 2), None);
        assert_eq!(hit(&spans, 3), None);
        assert_eq!(hit(&spans, 4), Some(Part::Tab(3)));
        assert_eq!(hit(&spans, 13), Some(Part::Tab(3)));
        assert_eq!(hit(&spans, 14), None);
        assert_eq!(hit(&spans, 16), Some(Part::Tab(4)));
        assert_eq!(hit(&spans, 39), Some(Part::Url));
        assert_eq!(hit(&spans, 40), None);
        assert_eq!(hit(&[], 0), None);
    }

    #[test]
    fn a_left_press_on_a_tab_switches_and_a_middle_press_closes_the_tab_under_it() {
        assert_eq!(step(Some(Part::Tab(4)), Some(0), 8), Step::Switch(4));
        assert_eq!(step(Some(Part::Tab(4)), Some(1), 8), Step::Close(4));
    }

    #[test]
    fn a_press_on_a_marker_goes_to_the_nearest_tab_past_that_end() {
        assert_eq!(step(Some(Part::Before(3)), Some(0), 8), Step::Switch(2));
        assert_eq!(step(Some(Part::After(2)), Some(0), 8), Step::Switch(6));
        assert_eq!(step(Some(Part::Before(3)), Some(1), 8), Step::Nothing);
        assert_eq!(step(Some(Part::After(2)), Some(1), 8), Step::Nothing);
    }

    #[test]
    fn a_middle_press_never_closes_the_last_tab() {
        assert_eq!(step(Some(Part::Tab(0)), Some(1), 1), Step::Nothing);
        assert_eq!(step(Some(Part::Tab(0)), Some(1), 2), Step::Close(0));
    }

    #[test]
    fn a_right_press_and_a_press_on_nothing_do_nothing() {
        assert_eq!(step(Some(Part::Tab(1)), Some(2), 3), Step::Nothing);
        assert_eq!(step(Some(Part::Url), Some(2), 3), Step::Nothing);
        assert_eq!(step(None, Some(0), 3), Step::Nothing);
        assert_eq!(step(None, Some(1), 3), Step::Nothing);
        assert_eq!(step(Some(Part::Tab(1)), None, 3), Step::Nothing);
    }

    #[test]
    fn a_left_press_on_the_url_opens_the_bar() {
        assert_eq!(step(Some(Part::Url), Some(0), 1), Step::EditUrl);
        assert_eq!(step(Some(Part::Url), Some(0), 5), Step::EditUrl);
        assert_eq!(step(Some(Part::Url), Some(1), 5), Step::Nothing);
    }
}
