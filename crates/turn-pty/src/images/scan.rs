//! Lifting image sequences out of the byte stream, before the terminal parser sees them.
//!
//! ## Why there is a scanner at all
//!
//! `vt100` cannot deliver these sequences. Its callbacks expose OSC and unhandled CSI, but
//! **DCS is silently swallowed** — so Sixel is invisible — and `ESC _` lands in `vte`'s
//! SOS/PM/APC state, which has no callback at all, so the Kitty protocol is invisible too.
//! Waiting for the parser to grow them is not an option, and forking it to add them would
//! put the bounds on untrusted input inside a dependency.
//!
//! There is a second, better reason. `vte` buffers an OSC payload in a `Vec` with **no
//! limit**: a process emitting `ESC ] 1337 ; File=` followed by half a gigabyte of base64
//! and no terminator would have all of it accumulated before any callback fired. The only
//! place that can be bounded is upstream of the parser, which is here.
//!
//! ## What it does, and what it must not do
//!
//! It is a byte-for-byte state machine over the stream. Bytes that are not part of an
//! image sequence come out **unchanged and in order**, so the terminal parser sees exactly
//! the stream it would have seen; bytes that are come out as a [`Sequence`] and never
//! reach the parser at all. That is what stops a Sixel body from being printed as
//! thousands of `?~$-` characters if the parser's handling ever changes.
//!
//! Sequences span writes. A four-megabyte Kitty payload arrives in as many pty reads as
//! the kernel likes, so every buffer here persists across [`Scanner::feed`] calls — and
//! every one of them is bounded, because the process decides when the sequence ends and
//! may decide never.
//!
//! ## Deciding whether a sequence is ours
//!
//! An `ESC ]` might be a title, a colour query, an OSC 52 clipboard attempt or an image.
//! The scanner buffers only as far as it takes to tell — ten bytes for `1337;File=` — and
//! the moment the prefix diverges it hands those buffered bytes back as ordinary output
//! and stops looking. Nothing is consumed speculatively that cannot be given back.

use std::borrow::Cow;

use super::base64::Base64Stream;
use super::RefusalReason;

/// Most bytes a single picture's payload may decode to, 8 MiB.
///
/// This is the compressed file, not the pixels: a PNG this big is well past anything a
/// terminal is asked to show, and the pixel count is bounded separately and much more
/// tightly. Two limits rather than one because they defend different things — this one
/// bounds what is *held*, and the pixel limit bounds what is *decoded*.
pub const MAX_IMAGE_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

/// Most bytes of `File=` arguments Turn will hold.
///
/// Arguments are a handful of short key-value pairs. A process sending more is not
/// describing a picture.
pub const MAX_ARGS_BYTES: usize = 512;

/// Most bytes of Kitty control data Turn will hold.
pub const MAX_CONTROL_BYTES: usize = 256;

/// Most bytes of DCS parameters read while deciding whether a sequence is a Sixel.
pub const MAX_DCS_PARAM_BYTES: usize = 32;

/// The prefix that makes an OSC an iTerm2 file transfer.
const ITERM_PREFIX: &[u8] = b"1337;File=";

/// One complete image sequence, as framing alone can describe it.
///
/// Deliberately not interpreted here: the scanner's job is to find the boundaries and
/// bound the bytes, and mixing the grammar of three protocols into the state machine that
/// frames them is how a framing bug becomes a decoding bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sequence {
    /// `ESC ] 1337 ; File = <args> : <payload> ST`
    ITerm { args: String, payload: Vec<u8> },
    /// `ESC P <params> q <body> ST`
    Sixel { body: Vec<u8> },
    /// `ESC _ G <control> ; <payload> ST`
    Kitty { control: String, payload: Vec<u8> },
}

/// What the scanner found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanEvent<'a> {
    /// Bytes for the terminal parser, unchanged. Borrowed from the chunk when they came
    /// straight from it, owned when they were buffered while a sequence was being
    /// identified.
    Text(Cow<'a, [u8]>),
    /// A sequence to decode and place.
    Image(Box<Sequence>),
    /// A sequence Turn will not act on, and why the user is about to be told.
    Refused(RefusalReason),
}

