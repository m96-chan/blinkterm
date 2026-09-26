//! How a frame reaches the terminal, decided once per run.
//!
//! The pane this program was written for is a tOS pane on the same machine:
//! raw pixels through `/dev/shm`, a placement at the cursor, the keyboard
//! protocol pushed. Two things put something between the program and that
//! terminal, and each changes what can be sent
//! ([#21](https://github.com/m96-chan/blinkterm/issues/21)):
//!
//! - **tmux.** It swallows a raw graphics command whole and never forwards
//!   it. It passes one through only inside `ESC P tmux; … ESC \` with every
//!   inner `ESC` doubled, and only when `allow-passthrough` is on; a DCS over
//!   its 1 MiB string buffer is dropped without a word. And tmux, not the
//!   terminal, owns the cursor and the cells, so a placement at the cursor
//!   lands wherever tmux left the outer cursor last: the picture has to be
//!   *text* tmux can see — Unicode placeholders, `U+10EEEE` cells coloured
//!   with the image id — and the image a virtual placement (`U=1`). See
//!   [`crate::graphics`] for the bytes.
//! - **ssh.** The shared memory object is on the wrong machine, and 3.9 MB
//!   of base64 a frame is no frame rate over any link people have. The
//!   engine's own PNG goes instead, as it came (`f=100`): within 15% of what
//!   a deflate of the raw pixels would make (measured, every page), ten times
//!   smaller than raw on text, and nothing to decode here.
//!
//! Four independent facts, so four axes in [`Route`], chosen by [`choose`]
//! from what the environment says ([`Env`]), what the command line or the
//! settings file overrode ([`Choices`]), and what the terminal answered
//! ([`crate::doctor::probe`]). The environment alone does not decide tmux:
//! a Kitty started from a tmux shell inherits `$TMUX` and speaks the protocol
//! raw, and the probe hears it answer. `$TMUX` decides only whether the
//! wrapped question is asked at all.
//!
//! GNU screen is not a route. Its passthrough is a different DCS with a
//! historical 768-byte cut, and there was no screen to measure against; `$STY`
//! shapes the sentence a refusal says, and nothing else.

use crate::doctor::Verdict;
use crate::graphics::Transport;

/// How the pixels are encoded on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload {
    /// `f=24`/`f=32`: decoded here, sent as pixels. What a tOS pane and a
    /// local Kitty take.
    Raw,
    /// `f=100`: the engine's PNG, sent as it came. Nothing is decoded here.
    Png,
}

/// Where the picture goes on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// `a=T` at the cursor with `C=1`.
    Direct,
    /// `U=1`, a virtual placement, and a pane of `U+10EEEE` cells.
    Unicode,
}

/// Whether every graphics command is wrapped for a multiplexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    None,
    /// `ESC P tmux;` … `ESC \`, every inner `ESC` doubled, at most
    /// [`crate::graphics::DCS_LIMIT`] bytes each.
    Tmux,
}

/// Everything the painter and the screencast need to know that is decided
/// once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub transport: Transport,
    pub payload: Payload,
    pub placement: Placement,
    pub wrap: Wrap,
    /// The motion cast's `everyNthFrame`, from the frame-rate cap.
    pub every_nth: u32,
}

impl Route {
    /// The route of a run with nobody in the way: raw pixels, through
    /// `/dev/shm` when it can be written, at the cursor, every frame.
    pub fn local(shm_usable: bool) -> Route {
        Route {
            transport: if shm_usable {
                Transport::SharedMemory
            } else {
                Transport::Inline
            },
            payload: Payload::Raw,
            placement: Placement::Direct,
            wrap: Wrap::None,
            every_nth: 1,
        }
    }

    /// Whether a frame is acknowledged only once it has been written to the
    /// terminal, rather than the moment it arrives: on the routes whose link
    /// is slower than the engine. See [`crate::motion::Throttle`].
    pub fn paced(&self) -> bool {
        self.payload == Payload::Png
    }

    /// The frame-rate cap this route asks the engine for.
    pub fn fps(&self) -> u32 {
        FULL_RATE / self.every_nth.max(1)
    }

