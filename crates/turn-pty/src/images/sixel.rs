//! Sixel: `ESC P … q … ST`.
//!
//! The oldest of the three protocols and still the most widely emitted, because it is what
//! `gnuplot`, `matplotlib`'s sixel backend and `img2sixel` produce. There is no compressed
//! container here — the escape sequence *is* the raster, six vertical pixels per printable
//! character — so there is no library to hand it to and no decoder to bound. This module
//! is the decoder, and the bounds are its own.
//!
//! ## What is implemented
//!
//! Everything a real emitter uses:
//!
//! * `"Pan;Pad;Ph;Pv` raster attributes, used to refuse an impossible picture **before**
//!   any canvas is allocated for it.
//! * `#Pc;Pu;Px;Py;Pz` colour definition, in both RGB (`Pu=2`) and DEC's HLS (`Pu=1`),
//!   and `#Pc` to select a register.
//! * `!Pn` run-length repetition.
//! * `$` graphics carriage return and `-` graphics newline.
//! * The 256 colour registers, starting from the VT340 palette so a file that selects a
//!   register it never defined draws in the colour its author expected.
//!
//! ## How it is bounded
//!
//! A Sixel is arbitrary bytes and its extent is not declared unless the emitter chose to
//! declare it, so the canvas has to grow. It grows **per row**, and the total number of
//! pixels reserved is charged against the caller's budget as it goes, which means a
//! sequence that never terminates is refused at a known size rather than at the limits of
//! the machine. Run-length is the dangerous instruction — `!999999?` is seven bytes asking
//! for a million pixels — so the repeat count is clamped to what remains of the row.
//!
//! ## Where transparency comes from
//!
//! A pixel no sixel ever set is left fully transparent, whatever `P2` said. In a terminal
//! the thing behind a picture is the pane's own background, and the useful behaviour is to
//! let it show through; filling it with a background colour the emitter guessed at is how
//! a plot ends up in a black box on a light theme.

use super::decode::Raster;

/// Widest a Sixel canvas may become.
///
/// Wide enough for a full-screen plot on a large display and far short of the pixel
/// budget, so the row bound and the pixel bound are two independent limits rather than one
/// restated.
pub const MAX_SIXEL_WIDTH: u32 = 4_096;

/// Tallest a Sixel canvas may become.
pub const MAX_SIXEL_HEIGHT: u32 = 4_096;

/// Most bytes of Sixel data Turn will hold for one picture, 8 MiB.
///
/// Sixel is not compressed: 8 MiB of it is around ten megapixels of one-bit-per-channel
/// data, comfortably more than any real emitter produces for a terminal. A sequence that
/// passes this is refused, and its bytes are released rather than held until the process
/// remembers to terminate it.
pub const MAX_SIXEL_BYTES: usize = 8 * 1024 * 1024;

/// How many colour registers a Sixel has.
const REGISTERS: usize = 256;

/// Why a Sixel could not be turned into pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SixelError {
    #[error("a Sixel of {width}x{height} is {pixels} pixels, over the limit of {max}")]
    TooLarge {
        width: u32,
        height: u32,
        pixels: u64,
        max: u64,
    },
    /// The picture ran past the canvas caps, so what was drawn is only part of it.
    ///
    /// Refused rather than cropped. A Sixel is drawn instruction by instruction with no
    /// declared extent, so the only way to find out it is too big is to run out of room —
    /// and handing back the part that fitted would show the user a picture that is missing
    /// its right-hand side without saying so.
    #[error("the Sixel drew past the {width}x{height} a canvas may reach")]
    PastCanvas { width: u32, height: u32 },
    /// The picture ran past what this pane may still decode.
    #[error("the Sixel drew past this pane's remaining decode budget")]
    OverBudget,
    #[error("the Sixel set no pixels at all")]
    Empty,
}

/// The VT340's sixteen colour registers, as a real terminal starts with them.
///
/// A file that selects register 3 without defining it expects green, and a decoder that
/// started every register at black would draw an invisible plot.
const DEFAULT_PALETTE: [[u8; 3]; 16] = [
    [0, 0, 0],
    [51, 51, 204],
    [204, 33, 33],
    [51, 204, 51],
    [204, 51, 204],
    [51, 204, 204],
    [204, 204, 51],
    [135, 135, 135],
    [66, 66, 66],
    [84, 84, 153],
    [153, 66, 66],
    [84, 153, 84],
    [153, 84, 153],
    [84, 153, 153],
    [153, 153, 84],
    [204, 204, 204],
];

