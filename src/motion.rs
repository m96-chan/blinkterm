//! Which format a frame arrives in, and when the lossless one is worth asking
//! for.
//!
//! # JPEG while it moves, PNG when it stops
//!
//! `Page.startScreencast` is bounded by the engine's own single-threaded
//! encode of each frame, which is why the format is the frame rate. Measured
//! on a 1280x770 pane, two cores, a scrolling ja.wikipedia page, and
//! unchanged from two cores to eight:
//!
//! ```text
//! format     fps     kB/frame    gap p50 / p95 / max
//! png       33.8      320        28 / 37 / 39 ms
//! jpeg q70  60.0      139        17 / 18 / 20 ms
//! jpeg q85  57.8      185        about 17 ms
//! jpeg q95  40.0      268
//! jpeg q100 27.8      387
//! ```
//!
//! `Page.captureScreenshot` in a loop is 10 to 12 frames a second in every
//! format including `png` with `optimizeForSpeed` and lossless webp, so there
//! is no fast lossless path in CDP to prefer: it is JPEG or it is half the
//! frame rate.
//!
//! **Chromium's screencast JPEG is 4:2:0 at every quality** — the encoder
//! hard-codes a sampling factor of 2x2,1x1,1x1 — so coloured text smears a
//! little whatever quality is asked for, and quality only decides how much is
//! left of the luma underneath it. At q70 the halo around blue link text is
//! visible at 1:1. At q85 the difference from PNG needs 3x zoom to find. The
//! person looked at both against the PNG and chose 85, and that is what
//! [`QUALITY`] is.
//!
//! **So: JPEG while the page is moving, PNG the moment it stops.** Text that
//! is being read is always lossless; the lossy frames are only ever the ones
//! scrolling past, which is exactly the trade VNC and RDP make and for the
//! same reason. A page that never moves costs one still and then nothing at
//! all — no screencast frames, no polling, no repainting.
//!
//! # Still JPEG while it moves, in alpha mode — keyed
//!
//! Under `--alpha` what the page leaves bare is to be transparent, and a JPEG
//! has nowhere to keep that. Chromium encodes the screencast from a
//! premultiplied bitmap and drops the alpha, so a transparent pixel comes out
//! 0, 0, 0: measured, the same pixel that is 0, 0, 0, 0 in a PNG frame of the
//! same page. With the page's own `html` and `body` backgrounds forced
//! transparent that is most of a real page, and asking for real transparency
//! made a light page moving its dark text on black.
//!
//! That was first taken to be a fair trade — the frames that pass while a
//! page moves are the ones somebody is driving past — and it was not,
//! because a page moves without anybody driving it: a text box's caret
//! blinking is a frame every half second, and Google's front page, with its
//! caret in the search box, flickered black and clear for as long as it was
//! open. The PNG cast keeps the alpha and was measured and set aside for what
//! it costs: on an Apple M3 at 1280x768, the scrolling article of the engine
//! tests, 55.2 fps at 292 kB a frame against JPEG's 59.7 at 234 kB, with
//! 8.3 ms to decode a frame here against 6.1 — and 33.8 against 57.8 on two
//! cores, per the table above.
//!
//! **So the cast stays JPEG, and the page is painted on a key colour that the
//! decoder takes back out** ([`crate::chroma`]): see-through while it moves as
//! well as at rest, at 0.9 ms a 1280x768 frame, and the still keyed the same
//! way so that the two look alike. The cast stays what the route makes it
//! ([`Cast::for_route`]). On a route whose frames are already the engine's
//! PNG — over ssh, inside tmux ([`crate::route::Payload::Png`]) — the frames
//! carry real transparency, and there is no key.
//!
//! # When a still is worth asking for
//!
//! The first version of this asked for a still 150 ms after the last frame,
//! and on an installed tOS in VirtualBox — two vCPUs, no GPU, a 1280x770
//! pane — that made a scroll flash. The measurements that say why were taken
//! on that machine's own engine:
//!
//! ```text
//! Page.captureScreenshot png, pane size    66 to 98 ms
//! Page.captureScreenshot jpeg, pane size   42 to 51 ms
//! jpeg q85 screencast while scrolling      about 42 fps, 24 to 27 ms gaps
//! ```
//!
//! A mouse-wheel notch moves the page for about 130 ms and then stops. A hand
//! on a wheel produces notches 150 to 300 ms apart. At 150 ms of
//! frame quiet, *every notch* therefore ended in a lossless still, and what
//! was on the screen was JPEG frames, PNG, JPEG frames, PNG, several times a
//! second. On a page with any colour in it — a gradient, a picture, coloured
//! text, all of which 4:2:0 chroma treats differently from PNG — that
//! difference is visible at 1:1, and a scroll looks like it is flashing.
//!
//! So a still now waits for two kinds of quiet rather than one: no screencast
//! frame for [`REST_AFTER`], **and** no wheel notch or key for
//! [`INPUT_QUIET`]. A hand on the wheel produces JPEG frames and nothing else;
//! the PNG arrives once the hand stops. The two together are what make the
//! format change once per scroll instead of once per notch.
//!
//! # Which frame wins
//!
//! Two sources paint the same pane, and they can arrive out of order. A
//! screencast frame captured *before* the still was asked for can turn up
//! after it, because the still is a round trip to the engine and the frame was
//! already in the mailbox; and a still can come back after the page started
//! moving again, because it takes tens of milliseconds to encode — 66 to 98 of
//! them on the machine above.
//!
//! The rule is **a still counts only if the page stayed still for the whole of
//! it** — with one frame forgiven, because the still photographs itself. See
//! [`SHUTTER_FRAMES`]: `Page.captureScreenshot` forces a surface capture and
//! the screencast is watching that same surface, so every screenshot is
//! followed by exactly one screencast frame of the picture it just took — at
//! a device scale of 1; see "A second shutter, after the reply" for 2. More
//! frames in the window than the still's own are the page moving. How the
//! still's own frame is told from the page's is "The picture, not the clock".
//!
//! That replaces an earlier rule, and the earlier one is worth recording
//! because of what it did with that shutter frame. It credited the still with
//! the wall-clock moment it was *asked for* — the earliest instant it could
//! depict — and compared that against each frame's `metadata.timestamp`. The
//! shutter frame is stamped about four milliseconds after the request, so it
//! counted as newer, and a JPEG of the page was painted over the PNG that had
//! just replaced it. Which cleared the tab's rest, which asked for another
//! still 150 ms later, which produced another shutter frame: **a loop, about
//! four times a second, on every page including one that nothing was
//! happening to at all.** That is the flashing, and the wheel only made it
//! worse by making the stills more frequent still.
//!
//! So a still was credited with the moment its **reply** arrived — the
//! latest instant it could depict — and a frame older than what is on screen
//! was dropped *and was not motion*, because it showed a moment that had
//! already been drawn. The shutter frame, stamped 35 to 48 ms before the
//! reply, was exactly such a frame, and the loop had nothing to stand on. It
//! hid more than the shutter, though: see "The picture, not the clock".
//!
//! # A second shutter, after the reply
//!
//! All of that was measured at a device scale of 1, and at 2 — a HiDPI
//! terminal, `--scale 2`, every Retina Mac — the engine does something else.
//! Probed against `chrome-headless-shell` 153 on a page nothing was happening
//! to, a still at scale 2 provokes one frame or two, and they are stamped
//! *after* the reply as often as before it: at +24 and +107 ms from the
//! request with the reply at +84, at +65 and +84 with the reply at +63. A
//! frame stamped after the reply is newer than the still by the rule above,
//! so it went up over it, cleared the rest, and 250 ms later asked for
//! another still, which did the same: **the loop was back**, at about two
//! stills a second, on a page that was not moving. Without `--alpha` that is
//! the text going soft and sharp twice a second; under it, a page whose bare
//! parts are black in a JPEG, and a pane that ends on whichever of the two
//! came last — which is how it was found, on Wikipedia in Kitty on a Mac.
//!
//! So a still owned a short window after its reply as well as the one before
//! it — frames stamped up to its own round trip after the reply, up to
//! [`SHUTTER_FRAMES`] of them, were not painted — and the first still to see
//! one was followed by one *confirming* still whose own window was taken on
//! trust. That stopped the loop, and it is not what the policy is now,
//! because of what it did on a slow machine.
//!
//! # The picture, not the clock
//!
//! Every rule above asks a timestamp whether a frame is the still's shutter
//! or the page changing, and a timestamp cannot say. Where a still takes long
//! enough for the page to change while it is out — a still at scale 2 on
//! GitHub's shared macOS VM, or any machine that descheduled this loop — the
//! pane could end on a still taken *before* the page changed, and stay there,
//! in three ways:
//!
//! - The change's frame was stamped before the reply but read after it. The
//!   still was credited with its reply, so the frame was older than what was
//!   on screen, dropped, and not motion.
//! - It was read while the still was in flight, and at scale 2 the still had
//!   provoked no frame of its own. One frame in flight was the one forgiven,
//!   so the still went up over the change and the tab was at rest.
//! - It landed in a confirming still's window, which was trusted.
//!
//! What does tell them apart is what the frame shows. Probed against
//! `chrome-headless-shell` 153, a still's shutter frame at scale 1 is **byte
//! for byte the frame before it**, sixteen of sixteen. At scale 2 on a
//! 3200x1760 pane the engine draws a page nothing is happening to in two ways
//! by turns: a still provokes a frame of the second, in flight, and sometimes
//! a frame of the first again a couple of hundred milliseconds later. The two
//! differ by a few hundred bytes of JPEG and nothing anybody could see.
//!
//! So a frame that shows **nothing new** is not painted, not motion, and not
//! counted against a still ([`Picture`]): one that repeats the frame before
//! it, or one that shows what the page showed when a still was asked for
//! since the page last moved. A still asked for while the page showed the
//! one drawing makes the other a picture the page has been photographed with
//! as well, so a page at rest costs one still at scale 1 and at most two at
//! scale 2, however the engine times its frames.
//!
//! A frame that shows something new is the page changing, as far as anything
//! can tell, and the policy no longer argues with it. A still is credited
//! with the moment it was *asked for*, the earliest it could depict, so only
//! a frame stamped before that is older than it. A new picture stamped after
//! it — in flight, read after the reply, or in the still's window after the
//! reply — may be newer than the still, so the tab is not at rest until a
//! still has been asked for since ([`Motion::at_rest`]). Up to
//! [`SHUTTER_FRAMES`] of them in a still's window after its reply are held
//! rather than painted ([`Motion::still_arrived`], at least
//! [`SHUTTER_GRACE`]), so a page at scale 2 does not flash a JPEG between two
//! stills; more are the page moving, and go up as it moves. Nothing is taken
//! on trust, so the pane does not end on a still from before the last new
//! picture. The one thing this cannot see is a page that changes back to a
//! picture it was photographed with while a still of the change is being
//! taken, with no frame painted in between: that still may show the change
//! the page has already undone. A page doing that within a round trip of the
//! engine is indistinguishable, frame for frame, from the engine's own two
//! drawings above, and the loop is the worse of the two.
//!
//! The clock all of that is measured on is the wall clock:
//! `Page.screencastFrame` carries `metadata.timestamp`, which CDP defines as
//! seconds since the epoch, and the engine is a child process on this machine,
//! so it is the same epoch this program reads with [`std::time::SystemTime`]
//! (checked on the VM: the two agree).

