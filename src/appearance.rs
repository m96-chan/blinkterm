//! What a page is told about where it is being shown: light or dark.
//!
//! A page asks with `@media (prefers-color-scheme: dark)`, and a headless
//! engine has no desktop to ask on its behalf, so it answers light. What does
//! know is the terminal, whose background is the colour the page is sitting
//! in — so the terminal is asked ([`crate::screen::ASK_BACKGROUND`], `OSC
//! 11 ; ?`), its answer is read as a colour ([`crate::input::colour_report`]),
//! and a background darker than the middle of perceptual lightness is a dark
//! one. `--color-scheme light` or `dark` says it outright instead, and a
//! terminal that does not answer gets the engine's light, unsaid, which is
//! what a terminal that cannot say has chosen.
//!
//! How it is told, measured against `chrome-headless-shell` 153 with a page
//! whose body is white, or black under the dark query:
//!
//! | step | `matchMedia` | the body's pixel |
//! | --- | --- | --- |
//! | the engine's default | light | 255 |
//! | `Emulation.setEmulatedMedia` dark, on the loaded page | dark | 0, with no reload |
//! | `Page.navigate` to the same page | dark | 0 |
//! | `Page.reload` | dark | 0 |
//! | through `about:blank` and back | dark | 0 |
//! | a new target, attached and told nothing | light | |
//!
//! 3.6 ms a command. So the emulation is per session and survives what
//! happens on it: it is sent when a session is made, to every session again
//! when the answer changes — a late answer flips a loaded page where it
//! stands — and never with a reload.
//!
//! A page with no dark style of its own stays white whatever it is told.
//! `--force-dark` has the engine paint it dark anyway, with
//! `Emulation.setAutoDarkModeOverride`, which paints a white page `#121212`
//! and is per session like the rest. It is the only one of three ways that
//! does anything in the headless shell 153 — the two command-line switches
//! people suggest, `--enable-features=WebContentsForceDark` and
//! `--force-dark-mode`, left the same page at 255 — and it is off unless
//! asked for, because Chromium's auto dark inverts pages that were designed
//! light, and a person has to have wanted that.
//!
//! # No background at all
//!
//! Where a page paints no background of its own, the engine paints white:
//! the canvas's default, which a desktop browser has a window behind and a
//! terminal does not need. `--alpha` has it paint nothing there instead, with
//! `Emulation.setDefaultBackgroundColorOverride` to black at alpha 0
//! ([`transparent_params`]), so the pixels come back transparent and the
//! terminal's own background — its colour, its opacity, its blur — shows
//! through. A page that paints a background keeps it; only the canvas under
//! it changes — which is why the override does not come alone (see the next
//! section).
//!
//! It is sent from [`Appearance::commands`], after the scheme and the
//! forcing, and for the same reason they are there: it is per session, and
//! every session is made through the one function that sends those
//! (`app::prepare_session`) — the first tab, a new one, one the page opened,
//! every tab of a relaunched engine, a renderer brought back from a crash —
//! so no path can make a page that was not told. It is a per-run setting, as
//! `--force-dark` is; there is no key that turns it off.
//!
//! Measured against `chrome-headless-shell` 153 with a page that paints
//! nothing, the pixel read from a PNG `Page.captureScreenshot`, with the
//! override alone:
//!
//! | step | the canvas's pixel, RGBA |
//! | --- | --- |
//! | the engine's default | 255, 255, 255, 255 |
//! | the override, on the loaded page | 0, 0, 0, 0 — no reload, within 170 ms |
//! | `Page.navigate` to a page with `background: #fff` | alpha 255, the page's own |
//! | `Page.navigate` back | 0, 0, 0, 0 |
//! | `Page.reload` | 0, 0, 0, 0 |
//! | a page saying `color-scheme: dark` | 0, 0, 0, 0, its text white |
//! | `--force-dark` as well | 0, 0, 0, 0, the black text made white |
//!
//! So it survives what happens on the session, as the emulated media does,
//! and is never sent again on a navigation. Neither a page's own dark canvas
//! nor auto dark's `#121212` is painted under it: each changes the text and
//! leaves the canvas transparent. That is the catch — a page that says
//! nothing about its colours is black text on whatever the terminal is, and on
//! a dark terminal that is unreadable. `--force-dark` makes the text light and
//! keeps the transparency, and a light terminal needs nothing.
//!
//! The screencast honours it too, but only in PNG: a JPEG frame of the same
//! page is 0, 0, 0 where the PNG is transparent. The moving frames stay JPEG
//! on the local route all the same, so there a page with no background is
//! black while it moves and transparent once it rests and the lossless still
//! arrives; see [`crate::motion`] for why that is the choice.
//!
//! # Forced transparent, and an amount
//!
//! The override alone shows nothing on most real pages, because most real
//! pages paint their own background — `body { background: #fff }`, or the
//! same on `html` — and a background the page paints is not the canvas's
//! default. So `--alpha` also makes the page's own `html` and `body`
//! backgrounds transparent, with [`TRANSPARENT_CSS`]: the `background`
//! shorthand, so a background image goes as well, and `!important`, which
//! in an adopted sheet beats the page's own `!important` at the same
//! specificity. Text, pictures and anything painted on another element stay:
//! a site whose white is a wrapper `div` keeps it.
//!
//! It is put there by [`TRANSPARENT_SCRIPT`], registered with
//! `Page.addScriptToEvaluateOnNewDocument` right after the override
//! ([`transparent_style_params`]), per session like the rest. The script
//! adopts a constructed stylesheet rather than adding a `<style>`: a
//! constructed sheet is not subject to a page's CSP `style-src`, and the DOM
//! is not changed, for the reasons [`crate::find`] gives for highlighting
//! without touching it. It runs in an isolated world, [`ALPHA_WORLD`], so the
//! flag that keeps it to once a document is not on the page's `window`, and
//! with `runImmediately`, so a page already loaded — the one on screen when
//! the colours are sent again — flips where it stands. What a page can see of
//! it is `document.adoptedStyleSheets` one longer; `document.styleSheets` is
//! as it was, and `__blinktermAlpha` is `undefined` to it.
//!
//! What it does not reach, documented rather than defended: a page that
//! assigns `adoptedStyleSheets` wholesale, dropping the sheet; a page's own
//! `!important` in an inline `style` attribute, which beats any sheet; an
//! iframe from another site, which runs in a process of its own that is
//! not attached, so its background stays; and a same-process iframe with no
//! script of its own that nothing reaches into — the engine makes a frame's
//! context only when something needs it, the script runs when it is made,
//! and such a frame keeps its white until the page touches its document.
//!
//! The number, `--alpha 70`, is the opacity the picture is sent at, text
//! included; it changes nothing the engine is told. The still's alpha is
//! scaled in place ([`crate::graphics::scale_alpha`]), and the moving JPEG
//! frame is decoded straight to RGBA with the amount as every pixel's alpha
//! ([`crate::jpeg::decode_rgba`]). At 100 neither happens. Over ssh and in
//! tmux, where the frames are the engine's PNG sent as they came
//! ([`crate::route::Payload::Png`]), the number is not applied: the pages
//! are see-through there, and what is left is opaque.
//!
//! Measured against `chrome-headless-shell` 153, a page whose `body` paints
//! `#fff` with a line of black text, read from a PNG `Page.captureScreenshot`:
//!
//! | step | the canvas's pixel, RGBA |
//! | --- | --- |
//! | the engine's default, or the override alone | 255, 255, 255, 255 |
//! | the script, on the loaded page | 0, 0, 0, 0 — within 50 ms, the same document |
//! | `Page.navigate`, `Page.reload` | 0, 0, 0, 0 |
//! | its text | black and opaque, as it was |
//! | `--force-dark` as well | 0, 0, 0, 0, the text made light |
//! | a same-process iframe's white `body`, with a script | 0, in place and after a navigation |
//! | the same with no script, untouched | 255, and 0 once the page reads its document |
//! | a screencast frame, PNG / JPEG | 0, 0, 0, 0 / 0, 0, 0 |
//! | the saved PNG (`alt+shift+s`) | 0, 0, 0, 0 |
//! | registered twice, as a colour re-send does | one adopted sheet |
//!
//! And what the amount costs, on an Apple M-series at 1280x770: 3.50 ms to
//! decode a JPEG frame to RGB and 3.53 ms to RGBA, 2.9 MB against 3.9 MB, and
//! 0.2 ms to scale a still's alpha.