/// A canvas that grows one row at a time, charging every pixel it reserves.
///
/// A row is its own `Vec` rather than a slice of one wide buffer on purpose. A Sixel that
/// does not declare its width would otherwise have to reserve the widest row it *might*
/// draw for every row it *does*, which for a tall narrow plot is thirty megabytes of
/// nothing. Per-row growth costs one pointer per row and reserves what is used.
struct Canvas {
    /// Colour register per pixel, offset by one so that zero means "never set".
    rows: Vec<Vec<u16>>,
    reserved: u64,
    budget: u64,
    width_cap: u32,
    height_cap: u32,
    /// Set once the pixel budget ran out.
    over_budget: bool,
    /// Set once the width or height cap was reached.
    past_canvas: bool,
}

impl Canvas {
    fn new(budget: u64, width_cap: u32, height_cap: u32) -> Self {
        Self {
            rows: Vec::new(),
            reserved: 0,
            budget,
            width_cap: width_cap.clamp(1, MAX_SIXEL_WIDTH),
            height_cap: height_cap.clamp(1, MAX_SIXEL_HEIGHT),
            over_budget: false,
            past_canvas: false,
        }
    }

    /// Whether a bound was hit, so what was drawn is only part of the picture.
    fn refused(&self) -> Option<SixelError> {
        if self.over_budget {
            return Some(SixelError::OverBudget);
        }
        if self.past_canvas {
            return Some(SixelError::PastCanvas {
                width: self.width_cap,
                height: self.height_cap,
            });
        }
        None
    }

    /// Sets one pixel, growing the canvas to reach it.
    ///
    /// Past a bound it records which bound and stops drawing. The sequence still has to be
    /// read to the end — its terminator is the only way to know where it stops — but nothing
    /// more is recorded, and the picture will be refused rather than cropped.
    fn set(&mut self, x: u32, y: u32, register: u8) {
        if x >= self.width_cap || y >= self.height_cap {
            self.past_canvas = true;
            return;
        }
        if self.over_budget {
            return;
        }
        while self.rows.len() <= y as usize {
            self.rows.push(Vec::new());
        }
        let row = match self.rows.get_mut(y as usize) {
            Some(row) => row,
            None => return,
        };
        if row.len() <= x as usize {
            let growth = x as u64 + 1 - row.len() as u64;
            if self.reserved + growth > self.budget {
                self.over_budget = true;
                return;
            }
            self.reserved += growth;
            row.resize(x as usize + 1, 0);
        }
        if let Some(slot) = row.get_mut(x as usize) {
            *slot = register as u16 + 1;
        }
    }

    /// The canvas as RGBA, cropped to what was actually drawn.
    fn into_raster(self, palette: &[[u8; 3]; REGISTERS]) -> Result<Raster, SixelError> {
        if let Some(error) = self.refused() {
            return Err(error);
        }
        let height = self
            .rows
            .iter()
            .rposition(|row| row.iter().any(|slot| *slot != 0))
            .map(|last| last + 1)
            .unwrap_or(0);
        let width = self
            .rows
            .iter()
            .take(height)
            .map(|row| row.iter().rposition(|slot| *slot != 0).map_or(0, |i| i + 1))
            .max()
            .unwrap_or(0);
        if width == 0 || height == 0 {
            return Err(SixelError::Empty);
        }

        let mut rgba = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            let row = self.rows.get(y);
            for x in 0..width {
                match row.and_then(|row| row.get(x)).copied().unwrap_or(0) {
                    // Never set: transparent, so the pane's own background shows through.
                    0 => rgba.extend_from_slice(&[0, 0, 0, 0]),
                    slot => {
                        let colour = palette[(slot - 1) as usize % REGISTERS];
                        rgba.extend_from_slice(&[colour[0], colour[1], colour[2], 255]);
                    }
                }
            }
        }
        Raster::new(width as u32, height as u32, rgba).ok_or(SixelError::Empty)
    }
}

