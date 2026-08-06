//! Inline images: three protocols in, bounded pixels and marker cells out.
//!
//! ## Where decoding happens, and why here
//!
//! In the **daemon**, in this crate, at the moment the bytes arrive from the pty.
//!
//! The alternative — passing the escape sequence through and letting the client decode it —
//! was rejected for three reasons, in order of how much they matter:
//!
//! 1. **The daemon has to know how many cells a picture occupies.** The cell box is what
//!    the markers fill, and the markers are what the terminal parser scrolls, clears and
//!    overwrites. Working out the box needs the picture's pixel dimensions, so something on
//!    this side of the socket has to read the header regardless. Having read it, decoding
//!    the rest is the cheap part.
//! 2. **The daemon must work with no client attached.** A pane whose picture only appeared
//!    once somebody looked at it would be a pane whose *text* moved when somebody looked at
//!    it, because the markers occupy cells and the cells push text around.
//! 3. **There must be one decoder, on the trusted side of the bound.** Two decoders means
//!    two sets of limits, and the one in the client would be the one that had never been
//!    audited. A payload that reaches a client has already been checked, decoded, and
//!    normalised to RGBA of a known size.
//!
//! A client re-attaching gets the pictures still on screen for free, because the markers
//! are in the screen the daemon has been keeping all along. It fetches the pixels it does
//! not already hold by id, once each.
//!
//! ## What a program may and may not make Turn do
//!
//! Refused, deliberately, and counted so the UI can say a process tried:
//!
//! * **iTerm2 without `inline=1`** — a request to save a file to the user's disk.
//! * **Kitty's `t=f`, `t=t` and `t=s`** — a request to read a path or shared-memory object
//!   the process chose, and in `t=t`'s case to delete it afterwards.
//! * **Writing anything back to the pty.** The Kitty protocol asks for an acknowledgement
//!   on the pty; Turn never types into a pane except when a human does, which is the same
//!   invariant that stops it approving an agent's permission prompt.
//!
//! This is the same posture the rest of [`crate::buffer`] already takes for OSC 52's
//! clipboard read and write and for a program asking to resize the window.
//!
//! ## The bounds, all of them in one place
//!
//! | What | Limit |
//! |---|---|
//! | Encoded payload held per sequence | [`scan::MAX_IMAGE_PAYLOAD_BYTES`] (8 MiB) |
//! | Sixel body held per sequence | [`sixel::MAX_SIXEL_BYTES`] (8 MiB) |
//! | Kitty payload after decompression | [`kitty::MAX_EXPANDED_BYTES`] (32 MiB) |
//! | Pixels decoded per picture | [`decode::MAX_DECODE_PIXELS`] (16 Mpx) |
//! | Pixels kept and sent per picture | [`turn_proto::MAX_IMAGE_PIXELS`] (1 Mpx, 4 MiB) |
//! | Pixels decoded per pane, amortised | [`PIXELS_PER_INPUT_BYTE`] per byte read, banked to [`MAX_BANKED_PIXELS`] |
//! | Pictures placed on one screen | [`turn_proto::MAX_PLACED_IMAGES`] (8) |
//! | Payloads retained per pane | [`MAX_STORED_IMAGES`] (16) and [`MAX_STORE_BYTES`] (16 MiB) |
//! | Cell box per picture | [`turn_proto::MAX_IMAGE_CELL_ROWS`] x [`turn_proto::MAX_IMAGE_CELL_COLS`] |
//! | `File=` arguments, Kitty control data | [`scan::MAX_ARGS_BYTES`], [`scan::MAX_CONTROL_BYTES`] |
//! | Filename kept as a label | [`iterm::MAX_NAME_CHARS`] |
//!
//! The amortised pixel budget is the one that is not obvious, and it closes the last hole.
//! Every other limit bounds *one* picture; without this, a process could send an unlimited
//! number of small sequences that each decode to a megapixel and spend the machine's whole
//! memory bandwidth doing it. Charging decoded pixels against bytes read ties the cost of
//! decoding to the cost of producing the input, and it needs no clock — which matters,
//! because a pty write has no timestamp and inventing one would put a heuristic in the
//! path of a security bound.

pub mod base64;
pub mod decode;
pub mod iterm;
pub mod kitty;
pub mod layout;
pub mod scan;
pub mod sixel;

use std::collections::VecDeque;

use turn_proto::{GridImage, ImageCell, ImageId, ImagePayload, MAX_PLACED_IMAGES};

pub use decode::Raster;
pub use layout::{BoxRequest, CellBox, SizeSpec, Viewport, NOMINAL_CELL_PIXELS};
pub use scan::{ScanEvent, Scanner, Sequence};

/// How many pixels a pane earns the right to decode per byte the process wrote.
///
/// A compressed picture is at worst a few hundred pixels a byte, so this is generous for
/// anything real: a 200 kB photograph earns fifty megapixels of budget and needs sixteen.
/// What it stops is the pathological case — a tiny sequence that decodes to a megapixel,
/// sent in a loop — which no bound on a single picture can catch.
pub const PIXELS_PER_INPUT_BYTE: u64 = 256;

/// The most decoding credit a pane may bank, sixty-four megapixels.
///
/// Without a ceiling, a pane that produced a gigabyte of ordinary text would have earned
/// the right to decode for the rest of its life.
pub const MAX_BANKED_PIXELS: u64 = 64 * 1024 * 1024;

/// Payloads kept per pane.
///
/// Larger than [`MAX_PLACED_IMAGES`] on purpose: a picture that has scrolled off the live
/// screen is still visible in the client's own scrollback, so keeping it a while longer is
/// what makes scrolling back to a plot show the plot rather than a placeholder.
pub const MAX_STORED_IMAGES: usize = 16;

/// Bytes of pixels kept per pane, 16 MiB.
pub const MAX_STORE_BYTES: usize = 16 * 1024 * 1024;

/// Kitty pictures transmitted but not yet placed, kept per pane.
pub const MAX_KITTY_HELD: usize = 8;