use std::hash::Hasher;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The JPEG quality the screencast runs at. See the table above.
pub const QUALITY: u32 = 85;

/// How long without a screencast frame counts as the page having stopped.
///
/// Long enough that it is never tripped between two frames of a scroll: the
/// gap at q85 is about 17 ms on the host the format was chosen on and 24 to
/// 27 ms in the VirtualBox machine, so this is ten frames' worth of the slow
/// one. Short enough that letting go of the wheel and the text sharpening
/// still feel like one event rather than two.
///
/// It was 150 ms, which is longer than a wheel notch's hundred milliseconds of
/// animation and shorter than the 150 to 300 ms between two notches of a hand
/// on a wheel — so it fired in the gaps between them, once per notch. This is
/// the frame half of the fix; [`INPUT_QUIET`] is the half that settles it,
/// because no frame-quiet interval on its own can tell the gap between two
/// notches from the end of a scroll.
pub const REST_AFTER: Duration = Duration::from_millis(250);

/// How long after the last wheel notch or key the page is left alone.
///
/// A hand turning a wheel produces notches 150 to 300 ms apart, and each one
/// makes the engine animate for about 100 ms and then stop. Nothing about the
/// frames says whether a gap is the end of a scroll or the moment before the
/// next notch; the wheel does. So a still is not worth asking for until the
/// wheel has been quiet for longer than the longest gap a hand leaves —
/// 400 ms is 300 with room — and a scroll then costs one still at the end of
/// it rather than one per notch.
///
/// A notch is now the start of an animation this program drives rather than a
/// single dispatched wheel event — a tick every 16 ms until every curve has
/// been delivered, which outlives the notch that asked for it by
/// [`crate::scroll::D`]. [`Motion::input`] is given the moment of the last of
/// those ticks as well as the moment of every notch, so this interval is
/// counted from the end of the *animation* rather than from the end of the
/// hand. The ticks happen on the animator's own thread and reach this state
/// through [`crate::scroll::Wheel::activity`], which the loop reads once a
/// pass. See [`crate::scroll`] and `docs/design/browser.md`.
pub const INPUT_QUIET: Duration = Duration::from_millis(400);