/// Decodes a Sixel body — everything between the `q` and the string terminator.
///
/// `budget_pixels` is what the caller will let this picture reserve; the raster attributes
/// are checked against it before anything is allocated, and the growth of the canvas is
/// charged against it as the body is read.
pub fn decode(data: &[u8], budget_pixels: u64) -> Result<Raster, SixelError> {
    let mut palette = starting_palette();
    let mut register = 0u8;
    let mut x = 0u32;
    // Top row of the current six-pixel band.
    let mut band = 0u32;
    let mut canvas: Option<Canvas> = None;
    let mut declared: Option<(u32, u32)> = None;

    let mut index = 0usize;
    while index < data.len() {
        let byte = data[index];
        match byte {
            b'"' => {
                let (params, next) = read_params(data, index + 1);
                index = next;
                // Pan;Pad;Ph;Pv — the aspect ratio, then the declared extent.
                let width = params.get(2).copied().unwrap_or(0);
                let height = params.get(3).copied().unwrap_or(0);
                if width > 0 && height > 0 {
                    let pixels = width as u64 * height as u64;
                    let max = budget_pixels.min(MAX_SIXEL_WIDTH as u64 * MAX_SIXEL_HEIGHT as u64);
                    if pixels > max {
                        // Refused from the declaration, before a canvas exists.
                        return Err(SixelError::TooLarge {
                            width,
                            height,
                            pixels,
                            max,
                        });
                    }
                    declared = Some((width, height));
                }
            }
            b'#' => {
                let (params, next) = read_params(data, index + 1);
                index = next;
                let selected = params.first().copied().unwrap_or(0);
                register = (selected as usize % REGISTERS) as u8;
                if params.len() >= 5 {
                    if let Some(colour) = define_colour(&params) {
                        palette[register as usize] = colour;
                    }
                }
            }
            b'!' => {
                let (count, next) = read_number(data, index + 1);
                index = next;
                // The repeat applies to whatever character comes next, which must be a
                // sixel. Anything else is a malformed sequence and the count is dropped.
                if let Some(sixel) = data.get(index).copied().filter(|b| is_sixel(*b)) {
                    index += 1;
                    let canvas =
                        canvas.get_or_insert_with(|| start_canvas(declared, budget_pixels));
                    // Clamped to the row, so `!999999?` cannot ask for more than a row's
                    // worth of pixels however few bytes it took to write.
                    let room = canvas.width_cap.saturating_sub(x);
                    let repeat = count.min(room);
                    draw_run(canvas, &mut x, band, register, sixel, repeat);
                }
            }
            b'$' => {
                x = 0;
                index += 1;
            }
            b'-' => {
                x = 0;
                band = band.saturating_add(6);
                index += 1;
            }
            byte if is_sixel(byte) => {
                index += 1;
                let canvas = canvas.get_or_insert_with(|| start_canvas(declared, budget_pixels));
                draw_run(canvas, &mut x, band, register, byte, 1);
            }
            // Whitespace between instructions is legal and carries nothing. Anything else
            // is skipped for the same reason a terminal skips an escape it does not know:
            // one unrecognised byte must not throw away the rest of the picture.
            _ => index += 1,
        }
    }

    match canvas {
        Some(canvas) => canvas.into_raster(&palette),
        None => Err(SixelError::Empty),
    }
}

/// The register palette a picture starts with.
fn starting_palette() -> [[u8; 3]; REGISTERS] {
    let mut palette = [[0u8; 3]; REGISTERS];
    for (slot, colour) in DEFAULT_PALETTE.iter().enumerate() {
        palette[slot] = *colour;
    }
    palette
}

/// A canvas sized from the declaration when there is one, and from the hard caps when
/// there is not.
fn start_canvas(declared: Option<(u32, u32)>, budget: u64) -> Canvas {
    match declared {
        Some((width, height)) => Canvas::new(budget, width, height),
        None => Canvas::new(budget, MAX_SIXEL_WIDTH, MAX_SIXEL_HEIGHT),
    }
}

/// Whether a byte is one of the printable characters that carry six pixels.
fn is_sixel(byte: u8) -> bool {
    (0x3F..=0x7E).contains(&byte)
}

/// Draws `repeat` copies of one sixel character, advancing `x`.
fn draw_run(canvas: &mut Canvas, x: &mut u32, band: u32, register: u8, sixel: u8, repeat: u32) {
    let bits = sixel - 0x3F;
    for _ in 0..repeat {
        for row in 0..6u32 {
            if bits & (1 << row) != 0 {
                canvas.set(*x, band + row, register);
            }
        }
        *x = x.saturating_add(1);
    }
}

