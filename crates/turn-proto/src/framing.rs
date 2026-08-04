//! Newline-delimited JSON over a unix socket.
//!
//! One JSON value per line, `\n`-terminated, UTF-8. Chosen over a
//! length-prefixed binary framing for one reason that matters more than
//! efficiency at this stage: the most important boundary in the system stays
//! readable. `socat - UNIX-CONNECT:...turnd.sock` is a working client, a bug
//! report can include the exact bytes, and a second frontend can be written in
//! any language without a codec library. Binary payloads pay for this in base64
//! (see [`crate::bytes`]).
//!
//! Two properties the decoder guarantees, because a terminal multiplexer that
//! drops its control connection on bad input is worse than useless:
//!
//! 1. **Partial reads are normal.** A socket hands over arbitrary slices; a frame
//!    may arrive in forty pieces or forty frames in one piece. [`LineDecoder`]
//!    buffers until it has a complete line and never assumes chunk boundaries
//!    mean anything.
//! 2. **A bad line costs one line.** Invalid JSON, an unknown message shape or a
//!    line over the limit yields an error for *that line* and the decoder carries
//!    on with the next one. The caller turns the error into a protocol error
//!    frame; the connection stays up.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{ErrorCode, ProtoError};

/// Largest line either side will accept, 8 MiB.
///
/// Generous enough that no legitimate message comes close — the biggest thing
/// this protocol carries is a pane replay, which is the *rendered screen* rather
/// than the scrollback and measures in kilobytes. Small enough that a peer cannot
/// exhaust memory by opening a socket and writing bytes without a newline.
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Recommended ceiling on raw bytes per terminal-output frame, 256 KiB.
///
/// Base64 takes that to ~342 KiB, comfortably inside [`MAX_LINE_BYTES`] with room
/// for the envelope. The daemon splits larger reads with
/// [`crate::TerminalBytes::chunks`] rather than emitting a frame it knows is too
/// big and having it discarded at the far end.
pub const MAX_OUTPUT_CHUNK_BYTES: usize = 256 * 1024;

/// Why a frame could not be turned into a message, or a message into a frame.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The line ran past the limit. `length` is how much had accumulated when the
    /// decoder gave up, which is a lower bound on the real length: the rest is
    /// discarded as it arrives rather than buffered to find out.
    #[error("frame of at least {length} bytes exceeds the {limit} byte limit")]
    LineTooLong { length: usize, limit: usize },

    /// Valid or invalid JSON that is not a message of the expected type.
    #[error("malformed frame: {source}")]
    Malformed {
        #[source]
        source: serde_json::Error,
        /// The start of the offending line, truncated and escaped, for logs. Kept
        /// short deliberately: a decode failure on a frame carrying pty output
        /// must not spill that output into a log file.
        excerpt: String,
    },

    /// Serialising an outgoing message failed. In practice this only happens for
    /// a non-string map key or a non-finite float, both of which are bugs.
    #[error("could not encode frame: {0}")]
    Encode(#[source] serde_json::Error),

    /// The encoder was asked to produce a frame larger than the limit.
    #[error("encoded frame of {length} bytes exceeds the {limit} byte limit")]
    TooLargeToSend { length: usize, limit: usize },
}

impl FrameError {
    /// The protocol error code this maps to.
    pub fn code(&self) -> ErrorCode {
        match self {
            FrameError::LineTooLong { .. } | FrameError::TooLargeToSend { .. } => {
                ErrorCode::LineTooLong
            }
            FrameError::Malformed { .. } => ErrorCode::MalformedMessage,
            FrameError::Encode(_) => ErrorCode::Internal,
        }
    }

    /// The error frame to send back. This is the whole point of not closing the
    /// connection: the peer is told precisely what it did wrong.
    pub fn to_proto_error(&self) -> ProtoError {
        let mut error = ProtoError::new(self.code(), summary_for(self));
        if let FrameError::Malformed { excerpt, source } = self {
            error = error.with_detail(format!("at {source}; line began {excerpt:?}"));
        }
        error
    }
}

