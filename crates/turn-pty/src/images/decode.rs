//! Turning compressed bytes a process supplied into bounded RGBA.
//!
//! This is the most dangerous function in the inline-image feature and it is written for
//! that. The input is arbitrary bytes claiming to be a picture, and the classic attack is
//! not a malformed file — decoders handle those — it is a **decompression bomb**: thirty
//! bytes of PNG header declaring sixty thousand by sixty thousand pixels, which is
//! fourteen gigabytes of RGBA the moment anything believes it.
//!
//! So the order of operations is the defence, and it is not negotiable:
//!
//! 1. **Read the header only** and get the declared dimensions. `image`'s reader does this
//!    without decoding a pixel.
//! 2. **Check the numbers** against [`MAX_DECODE_PIXELS`] and against the caller's own
//!    remaining budget. A refusal here has allocated nothing.
//! 3. **Decode with limits still set**, so a file whose header lies about its size is
//!    stopped by the decoder rather than by the kernel's OOM killer.
//! 4. **Downscale to [`turn_proto::MAX_IMAGE_PIXELS`]**, because a picture with more
//!    detail than a terminal cell box can show has nowhere to put it and no business
//!    crossing a socket.
//!
//! Step 4 is a feature rather than a concession: a phone photograph pasted into a terminal
//! is twelve megapixels and will be drawn into about a thousand by six hundred, so
//! shipping the original would be forty-eight megabytes to render one twentieth of.
//!
//! ## Which formats, and why so few
//!
//! PNG, JPEG, GIF, WebP and BMP. Every additional decoder is additional attack surface
//! reachable by a process printing to its own terminal, and these five are what the three
//! protocols are used with in practice. A format that is not one of them is refused with a
//! notice the user can read, which is a better outcome than a decoder nobody has looked
//! at.

use std::io::Cursor;

use image::{ImageReader, Limits};
use turn_proto::MAX_IMAGE_PIXELS;

/// Most pixels this will decode before downscaling, sixteen mebipixels.
///
/// The transient cost of decoding is four bytes a pixel, so this is a 64 MiB peak for one
/// image — high enough for a real photograph, low enough that it is a bounded spike rather
/// than a machine falling over. Anything larger is refused and the user is told, because
/// silently showing a thumbnail of something Turn could not actually read would be a lie.
pub const MAX_DECODE_PIXELS: u32 = 16 * 1024 * 1024;

/// A decoded picture: RGBA, unassociated alpha, row-major, no padding.
#[derive(Clone, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Raster {
    /// Builds a raster, checking the buffer is exactly the size the dimensions claim.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        if width == 0 || height == 0 || rgba.len() != expected {
            return None;
        }
        Some(Self {
            width,
            height,
            rgba,
        })
    }

    pub fn pixels(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    /// The pixel at `(x, y)`, for tests and for the downscaler.
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let base = (y as usize * self.width as usize + x as usize) * 4;
        match self.rgba.get(base..base + 4) {
            Some(slice) => [slice[0], slice[1], slice[2], slice[3]],
            None => [0, 0, 0, 0],
        }
    }
}

impl std::fmt::Debug for Raster {
    /// Shape only. A `Debug` that printed four megabytes of pixels into a failing
    /// assertion would make the assertion unreadable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Raster({}x{}, {} bytes)",
            self.width,
            self.height,
            self.rgba.len()
        )
    }
}