/// Where the scanner is in the stream.
#[derive(Debug)]
enum State {
    Ground,
    /// Saw `ESC`, waiting to see what kind of sequence this is.
    Escape,
    /// Saw `ESC ]`, collecting bytes to compare against [`ITERM_PREFIX`].
    OscProbe {
        seen: Vec<u8>,
    },
    /// An OSC that is not ours. Its bytes go to the parser.
    OscPassthrough,
    /// An iTerm2 file transfer, collecting its arguments.
    ItermArgs {
        args: Vec<u8>,
    },
    /// An iTerm2 file transfer, collecting its base64 payload.
    ItermPayload {
        args: String,
        payload: Base64Stream,
    },
    /// Saw `ESC P`, collecting parameters to see whether the final byte is `q`.
    ///
    /// `intermediate` matters: `ESC P $ q m ST` is DECRQSS, whose final byte is also `q`.
    /// Only a `q` with no intermediate byte before it introduces a Sixel.
    DcsProbe {
        seen: Vec<u8>,
        intermediate: bool,
    },
    /// A DCS that is not a Sixel. Its bytes go to the parser, which ignores them.
    DcsPassthrough,
    /// A Sixel body.
    Sixel {
        body: Vec<u8>,
        refused: bool,
    },
    /// Saw `ESC _`, waiting for the `G` that makes it a graphics command.
    ApcProbe,
    /// A Kitty graphics command, collecting its control data.
    KittyControl {
        control: Vec<u8>,
    },
    /// A Kitty graphics command, collecting its base64 payload.
    KittyPayload {
        control: String,
        payload: Base64Stream,
    },
    /// An APC that is not ours.
    ApcPassthrough,
    /// A sequence already refused. Its remaining bytes are discarded until it terminates,
    /// so a process that never terminates one costs nothing more.
    Discarding {
        report: Option<RefusalReason>,
    },
}

/// The stream scanner. One per terminal, living as long as it does.
#[derive(Debug)]
pub struct Scanner {
    state: State,
    payload_limit: usize,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    pub fn new() -> Self {
        Self::with_payload_limit(MAX_IMAGE_PAYLOAD_BYTES)
    }

    /// A scanner with a smaller payload limit, so the bound can be exercised in a test
    /// without allocating eight mebibytes of base64.
    pub fn with_payload_limit(payload_limit: usize) -> Self {
        Self {
            state: State::Ground,
            payload_limit,
        }
    }

    /// Whether the scanner is in the middle of a sequence, so a caller can tell that
    /// output is being withheld rather than lost.
    pub fn is_mid_sequence(&self) -> bool {
        !matches!(self.state, State::Ground)
    }