/// Why Turn will not show something a process sent, in words a user can act on.
///
/// Every variant produces a sentence, because the refusal is *shown in the pane*. A picture
/// that silently did not appear is a bug report nobody can write; a line saying the payload
/// was 12 MB and the limit is 8 MB is a thing somebody can do something about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// The payload passed a byte limit.
    PayloadTooLarge { limit: usize },
    /// The picture's dimensions are past what Turn will decode. The bomb case.
    TooManyPixels { width: u32, height: u32 },
    /// The pane's amortised decode budget is spent.
    OverBudget,
    /// The bytes are not a picture Turn can read.
    Unreadable,
    /// The sequence did not parse: truncated base64, a payload that is not there, arguments
    /// past any plausible length.
    Malformed,
    /// iTerm2 without `inline=1`: a request to write a file to the user's disk.
    Download,
    /// Kitty asking Turn to read a file or a shared-memory object.
    RefusedMedium { medium: &'static str },
    /// A Kitty `a=p` naming a picture the pane no longer holds.
    UnknownImage,
}

impl RefusalReason {
    /// The sentence the user sees in the pane.
    ///
    /// Bracketed and prefixed so it cannot be mistaken for the program's own output, short
    /// enough to survive a narrow pane, and specific enough to be actionable. Never
    /// includes anything the process supplied: a refusal notice is not a place to render
    /// untrusted text.
    pub fn notice(&self) -> String {
        let detail = match self {
            RefusalReason::PayloadTooLarge { limit } => {
                format!("payload over {}", megabytes(*limit))
            }
            RefusalReason::TooManyPixels { width, height } => {
                format!("{width}x{height} is too large to decode")
            }
            RefusalReason::OverBudget => "too many images too quickly".to_string(),
            RefusalReason::Unreadable => "not a picture Turn can read".to_string(),
            RefusalReason::Malformed => "the escape sequence was malformed".to_string(),
            RefusalReason::Download => "a download was requested, not an image".to_string(),
            RefusalReason::RefusedMedium { medium } => {
                format!("Turn does not read images from {medium}")
            }
            RefusalReason::UnknownImage => "that image is no longer held".to_string(),
        };
        format!("[turn: image not shown — {detail}]")
    }

    /// The bytes to feed the terminal parser so the notice lands in the pane.
    ///
    /// Plain text, in whatever colours the program had set. No SGR of Turn's own and no
    /// `DECSC`/`DECRC` around it: a program may be in the middle of using the saved-cursor
    /// register, and clobbering it to style a notice would corrupt output that has nothing
    /// to do with pictures. The bracketed prefix carries the meaning instead.
    pub fn notice_bytes(&self) -> Vec<u8> {
        let mut out = self.notice().into_bytes();
        out.extend_from_slice(b"\r\n");
        out
    }
}

/// A byte count as a short human figure, for a notice.
fn megabytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MB", bytes / (1024 * 1024))
    } else {
        format!("{} kB", bytes.div_ceil(1024))
    }
}

/// One slot's placement, as the store remembers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    id: ImageId,
    rows: u16,
    cols: u16,
    width: u32,
    height: u32,
    preserve_aspect: bool,
    /// When this slot was filled, so the oldest can be reused when all eight are taken.
    order: u64,
}

/// A pane's pictures: the payloads it holds, which slots are placed, and what it may still
/// afford to decode.
#[derive(Debug)]
pub struct ImageStore {
    payloads: VecDeque<ImagePayload>,
    stored_bytes: usize,
    slots: [Option<Slot>; MAX_PLACED_IMAGES],
    next_order: u64,
    /// Kitty's `a=t`: transmitted, waiting for an `a=p`. `(kitty id, payload id)`.
    held: VecDeque<(u32, ImageId)>,
    /// Pixels this pane may still decode.
    budget: u64,
    cell_pixels: (u16, u16),
    placements: u64,
    refusals: u64,
    decoded_pixels: u64,
}

impl Default for ImageStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageStore {
    pub fn new() -> Self {
        Self {
            payloads: VecDeque::new(),
            stored_bytes: 0,
            slots: [None; MAX_PLACED_IMAGES],
            next_order: 0,
            held: VecDeque::new(),
            // Enough for one picture before a byte has been read, so the first thing a
            // process prints can be an image.
            budget: turn_proto::MAX_IMAGE_PIXELS as u64,
            cell_pixels: NOMINAL_CELL_PIXELS,
            placements: 0,
            refusals: 0,
            decoded_pixels: 0,
        }
    }

    /// Earns decoding credit for bytes the process wrote.
    pub fn credit(&mut self, input_bytes: usize) {
        self.budget = self
            .budget
            .saturating_add((input_bytes as u64).saturating_mul(PIXELS_PER_INPUT_BYTE))
            .min(MAX_BANKED_PIXELS);
    }

    /// What this pane may still decode.
    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// The pixel size of a cell, as the client measured it.
    ///
    /// Used only to turn a *pixel* size in an escape sequence into a number of cells. Until
    /// a client reports one this is [`NOMINAL_CELL_PIXELS`], and being wrong about it
    /// changes how much of the pane a picture claims, never its shape.
    pub fn set_cell_pixels(&mut self, width: u16, height: u16) {
        self.cell_pixels = (width.max(1), height.max(1));
    }

    pub fn cell_pixels(&self) -> (u16, u16) {
        self.cell_pixels
    }

