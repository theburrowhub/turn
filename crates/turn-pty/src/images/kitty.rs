//! The Kitty graphics protocol: `ESC _ G <control data> ; <payload> ESC \`.
//!
//! The most capable of the three and the only one that separates *transmitting* a picture
//! from *placing* it, which is what lets a program send an image once and draw it in ten
//! places. Turn implements direct transmission (`t=d`) and placement (`a=t`, `a=T`, `a=p`),
//! deletion (`a=d`), and the chunking (`m=1`) every real client uses because a single
//! escape sequence of four megabytes is not something a pty likes.
//!
//! ## Two capabilities that are refused, on purpose
//!
//! **File and shared-memory transmission.** `t=f`, `t=t` and `t=s` mean "the picture is in
//! this file, or this shared-memory object — go and read it". That is a process asking the
//! terminal to open a path of the process's choosing, and `t=t` asks it to *delete* the
//! file afterwards. Turn refuses all three and tells the user. It is exactly the shape of
//! request this crate already refuses for OSC 52's clipboard read: the terminal must not
//! act as a more privileged agent on the process's behalf.
//!
//! **Responses.** The protocol says a terminal should answer `ESC_Gi=<id>;OK ESC\` on the
//! pty. Turn does not write to a pty except when a human types, which is a product
//! invariant rather than an oversight — the same one that means Turn cannot approve an
//! agent's permission prompt. Clients that treat the response as advisory (which is all of
//! them in practice, since `q=1` is defined precisely so it can be suppressed) work
//! normally.
//!
//! ## Animation
//!
//! `a=f` and `a=a` — frame transmission and animation control — are accepted as sequences
//! and ignored: a still terminal is the right place for a still picture, and a decoder that
//! half-implemented animation would show the first frame of something the program thinks is
//! moving. The first frame *is* what is shown, because `a=T` transmitted it.

use std::io::Read as _;

use super::base64::Base64Stream;

/// What a program asked the graphics protocol to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Action {
    /// `a=t`: store the picture, do not draw it.
    Transmit,
    /// `a=T`: store it and draw it here. The default when a picture is being sent.
    #[default]
    TransmitAndPlace,
    /// `a=p`: draw a picture already stored, by id.
    Place,
    /// `a=d`: forget a picture, and stop drawing it.
    Delete,
    /// `a=q`: a capability query. Nothing is stored and nothing is drawn.
    Query,
    /// `a=f` or `a=a`: animation. Accepted and ignored.
    Animation,
}

/// How the payload is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// `f=24`: raw RGB, three bytes a pixel. `s` and `v` are required.
    Rgb,
    /// `f=32`: raw RGBA, four bytes a pixel. `s` and `v` are required. The protocol's
    /// default.
    #[default]
    Rgba,
    /// `f=100`: a PNG file, which carries its own dimensions.
    Png,
}

/// Where the picture's bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Medium {
    /// `t=d`: in the escape sequence itself. The only one Turn accepts.
    #[default]
    Direct,
    /// `t=f`: in a file the terminal is asked to open.
    File,
    /// `t=t`: in a temporary file the terminal is asked to open and then delete.
    TemporaryFile,
    /// `t=s`: in a POSIX shared-memory object.
    SharedMemory,
}

impl Medium {
    /// Whether Turn will read from this medium.
    ///
    /// Only the sequence's own bytes. Everything else is a process asking the terminal to
    /// touch the filesystem for it.
    pub fn is_accepted(&self) -> bool {
        matches!(self, Medium::Direct)
    }

    /// What to call it in a refusal the user reads.
    pub fn describe(&self) -> &'static str {
        match self {
            Medium::Direct => "the escape sequence",
            Medium::File => "a file on disk",
            Medium::TemporaryFile => "a temporary file on disk",
            Medium::SharedMemory => "shared memory",
        }
    }
}