    /// Feeds a chunk and returns what it found, in order.
    ///
    /// The events are the whole of the chunk: concatenating every [`ScanEvent::Text`] with
    /// the sequences removed reproduces the input exactly, which is the property
    /// `the_stream_the_parser_sees_is_the_stream_minus_the_pictures` holds down.
    pub fn feed<'a>(&mut self, data: &'a [u8]) -> Vec<ScanEvent<'a>> {
        let mut events: Vec<ScanEvent<'a>> = Vec::new();
        let mut index = 0usize;

        while index < data.len() {
            if matches!(self.state, State::Ground) {
                // The fast path, and the common one: everything up to the next escape is
                // ordinary output and is handed over without being copied.
                let next = data[index..].iter().position(|byte| *byte == 0x1B);
                match next {
                    Some(0) => {
                        self.state = State::Escape;
                        index += 1;
                        continue;
                    }
                    Some(offset) => {
                        events.push(ScanEvent::Text(Cow::Borrowed(&data[index..index + offset])));
                        index += offset;
                        continue;
                    }
                    None => {
                        events.push(ScanEvent::Text(Cow::Borrowed(&data[index..])));
                        break;
                    }
                }
            }

            let byte = data[index];
            // A transition may decline the byte so the new state can read it instead. That
            // is how "this OSC is not ours after all" hands the byte to the passthrough
            // state without the caller having to re-enter `feed`.
            if self.step(byte, &mut events) {
                index += 1;
            }
        }
        events
    }

    /// Handles one byte. Returns whether it was consumed.
    fn step<'a>(&mut self, byte: u8, events: &mut Vec<ScanEvent<'a>>) -> bool {
        match &mut self.state {
            State::Ground => true,

            State::Escape => match byte {
                b']' => {
                    self.state = State::OscProbe { seen: Vec::new() };
                    true
                }
                b'P' => {
                    self.state = State::DcsProbe {
                        seen: Vec::new(),
                        intermediate: false,
                    };
                    true
                }
                b'_' => {
                    self.state = State::ApcProbe;
                    true
                }
                // Any other escape belongs to the parser, `ESC` included.
                _ => {
                    events.push(ScanEvent::Text(Cow::Owned(vec![0x1B, byte])));
                    self.state = State::Ground;
                    true
                }
            },

            State::OscProbe { seen } => {
                let position = seen.len();
                let expected = ITERM_PREFIX.get(position).copied();
                if expected == Some(byte) {
                    seen.push(byte);
                    if seen.len() == ITERM_PREFIX.len() {
                        self.state = State::ItermArgs { args: Vec::new() };
                    }
                    return true;
                }
                // Not ours. Everything buffered goes to the parser exactly as it arrived,
                // and this byte is read again by the passthrough state.
                let mut owed = Vec::with_capacity(seen.len() + 2);
                owed.extend_from_slice(b"\x1b]");
                owed.extend_from_slice(seen);
                events.push(ScanEvent::Text(Cow::Owned(owed)));
                self.state = State::OscPassthrough;
                false
            }

            State::OscPassthrough => {
                // `ESC` ends the string and begins a new escape, which is what the parser's
                // own state machine does. It is *not* emitted here: whatever it introduces
                // emits it, so a passthrough followed by `ESC \` does not produce two.
                if byte == 0x1B {
                    self.state = State::Escape;
                    return true;
                }
                events.push(ScanEvent::Text(Cow::Owned(vec![byte])));
                // BEL and the single-character ST both end an OSC.
                if matches!(byte, 0x07 | 0x9C) {
                    self.state = State::Ground;
                }
                true
            }

            State::ItermArgs { args } => {
                if is_terminator_start(byte) {
                    // A header with no payload at all. Nothing to draw, and nothing worth
                    // telling the user about.
                    return self.terminate(byte, None, events);
                }
                if byte == b':' {
                    let text = String::from_utf8_lossy(args).into_owned();
                    self.state = State::ItermPayload {
                        args: text,
                        payload: Base64Stream::with_limit(self.payload_limit),
                    };
                    return true;
                }
                if args.len() >= MAX_ARGS_BYTES {
                    self.state = State::Discarding {
                        report: Some(RefusalReason::Malformed),
                    };
                    return true;
                }
                args.push(byte);
                true
            }

            State::ItermPayload { args, payload } => {
                if is_terminator_start(byte) {
                    let args = std::mem::take(args);
                    let taken = std::mem::replace(payload, Base64Stream::with_limit(0));
                    let sequence = match taken.finish() {
                        Ok(bytes) if bytes.is_empty() => {
                            return self.terminate(byte, Some(RefusalReason::Malformed), events)
                        }
                        Ok(bytes) => Sequence::ITerm {
                            args,
                            payload: bytes,
                        },
                        Err(error) => {
                            return self.terminate(byte, Some(refusal_for(error)), events)
                        }
                    };
                    events.push(ScanEvent::Image(Box::new(sequence)));
                    return self.consume_terminator(byte);
                }
                payload.push(&[byte]);
                if let Some(error) = payload.failed() {
                    self.state = State::Discarding {
                        report: Some(refusal_for(error)),
                    };
                }
                true
            }

            State::DcsProbe { seen, intermediate } => {
                match byte {
                    // Parameter and intermediate bytes: still deciding.
                    0x20..=0x3B if seen.len() < MAX_DCS_PARAM_BYTES => {
                        *intermediate = *intermediate || byte <= 0x2F;
                        seen.push(byte);
                        true
                    }
                    // `q` with no intermediate is the Sixel final byte. `$ q` is DECRQSS,
                    // and everything else in the final range is some other DCS.
                    b'q' if !*intermediate => {
                        self.state = State::Sixel {
                            body: Vec::new(),
                            refused: false,
                        };
                        true
                    }
                    _ => {
                        let mut owed = Vec::with_capacity(seen.len() + 2);
                        owed.extend_from_slice(b"\x1bP");
                        owed.extend_from_slice(seen);
                        events.push(ScanEvent::Text(Cow::Owned(owed)));
                        self.state = State::DcsPassthrough;
                        false
                    }
                }
            }

            State::DcsPassthrough => {
                if byte == 0x1B {
                    self.state = State::Escape;
                    return true;
                }
                events.push(ScanEvent::Text(Cow::Owned(vec![byte])));
                if byte == 0x9C {
                    self.state = State::Ground;
                }
                true
            }

            State::Sixel { body, refused } => {
                if is_terminator_start(byte) {
                    if *refused {
                        return self.terminate(
                            byte,
                            Some(RefusalReason::PayloadTooLarge {
                                limit: super::sixel::MAX_SIXEL_BYTES,
                            }),
                            events,
                        );
                    }
                    let body = std::mem::take(body);
                    if body.is_empty() {
                        return self.terminate(byte, None, events);
                    }
                    events.push(ScanEvent::Image(Box::new(Sequence::Sixel { body })));
                    return self.consume_terminator(byte);
                }
                if body.len() >= super::sixel::MAX_SIXEL_BYTES {
                    // Released rather than kept: a refused eight-megabyte body must not
                    // stay resident until the process remembers to terminate it.
                    *body = Vec::new();
                    *refused = true;
                    return true;
                }
                if !*refused {
                    body.push(byte);
                }
                true
            }

            State::ApcProbe => {
                if byte == b'G' {
                    self.state = State::KittyControl {
                        control: Vec::new(),
                    };
                    return true;
                }
                events.push(ScanEvent::Text(Cow::Owned(vec![0x1B, b'_'])));
                self.state = State::ApcPassthrough;
                false
            }

            State::ApcPassthrough => {
                if byte == 0x1B {
                    self.state = State::Escape;
                    return true;
                }
                events.push(ScanEvent::Text(Cow::Owned(vec![byte])));
                if byte == 0x9C {
                    self.state = State::Ground;
                }
                true
            }

            State::KittyControl { control } => {
                if is_terminator_start(byte) {
                    // Control data with no payload: a delete, a placement of something
                    // already stored, or a query.
                    let text = String::from_utf8_lossy(control).into_owned();
                    events.push(ScanEvent::Image(Box::new(Sequence::Kitty {
                        control: text,
                        payload: Vec::new(),
                    })));
                    return self.consume_terminator(byte);
                }
                if byte == b';' {
                    let text = String::from_utf8_lossy(control).into_owned();
                    self.state = State::KittyPayload {
                        control: text,
                        payload: Base64Stream::with_limit(self.payload_limit),
                    };
                    return true;
                }
                if control.len() >= MAX_CONTROL_BYTES {
                    self.state = State::Discarding {
                        report: Some(RefusalReason::Malformed),
                    };
                    return true;
                }
                control.push(byte);
                true
            }

            State::KittyPayload { control, payload } => {
                if is_terminator_start(byte) {
                    let control = std::mem::take(control);
                    let taken = std::mem::replace(payload, Base64Stream::with_limit(0));
                    match taken.finish() {
                        Ok(bytes) => {
                            events.push(ScanEvent::Image(Box::new(Sequence::Kitty {
                                control,
                                payload: bytes,
                            })));
                            return self.consume_terminator(byte);
                        }
                        Err(error) => {
                            return self.terminate(byte, Some(refusal_for(error)), events)
                        }
                    }
                }
                payload.push(&[byte]);
                if let Some(error) = payload.failed() {
                    self.state = State::Discarding {
                        report: Some(refusal_for(error)),
                    };
                }
                true
            }

            State::Discarding { report } => {
                if is_terminator_start(byte) {
                    let report = report.take();
                    return self.terminate(byte, report, events);
                }
                true
            }
        }
    }

    /// Ends the current sequence, reporting a refusal if there is one.
    fn terminate<'a>(
        &mut self,
        byte: u8,
        report: Option<RefusalReason>,
        events: &mut Vec<ScanEvent<'a>>,
    ) -> bool {
        if let Some(reason) = report {
            events.push(ScanEvent::Refused(reason));
        }
        self.consume_terminator(byte)
    }

    /// Consumes a terminator byte.
    ///
    /// `ESC` is the first half of `ESC \` and also the start of whatever comes next, so it
    /// moves to the escape state rather than to ground: `ESC \` then resolves as an escape
    /// the parser can have, and `ESC ] …` starts a new sequence immediately, which is
    /// exactly what a program printing two pictures in a row does.
    fn consume_terminator(&mut self, byte: u8) -> bool {
        self.state = if byte == 0x1B {
            State::Escape
        } else {
            State::Ground
        };
        true
    }
}