    /// The table a grid carries: one entry per filled slot.
    ///
    /// Not filtered by what is on screen — `turn_proto::from_screen_with_images` does that,
    /// because it is the thing holding the screen.
    pub fn table(&self) -> Vec<GridImage> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(slot, state)| {
                let state = (*state)?;
                Some(GridImage {
                    slot: slot as u8,
                    id: state.id,
                    rows: state.rows,
                    cols: state.cols,
                    width: state.width,
                    height: state.height,
                    preserve_aspect: state.preserve_aspect,
                })
            })
            .collect()
    }

    /// The pixels of one picture, for a client that asked.
    pub fn payload(&self, id: ImageId) -> Option<&ImagePayload> {
        self.payloads.iter().find(|payload| payload.id == id)
    }

    pub fn stored_images(&self) -> usize {
        self.payloads.len()
    }

    pub fn stored_bytes(&self) -> usize {
        self.stored_bytes
    }

    /// How many pictures this pane has placed, for logs and for a test that wants to know
    /// something happened.
    pub fn placements(&self) -> u64 {
        self.placements
    }

    /// How many sequences were refused. Surfaced so the UI can say a process tried.
    pub fn refusals(&self) -> u64 {
        self.refusals
    }

    /// Total pixels this pane has ever decoded.
    ///
    /// The measure the amortised budget is about: it must stay under
    /// [`PIXELS_PER_INPUT_BYTE`] times the bytes the process wrote, plus the opening grant,
    /// which is what makes decoding cost linear in what the process paid to produce.
    pub fn decoded_pixels(&self) -> u64 {
        self.decoded_pixels
    }

    /// Charges a decode against the budget, whether or not it ends up on screen.
    ///
    /// The pixels have already been produced by the time this is called, so the pane pays
    /// for them regardless: a picture decoded and then found unplaceable cost exactly as
    /// much as one that was drawn.
    fn charge(&mut self, pixels: u64) {
        self.decoded_pixels = self.decoded_pixels.saturating_add(pixels);
        self.budget = self.budget.saturating_sub(pixels);
    }

    /// Records a refusal the scanner made on its own — a payload past its limit, base64
    /// that is not base64 — which never reaches [`apply`] and would otherwise go uncounted.
    pub fn note_refusal(&mut self) {
        self.refusals = self.refusals.saturating_add(1);
    }

    /// Adds a payload, evicting to stay inside both bounds.
    ///
    /// A payload already held is moved to the front of the queue rather than stored twice:
    /// ids are content-derived, so the same picture printed a hundred times costs one copy.
    fn keep(&mut self, payload: ImagePayload) -> ImageId {
        let id = payload.id;
        if let Some(position) = self.payloads.iter().position(|held| held.id == id) {
            if let Some(existing) = self.payloads.remove(position) {
                self.payloads.push_back(existing);
            }
            return id;
        }
        self.stored_bytes += payload.byte_len();
        self.payloads.push_back(payload);
        while self.payloads.len() > MAX_STORED_IMAGES || self.stored_bytes > MAX_STORE_BYTES {
            // The oldest payload no slot still refers to, so a picture on screen is never
            // dropped out from under a client that is about to ask for it. When every held
            // payload is placed, the oldest goes anyway — its slot is about to be reused.
            let victim = self
                .payloads
                .iter()
                .position(|held| !self.is_placed(held.id))
                .unwrap_or(0);
            match self.payloads.remove(victim) {
                Some(dropped) => self.stored_bytes -= dropped.byte_len(),
                None => break,
            }
        }
        id
    }

    fn is_placed(&self, id: ImageId) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.is_some_and(|state| state.id == id))
    }

    /// Records a placement in a slot.
    fn fill(&mut self, slot: u8, id: ImageId, cells: CellBox, raster: (u32, u32), preserve: bool) {
        if let Some(entry) = self.slots.get_mut(slot as usize) {
            self.next_order += 1;
            *entry = Some(Slot {
                id,
                rows: cells.rows,
                cols: cells.cols,
                width: raster.0,
                height: raster.1,
                preserve_aspect: preserve,
                order: self.next_order,
            });
        }
        self.placements += 1;
    }

    /// Forgets a slot's placement. The cells are the caller's business.
    fn release(&mut self, slot: u8) {
        if let Some(entry) = self.slots.get_mut(slot as usize) {
            *entry = None;
        }
    }

    /// Which slot has been filled longest, for reuse when all of them are.
    fn oldest_slot(&self) -> u8 {
        let mut oldest = 0u8;
        let mut best = u64::MAX;
        for (slot, state) in self.slots.iter().enumerate() {
            let order = state.map_or(0, |state| state.order);
            if order < best {
                best = order;
                oldest = slot as u8;
            }
        }
        oldest
    }

    /// Remembers a Kitty picture transmitted without being placed.
    fn hold(&mut self, kitty_id: u32, id: ImageId) {
        self.held.retain(|(held, _)| *held != kitty_id);
        self.held.push_back((kitty_id, id));
        while self.held.len() > MAX_KITTY_HELD {
            self.held.pop_front();
        }
    }

    fn held_image(&self, kitty_id: u32) -> Option<ImageId> {
        self.held
            .iter()
            .find(|(held, _)| *held == kitty_id)
            .map(|(_, id)| *id)
    }

    /// The dimensions of a held payload, for placing it without decoding it again.
    fn dimensions(&self, id: ImageId) -> Option<(u32, u32)> {
        self.payload(id)
            .map(|payload| (payload.width, payload.height))
    }
}

/// Acts on one image sequence: decodes it, places it, or refuses it.
///
/// Takes the parser rather than a screen because placing a picture *writes to the screen* —
/// the markers are cells, and putting them there is done by feeding the parser the same
/// kind of bytes a program would. That keeps one implementation of "what happens to the
/// cursor and to the rows below" instead of a second one that would drift.
pub fn apply<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    store: &mut ImageStore,
    sequence: Sequence,
) -> Option<RefusalReason> {
    let outcome = match sequence {
        Sequence::ITerm { args, payload } => apply_iterm(parser, store, &args, payload),
        Sequence::Sixel { body } => apply_sixel(parser, store, &body),
        Sequence::Kitty { control, payload } => apply_kitty(parser, store, &control, payload),
    };
    if outcome.is_some() {
        store.refusals += 1;
    }
    outcome
}

fn apply_iterm<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    store: &mut ImageStore,
    args: &str,
    payload: Vec<u8>,
) -> Option<RefusalReason> {
    let parsed = iterm::parse_args(args);
    if !parsed.inline {
        // A download: the process is asking Turn to write to the user's filesystem.
        return Some(RefusalReason::Download);
    }
    let raster = match decode::decode_bounded(&payload, store.budget) {
        Ok(raster) => raster,
        Err(error) => return Some(refusal_for_decode(error)),
    };
    store.charge(raster.pixels());
    place(parser, store, raster, parsed.size)
}

fn apply_sixel<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    store: &mut ImageStore,
    body: &[u8],
) -> Option<RefusalReason> {
    let raster = match sixel::decode(body, store.budget) {
        Ok(raster) => raster,
        Err(sixel::SixelError::TooLarge { width, height, .. })
        | Err(sixel::SixelError::PastCanvas { width, height }) => {
            return Some(RefusalReason::TooManyPixels { width, height })
        }
        Err(sixel::SixelError::OverBudget) => return Some(RefusalReason::OverBudget),
        Err(sixel::SixelError::Empty) => return Some(RefusalReason::Malformed),
    };
    store.charge(raster.pixels());
    // Sixel has no way to ask for a size: the picture is its own size, in pixels.
    place(parser, store, raster, BoxRequest::default())
}

