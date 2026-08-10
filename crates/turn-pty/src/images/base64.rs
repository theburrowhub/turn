//! Streaming base64, bounded, for payloads that arrive in pieces.
//!
//! Not [`turn_proto::decode_base64`], and the difference is the point. That decoder takes
//! a whole string and is deliberately strict — canonical padding, no whitespace, exact
//! length — because it decodes *Turn's own* frames, where anything else is a defect.
//!
//! This one decodes what a **process** wrote inside an escape sequence, and it has three
//! requirements the other cannot meet:
//!
//! * **It must be incremental.** A four-megabyte Sixel or Kitty payload arrives in as
//!   many pty reads as the kernel feels like, and a decoder that needs the whole string
//!   first would have to buffer the base64 as well as the bytes — a third of a megabyte
//!   wasted per image, and unbounded until the sequence terminates.
//! * **It must be bounded as it goes.** The limit is checked on every push, so a process
//!   that never terminates its sequence is refused after `limit` bytes rather than after
//!   it has exhausted memory.
//! * **It must tolerate what the protocols actually emit.** Real encoders wrap lines,
//!   omit padding, and split a payload at a boundary that is not a multiple of four. All
//!   three are accepted. What is *not* accepted is a character outside the alphabet: that
//!   means the stream is not what it claimed to be, and guessing at it would turn
//!   corruption into pixels.

/// Standard base64 alphabet, and the reverse table built from it at compile time.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const INVALID: u8 = 0xFF;

const DECODE_TABLE: [u8; 256] = {
    let mut table = [INVALID; 256];
    let mut i = 0usize;
    while i < 64 {
        table[ALPHABET[i] as usize] = i as u8;
        i += 1;
    }
    table
};

/// Why a payload was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Base64Error {
    /// A byte that is not base64, not padding and not whitespace. The stream is not what
    /// it said it was.
    #[error("byte {byte:#04x} is not base64")]
    BadCharacter { byte: u8 },
    /// The payload passed the byte limit. Reported the moment it does, so the rest is
    /// discarded as it arrives rather than accumulated to measure it exactly.
    #[error("a payload of over {limit} bytes was refused")]
    TooLarge { limit: usize },
    /// The final group holds one leftover character, which encodes six bits and cannot
    /// be part of any byte. A truncated payload, not a decodable one.
    #[error("the payload ends mid-byte, with {bits} leftover bits")]
    Truncated { bits: u8 },
}

/// A base64 payload being decoded as it arrives.
#[derive(Debug)]
pub struct Base64Stream {
    out: Vec<u8>,
    /// Bits shifted in but not yet a whole byte, low-order aligned.
    acc: u32,
    bits: u8,
    limit: usize,
    /// Set once the stream is known to be unusable. Everything after is discarded, and
    /// nothing is retained, so a hostile process gains no memory by carrying on.
    failed: Option<Base64Error>,
    /// Set by the first `=`. Padding only ever appears at the end, and anything after it
    /// is a payload that does not agree with its own framing.
    padded: bool,
}

impl Base64Stream {
    pub fn with_limit(limit: usize) -> Self {
        Self {
            out: Vec::new(),
            acc: 0,
            bits: 0,
            limit,
            failed: None,
            padded: false,
        }
    }

    /// Adds the next piece. Chunk boundaries carry no meaning.
    pub fn push(&mut self, chunk: &[u8]) {
        if self.failed.is_some() {
            return;
        }
        for byte in chunk {
            if let Err(error) = self.push_byte(*byte) {
                self.fail(error);
                return;
            }
        }
    }

    fn push_byte(&mut self, byte: u8) -> Result<(), Base64Error> {
        // Whitespace is how a real encoder wraps a long payload, and it carries no bits.
        if byte.is_ascii_whitespace() {
            return Ok(());
        }
        if byte == b'=' {
            // Padding is only ever a hint about the length, which the leftover bits
            // already say. Recorded so a character *after* it is refused.
            self.padded = true;
            return Ok(());
        }
        if self.padded {
            return Err(Base64Error::BadCharacter { byte });
        }
        let value = DECODE_TABLE[byte as usize];
        if value == INVALID {
            return Err(Base64Error::BadCharacter { byte });
        }
        self.acc = (self.acc << 6) | value as u32;
        self.bits += 6;
        if self.bits >= 8 {
            self.bits -= 8;
            if self.out.len() >= self.limit {
                return Err(Base64Error::TooLarge { limit: self.limit });
            }
            self.out.push((self.acc >> self.bits) as u8);
        }
        Ok(())
    }

    fn fail(&mut self, error: Base64Error) {
        self.failed = Some(error);
        // Released rather than cleared: a refused four-megabyte payload must not stay
        // resident for as long as the pane lives.
        self.out = Vec::new();
    }

    /// How many decoded bytes are held so far.
    pub fn len(&self) -> usize {
        self.out.len()
    }

    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    /// Whether the stream has already been refused, so a caller can stop scanning its
    /// payload without waiting for the terminator.
    pub fn failed(&self) -> Option<Base64Error> {
        self.failed
    }