    /// One line for `--doctor`: what the run would do, in the order the
    /// axes are listed.
    pub fn describe(&self) -> String {
        format!(
            "{} frames, {}, {}, {}, {} fps cap",
            match self.payload {
                Payload::Raw => "raw",
                Payload::Png => "png",
            },
            match self.placement {
                Placement::Direct => "placed at the cursor",
                Placement::Unicode => "unicode placeholders",
            },
            match self.wrap {
                Wrap::None => "not wrapped",
                Wrap::Tmux => "wrapped for tmux",
            },
            match self.transport {
                Transport::SharedMemory => "through /dev/shm",
                Transport::Inline => "inline",
            },
            self.fps()
        )
    }
}

/// The engine's own frame rate, which `everyNthFrame` divides: 2 is 30.0,
/// 3 is 20.0, 6 is 10.0 (measured on the cast of an animating page).
pub const FULL_RATE: u32 = 60;

/// The cap through tmux: tmux moves 10 MB/s of wrapped PNG, which is the
/// article page's 263 kB of base64 at 38 a second, so 30 is the rate the
/// link holds rather than one the acks have to find.
pub const TMUX_FPS: u32 = 30;

/// The cap over ssh. Not what adapts — the acks are — but what keeps the
/// far engine from encoding sixty PNGs a second for a terminal that scales
/// them into cells.
pub const SSH_FPS: u32 = 15;

/// What the environment says, read once.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Env {
    /// `$TMUX` is set and not empty.
    pub tmux: bool,
    /// `$STY`: GNU screen.
    pub screen: bool,
    /// Any of `$SSH_CONNECTION`, `$SSH_TTY`, `$SSH_CLIENT`.
    pub ssh: bool,
    /// `$TERM`, for the sentence.
    pub term: String,
}

impl Env {
    /// Pure over its getter, so a test can hand it a table.
    pub fn read(var: impl Fn(&str) -> Option<String>) -> Env {
        let set = |name: &str| var(name).is_some_and(|value| !value.is_empty());
        Env {
            tmux: set("TMUX"),
            screen: set("STY"),
            ssh: ["SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"]
                .iter()
                .any(|name| set(name)),
            term: var("TERM").unwrap_or_default(),
        }
    }

    /// The process's own environment.
    pub fn current() -> Env {
        Env::read(|name| std::env::var(name).ok())
    }
}

/// `--tmux`: whether frames are wrapped and placed for tmux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Choice {
    /// When the probe heard the terminal answer through tmux.
    #[default]
    Auto,
    On,
    Off,
}

impl Choice {
    pub fn parse(text: &str) -> Result<Choice, String> {
        match text {
            "auto" => Ok(Choice::Auto),
            "on" => Ok(Choice::On),
            "off" => Ok(Choice::Off),
            _ => Err(format!("--tmux is auto, on or off, not {text:?}")),
        }
    }
}

/// `--frames`: how the pixels are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Frames {
    /// PNG over ssh and through tmux, raw otherwise.
    #[default]
    Auto,
    Raw,
    Png,
}

impl Frames {
    pub fn parse(text: &str) -> Result<Frames, String> {
        match text {
            "auto" => Ok(Frames::Auto),
            "raw" => Ok(Frames::Raw),
            "png" => Ok(Frames::Png),
            _ => Err(format!("--frames is auto, raw or png, not {text:?}")),
        }
    }
}

/// `--fps`'s value: a whole number of frames a second, 1 to 60.
pub fn parse_fps(name: &str, text: &str) -> Result<u32, String> {
    match text.parse::<u32>() {
        Ok(n) if (1..=FULL_RATE).contains(&n) => Ok(n),
        _ => Err(format!(
            "{name} is a number of frames a second from 1 to {FULL_RATE}, not {text:?}"
        )),
    }
}

/// The overrides, from [`crate::options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choices {
    pub tmux: Choice,
    pub frames: Frames,
    /// `--fps`; `None` for the route's own cap.
    pub fps: Option<u32>,
    /// `false` with `--no-probe`.
    pub probe: bool,
}

impl Default for Choices {
    fn default() -> Choices {
        Choices {
            tmux: Choice::Auto,
            frames: Frames::Auto,
            fps: None,
            probe: true,
        }
    }
}

