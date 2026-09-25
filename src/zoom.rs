//! Page zoom, the HiDPI scale, and the arithmetic between terminal pixels and
//! the CSS pixels a page is laid out in.
//!
//! # What zoom is here
//!
//! One command: `Emulation.setDeviceMetricsOverride` with the pane's size
//! divided by the factor as `width` and `height`, and the factor as
//! `deviceScaleFactor`. That is what Chrome's own zoom does to a page, and it
//! was measured against `chrome-headless-shell` 153 to be exactly that: at
//! 200% on a 640x360 pane the page reads `innerWidth` 320 and
//! `devicePixelRatio` 2, lays itself out again for the narrower width without
//! a reload, and `Page.captureScreenshot` comes back 640x360 with a box that
//! was at CSS (100..150, 50..100) at device (200..300, 100..200), checked
//! pixel by pixel. The override outlives `Page.navigate`, `Page.reload` and a
//! trip through `about:blank` on its session, and a target attached
//! afterwards starts at the engine's defaults — so it is sent where the size
//! always was, when a tab is activated and when the pane is resized, and on
//! the one new occasion, a new level.
//!
//! What was measured and not taken:
//!
//! - `Emulation.setPageScaleFactor` works in the headless shell and is pinch
//!   zoom: `innerWidth` stays where it was, nothing reflows, and the frame is
//!   the top-left corner magnified, to be panned. A magnifier over the
//!   layout, not a zoom.
//! - CSS `zoom` on the document element works per document, reports a
//!   `clientWidth` that disagrees with `innerWidth`, and is gone on the next
//!   navigation, so it would have to be put back after every load and would
//!   fight any page that sets its own.
//!
//! # What the frames look like
//!
//! The screencast is capped at the CSS viewport's size, whatever its
//! `maxWidth` asks for: at 200% on 640x360 the frames are 320x180, with or
//! without `maxWidth` 1280, and a running cast follows a new level without a
//! restart. Zoomed out, the frames are the pane's size. So while a page
//! zoomed in is moving its frames are at the page's own resolution and the
//! terminal scales them into the same cells — the placement is in cells, so
//! nothing here resizes a picture — and 150 ms after it stops the lossless
//! still replaces them at the pane's full resolution. That is the argument
//! for JPEG while it moves (see [`crate::motion`]) made again: nobody reads
//! the frames that are moving.
//!
//! The two ways to get motion frames at device resolution were both
//! measured, and both cost the wrong people. `--force-device-scale-factor 2`
//! on the engine makes them sharp and rasterises every page at twice the
//! pixels whatever it is zoomed to: at 1280x784, 34.7 frames a second at
//! 100% instead of 50.3, and 10 instead of 35 at 50% — a third of the frame
//! rate taken from everyone who never zooms, to sharpen the frames of the
//! people who do. The `viewport` member of the override makes them sharp and
//! stops them following a scroll: it is a clip in page coordinates, and after
//! a wheel of 120 the page's `scrollY` was 120 while the frame still showed
//! its top. The plain override is 50 frames a second at every level.
//!
//! What the still needs is [`fit`]. It is the CSS size times the factor,
//! rounded by the engine, so at a fractional level it comes a pixel or two
//! off the pane either way — 641x368 at 150% and 175%, 640x369 at 110%,
//! 639x369 at 300% — and a picture 641 wide placed in 80 cells is resampled
//! by the terminal until every row of text goes soft. So it is cut or
//! edge-padded to exactly the pane. Above 300% the stills come three pixels
//! short, which is where [`LEVELS`] stops.
//!
//! # Coordinates
//!
//! Every coordinate CDP takes is a CSS pixel under every scale factor:
//! under 320x180 at 2, a mouse event at (125, 75) landed on the box at CSS
//! (100..150, 50..100) and the page reported `clientX` 125, and one at (500,
//! 290), which is where the box was on the screen, hit nothing. A
//! `mouseWheel` with `deltaY` 120 at 2 leaves `scrollY` at 120 CSS pixels,
//! 240 on the screen. So every point and every distance this program sends is
//! divided by the factor first, and [`Viewport::css_point`] is the one place
//! that happens.
//!
//! # HiDPI
//!
//! On a 2x display a page at one CSS pixel per terminal pixel is a page at
//! half the size anybody meant it. [`Scale`] is the terminal's pixels per CSS
//! pixel before any zoom — `--scale`, or guessed from the cell — and the
//! factor the engine is told is the scale times the zoom. The frames, the
//! stills, the clicks and the wheel all go through the same [`Viewport`]
//! whichever of the two a factor came from.
//!
//! # What is remembered
//!
//! A level is kept per host, as a browser keeps it, in `<profile>/zoom` —
//! one line per change, `host`, a tab, the percent — folded on load, the way
//! [`crate::history`] keeps the pages visited and for the same reasons: a
//! change costs one `write(2)`, a crash loses a line and never the file, and
//! the file is compacted, to a new file renamed over the old, when it has
//! grown to twice [`CAP`] lines or ends in a line cut short. It is a list of
//! hosts somebody visited, so it is 0600 like the history beside it, and a
//! temporary profile keeps it in memory and writes nothing.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::json::Json;
use crate::text;

