//! A chroma key: what `--alpha` makes of a JPEG frame, so that a page is
//! see-through while it moves as well as at rest.
//!
//! # Why a key
//!
//! A JPEG has no alpha. Chromium encodes the screencast from a premultiplied
//! bitmap and drops it, so what the page leaves transparent comes out 0, 0, 0
//! (measured, [`crate::motion`]): with the page's own backgrounds forced
//! transparent a light page moving is dark text on black, and a caret that
//! blinks is a frame, so a page with a text box in it flickered black and
//! clear for as long as it was open — no scrolling needed. A PNG cast keeps the
//! alpha and costs the frame rate (55.2 fps against 59.7 on an M3, 33.8
//! against 57.8 on two cores, and 8.3 ms a decode against 6.1).
//!
//! So on the route where this program decodes the frames itself
//! ([`crate::route::Payload::Raw`]) the engine paints a **key colour** where
//! the page would be transparent — `Emulation.setDefaultBackgroundColorOverride`
//! and the forced `html, body` background both say [`KEY`] rather than
//! nothing — and the decoder turns that colour back into transparency, pixel
//! by pixel, as it writes the RGBA it writes anyway
//! ([`crate::jpeg::decode_rgba_with`]). The still at rest is keyed the same
//! way, so a page looks the same moving and stopped. Over ssh and in tmux the
//! engine's PNG goes to the terminal as it came, so there is no key: the
//! backgrounds are made transparent as they always were
//! ([`crate::appearance`]).
//!
//! # Which colour
//!
//! Magenta, `#ff00ff`. Every key costs the page's own content of that hue,
//! and the three measured against real content (below) cost different
//! things: green took Google's green letter, success buttons and the greens
//! of a photograph; blue took every link, the focus ring and Google's blue
//! letters; magenta took a box of vivid purple (`#d500f9`) and nothing on
//! Google's front page. Magenta is also far from both skin and sky, and it
//! survives 4:2:0 chroma subsampling as well as any saturated colour does: a
//! flat field of it comes back within a count or two, and only the pixels
//! within two of an edge see its chroma averaged with the content's.
//!
//! # A soft key
//!
//! How much of the key a pixel carries is its **excess**: `min(r, b) - g`,
//! 255 for the key and 0 or less for anything grey. Below [`SOLID`] a pixel is
//! content and is left exactly as it was; above [`CLEAR`] it is background
//! and goes to nothing; in between, alpha falls linearly, and the key's share
//! is taken back out of the colour — `C = a·F + (1 − a)·K`, solved for `F` —
//! which is what makes an anti-aliased edge a dark pixel at partial alpha
//! rather than a pink one. A second pass, [`despill`], takes what excess is
//! left off opaque pixels within two of a keyed one: the tint that 4:2:0
//! chroma lays on a black letter's edge, which is below [`SOLID`] and would
//! otherwise be a magenta fringe round every word.
//!
//! # What it costs
//!
//! Measured against `chrome-headless-shell` 153, 1280x768, a JPEG q85
//! screencast frame of each page keyed and compared with the PNG still of the
//! same page with true transparency:
//!
//! | page | background clear | content keyed (alpha < 128) | cost |
//! | --- | --- | --- | --- |
//! | white `body`, black text | 99.96% | 0 | |
//! | text, a photo, buttons, a 50% overlay | 99.3% | 3.0% — the vivid purple box | |
//! | Google's front page | 99.3% | 0 | |
//! | the keying, a 1280x768 frame | | | 0.9 ms on an M-series, on a 3.5 ms decode |
//!
//! The 0.7% of the background that is not wholly clear is the ring of pixels
//! round content whose chroma JPEG averaged with it. What the page shows in
//! the key's colour goes transparent. A semi-transparent overlay over the
//! bare page is over magenta in the frame, so it keys to partly clear — a
//! 50% black one comes out about 75% — and white text on it picks up the tint
//! at its edges. Pink and purple next to the bare page lose their key share
//! at the edge, which on thin text is all of it: a visited link's purple
//! comes out navy where it touches the background.

/// The key colour: magenta. See "Which colour" above.
pub const KEY: (u8, u8, u8) = (255, 0, 255);

/// The key as CSS.
pub const KEY_CSS: &str = "#ff00ff";

/// Below this much excess a pixel is content, left exactly as it was.
pub const SOLID: i32 = 96;

/// Above this much excess a pixel is background, and goes to nothing.
pub const CLEAR: i32 = 224;

/// How much of the key a pixel carries: `min(r, b) - g`.
fn excess(r: i32, g: i32, b: i32) -> i32 {
    r.min(b) - g
}