/// How many screencast frames a still produces just by being taken.
///
/// `Page.captureScreenshot` forces a capture of the page's surface, and the
/// screencast is watching that same surface, so the screenshot shows up in it.
/// Probed against `chromium-shell` on a page nothing was happening to: eight
/// screenshots in a row, eight screencast frames, one each, every one stamped
/// about 4 ms after the request went out and 35 to 48 ms before the reply came
/// back — and not one frame in the three seconds either side of them.
/// `fromSurface=false` makes no difference.
///
/// That was at a device scale of 1. At 2 it is one or two, in flight or just
/// after the reply (see "A second shutter, after the reply" above), so two is
/// the most a still's window holds: up to two frames there may be the still
/// photographing itself, and anything more is the page moving. A frame that
/// repeats the picture before it is not counted at all ([`Picture`]), so what
/// this bounds is new pictures, and each of those owes another still.
/// `tests/engine.rs` asserts both scales, because this is the number the rule
/// is built on and an engine that changed it would otherwise change the
/// policy quietly.
pub const SHUTTER_FRAMES: u32 = 2;

/// The least a still's window runs on after its reply: a frame stamped up to
/// this long after it — or up to the still's own round trip, if that was
/// longer — may be the engine's second shutter rather than the page. The
/// probe at scale 2 put that frame 20 to 45 ms after the reply.
pub const SHUTTER_GRACE: Duration = Duration::from_millis(60);

/// Wall-clock seconds, on the clock CDP's `TimeSinceEpoch` uses.
pub fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or(0.0)
}

/// What a screencast frame shows, as far as telling two frames apart goes: a
/// hash of the frame as the engine sent it.
///
/// Only ever compared with another frame's, so what it needs is to be equal
/// when the bytes are and almost never otherwise. A frame is hashed as it
/// arrives, before anything decodes it — the base64 text is as good as the
/// JPEG for this, and a quarter of a millisecond for a 1280x768 frame. See
/// "The picture, not the clock" above for why it is asked at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picture(u64);

impl Picture {
    /// The picture of a frame, from its bytes in whichever form they came.
    pub fn of(frame: &[u8]) -> Picture {
        // `DefaultHasher::new` has fixed keys, so this is the same number for
        // the same bytes every time, which is the whole of what is wanted.
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        hasher.write(frame);
        Picture(hasher.finish())
    }
}

/// Whether the tab in front is moving, and what is on its screen.
///
/// One of these, not one per tab: only the tab in front has a screencast and
/// only the tab in front is painted, so a switch resets this rather than
/// keeping a second copy that would be wrong by the time it was used.
#[derive(Debug)]
pub struct Motion {
    /// When the last screencast frame arrived, on the monotonic clock.
    last_frame: Instant,
    /// When the person last turned the wheel or pressed a key, if they have.
    last_input: Option<Instant>,
    /// The newest moment what is on screen depicts, or is known to be newer
    /// than, in wall-clock seconds: a frame's stamp, or the moment the still
    /// on screen was asked for. A frame stamped before it is older than the
    /// screen.
    painted_at: f64,
    /// A still is on screen and no frame has been painted over it — or a
    /// still failed with nothing moving, which is as much rest as there is to
    /// be had. See [`Motion::at_rest`] for what else rest takes.
    still_up: bool,
    /// A still has been asked for and its reply has not come back.
    in_flight: bool,
    /// How many new pictures stamped after it was asked for have arrived
    /// while it was out. See [`SHUTTER_FRAMES`].
    frames_in_flight: u32,
    /// When the still in flight was asked for, in wall-clock seconds.
    requested_at: f64,
    /// The end of the painted still's window after its reply, in wall-clock
    /// seconds, and how many more new pictures in it are held rather than
    /// painted. See "The picture, not the clock" above.
    shutter_until: f64,
    shutter_left: u32,
    /// What the newest frame showed, whatever became of it.
    picture: Option<Picture>,
    /// What the page showed each time a still was asked for, since it last
    /// moved. A few at most: one, or two at scale 2, where the engine draws
    /// a page nothing is happening to in two ways by turns.
    photographed: Vec<Picture>,
}

impl Motion {
    /// A tab that has just come to the front: nothing painted, nothing at
    /// rest, and the clock started so that a page which never paints gets its
    /// still one rest interval from now.
    pub fn new(now: Instant) -> Motion {
        Motion {
            last_frame: now,
            last_input: None,
            painted_at: 0.0,
            still_up: false,
            in_flight: false,
            frames_in_flight: 0,
            requested_at: 0.0,
            shutter_until: 0.0,
            shutter_left: 0,
            picture: None,
            photographed: Vec::new(),
        }
    }

    /// The same, for a tab switch or a resize.
    ///
    /// A still that was in flight is forgotten along with everything else: its
    /// reply describes a page that is no longer the one in front, or one that
    /// is no longer the size it was. So are the pictures, which were of that
    /// page or that size.
    pub fn reset(&mut self, now: Instant) {
        *self = Motion::new(now);
    }

    /// A screencast frame arrived. `timestamp` is its `metadata.timestamp`,
    /// and `picture` what it shows. Returns whether it is worth decoding and
    /// painting.
    ///
    /// A frame older than what is on screen is dropped, and is not motion: it
    /// shows a moment that has already been drawn, or one before the still on
    /// screen was asked for.
    ///
    /// A frame that shows **nothing new** is dropped too, and is not motion
    /// or counted against a still either: one that repeats the frame before
    /// it, or one that shows what the page showed when a still was asked for
    /// since it last moved. That is what a still's own frames are, at either
    /// scale, and letting them count as movement is what made the screen
    /// flash.
    ///
    /// Anything else is the page changing. It restarts the rest timer and is
    /// painted — unless it is in the window after a painted still's reply,
    /// where up to [`SHUTTER_FRAMES`] are held so as not to flash a JPEG
    /// between two stills. Held or painted, the page no longer looks as it
    /// did when any still was asked for, so the tab is not at rest; painted
    /// with no still out, it is the page moving, and what it looked like
    /// before is history.
    pub fn motion_frame(&mut self, timestamp: Option<f64>, picture: Picture, now: Instant) -> bool {
        if timestamp.is_some_and(|when| when < self.painted_at) {
            return false;
        }
        if self.picture.replace(picture) == Some(picture) {
            return false;
        }
        if self.photographed.contains(&picture) {
            return false;
        }
        let after_the_request = timestamp.is_none_or(|when| when >= self.requested_at);
        if self.in_flight && after_the_request {
            self.frames_in_flight += 1;
        }
        if let Some(when) = timestamp {
            if self.still_up && self.shutter_left > 0 && when <= self.shutter_until {
                self.shutter_left -= 1;
                self.last_frame = now;
                return false;
            }
            self.painted_at = when;
        }
        // Painted — a frame with no timestamp is a frame the engine described
        // oddly, and a frame in hand beats no frame — so the still is no
        // longer what is on screen.
        if !self.in_flight {
            self.photographed.clear();
        }
        self.shutter_left = 0;
        self.still_up = false;
        self.last_frame = now;
        true
    }