/// User-facing sentence for a frame error. Separate from `Display` because
/// `Display` is for logs and may name byte counts a user does not care about.
fn summary_for(error: &FrameError) -> String {
    match error {
        FrameError::LineTooLong { limit, .. } | FrameError::TooLargeToSend { limit, .. } => {
            format!("A message exceeded the {limit} byte frame limit and was dropped")
        }
        FrameError::Malformed { .. } => "A message could not be understood".to_string(),
        FrameError::Encode(_) => "A message could not be encoded".to_string(),
    }
}

/// Serialises a message as one frame, newline included.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, FrameError> {
    let mut out = Vec::new();
    encode_into(message, &mut out)?;
    Ok(out)
}

/// Appends a frame to an existing buffer, so a writer can batch several frames
/// into one `write_all` instead of one syscall each.
pub fn encode_into<T: Serialize>(message: &T, out: &mut Vec<u8>) -> Result<(), FrameError> {
    let start = out.len();
    serde_json::to_writer(&mut *out, message).map_err(FrameError::Encode)?;
    debug_assert!(
        !out[start..].contains(&b'\n'),
        "serde_json escapes newlines inside strings, so a frame can never contain a raw one"
    );
    out.push(b'\n');
    Ok(())
}

/// Like [`encode`], but refuses to hand back a frame the peer would discard.
pub fn encode_checked<T: Serialize>(message: &T, limit: usize) -> Result<Vec<u8>, FrameError> {
    let frame = encode(message)?;
    // The newline is a delimiter, not part of the line.
    let length = frame.len() - 1;
    if length > limit {
        return Err(FrameError::TooLargeToSend { length, limit });
    }
    Ok(frame)
}

/// Accumulates bytes from a socket and yields whole frames.
///
/// Not an `Iterator`: it is fed from outside and legitimately runs dry between
/// reads, and `None` meaning "need more bytes" rather than "finished" is clearer
/// as its own method than as a stream that resumes after ending.
#[derive(Debug)]
pub struct LineDecoder {
    buf: Vec<u8>,
    limit: usize,
    /// Set while skipping the tail of a line that already blew the limit. Without
    /// this, the remainder of an oversized line would be parsed as a fresh frame
    /// and produce a second, confusing error.
    discarding: bool,
    lines_dropped: u64,
}

impl Default for LineDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl LineDecoder {
    pub fn new() -> Self {
        Self::with_limit(MAX_LINE_BYTES)
    }

