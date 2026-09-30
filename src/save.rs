//! Saving the page in front as a PDF (`alt+s`), or the whole of it as a
//! picture (`alt+shift+s`), into the downloads directory.
//!
//! The engine does both, headless: `Page.printToPDF` for the one and
//! `Page.captureScreenshot` with `captureBeyondViewport` for the other. What
//! is this program's is the naming, the bound on the size, and not waiting.
//!
//! # The name
//!
//! The page's title, sanitized and with its path separators made `_`, then
//! `.pdf` or `.png`; with no title, the host; with no host, `page`. It goes
//! where a download goes and by the same rules — [`download::safe_name`] for
//! what a file system takes, [`download::reserve`] for `(1)` — and is said on
//! the row as a download is, `saved ~/Downloads/<title>.pdf`, for eight
//! seconds. See [`file_name`] and [`crate::download`].
//!
//! # How big a picture of a whole page is
//!
//! Measured against `chrome-headless-shell` 153 at 1280 wide, a clip of the
//! whole of `Page.getLayoutMetrics`'s `cssContentSize` at scale 1:
//!
//! | page | PNG | message on the pipe | time |
//! | --- | --- | --- | --- |
//! | text, 1280x4000 | 1.53 MB | 2.04 MB | 0.25 s |
//! | text, 1280x16000 | 6.14 MB | 8.19 MB | 0.80 s |
//! | text, 1280x40000 | 15.5 MB | 20.6 MB | 2.0 s |
//! | noise, 1280x4000 | 15.4 MB (3.01 B/px) | 20.5 MB | 0.50 s |
//! | photo-like, 1280x4000 | 10.8 MB (2.10 B/px) | 14.4 MB | 0.42 s |
//! | blank, 1280x400000 | 2.3 MB | – | 3.4 s |
//! | blank, 1280x1000000 | – | – | `Unable to capture screenshot` |
//!
//! There is no 16384-pixel texture cap to slice around: the engine runs with
//! `--disable-gpu`, rasterizes in software, and draws a page 400000 pixels
//! tall. The bound that matters is this program's. The PNG comes back as
//! base64 in one message, and a message is bounded by
//! [`crate::cdp::MAX_MESSAGE`], past which the pipe — every tab — ends. The
//! PNG is always RGB, and the worst measured is noise at 3.01 bytes a pixel,
//! so [`PIXELS`] is sixteen million device pixels: 61.2 MiB on the pipe at
//! worst, under the sixty-four the reader will assemble. Measured on a noise
//! canvas of exactly that, 63.6 MB arrived 1.0 s after it was asked for, was
//! parsed in 22 ms, and took this process to 129 MB resident at the peak.
//!
//! A page taller than that is cut, not sliced: the picture is its top
//! [`PIXELS`] worth, and the row says so — `saved …png, the top 12500 of
//! 40000 px`. Slicing would mean joining PNGs — decoding every slice and
//! encoding the whole again, with the plain encoder below that exists for the
//! key — or several files, which is not a picture of the page. The budget is in device pixels, because what comes back is the CSS
//! clip times the device's pixel ratio: at 200% a page 640 wide is cut at
//! 6250 CSS rows. The picture is taken at the tab's own level, so there is
//! nothing to undo afterwards.
//!
//! The width is the content's, which is the viewport less a scrollbar where
//! there is one: 1265 for a 1280 pane with the engine's scrollbars.
//!
//! # Under a chroma key
//!
//! Where `--alpha` has the page painted on magenta for the frames' sake
//! ([`crate::chroma`]), the engine's picture is of the page on magenta, so it
//! is decoded, keyed as the screen is, and written back out with
//! [`crate::png::encode_rgba`] ([`key_png`]) — transparent where the page is.
//! That encoder is the least compression worth having, so a page of text is a
//! few times the engine's own PNG; measured, a 640x360 page of a line of text
//! was 4.2 kB from the engine and 8.3 kB keyed.
//!
//! # A PDF comes as a stream
//!
//! `Page.printToPDF` with `transferMode: "ReturnAsStream"` answers with a
//! handle, and `IO.read` hands the file over a megabyte at a time
//! ([`STREAM_CHUNK`]), each chunk written to the reserved file as it comes.
//! A PDF's size has nothing to do with the pane's — 8000 paragraphs were
//! 2.26 MB in 0.71 s — so it is bounded by the chunk and not by a budget. No
//! frame is sent and the viewport is not touched while it prints. The paper
//! is A4 or Letter ([`Paper`]), backgrounds printed, margins the engine's.
//!
//! # The loop never waits
//!
//! Every command here goes out with [`Client::send`] and is collected with
//! [`Client::take_reply`] on whichever pass it has come, the way the still
//! and a navigation are, and a phase with no answer in [`TIMEOUT`] is a
//! failure that says so. A picture of a long page is seconds of engine, and
//! a page that prints with a `beforeprint` handler that opens an `alert()`
//! is held until the question is answered; neither is a reason for keys to
//! stop working.
//!
//! # The frame the capture sends
//!
//! The engine lays the page out at its whole height for the capture and
//! restores the viewport itself afterwards — `innerWidth`, `innerHeight` and
//! `devicePixelRatio` are as they were, measured, so nothing is emulated
//! again. But a screencast that is running sends a frame of that layout,
//! 1280x8000 for a page 8000 tall, while it happens. Painted, it would be
//! the page squashed into the pane for a moment, so a frame from the tab
//! being captured is acknowledged and dropped ([`Job::capturing`]).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::base64;
use crate::cdp::{Client, Pending};
use crate::download;
use crate::json::Json;
use crate::text;
use crate::zoom;