/// Chrome's zoom presets, cut at 300%.
///
/// Chrome's steps, because they are what a hand pressing `ctrl+=` expects.
/// Cut at 300% because above it the engine's stills come back three pixels
/// short of the pane and nobody reads a terminal at 400%. Zooming out costs
/// nothing — the raster is the pane's size at every level below 100%, and a
/// frame at 25% compresses to under a kilobyte — so the bottom of the table
/// stays.
pub const LEVELS: [u16; 15] = [
    25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300,
];

/// A zoom level, in percent: always one of [`LEVELS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zoom(u16);

impl Default for Zoom {
    fn default() -> Zoom {
        Zoom::DEFAULT
    }
}

impl Zoom {
    /// 100%, which is no zoom at all.
    pub const DEFAULT: Zoom = Zoom(100);

    pub fn percent(self) -> u16 {
        self.0
    }

    /// The level at `percent`, if it is one of [`LEVELS`]; `None` otherwise,
    /// so that a hand-edited file cannot ask for 1000%.
    pub fn from_percent(percent: u16) -> Option<Zoom> {
        LEVELS.contains(&percent).then_some(Zoom(percent))
    }

    /// The next level up, or the top one, where it stays.
    pub fn step_in(self) -> Zoom {
        Zoom(
            LEVELS
                .iter()
                .copied()
                .find(|&level| level > self.0)
                .unwrap_or(LEVELS[LEVELS.len() - 1]),
        )
    }

    /// The next level down, or the bottom one, where it stays.
    pub fn step_out(self) -> Zoom {
        Zoom(
            LEVELS
                .iter()
                .rev()
                .copied()
                .find(|&level| level < self.0)
                .unwrap_or(LEVELS[0]),
        )
    }

    /// The level as a factor: 1.25 for 125%.
    pub fn factor(self) -> f64 {
        f64::from(self.0) / 100.0
    }

    /// `Some("125%")` while the level is not the default: the word the row
    /// shows, at its right-hand end, beside the title and never instead of
    /// it. Nothing at 100%, which is where a page is by default and so is not
    /// news — on a HiDPI terminal too, where 100% is the scale's baseline, as
    /// it is in Chrome.
    pub fn marker(self) -> Option<String> {
        (self != Zoom::DEFAULT).then(|| format!("{}%", self.0))
    }
}

/// The terminal's pixels per CSS pixel before any zoom: 1 on an ordinary
/// display, 2 on a HiDPI one. `--scale`, or guessed from the cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    /// Guessed from the cell, again on every resize.
    Auto,
    /// What `--scale` said.
    Fixed(f64),
}