/// A colour definition: `Pc;Pu;Px;Py;Pz`.
///
/// `Pu=2` is RGB in percent, which is what every emitter uses. `Pu=1` is DEC's HLS, whose
/// hue is offset by 240 degrees from the usual convention — zero is blue, not red — and
/// getting that wrong turns a plot's colours into their complements.
fn define_colour(params: &[u32]) -> Option<[u8; 3]> {
    let system = params.get(1).copied()?;
    let a = params.get(2).copied()?;
    let b = params.get(3).copied()?;
    let c = params.get(4).copied()?;
    match system {
        1 => Some(hls_to_rgb(a % 360, b.min(100), c.min(100))),
        2 => Some([
            percent_to_byte(a.min(100)),
            percent_to_byte(b.min(100)),
            percent_to_byte(c.min(100)),
        ]),
        _ => None,
    }
}

fn percent_to_byte(percent: u32) -> u8 {
    ((percent * 255 + 50) / 100).min(255) as u8
}

/// DEC HLS to RGB. Hue is in degrees with blue at zero, lightness and saturation in
/// percent.
fn hls_to_rgb(hue: u32, lightness: u32, saturation: u32) -> [u8; 3] {
    let l = lightness as f32 / 100.0;
    let s = saturation as f32 / 100.0;
    // Back to the convention where zero is red.
    let h = ((hue + 240) % 360) as f32;
    if s <= f32::EPSILON {
        let grey = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return [grey, grey, grey];
    }
    let chroma = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let sector = h / 60.0;
    let secondary = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (r, g, b) = match sector as u32 {
        0 => (chroma, secondary, 0.0),
        1 => (secondary, chroma, 0.0),
        2 => (0.0, chroma, secondary),
        3 => (0.0, secondary, chroma),
        4 => (secondary, 0.0, chroma),
        _ => (chroma, 0.0, secondary),
    };
    let base = l - chroma / 2.0;
    [
        (((r + base) * 255.0).round()).clamp(0.0, 255.0) as u8,
        (((g + base) * 255.0).round()).clamp(0.0, 255.0) as u8,
        (((b + base) * 255.0).round()).clamp(0.0, 255.0) as u8,
    ]
}

/// Reads a `;`-separated parameter list, stopping at the first byte that is not part of
/// one. Returns the values and the index to carry on from.
fn read_params(data: &[u8], mut index: usize) -> (Vec<u32>, usize) {
    let mut params = Vec::new();
    loop {
        let (value, next) = read_number(data, index);
        params.push(value);
        index = next;
        // Bounded so a parameter list of a hundred thousand semicolons cannot make this
        // allocate: no instruction here reads more than five.
        if data.get(index) == Some(&b';') && params.len() < 8 {
            index += 1;
            continue;
        }
        break;
    }
    (params, index)
}