/// The most device pixels a picture of a page is: see the module's table.
/// 16 million at 3.01 bytes a pixel, as base64, is 61.2 MiB on the pipe.
pub const PIXELS: u64 = 16_000_000;

/// How long one step of a save may take before it is a failure: a picture
/// of a page 40000 pixels tall is two seconds, and this is for an engine
/// that has stopped answering.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// How much of a PDF one `IO.read` asks for.
pub const STREAM_CHUNK: u32 = 1 << 20;

/// The countries that print on Letter; everywhere else is A4. The US and
/// Canada, Mexico, and the Americas that follow them, and the Philippines.
const LETTER_REGIONS: [&str; 14] = [
    "BZ", "CA", "CL", "CO", "CR", "GT", "MX", "NI", "PA", "PH", "PR", "SV", "US", "VE",
];

/// What is being saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `alt+s`: the page printed.
    Pdf,
    /// `alt+shift+s`: the whole page as a PNG.
    Screenshot,
}

impl Kind {
    /// `.pdf` or `.png`.
    pub fn extension(self) -> &'static str {
        match self {
            Kind::Pdf => ".pdf",
            Kind::Screenshot => ".png",
        }
    }
}

/// The paper a PDF is printed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Paper {
    /// 8.27 by 11.69 inches, which the engine makes 595.9 by 841.9 points.
    #[default]
    A4,
    /// 8.5 by 11 inches, 612 by 792 points.
    Letter,
}

impl Paper {
    /// `a4` or `letter`, in any case.
    pub fn parse(text: &str) -> Result<Paper, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "a4" => Ok(Paper::A4),
            "letter" => Ok(Paper::Letter),
            _ => Err(format!("--pdf-paper is a4 or letter, not {text:?}")),
        }
    }

    /// The paper of the region in a POSIX locale: `en_US.UTF-8` is Letter,
    /// `en_GB.UTF-8` is A4. The region is what comes between the `_` (or a
    /// `-`) and the `.` or `@`; no region — `C`, `POSIX`, nothing — is A4,
    /// which is most of the world's.
    pub fn from_locale(locale: &str) -> Paper {
        let name = locale.split(['.', '@']).next().unwrap_or("");
        let region = name
            .split_once(['_', '-'])
            .map(|(_, region)| region.to_ascii_uppercase());
        match region {
            Some(region) if LETTER_REGIONS.contains(&region.as_str()) => Paper::Letter,
            _ => Paper::A4,
        }
    }

    /// The locale that says which paper: `LC_ALL`, then `LC_PAPER`, which is
    /// the category for exactly this, then `LANG`.
    pub fn locale() -> String {
        for name in ["LC_ALL", "LC_PAPER", "LANG"] {
            if let Some(value) = std::env::var_os(name) {
                let value = value.to_string_lossy().into_owned();
                if !value.is_empty() {
                    return value;
                }
            }
        }
        String::new()
    }

    /// Width and height in inches, which is what `Page.printToPDF` takes.
    pub fn inches(self) -> (f64, f64) {
        match self {
            Paper::A4 => (8.27, 11.69),
            Paper::Letter => (8.5, 11.0),
        }
    }

    /// `a4` or `letter`, as [`Paper::parse`] reads it.
    pub fn name(self) -> &'static str {
        match self {
            Paper::A4 => "a4",
            Paper::Letter => "letter",
        }
    }
}