use crate::json::Json;

/// Light or dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Light,
    Dark,
}

impl Scheme {
    /// The word `prefers-color-scheme` uses.
    pub fn name(self) -> &'static str {
        match self {
            Scheme::Light => "light",
            Scheme::Dark => "dark",
        }
    }
}

/// `--color-scheme`: what the person said, or `Auto` for the terminal's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Choice {
    #[default]
    Auto,
    Light,
    Dark,
}

impl Choice {
    /// `--color-scheme`'s argument.
    pub fn parse(text: &str) -> Result<Choice, String> {
        match text {
            "auto" => Ok(Choice::Auto),
            "light" => Ok(Choice::Light),
            "dark" => Ok(Choice::Dark),
            _ => Err(format!(
                "--color-scheme is auto, light or dark, not {text:?}"
            )),
        }
    }
}

/// `--alpha`: off, or on with the opacity, from 1 to 100, the picture is sent
/// at. Any `On` has the engine paint no default background and makes the
/// page's own `html` and `body` backgrounds transparent; the number is only
/// how much of what is left shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alpha {
    #[default]
    Off,
    On(u8),
}

impl Alpha {
    /// `--alpha`'s amount or `alpha =`'s value, `name` saying which: `true`
    /// is 100, `false` is off, and a number is from 1 to 100 — 0 is refused,
    /// as `--fps 0` is, because a picture sent at nothing is not one.
    pub fn parse(name: &str, text: &str) -> Result<Alpha, String> {
        match text {
            "true" => return Ok(Alpha::On(100)),
            "false" => return Ok(Alpha::Off),
            _ => {}
        }
        let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
        match text.parse::<u8>() {
            Ok(n @ 1..=100) if digits => Ok(Alpha::On(n)),
            _ => Err(format!(
                "{name} is true, false or a number from 1 to 100, not {text:?}"
            )),
        }
    }

