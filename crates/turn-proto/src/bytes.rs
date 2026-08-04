//! Binary payloads inside a text protocol.
//!
//! Terminal traffic is bytes, not text: escape sequences carry the colours and
//! cursor state, and a pty may emit any byte at all including invalid UTF-8. JSON
//! has no byte type, so [`TerminalBytes`] serialises as standard base64.
//!
//! **The cost, stated plainly.** Base64 inflates every payload by 33% and costs a
//! pass over the data in each direction. For an interactive terminal that is
//! irrelevant — keystrokes are a handful of bytes and a redraw is a few kilobytes.
//! For a `cargo build` firehose it is not: 10 MB of output becomes 13.3 MB on the
//! wire plus encode and decode work. This is accepted for the MVP because one
//! human-readable frame format makes the boundary debuggable with `nc` and
//! testable without tooling, and because [`crate::framing`] caps how much can be
//! in flight per frame. The escape hatch is already in the handshake:
//! [`crate::OutputEncoding`] is negotiated in `Welcome`, so a length-prefixed
//! binary side channel can be added later without a protocol break.
//!
//! Base64 is implemented here rather than pulled in as a dependency: the encoder
//! is thirty lines, the decoder needs to be strict in a way we want to test
//! ourselves, and the protocol crate having no third-party surface beyond serde
//! is worth keeping.

use serde::de::{Error as DeError, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PAD: u8 = b'=';
/// Sentinel for "not a base64 character" in the reverse table.
const INVALID: u8 = 0xFF;

/// Reverse lookup table, built at compile time so decoding is a table index
/// rather than a search.
const DECODE_TABLE: [u8; 256] = {
    let mut table = [INVALID; 256];
    let mut i = 0usize;
    while i < 64 {
        table[ALPHABET[i] as usize] = i as u8;
        i += 1;
    }
    table
};

/// A block of raw bytes, carried as base64 on the wire.
///
/// Used for both directions: process output pushed to the UI and keystrokes or
/// pasted text sent back. Deliberately not `String`: pretending pty traffic is
/// text is how mojibake and half-written escape sequences get into a terminal.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct TerminalBytes(Vec<u8>);

impl TerminalBytes {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of base64 characters this will occupy on the wire.
    ///
    /// Exposed so the daemon can size an output chunk against the frame limit
    /// before it starts encoding, rather than discovering the overflow after.
    pub fn encoded_len(&self) -> usize {
        self.0.len().div_ceil(3) * 4
    }

    /// Splits into pieces of at most `max_raw_bytes`, in order.
    ///
    /// A single pty read can be megabytes; a frame must stay under the line
    /// limit. Chunking here — rather than letting the encoder fail — keeps the
    /// stream flowing instead of dropping the tail of a noisy build.
    pub fn chunks(&self, max_raw_bytes: usize) -> Vec<TerminalBytes> {
        if self.0.is_empty() {
            return Vec::new();
        }
        let size = max_raw_bytes.max(1);
        self.0
            .chunks(size)
            .map(|c| TerminalBytes(c.to_vec()))
            .collect()
    }

    /// The base64 form, without line breaks.
    pub fn to_base64(&self) -> String {
        encode_base64(&self.0)
    }

    /// Parses a base64 string, rejecting anything that is not canonical.
    pub fn from_base64(text: &str) -> Result<Self, Base64Error> {
        decode_base64(text).map(Self)
    }
}

impl From<Vec<u8>> for TerminalBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl From<&[u8]> for TerminalBytes {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

impl AsRef<[u8]> for TerminalBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Serialize for TerminalBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_base64())
    }
}

impl<'de> Deserialize<'de> for TerminalBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        TerminalBytes::from_base64(&text)
            .map_err(|e| D::Error::invalid_value(Unexpected::Str(&text), &e.expectation()))
    }
}

/// Why a base64 payload was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Base64Error {
    #[error("base64 length {0} is not a multiple of 4")]
    BadLength(usize),
    #[error("byte {byte:#04x} at offset {offset} is not a base64 character")]
    BadCharacter { byte: u8, offset: usize },
    #[error("padding at offset {offset} is followed by more data")]
    MisplacedPadding { offset: usize },
    /// Trailing bits that a canonical encoder would have left zero. Accepting
    /// these would mean two different strings decode to the same bytes, which
    /// makes any hash or comparison over the wire form unreliable.
    #[error("non-canonical trailing bits in the final group")]
    NonCanonical,
}