/// The file's name: the title, then the extension.
///
/// The title is the page's, so it is [`text::sanitize`]d, its runs of
/// whitespace made one space, `/` and `\` made `_` — so that `Sales / Q3` is
/// not cut to `Q3` as a path's last component would be — and trimmed of
/// whitespace and dots. A title with nothing left is the url's host, and a
/// url with no host (`about:blank`, `data:`, `file:`) is `page`. Then
/// [`download::safe_name`], which bounds the length and keeps the extension.
pub fn file_name(title: &str, url: &str, kind: Kind) -> String {
    let clean = text::sanitize(title);
    let stem = clean
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['/', '\\'], "_");
    let stem = stem.trim_matches(|c: char| c.is_whitespace() || c == '.');
    let stem = if stem.is_empty() {
        zoom::host_key(url)
            .filter(|host| host != "file:")
            .unwrap_or_else(|| "page".to_string())
    } else {
        stem.to_string()
    };
    download::safe_name(&format!("{stem}{}", kind.extension()))
}

/// The `Page.printToPDF` parameters: the paper, the backgrounds, and the
/// answer as a stream rather than one message.
pub fn pdf_params(paper: Paper) -> Json {
    let (width, height) = paper.inches();
    Json::object(vec![
        ("printBackground", Json::Bool(true)),
        ("paperWidth", Json::number(width)),
        ("paperHeight", Json::number(height)),
        ("transferMode", Json::string("ReturnAsStream")),
    ])
}

/// The page's size in CSS pixels, from a `Page.getLayoutMetrics` reply's
/// `cssContentSize`: rounded up, and never below one either way.
pub fn content_size(metrics: &Json) -> Option<(u32, u32)> {
    let size = metrics.get("cssContentSize")?;
    let side = |key: &str| {
        size.get(key)
            .and_then(Json::as_f64)
            .filter(|n| n.is_finite())
            .map(|n| n.ceil().clamp(1.0, f64::from(u32::MAX)) as u32)
    };
    Some((side("width")?, side("height")?))
}

/// How many CSS rows of a page `width` by `height` fit [`PIXELS`] at
/// `factor` device pixels to the CSS pixel, and whether that cuts it.
///
/// Never none of a page that has rows: a page so wide that one row is past
/// the budget still gets its top row.
pub fn rows_within(width: u32, height: u32, factor: f64) -> (u32, bool) {
    let factor = if factor.is_finite() && factor > 0.0 {
        factor
    } else {
        1.0
    };
    let per_row = f64::from(width.max(1)) * factor * factor;
    let fits = (PIXELS as f64 / per_row)
        .floor()
        .clamp(1.0, f64::from(u32::MAX)) as u32;
    if height <= fits {
        (height, false)
    } else {
        (fits, true)
    }
}

/// The `Page.captureScreenshot` parameters for the top `rows` of a page
/// `width` wide: PNG, beyond the viewport, at the device's own ratio.
pub fn capture_params(width: u32, rows: u32) -> Json {
    Json::object(vec![
        ("format", Json::string("png")),
        ("captureBeyondViewport", Json::Bool(true)),
        (
            "clip",
            Json::object(vec![
                ("x", Json::number(0)),
                ("y", Json::number(0)),
                ("width", Json::number(width)),
                ("height", Json::number(rows)),
                ("scale", Json::number(1)),
            ]),
        ),
    ])
}

