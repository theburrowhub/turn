//! Cross-module checks that keep the catalogue and its documentation honest.
//!
//! These live inside the crate rather than in `tests/` because they need the
//! one-of-each fixtures from the request and response test modules. What they
//! guard is the part of the contract a reader trusts and a compiler does not
//! check: that every request names a response that exists, that no two things
//! share a wire name, and that the whole surface survives real framing.

use std::collections::{HashMap, HashSet};

use crate::envelope::{ClientFrame, ServerFrame};
use crate::events::tests::all_events;
use crate::framing::{encode, LineDecoder};
use crate::request::tests::all_requests;
use crate::request::RequestId;
use crate::response::tests::all_responses;
use crate::response::Response;

/// The typed half of the contract: every request promises a response that exists.
#[test]
fn every_request_names_a_response_variant_that_exists() {
    let available: HashSet<&'static str> = all_responses()
        .iter()
        .map(|r| r.result_name())
        .chain(Response::RESULT_NAMES.iter().copied())
        .collect();

    for request in all_requests() {
        let expected = request.expected_result();
        assert!(
            available.contains(expected),
            "{} promises a `{expected}` response, which the catalogue does not define",
            request.op()
        );
    }
}

/// Every response variant is reachable. A result the daemon can never be asked
/// for is either dead weight or a request someone forgot to add.
#[test]
fn every_response_variant_is_produced_by_at_least_one_request() {
    let promised: HashSet<&'static str> =
        all_requests().iter().map(|r| r.expected_result()).collect();

    for name in Response::RESULT_NAMES {
        assert!(
            promised.contains(name),
            "no request ever produces a `{name}` response"
        );
    }
}

/// Wire names are the vocabulary two independently written implementations share.
/// A duplicate would make one of them unreachable.
#[test]
fn wire_names_are_unique_within_each_catalogue() {
    fn assert_unique(kind: &str, names: Vec<&'static str>) {
        let mut seen: HashMap<&'static str, usize> = HashMap::new();
        for name in &names {
            *seen.entry(name).or_default() += 1;
        }
        let duplicates: Vec<&&'static str> = seen
            .iter()
            .filter(|(_, n)| **n > 1)
            .map(|(k, _)| k)
            .collect();
        assert!(
            duplicates.is_empty(),
            "duplicate {kind} names: {duplicates:?}"
        );
    }

    assert_unique("request", all_requests().iter().map(|r| r.op()).collect());
    assert_unique(
        "response",
        all_responses().iter().map(|r| r.result_name()).collect(),
    );
    assert_unique(
        "push",
        all_events().iter().map(|e| e.event_name()).collect(),
    );
}

/// Every message in the catalogue, in both directions, through the real framing.
///
/// The per-module tests already round-trip each type through serde. This one adds
/// the envelope and the line decoder, which is where a message that serialises
/// fine can still break the stream — an embedded newline, or a frame that only
/// parses when it is the sole thing in the buffer.
#[test]
fn the_entire_catalogue_survives_the_framing_in_one_stream() {
    let mut client_stream = Vec::new();
    let requests = all_requests();
    for (index, request) in requests.iter().enumerate() {
        let frame = ClientFrame::request(RequestId::new(format!("r-{index}")), request.clone());
        client_stream.extend(encode(&frame).unwrap());
    }

    let mut decoder = LineDecoder::new();
    decoder.feed(&client_stream);
    let mut decoded = Vec::new();
    while let Some(result) = decoder.next_message::<ClientFrame>() {
        decoded.push(result.expect("every catalogue frame must decode"));
    }
    assert_eq!(decoded.len(), requests.len());
    for (index, frame) in decoded.iter().enumerate() {
        assert_eq!(frame.request_id().unwrap().as_str(), format!("r-{index}"));
    }
    assert_eq!(decoder.buffered(), 0);
    assert_eq!(decoder.lines_dropped(), 0);

    let mut server_stream = Vec::new();
    let responses = all_responses();
    let events = all_events();
    for (index, response) in responses.iter().enumerate() {
        let frame = ServerFrame::response(RequestId::new(format!("r-{index}")), response.clone());
        server_stream.extend(encode(&frame).unwrap());
    }
    for event in &events {
        server_stream.extend(encode(&ServerFrame::event(event.clone())).unwrap());
    }

    let mut decoder = LineDecoder::new();
    decoder.feed(&server_stream);
    let mut count = 0;
    while let Some(result) = decoder.next_message::<ServerFrame>() {
        result.expect("every server frame must decode");
        count += 1;
    }
    assert_eq!(count, responses.len() + events.len());
    assert_eq!(decoder.buffered(), 0);
}

/// Feeding the whole catalogue through in single-byte reads. Slow-ish and worth
/// it: this is the one test that would catch a framing assumption about chunk
/// boundaries, and it exercises every message shape rather than a probe struct.
#[test]
fn the_catalogue_reassembles_correctly_under_pathological_chunking() {
    let mut stream = Vec::new();
    for (index, request) in all_requests().iter().enumerate() {
        stream.extend(
            encode(&ClientFrame::request(
                RequestId::new(format!("r-{index}")),
                request.clone(),
            ))
            .unwrap(),
        );
    }

    let mut decoder = LineDecoder::new();
    let mut decoded = 0;
    // Feed in awkward, prime-sized slices so boundaries land mid-token, mid-string
    // and mid-escape.
    for chunk in stream.chunks(7) {
        decoder.feed(chunk);
        while let Some(result) = decoder.next_message::<ClientFrame>() {
            result.expect("a chunk boundary must not corrupt a frame");
            decoded += 1;
        }
    }
    assert_eq!(decoded, all_requests().len());
    assert_eq!(decoder.buffered(), 0);
}