/// The route for this run. Pure; every rule is a test below.
///
/// A choice on the command line or in the file beats the environment, and
/// the environment beats the probe only where the probe cannot tell:
///
/// 1. tmux is on with `--tmux on`, or with `auto` when the probe heard the
///    terminal only through tmux. With the probe skipped there is nothing
///    heard, and `$TMUX` is the best word left.
/// 2. PNG with `--frames png`, or with `auto` over ssh or through tmux: raw
///    through tmux is 3 MB/s, raw over ssh is 3.9 MB a frame.
/// 3. Inline for a PNG (nobody reads a PNG out of `/dev/shm`), over ssh (the
///    object is on the wrong machine, whatever `--frames` says), through tmux
///    (tmux's pty is local and the terminal behind it may not be), or when
///    `/dev/shm` cannot be written.
/// 4. Every nth frame from `--fps`, else 60 locally, 30 through tmux and 15
///    over ssh.
pub fn choose(env: &Env, choices: Choices, verdict: Verdict, shm_usable: bool) -> Route {
    let tmux = match choices.tmux {
        Choice::On => true,
        Choice::Off => false,
        Choice::Auto => match verdict {
            Verdict::ThroughTmux => true,
            Verdict::Skipped => env.tmux,
            _ => false,
        },
    };
    let png = match choices.frames {
        Frames::Png => true,
        Frames::Raw => false,
        Frames::Auto => env.ssh || tmux,
    };
    let inline = png || env.ssh || tmux || !shm_usable;
    let fps = choices.fps.unwrap_or(if env.ssh {
        SSH_FPS
    } else if tmux {
        TMUX_FPS
    } else {
        FULL_RATE
    });
    Route {
        transport: if inline {
            Transport::Inline
        } else {
            Transport::SharedMemory
        },
        payload: if png { Payload::Png } else { Payload::Raw },
        placement: if tmux {
            Placement::Unicode
        } else {
            Placement::Direct
        },
        wrap: if tmux { Wrap::Tmux } else { Wrap::None },
        every_nth: every_nth(fps),
    }
}