/// The `IO.read` parameters for the next [`STREAM_CHUNK`] of `handle`.
pub fn read_params(handle: &str) -> Json {
    Json::object(vec![
        ("handle", Json::string(handle)),
        ("size", Json::number(STREAM_CHUNK)),
    ])
}

/// The `IO.close` parameters for `handle`.
pub fn close_params(handle: &str) -> Json {
    Json::object(vec![("handle", Json::string(handle))])
}

/// One save, from the key to the file.
#[derive(Debug)]
pub struct Job {
    kind: Kind,
    target: String,
    name: String,
    factor: f64,
    phase: Phase,
    /// When the command the phase waits on went out.
    sent: Instant,
    /// The file while it is being written, which goes if the job does
    /// before it is whole — a failure, a tab closed, an engine that died.
    partial: Option<PathBuf>,
    /// The page is painted on a chroma key ([`crate::chroma`]), so the
    /// picture is keyed before it is written: the file transparent where the
    /// page is, not magenta.
    keyed: bool,
}

/// What a [`Job`] is waiting on.
#[derive(Debug)]
enum Phase {
    /// `Page.getLayoutMetrics`, for the page's size.
    Measuring(Pending),
    /// `Page.captureScreenshot` of the top `rows` of a page `height` tall.
    Capturing {
        pending: Pending,
        height: u32,
        rows: u32,
    },
    /// `Page.printToPDF`, for the stream's handle.
    Printing(Pending),
    /// The next `IO.read` of `handle`, into `file`.
    Reading {
        pending: Pending,
        handle: String,
        file: File,
    },
}

/// Where a [`Job`] has got to.
#[derive(Debug)]
pub enum Progress {
    /// Nothing yet: ask again next pass.
    Waiting,
    /// Over: the file, or why not.
    Done(Result<Saved, String>),
}

/// A file saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// Where it went, `(1)` and all.
    pub path: PathBuf,
    /// `Some((rows, height))` when the picture is the top `rows` of a page
    /// `height` tall, in CSS pixels.
    pub cut: Option<(u32, u32)>,
}

impl Job {
    /// Send the first command: `Page.getLayoutMetrics` for a picture,
    /// `Page.printToPDF` for a PDF. `factor` is the tab's device pixels per
    /// CSS pixel, the scale times the zoom.
    #[allow(clippy::too_many_arguments)]
    pub fn begin(
        client: &mut Client,
        kind: Kind,
        target: &str,
        title: &str,
        url: &str,
        paper: Paper,
        factor: f64,
        now: Instant,
    ) -> Result<Job, String> {
        let phase = match kind {
            Kind::Pdf => Phase::Printing(client.send("Page.printToPDF", pdf_params(paper))?),
            Kind::Screenshot => {
                Phase::Measuring(client.send("Page.getLayoutMetrics", Json::empty())?)
            }
        };
        Ok(Job {
            kind,
            target: target.to_string(),
            name: file_name(title, url, kind),
            factor,
            phase,
            sent: now,
            partial: None,
            keyed: false,
        })
    }

    /// Key the picture before it is written, for a page painted on the key.
    pub fn keyed(mut self, keyed: bool) -> Job {
        self.keyed = keyed;
        self
    }

    /// A PDF or a picture.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The target of the tab being saved, which is not always the one in
    /// front by the time it is done.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The name it will be saved under, before any `(1)`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the picture is being taken now, when a screencast frame from
    /// this job's tab is the page laid out at its whole height and not a
    /// frame to paint.
    pub fn capturing(&self) -> bool {
        matches!(self.phase, Phase::Capturing { .. })
    }