    /// Whether the backgrounds are made transparent at all.
    pub fn on(self) -> bool {
        matches!(self, Alpha::On(_))
    }

    /// What every pixel's alpha is scaled by, out of 255: `None` when nothing
    /// is to be scaled, which is off and 100 alike.
    pub fn scaling(self) -> Option<u8> {
        match self {
            Alpha::On(n) if n < 100 => Some(((u32::from(n) * 255 + 50) / 100) as u8),
            _ => None,
        }
    }
}

/// Everything a session is told about its appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appearance {
    choice: Choice,
    /// What the terminal answered, once it has.
    terminal: Option<Scheme>,
    /// `--force-dark`.
    pub force_dark: bool,
    /// `--alpha`: no default background and none on `html` or `body`, so the
    /// terminal's shows through, and the amount the picture is sent at.
    pub alpha: Alpha,
}

impl Appearance {
    pub fn new(choice: Choice, force_dark: bool, alpha: Alpha) -> Appearance {
        Appearance {
            choice,
            terminal: None,
            force_dark,
            alpha,
        }
    }

    /// The scheme to tell a page: the flag's, else the terminal's, else none
    /// — the engine's light, unsaid.
    pub fn scheme(&self) -> Option<Scheme> {
        match self.choice {
            Choice::Light => Some(Scheme::Light),
            Choice::Dark => Some(Scheme::Dark),
            Choice::Auto => self.terminal,
        }
    }

    /// The terminal said its background is `rgb`. `true` if what pages are
    /// told has changed, which is when every session has to be told again.
    pub fn learned(&mut self, rgb: (u8, u8, u8)) -> bool {
        let before = self.scheme();
        self.terminal = Some(if is_dark(rgb) {
            Scheme::Dark
        } else {
            Scheme::Light
        });
        self.scheme() != before
    }