/// Whether a byte ends a control string: BEL, the single-character ST, or the `ESC` of
/// `ESC \`.
fn is_terminator_start(byte: u8) -> bool {
    matches!(byte, 0x07 | 0x9C | 0x1B)
}

/// How a base64 failure is described to the user.
fn refusal_for(error: super::base64::Base64Error) -> RefusalReason {
    match error {
        super::base64::Base64Error::TooLarge { limit } => RefusalReason::PayloadTooLarge { limit },
        super::base64::Base64Error::BadCharacter { .. }
        | super::base64::Base64Error::Truncated { .. } => RefusalReason::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything the scanner handed to the parser, concatenated.
    fn text(events: &[ScanEvent<'_>]) -> Vec<u8> {
        let mut out = Vec::new();
        for event in events {
            if let ScanEvent::Text(bytes) = event {
                out.extend_from_slice(bytes);
            }
        }
        out
    }

    fn images(events: &[ScanEvent<'_>]) -> Vec<Sequence> {
        events
            .iter()
            .filter_map(|event| match event {
                ScanEvent::Image(sequence) => Some((**sequence).clone()),
                _ => None,
            })
            .collect()
    }

    fn refusals(events: &[ScanEvent<'_>]) -> Vec<RefusalReason> {
        events
            .iter()
            .filter_map(|event| match event {
                ScanEvent::Refused(reason) => Some(reason.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn ordinary_output_passes_through_untouched_and_uncopied() {
        let mut scanner = Scanner::new();
        let events = scanner.feed(b"hello \x1b[31mworld\x1b[0m\r\n");
        assert_eq!(text(&events), b"hello \x1b[31mworld\x1b[0m\r\n");
        assert!(images(&events).is_empty());
        assert!(!scanner.is_mid_sequence());
        // The bulk of it was handed over borrowed rather than copied.
        assert!(events
            .iter()
            .any(|event| matches!(event, ScanEvent::Text(Cow::Borrowed(_)))));
    }

    #[test]
    fn an_iterm_file_transfer_is_lifted_out_with_its_arguments_and_payload() {
        let mut scanner = Scanner::new();
        let events = scanner.feed(b"before\x1b]1337;File=inline=1;width=10:Zm9v\x07after");
        assert_eq!(
            text(&events),
            b"beforeafter",
            "not one byte of the sequence may reach the parser"
        );
        assert_eq!(
            images(&events),
            vec![Sequence::ITerm {
                args: "inline=1;width=10".into(),
                payload: b"foo".to_vec(),
            }]
        );
    }

    #[test]
    fn an_iterm_transfer_terminated_with_st_rather_than_bel_works_too() {
        let mut scanner = Scanner::new();
        let events = scanner.feed(b"\x1b]1337;File=inline=1:Zm9v\x1b\\tail");
        assert_eq!(
            text(&events),
            b"\x1b\\tail",
            "the ST belongs to the parser, which knows what to do with it"
        );
        assert_eq!(images(&events).len(), 1);
    }

    /// An OSC that is not ours must reach the parser byte for byte, or titles and the OSC
    /// 52 refusal would stop working.
    #[test]
    fn an_osc_that_is_not_an_image_reaches_the_parser_exactly_as_it_arrived() {
        for sequence in [
            b"\x1b]0;a title\x07".to_vec(),
            b"\x1b]52;c;bWFsaWNpb3Vz\x07".to_vec(),
            b"\x1b]8;;https://example.com\x1b\\".to_vec(),
            // The awkward ones: prefixes of `1337;File=` that then diverge.
            b"\x1b]1337;SetBadge=x\x07".to_vec(),
            b"\x1b]1337\x07".to_vec(),
            b"\x1b]13\x07".to_vec(),
            b"\x1b]1\x07".to_vec(),
            b"\x1b]\x07".to_vec(),
        ] {
            let mut scanner = Scanner::new();
            let events = scanner.feed(&sequence);
            assert_eq!(
                text(&events),
                sequence,
                "{:?} was altered on its way to the parser",
                String::from_utf8_lossy(&sequence)
            );
            assert!(images(&events).is_empty());
        }
    }

    #[test]
    fn a_sixel_body_is_lifted_out_and_a_dcs_that_is_not_one_is_not() {
        let mut scanner = Scanner::new();
        let events = scanner.feed(b"x\x1bP0;1;0q#1~~\x1b\\y");
        assert_eq!(text(&events), b"x\x1b\\y");
        assert_eq!(
            images(&events),
            vec![Sequence::Sixel {
                body: b"#1~~".to_vec()
            }]
        );

        // A DECRQSS request, which is a DCS with a different final byte.
        let mut scanner = Scanner::new();
        let request = b"\x1bP$qm\x1b\\".to_vec();
        let events = scanner.feed(&request);
        assert_eq!(text(&events), request);
        assert!(images(&events).is_empty());
    }

    #[test]
    fn a_kitty_command_is_lifted_out_with_and_without_a_payload() {
        let mut scanner = Scanner::new();
        let events = scanner.feed(b"a\x1b_Ga=T,f=100;Zm9v\x1b\\b");
        assert_eq!(text(&events), b"a\x1b\\b");
        assert_eq!(
            images(&events),
            vec![Sequence::Kitty {
                control: "a=T,f=100".into(),
                payload: b"foo".to_vec(),
            }]
        );

        // A delete has control data and no payload at all.
        let mut scanner = Scanner::new();
        let events = scanner.feed(b"\x1b_Ga=d,d=A\x1b\\");
        assert_eq!(
            images(&events),
            vec![Sequence::Kitty {
                control: "a=d,d=A".into(),
                payload: Vec::new(),
            }]
        );
    }

    #[test]
    fn an_apc_that_is_not_a_graphics_command_reaches_the_parser() {
        let mut scanner = Scanner::new();
        let sequence = b"\x1b_something else\x1b\\".to_vec();
        let events = scanner.feed(&sequence);
        assert_eq!(text(&events), sequence);
        assert!(images(&events).is_empty());
    }

    /// The property everything else rests on: with the pictures removed, the parser sees
    /// the stream it would have seen.
    #[test]
    fn the_stream_the_parser_sees_is_the_stream_minus_the_pictures() {
        let script: &[u8] = b"line one\r\n\
            \x1b]0;title\x07\
            \x1b[1;32mgreen\x1b[0m\
            \x1b]1337;File=inline=1:Zm9v\x07\
            middle\r\n\
            \x1bP0;0;0q#1~\x1b\\\
            \x1b_Ga=T,f=100;Zm9v\x1b\\\
            \x1b[?1049h\
            last";
        let mut scanner = Scanner::new();
        let events = scanner.feed(script);
        assert_eq!(
            text(&events),
            // The iTerm2 sequence was BEL-terminated, so nothing of it is left; the Sixel
            // and the Kitty command were ST-terminated, and their `ESC \` belongs to the
            // parser, which treats it as the no-op it is.
            b"line one\r\n\x1b]0;title\x07\x1b[1;32mgreen\x1b[0mmiddle\r\n\
              \x1b\\\x1b\\\x1b[?1049hlast"
                .to_vec()
        );
        assert_eq!(images(&events).len(), 3);
        assert!(!scanner.is_mid_sequence());
    }

    /// The kernel decides where the boundaries are, so a sequence split at every possible
    /// point has to produce the same result.
    #[test]
    fn a_sequence_split_at_every_byte_boundary_is_reassembled() {
        let script: &[u8] =
            b"pre\x1b]1337;File=inline=1;width=4:Zm9vYmFy\x07mid\x1bP0q#1~~\x1b\\post";
        let whole = {
            let mut scanner = Scanner::new();
            let events = scanner.feed(script);
            (text(&events), images(&events))
        };

        for split in 1..script.len() {
            let mut scanner = Scanner::new();
            let mut seen_text = Vec::new();
            let mut seen_images = Vec::new();
            for part in [&script[..split], &script[split..]] {
                let events = scanner.feed(part);
                seen_text.extend_from_slice(&text(&events));
                seen_images.extend(images(&events));
            }
            assert_eq!(seen_text, whole.0, "text differed for a split at {split}");
            assert_eq!(seen_images, whole.1, "images differed at {split}");
        }

        // And one byte at a time, the worst case.
        let mut scanner = Scanner::new();
        let mut seen_text = Vec::new();
        let mut seen_images = Vec::new();
        for byte in script {
            let one = [*byte];
            let events = scanner.feed(&one);
            seen_text.extend_from_slice(&text(&events));
            seen_images.extend(images(&events));
        }
        assert_eq!(seen_text, whole.0);
        assert_eq!(seen_images, whole.1);
    }

    /// The bound that matters most: a process that opens a sequence and never closes it
    /// must not be able to make the scanner hold its output.
    #[test]
    fn a_payload_that_never_terminates_is_refused_at_the_limit_and_held_no_longer() {
        let mut scanner = Scanner::with_payload_limit(1_024);
        scanner.feed(b"\x1b]1337;File=inline=1:");
        for _ in 0..200 {
            let events = scanner.feed(&[b'A'; 4_096]);
            assert!(images(&events).is_empty());
        }
        // Only when it finally terminates is the refusal reported, once.
        let events = scanner.feed(b"\x07after");
        assert_eq!(
            refusals(&events),
            vec![RefusalReason::PayloadTooLarge { limit: 1_024 }]
        );
        assert_eq!(text(&events), b"after");
        assert!(!scanner.is_mid_sequence());
    }

    #[test]
    fn a_sixel_that_never_terminates_is_refused_at_its_own_limit() {
        let mut scanner = Scanner::new();
        scanner.feed(b"\x1bP0q");
        let chunk = vec![b'~'; 1024 * 1024];
        for _ in 0..12 {
            scanner.feed(&chunk);
        }
        let events = scanner.feed(b"\x1b\\");
        assert_eq!(
            refusals(&events),
            vec![RefusalReason::PayloadTooLarge {
                limit: super::super::sixel::MAX_SIXEL_BYTES
            }]
        );
        assert!(images(&events).is_empty());
    }

    #[test]
    fn a_payload_that_is_not_base64_is_refused_rather_than_decoded_into_noise() {
        let mut scanner = Scanner::new();
        let events = scanner.feed(b"\x1b]1337;File=inline=1:not base64!!\x07tail");
        assert_eq!(refusals(&events), vec![RefusalReason::Malformed]);
        assert!(images(&events).is_empty());
        assert_eq!(text(&events), b"tail");
    }

    #[test]
    fn a_header_of_absurdly_many_arguments_is_refused_rather_than_retained() {
        let mut scanner = Scanner::new();
        let mut sequence = Vec::from(b"\x1b]1337;File=".as_slice());
        sequence.extend(std::iter::repeat_n(b'x', MAX_ARGS_BYTES * 4));
        sequence.extend_from_slice(b":Zm9v\x07");
        let events = scanner.feed(&sequence);
        assert_eq!(refusals(&events), vec![RefusalReason::Malformed]);
        assert!(images(&events).is_empty());

        // The same for Kitty control data.
        let mut scanner = Scanner::new();
        let mut sequence = Vec::from(b"\x1b_G".as_slice());
        sequence.extend(std::iter::repeat_n(b'a', MAX_CONTROL_BYTES * 4));
        sequence.extend_from_slice(b";Zm9v\x1b\\");
        let events = scanner.feed(&sequence);
        assert_eq!(refusals(&events), vec![RefusalReason::Malformed]);
    }

    #[test]
    fn an_empty_payload_is_a_malformed_sequence_rather_than_an_empty_picture() {
        let mut scanner = Scanner::new();
        let events = scanner.feed(b"\x1b]1337;File=inline=1:\x07");
        assert_eq!(refusals(&events), vec![RefusalReason::Malformed]);

        // A header with no colon at all carries nothing and is not worth a notice.
        let mut scanner = Scanner::new();
        let events = scanner.feed(b"\x1b]1337;File=inline=1\x07");
        assert!(refusals(&events).is_empty());
        assert!(images(&events).is_empty());
    }

    /// Two pictures with nothing between them: the terminator of the first must not eat
    /// the introducer of the second.
    #[test]
    fn two_sequences_back_to_back_are_both_found() {
        let mut scanner = Scanner::new();
        let events =
            scanner.feed(b"\x1b]1337;File=inline=1:Zm9v\x1b\\\x1b]1337;File=inline=1:YmFy\x07");
        assert_eq!(images(&events).len(), 2);
        assert_eq!(
            text(&events),
            b"\x1b\\",
            "only the first ST is the parser's"
        );
    }

    /// A sequence interrupted by ordinary output is what a program that crashed mid-write
    /// produces, and it must not swallow everything after it.
    #[test]
    fn a_sequence_abandoned_mid_payload_ends_at_the_next_escape() {
        let mut scanner = Scanner::new();
        // Five base64 characters: four make three bytes and the fifth is six leftover bits,
        // which is a payload cut off in the middle of a byte.
        let events = scanner.feed(b"\x1b]1337;File=inline=1:Zm9vZ\x1b[31mred");
        assert_eq!(refusals(&events), vec![RefusalReason::Malformed]);
        assert!(images(&events).is_empty());
        assert_eq!(
            text(&events),
            b"\x1b[31mred",
            "the output after an abandoned sequence must not be swallowed"
        );
        assert!(!scanner.is_mid_sequence());

        // A payload cut at a byte boundary is decodable as far as framing goes, so it comes
        // out as a picture and is refused a layer up, where the bytes are read as an image.
        let mut scanner = Scanner::new();
        let events = scanner.feed(b"\x1b]1337;File=inline=1:Zm9v\x1b[31mred");
        assert_eq!(images(&events).len(), 1);
        assert_eq!(text(&events), b"\x1b[31mred");
    }

    #[test]
    fn no_byte_sequence_at_all_can_make_the_scanner_panic_or_hoard() {
        let mut scanner = Scanner::with_payload_limit(4_096);
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..200 {
            let mut chunk = Vec::with_capacity(512);
            for _ in 0..512 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                chunk.push((state & 0xFF) as u8);
            }
            let _ = scanner.feed(&chunk);
        }
        // Every escape introducer followed by every possible byte.
        for intro in *b"]P_[\\" {
            for byte in 0..=255u8 {
                let mut scanner = Scanner::new();
                let _ = scanner.feed(&[0x1B, intro, byte]);
                let _ = scanner.feed(&[byte, 0x07]);
            }
        }
        // A stream of nothing but escapes.
        let mut scanner = Scanner::new();
        let _ = scanner.feed(&vec![0x1B; 10_000]);
    }

    #[test]
    fn an_escape_at_the_very_end_of_a_chunk_waits_for_the_next_one() {
        let mut scanner = Scanner::new();
        let events = scanner.feed(b"text\x1b");
        assert_eq!(text(&events), b"text");
        assert!(scanner.is_mid_sequence(), "the escape is still pending");

        let events = scanner.feed(b"]1337;File=inline=1:Zm9v\x07");
        assert_eq!(images(&events).len(), 1);
        assert!(text(&events).is_empty());
    }
}