    /// The decoded bytes, or why they cannot be trusted.
    ///
    /// Leftover bits are an error rather than something to discard: two leftover bits are
    /// the normal tail of a payload whose length is not a multiple of three, but six are
    /// a payload that was cut off in the middle of a byte, and pixels decoded from a
    /// truncated raster are pixels nobody asked for.
    pub fn finish(self) -> Result<Vec<u8>, Base64Error> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if self.bits == 6 {
            return Err(Base64Error::Truncated { bits: self.bits });
        }
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(text: &str, limit: usize) -> Result<Vec<u8>, Base64Error> {
        let mut stream = Base64Stream::with_limit(limit);
        stream.push(text.as_bytes());
        stream.finish()
    }

    #[test]
    fn the_standard_vectors_decode_the_same_as_they_would_in_one_piece() {
        for (encoded, plain) in [
            ("", ""),
            ("Zg==", "f"),
            ("Zm8=", "fo"),
            ("Zm9v", "foo"),
            ("Zm9vYg==", "foob"),
            ("Zm9vYmE=", "fooba"),
            ("Zm9vYmFy", "foobar"),
        ] {
            assert_eq!(
                decode(encoded, 1_000).expect("it decodes"),
                plain.as_bytes(),
                "decoding {encoded:?}"
            );
        }
    }

    /// The property this decoder exists for: a payload split at every possible boundary
    /// has to decode identically, because the kernel decides where the boundaries are.
    #[test]
    fn a_payload_split_at_every_boundary_decodes_to_the_same_bytes() {
        let plain: Vec<u8> = (0..200u32).map(|i| (i * 7) as u8).collect();
        let encoded = turn_proto::encode_base64(&plain);
        for split in 0..encoded.len() {
            let mut stream = Base64Stream::with_limit(1_000);
            stream.push(&encoded.as_bytes()[..split]);
            stream.push(&encoded.as_bytes()[split..]);
            assert_eq!(
                stream.finish().expect("it decodes"),
                plain,
                "split at {split}"
            );
        }

        // And one byte at a time, which is the worst the kernel can do.
        let mut stream = Base64Stream::with_limit(1_000);
        for byte in encoded.as_bytes() {
            stream.push(&[*byte]);
        }
        assert_eq!(stream.finish().expect("it decodes"), plain);
    }

    /// What real encoders emit and the strict decoder would refuse.
    #[test]
    fn wrapped_lines_and_missing_padding_are_tolerated_because_encoders_emit_them() {
        assert_eq!(decode("Zm9v\r\nYmFy", 100).expect("it decodes"), b"foobar");
        assert_eq!(decode("Zm9v YmFy\n", 100).expect("it decodes"), b"foobar");
        assert_eq!(
            decode("Zm8", 100).expect("it decodes"),
            b"fo",
            "a payload with its padding stripped is still two bytes of `fo`"
        );
        assert_eq!(decode("Zg", 100).expect("it decodes"), b"f");
    }

    /// A character outside the alphabet means the stream is not base64, and pixels made
    /// from it would be noise presented as a picture.
    #[test]
    fn a_character_outside_the_alphabet_refuses_the_whole_payload() {
        assert_eq!(
            decode("Zm9v!!!!", 100),
            Err(Base64Error::BadCharacter { byte: b'!' })
        );
        // A URL-safe alphabet is not the standard one.
        assert!(decode("-_-_", 100).is_err());
        // Data after the padding is a payload that disagrees with its own framing.
        assert_eq!(
            decode("Zm8=Zm8=", 100),
            Err(Base64Error::BadCharacter { byte: b'Z' })
        );
    }

    #[test]
    fn a_payload_cut_off_in_the_middle_of_a_byte_is_refused_rather_than_rounded() {
        // One leftover character is six bits: not a byte, and not a tail any encoder
        // produces.
        assert_eq!(decode("Z", 100), Err(Base64Error::Truncated { bits: 6 }));
        assert_eq!(
            decode("Zm9vZ", 100),
            Err(Base64Error::Truncated { bits: 6 })
        );
    }

    /// The bound that matters: a process that never terminates its sequence must be
    /// refused at the limit, and the memory it had claimed must be given back.
    #[test]
    fn a_payload_over_the_limit_is_refused_the_moment_it_passes_it_and_releases_its_bytes() {
        let mut stream = Base64Stream::with_limit(64);
        let flood = turn_proto::encode_base64(&vec![b'x'; 4_096]);
        stream.push(flood.as_bytes());

        assert_eq!(stream.failed(), Some(Base64Error::TooLarge { limit: 64 }));
        assert_eq!(
            stream.len(),
            0,
            "a refused payload must not stay resident for the life of the pane"
        );
        // More of the same payload costs nothing at all.
        stream.push(flood.as_bytes());
        assert_eq!(stream.len(), 0);
        assert_eq!(stream.finish(), Err(Base64Error::TooLarge { limit: 64 }));
    }

    #[test]
    fn a_payload_of_exactly_the_limit_is_accepted() {
        let plain = vec![7u8; 64];
        let encoded = turn_proto::encode_base64(&plain);
        assert_eq!(decode(&encoded, 64).expect("it fits"), plain);
        assert!(decode(&turn_proto::encode_base64(&[7u8; 65]), 64).is_err());
    }

    #[test]
    fn every_byte_value_survives_the_round_trip() {
        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(
            decode(&turn_proto::encode_base64(&all), 1_000).expect("it decodes"),
            all
        );
    }
}