impl Base64Error {
    /// What a valid value would have looked like, for serde's error text.
    fn expectation(&self) -> &'static str {
        "canonical standard base64 with padding"
    }
}

/// Standard base64 with padding, no line breaks.
pub fn encode_base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for group in data.chunks(3) {
        let b0 = group[0] as u32;
        let b1 = *group.get(1).unwrap_or(&0) as u32;
        let b2 = *group.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        if group.len() > 1 {
            out.push(ALPHABET[(triple >> 6) as usize & 0x3F] as char);
        } else {
            out.push(PAD as char);
        }
        if group.len() > 2 {
            out.push(ALPHABET[triple as usize & 0x3F] as char);
        } else {
            out.push(PAD as char);
        }
    }
    out
}

/// Strict standard base64 decode.
///
/// Strict on purpose: whitespace, missing padding, alternate alphabets and
/// non-canonical tail bits are all rejected rather than guessed at. A protocol
/// that quietly repairs its own input is a protocol whose two implementations
/// will eventually disagree.
pub fn decode_base64(text: &str) -> Result<Vec<u8>, Base64Error> {
    let raw = text.as_bytes();
    if raw.len() % 4 != 0 {
        return Err(Base64Error::BadLength(raw.len()));
    }
    let mut out = Vec::with_capacity(raw.len() / 4 * 3);

    for (index, group) in raw.chunks(4).enumerate() {
        let last_group = (index + 1) * 4 == raw.len();
        let mut values = [0u8; 4];
        let mut pads = 0usize;

        for (i, &byte) in group.iter().enumerate() {
            let offset = index * 4 + i;
            if byte == PAD {
                // Padding only ever occupies the last one or two slots of the
                // final group.
                if !last_group || i < 2 {
                    return Err(Base64Error::MisplacedPadding { offset });
                }
                pads += 1;
                continue;
            }
            if pads > 0 {
                return Err(Base64Error::MisplacedPadding { offset });
            }
            let value = DECODE_TABLE[byte as usize];
            if value == INVALID {
                return Err(Base64Error::BadCharacter { byte, offset });
            }
            values[i] = value;
        }

        let triple = ((values[0] as u32) << 18)
            | ((values[1] as u32) << 12)
            | ((values[2] as u32) << 6)
            | values[3] as u32;

        match pads {
            0 => {
                out.push((triple >> 16) as u8);
                out.push((triple >> 8) as u8);
                out.push(triple as u8);
            }
            1 => {
                // Three data characters carry 18 bits but only two output bytes.
                // The low 2 bits of the third sextet are unused, and a canonical
                // encoder leaves them clear.
                if values[2] & 0b11 != 0 {
                    return Err(Base64Error::NonCanonical);
                }
                out.push((triple >> 16) as u8);
                out.push((triple >> 8) as u8);
            }
            2 => {
                // Two data characters carry 12 bits for one output byte; the low 4
                // bits of the second sextet are unused.
                if values[1] & 0b1111 != 0 {
                    return Err(Base64Error::NonCanonical);
                }
                out.push((triple >> 16) as u8);
            }
            _ => return Err(Base64Error::MisplacedPadding { offset: index * 4 }),
        }
    }

    Ok(out)
}

impl fmt::Display for TerminalBytes {
    /// Shows the size, never the content. Terminal traffic contains whatever the
    /// user typed, including secrets pasted into a prompt, and it must not land
    /// in a log line by accident.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} bytes>", self.0.len())
    }
}