/// Reads a decimal number, saturating rather than wrapping, and returns where it ended.
fn read_number(data: &[u8], mut index: usize) -> (u32, usize) {
    let mut value: u32 = 0;
    while let Some(byte) = data.get(index).copied().filter(u8::is_ascii_digit) {
        value = value
            .saturating_mul(10)
            .saturating_add((byte - b'0') as u32);
        index += 1;
    }
    (value, index)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest real picture: two columns, six rows, all set, in register 1.
    #[test]
    fn a_sixel_of_one_colour_becomes_the_pixels_it_describes() {
        // `#1` selects blue, `~` is 0x7E, all six bits set.
        let raster = decode(b"#1~~", 1_000_000).expect("it decodes");
        assert_eq!((raster.width, raster.height), (2, 6));
        for y in 0..6 {
            for x in 0..2 {
                assert_eq!(
                    pixel(&raster, x, y),
                    [51, 51, 204, 255],
                    "pixel ({x}, {y}) is not the VT340's blue"
                );
            }
        }
    }

    /// The bit order is the whole format: bit zero is the top pixel of the band.
    #[test]
    fn the_bits_of_a_sixel_character_run_top_to_bottom() {
        // 0x3F + 1 = '@' sets only the top pixel; 0x3F + 32 = '_' sets only the bottom.
        let top = decode(b"#2@", 1_000).expect("it decodes");
        assert_eq!((top.width, top.height), (1, 1));
        assert_eq!(pixel(&top, 0, 0)[3], 255);

        let bottom = decode(b"#2_", 1_000).expect("it decodes");
        assert_eq!((bottom.width, bottom.height), (1, 6));
        for y in 0..5 {
            assert_eq!(pixel(&bottom, 0, y)[3], 0, "row {y} must be transparent");
        }
        assert_eq!(pixel(&bottom, 0, 5), [204, 33, 33, 255]);
    }

    #[test]
    fn a_graphics_newline_starts_the_next_band_six_rows_down() {
        let raster = decode(b"#1~-#3~", 1_000_000).expect("it decodes");
        assert_eq!((raster.width, raster.height), (1, 12));
        assert_eq!(pixel(&raster, 0, 0), [51, 51, 204, 255]);
        assert_eq!(pixel(&raster, 0, 6), [51, 204, 51, 255], "the second band");
    }

    #[test]
    fn a_carriage_return_draws_the_next_colour_over_the_same_columns() {
        // Blue across two columns, back to the start, then green over the top row only.
        let raster = decode(b"#1~~$#3@@", 1_000_000).expect("it decodes");
        assert_eq!((raster.width, raster.height), (2, 6));
        assert_eq!(
            pixel(&raster, 0, 0),
            [51, 204, 51, 255],
            "green won the top row"
        );
        assert_eq!(
            pixel(&raster, 0, 1),
            [51, 51, 204, 255],
            "blue kept the rest"
        );
    }

    #[test]
    fn a_colour_definition_in_rgb_percent_is_the_colour_it_asked_for() {
        // Register 9 defined as 100% red, 50% green, 0% blue, then used.
        let raster = decode(b"#9;2;100;50;0#9~", 1_000).expect("it decodes");
        assert_eq!(pixel(&raster, 0, 0), [255, 128, 0, 255]);
    }

    /// DEC's hue is offset by 240 degrees. A decoder that used the usual convention would
    /// draw every plot in its complementary colour.
    #[test]
    fn a_colour_definition_in_hls_uses_decs_hue_where_zero_is_blue() {
        let raster = decode(b"#5;1;0;50;100#5~", 1_000).expect("it decodes");
        let [r, g, b, _] = pixel(&raster, 0, 0);
        assert!(
            b > 200 && r < 40 && g < 40,
            "hue zero must be blue, got ({r}, {g}, {b})"
        );

        // 120 degrees is DEC's red.
        let red = decode(b"#5;1;120;50;100#5~", 1_000).expect("it decodes");
        let [r, g, b, _] = pixel(&red, 0, 0);
        assert!(r > 200 && g < 40 && b < 40, "got ({r}, {g}, {b})");

        // Zero saturation is grey whatever the hue.
        let grey = decode(b"#5;1;77;50;0#5~", 1_000).expect("it decodes");
        let [r, g, b, _] = pixel(&grey, 0, 0);
        assert_eq!((r, g), (b, b));
    }

    #[test]
    fn run_length_repetition_draws_the_same_column_many_times() {
        let raster = decode(b"#1!20~", 1_000_000).expect("it decodes");
        assert_eq!((raster.width, raster.height), (20, 6));
        assert_eq!(pixel(&raster, 19, 5), [51, 51, 204, 255]);
    }

    /// The dangerous instruction. Seven bytes must not be able to ask for a million
    /// pixels' worth of work, so the count is clamped to the row.
    #[test]
    fn an_absurd_repeat_count_is_clamped_to_the_row_rather_than_believed() {
        let raster = decode(b"#1!4000000000~", 1_000_000).expect("it decodes");
        assert!(
            raster.width <= MAX_SIXEL_WIDTH,
            "the canvas grew to {} columns",
            raster.width
        );
        assert!(raster.pixels() <= 1_000_000, "{} pixels", raster.pixels());
    }

    /// The bomb, declared rather than compressed: raster attributes claiming an enormous
    /// picture must be refused before a canvas exists.
    #[test]
    fn declared_raster_attributes_past_the_budget_are_refused_before_anything_is_drawn() {
        let error = decode(b"\"1;1;60000;60000#1~", 1_048_576).expect_err("refused");
        match error {
            SixelError::TooLarge { pixels, .. } => assert_eq!(pixels, 3_600_000_000),
            other => panic!("expected a size refusal, got {other}"),
        }
    }

    /// And undeclared: a Sixel that just keeps drawing must stop at the budget rather than
    /// at the machine's memory — and be refused rather than cropped, because a picture
    /// missing its right-hand side with no explanation is worse than no picture.
    #[test]
    fn an_undeclared_sixel_that_keeps_drawing_is_refused_at_the_budget_rather_than_cropped() {
        // Two hundred bands of a thousand columns is 1.2 million pixels; the budget is a
        // tenth of that.
        let mut body = Vec::from(b"#1".as_slice());
        for _ in 0..200 {
            body.extend_from_slice(b"!1000~-");
        }
        assert_eq!(decode(&body, 120_000), Err(SixelError::OverBudget));
        // With room for it, the same picture decodes whole.
        let raster = decode(&body, 4_000_000).expect("it fits");
        assert_eq!((raster.width, raster.height), (1_000, 1_200));
    }

    /// A Sixel that draws past the widest canvas is refused for the same reason.
    #[test]
    fn a_sixel_that_draws_past_the_canvas_caps_is_refused_rather_than_cropped() {
        let mut body = Vec::from(b"#1".as_slice());
        // One band, wider than the cap, without using run-length so nothing clamps it.
        body.extend(std::iter::repeat_n(b'~', MAX_SIXEL_WIDTH as usize + 16));
        assert_eq!(
            decode(&body, u64::MAX),
            Err(SixelError::PastCanvas {
                width: MAX_SIXEL_WIDTH,
                height: MAX_SIXEL_HEIGHT
            })
        );
    }

    #[test]
    fn a_sixel_that_sets_no_pixels_is_refused_rather_than_placed_as_nothing() {
        assert_eq!(decode(b"", 1_000), Err(SixelError::Empty));
        assert_eq!(decode(b"#1", 1_000), Err(SixelError::Empty));
        // `?` is 0x3F: a sixel character with no bits set.
        assert_eq!(decode(b"#1????", 1_000), Err(SixelError::Empty));
        assert_eq!(decode(b"$$---", 1_000), Err(SixelError::Empty));
    }

    /// Every byte in the range is a valid instruction or ignorable, so no input can panic.
    #[test]
    fn no_byte_sequence_at_all_can_make_the_decoder_panic() {
        // Every single byte on its own.
        for byte in 0..=255u8 {
            let _ = decode(&[byte], 10_000);
            let _ = decode(&[b'#', byte], 10_000);
            let _ = decode(&[b'!', byte], 10_000);
            let _ = decode(&[b'"', byte], 10_000);
        }
        // A pseudo-random soup, deterministic so a failure is reproducible.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut soup = Vec::with_capacity(20_000);
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            soup.push((state & 0xFF) as u8);
        }
        let _ = decode(&soup, 200_000);
        // And a parameter list of nothing but separators, which must not allocate per one.
        let semicolons = vec![b';'; 100_000];
        let mut body = Vec::from(b"#1".as_slice());
        body.extend_from_slice(&semicolons);
        let _ = decode(&body, 10_000);
    }

    /// The realistic case, end to end: what `img2sixel` emits for a small block of
    /// colour, raster attributes and all.
    #[test]
    fn a_sixel_of_the_shape_img2sixel_emits_decodes_to_the_right_picture() {
        // 4x6 red block: raster attributes, one colour definition, one run.
        let body = b"\"1;1;4;6#0;2;100;0;0!4~";
        let raster = decode(body, 1_048_576).expect("it decodes");
        assert_eq!((raster.width, raster.height), (4, 6));
        for y in 0..6 {
            for x in 0..4 {
                assert_eq!(pixel(&raster, x, y), [255, 0, 0, 255]);
            }
        }
    }

    #[test]
    fn a_pixel_no_sixel_set_is_transparent_so_the_pane_shows_through() {
        // Two columns, only the first drawn.
        let raster = decode(b"#1~?", 1_000).expect("it decodes");
        assert_eq!(
            raster.width, 1,
            "an undrawn trailing column is cropped away"
        );

        // And a hole in the middle stays a hole.
        let holed = decode(b"#1~?~", 1_000).expect("it decodes");
        assert_eq!(holed.width, 3);
        assert_eq!(
            pixel(&holed, 1, 0)[3],
            0,
            "the middle column is transparent"
        );
        assert_eq!(pixel(&holed, 2, 0)[3], 255);
    }

    #[test]
    fn a_register_never_defined_draws_in_the_colour_its_author_expected() {
        // The VT340 palette, so a file that selects register 3 gets green.
        let raster = decode(b"#3~", 1_000).expect("it decodes");
        assert_eq!(pixel(&raster, 0, 0), [51, 204, 51, 255]);
        // And a register past the 256 wraps rather than indexing out of range.
        assert!(decode(b"#9999~", 1_000).is_ok());
    }

    fn pixel(raster: &Raster, x: u32, y: u32) -> [u8; 4] {
        let base = (y as usize * raster.width as usize + x as usize) * 4;
        let slice = raster
            .rgba
            .get(base..base + 4)
            .unwrap_or_else(|| panic!("({x}, {y}) is outside the raster"));
        [slice[0], slice[1], slice[2], slice[3]]
    }
}