/// Why a picture could not be decoded.
///
/// Every variant is something a user should be told about in the pane, so each one has to
/// be describable in a short sentence — see [`crate::images::RefusalReason`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The declared dimensions are past what Turn will decode. The bomb case.
    #[error("an image of {width}x{height} is {pixels} pixels, over the limit of {max}")]
    TooLarge {
        width: u32,
        height: u32,
        pixels: u64,
        max: u32,
    },
    /// The dimensions are allowed but the pane's own decode budget is spent. A process
    /// cannot buy unlimited decoding with a handful of bytes.
    #[error("{pixels} pixels is more than the {budget} left in this pane's decode budget")]
    OverBudget { pixels: u64, budget: u64 },
    /// The bytes are not a format Turn decodes.
    #[error("the payload is not a picture in a format Turn reads")]
    UnknownFormat,
    /// The bytes are the right format and still will not decode: truncated, corrupt, or
    /// declaring something the decoder itself refused.
    #[error("the payload is a damaged or unreadable picture")]
    Damaged,
    /// A zero dimension. Not decodable, and not worth a place on screen.
    #[error("the payload has no pixels")]
    Empty,
}

/// Decodes a compressed picture, bounded at every step.
///
/// `budget_pixels` is what the caller will allow *this* decode to cost, so the pane's
/// amortised budget is enforced before the expensive part rather than after it.
pub fn decode_bounded(bytes: &[u8], budget_pixels: u64) -> Result<Raster, DecodeError> {
    if bytes.is_empty() {
        return Err(DecodeError::Empty);
    }
    // 1. The header, and nothing else. `with_guessed_format` sniffs the magic bytes and
    //    `into_dimensions` stops as soon as the size is known.
    let probe = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| DecodeError::UnknownFormat)?;
    if probe.format().is_none() {
        return Err(DecodeError::UnknownFormat);
    }
    let (width, height) = probe.into_dimensions().map_err(|_| DecodeError::Damaged)?;

    // 2. The numbers, before a single pixel is reserved.
    check_pixels(width, height, budget_pixels)?;

    // 3. Decode, with the limits left in place so a header that lied is stopped by the
    //    decoder rather than by memory exhaustion.
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| DecodeError::UnknownFormat)?;
    let mut limits = Limits::no_limits();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    // Four bytes a pixel for the output, and a decoder's own scratch on top; twice the
    // final buffer is enough for every format enabled here and still bounded.
    limits.max_alloc = Some(2 * width as u64 * height as u64 * 4 + 64 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| DecodeError::Damaged)?;
    let rgba = decoded.to_rgba8();
    let raster =
        Raster::new(rgba.width(), rgba.height(), rgba.into_raw()).ok_or(DecodeError::Damaged)?;

    // 4. Down to something a terminal can actually show.
    Ok(fit_within(raster, MAX_IMAGE_PIXELS))
}

/// Refuses a geometry from its numbers alone.
pub fn check_pixels(width: u32, height: u32, budget_pixels: u64) -> Result<(), DecodeError> {
    if width == 0 || height == 0 {
        return Err(DecodeError::Empty);
    }
    let pixels = width as u64 * height as u64;
    if pixels > MAX_DECODE_PIXELS as u64 {
        return Err(DecodeError::TooLarge {
            width,
            height,
            pixels,
            max: MAX_DECODE_PIXELS,
        });
    }
    if pixels > budget_pixels {
        return Err(DecodeError::OverBudget {
            pixels,
            budget: budget_pixels,
        });
    }
    Ok(())
}

/// Scales a raster down until it holds at most `max_pixels`, keeping its aspect ratio.
///
/// Area averaging rather than nearest neighbour, because a nearest-neighbour reduction of
/// a screenshot turns text into noise, and a plot's gridlines disappear entirely.
///
/// Alpha is **premultiplied while averaging and separated again after**. Averaging
/// unassociated RGBA mixes the colour of fully transparent pixels into their neighbours,
/// which is what puts a dark halo around every antialiased edge of a transparent PNG.
pub fn fit_within(raster: Raster, max_pixels: u32) -> Raster {
    if raster.pixels() <= max_pixels as u64 {
        return raster;
    }
    let scale = (max_pixels as f64 / raster.pixels() as f64).sqrt();
    let width = ((raster.width as f64 * scale).floor() as u32).max(1);
    let height = ((raster.height as f64 * scale).floor() as u32).max(1);
    downsample(&raster, width, height)
}

