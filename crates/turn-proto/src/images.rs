//! Inline images: how a pixel payload is anchored to cells, and how it travels.
//!
//! Three protocols put pictures in a terminal — iTerm2's `OSC 1337 File=`, Sixel, and
//! the Kitty graphics protocol — and all three describe the same thing: a raster that
//! occupies a rectangle of character cells at the position the cursor happened to be.
//! An image is therefore neither a cell nor a screen. It is a payload plus a placement,
//! and the two have completely different lifetimes.
//!
//! ## Placement lives in the cells
//!
//! The hard part of inline images is not decoding them. It is that once an image is on
//! screen, **every single thing the terminal does to text has to happen to it too**: it
//! must scroll with the rows above it, vanish when the screen is cleared, survive a
//! partial overwrite by being partially erased, move when a line is inserted, and be
//! gone when the alternate screen takes over.
//!
//! Reimplementing all of that for a side table of rectangles means reimplementing a
//! terminal, badly. So Turn does not keep a side table of rectangles: **each cell an
//! image covers carries a marker character** in the private-use plane, encoding which
//! image it belongs to and which tile of that image it is
//! ([`ImageCell`]). The marker is stored in the cell like any other character, which
//! means the terminal parser moves it, clears it and overwrites it with exactly the
//! rules it already has. Scrollback works. `clear` works. A program printing over the
//! middle of an image punches a hole in it, which is what a real terminal does.
//!
//! Nothing else in the grid needs to know: a marker cell is one column wide, it is not
//! blank, and it round-trips through the run encoding as text. The only readers that
//! care are the painter, which draws a tile instead of a glyph, and anything that turns
//! cells into text, which substitutes a space.
//!
//! ## Payload does not live in the grid
//!
//! A grid crosses the socket many times a second. A megabyte of pixels must not. So
//! [`Grid::images`](crate::cells::Grid::images) carries only the small side table that
//! maps a marker's slot to an [`ImageId`] and the geometry needed to lay the image out,
//! and the pixels are fetched once per image with
//! [`Request::PaneImage`](crate::Request::PaneImage). A client caches them by id, so a
//! screen that scrolls thirty times sends thirty cheap row diffs and no pixels at all,
//! and a client that re-attaches asks only for the images it does not already hold.
//!
//! ## Slots, and why there are only eight
//!
//! A marker has to fit in one character. The private-use plane 16 gives 65,534 usable
//! code points, and a marker must encode a slot, a tile row and a tile column, so the
//! three bounds multiply out to the budget: [`MAX_PLACED_IMAGES`] slots of
//! [`MAX_IMAGE_CELL_ROWS`] by [`MAX_IMAGE_CELL_COLS`] tiles is 65,024 markers. A ninth
//! image on one screen reuses the oldest slot, and the daemon erases that slot's
//! remaining cells first so a stale marker can never point at the wrong picture.
//!
//! ## Everything here is bounded, because it is all untrusted
//!
//! Every byte described in this module was written by a process. A payload arriving off
//! the wire is checked before anything is allocated for it: the pixel count against
//! [`MAX_IMAGE_PIXELS`], the byte length against the pixel count, the slot against the
//! slot budget, the tile against the image's own size. A structurally impossible image
//! is refused rather than repaired, for the same reason a structurally impossible row
//! is — a protocol that quietly fixes its own input is one whose two implementations
//! will eventually disagree.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::bytes::TerminalBytes;

/// How many images may be placed on one screen at the same time.
///
/// A bound on *markers*, not on how many images a pane may show over its life: the
/// number is what the private-use plane can address alongside the tile coordinates.
/// Placing a ninth reuses the least recently placed slot.
pub const MAX_PLACED_IMAGES: usize = 8;

/// Tallest an image may be, in cells.
pub const MAX_IMAGE_CELL_ROWS: u16 = 64;

/// Widest an image may be, in cells.
///
/// 127 rather than 128 so the marker alphabet stays inside plane 16 without touching
/// its two noncharacters at the very top.
pub const MAX_IMAGE_CELL_COLS: u16 = 127;