impl Scale {
    /// What a cell says about the display under it: 2 from 28 pixels tall,
    /// else 1.
    ///
    /// A 1x display's cells are 14 to 22 pixels tall — Kitty at its default
    /// eleven points is 7x15, Ghostty at thirteen is 8x17, a tOS pane 8x16 —
    /// and a 2x display's are 24 to 40: the same fonts are 14x30 and 16x34
    /// there. A 1x terminal with a twenty-point font is 12x26 and stays at 1,
    /// which is what somebody who chose a big font gets from a page at one
    /// CSS pixel to the pixel: big text. Above 28 it is a 2x display, or a
    /// font so large that 2 is the right answer anyway. And since a font made
    /// bigger in the terminal is a `SIGWINCH`, the answer follows it.
    pub fn auto(cell: (u32, u32)) -> f64 {
        if cell.1 >= 28 {
            2.0
        } else {
            1.0
        }
    }

    /// The scale for a pane with this cell.
    pub fn resolve(self, cell: (u32, u32)) -> f64 {
        match self {
            Scale::Auto => Scale::auto(cell),
            Scale::Fixed(scale) => scale,
        }
    }

    /// `--scale`'s argument: `auto`, or a number from 0.5 to 4.
    ///
    /// The range is where a page is still a page: at 4 an 80-column pane is
    /// a phone's width of CSS, and below 0.5 the text is smaller than a pixel
    /// of the display can draw.
    pub fn parse(text: &str) -> Result<Scale, String> {
        if text == "auto" {
            return Ok(Scale::Auto);
        }
        match text.parse::<f64>() {
            Ok(scale) if (0.5..=4.0).contains(&scale) => Ok(Scale::Fixed(scale)),
            _ => Err(format!(
                "--scale is auto or a number from 0.5 to 4, not {text:?}"
            )),
        }
    }
}

/// The page as the engine is told it: the pane in terminal pixels, the CSS
/// viewport, and the factor between them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// The page's part of the pane, in terminal pixels.
    pub pane: (u32, u32),
    /// What the page is laid out in, in CSS pixels.
    pub css: (u32, u32),
    /// Terminal pixels per CSS pixel: the scale times the zoom.
    pub factor: f64,
}

impl Viewport {
    /// The pane divided by the factor, rounded, and never below one pixel
    /// either way.
    ///
    /// Rounded rather than floored, because the engine rounds too: 640x368
    /// at 1.5 is 427x245, and that is what came back 641x368 as a still.
    pub fn fit(pane: (u32, u32), factor: f64) -> Viewport {
        let factor = if factor.is_finite() && factor > 0.0 {
            factor
        } else {
            1.0
        };
        let css = |pixels: u32| ((f64::from(pixels) / factor).round() as u32).max(1);
        Viewport {
            pane,
            css: (css(pane.0), css(pane.1)),
            factor,
        }
    }

    /// The mapping from terminal pixels to CSS pixels, and the only one.
    ///
    /// A point in terminal pixels inside the page — what
    /// [`crate::input::page_point`] returns, zero-based from the page's top
    /// left, after the status row has been taken off — as the CSS point every
    /// CDP input event and hit test wants: divided by the factor, and nothing
    /// else, since both count from the same corner. Clicks, drags, the wheel
    /// and a hover all go through this; nothing that sends a coordinate to
    /// the engine divides by anything itself. At factor 1 it is the identity,
    /// which is what every coordinate was before there was a zoom.
    ///
    /// The terminal's side stays in terminal pixels: whether a report is on
    /// the status row, and how far apart two presses are for a double click,
    /// are questions about the screen, not the page.
    pub fn css_point(&self, x: i32, y: i32) -> (f64, f64) {
        (f64::from(x) / self.factor, f64::from(y) / self.factor)
    }

    /// The `Emulation.setDeviceMetricsOverride` parameters: the CSS size, and
    /// the factor as the device's pixel ratio.
    pub fn metrics_params(&self) -> Json {
        Json::object(vec![
            ("width", Json::number(self.css.0)),
            ("height", Json::number(self.css.1)),
            ("deviceScaleFactor", Json::number(self.factor)),
            ("mobile", Json::Bool(false)),
        ])
    }