/// The control data of one `ESC _ G` sequence.
///
/// Every field's default is the protocol's own: no action, RGBA, direct transmission, and
/// every geometry key absent — which is what an empty control string means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Control {
    pub action: Action,
    pub format: Format,
    pub medium: Medium,
    /// `s` and `v`: the picture's pixel dimensions, required for the raw formats.
    pub width: u32,
    pub height: u32,
    /// `x`, `y`, `w`, `h`: the part of the picture to draw. Zero width or height means all
    /// of it.
    pub crop: (u32, u32, u32, u32),
    /// `c` and `r`: the cell box to draw into. Zero means "work it out".
    pub cols: u32,
    pub rows: u32,
    /// `i`: the id the program gave the picture, for placing or deleting it later.
    pub id: u32,
    /// `m=1`: more chunks of this payload follow.
    pub more: bool,
    /// `o=z`: the payload is zlib-compressed.
    pub compressed: bool,
    /// `d`: which pictures a delete applies to. `a`/`A` mean all of them.
    pub delete_all: bool,
}

/// Parses the comma-separated control data before the `;`.
///
/// Unknown keys are ignored: the protocol is still growing, and refusing a picture because
/// of a hint would be worse than showing it. Values that are not numbers are treated as
/// absent for the same reason.
pub fn parse_control(text: &str) -> Control {
    let mut out = Control::default();
    // `a=T` is the default only when a payload is present; the protocol's own default for
    // the action key is `t`. Turn follows the protocol and lets the caller decide what an
    // absent action means, so a sequence that transmits without saying so still stores.
    let mut saw_action = false;
    for field in text.split(',') {
        let (key, value) = match field.split_once('=') {
            Some(pair) => pair,
            None => continue,
        };
        let number = || value.trim().parse::<u32>().unwrap_or(0);
        match key.trim() {
            "a" => {
                saw_action = true;
                out.action = match value.trim() {
                    "t" => Action::Transmit,
                    "T" => Action::TransmitAndPlace,
                    "p" => Action::Place,
                    "d" => Action::Delete,
                    "q" => Action::Query,
                    "f" | "a" => Action::Animation,
                    _ => Action::Transmit,
                };
            }
            "f" => {
                out.format = match value.trim() {
                    "24" => Format::Rgb,
                    "32" => Format::Rgba,
                    "100" => Format::Png,
                    // An unknown format is not something to guess at: the bytes would be
                    // read as the wrong thing and drawn as noise.
                    _ => Format::Png,
                }
            }
            "t" => {
                out.medium = match value.trim() {
                    "d" => Medium::Direct,
                    "f" => Medium::File,
                    "t" => Medium::TemporaryFile,
                    "s" => Medium::SharedMemory,
                    _ => Medium::Direct,
                }
            }
            "s" => out.width = number(),
            "v" => out.height = number(),
            "x" => out.crop.0 = number(),
            "y" => out.crop.1 = number(),
            "w" => out.crop.2 = number(),
            "h" => out.crop.3 = number(),
            "c" => out.cols = number(),
            "r" => out.rows = number(),
            "i" => out.id = number(),
            "m" => out.more = value.trim() != "0",
            "o" => out.compressed = value.trim() == "z",
            "d" => out.delete_all = matches!(value.trim(), "a" | "A"),
            _ => {}
        }
    }
    if !saw_action {
        // The protocol's stated default. A bare `ESC_G;<payload>ESC\` stores a picture.
        out.action = Action::Transmit;
    }
    out
}