/// Key one RGBA pixel in place, its alpha from nothing to 255: content left
/// as it was, background cleared, and what is between made partly clear
/// with the key's share taken back out of its colour.
#[inline]
pub fn key_pixel(pixel: &mut [u8]) {
    let (r, g, b) = (
        i32::from(pixel[0]),
        i32::from(pixel[1]),
        i32::from(pixel[2]),
    );
    let e = excess(r, g, b);
    if e <= SOLID {
        pixel[3] = 255;
        return;
    }
    if e >= CLEAR {
        pixel[..4].copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    let a = 255 * (CLEAR - e) / (CLEAR - SOLID);
    let unmix = |c: i32, k: u8| ((c * 255 - (255 - a) * i32::from(k)) / a).clamp(0, 255);
    let (r, g, b) = (unmix(r, KEY.0), unmix(g, KEY.1), unmix(b, KEY.2));
    // What is left of the key after that is spill.
    let left = excess(r, g, b).max(0);
    pixel[0] = (r - left) as u8;
    pixel[1] = g as u8;
    pixel[2] = (b - left) as u8;
    pixel[3] = a as u8;
}

/// Key a whole RGBA picture in place: [`key_pixel`] on every pixel, then
/// [`despill`]. For the still, which arrives decoded; a JPEG frame has the
/// first half done as it is decoded.
pub fn key(rgba: &mut [u8], width: u32, height: u32) {
    for pixel in rgba.chunks_exact_mut(4) {
        key_pixel(pixel);
    }
    despill(rgba, width, height);
}

/// Take the key's excess off opaque pixels within two of a keyed one.
///
/// 4:2:0 chroma is at half resolution, so the key's colour reaches two pixels
/// into whatever borders it: on a black letter that is a magenta fringe, too
/// faint to key and too strong not to see. Only near the key, because
/// further in a pinkish pixel is the page's own pink.
pub fn despill(rgba: &mut [u8], width: u32, height: u32) {
    let (w, h) = (width as usize, height as usize);
    if rgba.len() < w * h * 4 {
        return;
    }
    for y in 0..h {
        for x in 0..w {
            let at = (y * w + x) * 4;
            if rgba[at + 3] != 255 {
                continue;
            }
            let (r, g, b) = (
                i32::from(rgba[at]),
                i32::from(rgba[at + 1]),
                i32::from(rgba[at + 2]),
            );
            let e = excess(r, g, b);
            if e <= 0 {
                continue;
            }
            let near = (y.saturating_sub(2)..(y + 3).min(h)).any(|yy| {
                (x.saturating_sub(2)..(x + 3).min(w)).any(|xx| rgba[(yy * w + xx) * 4 + 3] != 255)
            });
            if near {
                rgba[at] = (r - e) as u8;
                rgba[at + 2] = (b - e) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyed(rgb: [u8; 3]) -> [u8; 4] {
        let mut pixel = [rgb[0], rgb[1], rgb[2], 0];
        key_pixel(&mut pixel);
        pixel
    }

    #[test]
    fn the_key_goes_clear_and_content_is_left_exactly_as_it_was() {
        assert_eq!(keyed([255, 0, 255]), [0, 0, 0, 0]);
        assert_eq!(
            keyed([250, 4, 252]),
            [0, 0, 0, 0],
            "a JPEG's key, near enough"
        );
        for content in [
            [0, 0, 0],
            [255, 255, 255],
            [0x1a, 0x0d, 0xab],
            [0x34, 0xa8, 0x53],
            [0xe9, 0x1e, 0x63],
            [0x68, 0x1d, 0xa8],
        ] {
            assert_eq!(
                keyed(content),
                [content[0], content[1], content[2], 255],
                "{content:?}"
            );
        }
    }

    #[test]
    fn a_pixel_half_key_half_black_is_black_and_partly_clear() {
        // What an anti-aliased black edge over the key is. Partly clear and
        // black — at about three quarters rather than a half, because the
        // band starts at SOLID rather than at nothing.
        let half = keyed([128, 0, 128]);
        assert!((160..220).contains(&half[3]), "{half:?}");
        assert!(half[0] < 16 && half[1] < 16 && half[2] < 16, "{half:?}");
        // A vivid purple is keyed, which is the cost of the colour.
        assert!(keyed([0xd5, 0x00, 0xf9])[3] < 128);
    }

    #[test]
    fn despill_takes_the_tint_off_an_edge_and_leaves_pink_away_from_the_key_alone() {
        // A row: the key, a black letter's tinted edge, then pink well away.
        let mut row: Vec<u8> = Vec::new();
        for pixel in [
            [255u8, 0, 255],
            [60, 0, 60],
            [0, 0, 0],
            [0, 0, 0],
            [0, 0, 0],
            [0xe9, 0x1e, 0x63],
        ] {
            row.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0]);
        }
        key(&mut row, 6, 1);
        assert_eq!(row[3], 0, "the key");
        assert_eq!(&row[4..8], &[0, 0, 0, 255], "the edge, despilled");
        assert_eq!(
            &row[20..24],
            &[0xe9, 0x1e, 0x63, 255],
            "the page's own pink"
        );
    }
}