    /// The person turned the wheel or pressed a key, or a step of the wheel
    /// animation went out. See [`INPUT_QUIET`].
    ///
    /// The later of what is known and what is being told, because two clocks
    /// feed this. A key is stamped when the loop reads it; a tick of the
    /// animation is stamped by [`crate::scroll::Wheel`]'s thread and read off
    /// an atomic on whichever pass comes next, so it can arrive after a key
    /// that happened later than it did. Taking the later of the two is what
    /// stops a tick read a pass late from winding the quiet interval
    /// backwards.
    pub fn input(&mut self, now: Instant) {
        self.last_input = Some(match self.last_input {
            Some(was) if was > now => was,
            _ => now,
        });
    }

    /// A frame was acknowledged on a route paced by the link
    /// ([`crate::route::Route::paced`]): until now the engine could send
    /// nothing, so its silence is only silence from now on.
    ///
    /// Without this a link that takes more than [`REST_AFTER`] a frame is a
    /// page that looks at rest between every two frames of an animation, and
    /// a full-size still went out after each small one — measured at
    /// 160 kB/s, half of the frames on the wire were stills of a page that
    /// never stopped.
    pub fn frame_acknowledged(&mut self, now: Instant) {
        if now > self.last_frame {
            self.last_frame = now;
        }
    }

    /// Whether the page has been quiet long enough to be worth a lossless
    /// picture: no frame for [`REST_AFTER`], no input for [`INPUT_QUIET`],
    /// nothing already on its way, and not already at rest.
    pub fn wants_still(&self, now: Instant) -> bool {
        !self.at_rest()
            && !self.in_flight
            && now.duration_since(self.last_frame) >= REST_AFTER
            && match self.last_input {
                Some(at) => now.duration_since(at) >= INPUT_QUIET,
                // Nobody has touched this tab, so there is nobody to wait for.
                None => true,
            }
    }

    /// A still has been asked for.
    ///
    /// Nothing about the screen changes here. What starts is the window the
    /// rule is about: the new pictures that arrive between now and the reply
    /// are counted, and what the page shows now is one they are not new
    /// against.
    /// `at` is the wall clock, read **before** the request went out: it is
    /// the earliest moment the still can depict, and a frame stamped before
    /// it is older than the still. The window of a still already on screen
    /// closes, since this one supersedes it.
    pub fn still_requested(&mut self, at: f64) {
        self.in_flight = true;
        self.frames_in_flight = 0;
        self.requested_at = at;
        self.shutter_left = 0;
        self.remember_the_picture();
    }

    /// The still came back, at wall-clock `replied_at`. Returns whether it
    /// should be decoded and painted.
    ///
    /// `false` means more new pictures arrived than [`SHUTTER_FRAMES`]: the
    /// page moved while the engine was drawing, what is on screen is newer
    /// than this, and the tab stays in motion so that the next still waits
    /// for quiet all over again.
    ///
    /// `true` puts the still on screen, credited with the moment it was asked
    /// for. Whether that is rest depends on what the page shows now, which a
    /// new picture while it was out may have changed; see
    /// [`Motion::at_rest`].
    ///
    /// The still's window then runs on past the reply for as long as the
    /// still took, and at least [`SHUTTER_GRACE`], with what is left of
    /// [`SHUTTER_FRAMES`] to hold there.
    pub fn still_arrived(&mut self, replied_at: f64) -> bool {
        if !self.in_flight {
            return false;
        }
        self.in_flight = false;
        let frames = std::mem::take(&mut self.frames_in_flight);
        if frames > SHUTTER_FRAMES {
            return false;
        }
        let took = (replied_at - self.requested_at).max(SHUTTER_GRACE.as_secs_f64());
        self.painted_at = self.painted_at.max(self.requested_at);
        self.shutter_until = replied_at + took;
        self.shutter_left = SHUTTER_FRAMES - frames;
        self.still_up = true;
        true
    }

    /// The still could not be taken, or would not decode.
    ///
    /// If nothing new was painted while it was out, the tab is marked at rest
    /// anyway, so that a page whose screenshots fail is asked once rather
    /// than fifty times a second until it moves again. If something was — the
    /// page moved while the engine failed to photograph it, a timeout during
    /// a load — the screen is that frame, so the tab stays in motion and the
    /// next quiet asks again; otherwise the pane would stay on a moving frame
    /// for as long as the page stayed still.
    pub fn still_failed(&mut self) {
        self.in_flight = false;
        let moved = std::mem::take(&mut self.frames_in_flight) > 0;
        if !moved {
            self.still_up = true;
            self.remember_the_picture();
        }
    }

    /// Whether a still has been asked for and not yet answered.
    pub fn still_in_flight(&self) -> bool {
        self.in_flight
    }

    /// Whether the tab is at rest: a lossless still is what was painted last,
    /// **and** the page, as the screencast last showed it, looks as it did
    /// when a still was asked for since it last moved.
    ///
    /// The second half is what stops the pane ending on a still from before
    /// the page changed. A still is taken some time after it is asked for
    /// and nothing says when, so a new picture after the request may be newer
    /// than the still; it is not at rest until one has been asked for since.
    pub fn at_rest(&self) -> bool {
        self.still_up
            && self
                .picture
                .is_none_or(|picture| self.photographed.contains(&picture))
    }

    /// What the page shows now is one it has been photographed with.
    fn remember_the_picture(&mut self) {
        if let Some(picture) = self.picture {
            if !self.photographed.contains(&picture) {
                self.photographed.push(picture);
            }
        }
    }
}

/// How the motion cast is asked for.
///
/// On the local route it is what it always was: JPEG at [`QUALITY`], every
/// frame, at the pane's size. On a route that sends the engine's PNG
/// ([`crate::route::Payload::Png`]) it is PNG — which on this engine costs
/// what JPEG does on text, 60 fps at 35 kB a frame against 59 at 110 kB on an
/// animating page — every nth frame for the route's cap, and at a width
/// [`Throttle`] steps down when the link cannot keep up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cast {
    /// `format: "png"` and no quality, rather than JPEG at [`QUALITY`].
    pub png: bool,
    /// `everyNthFrame`.
    pub every_nth: u32,
    /// The cast's size as a fraction of the pane: 1, 2 or 3, meaning the
    /// pane, half of it, and three-eighths.
    pub step: u8,
}

impl Cast {
    /// The cast for a route, at the pane's full size.
    pub fn for_route(route: &crate::route::Route) -> Cast {
        Cast {
            png: route.payload == crate::route::Payload::Png,
            every_nth: route.every_nth.max(1),
            step: 1,
        }
    }