/// Most pixels a decoded image may contain, one mebipixel.
///
/// Two limits meet here. Four bytes a pixel makes this 4 MiB of RGBA, which base64
/// takes to 5.6 MB — inside [`MAX_LINE_BYTES`](crate::framing::MAX_LINE_BYTES) with
/// room for the envelope, so one image is always one frame. And a cell box of
/// [`MAX_IMAGE_CELL_COLS`] by [`MAX_IMAGE_CELL_ROWS`] is around a megapixel at any
/// plausible cell size, so an image with more detail than this has nowhere to put it.
pub const MAX_IMAGE_PIXELS: u32 = 1_048_576;

/// Bytes per pixel in a payload: RGBA, unassociated alpha, sRGB.
///
/// One layout rather than a negotiated set. The decoders all normalise to it, and a
/// renderer that has to branch on pixel format is a renderer with a bug in the branch
/// nobody exercises.
pub const BYTES_PER_PIXEL: usize = 4;

/// First marker code point, in private-use plane 16.
///
/// Plane 16 rather than the Basic Multilingual Plane's private-use area, which real
/// fonts and real programs do use — Nerd Font glyphs live there, and a marker that
/// collided with one would turn somebody's prompt into a picture.
pub const MARKER_FIRST: char = '\u{100000}';

/// How many marker code points the alphabet uses.
pub const MARKER_COUNT: u32 =
    MAX_PLACED_IMAGES as u32 * MAX_IMAGE_CELL_ROWS as u32 * MAX_IMAGE_CELL_COLS as u32;

/// Last marker code point, inclusive.
pub const MARKER_LAST: char = {
    // `char::from_u32` is not const, and the arithmetic is fixed at compile time, so the
    // conversion is written out and checked by
    // `the_marker_alphabet_stays_inside_plane_sixteen`.
    match char::from_u32(MARKER_FIRST as u32 + MARKER_COUNT - 1) {
        Some(c) => c,
        None => MARKER_FIRST,
    }
};

/// Identity of an image payload, derived from its contents.
///
/// Content-derived rather than a counter so that the same picture printed twice is one
/// payload, a client's cache survives a re-attach, and a stale id can never name a
/// different image than the one it was fetched for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImageId(pub u64);

impl ImageId {
    /// The id of a decoded raster.
    ///
    /// FNV-1a over the dimensions and the pixels. Not a cryptographic hash and not
    /// claiming to be: the only thing riding on it is a per-pane cache lookup, and the
    /// process supplying the pixels gains nothing by colliding with itself.
    pub fn of(width: u32, height: u32, pixels: &[u8]) -> Self {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash = OFFSET;
        let mut mix = |byte: u8| {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(PRIME);
        };
        for byte in width.to_le_bytes() {
            mix(byte);
        }
        for byte in height.to_le_bytes() {
            mix(byte);
        }
        for byte in pixels {
            mix(*byte);
        }
        Self(hash)
    }
}

impl std::fmt::Display for ImageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "img_{:016x}", self.0)
    }
}

/// Which image a cell belongs to, and which tile of it the cell shows.
///
/// One of these is encoded into every cell an image covers. `dy`/`dx` are the tile's
/// position inside the image, so a screen that has scrolled an image half off the top
/// still says exactly which part of it each surviving row holds — no arithmetic against
/// a remembered anchor, which is the thing that goes wrong when the anchor moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageCell {
    /// Which of the screen's [`MAX_PLACED_IMAGES`] slots.
    pub slot: u8,
    /// Tile row inside the image.
    pub dy: u16,
    /// Tile column inside the image.
    pub dx: u16,
}

impl ImageCell {
    pub fn new(slot: u8, dy: u16, dx: u16) -> Self {
        Self { slot, dy, dx }
    }

    /// Whether this is a tile a marker can name at all.
    pub fn is_addressable(&self) -> bool {
        (self.slot as usize) < MAX_PLACED_IMAGES
            && self.dy < MAX_IMAGE_CELL_ROWS
            && self.dx < MAX_IMAGE_CELL_COLS
    }

    /// The marker character for this tile, or `None` when it is outside the alphabet.
    ///
    /// `None` rather than a clamped marker: a clamped one would name a different tile,
    /// and drawing the wrong part of a picture is worse than drawing none of it.
    pub fn to_marker(self) -> Option<char> {
        if !self.is_addressable() {
            return None;
        }
        let index = (self.slot as u32 * MAX_IMAGE_CELL_ROWS as u32 + self.dy as u32)
            * MAX_IMAGE_CELL_COLS as u32
            + self.dx as u32;
        char::from_u32(MARKER_FIRST as u32 + index)
    }