fn apply_kitty<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    store: &mut ImageStore,
    control: &str,
    payload: Vec<u8>,
) -> Option<RefusalReason> {
    let parsed = kitty::parse_control(control);
    match parsed.action {
        kitty::Action::Query | kitty::Action::Animation => return None,
        kitty::Action::Delete => {
            delete(parser, store, &parsed);
            return None;
        }
        kitty::Action::Place => {
            let Some(id) = store.held_image(parsed.id) else {
                return Some(RefusalReason::UnknownImage);
            };
            let Some((width, height)) = store.dimensions(id) else {
                return Some(RefusalReason::UnknownImage);
            };
            return place_stored(parser, store, id, (width, height), &parsed);
        }
        kitty::Action::Transmit | kitty::Action::TransmitAndPlace => {}
    }

    if !parsed.medium.is_accepted() {
        return Some(RefusalReason::RefusedMedium {
            medium: parsed.medium.describe(),
        });
    }
    let payload = if parsed.compressed {
        match kitty::inflate(&payload, kitty::MAX_EXPANDED_BYTES) {
            Ok(bytes) => bytes,
            Err(_) => return Some(RefusalReason::Malformed),
        }
    } else {
        payload
    };
    if payload.is_empty() {
        return Some(RefusalReason::Malformed);
    }
    let raster = match kitty::to_raster(&parsed, payload, store.budget) {
        Ok(raster) => kitty::crop(raster, parsed.crop),
        Err(kitty::KittyError::MissingDimensions { .. })
        | Err(kitty::KittyError::WrongLength { .. }) => return Some(RefusalReason::Malformed),
        Err(_) => return Some(RefusalReason::Unreadable),
    };
    store.charge(raster.pixels());

    let request = kitty_box(&parsed);
    if matches!(parsed.action, kitty::Action::Transmit) {
        // Stored, not drawn. Its pixels are kept so a later `a=p` can place it without the
        // program having to send them again.
        let payload = match ImagePayload::new(raster.width, raster.height, raster.rgba) {
            Ok(payload) => payload,
            Err(_) => return Some(RefusalReason::Unreadable),
        };
        let id = store.keep(payload);
        store.hold(parsed.id, id);
        return None;
    }
    place(parser, store, raster, request)
}

/// The cell box a Kitty command asked for. `c` and `r` are already in cells.
fn kitty_box(control: &kitty::Control) -> BoxRequest {
    BoxRequest {
        width: if control.cols == 0 {
            SizeSpec::Auto
        } else {
            SizeSpec::Cells(control.cols)
        },
        height: if control.rows == 0 {
            SizeSpec::Auto
        } else {
            SizeSpec::Cells(control.rows)
        },
        preserve_aspect: true,
    }
}

/// How a decode failure is described to the user.
fn refusal_for_decode(error: decode::DecodeError) -> RefusalReason {
    match error {
        decode::DecodeError::TooLarge { width, height, .. } => {
            RefusalReason::TooManyPixels { width, height }
        }
        decode::DecodeError::OverBudget { .. } => RefusalReason::OverBudget,
        decode::DecodeError::UnknownFormat | decode::DecodeError::Damaged => {
            RefusalReason::Unreadable
        }
        decode::DecodeError::Empty => RefusalReason::Malformed,
    }
}

/// Places a freshly decoded picture at the cursor.
fn place<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    store: &mut ImageStore,
    raster: Raster,
    request: BoxRequest,
) -> Option<RefusalReason> {
    let dimensions = (raster.width, raster.height);
    let payload = match ImagePayload::new(raster.width, raster.height, raster.rgba) {
        Ok(payload) => payload,
        Err(_) => return Some(RefusalReason::Unreadable),
    };
    let id = store.keep(payload);
    place_stored_inner(parser, store, id, dimensions, request)
}

/// Places a picture the pane already holds, for Kitty's `a=p`.
fn place_stored<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    store: &mut ImageStore,
    id: ImageId,
    dimensions: (u32, u32),
    control: &kitty::Control,
) -> Option<RefusalReason> {
    place_stored_inner(parser, store, id, dimensions, kitty_box(control))
}

fn place_stored_inner<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    store: &mut ImageStore,
    id: ImageId,
    dimensions: (u32, u32),
    request: BoxRequest,
) -> Option<RefusalReason> {
    // A picture that would start in the last column has no room for even one cell, so it
    // begins on the next line — which is what a terminal does with a character too.
    let (screen_rows, screen_cols) = parser.screen().size();
    if parser.screen().cursor_position().1 >= screen_cols.saturating_sub(1) {
        parser.process(b"\r\n");
    }
    let (_, left) = parser.screen().cursor_position();
    let room = screen_cols.saturating_sub(left);

    let cells = layout::resolve(
        request,
        dimensions,
        Viewport::new(screen_rows, screen_cols, store.cell_pixels, room),
    );

    let slot = free_slot(parser.screen()).unwrap_or_else(|| {
        let oldest = store.oldest_slot();
        // The slot is about to name a different picture, so every cell still carrying its
        // marker has to go first. A stale marker would draw a tile of the *new* image at
        // the old one's position, which is the one failure this design could produce and
        // the reason eviction erases rather than overwrites.
        let bytes = erase_slot_bytes(parser.screen(), oldest);
        if !bytes.is_empty() {
            parser.process(&bytes);
        }
        store.release(oldest);
        oldest
    });

    store.fill(slot, id, cells, dimensions, request.preserve_aspect);
    let bytes = marker_bytes(slot, cells, left, screen_cols);
    parser.process(&bytes);
    None
}

/// Acts on a Kitty delete.
fn delete<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    store: &mut ImageStore,
    control: &kitty::Control,
) {
    let targets: Vec<u8> = if control.delete_all {
        (0..MAX_PLACED_IMAGES as u8).collect()
    } else {
        match store.held_image(control.id) {
            Some(id) => store
                .slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| slot.is_some_and(|state| state.id == id))
                .map(|(slot, _)| slot as u8)
                .collect(),
            None => Vec::new(),
        }
    };
    for slot in targets {
        let bytes = erase_slot_bytes(parser.screen(), slot);
        if !bytes.is_empty() {
            parser.process(&bytes);
        }
        store.release(slot);
    }
}