    /// The `maxWidth` and `maxHeight` for a pane of `pane` pixels.
    ///
    /// Half, then three-eighths, and nothing between: resampled text
    /// compresses worse than crisp text, so three-quarters of the pane was a
    /// step *up* in bytes on a page of text (261 kB against 193 kB at 1280
    /// wide), while half is a quarter of the bytes on pictures and two-thirds
    /// on text, and three-eighths is the last step at which a page in motion
    /// is still legible. The terminal scales the frame into the same cells,
    /// as it already does for a zoomed page.
    pub fn size(&self, pane: (u32, u32)) -> (u32, u32) {
        let scale = |n: u32| match self.step {
            0 | 1 => n,
            2 => n.div_ceil(2),
            _ => (n * 3).div_ceil(8),
        };
        (scale(pane.0).max(1), scale(pane.1).max(1))
    }
}

impl Default for Cast {
    /// JPEG, every frame, the pane's size: the local route.
    fn default() -> Cast {
        Cast {
            png: false,
            every_nth: 1,
            step: 1,
        }
    }
}

/// How long a frame may take from being handed to the pane to being written
/// before it counts as the link falling behind.
pub const SLOW: Duration = Duration::from_millis(400);

/// How quickly a frame has to go for it to count as the link keeping up.
pub const FAST: Duration = Duration::from_millis(80);

/// How long every frame has to have been fast before the cast steps back up.
pub const RECOVER: Duration = Duration::from_secs(5);

/// The coarsest step: three-eighths of the pane.
pub const MAX_STEP: u8 = 3;

/// Steps the cast's size down when frames wait on the link, and up when
/// they stop waiting.
///
/// The acknowledgements already make the engine produce frames at the rate
/// the link takes them (see `crate::app`): one ack when a frame has gone,
/// and the engine's next frame is the page as it is then. What that cannot
/// do is make a frame smaller, and a quarter-megabyte frame over a
/// 250 kB/s link is one a second however current it is. So: two frames in a
/// row that took [`SLOW`] or longer step down one; [`RECOVER`] of frames
/// that each took under [`FAST`] step up one. Off on the local route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Throttle {
    step: u8,
    slow_in_a_row: u8,
    fast_since: Option<Instant>,
}

impl Default for Throttle {
    fn default() -> Throttle {
        Throttle {
            step: 1,
            slow_in_a_row: 0,
            fast_since: None,
        }
    }
}

impl Throttle {
    /// The step the cast is at.
    pub fn step(&self) -> u8 {
        self.step
    }