    /// The tile a marker names, or `None` for any other character.
    pub fn from_marker(marker: char) -> Option<Self> {
        let index = (marker as u32).checked_sub(MARKER_FIRST as u32)?;
        if index >= MARKER_COUNT {
            return None;
        }
        let per_slot = MAX_IMAGE_CELL_ROWS as u32 * MAX_IMAGE_CELL_COLS as u32;
        Some(Self {
            slot: (index / per_slot) as u8,
            dy: ((index % per_slot) / MAX_IMAGE_CELL_COLS as u32) as u16,
            dx: (index % MAX_IMAGE_CELL_COLS as u32) as u16,
        })
    }
}

/// Whether a character is one of Turn's image markers.
pub fn is_marker(c: char) -> bool {
    ImageCell::from_marker(c).is_some()
}

/// Whether a cell's text is exactly one image marker, and which tile it is.
///
/// Takes the cell's whole text rather than a character because a cell holds a string:
/// a marker is one character and nothing else, so a cell carrying a marker followed by
/// anything is not an image cell.
pub fn marker_of(text: &str) -> Option<ImageCell> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(single), None) => ImageCell::from_marker(single),
        _ => None,
    }
}

/// One image placed on a screen: which slot its markers use, and how to lay it out.
///
/// Carries no pixels. `rows`/`cols` are the cell box the image was given, and
/// `width`/`height` are the payload's own pixel size — both are needed, because the box
/// is decided by the daemon from the escape sequence and the aspect ratio can only be
/// honoured by whoever knows how big a cell actually is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridImage {
    /// The slot this image's markers carry.
    pub slot: u8,
    pub id: ImageId,
    /// Height of the cell box, in rows.
    pub rows: u16,
    /// Width of the cell box, in columns.
    pub cols: u16,
    /// Payload width in pixels.
    pub width: u32,
    /// Payload height in pixels.
    pub height: u32,
    /// Whether the image must keep its aspect ratio inside the box.
    ///
    /// The default for every one of the three protocols, and the reason the client does
    /// the fitting: only the client knows the pixel size of a cell.
    /// The default when the field is absent is `true`, not `false`: every one of the
    /// three protocols preserves aspect ratio unless told otherwise, so the flag is
    /// omitted in the common case and a receiver must read the omission as "preserve".
    #[serde(default = "preserve_by_default", skip_serializing_if = "is_true")]
    pub preserve_aspect: bool,
}

fn is_true(value: &bool) -> bool {
    *value
}

fn preserve_by_default() -> bool {
    true
}

impl GridImage {
    /// A placement with the conventional defaults.
    pub fn new(slot: u8, id: ImageId, rows: u16, cols: u16, width: u32, height: u32) -> Self {
        Self {
            slot,
            id,
            rows,
            cols,
            width,
            height,
            preserve_aspect: true,
        }
    }

    /// Whether this placement is one a screen could actually hold.
    ///
    /// Checked on the way in, before a client indexes anything by it.
    pub fn is_valid(&self) -> bool {
        (self.slot as usize) < MAX_PLACED_IMAGES
            && (1..=MAX_IMAGE_CELL_ROWS).contains(&self.rows)
            && (1..=MAX_IMAGE_CELL_COLS).contains(&self.cols)
            && self.width > 0
            && self.height > 0
            && self.width.saturating_mul(self.height) <= MAX_IMAGE_PIXELS
    }
}

/// The pixels of one image.
///
/// Raw RGBA rather than the format the process sent. The daemon has already decoded it —
/// it must, to know how many cells the image occupies — and shipping the original bytes
/// would mean decoding untrusted input twice, in two crates, with two sets of bounds.
/// The cost is honest: base64 of raw RGBA is the largest thing this protocol carries, so
/// it is fetched once per image and cached by id rather than pushed with the screen.
#[derive(Clone, PartialEq, Eq)]
pub struct ImagePayload {
    pub id: ImageId,
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major, no padding.
    pub pixels: TerminalBytes,
}