    /// Wheel notches as a distance in CSS pixels: `pixels_per_notch` divided
    /// by the factor, so that a notch moves the page the same distance on the
    /// screen at every level.
    pub fn notch(&self, notches: (i32, i32), pixels_per_notch: f64) -> (f64, f64) {
        (
            f64::from(notches.0) * pixels_per_notch / self.factor,
            f64::from(notches.1) * pixels_per_notch / self.factor,
        )
    }
}

/// How far a picture may be from the pane and still be taken for a still at
/// a fractional level rather than a moving frame.
pub const SLACK: u32 = 4;

/// A decoded picture made exactly `pane`: cut, or with its last column and
/// row repeated, when it is within [`SLACK`] pixels of it on both axes and
/// not already it.
///
/// `None` when it is exact, and when it is further off than that — a frame
/// of a page zoomed in, which is smaller than the pane by design and is left
/// for the terminal to scale into the same cells. Repeating the edge rather
/// than padding with a colour, because the edge is what a pixel more of the
/// page would most likely have been.
pub fn fit(
    pixels: &[u8],
    width: u32,
    height: u32,
    channels: usize,
    pane: (u32, u32),
) -> Option<Vec<u8>> {
    if (width, height) == pane
        || width == 0
        || height == 0
        || width.abs_diff(pane.0) > SLACK
        || height.abs_diff(pane.1) > SLACK
        || pixels.len() < width as usize * height as usize * channels
    {
        return None;
    }
    let (from_w, to_w) = (width as usize, pane.0 as usize);
    let mut out = Vec::with_capacity(to_w * pane.1 as usize * channels);
    for row in 0..pane.1 as usize {
        let row = row.min(height as usize - 1);
        let start = row * from_w * channels;
        let line = &pixels[start..start + from_w * channels];
        let kept = to_w.min(from_w) * channels;
        out.extend_from_slice(&line[..kept]);
        let last = &line[(from_w - 1) * channels..];
        for _ in from_w..to_w {
            out.extend_from_slice(last);
        }
    }
    Some(out)
}