    /// Told once per frame written, with how long it took. `Some(step)` when
    /// the cast should be restarted at a new size.
    pub fn frame_waited(&mut self, waited: Duration, now: Instant) -> Option<u8> {
        if waited >= SLOW {
            self.fast_since = None;
            self.slow_in_a_row = self.slow_in_a_row.saturating_add(1);
            if self.slow_in_a_row >= 2 && self.step < MAX_STEP {
                self.step += 1;
                self.slow_in_a_row = 0;
                return Some(self.step);
            }
            return None;
        }
        self.slow_in_a_row = 0;
        if waited >= FAST {
            self.fast_since = None;
            return None;
        }
        let since = *self.fast_since.get_or_insert(now);
        if self.step > 1 && now.duration_since(since) >= RECOVER {
            self.step -= 1;
            self.fast_since = Some(now);
            return Some(self.step);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long enough after everything that both kinds of quiet have run out.
    const QUIET: Duration = Duration::from_millis(500);

    /// The page as the screencast showed it last.
    fn page() -> Picture {
        Picture::of(b"the page")
    }

    /// The page after something on it changed.
    fn changed() -> Picture {
        Picture::of(b"the page, changed")
    }

    /// The page `n` frames into a scroll: a new picture every time.
    fn scrolled(n: u32) -> Picture {
        Picture::of(format!("the page, {n} frames down").as_bytes())
    }

    /// The frame `Page.captureScreenshot` produces of its own accord: stamped
    /// a few milliseconds after the request and well before the reply, which
    /// is what the probe in `tests/engine.rs` measured, and showing what the
    /// frame before it showed, byte for byte.
    fn shutter(motion: &mut Motion, requested_at: f64, at: Instant) -> bool {
        motion.motion_frame(Some(requested_at + 0.004), page(), at)
    }

    /// A tab whose page has painted once, as `page()`, at `1000.0`.
    fn painted(start: Instant) -> Motion {
        let mut motion = Motion::new(start);
        assert!(motion.motion_frame(Some(1000.0), page(), start));
        motion
    }

    /// A page that is scrolling: frames in order, every one painted, and no
    /// still ever asked for.
    #[test]
    fn a_moving_page_is_all_motion_frames_and_no_stills() {
        let start = Instant::now();
        let mut motion = Motion::new(start);
        let mut at = start;
        for tick in 1..60u32 {
            at += Duration::from_millis(17);
            let stamp = 1000.0 + tick as f64 * 0.017;
            assert!(motion.motion_frame(Some(stamp), scrolled(tick), at));
            assert!(!motion.wants_still(at), "frame {tick}");
            assert!(!motion.at_rest());
        }
    }

    /// A page that stops: one still, and then nothing however long it is left.
    ///
    /// Including the frame the still takes of itself, which is the whole of
    /// what used to make this four stills a second for ever.
    #[test]
    fn a_page_that_stops_costs_one_still_and_then_nothing() {
        let start = Instant::now();
        let at = start + Duration::from_millis(17);
        let mut motion = painted(at);

        assert!(!motion.wants_still(at + Duration::from_millis(249)));
        let resting = at + REST_AFTER;
        assert!(motion.wants_still(resting));

        let requested_at = 1000.2;
        motion.still_requested(requested_at);
        assert!(motion.still_in_flight());
        assert!(!motion.wants_still(resting), "one at a time");
        // Ninety milliseconds of engine, and the shutter frame turns up in
        // them: the picture already on the screen, so not painted.
        assert!(!shutter(&mut motion, requested_at, resting));
        assert!(motion.still_arrived(requested_at + 0.09));
        assert!(motion.at_rest());

        // And an hour later it still wants nothing.
        assert!(!motion.wants_still(resting + Duration::from_secs(3600)));
    }

    /// The same, with the shutter frame arriving after the reply rather than
    /// before it. Either way it is the picture that is already on the screen.
    #[test]
    fn the_frame_a_still_takes_of_itself_is_not_the_page_moving() {
        let start = Instant::now();
        let mut motion = painted(start);
        let resting = start + REST_AFTER;
        assert!(motion.wants_still(resting));
        let requested_at = 1000.3;
        motion.still_requested(requested_at);
        assert!(motion.still_arrived(requested_at + 0.09));
        assert!(motion.at_rest());

        let late = resting + Duration::from_millis(95);
        assert!(
            !shutter(&mut motion, requested_at, late),
            "it shows what the still already showed"
        );
        assert!(
            motion.at_rest(),
            "and it must not clear the rest, or the next still is 250 ms away \
             and the screen flashes for ever"
        );
        assert!(!motion.wants_still(late + Duration::from_secs(3600)));
    }

    /// #85's guarantee, whatever the engine does with its shutter: frames
    /// that repeat the page — in flight, just after the reply, well after
    /// the window, two at a time as at scale 2 — never cost a second still.
    #[test]
    fn a_page_at_rest_costs_one_still_at_either_scale_however_its_shutter_is_timed() {
        let start = Instant::now();
        let mut motion = painted(start);
        let resting = start + REST_AFTER;
        motion.still_requested(1000.3);
        assert!(!motion.motion_frame(Some(1000.31), page(), resting));
        assert!(motion.still_arrived(1000.38));
        for stamp in [1000.39, 1000.40, 1000.46, 1000.9, 1005.0] {
            assert!(!motion.motion_frame(Some(stamp), page(), resting));
            assert!(motion.at_rest(), "a repeat at {stamp}");
        }
        assert!(!motion.wants_still(resting + Duration::from_secs(3600)));
    }

    /// At scale 2 on a Retina pane the engine draws a page nothing is
    /// happening to in two ways by turns, and a still provokes the other one.
    /// The first still cannot tell that from a change, so a second is taken;
    /// after that both drawings are ones the page was photographed with, and
    /// however they alternate, nothing more is asked for.
    #[test]
    fn at_scale_two_a_page_drawn_two_ways_by_turns_costs_two_stills_and_no_more() {
        let start = Instant::now();
        let mut motion = painted(start);
        let other = Picture::of(b"the page, drawn the other way");
        let resting = start + REST_AFTER;
        motion.still_requested(1000.3);
        assert!(motion.motion_frame(Some(1000.36), other, resting));
        assert!(motion.still_arrived(1000.38));
        assert!(
            !motion.at_rest(),
            "the other drawing may have been a change"
        );

        let quiet = resting + REST_AFTER;
        assert!(motion.wants_still(quiet));
        motion.still_requested(1000.8);
        assert!(!motion.motion_frame(Some(1000.86), other, quiet));
        assert!(motion.still_arrived(1000.88));
        assert!(motion.at_rest());

        // The first drawing back, well past the window, and the two by turns
        // after it.
        let mut at = quiet;
        for (n, stamp) in [1001.1, 1001.5, 1002.0, 1003.0].into_iter().enumerate() {
            at += Duration::from_millis(400);
            let picture = if n % 2 == 0 { page() } else { other };
            assert!(!motion.motion_frame(Some(stamp), picture, at), "{n}");
            assert!(motion.at_rest(), "{n}");
        }
        assert!(!motion.wants_still(at + Duration::from_secs(3600)));

        // And a change after all that is a change.
        at += Duration::from_millis(400);
        assert!(motion.motion_frame(Some(1004.0), changed(), at));
        assert!(motion.wants_still(at + QUIET));
    }

    /// Three new pictures in the window are the page moving, and the still
    /// goes.
    #[test]
    fn frames_beyond_the_shutter_throw_the_still_away() {
        let start = Instant::now();
        let mut motion = painted(start);
        let resting = start + REST_AFTER;
        assert!(motion.wants_still(resting));
        let requested_at = 1000.3;
        motion.still_requested(requested_at);

        // The shutter, and then the page itself, at the VM's 25 ms gap.
        assert!(!shutter(&mut motion, requested_at, resting));
        assert!(motion.motion_frame(Some(requested_at + 0.015), scrolled(1), resting));
        assert!(motion.motion_frame(Some(requested_at + 0.03), scrolled(2), resting));
        let moved = resting + Duration::from_millis(40);
        assert!(motion.motion_frame(Some(requested_at + 0.04), scrolled(3), moved));
        assert!(
            !motion.still_arrived(requested_at + 0.09),
            "the page did not stay still for the whole of it"
        );
        assert!(!motion.at_rest(), "so the tab is still in motion");
        assert!(!motion.still_in_flight());

        // And the next still waits for quiet from that frame, not from before.
        assert!(!motion.wants_still(moved + Duration::from_millis(249)));
        assert!(motion.wants_still(moved + QUIET));
    }

    /// A reply with nothing new in between is painted, and that is the only
    /// thing that puts a tab at rest.
    #[test]
    fn a_still_with_no_frame_but_its_own_is_the_one_that_is_painted() {
        let start = Instant::now();
        let mut motion = painted(start);
        let resting = start + REST_AFTER;
        assert!(motion.wants_still(resting));
        motion.still_requested(1000.3);
        assert!(motion.still_arrived(1000.4));
        assert!(motion.at_rest());
    }

    /// A hand on the wheel: notches 200 ms apart, the frames they cause dying
    /// out after each one, and not a single still until the hand stops.
    #[test]
    fn a_hand_on_the_wheel_is_not_interrupted_by_a_still() {
        let start = Instant::now();
        let mut motion = Motion::new(start);
        let mut at = start;
        for notch in 0..10u32 {
            motion.input(at);
            // About a hundred milliseconds of animation and then quiet until
            // the next notch: four frames at the VM's 25 ms gap.
            for frame in 1..=4u32 {
                let when = at + Duration::from_millis(frame as u64 * 25);
                let stamp = 1000.0 + notch as f64 * 0.2 + frame as f64 * 0.025;
                assert!(motion.motion_frame(Some(stamp), scrolled(notch * 4 + frame), when));
            }
            // Every instant between this notch and the next is checked,
            // because one still anywhere in there is the flicker.
            for step in 0..20u64 {
                let when = at + Duration::from_millis(step * 10);
                assert!(!motion.wants_still(when), "notch {notch}, {step}0 ms in");
            }
            at += Duration::from_millis(200);
        }
        // The hand comes off, and the still arrives once the wheel has been
        // quiet for its interval.
        let last_input = at - Duration::from_millis(200);
        assert!(!motion.wants_still(last_input + Duration::from_millis(399)));
        assert!(motion.wants_still(last_input + INPUT_QUIET));
    }

    /// Frames can be quiet long before the wheel is, and that alone is not
    /// enough.
    #[test]
    fn frames_quiet_but_a_key_just_pressed_is_not_rest() {
        let start = Instant::now();
        let mut motion = painted(start);
        let quiet = start + REST_AFTER;
        assert!(motion.wants_still(quiet), "frames alone would allow it");
        motion.input(quiet);
        assert!(!motion.wants_still(quiet));
        assert!(!motion.wants_still(quiet + Duration::from_millis(399)));
        assert!(motion.wants_still(quiet + INPUT_QUIET));
    }

    /// A frame that was in the mailbox before a still that was painted,
    /// arriving after it. It was stamped before the still was even asked
    /// for, so the still is newer whatever it shows, and it is not motion
    /// either.
    #[test]
    fn a_frame_from_before_a_painted_still_does_not_overwrite_it() {
        let start = Instant::now();
        let mut motion = painted(start);
        motion.still_requested(1000.4);
        assert!(motion.still_arrived(1000.5));

        let late = start + Duration::from_millis(10);
        assert!(
            !motion.motion_frame(Some(1000.3), changed(), late),
            "captured before the still that is on screen was asked for, so it is stale"
        );
        assert!(
            motion.at_rest(),
            "and a moment already drawn is not movement"
        );
        assert!(!motion.wants_still(late + QUIET));
    }

    /// The loop found on Wikipedia at scale 2 was frames stamped just after
    /// a still's reply. When they repeat the page, they are its shutter and
    /// ask for nothing; when one shows something new it is held rather than
    /// painted — no JPEG between two stills — and one more still follows,
    /// which is the one the pane ends on.
    #[test]
    fn a_new_picture_just_after_a_stills_reply_is_held_and_another_still_follows() {
        let start = Instant::now();
        let mut motion = painted(start);
        let resting = start + REST_AFTER;
        assert!(motion.wants_still(resting));
        motion.still_requested(1000.3);
        assert!(motion.still_arrived(1000.38));
        assert!(motion.at_rest());

        // 20 ms after the reply: inside the window, which is the still's own
        // 80 ms round trip.
        let second = resting + Duration::from_millis(100);
        assert!(
            !motion.motion_frame(Some(1000.4), changed(), second),
            "held: the still stays on the screen"
        );
        assert!(!motion.at_rest(), "but another still is owed");
        assert!(!motion.wants_still(second + Duration::from_millis(249)));
        let quiet = second + REST_AFTER;
        assert!(motion.wants_still(quiet));

        motion.still_requested(1000.8);
        assert!(motion.still_arrived(1000.88));
        assert!(motion.at_rest());
        // Its own shutter after the reply, twice over, repeats the change.
        let late = quiet + Duration::from_millis(100);
        assert!(!motion.motion_frame(Some(1000.9), changed(), late));
        assert!(!motion.motion_frame(Some(1000.91), changed(), late));
        assert!(motion.at_rest(), "the second still is the last word");
        assert!(!motion.wants_still(late + Duration::from_secs(3600)));
    }

    /// Two new pictures in flight are painted as they come, and the still
    /// that follows them is painted too — but it may be older than them, so
    /// it owes another.
    #[test]
    fn two_frames_in_flight_paint_the_still_and_owe_another() {
        let start = Instant::now();
        let mut motion = painted(start);
        let resting = start + REST_AFTER;
        motion.still_requested(1000.3);
        assert!(motion.motion_frame(Some(1000.32), scrolled(1), resting));
        assert!(motion.motion_frame(Some(1000.36), changed(), resting));
        assert!(motion.still_arrived(1000.38), "the still goes up over them");
        assert!(!motion.at_rest(), "and one more is owed");
        let quiet = resting + REST_AFTER;
        assert!(motion.wants_still(quiet));
        motion.still_requested(1000.8);
        assert!(!motion.motion_frame(Some(1000.82), changed(), quiet));
        assert!(motion.still_arrived(1000.88));
        assert!(
            motion.at_rest(),
            "nothing new came while the second was out"
        );
    }

    /// A new picture past a still's window is the page moving, as it always
    /// was: painted, and the rest is over.
    #[test]
    fn a_frame_after_a_stills_window_is_painted() {
        let start = Instant::now();
        let mut motion = painted(start);
        motion.still_requested(1000.3);
        assert!(motion.still_arrived(1000.33));
        // The window is at least SHUTTER_GRACE: 60 ms past the reply.
        let past = 1000.33 + SHUTTER_GRACE.as_secs_f64() + 0.005;
        assert!(
            motion.motion_frame(Some(past), changed(), start),
            "past the window, so it goes up"
        );
        assert!(!motion.at_rest());
    }

    /// A page that changes once, late — a script's timer — in the moment
    /// after a still: the pane ends on a still taken after it.
    #[test]
    fn a_late_change_in_a_stills_window_still_ends_in_a_still_that_shows_it() {
        let start = Instant::now();
        let mut motion = painted(start);
        let resting = start + REST_AFTER;
        motion.still_requested(1000.3);
        assert!(!shutter(&mut motion, 1000.3, resting));
        assert!(motion.still_arrived(1000.39));
        let changed_at = resting + Duration::from_millis(120);
        assert!(
            !motion.motion_frame(Some(1000.42), changed(), changed_at),
            "held"
        );
        let quiet = changed_at + REST_AFTER;
        assert!(motion.wants_still(quiet), "and photographed again");
        motion.still_requested(1000.7);
        assert!(motion.still_arrived(1000.79));
        assert!(motion.at_rest());
    }

    // The three ways the pane ended on a still older than the page, on the
    // shared macOS VM, where a still at scale 2 is slow enough for a page's
    // timer to fire while it is out. Each is the order of events that did it,
    // and each ends on a still asked for after the change.

    /// The change's frame is stamped while the still is out and read after
    /// its reply. Crediting the still with its reply made that frame older
    /// than the still, and it was dropped.
    #[test]
    fn a_change_stamped_while_a_still_was_out_and_read_after_its_reply_is_not_lost() {
        let start = Instant::now();
        let mut motion = painted(start);
        let resting = start + REST_AFTER;
        motion.still_requested(1000.3);
        assert!(motion.still_arrived(1000.7), "a slow still, 400 ms");
        let read = resting + Duration::from_millis(400);
        assert!(!motion.motion_frame(Some(1000.65), changed(), read), "held");
        assert!(!motion.at_rest(), "the still may have been taken before it");
        assert!(motion.wants_still(read + QUIET));
    }

    /// The change's frame is read while the still is out, and the still
    /// provoked no frame of its own, as at scale 2 it often does not. One
    /// frame in flight was forgiven as the shutter.
    #[test]
    fn one_change_in_flight_is_not_taken_for_the_stills_own_frame() {
        let start = Instant::now();
        let mut motion = painted(start);
        let resting = start + REST_AFTER;
        motion.still_requested(1000.3);
        let read = resting + Duration::from_millis(350);
        assert!(motion.motion_frame(Some(1000.65), changed(), read));
        assert!(motion.still_arrived(1000.7));
        assert!(!motion.at_rest(), "the still may have been taken before it");
        assert!(motion.wants_still(read + QUIET));
        motion.still_requested(1001.3);
        assert!(motion.still_arrived(1001.4));
        assert!(motion.at_rest());
    }

    /// The change lands in the window of a still that was itself taken
    /// because of an earlier new picture. That still's window was trusted.
    #[test]
    fn a_change_after_a_second_still_is_not_trusted_away() {
        let start = Instant::now();
        let mut motion = painted(start);
        let resting = start + REST_AFTER;
        motion.still_requested(1000.3);
        assert!(motion.still_arrived(1000.38));
        assert!(!motion.motion_frame(Some(1000.4), scrolled(1), resting));
        let quiet = resting + REST_AFTER;
        assert!(motion.wants_still(quiet));
        motion.still_requested(1000.8);
        assert!(motion.still_arrived(1000.88));
        assert!(motion.at_rest());
        assert!(!motion.motion_frame(Some(1000.9), changed(), quiet), "held");
        assert!(!motion.at_rest(), "a third still is owed");
        assert!(motion.wants_still(quiet + QUIET));
    }

    /// A still that fails after frames went up while it was out: the screen
    /// is one of those frames, so the tab is not at rest and asks again —
    /// the first load, when a screenshot can time out while the page paints.
    #[test]
    fn a_still_that_fails_after_frames_were_painted_is_asked_for_again() {
        let start = Instant::now();
        let mut motion = painted(start);
        let resting = start + REST_AFTER;
        motion.still_requested(1000.3);
        let moved = resting + Duration::from_millis(500);
        assert!(motion.motion_frame(Some(1000.8), changed(), moved));
        motion.still_failed();
        assert!(!motion.at_rest());
        assert!(motion.wants_still(moved + QUIET));
    }

    /// A tab switch forgets everything, including a still that was in flight:
    /// its reply is about the page that was left behind.
    #[test]
    fn switching_tabs_starts_the_policy_again() {
        let start = Instant::now();
        let mut motion = Motion::new(start);
        motion.motion_frame(Some(2000.0), page(), start);
        motion.still_requested(2000.1);
        assert!(motion.still_in_flight());

        let switched = start + Duration::from_secs(5);
        motion.reset(switched);
        assert!(!motion.at_rest());
        assert!(!motion.still_in_flight());
        assert!(!motion.still_arrived(2000.6), "nothing is owed a reply now");
        assert!(!motion.wants_still(switched));
        // A frame from the new tab's past is not compared against the old
        // tab's clock, nor its picture against the old tab's picture.
        assert!(motion.motion_frame(Some(1.0), page(), switched));
        assert!(motion.wants_still(switched + QUIET));
    }

    /// A screenshot that fails is asked for once, not every pass.
    #[test]
    fn a_still_that_cannot_be_taken_is_not_asked_for_again() {
        let start = Instant::now();
        let mut motion = painted(start);
        let resting = start + REST_AFTER;
        assert!(motion.wants_still(resting));
        motion.still_requested(1000.3);
        motion.still_failed();
        assert!(!motion.still_in_flight());
        assert!(!motion.wants_still(resting + Duration::from_secs(10)));
        // Until the page moves, which is when it is worth trying again.
        let moved = resting + Duration::from_secs(10);
        motion.motion_frame(Some(3000.0), changed(), moved);
        assert!(motion.wants_still(moved + QUIET));
    }

    /// Two frames alike are one picture, and two that differ by a byte are
    /// two.
    #[test]
    fn a_picture_is_the_same_for_the_same_bytes_and_only_for_them() {
        assert_eq!(Picture::of(b"/9j/4AAQ"), Picture::of(b"/9j/4AAQ"));
        assert_ne!(Picture::of(b"/9j/4AAQ"), Picture::of(b"/9j/4AAR"));
        assert_ne!(Picture::of(b""), Picture::of(b"\0"));
    }

    /// The clock the timestamps are compared on is the one CDP uses.
    #[test]
    fn the_clock_is_seconds_since_the_epoch() {
        let seconds = now_seconds();
        // Somewhere between 2020 and 2100, which is enough to catch a
        // milliseconds-or-seconds mistake and nothing else.
        assert!(seconds > 1_577_836_800.0, "{seconds}");
        assert!(seconds < 4_102_444_800.0, "{seconds}");
    }

    #[test]
    fn on_a_paced_route_the_rest_interval_counts_from_the_acknowledgement_not_the_frame() {
        let start = Instant::now();
        let mut motion = Motion::new(start);
        assert!(motion.motion_frame(Some(now_seconds()), page(), start));
        // The frame took a second to go down a slow link, and was
        // acknowledged only then: the engine has had no chance to send the
        // next one, so the page is not at rest.
        let acked = start + Duration::from_secs(1);
        motion.frame_acknowledged(acked);
        assert!(!motion.wants_still(acked + REST_AFTER / 2));
        assert!(motion.wants_still(acked + REST_AFTER));
        // An acknowledgement never winds the clock back.
        motion.frame_acknowledged(start);
        assert!(motion.wants_still(acked + REST_AFTER));
    }

    #[test]
    fn two_slow_frames_in_a_row_step_the_width_down_and_five_quiet_seconds_step_it_up() {
        let start = Instant::now();
        let mut throttle = Throttle::default();
        assert_eq!(
            throttle.frame_waited(SLOW, start),
            None,
            "one is not a trend"
        );
        assert_eq!(throttle.frame_waited(FAST, start), None);
        assert_eq!(
            throttle.frame_waited(SLOW, start),
            None,
            "the run was broken"
        );
        assert_eq!(throttle.frame_waited(SLOW, start), Some(2));
        assert_eq!(throttle.step(), 2);
        let ms = |n| start + Duration::from_millis(n);
        assert_eq!(
            throttle.frame_waited(Duration::from_millis(10), ms(100)),
            None
        );
        assert_eq!(
            throttle.frame_waited(Duration::from_millis(10), ms(3000)),
            None
        );
        // A frame between fast and slow starts the quiet over.
        assert_eq!(
            throttle.frame_waited(Duration::from_millis(200), ms(4000)),
            None
        );
        assert_eq!(
            throttle.frame_waited(Duration::from_millis(10), ms(5200)),
            None
        );
        assert_eq!(
            throttle.frame_waited(Duration::from_millis(10), ms(10_199)),
            None
        );
        assert_eq!(
            throttle.frame_waited(Duration::from_millis(10), ms(10_200)),
            Some(1)
        );
    }

    #[test]
    fn the_throttle_never_goes_below_three_eighths_and_never_above_the_pane() {
        let now = Instant::now();
        let mut throttle = Throttle::default();
        for _ in 0..20 {
            throttle.frame_waited(Duration::from_secs(2), now);
        }
        assert_eq!(throttle.step(), MAX_STEP);
        let mut throttle = Throttle::default();
        for n in 0..100 {
            let later = now + Duration::from_secs(n);
            assert_eq!(throttle.frame_waited(Duration::ZERO, later), None);
        }
        assert_eq!(throttle.step(), 1);
    }

    #[test]
    fn a_cast_at_step_two_asks_for_half_the_pane_and_at_step_three_three_eighths() {
        let at = |step| Cast {
            png: true,
            every_nth: 2,
            step,
        };
        assert_eq!(at(1).size((1280, 770)), (1280, 770));
        assert_eq!(at(2).size((1280, 770)), (640, 385));
        assert_eq!(at(3).size((1280, 770)), (480, 289));
        assert_eq!(at(3).size((1, 1)), (1, 1));
        assert_eq!(
            Cast::default(),
            Cast::for_route(&crate::route::Route::local(true))
        );
    }
}