/// Why a payload could not be accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    #[error("an image of {width}x{height} is {pixels} pixels, over the limit of {max}")]
    TooManyPixels {
        width: u32,
        height: u32,
        pixels: u64,
        max: u32,
    },
    #[error("an image of {width}x{height} needs {expected} bytes but carries {actual}")]
    WrongLength {
        width: u32,
        height: u32,
        expected: usize,
        actual: usize,
    },
    #[error("an image of {width}x{height} has no pixels")]
    Empty { width: u32, height: u32 },
    #[error("the id {claimed} does not match the {computed} these pixels hash to")]
    WrongId { claimed: ImageId, computed: ImageId },
    #[error("slot {slot} is outside the {max} slots a screen has")]
    BadSlot { slot: u8, max: usize },
    #[error("a placement of {rows}x{cols} cells is not one a screen can hold")]
    BadBox { rows: u16, cols: u16 },
    #[error("slot {slot} is claimed by two placements")]
    DuplicateSlot { slot: u8 },
    #[error("a screen may place {max} images, not {count}")]
    TooManyPlacements { count: usize, max: usize },
}

impl ImagePayload {
    /// Builds a payload from decoded pixels, checking the shape before it is trusted.
    ///
    /// The id is computed here rather than accepted, so there is one definition of what
    /// a payload's identity is and a caller cannot mislabel one.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, ImageError> {
        check_dimensions(width, height)?;
        let expected = expected_bytes(width, height);
        if pixels.len() != expected {
            return Err(ImageError::WrongLength {
                width,
                height,
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            id: ImageId::of(width, height, &pixels),
            width,
            height,
            pixels: TerminalBytes::new(pixels),
        })
    }

    /// How many bytes of RGBA this payload holds.
    pub fn byte_len(&self) -> usize {
        self.pixels.len()
    }

    pub fn pixels(&self) -> &[u8] {
        self.pixels.as_slice()
    }
}

/// Refuses a geometry before anything is allocated for it.
///
/// The decompression-bomb check, and the reason it takes dimensions rather than bytes: a
/// thirty-byte PNG can declare 60,000 by 60,000, which is fourteen gigabytes of RGBA. The
/// only safe order is to read the header, check the numbers, and *then* decide whether to
/// allocate.
pub fn check_dimensions(width: u32, height: u32) -> Result<(), ImageError> {
    if width == 0 || height == 0 {
        return Err(ImageError::Empty { width, height });
    }
    let pixels = width as u64 * height as u64;
    if pixels > MAX_IMAGE_PIXELS as u64 {
        return Err(ImageError::TooManyPixels {
            width,
            height,
            pixels,
            max: MAX_IMAGE_PIXELS,
        });
    }
    Ok(())
}