/// `everyNthFrame` for a cap of `fps`: sixty over it, rounded, at least 1.
pub fn every_nth(fps: u32) -> u32 {
    let fps = fps.clamp(1, FULL_RATE);
    ((FULL_RATE as f64 / fps as f64).round() as u32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Env::read(move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        })
    }

    fn auto() -> Choices {
        Choices::default()
    }

    #[test]
    fn the_environment_is_read_from_its_six_variables_and_nothing_else() {
        let e = env(&[
            ("TMUX", "/tmp/tmux-0/default,1,0"),
            ("STY", "1234.pts-0.host"),
            ("SSH_TTY", "/dev/pts/3"),
            ("TERM", "tmux-256color"),
            ("TMUX_PANE", "%0"),
            ("KITTY_WINDOW_ID", "1"),
        ]);
        assert_eq!(
            e,
            Env {
                tmux: true,
                screen: true,
                ssh: true,
                term: "tmux-256color".to_string()
            }
        );
        for name in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"] {
            assert!(env(&[(name, "x")]).ssh, "{name}");
        }
        assert_eq!(
            env(&[("TMUX", ""), ("SSH_CONNECTION", "")]),
            Env::default(),
            "an empty variable says nothing, as it would to a shell"
        );
    }

    #[test]
    fn a_kitty_started_from_a_tmux_shell_keeps_the_raw_route_because_the_probe_answered_raw() {
        let route = choose(&env(&[("TMUX", "x")]), auto(), Verdict::Direct, true);
        assert_eq!(route, Route::local(true));
    }

    #[test]
    fn tmux_is_on_when_the_raw_query_went_unanswered_and_the_wrapped_one_answered() {
        let route = choose(&env(&[("TMUX", "x")]), auto(), Verdict::ThroughTmux, true);
        assert_eq!(route.wrap, Wrap::Tmux);
        assert_eq!(route.placement, Placement::Unicode);
        assert_eq!(route.payload, Payload::Png);
        assert_eq!(route.transport, Transport::Inline);
    }

    #[test]
    fn tmux_off_on_the_command_line_never_wraps_whatever_the_environment_says() {
        let choices = Choices {
            tmux: Choice::Off,
            ..auto()
        };
        for verdict in [Verdict::ThroughTmux, Verdict::Skipped, Verdict::Direct] {
            let route = choose(&env(&[("TMUX", "x")]), choices, verdict, true);
            assert_eq!(route.wrap, Wrap::None, "{verdict:?}");
            assert_eq!(route.placement, Placement::Direct, "{verdict:?}");
        }
        let on = Choices {
            tmux: Choice::On,
            ..auto()
        };
        assert_eq!(
            choose(&Env::default(), on, Verdict::Direct, true).wrap,
            Wrap::Tmux
        );
    }

    #[test]
    fn a_skipped_probe_leaves_tmux_to_the_environment() {
        let inside = choose(&env(&[("TMUX", "x")]), auto(), Verdict::Skipped, true);
        assert_eq!(inside.wrap, Wrap::Tmux);
        let outside = choose(&Env::default(), auto(), Verdict::Skipped, true);
        assert_eq!(outside, Route::local(true));
    }

    #[test]
    fn ssh_makes_the_frames_png_and_the_transport_inline_whatever_dev_shm_says() {
        let route = choose(
            &env(&[("SSH_CONNECTION", "1 2 3 4")]),
            auto(),
            Verdict::Direct,
            true,
        );
        assert_eq!(route.payload, Payload::Png);
        assert_eq!(route.transport, Transport::Inline);
        assert_eq!(route.placement, Placement::Direct);
        assert_eq!(route.wrap, Wrap::None);
        assert!(route.paced());
    }

    #[test]
    fn frames_raw_over_ssh_still_refuses_shared_memory() {
        let choices = Choices {
            frames: Frames::Raw,
            ..auto()
        };
        let route = choose(&env(&[("SSH_TTY", "x")]), choices, Verdict::Direct, true);
        assert_eq!(route.payload, Payload::Raw);
        assert_eq!(route.transport, Transport::Inline);
        assert!(!route.paced());
    }

    #[test]
    fn frames_png_locally_is_png_inline_and_an_unwritable_dev_shm_is_inline_raw() {
        let png = Choices {
            frames: Frames::Png,
            ..auto()
        };
        let route = choose(&Env::default(), png, Verdict::Direct, true);
        assert_eq!(
            (route.payload, route.transport),
            (Payload::Png, Transport::Inline)
        );
        assert_eq!(
            choose(&Env::default(), auto(), Verdict::Direct, false),
            Route::local(false)
        );
    }

    #[test]
    fn the_frame_rate_cap_is_sixty_locally_thirty_through_tmux_and_fifteen_over_ssh() {
        let local = choose(&Env::default(), auto(), Verdict::Direct, true);
        let tmux = choose(&env(&[("TMUX", "x")]), auto(), Verdict::ThroughTmux, true);
        let ssh = choose(&env(&[("SSH_TTY", "x")]), auto(), Verdict::Direct, true);
        let both = choose(
            &env(&[("SSH_TTY", "x"), ("TMUX", "x")]),
            auto(),
            Verdict::ThroughTmux,
            true,
        );
        assert_eq!(
            [local.fps(), tmux.fps(), ssh.fps(), both.fps()],
            [60, 30, 15, 15]
        );
        assert_eq!([local.every_nth, tmux.every_nth, ssh.every_nth], [1, 2, 4]);
    }

    #[test]
    fn a_fps_of_seven_asks_the_engine_for_every_ninth_frame() {
        let choices = Choices {
            fps: Some(7),
            ..auto()
        };
        assert_eq!(
            choose(&Env::default(), choices, Verdict::Direct, true).every_nth,
            9
        );
        assert_eq!(every_nth(60), 1);
        assert_eq!(every_nth(1), 60);
    }

    #[test]
    fn screen_is_not_a_route_and_only_shapes_the_sentence() {
        let route = choose(&env(&[("STY", "x")]), auto(), Verdict::Direct, true);
        assert_eq!(route, Route::local(true));
    }

    #[test]
    fn the_three_values_are_parsed_by_name() {
        assert_eq!(Choice::parse("on"), Ok(Choice::On));
        assert_eq!(Frames::parse("png"), Ok(Frames::Png));
        assert!(Choice::parse("yes").unwrap_err().contains("--tmux"));
        assert!(Frames::parse("jpeg").unwrap_err().contains("--frames"));
        assert_eq!(parse_fps("--fps", "30"), Ok(30));
        for bad in ["0", "61", "", "ten", "-1"] {
            assert!(
                parse_fps("--fps", bad).unwrap_err().contains("--fps"),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_doctor_line_names_every_axis() {
        let tmux = choose(&env(&[("TMUX", "x")]), auto(), Verdict::ThroughTmux, true);
        assert_eq!(
            tmux.describe(),
            "png frames, unicode placeholders, wrapped for tmux, inline, 30 fps cap"
        );
        assert_eq!(
            Route::local(true).describe(),
            "raw frames, placed at the cursor, not wrapped, through /dev/shm, 60 fps cap"
        );
    }
}