/// Area-averages `source` into a `width` by `height` raster.
fn downsample(source: &Raster, width: u32, height: u32) -> Raster {
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        // The source rows this destination row covers. Computed from the boundaries so
        // every source pixel belongs to exactly one destination pixel and none is skipped.
        let y0 = (y as u64 * source.height as u64 / height as u64) as u32;
        let y1 = (((y as u64 + 1) * source.height as u64 / height as u64) as u32).max(y0 + 1);
        for x in 0..width {
            let x0 = (x as u64 * source.width as u64 / width as u64) as u32;
            let x1 = (((x as u64 + 1) * source.width as u64 / width as u64) as u32).max(x0 + 1);

            let mut r = 0u64;
            let mut g = 0u64;
            let mut b = 0u64;
            let mut a = 0u64;
            let mut count = 0u64;
            for sy in y0..y1.min(source.height) {
                for sx in x0..x1.min(source.width) {
                    let [pr, pg, pb, pa] = source.at(sx, sy);
                    // Premultiplied, so a transparent pixel contributes no colour.
                    r += pr as u64 * pa as u64;
                    g += pg as u64 * pa as u64;
                    b += pb as u64 * pa as u64;
                    a += pa as u64;
                    count += 1;
                }
            }
            if count == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let alpha = a / count;
            if alpha == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            // Back to unassociated alpha: divide by the alpha that was multiplied in.
            out.push((r / a).min(255) as u8);
            out.push((g / a).min(255) as u8);
            out.push((b / a).min(255) as u8);
            out.push(alpha.min(255) as u8);
        }
    }
    Raster {
        width,
        height,
        rgba: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG of `width` by `height` in one colour, built with the encoder rather than a
    /// hand-rolled byte string, so the test exercises a real file.
    fn png(width: u32, height: u32, colour: [u8; 4]) -> Vec<u8> {
        let mut buffer = image::RgbaImage::new(width, height);
        for pixel in buffer.pixels_mut() {
            *pixel = image::Rgba(colour);
        }
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(buffer)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("the encoder produces a PNG");
        out.into_inner()
    }

    /// A PNG that claims an enormous raster with almost no bytes behind it. This is the
    /// bomb, and it is a *valid* PNG as far as any decoder can tell from the header: the
    /// real ones are exactly this, an honest IHDR and a few hundred kilobytes of compressed
    /// zeroes. It is small, it parses, and believing it costs gigabytes.
    fn bomb(width: u32, height: u32) -> Vec<u8> {
        let mut out: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut ihdr: Vec<u8> = Vec::from(b"IHDR".as_slice());
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        // 8-bit RGBA, no interlace.
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        push_chunk(&mut out, &ihdr);

        // A tiny compressed stream. A real bomb's is longer and expands to the whole
        // raster; this one is enough for the header to be read, which is the point.
        let mut idat: Vec<u8> = Vec::from(b"IDAT".as_slice());
        idat.extend_from_slice(&[0x78, 0x9c, 0x63, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01]);
        push_chunk(&mut out, &idat);
        push_chunk(&mut out, b"IEND");
        out
    }

    /// Appends a PNG chunk: its length, its body (type included) and its CRC.
    fn push_chunk(out: &mut Vec<u8>, body: &[u8]) {
        out.extend_from_slice(&((body.len() - 4) as u32).to_be_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(&crc32(body).to_be_bytes());
    }

    /// The PNG CRC, so the forged header is one a decoder will actually read.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for byte in data {
            crc ^= *byte as u32;
            for _ in 0..8 {
                let mask = if crc & 1 != 0 { 0xEDB8_8320 } else { 0 };
                crc = (crc >> 1) ^ mask;
            }
        }
        !crc
    }

    #[test]
    fn a_real_png_decodes_to_the_pixels_it_holds() {
        let raster = decode_bounded(&png(4, 3, [10, 20, 30, 255]), u64::MAX).expect("it decodes");
        assert_eq!((raster.width, raster.height), (4, 3));
        assert_eq!(raster.rgba.len(), 4 * 3 * 4);
        assert_eq!(raster.at(0, 0), [10, 20, 30, 255]);
        assert_eq!(raster.at(3, 2), [10, 20, 30, 255]);
    }

    /// The attack this module is written for. A tiny header claiming 3.6 gigapixels must
    /// be refused **from its numbers**, with nothing allocated for it.
    #[test]
    fn a_decompression_bomb_is_refused_from_its_header_before_anything_is_allocated() {
        let tiny = bomb(60_000, 60_000);
        assert!(
            tiny.len() < 100,
            "the point is that the attack is small: {} bytes",
            tiny.len()
        );
        let error = decode_bounded(&tiny, u64::MAX).expect_err("3.6 gigapixels is refused");
        match error {
            DecodeError::TooLarge { pixels, max, .. } => {
                assert_eq!(pixels, 3_600_000_000);
                assert_eq!(max, MAX_DECODE_PIXELS);
            }
            other => panic!("expected a size refusal, got {other}"),
        }

        // And the same by the other route: a header inside the hard limit but past what
        // this pane may spend.
        let error = decode_bounded(&bomb(4_000, 4_000), 1_000).expect_err("over budget");
        assert!(matches!(error, DecodeError::OverBudget { .. }), "{error}");
    }

    #[test]
    fn a_payload_that_is_not_a_picture_is_refused_rather_than_guessed_at() {
        assert_eq!(
            decode_bounded(b"this is just text", u64::MAX),
            Err(DecodeError::UnknownFormat)
        );
        assert_eq!(decode_bounded(&[], u64::MAX), Err(DecodeError::Empty));
        // A format Turn deliberately does not enable.
        assert!(matches!(
            decode_bounded(b"II*\0\x08\0\0\0", u64::MAX),
            Err(DecodeError::UnknownFormat) | Err(DecodeError::Damaged)
        ));
    }

    /// Truncation is the ordinary failure — a payload cut off by a limit upstream, or an
    /// escape sequence that ended early — and it must be a refusal rather than a panic.
    #[test]
    fn a_truncated_picture_is_refused_and_does_not_panic() {
        let whole = png(8, 8, [255, 0, 0, 255]);
        for cut in 1..whole.len() {
            // Whatever the result, the only unacceptable outcome is a panic.
            let _ = decode_bounded(&whole[..cut], u64::MAX);
        }
        assert!(
            decode_bounded(&whole[..whole.len() / 2], u64::MAX).is_err(),
            "half a PNG is not a picture"
        );
    }

    /// A picture too detailed to show is scaled rather than refused: the alternative is
    /// telling somebody their perfectly ordinary photograph is too big for a terminal.
    #[test]
    fn a_picture_larger_than_a_terminal_can_show_is_scaled_down_not_refused() {
        // 2000x1200 is 2.4 megapixels, over the transmit limit of one.
        let big = png(2_000, 1_200, [40, 80, 120, 255]);
        let raster = decode_bounded(&big, u64::MAX).expect("it decodes");
        assert!(
            raster.pixels() <= MAX_IMAGE_PIXELS as u64,
            "{} pixels survived the fit",
            raster.pixels()
        );
        // The aspect ratio survives, which is what stops a photograph coming out squashed.
        let before = 2_000.0 / 1_200.0;
        let after = raster.width as f64 / raster.height as f64;
        assert!(
            (before - after).abs() < 0.02,
            "the aspect ratio moved from {before} to {after}"
        );
        // And the colour survives, because averaging one colour gives that colour.
        assert_eq!(
            raster.at(raster.width / 2, raster.height / 2),
            [40, 80, 120, 255]
        );
    }

    #[test]
    fn a_picture_small_enough_to_send_is_handed_back_untouched() {
        let raster = Raster::new(4, 4, vec![9; 64]).expect("a valid raster");
        let fitted = fit_within(raster.clone(), MAX_IMAGE_PIXELS);
        assert_eq!(fitted, raster, "an unnecessary resample would lose detail");
    }

    /// Averaging unassociated alpha is what puts a dark ring around every antialiased
    /// edge of a transparent PNG. This is the test that says it does not happen here.
    #[test]
    fn scaling_a_transparent_image_does_not_bleed_the_colour_of_invisible_pixels() {
        // Two pixels: one opaque white, one fully transparent black.
        let source = Raster::new(2, 1, vec![255, 255, 255, 255, 0, 0, 0, 0]).expect("a raster");
        let scaled = downsample(&source, 1, 1);
        let [r, g, b, a] = scaled.at(0, 0);
        assert_eq!(
            [r, g, b],
            [255, 255, 255],
            "the invisible pixel must contribute no colour, only transparency"
        );
        assert_eq!(a, 127, "and half of the area is transparent");
    }

    #[test]
    fn scaling_averages_rather_than_dropping_pixels() {
        // A two-by-two checkerboard of black and white averages to mid grey. Nearest
        // neighbour would give black or white, which is how a plot loses its gridlines.
        let source = Raster::new(
            2,
            2,
            vec![
                0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255,
            ],
        )
        .expect("a raster");
        let scaled = downsample(&source, 1, 1);
        assert_eq!(scaled.at(0, 0), [127, 127, 127, 255]);
    }

    #[test]
    fn every_source_pixel_belongs_to_exactly_one_destination_pixel() {
        // A gradient, scaled by a factor that does not divide evenly. If the boundary
        // arithmetic dropped or double-counted a row, the ends would not match.
        let width = 7u32;
        let height = 5u32;
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let v = ((x + y * width) * 255 / (width * height - 1)) as u8;
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let source = Raster::new(width, height, rgba).expect("a raster");
        let scaled = downsample(&source, 3, 2);
        assert_eq!(scaled.rgba.len(), 3 * 2 * 4);
        assert!(
            scaled.at(0, 0)[0] < scaled.at(2, 1)[0],
            "the gradient survives"
        );
        assert!(scaled.rgba.iter().skip(3).step_by(4).all(|a| *a == 255));
    }

    #[test]
    fn a_raster_whose_buffer_does_not_match_its_dimensions_cannot_be_built() {
        assert!(Raster::new(2, 2, vec![0; 16]).is_some());
        assert!(Raster::new(2, 2, vec![0; 15]).is_none());
        assert!(Raster::new(0, 2, Vec::new()).is_none());
        assert!(Raster::new(u32::MAX, u32::MAX, Vec::new()).is_none());
    }

    #[test]
    fn a_jpeg_and_a_gif_decode_too_because_that_is_what_tools_emit() {
        for format in [image::ImageFormat::Jpeg, image::ImageFormat::Gif] {
            let mut buffer = image::RgbaImage::new(8, 8);
            for pixel in buffer.pixels_mut() {
                *pixel = image::Rgba([200, 100, 50, 255]);
            }
            let mut out = Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(buffer)
                .write_to(&mut out, format)
                .expect("the encoder produces a file");
            let raster = decode_bounded(&out.into_inner(), u64::MAX)
                .unwrap_or_else(|e| panic!("{format:?} did not decode: {e}"));
            assert_eq!((raster.width, raster.height), (8, 8));
        }
    }

    #[test]
    fn the_debug_form_of_a_raster_names_its_shape_and_not_its_pixels() {
        let raster = Raster::new(2, 2, vec![0xCD; 16]).expect("a raster");
        let debugged = format!("{raster:?}");
        assert!(debugged.contains("2x2"), "got {debugged}");
        assert!(!debugged.contains("205"), "the pixels leaked: {debugged}");
    }
}