/// The first slot with no cells on the screen.
///
/// Asked of the *screen* rather than of the store, so a slot whose picture has scrolled
/// away or been overwritten is free again without anything having to notice that it was.
fn free_slot(screen: &vt100::Screen) -> Option<u8> {
    let mut occupied = [false; MAX_PLACED_IMAGES];
    for (_, _, tile) in marker_cells(screen) {
        if let Some(slot) = occupied.get_mut(tile.slot as usize) {
            *slot = true;
        }
    }
    occupied.iter().position(|taken| !*taken).map(|s| s as u8)
}

/// Every marker cell on the visible screen, as `(row, col, tile)`.
fn marker_cells(screen: &vt100::Screen) -> Vec<(u16, u16, ImageCell)> {
    let (rows, cols) = screen.size();
    let mut out = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if let Some(tile) = turn_proto::images::marker_of(cell.contents()) {
                out.push((row, col, tile));
            }
        }
    }
    out
}

/// The bytes that erase every cell still carrying `slot`'s markers, leaving the cursor
/// where it was.
///
/// `ECH` per contiguous run rather than a space per cell, so a full-width picture is a
/// handful of sequences. The cursor is put back explicitly rather than with `DECSC`/`DECRC`
/// because a program may be using the saved-cursor register itself.
fn erase_slot_bytes(screen: &vt100::Screen, slot: u8) -> Vec<u8> {
    let cells: Vec<(u16, u16)> = marker_cells(screen)
        .into_iter()
        .filter(|(_, _, tile)| tile.slot == slot)
        .map(|(row, col, _)| (row, col))
        .collect();
    if cells.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < cells.len() {
        let (row, col) = cells[index];
        let mut run = 1usize;
        while index + run < cells.len() && cells[index + run] == (row, col + run as u16) {
            run += 1;
        }
        out.extend_from_slice(format!("\x1b[{};{}H\x1b[{}X", row + 1, col + 1, run).as_bytes());
        index += run;
    }
    let (row, col) = screen.cursor_position();
    out.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    out
}