/// Why a Kitty payload could not be turned into pixels.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KittyError {
    #[error("Turn reads pictures from the escape sequence, not from {medium}")]
    RefusedMedium { medium: &'static str },
    #[error("a raw {format} payload must say how big it is")]
    MissingDimensions { format: &'static str },
    #[error("a raw payload of {actual} bytes does not fill {width}x{height}")]
    WrongLength {
        width: u32,
        height: u32,
        actual: usize,
    },
    #[error("the compressed payload could not be expanded")]
    BadCompression,
}

/// Most bytes a compressed Kitty payload may expand to.
///
/// The expansion is the attack: zlib reaches a thousand to one, so a four-kilobyte payload
/// can ask for four megabytes and a four-megabyte one for four gigabytes. The reader is
/// wrapped in a limit rather than trusted, so the refusal costs the limit and not the
/// expansion.
pub const MAX_EXPANDED_BYTES: usize = 32 * 1024 * 1024;

/// Expands a `o=z` payload, bounded.
pub fn inflate(payload: &[u8], limit: usize) -> Result<Vec<u8>, KittyError> {
    let mut out = Vec::new();
    let mut reader = flate2::read::ZlibDecoder::new(payload).take(limit as u64 + 1);
    reader
        .read_to_end(&mut out)
        .map_err(|_| KittyError::BadCompression)?;
    if out.len() > limit {
        return Err(KittyError::BadCompression);
    }
    Ok(out)
}

/// Turns a decoded Kitty payload into a raster.
///
/// The raw formats carry no dimensions of their own, so `s` and `v` are load-bearing: a
/// payload whose length does not match them is refused rather than padded, because padded
/// pixels are a picture with a torn edge and a wrong one everywhere after it.
pub fn to_raster(
    control: &Control,
    payload: Vec<u8>,
    budget_pixels: u64,
) -> Result<super::decode::Raster, KittyError> {
    match control.format {
        Format::Png => super::decode::decode_bounded(&payload, budget_pixels)
            .map_err(|_| KittyError::BadCompression),
        Format::Rgb | Format::Rgba => {
            let channels = if matches!(control.format, Format::Rgb) {
                3
            } else {
                4
            };
            let name = if channels == 3 { "RGB" } else { "RGBA" };
            if control.width == 0 || control.height == 0 {
                return Err(KittyError::MissingDimensions { format: name });
            }
            // Checked before the buffer is built, so an enormous declaration costs nothing.
            super::decode::check_pixels(control.width, control.height, budget_pixels).map_err(
                |_| KittyError::WrongLength {
                    width: control.width,
                    height: control.height,
                    actual: payload.len(),
                },
            )?;
            let expected = control.width as usize * control.height as usize * channels;
            if payload.len() != expected {
                return Err(KittyError::WrongLength {
                    width: control.width,
                    height: control.height,
                    actual: payload.len(),
                });
            }
            let rgba = if channels == 4 {
                payload
            } else {
                let mut rgba = Vec::with_capacity(expected / 3 * 4);
                for chunk in payload.chunks_exact(3) {
                    rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
                }
                rgba
            };
            let raster = super::decode::Raster::new(control.width, control.height, rgba).ok_or(
                KittyError::WrongLength {
                    width: control.width,
                    height: control.height,
                    actual: expected,
                },
            )?;
            Ok(super::decode::fit_within(
                raster,
                turn_proto::MAX_IMAGE_PIXELS,
            ))
        }
    }
}

/// Crops a raster to the `x`, `y`, `w`, `h` the control data asked for.
///
/// A crop entirely outside the picture yields the whole picture rather than nothing: a
/// program with an off-by-one in its source rectangle should see its image, not a blank.
pub fn crop(
    raster: super::decode::Raster,
    (x, y, w, h): (u32, u32, u32, u32),
) -> super::decode::Raster {
    if w == 0 || h == 0 || x >= raster.width || y >= raster.height {
        return raster;
    }
    let width = w.min(raster.width - x);
    let height = h.min(raster.height - y);
    if width == raster.width && height == raster.height {
        return raster;
    }
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for row in y..y + height {
        let start = (row as usize * raster.width as usize + x as usize) * 4;
        let end = start + width as usize * 4;
        match raster.rgba.get(start..end) {
            Some(slice) => rgba.extend_from_slice(slice),
            None => return raster,
        }
    }
    // Unreachable: the buffer was built from exactly `width * height` clamped pixels. Written
    // as a fallback to the uncropped picture rather than an unwrap because this runs inside
    // the pty read loop, where a panic would take the pane with it.
    match super::decode::Raster::new(width, height, rgba) {
        Some(cropped) => cropped,
        None => raster,
    }
}

/// A payload being assembled across `m=1` chunks.
///
/// One in flight at a time, which is what the protocol allows: chunks of a picture must be
/// contiguous. A new transmission starting before the last one finished replaces it, and
/// the abandoned bytes are released rather than held.
#[derive(Debug)]
pub struct Chunked {
    pub control: Control,
    payload: Base64Stream,
}

impl Chunked {
    pub fn new(control: Control, limit: usize) -> Self {
        Self {
            control,
            payload: Base64Stream::with_limit(limit),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.payload.push(bytes);
    }

    /// Whether the payload has already been refused, so a caller can stop early.
    pub fn failed(&self) -> bool {
        self.payload.failed().is_some()
    }

    pub fn finish(self) -> Result<Vec<u8>, super::base64::Base64Error> {
        self.payload.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `kitten icat` sends for a PNG: transmit and display, PNG format, direct.
    #[test]
    fn the_control_data_icat_sends_transmits_and_places_a_png() {
        let control = parse_control("a=T,f=100,t=d,m=1");
        assert_eq!(control.action, Action::TransmitAndPlace);
        assert_eq!(control.format, Format::Png);
        assert_eq!(control.medium, Medium::Direct);
        assert!(control.more);
        assert!(control.medium.is_accepted());
    }

    #[test]
    fn a_sequence_that_does_not_say_what_it_wants_stores_the_picture() {
        assert_eq!(parse_control("f=100").action, Action::Transmit);
        assert_eq!(parse_control("").action, Action::Transmit);
    }

    /// The capability that is refused. A program must not be able to make the terminal open
    /// a path of the program's choosing, still less delete it afterwards.
    #[test]
    fn every_medium_but_the_escape_sequence_itself_is_refused() {
        for (spec, medium) in [
            ("t=f", Medium::File),
            ("t=t", Medium::TemporaryFile),
            ("t=s", Medium::SharedMemory),
        ] {
            let control = parse_control(&format!("a=T,f=100,{spec}"));
            assert_eq!(control.medium, medium);
            assert!(
                !control.medium.is_accepted(),
                "{spec} must not be readable by the terminal"
            );
            assert!(!control.medium.describe().is_empty());
        }
        assert!(parse_control("a=T,t=d").medium.is_accepted());
        // An absent medium is the protocol's default, which is the safe one.
        assert!(parse_control("a=T").medium.is_accepted());
    }

    #[test]
    fn the_geometry_keys_are_read_and_an_unreadable_value_is_treated_as_absent() {
        let control = parse_control("a=T,f=32,s=40,v=30,c=10,r=4,i=7,x=1,y=2,w=8,h=9");
        assert_eq!((control.width, control.height), (40, 30));
        assert_eq!((control.cols, control.rows), (10, 4));
        assert_eq!(control.id, 7);
        assert_eq!(control.crop, (1, 2, 8, 9));

        let broken = parse_control("a=T,s=abc,v=,c=99999999999999999999");
        assert_eq!((broken.width, broken.height), (0, 0));
        assert_eq!(broken.cols, 0);
    }

    #[test]
    fn an_unknown_key_is_ignored_rather_than_refusing_the_picture() {
        let control = parse_control("a=T,f=100,z=5,P=1,q=2,U=1,nonsense=x");
        assert_eq!(control.action, Action::TransmitAndPlace);
        assert_eq!(control.format, Format::Png);
    }

    #[test]
    fn a_raw_rgba_payload_becomes_the_pixels_it_carries() {
        let control = parse_control("a=T,f=32,s=2,v=1");
        let raster = to_raster(&control, vec![1, 2, 3, 4, 5, 6, 7, 8], u64::MAX)
            .expect("a 2x1 RGBA payload");
        assert_eq!((raster.width, raster.height), (2, 1));
        assert_eq!(raster.rgba, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    /// RGB has no alpha channel, and a decoder that forgot to add one would read every
    /// pixel three-quarters of a pixel out of step.
    #[test]
    fn a_raw_rgb_payload_gains_an_opaque_alpha_channel() {
        let control = parse_control("a=T,f=24,s=2,v=1");
        let raster =
            to_raster(&control, vec![10, 20, 30, 40, 50, 60], u64::MAX).expect("a 2x1 RGB payload");
        assert_eq!(raster.rgba, vec![10, 20, 30, 255, 40, 50, 60, 255]);
    }

    /// The raw formats carry no dimensions, so the declaration is load-bearing: a payload
    /// that does not fill it must be refused rather than padded into a torn picture.
    #[test]
    fn a_raw_payload_that_does_not_fill_its_declared_size_is_refused() {
        let control = parse_control("a=T,f=32,s=4,v=4");
        assert!(matches!(
            to_raster(&control, vec![0; 63], u64::MAX),
            Err(KittyError::WrongLength { .. })
        ));
        assert!(to_raster(&control, vec![0; 64], u64::MAX).is_ok());

        let undeclared = parse_control("a=T,f=32");
        assert_eq!(
            to_raster(&undeclared, vec![0; 64], u64::MAX),
            Err(KittyError::MissingDimensions { format: "RGBA" })
        );
    }

    /// The bomb in its raw-format form: an enormous declaration with nothing behind it. It
    /// must be refused from the numbers, before a buffer is built.
    #[test]
    fn an_enormous_declared_size_is_refused_before_a_buffer_is_built() {
        let control = parse_control("a=T,f=32,s=60000,v=60000");
        assert!(to_raster(&control, vec![0; 16], u64::MAX).is_err());
        // And by budget rather than by the hard limit.
        let modest = parse_control("a=T,f=32,s=2000,v=2000");
        assert!(to_raster(&modest, vec![0; 16], 1_000).is_err());
    }

    /// zlib reaches a thousand to one, so the expansion has to be bounded rather than
    /// trusted.
    #[test]
    fn a_compression_bomb_is_refused_at_the_expansion_limit() {
        use std::io::Write as _;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        encoder
            .write_all(&vec![0u8; 4 * 1024 * 1024])
            .expect("the encoder accepts the input");
        let compressed = encoder.finish().expect("it compresses");
        assert!(
            compressed.len() < 32 * 1024,
            "four megabytes of zeroes compress to {} bytes",
            compressed.len()
        );

        assert_eq!(inflate(&compressed, 1_024), Err(KittyError::BadCompression));
        assert_eq!(
            inflate(&compressed, 4 * 1024 * 1024)
                .expect("within the limit it expands")
                .len(),
            4 * 1024 * 1024
        );
        // And bytes that are not zlib at all.
        assert_eq!(
            inflate(b"not compressed", MAX_EXPANDED_BYTES),
            Err(KittyError::BadCompression)
        );
    }

    #[test]
    fn a_source_rectangle_crops_the_picture_it_names() {
        // A 4x2 picture where each pixel's red channel is its index.
        let mut rgba = Vec::new();
        for i in 0..8u8 {
            rgba.extend_from_slice(&[i, 0, 0, 255]);
        }
        let raster = super::super::decode::Raster::new(4, 2, rgba).expect("a raster");

        let cropped = crop(raster.clone(), (1, 0, 2, 2));
        assert_eq!((cropped.width, cropped.height), (2, 2));
        assert_eq!(cropped.rgba[0], 1);
        assert_eq!(cropped.rgba[8], 5, "the second row starts at index five");

        // A crop that names the whole picture, and one that names nothing, both give the
        // picture back rather than an empty raster.
        assert_eq!(crop(raster.clone(), (0, 0, 4, 2)), raster);
        assert_eq!(crop(raster.clone(), (0, 0, 0, 0)), raster);
        assert_eq!(crop(raster.clone(), (99, 99, 4, 2)), raster);
        // And one that runs off the edge is clamped rather than reading past the buffer.
        let clamped = crop(raster, (3, 1, 100, 100));
        assert_eq!((clamped.width, clamped.height), (1, 1));
    }

    #[test]
    fn a_chunked_payload_is_assembled_across_sequences() {
        let mut chunked = Chunked::new(parse_control("a=T,f=100,m=1"), 1_000);
        chunked.push(b"Zm9v");
        chunked.push(b"YmFy");
        assert!(!chunked.failed());
        assert_eq!(chunked.finish().expect("it decodes"), b"foobar");
    }

    #[test]
    fn a_chunked_payload_over_its_limit_is_refused_and_says_so_before_it_ends() {
        let mut chunked = Chunked::new(parse_control("a=T,f=100,m=1"), 8);
        for _ in 0..10 {
            chunked.push(b"AAAAAAAAAAAAAAAA");
        }
        assert!(
            chunked.failed(),
            "a caller must be able to stop scanning a doomed payload"
        );
        assert!(chunked.finish().is_err());
    }

    #[test]
    fn every_action_the_protocol_defines_parses_to_something_meaningful() {
        for (spec, action) in [
            ("a=t", Action::Transmit),
            ("a=T", Action::TransmitAndPlace),
            ("a=p", Action::Place),
            ("a=d", Action::Delete),
            ("a=q", Action::Query),
            ("a=f", Action::Animation),
            ("a=a", Action::Animation),
            // An action nobody defines falls back to storing rather than drawing, which is
            // the conservative reading.
            ("a=Z", Action::Transmit),
        ] {
            assert_eq!(parse_control(spec).action, action, "for {spec}");
        }
        assert!(parse_control("a=d,d=A").delete_all);
        assert!(!parse_control("a=d,d=i").delete_all);
    }
}