    pub fn with_limit(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            limit: limit.max(1),
            discarding: false,
            lines_dropped: 0,
        }
    }

    /// Adds bytes as they arrive. Chunk boundaries carry no meaning.
    pub fn feed(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Bytes held pending a newline. Exposed so a caller can assert the decoder
    /// is not quietly hoarding memory.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// How many lines have been discarded for exceeding the limit.
    pub fn lines_dropped(&self) -> u64 {
        self.lines_dropped
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The next complete line, without its terminator.
    ///
    /// `None` means "not yet"; `Some(Err(..))` means one line was lost and the
    /// next call resumes normally. Blank lines are skipped rather than reported:
    /// they carry no message, and a peer that pads its stream is being harmless
    /// rather than wrong.
    pub fn next_line(&mut self) -> Option<Result<Vec<u8>, FrameError>> {
        loop {
            if self.discarding {
                match self.find_newline() {
                    Some(position) => {
                        self.buf.drain(..=position);
                        self.discarding = false;
                    }
                    None => {
                        // Everything so far belongs to the doomed line.
                        self.release_buffer();
                        return None;
                    }
                }
                continue;
            }

            match self.find_newline() {
                Some(position) => {
                    let mut line: Vec<u8> = self.buf.drain(..=position).collect();
                    line.pop();
                    // Tolerate CRLF from a peer using a line-oriented writer.
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    if line.len() > self.limit {
                        self.lines_dropped += 1;
                        // The line is gone with `line`, but the buffer it was
                        // drained out of still holds room for another one.
                        self.buf.shrink_to_fit();
                        return Some(Err(FrameError::LineTooLong {
                            length: line.len(),
                            limit: self.limit,
                        }));
                    }
                    if line.iter().all(|b| b.is_ascii_whitespace()) {
                        continue;
                    }
                    return Some(Ok(line));
                }
                None => {
                    if self.buf.len() > self.limit {
                        // Report now and drop the rest as it arrives, rather than
                        // buffering an unbounded amount to measure it exactly.
                        let length = self.buf.len();
                        self.release_buffer();
                        self.discarding = true;
                        self.lines_dropped += 1;
                        return Some(Err(FrameError::LineTooLong {
                            length,
                            limit: self.limit,
                        }));
                    }
                    return None;
                }
            }
        }
    }

    /// The next complete frame, deserialised.
    pub fn next_message<T: DeserializeOwned>(&mut self) -> Option<Result<T, FrameError>> {
        match self.next_line()? {
            Ok(line) => {
                Some(
                    serde_json::from_slice(&line).map_err(|source| FrameError::Malformed {
                        source,
                        excerpt: excerpt(&line),
                    }),
                )
            }
            Err(error) => Some(Err(error)),
        }
    }

    fn find_newline(&self) -> Option<usize> {
        self.buf.iter().position(|&b| b == b'\n')
    }

    /// Gives up the allocation as well as the bytes.
    ///
    /// `Vec::clear` keeps whatever the longest line so far reserved, so a peer that
    /// sends one 8 MiB line — refused, dropped, never parsed — would still cost the
    /// connection 8 MiB of resident memory for as long as it stays open. That is
    /// the hoarding the limit exists to prevent, so the limit has to release it.
    fn release_buffer(&mut self) {
        self.buf = Vec::new();
    }
}

/// The first few characters of a line, for a log message.
///
/// Capped and lossy-decoded: the excerpt exists to identify which frame failed,
/// not to reproduce it. Truncating at a char boundary avoids emitting a broken
/// UTF-8 fragment into a log.
fn excerpt(line: &[u8]) -> String {
    const MAX: usize = 120;
    let text = String::from_utf8_lossy(&line[..line.len().min(MAX)]);
    let mut out: String = text.chars().take(MAX).collect();
    if line.len() > MAX {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Probe {
        name: String,
        count: u32,
    }

    fn probe(name: &str, count: u32) -> Probe {
        Probe {
            name: name.to_string(),
            count,
        }
    }

    #[test]
    fn a_frame_round_trips_and_ends_in_exactly_one_newline() {
        let frame = encode(&probe("alpha", 1)).unwrap();
        assert_eq!(frame.last(), Some(&b'\n'));
        assert_eq!(frame.iter().filter(|b| **b == b'\n').count(), 1);

        let mut decoder = LineDecoder::new();
        decoder.feed(&frame);
        let back: Probe = decoder.next_message().unwrap().unwrap();
        assert_eq!(back, probe("alpha", 1));
        assert!(decoder.next_message::<Probe>().is_none());
        assert_eq!(decoder.buffered(), 0);
    }

    /// A socket hands over whatever it feels like. Feeding a frame one byte at a
    /// time must produce exactly the same result as feeding it whole.
    #[test]
    fn a_frame_split_across_arbitrary_reads_is_reassembled() {
        let frame = encode(&probe("split across reads", 42)).unwrap();
        let mut decoder = LineDecoder::new();

        for (index, byte) in frame.iter().enumerate() {
            decoder.feed(&[*byte]);
            let is_last = index + 1 == frame.len();
            match decoder.next_message::<Probe>() {
                None => assert!(!is_last, "the final byte completes the frame"),
                Some(Ok(message)) => {
                    assert!(is_last, "a frame appeared before its newline");
                    assert_eq!(message, probe("split across reads", 42));
                }
                Some(Err(error)) => panic!("unexpected error: {error}"),
            }
        }
    }

    #[test]
    fn several_frames_in_one_read_are_all_delivered_in_order() {
        let mut buffer = Vec::new();
        for i in 0..5 {
            encode_into(&probe(&format!("m{i}"), i), &mut buffer).unwrap();
        }
        let mut decoder = LineDecoder::new();
        decoder.feed(&buffer);

        let mut seen = Vec::new();
        while let Some(result) = decoder.next_message::<Probe>() {
            seen.push(result.unwrap());
        }
        assert_eq!(seen.len(), 5);
        assert_eq!(seen[0], probe("m0", 0));
        assert_eq!(seen[4], probe("m4", 4));
    }

    /// The central robustness requirement: a bad line must cost one line.
    #[test]
    fn a_malformed_line_reports_an_error_and_the_stream_continues() {
        let mut buffer = Vec::new();
        encode_into(&probe("before", 1), &mut buffer).unwrap();
        buffer.extend_from_slice(b"{this is not json\n");
        encode_into(&probe("after", 2), &mut buffer).unwrap();

        let mut decoder = LineDecoder::new();
        decoder.feed(&buffer);

        assert_eq!(
            decoder.next_message::<Probe>().unwrap().unwrap(),
            probe("before", 1)
        );

        let error = decoder.next_message::<Probe>().unwrap().unwrap_err();
        assert_eq!(error.code(), ErrorCode::MalformedMessage);
        assert!(matches!(error, FrameError::Malformed { .. }));

        assert_eq!(
            decoder.next_message::<Probe>().unwrap().unwrap(),
            probe("after", 2),
            "the connection must survive a bad frame"
        );
    }

    #[test]
    fn valid_json_of_the_wrong_shape_is_a_malformed_frame_not_a_panic() {
        let mut decoder = LineDecoder::new();
        decoder.feed(b"{\"name\":\"missing count\"}\n[1,2,3]\n\"a bare string\"\nnull\n");
        for _ in 0..4 {
            let error = decoder.next_message::<Probe>().unwrap().unwrap_err();
            assert_eq!(error.code(), ErrorCode::MalformedMessage);
        }
        assert!(decoder.next_message::<Probe>().is_none());
    }

    /// `buffered() == 0` is satisfied by `Vec::clear`, which keeps the whole
    /// allocation: the bytes are unreachable but the memory is still the
    /// connection's for as long as it lives. Capacity is the honest measure, so it
    /// is what this asserts.
    #[test]
    fn an_oversized_line_is_dropped_without_the_decoder_hoarding_it() {
        let mut decoder = LineDecoder::with_limit(256);
        // A megabyte with no newline in sight.
        decoder.feed(&vec![b'x'; 1_000_000]);

        let error = decoder.next_line().unwrap().unwrap_err();
        assert_eq!(error.code(), ErrorCode::LineTooLong);
        assert_eq!(
            decoder.buffered(),
            0,
            "the decoder must not keep a line it has already refused"
        );
        assert_eq!(
            decoder.buf.capacity(),
            0,
            "nor the megabyte it reserved to hold it"
        );
        assert_eq!(decoder.lines_dropped(), 1);

        // More of the same line keeps being discarded, and only one error was
        // reported for it.
        decoder.feed(&vec![b'x'; 1_000_000]);
        assert!(decoder.next_line().is_none());
        assert_eq!(decoder.lines_dropped(), 1);
        assert_eq!(decoder.buffered(), 0);
        assert_eq!(decoder.buf.capacity(), 0, "and each further chunk too");
    }

    /// The same leak by the other route: a line that arrives complete, newline and
    /// all, and is only then found to be over the limit.
    #[test]
    fn a_refused_line_that_arrived_whole_leaves_its_allocation_behind_too() {
        let mut decoder = LineDecoder::with_limit(256);
        let mut buffer = vec![b'x'; 1_000_000];
        buffer.push(b'\n');
        encode_into(&probe("survivor", 7), &mut buffer).unwrap();
        decoder.feed(&buffer);

        assert!(matches!(
            decoder.next_line().unwrap().unwrap_err(),
            FrameError::LineTooLong { .. }
        ));
        assert!(
            decoder.buf.capacity() < 4_096,
            "a megabyte was still reserved: {}",
            decoder.buf.capacity()
        );
        assert_eq!(
            decoder.next_message::<Probe>().unwrap().unwrap(),
            probe("survivor", 7),
            "and the frame behind it still parses"
        );
    }

    #[test]
    fn the_frame_after_an_oversized_one_is_parsed_normally() {
        let mut decoder = LineDecoder::with_limit(128);
        let mut buffer = vec![b'x'; 500];
        buffer.push(b'\n');
        encode_into(&probe("survivor", 7), &mut buffer).unwrap();
        decoder.feed(&buffer);

        assert!(matches!(
            decoder.next_message::<Probe>().unwrap().unwrap_err(),
            FrameError::LineTooLong { .. }
        ));
        assert_eq!(
            decoder.next_message::<Probe>().unwrap().unwrap(),
            probe("survivor", 7),
            "the tail of an oversized line must not eat the next frame"
        );
    }

    /// The awkward case: the line is under the limit while buffering but over it
    /// once complete. Caught at the newline rather than never.
    #[test]
    fn a_line_that_only_exceeds_the_limit_at_its_newline_is_still_refused() {
        let mut decoder = LineDecoder::with_limit(10);
        decoder.feed(b"0123456789AB\n");
        let error = decoder.next_line().unwrap().unwrap_err();
        match error {
            FrameError::LineTooLong { length, limit } => {
                assert_eq!(length, 12);
                assert_eq!(limit, 10);
            }
            other => panic!("expected an over-length error, got {other}"),
        }
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn blank_and_crlf_terminated_lines_are_tolerated() {
        let mut decoder = LineDecoder::new();
        let json = serde_json::to_string(&probe("crlf", 3)).unwrap();
        decoder.feed(b"\n\n   \n");
        decoder.feed(json.as_bytes());
        decoder.feed(b"\r\n");

        assert_eq!(
            decoder.next_message::<Probe>().unwrap().unwrap(),
            probe("crlf", 3)
        );
        assert!(decoder.next_message::<Probe>().is_none());
    }

    #[test]
    fn a_frame_containing_newlines_in_its_data_stays_one_line() {
        // A pasted multi-line command is the everyday version of this.
        let message = probe("line one\nline two\r\nline three", 1);
        let frame = encode(&message).unwrap();
        assert_eq!(
            frame.iter().filter(|b| **b == b'\n').count(),
            1,
            "the only raw newline is the delimiter"
        );

        let mut decoder = LineDecoder::new();
        decoder.feed(&frame);
        assert_eq!(decoder.next_message::<Probe>().unwrap().unwrap(), message);
    }

    #[test]
    fn the_encoder_refuses_to_emit_a_frame_the_peer_would_discard() {
        let huge = probe(&"x".repeat(1_000), 0);
        match encode_checked(&huge, 100) {
            Err(FrameError::TooLargeToSend { length, limit }) => {
                assert!(length > 1_000);
                assert_eq!(limit, 100);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        // And a frame that fits comes back with its newline intact.
        let small = encode_checked(&probe("fits", 1), MAX_LINE_BYTES).unwrap();
        assert_eq!(small.last(), Some(&b'\n'));
    }

    #[test]
    fn a_frame_error_becomes_a_protocol_error_with_a_useful_detail() {
        let mut decoder = LineDecoder::new();
        decoder.feed(b"{\"name\": broken}\n");
        let error = decoder.next_message::<Probe>().unwrap().unwrap_err();
        let proto = error.to_proto_error();
        assert_eq!(proto.code, ErrorCode::MalformedMessage);
        assert!(!proto.message.is_empty());
        let detail = proto.detail.expect("a malformed frame carries context");
        assert!(detail.contains("line began"), "got {detail}");
    }

    #[test]
    fn a_long_excerpt_is_truncated_and_never_reproduces_the_whole_frame() {
        let line = format!("{{\"junk\":\"{}\"", "s".repeat(5_000));
        let shown = excerpt(line.as_bytes());
        assert!(
            shown.chars().count() <= 121,
            "got {} chars",
            shown.chars().count()
        );
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn invalid_utf8_in_a_line_is_reported_rather_than_crashing_the_decoder() {
        let mut decoder = LineDecoder::new();
        decoder.feed(&[0xff, 0xfe, 0xfd, b'\n']);
        let error = decoder.next_message::<Probe>().unwrap().unwrap_err();
        assert_eq!(error.code(), ErrorCode::MalformedMessage);
        // The excerpt is lossy rather than absent.
        assert!(error.to_proto_error().detail.is_some());
    }

    #[test]
    fn a_zero_limit_is_clamped_so_the_decoder_can_still_make_progress() {
        let decoder = LineDecoder::with_limit(0);
        assert_eq!(decoder.limit(), 1);
    }
}