    /// The commands for one session, in order: `setEmulatedMedia` when there
    /// is a scheme to say, `setAutoDarkModeOverride` when forcing,
    /// `setDefaultBackgroundColorOverride` to transparent under `--alpha`,
    /// and after it the script that makes the page's own backgrounds
    /// transparent ([`transparent_style_params`]). Nothing at all for a session that has nothing to be told, which is a
    /// session the engine's defaults already describe.
    pub fn commands(&self) -> Vec<(&'static str, Json)> {
        let mut out = Vec::new();
        if let Some(scheme) = self.scheme() {
            out.push(("Emulation.setEmulatedMedia", media_params(scheme)));
        }
        if self.force_dark {
            out.push(("Emulation.setAutoDarkModeOverride", auto_dark_params(true)));
        }
        if self.alpha.on() {
            out.push((
                "Emulation.setDefaultBackgroundColorOverride",
                transparent_params(),
            ));
            out.push((
                "Page.addScriptToEvaluateOnNewDocument",
                transparent_style_params(),
            ));
        }
        out
    }
}

/// How bright a colour is: its relative luminance, from 0 for black to 1 for
/// white — sRGB made linear, weighted by Rec. 709's primaries.
pub fn luminance(rgb: (u8, u8, u8)) -> f64 {
    let linear = |channel: u8| {
        let c = f64::from(channel) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(rgb.0) + 0.7152 * linear(rgb.1) + 0.0722 * linear(rgb.2)
}

/// Whether a background is a dark one: a luminance below 0.184, which is
/// L* 50, the middle of perceptual lightness.
///
/// So black is 0, gruvbox's `#282828` and solarized's `#002b36` are 0.02 and
/// dark, a mid-grey `#808080` is 0.216 and light — a grey terminal is not a
/// dark one — and solarized light's `#fdf6e3` is 0.92.
pub fn is_dark(rgb: (u8, u8, u8)) -> bool {
    luminance(rgb) < 0.184
}

/// `Emulation.setEmulatedMedia`'s parameters for one scheme.
pub fn media_params(scheme: Scheme) -> Json {
    Json::object(vec![(
        "features",
        Json::Array(vec![Json::object(vec![
            ("name", Json::string("prefers-color-scheme")),
            ("value", Json::string(scheme.name())),
        ])]),
    )])
}

/// `Emulation.setAutoDarkModeOverride`'s parameters.
pub fn auto_dark_params(enabled: bool) -> Json {
    Json::object(vec![("enabled", Json::Bool(enabled))])
}

/// `Emulation.setDefaultBackgroundColorOverride`'s parameters for no
/// background at all: black with nothing of it showing.
pub fn transparent_params() -> Json {
    Json::object(vec![(
        "color",
        Json::object(vec![
            ("r", Json::number(0)),
            ("g", Json::number(0)),
            ("b", Json::number(0)),
            ("a", Json::number(0)),
        ]),
    )])
}

/// The isolated world [`TRANSPARENT_SCRIPT`] runs in, so the flag it leaves
/// on its global is not on the page's `window`.
pub const ALPHA_WORLD: &str = "blinkterm-alpha";

/// [`TRANSPARENT_CSS`], as a literal [`TRANSPARENT_SCRIPT`] can be built from.
macro_rules! transparent_css {
    () => {
        "html, body { background: transparent !important; }"
    };
}

/// What `--alpha` makes of a page's own backgrounds: the shorthand, so a
/// background image goes too, and `!important`, so it beats the page's.
pub const TRANSPARENT_CSS: &str = transparent_css!();

/// Adopts [`TRANSPARENT_CSS`] as a constructed stylesheet, once a document.
///
/// The flag on the world's global is the document it was adopted into, so a
/// second run on that document — the script registered again when the
/// colours are re-sent — does nothing. It is the document and not `true`
/// because a window can outlive its first document: a frame's initial
/// `about:blank` hands its window to the same-origin document that replaces
/// it, and a `true` left on the first must not have the second skipped. A
/// document that refuses the sheet is left as it was.
pub const TRANSPARENT_SCRIPT: &str = concat!(
    "(() => {",
    " if (globalThis.__blinktermAlpha === document) return;",
    " globalThis.__blinktermAlpha = document;",
    " try {",
    " const s = new CSSStyleSheet();",
    " s.replaceSync(\"",
    transparent_css!(),
    "\");",
    " document.adoptedStyleSheets = [...document.adoptedStyleSheets, s];",
    " } catch (e) {}",
    " })()"
);

/// `Page.addScriptToEvaluateOnNewDocument`'s parameters for
/// [`TRANSPARENT_SCRIPT`]: in [`ALPHA_WORLD`], and run on the document already
/// there as well as on every one after it.
pub fn transparent_style_params() -> Json {
    Json::object(vec![
        ("source", Json::string(TRANSPARENT_SCRIPT)),
        ("worldName", Json::string(ALPHA_WORLD)),
        ("runImmediately", Json::Bool(true)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luminance_calls_black_dark_white_light_and_a_mid_grey_light() {
        assert_eq!(luminance((0, 0, 0)), 0.0);
        assert!((luminance((255, 255, 255)) - 1.0).abs() < 1e-9);
        assert!(is_dark((0, 0, 0)));
        assert!(!is_dark((255, 255, 255)));
        let grey = luminance((0x80, 0x80, 0x80));
        assert!((grey - 0.216).abs() < 0.001, "{grey}");
        assert!(!is_dark((0x80, 0x80, 0x80)));
    }

    #[test]
    fn solarized_and_gruvbox_come_out_the_right_way_round() {
        for dark in [(0x28, 0x28, 0x28), (0x00, 0x2b, 0x36), (0x1c, 0x1c, 0x1c)] {
            assert!(is_dark(dark), "{dark:?}: {}", luminance(dark));
        }
        for light in [(0xfd, 0xf6, 0xe3), (0xee, 0xe8, 0xd5), (0xfb, 0xf1, 0xc7)] {
            assert!(!is_dark(light), "{light:?}: {}", luminance(light));
        }
    }

    #[test]
    fn the_flag_beats_the_terminal_and_auto_without_an_answer_says_nothing() {
        let mut auto = Appearance::new(Choice::Auto, false, Alpha::Off);
        assert_eq!(auto.scheme(), None);
        assert!(auto.commands().is_empty(), "the engine's default, unsaid");
        let alpha = Appearance::new(Choice::Auto, false, Alpha::On(70)).commands();
        assert_eq!(
            alpha.iter().map(|(method, _)| *method).collect::<Vec<_>>(),
            [
                "Emulation.setDefaultBackgroundColorOverride",
                "Page.addScriptToEvaluateOnNewDocument"
            ],
            "the override and the script alone, with no answer yet"
        );
        assert_is_the_script(&alpha[1].1);
        assert!(auto.learned((0, 0, 0)));
        assert_eq!(auto.scheme(), Some(Scheme::Dark));

        let mut light = Appearance::new(Choice::Light, false, Alpha::Off);
        assert_eq!(light.scheme(), Some(Scheme::Light));
        assert!(!light.learned((0, 0, 0)), "the flag was not asking");
        assert_eq!(light.scheme(), Some(Scheme::Light));

        let mut dark = Appearance::new(Choice::Dark, false, Alpha::Off);
        assert!(!dark.learned((255, 255, 255)));
        assert_eq!(dark.scheme(), Some(Scheme::Dark));
    }

    #[test]
    fn learning_the_same_answer_twice_changes_nothing() {
        let mut appearance = Appearance::new(Choice::Auto, false, Alpha::Off);
        assert!(appearance.learned((0x1c, 0x1c, 0x1c)));
        assert!(!appearance.learned((0x28, 0x28, 0x28)), "still dark");
        assert!(appearance.learned((0xfd, 0xf6, 0xe3)), "now light");
        assert_eq!(appearance.scheme(), Some(Scheme::Light));
    }

    #[test]
    fn the_commands_name_the_scheme_and_the_forcing() {
        let mut appearance = Appearance::new(Choice::Auto, true, Alpha::Off);
        let text = |appearance: &Appearance| {
            appearance
                .commands()
                .into_iter()
                .map(|(method, params)| format!("{method} {params}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            text(&appearance),
            ["Emulation.setAutoDarkModeOverride {\"enabled\":true}"]
        );
        appearance.learned((0, 0, 0));
        assert_eq!(
            text(&appearance),
            [
                "Emulation.setEmulatedMedia {\"features\":[{\"name\":\"prefers-color-scheme\",\"value\":\"dark\"}]}",
                "Emulation.setAutoDarkModeOverride {\"enabled\":true}",
            ]
        );
        assert_eq!(
            media_params(Scheme::Light).to_string(),
            r#"{"features":[{"name":"prefers-color-scheme","value":"light"}]}"#
        );
        assert_eq!(auto_dark_params(false).to_string(), r#"{"enabled":false}"#);

        appearance.alpha = Alpha::On(100);
        let all = text(&appearance);
        assert_eq!(
            all[..3],
            [
                "Emulation.setEmulatedMedia {\"features\":[{\"name\":\"prefers-color-scheme\",\"value\":\"dark\"}]}",
                "Emulation.setAutoDarkModeOverride {\"enabled\":true}",
                "Emulation.setDefaultBackgroundColorOverride {\"color\":{\"r\":0,\"g\":0,\"b\":0,\"a\":0}}",
            ],
            "the override comes after the scheme and the forcing"
        );
        assert_eq!(all.len(), 4, "and the script after it: {all:?}");
        let (method, params) = appearance.commands().pop().expect("the script");
        assert_eq!(method, "Page.addScriptToEvaluateOnNewDocument");
        assert_is_the_script(&params);
        assert_eq!(
            transparent_params().to_string(),
            r#"{"color":{"r":0,"g":0,"b":0,"a":0}}"#
        );
    }

    /// `Page.addScriptToEvaluateOnNewDocument`'s parameters are the alpha
    /// script's: its world, run on the document already there, and the CSS.
    fn assert_is_the_script(params: &Json) {
        assert_eq!(
            params.get("worldName").and_then(Json::as_str),
            Some(ALPHA_WORLD)
        );
        assert_eq!(
            params.get("runImmediately").and_then(Json::as_bool),
            Some(true)
        );
        let source = params.get("source").and_then(Json::as_str).expect("source");
        assert!(source.contains(TRANSPARENT_CSS), "{source}");
        assert!(source.contains("adoptedStyleSheets"), "{source}");
        assert!(source.contains("__blinktermAlpha"), "{source}");
    }

    #[test]
    fn an_alpha_is_true_false_or_one_to_a_hundred() {
        assert_eq!(Alpha::parse("--alpha", "true"), Ok(Alpha::On(100)));
        assert_eq!(Alpha::parse("--alpha", "false"), Ok(Alpha::Off));
        assert_eq!(Alpha::parse("--alpha", "70"), Ok(Alpha::On(70)));
        assert_eq!(Alpha::parse("--alpha", "100"), Ok(Alpha::On(100)));
        assert_eq!(Alpha::parse("--alpha", "1"), Ok(Alpha::On(1)));
        assert_eq!(Alpha::default(), Alpha::Off);
        for text in ["0", "101", "yes", "70%", "", "+70", "256"] {
            let why = Alpha::parse("alpha", text).expect_err(text);
            assert_eq!(
                why,
                format!("alpha is true, false or a number from 1 to 100, not {text:?}")
            );
        }
        assert!(!Alpha::Off.on());
        assert!(Alpha::On(1).on());
        assert_eq!(Alpha::Off.scaling(), None);
        assert_eq!(Alpha::On(100).scaling(), None, "at 100 nothing is touched");
        assert_eq!(Alpha::On(70).scaling(), Some(179));
        assert_eq!(Alpha::On(50).scaling(), Some(128));
        assert_eq!(Alpha::On(1).scaling(), Some(3));
        assert_eq!(Alpha::On(99).scaling(), Some(252));
    }

    #[test]
    fn a_choice_is_auto_light_or_dark() {
        assert_eq!(Choice::parse("auto"), Ok(Choice::Auto));
        assert_eq!(Choice::parse("light"), Ok(Choice::Light));
        assert_eq!(Choice::parse("dark"), Ok(Choice::Dark));
        assert_eq!(Choice::default(), Choice::Auto);
        for text in ["", "Dark", "black", "no-preference"] {
            let why = Choice::parse(text).expect_err("refused");
            assert!(why.contains("--color-scheme"), "{text:?}: {why}");
        }
    }
}