/// The host a level is kept under.
///
/// `http` and `https`: the host, lower-cased, an IPv6 address with its
/// brackets, and without the port or anybody's user name — the level is the
/// site's, as Chrome keeps it, and a port is the same site. `file://` is one
/// host, `file:`. Anything else — `about:blank`, `data:`, the engine's
/// `chrome-error:` — is `None`: a page whose level is its tab's alone and is
/// not remembered. So is a host that could not be one field of one line of
/// the file: nothing with a tab, a space or a control in it.
pub fn host_key(url: &str) -> Option<String> {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    if lower.starts_with("file://") {
        return Some("file:".to_string());
    }
    let rest = ["http://", "https://"]
        .into_iter()
        .find(|scheme| lower.starts_with(scheme))
        .map(|scheme| &url[scheme.len()..])?;
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if host_port.starts_with('[') {
        &host_port[..=host_port.find(']')?]
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    let plain = !host.is_empty()
        && host
            .chars()
            .all(|c| text::is_plain(c) && !c.is_whitespace());
    plain.then(|| host.to_ascii_lowercase())
}

/// The file in the profile the levels are kept in.
pub const FILE: &str = "zoom";

/// Hosts kept: more sites than anybody zooms, and a fold of this many lines
/// is instant.
pub const CAP: usize = 500;

/// Lines in the file at which it is compacted on load.
pub const COMPACT_AT: usize = 2 * CAP;

/// The levels remembered per host, in `<profile>/zoom` — or nowhere, for a
/// temporary profile.
#[derive(Debug)]
pub struct Zooms {
    levels: HashMap<String, Zoom>,
    /// The hosts in `levels`, oldest change first: what goes past [`CAP`].
    order: Vec<String>,
    /// The file, or `None` for a temporary profile.
    path: Option<PathBuf>,
}

impl Zooms {
    /// Levels that are never written anywhere: a temporary profile's.
    pub fn in_memory() -> Zooms {
        Zooms {
            levels: HashMap::new(),
            order: Vec::new(),
            path: None,
        }
    }

    /// The levels kept in the profile at `dir`, and where to add to them.
    ///
    /// As [`crate::history::History::load`]: a missing file is no levels, a
    /// line that does not parse is skipped, as is a last line with no newline
    /// after it, and nothing here fails. The last line for a host is its
    /// level, and a 100 is a level forgotten. Compacted when it has grown to
    /// [`COMPACT_AT`] lines or its last line was cut short.
    pub fn load(dir: &Path) -> Zooms {
        let path = dir.join(FILE);
        let file = std::fs::read(&path).unwrap_or_default();
        let file = String::from_utf8_lossy(&file);
        let mut zooms = Zooms::in_memory();
        let mut lines = 0;
        for line in file.split_inclusive('\n') {
            lines += 1;
            let Some(line) = line.strip_suffix('\n') else {
                continue;
            };
            if let Some((host, zoom)) = Zooms::parse_line(line) {
                zooms.remember(host, zoom);
            }
        }
        zooms.path = Some(path);
        if lines >= COMPACT_AT || !(file.is_empty() || file.ends_with('\n')) {
            let _ = zooms.compact();
        }
        zooms
    }

    /// One line of the file, `host`, a tab, the percent; or `None` for one
    /// that is not: the pure half of [`Zooms::load`].
    pub fn parse_line(line: &str) -> Option<(String, Zoom)> {
        let (host, percent) = line.split_once('\t')?;
        let plain = !host.is_empty()
            && host
                .chars()
                .all(|c| text::is_plain(c) && !c.is_whitespace());
        if !plain {
            return None;
        }
        let zoom = Zoom::from_percent(percent.parse().ok()?)?;
        Some((host.to_string(), zoom))
    }

    /// The level for a host: what was set for it, or 100%. `None` — a page
    /// with no host — is 100% too.
    pub fn get(&self, host: Option<&str>) -> Zoom {
        host.and_then(|host| self.levels.get(host))
            .copied()
            .unwrap_or_default()
    }

    /// Keep `zoom` for `host`, and append one line saying so, if there is a
    /// file. 100% forgets it.
    ///
    /// The error is for a test to see: the caller drops it, as it drops the
    /// history's, because a level that cannot be written is still the level
    /// the page is at.
    pub fn set(&mut self, host: &str, zoom: Zoom) -> Result<(), String> {
        self.remember(host.to_string(), zoom);
        let Some(path) = &self.path else {
            return Ok(());
        };
        OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut file| file.write_all(line(host, zoom).as_bytes()))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// The map's half of [`Zooms::set`] and of every line read.
    fn remember(&mut self, host: String, zoom: Zoom) {
        self.order.retain(|known| *known != host);
        if zoom == Zoom::DEFAULT {
            self.levels.remove(&host);
            return;
        }
        self.levels.insert(host.clone(), zoom);
        self.order.push(host);
        if self.order.len() > CAP {
            let gone = self.order.remove(0);
            self.levels.remove(&gone);
        }
    }

    /// Write the levels as a fresh file, oldest first so that appending goes
    /// on in order, to a file beside it that is then renamed over it.
    fn compact(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let fresh = path.with_extension("tmp");
        let file: String = self
            .order
            .iter()
            .map(|host| line(host, self.get(Some(host))))
            .collect();
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&fresh)
            .and_then(|mut out| out.write_all(file.as_bytes()))
            .and_then(|()| std::fs::rename(&fresh, path))
            .map_err(|e| format!("cannot compact {}: {e}", path.display()))
    }
}