    /// Take whatever has come back, send what comes next, and say whether it
    /// is over. `client` is the connection of the job's tab, and `dir` the
    /// downloads directory, which the caller has made.
    pub fn poll(&mut self, client: &mut Client, dir: &Path, now: Instant) -> Progress {
        match self.step(client, dir, now) {
            Ok(Some(saved)) => {
                self.partial = None;
                Progress::Done(Ok(saved))
            }
            Ok(None) if now.duration_since(self.sent) >= TIMEOUT => {
                self.close(client);
                Progress::Done(Err(format!("no answer in {} seconds", TIMEOUT.as_secs())))
            }
            Ok(None) => Progress::Waiting,
            Err(why) => {
                self.close(client);
                Progress::Done(Err(why))
            }
        }
    }

    /// One step: `Ok(None)` for nothing yet or a phase begun, `Ok(Some)` for
    /// the file whole.
    fn step(
        &mut self,
        client: &mut Client,
        dir: &Path,
        now: Instant,
    ) -> Result<Option<Saved>, String> {
        match &mut self.phase {
            Phase::Measuring(pending) => {
                let Some(reply) = client.take_reply(pending) else {
                    return Ok(None);
                };
                let (width, height) =
                    content_size(&reply?).ok_or("the engine did not say how big the page is")?;
                let (rows, _) = rows_within(width, height, self.factor);
                let pending = client.send("Page.captureScreenshot", capture_params(width, rows))?;
                self.phase = Phase::Capturing {
                    pending,
                    height,
                    rows,
                };
                self.sent = now;
                Ok(None)
            }
            Phase::Capturing {
                pending,
                height,
                rows,
            } => {
                let Some(reply) = client.take_reply(pending) else {
                    return Ok(None);
                };
                let (height, rows) = (*height, *rows);
                let mut bytes = decoded(&reply?)?;
                if self.keyed {
                    bytes = key_png(&bytes)?;
                }
                let path = self.write_whole(dir, &bytes)?;
                Ok(Some(Saved {
                    path,
                    cut: (rows < height).then_some((rows, height)),
                }))
            }
            Phase::Printing(pending) => {
                let Some(reply) = client.take_reply(pending) else {
                    return Ok(None);
                };
                let reply = reply?;
                // The reply has `data` too, empty, beside the stream — 153
                // sends both — so the stream is looked for first, and the
                // data taken only from an engine that ignored the mode.
                let Some(handle) = reply.get("stream").and_then(Json::as_str) else {
                    let bytes = decoded(&reply)?;
                    if bytes.is_empty() {
                        return Err("the engine sent no PDF".to_string());
                    }
                    let path = self.write_whole(dir, &bytes)?;
                    return Ok(Some(Saved { path, cut: None }));
                };
                let handle = handle.to_string();
                let opened = self.reserve(dir).and_then(|path| {
                    OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(&path)
                        .map_err(|e| format!("cannot write it: {e}"))
                });
                let reading = opened
                    .and_then(|file| Ok((file, client.send("IO.read", read_params(&handle))?)));
                match reading {
                    Ok((file, pending)) => {
                        self.phase = Phase::Reading {
                            pending,
                            handle,
                            file,
                        };
                        self.sent = now;
                        Ok(None)
                    }
                    // The stream is the engine's until it is closed.
                    Err(why) => {
                        let _ = client.notify("IO.close", close_params(&handle));
                        Err(why)
                    }
                }
            }
            Phase::Reading {
                pending,
                handle,
                file,
            } => {
                let Some(reply) = client.take_reply(pending) else {
                    return Ok(None);
                };
                let reply = reply?;
                let data = reply.get("data").and_then(Json::as_str).unwrap_or("");
                let bytes = if reply.get("base64Encoded").and_then(Json::as_bool) == Some(true) {
                    base64::decode(data.as_bytes())
                        .map_err(|_| "the engine's PDF was not base64".to_string())?
                } else {
                    data.as_bytes().to_vec()
                };
                file.write_all(&bytes)
                    .map_err(|e| format!("cannot write it: {e}"))?;
                if reply.get("eof").and_then(Json::as_bool) == Some(true) {
                    let _ = client.notify("IO.close", close_params(handle));
                    let path = self.partial.clone().ok_or("the file went missing")?;
                    return Ok(Some(Saved { path, cut: None }));
                }
                *pending = client.send("IO.read", read_params(handle))?;
                self.sent = now;
                Ok(None)
            }
        }
    }