/// The bytes that write a picture's markers into the screen.
///
/// Written as output rather than poked into the grid so the terminal parser applies its own
/// rules: a picture that reaches the bottom of the screen **scrolls it**, and every marker
/// row already written moves up with the text, which is exactly what has to happen.
///
/// Each row is positioned with an absolute column, so a scroll in the middle of writing
/// cannot leave the rest of the picture in the wrong place.
fn marker_bytes(slot: u8, cells: CellBox, left: u16, screen_cols: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(cells.rows as usize * (cells.cols as usize * 4 + 8));
    for dy in 0..cells.rows {
        if dy > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("\x1b[{}G", left as u32 + 1).as_bytes());
        for dx in 0..cells.cols {
            // A tile outside the alphabet is unreachable for a box the layout produced,
            // since the box is clamped to it. Skipped rather than unwrapped because this
            // runs inside the pty read loop, where a panic would take the pane with it.
            if let Some(marker) = ImageCell::new(slot, dy, dx).to_marker() {
                let mut buffer = [0u8; 4];
                out.extend_from_slice(marker.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }
    // Where the cursor ends up. A picture that reached the right margin gets a fresh line,
    // so the next thing printed is not squeezed into the last column; otherwise the cursor
    // sits just past the picture, which is what makes text flow around one.
    let after = left as u32 + cells.cols as u32;
    if after >= screen_cols as u32 {
        out.extend_from_slice(b"\r\n");
    } else {
        out.extend_from_slice(format!("\x1b[{}G", after + 1).as_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser(rows: u16, cols: u16) -> vt100::Parser<()> {
        vt100::Parser::new(rows, cols, 100)
    }

    /// Every marker on the screen, as `(row, col, tile)`, for asserting on placement.
    fn placed(parser: &vt100::Parser<()>) -> Vec<(u16, u16, ImageCell)> {
        marker_cells(parser.screen())
    }

    fn tiny_png(width: u32, height: u32) -> Vec<u8> {
        let mut buffer = image::RgbaImage::new(width, height);
        for pixel in buffer.pixels_mut() {
            *pixel = image::Rgba([200, 30, 30, 255]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(buffer)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("the encoder produces a PNG");
        out.into_inner()
    }

    fn iterm_sequence(args: &str, payload: &[u8]) -> Sequence {
        Sequence::ITerm {
            args: args.to_string(),
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn an_iterm_picture_fills_the_cells_it_claims_and_moves_the_cursor_past_it() {
        let mut parser = parser(10, 40);
        let mut store = ImageStore::new();
        // 32 by 34 pixels is four columns and two rows of the nominal cell.
        let refusal = apply(
            &mut parser,
            &mut store,
            iterm_sequence("inline=1", &tiny_png(32, 34)),
        );
        assert_eq!(refusal, None);

        let cells = placed(&parser);
        assert_eq!(cells.len(), 8, "four columns by two rows: {cells:?}");
        assert_eq!(cells[0], (0, 0, ImageCell::new(0, 0, 0)));
        assert_eq!(cells[3], (0, 3, ImageCell::new(0, 0, 3)));
        assert_eq!(cells[4], (1, 0, ImageCell::new(0, 1, 0)));
        assert_eq!(
            parser.screen().cursor_position(),
            (1, 4),
            "the cursor sits just past the picture, so text flows around it"
        );

        let table = store.table();
        assert_eq!(table.len(), 1);
        assert_eq!((table[0].rows, table[0].cols), (2, 4));
        assert_eq!((table[0].width, table[0].height), (32, 34));
        assert!(table[0].preserve_aspect);
        assert!(store.payload(table[0].id).is_some());
        assert_eq!(store.placements(), 1);
    }

    /// The grid the client sees, built the way the daemon builds it.
    #[test]
    fn the_grid_a_client_receives_has_the_picture_in_it_with_its_table() {
        let mut parser = parser(6, 20);
        let mut store = ImageStore::new();
        parser.process(b"before ");
        apply(
            &mut parser,
            &mut store,
            iterm_sequence("inline=1;width=4;height=1", &tiny_png(40, 40)),
        );
        parser.process(b" after");

        let grid = turn_proto::from_screen_with_images(
            parser.screen(),
            std::iter::empty(),
            &store.table(),
        );
        assert_eq!(grid.images.len(), 1);
        assert!(grid.has_images());
        assert_eq!(
            grid.row_text(0),
            "before      after",
            "the picture reads as the space it occupies, with text flowing after it"
        );
        let (image, tile) = grid.image_at(0, 7).expect("a tile of the picture");
        assert_eq!(tile, ImageCell::new(0, 0, 0));
        assert_eq!((image.rows, image.cols), (1, 4));
    }

    #[test]
    fn a_sixel_is_placed_at_the_cursor_like_any_other_picture() {
        let mut parser = parser(10, 40);
        let mut store = ImageStore::new();
        // Sixteen columns of six pixels: two cell columns wide, one row tall.
        let refusal = apply(
            &mut parser,
            &mut store,
            Sequence::Sixel {
                body: b"#1!16~".to_vec(),
            },
        );
        assert_eq!(refusal, None);
        let cells = placed(&parser);
        assert_eq!(cells.len(), 2, "16 by 6 pixels is two cells: {cells:?}");
        let table = store.table();
        assert_eq!((table[0].width, table[0].height), (16, 6));
    }

    #[test]
    fn a_kitty_transmit_and_place_draws_it_and_a_bare_transmit_does_not() {
        let mut parser = parser(10, 40);
        let mut store = ImageStore::new();

        // `a=t` stores without drawing.
        assert_eq!(
            apply(
                &mut parser,
                &mut store,
                Sequence::Kitty {
                    control: "a=t,f=100,i=7".into(),
                    payload: tiny_png(16, 17),
                },
            ),
            None
        );
        assert!(placed(&parser).is_empty(), "a=t must not draw");
        assert_eq!(store.stored_images(), 1);
        assert!(store.table().is_empty());

        // `a=p` draws the one it stored.
        assert_eq!(
            apply(
                &mut parser,
                &mut store,
                Sequence::Kitty {
                    control: "a=p,i=7".into(),
                    payload: Vec::new(),
                },
            ),
            None
        );
        assert_eq!(placed(&parser).len(), 2, "16 by 17 pixels is two cells");

        // And `a=p` for something never sent is a refusal, not a blank.
        assert_eq!(
            apply(
                &mut parser,
                &mut store,
                Sequence::Kitty {
                    control: "a=p,i=99".into(),
                    payload: Vec::new(),
                },
            ),
            Some(RefusalReason::UnknownImage)
        );
    }

    #[test]
    fn a_kitty_delete_erases_the_cells_the_picture_was_drawn_in() {
        let mut parser = parser(6, 20);
        let mut store = ImageStore::new();
        apply(
            &mut parser,
            &mut store,
            Sequence::Kitty {
                control: "a=T,f=100,i=3,c=4,r=2".into(),
                payload: tiny_png(40, 40),
            },
        );
        assert_eq!(placed(&parser).len(), 8);
        // The picture has to be held under its Kitty id for a delete to find it.
        store.hold(3, store.table()[0].id);

        apply(
            &mut parser,
            &mut store,
            Sequence::Kitty {
                control: "a=d,d=A".into(),
                payload: Vec::new(),
            },
        );
        assert!(placed(&parser).is_empty(), "the markers must be gone");
        assert!(store.table().is_empty());
    }

    /// The refusals a user has to be told about, each with a sentence.
    #[test]
    fn every_refusal_produces_a_sentence_that_says_what_happened() {
        let reasons = [
            RefusalReason::PayloadTooLarge {
                limit: 8 * 1024 * 1024,
            },
            RefusalReason::TooManyPixels {
                width: 60_000,
                height: 60_000,
            },
            RefusalReason::OverBudget,
            RefusalReason::Unreadable,
            RefusalReason::Malformed,
            RefusalReason::Download,
            RefusalReason::RefusedMedium {
                medium: "a file on disk",
            },
            RefusalReason::UnknownImage,
        ];
        for reason in &reasons {
            let notice = reason.notice();
            assert!(notice.starts_with("[turn: image not shown"), "got {notice}");
            assert!(notice.ends_with(']'), "got {notice}");
            assert!(notice.len() < 90, "too long for a narrow pane: {notice}");
            assert!(
                notice.chars().all(crate::is_display_safe),
                "a notice must be safe to display: {notice:?}"
            );
            assert!(reason.notice_bytes().ends_with(b"\r\n"));
        }
        assert!(RefusalReason::PayloadTooLarge {
            limit: 8 * 1024 * 1024
        }
        .notice()
        .contains("8 MB"));
        assert!(RefusalReason::TooManyPixels {
            width: 60_000,
            height: 60_000
        }
        .notice()
        .contains("60000x60000"));
    }

    /// A program asking Turn to write a file to disk is refused, and told so.
    #[test]
    fn a_download_request_is_refused_rather_than_written_anywhere() {
        let mut parser = parser(6, 20);
        let mut store = ImageStore::new();
        let refusal = apply(
            &mut parser,
            &mut store,
            iterm_sequence("size=99;name=Zm9v", &tiny_png(8, 8)),
        );
        assert_eq!(refusal, Some(RefusalReason::Download));
        assert!(placed(&parser).is_empty());
        assert_eq!(store.refusals(), 1);
    }

    /// A program asking Turn to read a path it chose is refused, and told so.
    #[test]
    fn kitty_transmission_from_a_file_or_shared_memory_is_refused() {
        for medium in ["t=f", "t=t", "t=s"] {
            let mut parser = parser(6, 20);
            let mut store = ImageStore::new();
            let refusal = apply(
                &mut parser,
                &mut store,
                Sequence::Kitty {
                    control: format!("a=T,f=100,{medium}"),
                    payload: turn_proto::encode_base64(b"/etc/passwd").into_bytes(),
                },
            );
            match refusal {
                Some(RefusalReason::RefusedMedium { medium }) => {
                    assert!(!medium.is_empty())
                }
                other => panic!("{medium} was not refused: {other:?}"),
            }
            assert!(placed(&parser).is_empty());
        }
    }

    /// The bomb, through the whole path this module owns.
    #[test]
    fn a_decompression_bomb_is_refused_with_a_notice_and_nothing_is_placed() {
        let mut parser = parser(10, 40);
        let mut store = ImageStore::new();
        let refusal = apply(
            &mut parser,
            &mut store,
            iterm_sequence("inline=1", &bomb(60_000, 60_000)),
        );
        assert_eq!(
            refusal,
            Some(RefusalReason::TooManyPixels {
                width: 60_000,
                height: 60_000
            })
        );
        assert!(placed(&parser).is_empty());
        assert_eq!(store.stored_bytes(), 0, "nothing was retained for it");
    }

    /// A valid PNG claiming an enormous raster with nothing behind it: an honest IHDR, a
    /// token IDAT and an IEND. This is the shape a real decompression bomb has.
    fn bomb(width: u32, height: u32) -> Vec<u8> {
        let mut out: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut ihdr = Vec::from(b"IHDR".as_slice());
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        push_chunk(&mut out, &ihdr);
        let mut idat = Vec::from(b"IDAT".as_slice());
        idat.extend_from_slice(&[0x78, 0x9c, 0x63, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01]);
        push_chunk(&mut out, &idat);
        push_chunk(&mut out, b"IEND");
        out
    }

    fn push_chunk(out: &mut Vec<u8>, body: &[u8]) {
        out.extend_from_slice(&((body.len() - 4) as u32).to_be_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(&crc32(body).to_be_bytes());
    }

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

    /// The bound no single-picture limit can provide: a process cannot buy unlimited
    /// decoding with a handful of bytes.
    #[test]
    fn a_pane_stops_decoding_once_its_amortised_budget_is_spent() {
        let mut parser = parser(40, 120);
        let mut store = ImageStore::new();
        // 400 by 400 is 160,000 pixels a time. The opening grant is one megapixel, so
        // about six fit before the budget runs out.
        let picture = tiny_png(400, 400);
        let mut placed_count = 0usize;
        let mut refused = None;
        for _ in 0..20 {
            match apply(
                &mut parser,
                &mut store,
                iterm_sequence("inline=1", &picture),
            ) {
                None => placed_count += 1,
                Some(reason) => {
                    refused = Some(reason);
                    break;
                }
            }
        }
        assert!(placed_count > 0, "the first pictures must be shown");
        assert_eq!(refused, Some(RefusalReason::OverBudget));

        // And reading more output earns the right to decode again.
        store.credit(1024 * 1024);
        assert_eq!(
            apply(
                &mut parser,
                &mut store,
                iterm_sequence("inline=1", &picture)
            ),
            None
        );
    }

    #[test]
    fn the_budget_is_banked_but_never_without_limit() {
        let mut store = ImageStore::new();
        store.credit(usize::MAX / 2);
        assert_eq!(store.budget(), MAX_BANKED_PIXELS);
        store.credit(usize::MAX / 2);
        assert_eq!(store.budget(), MAX_BANKED_PIXELS, "and it does not wrap");
    }

    /// The one failure this design could produce, and the reason eviction erases: a slot
    /// reused while its old markers are still on screen would draw the new picture's tiles
    /// at the old picture's position.
    #[test]
    fn a_ninth_picture_reuses_the_oldest_slot_and_erases_its_markers_first() {
        let mut parser = parser(40, 120);
        let mut store = ImageStore::new();
        let picture = tiny_png(16, 17);

        for index in 0..MAX_PLACED_IMAGES {
            // Each on its own line, so nine pictures fit on the screen.
            parser.process(b"\r\n");
            store.credit(64 * 1024);
            assert_eq!(
                apply(
                    &mut parser,
                    &mut store,
                    iterm_sequence("inline=1", &picture)
                ),
                None,
                "picture {index} was refused"
            );
        }
        let slots: std::collections::BTreeSet<u8> = placed(&parser)
            .iter()
            .map(|(_, _, tile)| tile.slot)
            .collect();
        assert_eq!(slots.len(), MAX_PLACED_IMAGES, "all eight slots are in use");

        // The ninth. Slot zero is the oldest, so its cells must be gone.
        parser.process(b"\r\n");
        store.credit(64 * 1024);
        // A different picture, so a reused slot showing the wrong one would be visible.
        assert_eq!(
            apply(
                &mut parser,
                &mut store,
                iterm_sequence("inline=1", &tiny_png(32, 17))
            ),
            None
        );

        let cells = placed(&parser);
        let zeros: Vec<&(u16, u16, ImageCell)> =
            cells.iter().filter(|(_, _, tile)| tile.slot == 0).collect();
        assert_eq!(
            zeros.len(),
            4,
            "slot zero must hold only the new picture's four tiles: {zeros:?}"
        );
        // And exactly one row carries them, which is the new picture's own row.
        let rows: std::collections::BTreeSet<u16> = zeros.iter().map(|(row, _, _)| *row).collect();
        assert_eq!(rows.len(), 1);
        let table = store.table();
        let slot_zero = table
            .iter()
            .find(|image| image.slot == 0)
            .expect("slot zero is filled");
        assert_eq!((slot_zero.width, slot_zero.height), (32, 17));
    }

    /// A slot whose cells have scrolled away is free again, without anything having to
    /// notice that they went.
    #[test]
    fn a_slot_whose_picture_scrolled_off_is_free_again() {
        let mut parser = parser(4, 20);
        let mut store = ImageStore::new();
        apply(
            &mut parser,
            &mut store,
            iterm_sequence("inline=1", &tiny_png(16, 17)),
        );
        assert_eq!(free_slot(parser.screen()), Some(1));

        // Scroll it off the top.
        parser.process(b"\r\n\r\n\r\n\r\n\r\n");
        assert_eq!(
            free_slot(parser.screen()),
            Some(0),
            "the markers are gone, so the slot is available"
        );
    }

    /// A picture at the bottom of the screen has to scroll it, and the rows already written
    /// have to come with it. This works because the markers are written as output.
    #[test]
    fn a_picture_at_the_bottom_of_the_screen_scrolls_it_and_stays_whole() {
        let mut parser = parser(6, 20);
        let mut store = ImageStore::new();
        parser.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
        assert_eq!(parser.screen().cursor_position().0, 5);

        // Three rows tall, at the last row: the screen must scroll twice.
        apply(
            &mut parser,
            &mut store,
            iterm_sequence("inline=1;width=4;height=3", &tiny_png(40, 40)),
        );
        let cells = placed(&parser);
        assert_eq!(cells.len(), 12, "all twelve tiles survive: {cells:?}");
        let rows: std::collections::BTreeSet<u16> = cells.iter().map(|(row, _, _)| *row).collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            *rows.iter().max().expect("a row"),
            5,
            "the picture ends on the last row, having scrolled the text above it"
        );
        // And the tiles are still in the right order after the scroll.
        let top = *rows.iter().min().expect("a row");
        let left = cells
            .iter()
            .map(|(_, col, _)| *col)
            .min()
            .expect("a column");
        for (row, col, tile) in &cells {
            assert_eq!(*tile, ImageCell::new(0, row - top, col - left));
        }
    }

    /// A picture beginning in the last column has nowhere to go, so it starts on a fresh
    /// line rather than being cut to one column.
    #[test]
    fn a_picture_with_no_room_left_on_the_line_starts_on_the_next_one() {
        let mut parser = parser(6, 20);
        let mut store = ImageStore::new();
        parser.process(&[b'x'; 19]);
        assert_eq!(parser.screen().cursor_position(), (0, 19));

        apply(
            &mut parser,
            &mut store,
            iterm_sequence("inline=1;width=4;height=1", &tiny_png(40, 40)),
        );
        let cells = placed(&parser);
        assert_eq!(cells.len(), 4);
        assert!(
            cells.iter().all(|(row, _, _)| *row == 1),
            "the picture belongs on the next line: {cells:?}"
        );
        assert_eq!(cells[0].1, 0);
    }

    /// A picture that reaches the right margin leaves the cursor on a fresh line, so the
    /// next thing printed is not squeezed into the last column.
    #[test]
    fn a_picture_reaching_the_right_margin_leaves_the_cursor_on_the_next_line() {
        let mut parser = parser(6, 20);
        let mut store = ImageStore::new();
        apply(
            &mut parser,
            &mut store,
            iterm_sequence("inline=1;width=100%;height=1", &tiny_png(40, 40)),
        );
        assert_eq!(placed(&parser).len(), 20);
        assert_eq!(parser.screen().cursor_position(), (1, 0));
    }

    #[test]
    fn the_same_picture_twice_is_stored_once_because_ids_are_its_contents() {
        let mut parser = parser(10, 40);
        let mut store = ImageStore::new();
        let picture = tiny_png(16, 17);
        for _ in 0..4 {
            store.credit(64 * 1024);
            parser.process(b"\r\n");
            apply(
                &mut parser,
                &mut store,
                iterm_sequence("inline=1", &picture),
            );
        }
        assert_eq!(store.stored_images(), 1, "one payload for four placements");
        assert_eq!(store.table().len(), 4, "and four slots pointing at it");
        assert_eq!(store.placements(), 4);
    }

    /// The retained-memory bound. A pane that shows a hundred different pictures must not
    /// keep a hundred of them.
    #[test]
    fn the_payload_store_stays_inside_both_of_its_bounds() {
        let mut parser = parser(40, 120);
        let mut store = ImageStore::new();
        for index in 0..40u32 {
            store.credit(4 * 1024 * 1024);
            parser.process(b"\r\n");
            // A different picture each time, so nothing deduplicates.
            let mut buffer = image::RgbaImage::new(64, 64);
            for (x, y, pixel) in buffer.enumerate_pixels_mut() {
                *pixel = image::Rgba([(x + index) as u8, y as u8, index as u8, 255]);
            }
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(buffer)
                .write_to(&mut bytes, image::ImageFormat::Png)
                .expect("it encodes");
            apply(
                &mut parser,
                &mut store,
                iterm_sequence("inline=1", &bytes.into_inner()),
            );
        }
        assert!(
            store.stored_images() <= MAX_STORED_IMAGES,
            "{} payloads were kept",
            store.stored_images()
        );
        assert!(
            store.stored_bytes() <= MAX_STORE_BYTES,
            "{} bytes were kept",
            store.stored_bytes()
        );
        // And every picture still placed is one a client can still fetch.
        for image in store.table() {
            assert!(
                store.payload(image.id).is_some(),
                "slot {} names a payload the pane no longer holds",
                image.slot
            );
        }
    }

    #[test]
    fn a_reported_cell_size_changes_how_many_cells_a_pixel_request_claims() {
        let mut parser = parser(20, 80);
        let mut store = ImageStore::new();
        store.set_cell_pixels(16, 34);
        apply(
            &mut parser,
            &mut store,
            iterm_sequence("inline=1;width=64px;height=68px", &tiny_png(64, 68)),
        );
        let table = store.table();
        assert_eq!(
            (table[0].rows, table[0].cols),
            (2, 4),
            "a 16-pixel cell halves the columns a 64-pixel request claims"
        );
        assert_eq!(store.cell_pixels(), (16, 34));

        // A degenerate report is clamped rather than dividing by zero.
        store.set_cell_pixels(0, 0);
        assert_eq!(store.cell_pixels(), (1, 1));
    }

    #[test]
    fn a_malformed_sequence_is_refused_without_placing_anything() {
        let mut parser = parser(6, 20);
        let mut store = ImageStore::new();
        for sequence in [
            iterm_sequence("inline=1", b"not a picture"),
            iterm_sequence("inline=1", &[]),
            Sequence::Sixel { body: Vec::new() },
            Sequence::Sixel {
                body: b"????".to_vec(),
            },
            Sequence::Kitty {
                control: "a=T,f=32".into(),
                payload: vec![1, 2, 3],
            },
            Sequence::Kitty {
                control: "a=T,f=100".into(),
                payload: b"garbage".to_vec(),
            },
            Sequence::Kitty {
                control: String::new(),
                payload: Vec::new(),
            },
        ] {
            assert!(
                apply(&mut parser, &mut store, sequence.clone()).is_some(),
                "{sequence:?} should have been refused"
            );
            assert!(placed(&parser).is_empty(), "after {sequence:?}");
        }
    }

    #[test]
    fn a_query_or_an_animation_command_is_accepted_and_draws_nothing() {
        let mut parser = parser(6, 20);
        let mut store = ImageStore::new();
        for control in ["a=q,i=1,s=1,v=1", "a=f,i=1", "a=a,i=1"] {
            assert_eq!(
                apply(
                    &mut parser,
                    &mut store,
                    Sequence::Kitty {
                        control: control.into(),
                        payload: Vec::new(),
                    },
                ),
                None,
                "{control} must not be an error"
            );
        }
        assert!(placed(&parser).is_empty());
        assert_eq!(store.refusals(), 0);
    }

    #[test]
    fn a_byte_count_reads_as_a_short_human_figure() {
        assert_eq!(megabytes(8 * 1024 * 1024), "8 MB");
        assert_eq!(megabytes(1024), "1 kB");
        assert_eq!(megabytes(1), "1 kB");
        assert_eq!(megabytes(0), "0 kB");
    }
}