/// One line of the file, newline included.
fn line(host: &str, zoom: Zoom) -> String {
    format!("{host}\t{}\n", zoom.percent())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A scratch directory of this test's own, gone again before and after.
    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blinkterm-zoom-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn at(percent: u16) -> Zoom {
        Zoom::from_percent(percent).expect("a level in the table")
    }

    #[test]
    fn zoom_steps_through_chromes_table_and_stops_at_its_ends() {
        let mut zoom = Zoom::DEFAULT;
        let mut went = Vec::new();
        for _ in 0..8 {
            zoom = zoom.step_in();
            went.push(zoom.percent());
        }
        assert_eq!(went, [110, 125, 150, 175, 200, 250, 300, 300]);
        let mut zoom = Zoom::DEFAULT;
        let mut went = Vec::new();
        for _ in 0..8 {
            zoom = zoom.step_out();
            went.push(zoom.percent());
        }
        assert_eq!(went, [90, 80, 75, 67, 50, 33, 25, 25]);
        assert_eq!(Zoom::default(), Zoom::DEFAULT);
        assert_eq!(at(125).factor(), 1.25);
        // Only what is in the table is a level.
        assert_eq!(Zoom::from_percent(120), None);
        assert_eq!(Zoom::from_percent(400), None);
        assert_eq!(Zoom::from_percent(0), None);
    }

    #[test]
    fn a_viewport_is_the_pane_divided_by_the_factor_and_never_empty() {
        assert_eq!(Viewport::fit((640, 368), 2.0).css, (320, 184));
        assert_eq!(Viewport::fit((640, 368), 1.5).css, (427, 245));
        assert_eq!(Viewport::fit((640, 368), 0.25).css, (2560, 1472));
        assert_eq!(Viewport::fit((640, 368), 1.0).css, (640, 368));
        assert_eq!(Viewport::fit((1, 1), 3.0).css, (1, 1));
        // A factor that is not one is not divided by.
        assert_eq!(Viewport::fit((640, 368), 0.0).css, (640, 368));
        assert_eq!(Viewport::fit((640, 368), f64::NAN).factor, 1.0);
    }

    #[test]
    fn a_terminal_pixel_becomes_a_css_point_by_dividing() {
        let pane = (640, 368);
        assert_eq!(Viewport::fit(pane, 2.0).css_point(250, 150), (125.0, 75.0));
        assert_eq!(Viewport::fit(pane, 1.0).css_point(250, 150), (250.0, 150.0));
        assert_eq!(Viewport::fit(pane, 0.5).css_point(250, 150), (500.0, 300.0));
        // The middle of cell (11, 4) under the status row, as
        // `a_click_lands_where_the_cell_was` has it, at 200%.
        assert_eq!(Viewport::fit(pane, 2.0).css_point(84, 40), (42.0, 20.0));
    }

    #[test]
    fn a_notch_is_the_same_distance_on_screen_at_every_level() {
        let pane = (640, 368);
        assert_eq!(Viewport::fit(pane, 2.0).notch((0, 1), 120.0), (0.0, 60.0));
        assert_eq!(
            Viewport::fit(pane, 0.5).notch((0, -1), 120.0),
            (0.0, -240.0)
        );
        assert_eq!(Viewport::fit(pane, 1.0).notch((1, 0), 120.0), (120.0, 0.0));
    }

    #[test]
    fn the_viewport_is_told_to_the_engine_as_the_css_size_and_the_factor() {
        assert_eq!(
            Viewport::fit((640, 368), 1.5).metrics_params().to_string(),
            r#"{"width":427,"height":245,"deviceScaleFactor":1.5,"mobile":false}"#
        );
        assert_eq!(
            Viewport::fit((640, 368), 1.0).metrics_params().to_string(),
            r#"{"width":640,"height":368,"deviceScaleFactor":1,"mobile":false}"#
        );
    }

    #[test]
    fn the_marker_says_the_level_only_when_it_is_not_the_default() {
        assert_eq!(Zoom::DEFAULT.marker(), None);
        assert_eq!(at(150).marker().as_deref(), Some("150%"));
        assert_eq!(at(25).marker().as_deref(), Some("25%"));
    }

    /// A picture whose every pixel says where it is: one channel of its
    /// column and one of its row.
    fn picture(width: u32, height: u32, channels: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let mut pixel = vec![x as u8, y as u8, 7, 255];
                pixel.truncate(channels);
                out.extend(pixel);
            }
        }
        out
    }

    fn pixel(pixels: &[u8], width: u32, channels: usize, x: u32, y: u32) -> &[u8] {
        let at = (y * width + x) as usize * channels;
        &pixels[at..at + channels]
    }

    #[test]
    fn a_still_a_pixel_off_is_cut_or_padded_to_the_pane_and_a_half_size_frame_is_left_alone() {
        let pane = (640, 368);
        // One column too many: the last goes.
        let wide = fit(&picture(641, 368, 4), 641, 368, 4, pane).expect("fitted");
        assert_eq!(wide.len(), 640 * 368 * 4);
        assert_eq!(pixel(&wide, 640, 4, 639, 0), [(639 % 256) as u8, 0, 7, 255]);
        // One row too few: the last is repeated.
        let short = fit(&picture(640, 367, 4), 640, 367, 4, pane).expect("fitted");
        assert_eq!(short.len(), 640 * 368 * 4);
        assert_eq!(pixel(&short, 640, 4, 5, 367), pixel(&short, 640, 4, 5, 366));
        assert_eq!(pixel(&short, 640, 4, 5, 366)[1], 366u32 as u8);
        // Both, and in three channels too.
        let both = fit(&picture(639, 369, 3), 639, 369, 3, pane).expect("fitted");
        assert_eq!(both.len(), 640 * 368 * 3);
        assert_eq!(pixel(&both, 640, 3, 639, 10), pixel(&both, 640, 3, 638, 10));
        // A moving frame at 200% is half the pane, and the terminal's to
        // scale; an exact still is already right.
        assert_eq!(fit(&picture(320, 184, 4), 320, 184, 4, pane), None);
        assert_eq!(fit(&picture(640, 368, 4), 640, 368, 4, pane), None);
        // A picture shorter than it says it is is not read past its end.
        assert_eq!(fit(&[0; 16], 641, 368, 4, pane), None);
    }

    #[test]
    fn the_host_key_is_the_host_lowercased_without_a_port_and_nothing_for_an_about_page() {
        let cases = [
            ("https://WWW.Example.com:8443/a?b", Some("www.example.com")),
            ("http://user@h/", Some("h")),
            ("http://user:pw@h:80", Some("h")),
            ("http://[::1]:3000/", Some("[::1]")),
            ("HTTPS://example.com#x", Some("example.com")),
            ("file:///etc/x", Some("file:")),
            ("about:blank", None),
            ("data:text/html,hi", None),
            ("chrome-error://chromewebdata/", None),
            ("https://", None),
            ("https://a\tb/", None),
            ("https://a b/", None),
            ("", None),
        ];
        for (url, key) in cases {
            assert_eq!(host_key(url).as_deref(), key, "{url:?}");
        }
    }

    #[test]
    fn a_zoom_is_remembered_per_host_and_one_hundred_forgets_it() {
        let mut zooms = Zooms::in_memory();
        assert_eq!(zooms.get(Some("example.com")), Zoom::DEFAULT);
        assert_eq!(zooms.get(None), Zoom::DEFAULT);
        zooms.set("example.com", at(150)).expect("kept");
        zooms.set("other.example", at(80)).expect("kept");
        assert_eq!(zooms.get(Some("example.com")), at(150));
        assert_eq!(zooms.get(Some("other.example")), at(80));
        assert_eq!(zooms.get(Some("third.example")), Zoom::DEFAULT);
        zooms.set("example.com", Zoom::DEFAULT).expect("kept");
        assert_eq!(zooms.get(Some("example.com")), Zoom::DEFAULT);
        assert_eq!(zooms.order, ["other.example"]);
        // And the oldest goes past the cap.
        for n in 0..CAP {
            zooms.set(&format!("{n}.example"), at(110)).expect("kept");
        }
        assert_eq!(zooms.get(Some("other.example")), Zoom::DEFAULT);
        assert_eq!(zooms.levels.len(), CAP);
    }

    #[test]
    fn the_zoom_file_is_appended_folded_compacted_and_readable_by_its_owner_alone() {
        let dir = scratch("file");
        let mut zooms = Zooms::load(&dir);
        assert_eq!(zooms.get(Some("example.com")), Zoom::DEFAULT, "no file");
        zooms.set("example.com", at(150)).expect("written");
        zooms.set("[::1]", at(80)).expect("written");
        zooms.set("example.com", Zoom::DEFAULT).expect("written");
        let file = std::fs::read_to_string(dir.join(FILE)).expect("a file");
        assert_eq!(file, "example.com\t150\n[::1]\t80\nexample.com\t100\n");
        let mode = std::fs::metadata(dir.join(FILE))
            .expect("the file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "a list of hosts visited nobody else may read");

        let again = Zooms::load(&dir);
        assert_eq!(again.get(Some("example.com")), Zoom::DEFAULT);
        assert_eq!(again.get(Some("[::1]")), at(80));

        // A line cut short, and a line that is not one, are not read; the
        // file is written again so that the next line starts a line.
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join(FILE))
            .expect("the file");
        file.write_all(b"junk\nexample.org\t2").expect("written");
        drop(file);
        let mut again = Zooms::load(&dir);
        assert_eq!(again.get(Some("example.org")), Zoom::DEFAULT);
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE)).expect("a file"),
            "[::1]\t80\n",
            "compacted to the levels that are not 100%"
        );
        again.set("example.org", at(200)).expect("written");
        assert_eq!(Zooms::load(&dir).get(Some("example.org")), at(200));

        // A long file is compacted on load.
        let long: String = (0..COMPACT_AT)
            .map(|n| {
                line(
                    &format!("{}.example", n % 3),
                    at(if n % 2 == 0 { 125 } else { 175 }),
                )
            })
            .collect();
        std::fs::write(dir.join(FILE), long).expect("written");
        let loaded = Zooms::load(&dir);
        let file = std::fs::read_to_string(dir.join(FILE)).expect("a file");
        assert_eq!(file.lines().count(), 3, "{file:?}");
        assert!(!dir.join("zoom.tmp").exists());
        assert_eq!(Zooms::load(&dir).levels, loaded.levels);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_in_memory_zoom_writes_nothing() {
        let mut zooms = Zooms::in_memory();
        zooms.set("example.com", at(150)).expect("kept");
        assert!(zooms.path.is_none());
        assert_eq!(zooms.get(Some("example.com")), at(150));
    }

    #[test]
    fn a_line_of_the_zoom_file_is_a_host_and_a_level_in_the_table() {
        assert_eq!(
            Zooms::parse_line("example.com\t150"),
            Some(("example.com".to_string(), at(150)))
        );
        for broken in [
            "",
            "example.com",
            "example.com\t",
            "example.com\t120",
            "example.com\t1000",
            "\t150",
            "a b\t150",
            "a\u{202e}b\t150",
            "example.com\t150\textra",
        ] {
            assert_eq!(Zooms::parse_line(broken), None, "{broken:?}");
        }
    }

    #[test]
    fn a_tall_cell_is_a_hidpi_terminal() {
        assert_eq!(Scale::auto((8, 16)), 1.0);
        assert_eq!(Scale::auto((14, 28)), 2.0);
        assert_eq!(Scale::auto((10, 22)), 1.0);
        assert_eq!(Scale::auto((12, 26)), 1.0, "a big font on a 1x display");
        assert_eq!(Scale::auto((16, 36)), 2.0);
        assert_eq!(Scale::Auto.resolve((16, 36)), 2.0);
        assert_eq!(Scale::Fixed(1.5).resolve((16, 36)), 1.5);
    }

    #[test]
    fn a_scale_is_a_number_in_range_or_auto() {
        assert_eq!(Scale::parse("auto"), Ok(Scale::Auto));
        for (text, scale) in [("2", 2.0), ("1.5", 1.5), ("0.5", 0.5), ("4", 4.0)] {
            assert_eq!(Scale::parse(text), Ok(Scale::Fixed(scale)), "{text}");
        }
        for text in ["0", "5", "x", "", "NaN", "-1", "0.49"] {
            let why = Scale::parse(text).expect_err("refused");
            assert!(why.contains("--scale"), "{text:?}: {why}");
        }
    }
}