/// How many bytes an image of this size occupies, saturating rather than wrapping.
///
/// Saturating matters: on a 32-bit target `width * height * 4` for a refused geometry
/// would wrap to a small number and make a length check pass.
pub fn expected_bytes(width: u32, height: u32) -> usize {
    (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(BYTES_PER_PIXEL)
}

/// Checks a screen's whole image table.
///
/// Called when a grid arrives off the wire, before a renderer indexes anything by slot.
pub fn check_table(images: &[GridImage]) -> Result<(), ImageError> {
    if images.len() > MAX_PLACED_IMAGES {
        return Err(ImageError::TooManyPlacements {
            count: images.len(),
            max: MAX_PLACED_IMAGES,
        });
    }
    let mut seen = [false; MAX_PLACED_IMAGES];
    for image in images {
        let slot = image.slot as usize;
        if slot >= MAX_PLACED_IMAGES {
            return Err(ImageError::BadSlot {
                slot: image.slot,
                max: MAX_PLACED_IMAGES,
            });
        }
        if seen[slot] {
            return Err(ImageError::DuplicateSlot { slot: image.slot });
        }
        seen[slot] = true;
        if !(1..=MAX_IMAGE_CELL_ROWS).contains(&image.rows)
            || !(1..=MAX_IMAGE_CELL_COLS).contains(&image.cols)
        {
            return Err(ImageError::BadBox {
                rows: image.rows,
                cols: image.cols,
            });
        }
        check_dimensions(image.width, image.height)?;
    }
    Ok(())
}

/// The payload as it is written and read: dimensions, then base64 pixels.
#[derive(Serialize, Deserialize)]
struct PayloadWire {
    id: ImageId,
    width: u32,
    height: u32,
    pixels: TerminalBytes,
}

impl Serialize for ImagePayload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        PayloadWire {
            id: self.id,
            width: self.width,
            height: self.height,
            pixels: self.pixels.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ImagePayload {
    /// Strict: the dimensions are checked before the length, and the length before the
    /// id, so a peer cannot make a receiver allocate for a geometry it would then
    /// refuse — and cannot hand over pixels under a name that is not theirs.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = PayloadWire::deserialize(deserializer)?;
        check_dimensions(wire.width, wire.height).map_err(D::Error::custom)?;
        let expected = expected_bytes(wire.width, wire.height);
        if wire.pixels.len() != expected {
            return Err(D::Error::custom(ImageError::WrongLength {
                width: wire.width,
                height: wire.height,
                expected,
                actual: wire.pixels.len(),
            }));
        }
        let computed = ImageId::of(wire.width, wire.height, wire.pixels.as_slice());
        if computed != wire.id {
            return Err(D::Error::custom(ImageError::WrongId {
                claimed: wire.id,
                computed,
            }));
        }
        Ok(Self {
            id: wire.id,
            width: wire.width,
            height: wire.height,
            pixels: wire.pixels,
        })
    }
}

impl std::fmt::Debug for ImagePayload {
    /// Shows the shape, never the pixels. A payload is megabytes and a `Debug` of one in
    /// a failing assertion or a `tracing` field would be unreadable at best.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ImagePayload({}, {}x{}, {} bytes)",
            self.id,
            self.width,
            self.height,
            self.pixels.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget the whole marker design rests on. If the three bounds ever multiply out
    /// past the plane, some tile would be unaddressable and the image containing it would
    /// silently lose a cell.
    #[test]
    fn the_marker_alphabet_stays_inside_plane_sixteen() {
        assert_eq!(MARKER_COUNT, 8 * 64 * 127);
        assert_eq!(MARKER_COUNT, 65_024);
        const { assert!(MARKER_COUNT <= 65_534, "plane 16 has 65,534 usable points") };
        assert_eq!(MARKER_FIRST as u32, 0x100000);
        assert_ne!(MARKER_LAST, MARKER_FIRST, "the last point must exist");
        assert!(
            (MARKER_LAST as u32) < 0x10FFFE,
            "the two noncharacters at the top of the plane must stay unused"
        );
        // And nothing in the alphabet is a character a font would draw.
        for offset in [0u32, 1, MARKER_COUNT / 2, MARKER_COUNT - 1] {
            let c = char::from_u32(MARKER_FIRST as u32 + offset).expect("a marker exists");
            assert!(!c.is_control());
            assert!(is_marker(c));
        }
    }

    /// Every tile a placement can name must survive the round trip, or a picture would
    /// come out with its tiles in the wrong order.
    #[test]
    fn every_addressable_tile_round_trips_through_its_marker() {
        for slot in 0..MAX_PLACED_IMAGES as u8 {
            for dy in [0u16, 1, MAX_IMAGE_CELL_ROWS - 1] {
                for dx in [0u16, 1, MAX_IMAGE_CELL_COLS - 1] {
                    let cell = ImageCell::new(slot, dy, dx);
                    let marker = cell.to_marker().expect("an addressable tile has a marker");
                    assert_eq!(
                        ImageCell::from_marker(marker),
                        Some(cell),
                        "{cell:?} did not survive"
                    );
                }
            }
        }
        // Exhaustively, because an off-by-one in the packing would only show up for one
        // slot at one edge.
        let mut seen = std::collections::HashSet::new();
        for slot in 0..MAX_PLACED_IMAGES as u8 {
            for dy in 0..MAX_IMAGE_CELL_ROWS {
                for dx in 0..MAX_IMAGE_CELL_COLS {
                    let marker = ImageCell::new(slot, dy, dx)
                        .to_marker()
                        .expect("every tile is addressable");
                    assert!(seen.insert(marker), "two tiles share a marker");
                }
            }
        }
        assert_eq!(seen.len(), MARKER_COUNT as usize);
    }

    #[test]
    fn a_tile_outside_the_budget_has_no_marker_rather_than_a_clamped_one() {
        assert!(ImageCell::new(MAX_PLACED_IMAGES as u8, 0, 0)
            .to_marker()
            .is_none());
        assert!(ImageCell::new(0, MAX_IMAGE_CELL_ROWS, 0)
            .to_marker()
            .is_none());
        assert!(ImageCell::new(0, 0, MAX_IMAGE_CELL_COLS)
            .to_marker()
            .is_none());
        assert!(!ImageCell::new(0, 0, MAX_IMAGE_CELL_COLS).is_addressable());
    }

    /// Ordinary text must never be mistaken for an image, including the private-use area
    /// real fonts use for icons.
    #[test]
    fn ordinary_characters_are_never_read_as_markers() {
        for c in [
            'a', ' ', '─', '漢', '\u{e0b0}', '\u{f8ff}', '\u{fffd}', '\0',
        ] {
            assert!(!is_marker(c), "{c:?} was read as an image marker");
        }
        // The code point immediately below and immediately above the alphabet.
        assert!(!is_marker('\u{fffff}'));
        let past = char::from_u32(MARKER_FIRST as u32 + MARKER_COUNT).expect("it exists");
        assert!(!is_marker(past));
    }

    #[test]
    fn a_cell_holding_a_marker_and_anything_else_is_not_an_image_cell() {
        let marker = ImageCell::new(1, 2, 3).to_marker().expect("a marker");
        assert_eq!(
            marker_of(&marker.to_string()),
            Some(ImageCell::new(1, 2, 3))
        );
        assert_eq!(marker_of(&format!("{marker}x")), None);
        assert_eq!(marker_of(""), None);
        assert_eq!(marker_of("a"), None);
    }

    /// The bomb check. A tiny header claiming an enormous raster must be refused by the
    /// numbers, before a single byte is reserved for it.
    #[test]
    fn an_impossible_geometry_is_refused_from_its_dimensions_alone() {
        let error = check_dimensions(60_000, 60_000).expect_err("3.6 gigapixels is refused");
        assert!(matches!(error, ImageError::TooManyPixels { .. }));
        assert!(error.to_string().contains("limit"), "got {error}");
        // The multiplication must not wrap on the way to the comparison.
        assert!(check_dimensions(u32::MAX, u32::MAX).is_err());
        assert!(check_dimensions(0, 10).is_err());
        assert!(check_dimensions(10, 0).is_err());
        assert!(check_dimensions(1024, 1024).is_ok());
        assert!(check_dimensions(1025, 1024).is_err());
    }

    #[test]
    fn the_byte_length_of_a_refused_geometry_saturates_rather_than_wrapping() {
        assert_eq!(expected_bytes(2, 3), 24);
        assert_eq!(expected_bytes(u32::MAX, u32::MAX), usize::MAX);
    }

    #[test]
    fn a_payload_whose_length_disagrees_with_its_dimensions_is_refused() {
        assert!(ImagePayload::new(2, 2, vec![0; 16]).is_ok());
        let error = ImagePayload::new(2, 2, vec![0; 15]).expect_err("one byte short");
        assert!(matches!(error, ImageError::WrongLength { .. }));
        assert!(ImagePayload::new(0, 2, Vec::new()).is_err());
    }

    #[test]
    fn a_payload_round_trips_through_json_with_its_pixels_intact() {
        let pixels: Vec<u8> = (0..64u32).map(|i| i as u8).collect();
        let payload = ImagePayload::new(4, 4, pixels.clone()).expect("a 4x4 image");
        let json = serde_json::to_string(&payload).expect("it serialises");
        let back: ImagePayload = serde_json::from_str(&json).expect("and reads back");
        assert_eq!(back, payload);
        assert_eq!(back.pixels(), pixels.as_slice());
        assert_eq!(back.byte_len(), 64);
    }

    /// The same three checks as [`ImagePayload::new`], but on the path an adversary
    /// controls: a frame off the socket.
    #[test]
    fn a_payload_off_the_wire_is_refused_before_it_is_believed() {
        let good = ImagePayload::new(2, 2, vec![7; 16]).expect("a 2x2 image");
        let json = serde_json::to_string(&good).expect("it serialises");

        // A geometry no allocation should be attempted for.
        let bomb = json.replace(
            "\"width\":2,\"height\":2",
            "\"width\":60000,\"height\":60000",
        );
        let error = serde_json::from_str::<ImagePayload>(&bomb).expect_err("refused");
        assert!(error.to_string().contains("limit"), "got {error}");

        // Pixels that do not fill the geometry.
        let short = format!(
            "{{\"id\":{},\"width\":2,\"height\":2,\"pixels\":\"{}\"}}",
            good.id.0,
            crate::encode_base64(&[7; 12])
        );
        let error = serde_json::from_str::<ImagePayload>(&short).expect_err("refused");
        assert!(error.to_string().contains("bytes"), "got {error}");

        // Pixels under somebody else's name, which would poison a client's cache.
        let mistagged = json.replace(&good.id.0.to_string(), "12345");
        let error = serde_json::from_str::<ImagePayload>(&mistagged).expect_err("refused");
        assert!(error.to_string().contains("does not match"), "got {error}");
    }

    #[test]
    fn an_id_is_the_contents_so_the_same_picture_twice_is_one_payload() {
        let a = ImagePayload::new(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]).expect("an image");
        let b = ImagePayload::new(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]).expect("the same image");
        assert_eq!(a.id, b.id);

        let different = ImagePayload::new(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 9]).expect("another");
        assert_ne!(a.id, different.id);
        // The dimensions are part of the identity, or a 1x2 and a 2x1 of the same bytes
        // would collide.
        let rotated = ImagePayload::new(1, 2, vec![1, 2, 3, 4, 5, 6, 7, 8]).expect("rotated");
        assert_ne!(a.id, rotated.id);
        assert!(a.id.to_string().starts_with("img_"));
    }

    #[test]
    fn a_table_with_two_placements_in_one_slot_is_refused() {
        let id = ImageId(1);
        let one = GridImage::new(0, id, 4, 8, 64, 32);
        assert!(check_table(&[one]).is_ok());
        assert!(matches!(
            check_table(&[one, one]),
            Err(ImageError::DuplicateSlot { slot: 0 })
        ));

        let too_many: Vec<GridImage> = (0..=MAX_PLACED_IMAGES as u8)
            .map(|slot| GridImage::new(slot, id, 1, 1, 1, 1))
            .collect();
        assert!(matches!(
            check_table(&too_many),
            Err(ImageError::TooManyPlacements { .. })
        ));

        assert!(matches!(
            check_table(&[GridImage::new(MAX_PLACED_IMAGES as u8, id, 1, 1, 1, 1)]),
            Err(ImageError::BadSlot { .. })
        ));
        assert!(matches!(
            check_table(&[GridImage::new(0, id, 0, 8, 4, 4)]),
            Err(ImageError::BadBox { rows: 0, cols: 8 })
        ));
        assert!(matches!(
            check_table(&[GridImage::new(0, id, 1, MAX_IMAGE_CELL_COLS + 1, 4, 4)]),
            Err(ImageError::BadBox { .. })
        ));
    }

    #[test]
    fn a_placement_reports_whether_it_is_one_a_screen_could_hold() {
        let id = ImageId(9);
        assert!(GridImage::new(0, id, 4, 8, 64, 32).is_valid());
        assert!(!GridImage::new(0, id, 4, 8, 0, 32).is_valid());
        assert!(!GridImage::new(0, id, 4, 8, 60_000, 60_000).is_valid());
        assert!(!GridImage::new(0, id, MAX_IMAGE_CELL_ROWS + 1, 8, 4, 4).is_valid());
    }

    /// Aspect preservation is the normal case, so the wire form leaves it out and a
    /// missing field must mean "preserve" rather than "stretch".
    #[test]
    fn a_placement_omits_the_flag_it_almost_always_has_and_defaults_to_preserving() {
        let placement = GridImage::new(2, ImageId(3), 4, 8, 64, 32);
        let json = serde_json::to_string(&placement).expect("it serialises");
        assert!(!json.contains("preserve"), "got {json}");
        assert_eq!(
            serde_json::from_str::<GridImage>(&json).expect("it reads back"),
            placement
        );

        let stretched = GridImage {
            preserve_aspect: false,
            ..placement
        };
        let json = serde_json::to_string(&stretched).expect("it serialises");
        assert!(json.contains("\"preserve_aspect\":false"), "got {json}");
        assert_eq!(
            serde_json::from_str::<GridImage>(&json).expect("it reads back"),
            stretched
        );
    }

    #[test]
    fn the_debug_form_of_a_payload_names_its_shape_and_not_its_pixels() {
        let payload = ImagePayload::new(2, 2, vec![0xAB; 16]).expect("an image");
        let debugged = format!("{payload:?}");
        assert!(debugged.contains("2x2"), "got {debugged}");
        assert!(debugged.contains("16 bytes"), "got {debugged}");
        assert!(!debugged.contains("171"), "the pixels leaked: {debugged}");
    }
}