impl fmt::Debug for TerminalBytes {
    /// Same reasoning as [`fmt::Display`]: `Debug` is what ends up in a `tracing`
    /// field or a failing assertion, so it must not print the payload either.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TerminalBytes(<{} bytes>)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_test_vectors() {
        // RFC 4648 section 10.
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in cases {
            assert_eq!(
                encode_base64(plain.as_bytes()),
                encoded,
                "encoding {plain:?}"
            );
            assert_eq!(
                decode_base64(encoded).unwrap(),
                plain.as_bytes(),
                "decoding {encoded:?}"
            );
        }
    }

    #[test]
    fn every_possible_byte_survives_a_round_trip() {
        let all: Vec<u8> = (0..=255u8).collect();
        let encoded = encode_base64(&all);
        assert_eq!(decode_base64(&encoded).unwrap(), all);

        // And every length up to a full group boundary, which is where the
        // padding logic lives.
        for len in 0..=16 {
            let slice = &all[..len];
            assert_eq!(decode_base64(&encode_base64(slice)).unwrap(), slice);
        }
    }

    #[test]
    fn escape_sequences_and_invalid_utf8_survive_intact() {
        // A real replay: colour, cursor movement, alternate screen, plus a byte
        // that is not valid UTF-8 on its own.
        let raw = b"\x1b[31mred\x1b[0m\r\n\x1b[?1049h\xff\xfe";
        let bytes = TerminalBytes::new(raw.to_vec());
        let json = serde_json::to_string(&bytes).unwrap();
        let back: TerminalBytes = serde_json::from_str(&json).unwrap();
        assert_eq!(back.as_slice(), raw);
    }

    #[test]
    fn malformed_base64_is_rejected_rather_than_repaired() {
        // Wrong length.
        assert_eq!(decode_base64("Zm9"), Err(Base64Error::BadLength(3)));
        // Not a base64 character.
        assert!(matches!(
            decode_base64("Zm9v!!!!"),
            Err(Base64Error::BadCharacter { .. })
        ));
        // Whitespace is a character like any other here.
        assert!(matches!(
            decode_base64("Zm9v Zm9v"),
            Err(Base64Error::BadLength(9))
        ));
        // Padding in the middle.
        assert!(matches!(
            decode_base64("Zm=vZm9v"),
            Err(Base64Error::MisplacedPadding { .. })
        ));
        // Padding followed by data inside the last group.
        assert!(matches!(
            decode_base64("Zm=v"),
            Err(Base64Error::MisplacedPadding { .. })
        ));
        // A URL-safe alphabet is not the standard one.
        assert!(matches!(
            decode_base64("-_-_"),
            Err(Base64Error::BadCharacter { .. })
        ));
    }

    #[test]
    fn non_canonical_trailing_bits_are_refused_so_encodings_stay_unique() {
        // "Zg==" is the canonical encoding of "f". "Zh==" decodes to the same
        // byte but sets bits a real encoder would have left clear.
        assert_eq!(decode_base64("Zg==").unwrap(), b"f");
        assert_eq!(decode_base64("Zh=="), Err(Base64Error::NonCanonical));
        assert_eq!(decode_base64("Zm8=").unwrap(), b"fo");
        assert_eq!(decode_base64("Zm9="), Err(Base64Error::NonCanonical));
    }

    #[test]
    fn a_bad_payload_fails_deserialisation_instead_of_yielding_garbage() {
        let error = serde_json::from_str::<TerminalBytes>("\"not base64!\"").unwrap_err();
        assert!(
            error.to_string().contains("base64"),
            "the reason must be legible: {error}"
        );
        // A non-string is refused too.
        assert!(serde_json::from_str::<TerminalBytes>("12345").is_err());
    }

    #[test]
    fn encoded_length_is_predicted_exactly_so_frames_can_be_sized_up_front() {
        for len in [0usize, 1, 2, 3, 4, 100, 4095, 4096] {
            let bytes = TerminalBytes::new(vec![b'x'; len]);
            assert_eq!(
                bytes.encoded_len(),
                bytes.to_base64().len(),
                "prediction wrong for {len} bytes"
            );
        }
    }

    #[test]
    fn a_large_payload_splits_into_ordered_chunks_that_reassemble() {
        let big = TerminalBytes::new((0..10_000u32).map(|i| i as u8).collect::<Vec<u8>>());
        let chunks = big.chunks(4_096);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.len() <= 4_096));

        let rejoined: Vec<u8> = chunks.iter().flat_map(|c| c.as_slice().to_vec()).collect();
        assert_eq!(rejoined, big.into_vec());

        // Nothing to send is no frames, not one empty frame.
        assert!(TerminalBytes::default().chunks(1_024).is_empty());
        // A nonsense chunk size does not divide by zero.
        assert_eq!(TerminalBytes::new(vec![1, 2, 3]).chunks(0).len(), 3);
    }

    #[test]
    fn the_debug_and_display_forms_never_leak_what_the_user_typed() {
        let secret = TerminalBytes::new(b"hunter2\r".to_vec());
        assert_eq!(secret.to_string(), "<8 bytes>");
        let debugged = format!("{secret:?}");
        assert_eq!(debugged, "TerminalBytes(<8 bytes>)");
        assert!(
            !debugged.contains("hunter2") && !debugged.contains("aHVudGVy"),
            "a pasted secret must not reach a log line: {debugged}"
        );
    }
}
