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

/// Everything a session is told about its appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appearance {
    choice: Choice,
    /// What the terminal answered, once it has.
    terminal: Option<Scheme>,
    /// `--force-dark`.
    pub force_dark: bool,
}

impl Appearance {
    pub fn new(choice: Choice, force_dark: bool) -> Appearance {
        Appearance {
            choice,
            terminal: None,
            force_dark,
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
    /// is a scheme to say, `setAutoDarkModeOverride` when forcing. Nothing at
    /// all for a session that has nothing to be told, which is a session the
    /// engine's defaults already describe.
    pub fn commands(&self) -> Vec<(&'static str, Json)> {
        let mut out = Vec::new();
        if let Some(scheme) = self.scheme() {
            out.push(("Emulation.setEmulatedMedia", media_params(scheme)));
        }
        if self.force_dark {
            out.push(("Emulation.setAutoDarkModeOverride", auto_dark_params(true)));
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
        let mut auto = Appearance::new(Choice::Auto, false);
        assert_eq!(auto.scheme(), None);
        assert!(auto.commands().is_empty(), "the engine's default, unsaid");
        assert!(auto.learned((0, 0, 0)));
        assert_eq!(auto.scheme(), Some(Scheme::Dark));

        let mut light = Appearance::new(Choice::Light, false);
        assert_eq!(light.scheme(), Some(Scheme::Light));
        assert!(!light.learned((0, 0, 0)), "the flag was not asking");
        assert_eq!(light.scheme(), Some(Scheme::Light));

        let mut dark = Appearance::new(Choice::Dark, false);
        assert!(!dark.learned((255, 255, 255)));
        assert_eq!(dark.scheme(), Some(Scheme::Dark));
    }

    #[test]
    fn learning_the_same_answer_twice_changes_nothing() {
        let mut appearance = Appearance::new(Choice::Auto, false);
        assert!(appearance.learned((0x1c, 0x1c, 0x1c)));
        assert!(!appearance.learned((0x28, 0x28, 0x28)), "still dark");
        assert!(appearance.learned((0xfd, 0xf6, 0xe3)), "now light");
        assert_eq!(appearance.scheme(), Some(Scheme::Light));
    }

    #[test]
    fn the_commands_name_the_scheme_and_the_forcing() {
        let mut appearance = Appearance::new(Choice::Auto, true);
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