    /// A free name in `dir` for this job, kept as the partial file until
    /// the job is done.
    fn reserve(&mut self, dir: &Path) -> Result<PathBuf, String> {
        let path =
            download::reserve(dir, &self.name).map_err(|e| format!("cannot name it: {e}"))?;
        self.partial = Some(path.clone());
        Ok(path)
    }

    /// The whole file in one write, to a name reserved for it.
    fn write_whole(&mut self, dir: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
        let path = self.reserve(dir)?;
        std::fs::write(&path, bytes).map_err(|e| format!("cannot write it: {e}"))?;
        Ok(path)
    }

    /// Tell the engine a stream that is being given up on can go.
    fn close(&self, client: &mut Client) {
        if let Phase::Reading { handle, .. } = &self.phase {
            let _ = client.notify("IO.close", close_params(handle));
        }
    }
}

impl Drop for Job {
    /// A file that was not finished is not left looking like one that was.
    fn drop(&mut self) {
        if let Some(path) = self.partial.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The engine's PNG of a page painted on the key, keyed and written back
/// out: transparent where the page is, as it looks on the screen, but at
/// full opacity whatever `--alpha`'s number is.
pub fn key_png(png: &[u8]) -> Result<Vec<u8>, String> {
    let mut image = crate::png::decode(png, PIXELS as usize * 4)
        .map_err(|e| format!("cannot key the picture: {e}"))?;
    crate::chroma::key(&mut image.rgba, image.width, image.height);
    Ok(crate::png::encode_rgba(
        image.width,
        image.height,
        &image.rgba,
    ))
}

/// The bytes of a reply's base64 `data`.
fn decoded(reply: &Json) -> Result<Vec<u8>, String> {
    let data = reply
        .get("data")
        .and_then(Json::as_str)
        .ok_or("the engine sent nothing")?;
    base64::decode(data.as_bytes()).map_err(|_| "the engine's answer was not base64".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_becomes_a_file_name_with_the_kinds_extension() {
        let shop = "https://shop.example/x";
        assert_eq!(
            file_name("Receipt #123", shop, Kind::Pdf),
            "Receipt #123.pdf"
        );
        assert_eq!(file_name("", shop, Kind::Screenshot), "shop.example.png");
        assert_eq!(file_name("", "about:blank", Kind::Pdf), "page.pdf");
        assert_eq!(
            file_name("", "file:///home/u/x.html", Kind::Pdf),
            "page.pdf"
        );
        assert_eq!(
            file_name("Sales / Q3\\report", shop, Kind::Pdf),
            "Sales _ Q3_report.pdf"
        );
        assert_eq!(
            file_name("\x1b]0;x\x07 Title\nline", shop, Kind::Pdf),
            "]0;x Title line.pdf"
        );
        assert_eq!(
            file_name("  ...  ", "https://a.example/", Kind::Pdf),
            "a.example.pdf"
        );
        assert_eq!(file_name(".hidden", shop, Kind::Pdf), "hidden.pdf");
        assert_eq!(file_name("日本語 報告", shop, Kind::Pdf), "日本語 報告.pdf");
        let long = file_name(&"y".repeat(400), shop, Kind::Pdf);
        assert!(long.len() <= download::NAME_BYTES, "{}", long.len());
        assert!(long.ends_with(".pdf"), "{long}");
        for title in ["\u{202e}evil", "a\u{7f}b", "tab\there", "\u{85}x"] {
            let name = file_name(title, shop, Kind::Screenshot);
            assert!(name.chars().all(text::is_plain), "{name:?}");
            assert!(!name.contains('/'), "{name:?}");
        }
    }

    #[test]
    fn the_paper_is_letter_where_the_locale_says_and_a4_everywhere_else() {
        for letter in ["en_US.UTF-8", "en_CA", "es_MX.UTF-8", "en-US", "en_ph@x"] {
            assert_eq!(Paper::from_locale(letter), Paper::Letter, "{letter}");
        }
        for a4 in [
            "en_GB.UTF-8",
            "ja_JP.UTF-8",
            "pt_BR",
            "C",
            "POSIX",
            "",
            "C.UTF-8",
        ] {
            assert_eq!(Paper::from_locale(a4), Paper::A4, "{a4}");
        }
    }

    #[test]
    fn paper_parses_its_two_names_and_refuses_the_rest() {
        assert_eq!(Paper::parse("a4"), Ok(Paper::A4));
        assert_eq!(Paper::parse("Letter"), Ok(Paper::Letter));
        assert_eq!(Paper::parse(" LETTER "), Ok(Paper::Letter));
        for paper in [Paper::A4, Paper::Letter] {
            assert_eq!(Paper::parse(paper.name()), Ok(paper));
        }
        assert_eq!(
            Paper::parse("a5"),
            Err("--pdf-paper is a4 or letter, not \"a5\"".to_string())
        );
        assert!(Paper::parse("").is_err());
    }

    #[test]
    fn a_page_taller_than_the_budget_is_cut_at_it() {
        assert_eq!(rows_within(1280, 4000, 1.0), (4000, false));
        assert_eq!(rows_within(1280, 12500, 1.0), (12500, false));
        assert_eq!(rows_within(1280, 40000, 1.0), (12500, true));
        assert_eq!(rows_within(640, 40000, 2.0), (6250, true));
        assert_eq!(rows_within(640, 28000, 1.0), (25000, true));
        assert_eq!(rows_within(0, 0, 1.0), (0, false));
        assert_eq!(rows_within(0, 10, 0.0), (10, false));
        assert_eq!(rows_within(20_000_000, 10, 1.0), (1, true), "never no rows");
        assert_eq!(rows_within(1280, 4000, f64::NAN), (4000, false));
        // The worst PNG measured, as base64, fits what the reader assembles.
        assert!(PIXELS as f64 * 3.02 * 4.0 / 3.0 < crate::cdp::MAX_MESSAGE as f64);
    }

    #[test]
    fn the_pdf_asks_for_the_background_the_paper_and_a_stream() {
        let params = pdf_params(Paper::Letter);
        assert_eq!(params.get("printBackground"), Some(&Json::Bool(true)));
        assert_eq!(params.get("paperWidth").and_then(Json::as_f64), Some(8.5));
        assert_eq!(params.get("paperHeight").and_then(Json::as_f64), Some(11.0));
        assert_eq!(
            params.get("transferMode").and_then(Json::as_str),
            Some("ReturnAsStream")
        );
        let params = pdf_params(Paper::A4);
        assert_eq!(params.get("paperWidth").and_then(Json::as_f64), Some(8.27));
        assert_eq!(
            read_params("h").get("size").and_then(Json::as_f64),
            Some(1048576.0)
        );
        assert_eq!(
            close_params("h").get("handle").and_then(Json::as_str),
            Some("h")
        );
    }

    #[test]
    fn content_size_reads_the_css_content_size_rounded_up() {
        let metrics = Json::parse(
            r#"{"cssContentSize":{"x":0,"y":0,"width":1265,"height":3999.4},
                "contentSize":{"x":0,"y":0,"width":2530,"height":7999}}"#,
        )
        .expect("json");
        assert_eq!(content_size(&metrics), Some((1265, 4000)));
        let empty = Json::parse(r#"{"cssContentSize":{"width":0,"height":0}}"#).expect("json");
        assert_eq!(content_size(&empty), Some((1, 1)));
        assert_eq!(content_size(&Json::empty()), None);
        let capture = capture_params(1265, 4000);
        assert_eq!(
            capture.path(&["clip", "height"]).and_then(Json::as_f64),
            Some(4000.0)
        );
        assert_eq!(
            capture.get("captureBeyondViewport"),
            Some(&Json::Bool(true))
        );
    }
}
